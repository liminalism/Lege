//! Page raster cost by zoom and thread count: `cargo run --release --example raster_profile -- page.jpg`.

use std::time::Instant;

use liminal_annotator::annotation::geometry::Point;
use liminal_annotator::canvas::draw::{PageRaster, render_page};
use liminal_annotator::canvas::viewport::{CanvasRect, ViewTransform};
use liminal_annotator::image_io::decode::{decode_page, synthetic_page};

const ITERS: usize = 200;

fn main() {
    let img = match std::env::args().nth(1) {
        Some(p) => decode_page(std::path::Path::new(&p)).expect("decode"),
        None => synthetic_page(850, 1280, 3),
    };
    println!("image {}x{}", img.width(), img.height());
    for (w, h) in [(1920u32, 1016u32), (3840, 2032)] {
        let canvas = CanvasRect {
            x: 0.0,
            y: 40.0,
            w: w as f64,
            h: h as f64,
        };
        let fit = ViewTransform::fit(canvas, img.width() as f64, img.height() as f64, 8.0);
        let mut cases: Vec<(&str, ViewTransform)> = Vec::new();
        for zoom in [1.0, 1.2, 1.0 / fit.scale, 3.0 / fit.scale] {
            let mut view = fit;
            view.zoom_at(Point::new(w as f64 / 2.0, h as f64 / 2.0), zoom);
            cases.push(("", view));
        }
        // Exactly 100% on whole pixels: the straight-copy path.
        cases.push((
            "100% exact",
            ViewTransform {
                origin: Point::new(((w - img.width()) / 2) as f64, 40.0),
                scale: 1.0,
            },
        ));
        for (label, view) in cases {
            for threads in [1usize, 4, 8] {
                let mut r = PageRaster::default();
                let mut times = Vec::new();
                for i in 0..ITERS {
                    let mut v = view;
                    v.origin.x += (i % 2) as f64; // defeat nothing: every call renders
                    let t = Instant::now();
                    render_page(&mut r, &img, &v, 0, 40, w, h, threads);
                    times.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(|a, b| a.total_cmp(b));
                println!(
                    "{w}x{h} scale {:>5.2} threads {threads}: p50 {:>6.2} ms  p95 {:>6.2} ms {label}",
                    view.scale,
                    times[ITERS / 2],
                    times[ITERS * 95 / 100]
                );
            }
        }
    }

    // Strips: what a pan or a partial update draws, by thread count.
    let w = 1920u32;
    // The page's top-left sits at the strip's, so every strip row is on the page.
    let view = ViewTransform {
        origin: Point::new(0.37, 40.25),
        scale: 1.2,
    };
    for h in [32u32, 64, 128, 256, 512] {
        for threads in [1usize, 2, 4, 8] {
            let mut r = PageRaster::default();
            let mut times = Vec::new();
            for i in 0..ITERS {
                let mut v = view;
                v.origin.x += (i % 2) as f64;
                let t = Instant::now();
                render_page(&mut r, &img, &v, 0, 40, w, h, threads);
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(|a, b| a.total_cmp(b));
            println!(
                "strip {w}x{h} ({:>4} kpx) threads {threads}: p50 {:>6.3} ms  p95 {:>6.3} ms",
                (w * h) / 1000,
                times[ITERS / 2],
                times[ITERS * 95 / 100]
            );
        }
    }
}
