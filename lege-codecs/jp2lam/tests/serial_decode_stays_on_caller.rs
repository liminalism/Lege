//! A decode with a worker budget of one runs entirely on the calling thread.
//!
//! `DecodeConcurrency::Serial` and `Budgeted(1)` skip jp2lam's shared executor
//! and decode on the caller. The stages after Tier-1 (the per-plane 9/7
//! synthesis and the fused packers) must then not fan out over whatever Rayon
//! pool is ambient: in the PDF renderer the caller is a worker of the
//! renderer's own pool, and a fan-out there contends with the rest of the
//! page's work.
//!
//! Each case decodes on one worker of a two-thread pool whose other worker
//! does nothing but steal: it loops on `rayon::yield_now()` and counts every
//! job it runs. A decode that splits work into Rayon jobs leaves them on its
//! worker's deque, and the spinning thief all but certainly takes one. A
//! decode that spawns no job leaves nothing to steal, so in the passing case
//! the count is exactly zero, with no timing involved.
//!
//! The planes are large enough (at least `FUSED_PACK_PARALLEL_SAMPLES`, 256 Ki
//! pixels) that every packer takes its contiguous-target branch, the one that
//! fans out when the budget allows it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jp2lam::{
    CodingOptions, DecodeConcurrency, DecodeOutputFormat, DecodeRequest, DecodeResult,
    DecodeTarget, EncodeOptions, Image, Jp2Decoder, OutputFormat, RateControl, TilePolicy, encode,
};

const WIDTH: u32 = 576;
const HEIGHT: u32 = 480;

fn rgba_fixture(width: u32, height: u32) -> Vec<u8> {
    (0..height)
        .flat_map(|y| {
            (0..width).flat_map(move |x| {
                [
                    ((x * 5 + y * 3 + (x ^ y)) & 0xff) as u8,
                    ((x * 2 + y * 11) & 0xff) as u8,
                    ((x * 13 + y * 8) & 0xff) as u8,
                    if (x / 32 + y / 32) % 3 == 0 {
                        0
                    } else {
                        (128 + (x ^ y) % 128) as u8
                    },
                ]
            })
        })
        .collect()
}

fn rgb_fixture(width: u32, height: u32) -> Vec<u8> {
    rgba_fixture(width, height)
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

struct Case {
    label: &'static str,
    encoded: Vec<u8>,
    output: DecodeOutputFormat,
}

fn encode_case(
    label: &'static str,
    image: &Image,
    rate_control: RateControl,
    decomposition_levels: Option<u8>,
    output: DecodeOutputFormat,
) -> Case {
    let encoded = encode(
        image,
        &EncodeOptions {
            rate_control: Some(rate_control),
            format: OutputFormat::Jp2,
            tile_policy: TilePolicy::Single,
            coding: CodingOptions {
                decomposition_levels,
                ..CodingOptions::default()
            },
            ..EncodeOptions::default()
        },
    )
    .unwrap_or_else(|error| panic!("{label}: encode failed: {error}"));
    Case {
        label,
        encoded,
        output,
    }
}

/// One case per decode-side site that fans out under a larger budget.
fn cases() -> Vec<Case> {
    let rgb = Image::from_rgb_bytes(WIDTH, HEIGHT, &rgb_fixture(WIDTH, HEIGHT)).expect("rgb");
    let rgba = Image::from_rgba_bytes(WIDTH, HEIGHT, &rgba_fixture(WIDTH, HEIGHT)).expect("rgba");
    vec![
        // Per-plane coarse synthesis, then the banded fused finest level.
        encode_case(
            "rgb 9/7 fused final level",
            &rgb,
            RateControl::ApproxQuality(75),
            None,
            DecodeOutputFormat::Rgb8,
        ),
        // No decomposition level: the planes go through `fuse_pack_f32_into`.
        encode_case(
            "rgb 9/7 f32 pack",
            &rgb,
            RateControl::ApproxQuality(75),
            Some(0),
            DecodeOutputFormat::Bgra8,
        ),
        // Reversible planes go through `fuse_pack_i32_into`.
        encode_case(
            "rgb 5/3 i32 pack",
            &rgb,
            RateControl::Lossless,
            None,
            DecodeOutputFormat::Rgb8,
        ),
        encode_case(
            "rgba 9/7 fused final level",
            &rgba,
            RateControl::ApproxQuality(75),
            None,
            DecodeOutputFormat::Rgba8,
        ),
        encode_case(
            "rgba 5/3 i32 pack",
            &rgba,
            RateControl::Lossless,
            None,
            DecodeOutputFormat::Rgba8,
        ),
    ]
}

fn request(case: &Case, concurrency: DecodeConcurrency) -> DecodeRequest {
    DecodeRequest {
        output: case.output,
        concurrency,
        ..DecodeRequest::default()
    }
}

/// `Jp2Decoder::decode` to a packed raster's bytes.
fn decode_bytes(case: &Case, concurrency: DecodeConcurrency) -> Vec<u8> {
    match Jp2Decoder::new()
        .decode(&case.encoded, &request(case, concurrency))
        .unwrap_or_else(|error| panic!("{}: {concurrency:?} decode: {error}", case.label))
    {
        DecodeResult::Raster(raster) => {
            assert_eq!((raster.width, raster.height), (WIDTH, HEIGHT));
            raster.data
        }
        DecodeResult::Native(_) => panic!("{}: expected a packed raster", case.label),
    }
}

/// `Jp2Decoder::decode_into`, the renderer's packed entry point.
fn decode_into_bytes(case: &Case, concurrency: DecodeConcurrency) -> Vec<u8> {
    let stride = WIDTH as usize * case.output.bytes_per_pixel();
    let mut data = vec![0u8; stride * HEIGHT as usize];
    Jp2Decoder::new()
        .decode_into(
            &case.encoded,
            &request(case, concurrency),
            DecodeTarget {
                data: &mut data,
                width: WIDTH,
                height: HEIGHT,
                stride,
                format: case.output,
                premultiplied: false,
            },
        )
        .unwrap_or_else(|error| panic!("{}: {concurrency:?} decode_into: {error}", case.label));
    data
}

/// Sets `stop` when dropped, so the thief exits even if the caller panics
/// and the scope does not wait on it forever.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Run `decode` on one worker of a two-thread pool while the other worker
/// steals, and return its result with the number of jobs the thief ran.
fn run_beside_thief<T: Send>(decode: impl FnOnce() -> T + Send) -> (T, usize) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .expect("two-thread pool");
    let started = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let stolen = AtomicUsize::new(0);
    let result = pool.scope(|scope| {
        scope.spawn(|_| {
            started.store(true, Ordering::SeqCst);
            while !stop.load(Ordering::SeqCst) {
                match rayon::yield_now() {
                    Some(rayon::Yield::Executed) => {
                        stolen.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => std::thread::yield_now(),
                }
            }
        });
        let _stop = StopOnDrop(&stop);
        // The thief must be running on the other worker before the decode
        // starts; this worker spins without touching Rayon, so it cannot run
        // the thief itself.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !started.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "the thief never started on the second worker"
            );
            std::thread::yield_now();
        }
        assert!(
            rayon::current_thread_index().is_some(),
            "the decode must run on a pool worker, like a renderer's"
        );
        decode()
    });
    (result, stolen.load(Ordering::SeqCst))
}

#[test]
fn single_worker_budget_spawns_no_rayon_work_inside_a_caller_pool() {
    let mut fanned_out = Vec::new();
    for case in cases() {
        for concurrency in [DecodeConcurrency::Serial, DecodeConcurrency::Budgeted(1)] {
            let (_, stolen) = run_beside_thief(|| decode_bytes(&case, concurrency));
            if stolen != 0 {
                fanned_out.push(format!(
                    "{}: {concurrency:?} decode ({stolen} jobs)",
                    case.label
                ));
            }
            let (_, stolen) = run_beside_thief(|| decode_into_bytes(&case, concurrency));
            if stolen != 0 {
                fanned_out.push(format!(
                    "{}: {concurrency:?} decode_into ({stolen} jobs)",
                    case.label
                ));
            }
        }
    }
    assert!(
        fanned_out.is_empty(),
        "single-worker decodes ran Rayon jobs on another worker of the caller's pool:\n{}",
        fanned_out.join("\n")
    );
}

#[test]
fn single_worker_and_budgeted_decodes_are_byte_identical() {
    for case in cases() {
        let budgeted = decode_bytes(&case, DecodeConcurrency::Budgeted(4));
        for concurrency in [DecodeConcurrency::Serial, DecodeConcurrency::Budgeted(1)] {
            assert!(
                decode_bytes(&case, concurrency) == budgeted,
                "{}: {concurrency:?} decode differs from Budgeted(4)",
                case.label
            );
            assert!(
                decode_into_bytes(&case, concurrency) == budgeted,
                "{}: {concurrency:?} decode_into differs from Budgeted(4)",
                case.label
            );
        }
        // The same request from inside a caller's pool, where the old code
        // took its parallel branches on that pool.
        let (inside, _) = run_beside_thief(|| decode_bytes(&case, DecodeConcurrency::Serial));
        assert!(
            inside == budgeted,
            "{}: Serial decode inside a pool differs from Budgeted(4)",
            case.label
        );
    }
}
