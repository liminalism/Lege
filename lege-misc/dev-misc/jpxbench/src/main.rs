//! Paired serial micro-benchmark: jp2lam vs hayro-jpeg2000 on raw JPX streams.
//!
//! usage: jpxbench [--iters N] [--stats] [--levels 0,1] [--check] [--only jp2|hay] FILE...
use std::time::Instant;

use jp2lam::{
    DecodeConcurrency, DecodeOutputFormat, DecodeRequest, DecodeResolution, DecodeResult,
};

fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

struct Options {
    iters: usize,
    stats: bool,
    check: bool,
    levels: Vec<u8>,
    files: Vec<String>,
    only: Option<String>,
}

fn parse_args() -> Options {
    let mut iters = 7usize;
    let mut stats = false;
    let mut check = false;
    let mut levels: Vec<u8> = vec![0, 1];
    let mut files = Vec::new();
    let mut only: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--iters" => iters = args.next().unwrap().parse().unwrap(),
            "--stats" => stats = true,
            "--check" => check = true,
            "--only" => only = Some(args.next().unwrap()),
            "--levels" => {
                levels = args
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|s| s.parse().unwrap())
                    .collect()
            }
            _ => files.push(a),
        }
    }
    Options {
        iters,
        stats,
        check,
        levels,
        files,
        only,
    }
}

fn main() {
    let opts = parse_args();
    println!(
        "{:<22} {:>3} {:>11} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
        "stream",
        "lvl",
        "out",
        "jp2min",
        "jp2med",
        "haymin",
        "haymed",
        "jp2MB/s",
        "hayMB/s",
        "ratio"
    );
    for file in &opts.files {
        let bytes = std::fs::read(file).unwrap();
        let name = std::path::Path::new(file)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let meta = jp2lam::inspect_jp2(&bytes).unwrap();
        if opts.stats {
            let cod = &meta.codestream.cod;
            println!(
                "# {name}: {}x{} in={}B {:?}",
                meta.width,
                meta.height,
                bytes.len(),
                cod
            );
        }
        for &lvl in &opts.levels {
            bench_level(&name, &bytes, &meta, lvl, &opts);
        }
    }
}

/// One stream at one reduction level: both decoders, interleaved, then the
/// timing row (and the requested checks and stage statistics).
fn bench_level(name: &str, bytes: &[u8], meta: &jp2lam::DecodeMetadata, lvl: u8, opts: &Options) {
    let (ow, oh) = meta
        .decoded_dimensions(DecodeResolution::ReduceLevels(lvl))
        .unwrap();
    let out_bytes = (ow as f64) * (oh as f64) * 3.0;
    // --- jp2lam ---
    let mut dec = jp2lam::Jp2Decoder::new();
    let req = DecodeRequest {
        resolution: DecodeResolution::ReduceLevels(lvl),
        output: DecodeOutputFormat::Rgb8,
        concurrency: DecodeConcurrency::Serial,
        ..Default::default()
    };
    let run_jp2 = |dec: &mut jp2lam::Jp2Decoder| -> (f64, Vec<u8>) {
        let t = Instant::now();
        let r = dec.decode(bytes, &req).unwrap();
        let e = t.elapsed().as_secs_f64() * 1e3;
        let r = match r {
            DecodeResult::Raster(r) => r,
            _ => panic!("native"),
        };
        assert_eq!((r.width, r.height), (ow, oh));
        (e, r.data)
    };
    // --- hayro ---
    let settings = hayro_jpeg2000::DecodeSettings {
        target_resolution: if lvl == 0 {
            None
        } else {
            Some((meta.width >> lvl, meta.height >> lvl))
        },
        ..Default::default()
    };
    let run_hay = || -> (f64, Vec<u8>) {
        let t = Instant::now();
        let img = hayro_jpeg2000::Image::new(bytes, &settings).unwrap();
        let mut ctx = hayro_jpeg2000::DecoderContext::default();
        let d = img.decode(&mut ctx).unwrap();
        let data = d.data_u8();
        let e = t.elapsed().as_secs_f64() * 1e3;
        (e, data)
    };
    let only = &opts.only;
    let mut tj = Vec::new();
    let mut th = Vec::new();
    let (_, jdata) = run_jp2(&mut dec);
    // FNV-1a over the jp2lam raster: bit-identity check across builds.
    println!(
        "# {name} lvl{lvl}: jp2lam raster hash {:016x}",
        fnv1a(&jdata)
    );
    let (_, hdata) = if only.as_deref() == Some("jp2") {
        (0.0, Vec::new())
    } else {
        run_hay()
    };
    for _ in 0..opts.iters {
        if only.as_deref() != Some("hay") {
            tj.push(run_jp2(&mut dec).0);
        }
        if only.as_deref() != Some("jp2") {
            th.push(run_hay().0);
        }
    }
    if opts.check && !hdata.is_empty() {
        print_raster_diff(name, lvl, &jdata, &hdata);
    }
    let jmin = tj.iter().cloned().fold(f64::INFINITY, f64::min);
    let hmin = th.iter().cloned().fold(f64::INFINITY, f64::min);
    let jmed = if tj.is_empty() {
        f64::NAN
    } else {
        median(&mut tj)
    };
    let hmed = if th.is_empty() {
        f64::NAN
    } else {
        median(&mut th)
    };
    println!(
        "{:<22} {:>3} {:>5}x{:<5} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.1} {:>8.1} {:>6.3}",
        name,
        lvl,
        ow,
        oh,
        jmin,
        jmed,
        hmin,
        hmed,
        out_bytes / 1e6 / (jmed / 1e3),
        out_bytes / 1e6 / (hmed / 1e3),
        jmed / hmed
    );
    if opts.stats {
        #[cfg(feature = "hayro-trace")]
        print_hayro_trace(lvl, th.len());
        print_jp2lam_stats(&mut dec, bytes, &req, lvl);
    }
}

/// FNV-1a, 64-bit.
fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// `--check`: how far hayro's raster is from jp2lam's.
fn print_raster_diff(name: &str, lvl: u8, jdata: &[u8], hdata: &[u8]) {
    if hdata.len() != jdata.len() {
        println!(
            "# {name} lvl{lvl}: size mismatch: hayro {} B vs jp2lam {} B",
            hdata.len(),
            jdata.len()
        );
        return;
    }
    let mut maxd = 0i32;
    let mut sum = 0u64;
    for (a, b) in jdata.iter().zip(hdata.iter()) {
        let d = (*a as i32 - *b as i32).abs();
        maxd = maxd.max(d);
        sum += d as u64;
    }
    println!(
        "# {name} lvl{lvl}: max abs diff {maxd}, mean {:.4}",
        sum as f64 / jdata.len() as f64
    );
}

/// `--stats` with the patched hayro: its per-stage times, per run.
#[cfg(feature = "hayro-trace")]
fn print_hayro_trace(lvl: u8, runs: usize) {
    let n = runs.max(1) as f64;
    hayro_jpeg2000::TRACE.with(|cell| {
        let t = *cell.borrow();
        println!(
            "#   hayro lvl{lvl} (per run): parse {:.1} t2 {:.1} t1 {:.1} idwt {:.1} store {:.1} mct {:.1} shift {:.1}",
            t.parse / n, t.t2 / n, t.t1 / n, t.idwt / n, t.store / n, t.mct / n, t.shift / n
        );
        *cell.borrow_mut() = Default::default();
    });
}

/// `--stats`: one more jp2lam decode, with its per-stage times.
fn print_jp2lam_stats(dec: &mut jp2lam::Jp2Decoder, bytes: &[u8], req: &DecodeRequest, lvl: u8) {
    let (_, s) = dec.decode_with_stats(bytes, req).unwrap();
    let ms = |n: u64| n as f64 / 1e6;
    println!(
        "#   jp2lam lvl{lvl} (packed): total {:.1} | t2 setup {:.1} hdr {:.1} | t1 {:.1} (mq {:.1} sig {:.1} ref {:.1} cln {:.1} out {:.1}) | deq {:.1} dwt {:.1} (h {:.1} v {:.1}) mct {:.1} fin {:.1} pack {:.1} | blocks {} passes s/r/c {}/{}/{} cw {}B",
        ms(s.total_ns),
        ms(s.tier2_setup_ns),
        ms(s.tier2_packet_headers_ns),
        ms(s.tier1_total_ns),
        ms(s.tier1_mq_ns),
        ms(s.tier1_significance_ns),
        ms(s.tier1_refinement_ns),
        ms(s.tier1_cleanup_ns),
        ms(s.tier1_block_output_ns),
        ms(s.dequantize_ns),
        ms(s.dwt_total_ns),
        ms(s.dwt_horizontal_ns),
        ms(s.dwt_vertical_ns),
        ms(s.inverse_mct_ns),
        ms(s.finalize_ns),
        ms(s.output_pack_ns),
        s.codeblocks,
        s.significance_passes,
        s.refinement_passes,
        s.cleanup_passes,
        s.codeword_bytes
    );
}
