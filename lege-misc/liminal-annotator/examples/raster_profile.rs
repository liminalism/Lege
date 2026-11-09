//! Page raster cost by zoom and thread count: `cargo run --release --example raster_profile -- page.jpg`.

use std::time::Instant;

use liminal_annotator::annotation::geometry::Point;
use liminal_annotator::canvas::draw::{PageRaster, render_page};
use liminal_annotator::canvas::viewport::{CanvasRect, ViewTransform};
use liminal_annotator::image_io::decode::{decode_page, synthetic_page};

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
        for zoom in [1.0, 1.2, 1.0 / fit.scale, 3.0 / fit.scale] {
            let mut view = fit;
            view.zoom_at(Point::new(w as f64 / 2.0, h as f64 / 2.0), zoom);
            for threads in [1usize, 4, 8] {
                let mut r = PageRaster::default();
                let mut times = Vec::new();
                for i in 0..40 {
                    let mut v = view;
                    v.origin.x += (i % 2) as f64; // defeat nothing: every call renders
                    let t = Instant::now();
                    render_page(&mut r, &img, &v, 0, 40, w, h, threads);
                    times.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(|a, b| a.total_cmp(b));
                println!(
                    "{w}x{h} scale {:>5.2} threads {threads}: p50 {:>6.2} ms  p95 {:>6.2} ms",
                    view.scale, times[20], times[38]
                );
            }
        }
    }
}
