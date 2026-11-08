//! `session-bench`: time whole-document renders through `lege_pdf_read::RenderSession`,
//! the seam lege-process renders through, on the calling thread.
//!
//! ```text
//! session-bench <pdf> <dpi> <rgb8|gray8> [passes]
//! ```
//!
//! Opens one session (as lege does per document), then makes `passes`
//! (default 2) sequential passes over every page: compile, then render at
//! `dpi` (crop box, `/Rotate` applied). The first pass is cold (fresh session:
//! parse, font, glyph and image caches empty); later passes reuse the session,
//! like lege's analysis and base renders of one page. Prints one line:
//!
//! ```text
//! pass_ms <p0> <p1> ...  pages <n>  checksum <hex>
//! ```
//!
//! Optional flags (anywhere after the positionals):
//!
//! * `--alternate <dpi2>`: per page, first render at `dpi2` (the analysis-sized
//!   render), then at `dpi` (the base render), on one thread, as lege-process
//!   alternates sizes. Time covers both; the checksum covers the base renders
//!   only, so it equals the default mode's checksum.
//! * `--threads N`: split the pages round-robin over N threads sharing one
//!   session (lege's shape). Checksums are combined in page order.
//!
//! The checksum (FNV-1a over every plane of the last pass) lets an A/B run
//! confirm both binaries produced the same bytes. Run it single-threaded and
//! pinned for timing (see the README's environment line) and compare binaries
//! with `ab.py`, which interleaves runs and reports paired ratios.

use std::sync::Arc;
use std::time::Instant;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use lege_pdf_read::{RasterPlane, RasterProduct, RenderSession};

fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn main() {
    let mut a: Vec<String> = Vec::new();
    let mut alternate: Option<f64> = None;
    let mut threads: usize = 1;
    let mut it = std::env::args();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--alternate" => alternate = Some(it.next().expect("--alternate dpi2").parse().expect("dpi2")),
            "--threads" => threads = it.next().expect("--threads N").parse().expect("N"),
            _ => a.push(arg),
        }
    }
    if a.len() < 4 {
        eprintln!("usage: session-bench <pdf> <dpi> <rgb8|gray8> [passes]");
        std::process::exit(2);
    }
    let dpi: f64 = a[2].parse().expect("dpi");
    let gray = match a[3].as_str() {
        "gray8" => true,
        "rgb8" => false,
        other => panic!("format must be rgb8 or gray8, got {other}"),
    };
    let passes: usize = a.get(4).map(|v| v.parse().expect("passes")).unwrap_or(2);
    let bytes: Arc<[u8]> = Arc::from(std::fs::read(&a[1]).expect("read pdf"));

    let session = Arc::new(RenderSession::open(Arc::clone(&bytes), None).expect("open"));
    let n = session.page_count();
    let make_products = |dpi: f64| -> Vec<RasterProduct> {
        (0..n)
            .map(|p| {
                let g = session.page_geometry(p).expect("geometry");
                let [x0, y0, x1, y1] = g.crop_box;
                let (mut w, mut h) = ((x1 - x0).abs(), (y1 - y0).abs());
                if g.rotate % 180 == 90 {
                    std::mem::swap(&mut w, &mut h);
                }
                let s = dpi / 72.0;
                let (w, h) = (((w * s).round() as u32).max(1), ((h * s).round() as u32).max(1));
                if gray {
                    RasterProduct::gray8(w, h)
                } else {
                    RasterProduct::rgb8(w, h)
                }
            })
            .collect()
    };
    let products = make_products(dpi);
    let products2 = alternate.map(make_products);

    let page_hash = |p: u32, seed: u64| -> u64 {
        if let Some(pr2) = &products2 {
            let page = session.compile(p).expect("compile");
            let _ = session.render(&page, &pr2[p as usize]).expect("render");
            let plane = session.render(&page, &products[p as usize]).expect("render");
            return plane_hash(seed, &plane);
        }
        let page = session.compile(p).expect("compile");
        let plane = session.render(&page, &products[p as usize]).expect("render");
        plane_hash(seed, &plane)
    };

    let mut times = Vec::with_capacity(passes);
    let mut checksum = 0xcbf2_9ce4_8422_2325u64;
    for pass in 0..passes {
        let last = pass + 1 == passes;
        let t = Instant::now();
        let mut hashes = vec![0u64; n as usize];
        if threads <= 1 {
            // Chained over every plane, as the original harness did.
            for p in 0..n {
                let seed = if last { checksum } else { SEED };
                let h = page_hash(p, seed);
                if last {
                    checksum = h;
                }
            }
        } else {
            let results: Vec<Vec<(u32, u64)>> = std::thread::scope(|sc| {
                let hs: Vec<_> = (0..threads)
                    .map(|t| {
                        let page_hash = &page_hash;
                        sc.spawn(move || {
                            (0..n).filter(|p| *p as usize % threads == t).map(|p| (p, page_hash(p, SEED))).collect()
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for (p, h) in results.into_iter().flatten() {
                hashes[p as usize] = h;
            }
        }
        times.push(t.elapsed().as_secs_f64() * 1e3);
        if last && threads > 1 {
            for h in hashes {
                checksum = fnv1a(checksum, &h.to_le_bytes());
            }
        }
    }
    let times: Vec<String> = times.iter().map(|t| format!("{t:.3}")).collect();
    println!("pass_ms {}  pages {n}  checksum {checksum:016x}", times.join(" "));
}

const SEED: u64 = 0xcbf2_9ce4_8422_2325u64;

fn plane_hash(h: u64, plane: &RasterPlane) -> u64 {
    match plane {
        RasterPlane::Rgb8(s) => fnv1a(h, &s.pixels),
        RasterPlane::Gray8(s) => fnv1a(h, &s.pixels),
    }
}
