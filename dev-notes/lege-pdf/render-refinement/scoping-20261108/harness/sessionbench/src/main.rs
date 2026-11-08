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
//! The checksum (FNV-1a over every plane of the last pass) lets an A/B run
//! confirm both binaries produced the same bytes. Run it single-threaded and
//! pinned for timing (see the README's environment line) and compare binaries
//! with `ab.py`, which interleaves runs and reports paired ratios.

use std::sync::Arc;
use std::time::Instant;

use lege_pdf_read::{RasterPlane, RasterProduct, RenderSession};

fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
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

    let session = RenderSession::open(Arc::clone(&bytes), None).expect("open");
    let n = session.page_count();
    let products: Vec<RasterProduct> = (0..n)
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
        .collect();

    let mut times = Vec::with_capacity(passes);
    let mut checksum = 0xcbf2_9ce4_8422_2325u64;
    for pass in 0..passes {
        let last = pass + 1 == passes;
        let t = Instant::now();
        for p in 0..n {
            let page = session.compile(p).expect("compile");
            let plane = session.render(&page, &products[p as usize]).expect("render");
            if last {
                checksum = match &plane {
                    RasterPlane::Rgb8(s) => fnv1a(checksum, &s.pixels),
                    RasterPlane::Gray8(s) => fnv1a(checksum, &s.pixels),
                };
            }
        }
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let times: Vec<String> = times.iter().map(|t| format!("{t:.3}")).collect();
    println!("pass_ms {}  pages {n}  checksum {checksum:016x}", times.join(" "));
}
