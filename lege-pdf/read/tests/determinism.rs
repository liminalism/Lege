#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! G1 level 2 (read seam): a page rendered through `RenderSession` must give
//! the bytes it gives cold, whatever page the same worker thread compiled
//! and rendered before it.
//!
//! The read seam keeps one parse context and one raster worker context per
//! thread for a document's lifetime, so worker-local caches persist across
//! pages. With the page cache at capacity 0 every compiled page is freed
//! after use, and the next one tends to reuse its address: the trigger for
//! B2, where the worker-local font-program cache recognised "the same page"
//! by the `CompiledPage` address and a page drew with the previous page's
//! Type 1 program (8 of 8 runs at capacity 0 in the review).
//!
//! The fixture alternates a Type 1 font, a TrueType font and a Type 1 font
//! with a doubled `/FontMatrix`, all under the one resource name `/F1`.
//! Real threads drain a shared page cursor in two orders, at 1 and 4
//! threads and page-cache capacities 0 and 8; every page's hash must equal
//! a cold render on a fresh session on a fresh thread.
//!
//! Confirmed by reintroducing B2 on top of its fix (the font-program cache
//! keyed by the raw page address): the doubled-matrix neighbour's program
//! leaks into page 3 at capacity 0, one thread, forward order.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lege_pdf_read::{RasterFormat, RasterPlane, RasterProduct, RenderSession};
use pdf_test_support::builder::PdfBuilder;
use pdf_test_support::fonts::{minimal_ttf, minimal_type1};

const CYCLES: u32 = 3;
const DIM: u32 = 120;

/// `minimal_type1` with its cleartext `/FontMatrix` doubled: a distinct
/// Type 1 program whose glyph is twice as large, of the same length.
fn larger_type1() -> Vec<u8> {
    let original = minimal_type1();
    let from: &[u8] = b"/FontMatrix [0.001 0 0 0.001 0 0]";
    let to: &[u8] = b"/FontMatrix [0.002 0 0 0.002 0 0]";
    let at = original
        .windows(from.len())
        .position(|w| w == from)
        .expect("minimal_type1 declares its FontMatrix in cleartext");
    let mut out = original;
    out[at..at + to.len()].copy_from_slice(to);
    out
}

/// `CYCLES` x (Type 1, TrueType, doubled Type 1) pages, each drawing `AA`
/// with its own font object under the resource name `/F1`.
fn fixture() -> Vec<u8> {
    let pages = 3 * CYCLES;
    let mut b = PdfBuilder::new();
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 10 + i)).collect();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        &format!(
            "<</Type/Pages/Kids[{}]/Count {pages}/MediaBox[0 0 100 100]>>",
            kids.join(" ")
        ),
    );
    // Three fonts: objects 3/4/5 (font dicts), 6/7/8 (descriptors), and the
    // programs at 100/101/102.
    let programs: [(&str, &str, Vec<u8>); 3] = [
        ("Type1", "FontFile", minimal_type1()),
        ("TrueType", "FontFile2", minimal_ttf()),
        ("Type1", "FontFile", larger_type1()),
    ];
    for (k, (subtype, file_key, program)) in programs.iter().enumerate() {
        let (font, descriptor, file) = (3 + k as u32, 6 + k as u32, 100 + k as u32);
        b.add_object(
            font,
            &format!(
                "<</Type/Font/Subtype/{subtype}/BaseFont/Test{k}/FirstChar 65/LastChar 65\
                 /Widths[600]/FontDescriptor {descriptor} 0 R>>"
            ),
        );
        b.add_object(
            descriptor,
            &format!(
                "<</Type/FontDescriptor/FontName/Test{k}/Flags 4/ItalicAngle 0\
                 /FontBBox[0 0 1000 1000]/{file_key} {file} 0 R>>"
            ),
        );
        let lengths = if *file_key == "FontFile" {
            format!("/Length1 {}/Length2 0/Length3 0", program.len())
        } else {
            format!("/Length1 {}", program.len())
        };
        b.add_stream(file, &lengths, program);
    }
    for i in 0..pages {
        let (page, content) = (10 + i, 50 + i);
        let font = 3 + i % 3;
        b.add_object(
            page,
            &format!(
                "<</Type/Page/Parent 2 0 R/Contents {content} 0 R\
                 /Resources<</Font<</F1 {font} 0 R>>>>>>"
            ),
        );
        b.add_stream(content, "", b"BT /F1 40 Tf 8 30 Td (AA) Tj ET");
    }
    b.finish_classic_xref("/Root 1 0 R");
    b.into_bytes()
}

fn product() -> RasterProduct {
    RasterProduct {
        width: DIM,
        height: DIM,
        format: RasterFormat::Rgb8,
        crop: None,
        decode_reference: None,
    }
}

/// FNV-1a over the plane's pixels.
fn hash(plane: &RasterPlane) -> u64 {
    let pixels = match plane {
        RasterPlane::Rgb8(s) => &s.pixels,
        RasterPlane::Gray8(s) => &s.pixels,
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in pixels.iter() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Compile and render `page` on `session` on the calling thread.
fn render(session: &RenderSession, page: u32) -> u64 {
    let compiled = session.compile(page).expect("compile");
    let plane = session.render(&compiled, &product()).expect("render");
    hash(&plane)
}

#[test]
fn read_seam_renders_match_cold_renders_whatever_the_thread_rendered_before() {
    let bytes: Arc<[u8]> = Arc::from(fixture());
    let pages = 3 * CYCLES;
    // Cold truth: every page on a fresh session on a fresh thread (fresh
    // thread-local worker contexts).
    let cold: Vec<u64> = (0..pages)
        .map(|page| {
            let bytes = bytes.clone();
            std::thread::spawn(move || {
                let session = RenderSession::open(bytes, None).expect("open");
                render(&session, page)
            })
            .join()
            .expect("cold render thread")
        })
        .collect();
    // The test fonts' glyphs share one outline, so the Type 1 and TrueType
    // pages render alike; the doubled program must differ from both, which
    // is what makes a page drawn with its neighbour's program visible.
    assert_ne!(
        cold[0], cold[2],
        "the doubled Type 1 program must draw larger"
    );
    assert_ne!(
        cold[1], cold[2],
        "the doubled Type 1 program must draw larger"
    );

    let forward: Vec<u32> = (0..pages).collect();
    let reverse: Vec<u32> = forward.iter().rev().copied().collect();
    for capacity in [0usize, 8] {
        for threads in [1usize, 4] {
            for (order_name, order) in [("forward", &forward), ("reverse", &reverse)] {
                let session =
                    RenderSession::open_with_cache_capacity(bytes.clone(), None, capacity)
                        .expect("open");
                let cursor = AtomicUsize::new(0);
                let got: Vec<(u32, u64)> = std::thread::scope(|scope| {
                    let workers: Vec<_> = (0..threads)
                        .map(|_| {
                            scope.spawn(|| {
                                let mut out = Vec::new();
                                loop {
                                    let at = cursor.fetch_add(1, Ordering::Relaxed);
                                    let Some(&page) = order.get(at) else {
                                        break out;
                                    };
                                    out.push((page, render(&session, page)));
                                }
                            })
                        })
                        .collect();
                    workers
                        .into_iter()
                        .flat_map(|w| w.join().expect("worker thread"))
                        .collect()
                });
                assert_eq!(got.len(), pages as usize);
                for (page, h) in got {
                    assert_eq!(
                        h,
                        cold[page as usize],
                        "page {page} (font {}) differs from its cold render \
                         (capacity {capacity}, {threads} threads, {order_name} order)",
                        page % 3
                    );
                }
            }
        }
    }
}
