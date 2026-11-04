//! Per-pixel timing of PDF-embedded JBIG2 page decodes.
//!
//! ```text
//! cargo run --release --example generic_decode_timing --no-default-features \
//!     --features decode -- <iterations> <stream.jb2>...
//! ```
//!
//! Each argument is a bare embedded JBIG2 segment stream (the bytes of a PDF
//! `/JBIG2Decode` stream without `/JBIG2Globals`). Every stream is decoded
//! `iterations` times through [`decode_embedded_with_context`] with one reused
//! [`DecoderContext`], the way the renderer's worker-local adapter decodes it.
//! The program prints the minimum and median wall time per decode, the same
//! in nanoseconds per page pixel, and an FNV-1a hash of the decoded page words
//! so two builds can be checked for identical output.

use std::time::Instant;

use jbig2enc_rust::decode::{DecodeOptions, DecoderContext, decode_embedded_with_context};

fn fnv1a(words: impl Iterator<Item = u32>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for w in words {
        for b in w.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: generic_decode_timing <iterations> <stream.jb2>...");
        std::process::exit(2);
    }
    let iterations: usize = args[1].parse().unwrap_or(20).max(1);
    let options = DecodeOptions::default();
    let mut ctx = DecoderContext::new();
    for path in &args[2..] {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{path}: {e}");
                std::process::exit(1);
            }
        };
        let mut times = Vec::with_capacity(iterations);
        let mut hash = 0u64;
        let mut pixels = 0u64;
        let mut ink = 0u64;
        // One untimed warm-up decode sizes the context's pooled buffers.
        for i in 0..=iterations {
            let start = Instant::now();
            let page = match decode_embedded_with_context(None, &data, &options, &mut ctx) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{path}: decode failed: {e}");
                    std::process::exit(1);
                }
            };
            let elapsed = start.elapsed().as_secs_f64();
            if i == 0 {
                pixels = page.width() as u64 * page.height() as u64;
                hash = fnv1a((0..page.height()).flat_map(|y| page.row(y).iter().copied()));
                ink = (0..page.height())
                    .flat_map(|y| page.row(y).iter())
                    .map(|w| w.count_ones() as u64)
                    .sum();
            } else {
                times.push(elapsed);
            }
            std::hint::black_box(page);
        }
        times.sort_by(f64::total_cmp);
        let min = times[0];
        let median = times[times.len() / 2];
        let ns_px = |t: f64| t * 1e9 / pixels.max(1) as f64;
        println!(
            "{path}: {pixels} px ({:.1}% set), min {:.3} ms ({:.3} ns/px), median {:.3} ms ({:.3} ns/px), hash {hash:016x}",
            ink as f64 * 100.0 / pixels.max(1) as f64,
            min * 1e3,
            ns_px(min),
            median * 1e3,
            ns_px(median),
        );
    }
}
