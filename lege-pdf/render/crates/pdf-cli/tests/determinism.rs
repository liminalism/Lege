#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! G1 level 2 (backend + scheduler): a page's pixels must not depend on what
//! else one backend rendered, in which order, or on how many workers.
//!
//! One `CpuBackend` (with its document-scoped glyph, font-program and
//! decoded-image caches, as lege holds one per session) renders a work list
//! of (page, size) items through `pdf-render-scheduler` at 1, 2 and 4
//! compile and render workers, in two orders, and every item's hash is
//! compared with a cold render on a fresh backend. The list renders every
//! page small and large, so cache keys that omit a size dimension surface:
//!
//! - B1: a reduced-scale DCT decode cached under a key without the scale and
//!   served to the large render (the DCT pages at both sizes);
//! - B3: the glyph-cache key quantised the per-design-unit scale at 1e-4,
//!   so 12.00 pt and 12.03 pt text shared whichever bitmap came first (the
//!   two Helvetica pages, small size).
//!
//! Both were confirmed by reintroducing them on top of the fixes: the B1 key
//! fails on the large upright DCT page in forward order, the B3 key on the
//! 12.03 pt page at the small size. The quarter-turn DCT page (B6's
//! placement) and the embedded Type 1 page widen the coverage; B6 itself
//! is not a determinism bug.
//!
//! Worker-local state (the font-program cache behind B2, surface reuse) is
//! not exercised here: the scheduler's backend builds a fresh worker context
//! per page. `lege-pdf/read/tests/determinism.rs` covers that seam.

use std::sync::Arc;

use pdf_content::PageCompiler;
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_page_ir::{DeviceSize, Matrix};
use pdf_render_api::{
    AnnotationMode, Background, HostPage, OutputFormat, OutputResidency, PageTransform,
    RenderBackend, RenderError, RenderLimits, RenderQuality, RenderRequest, RenderedPage,
};
use pdf_render_cpu::CpuBackend;
use pdf_render_scheduler::{RenderScheduler, SchedulerOptions};
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_test_support::builder::PdfBuilder;

/// Page contents, one per page; every page shares one resource dictionary.
const PAGES: [&[u8]; 6] = [
    b"BT /H 12 Tf 4 50 Td (Hamburgefonstiv) Tj ET",
    b"BT /H 12.03 Tf 4 50 Td (Hamburgefonstiv) Tj ET",
    b"BT /T 24 Tf 4 40 Td (Quick) Tj ET",
    b"q 80 0 0 20 10 40 cm /Im Do Q",
    b"q 0 80 -20 0 60 10 cm /Im Do Q",
    b"BT /F1 40 Tf 6 30 Td (AAA) Tj ET",
];

/// Device sizes every page is rendered at (square, from a 100x100 box).
const SIZES: [u32; 2] = [41, 157];

fn fixture() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let kids: Vec<String> = (0..PAGES.len()).map(|i| format!("{} 0 R", 3 + i)).collect();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        &format!(
            "<</Type/Pages/Kids[{}]/Count {}/MediaBox[0 0 100 100]>>",
            kids.join(" "),
            PAGES.len()
        ),
    );
    for (i, content) in PAGES.iter().enumerate() {
        let (page, stream) = (3 + i as u32, 30 + i as u32);
        b.add_object(
            page,
            &format!("<</Type/Page/Parent 2 0 R/Contents {stream} 0 R/Resources 9 0 R>>"),
        );
        b.add_stream(stream, "", content);
    }
    b.add_object(
        9,
        "<</Font<</H 20 0 R/T 21 0 R/F1 22 0 R>>/XObject<</Im 25 0 R>>>>",
    );
    b.add_object(20, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    b.add_object(21, "<</Type/Font/Subtype/Type1/BaseFont/Times-Roman>>");
    b.add_object(
        22,
        "<</Type/Font/Subtype/Type1/BaseFont/TestFont/FirstChar 65/LastChar 65\
         /Widths[600]/FontDescriptor 23 0 R>>",
    );
    b.add_object(
        23,
        "<</Type/FontDescriptor/FontName/TestFont/Flags 4/ItalicAngle 0/FontFile 24 0 R>>",
    );
    let t1 = pdf_test_support::fonts::minimal_type1();
    b.add_stream(
        24,
        &format!("/Length1 {}/Length2 0/Length3 0", t1.len()),
        &t1,
    );
    // A non-square, high-frequency greyscale JPEG: minified draws decode it
    // at a reduced DCT scale chosen from the footprint.
    let samples: Vec<u8> = (0..256u32 * 64)
        .map(|i| (((i % 256) * 37 ^ (i / 256) * 91).wrapping_mul(13) & 0xff) as u8)
        .collect();
    let jpeg = pdf_image::jpeg::encoder::write_jpeg(&samples, 256, 64, false, 90, false)
        .expect("encode jpeg");
    b.add_stream(
        25,
        "/Type/XObject/Subtype/Image/Width 256/Height 64/BitsPerComponent 8\
         /ColorSpace/DeviceGray/Filter/DCTDecode",
        &jpeg,
    );
    b.finish_classic_xref("/Root 1 0 R");
    b.into_bytes()
}

fn open(bytes: Vec<u8>) -> DocumentSnapshot {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes));
    DocumentSnapshot::open(source, DocumentLimits::default()).expect("open")
}

/// The work list: every page at every size, page-major.
fn items() -> Vec<(u32, u32)> {
    (0..PAGES.len() as u32)
        .flat_map(|page| SIZES.iter().map(move |&dim| (page, dim)))
        .collect()
}

/// Compile `page` on a fresh parse context and request it at `dim` square,
/// mapping the 100x100 box with a y-flip.
fn request(snapshot: &DocumentSnapshot, page: u32, dim: u32) -> Result<RenderRequest, RenderError> {
    let mut ctx = ParseContext::new();
    let compiled = PageCompiler::new()
        .compile(snapshot, PageIndex(page), &mut ctx)
        .map_err(|e| RenderError::Backend(format!("compile page {page}: {e}")))?;
    let scale = f64::from(dim) / 100.0;
    Ok(RenderRequest {
        page: Arc::new(compiled),
        transform: PageTransform {
            matrix: Matrix {
                a: scale,
                b: 0.0,
                c: 0.0,
                d: -scale,
                e: 0.0,
                f: f64::from(dim),
            },
        },
        crop: None,
        output_size: DeviceSize {
            width: dim,
            height: dim,
        },
        output_format: OutputFormat::Rgba8PremultipliedSrgb,
        background: Background::White,
        annotations: AnnotationMode::None,
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        quality: RenderQuality::Normal,
        limits: RenderLimits::default(),
        residency: OutputResidency::HostRequired,
    })
}

/// FNV-1a over the raster's size and pixels.
fn hash(host: &HostPage) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let size = [host.width.to_le_bytes(), host.height.to_le_bytes()].concat();
    for &byte in size.iter().chain(host.pixels.iter()) {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[test]
fn scheduled_renders_match_cold_renders_at_any_worker_count_and_order() {
    let snapshot = open(fixture());
    let items = items();
    // Ground truth: each item on its own fresh backend.
    let cold: Vec<u64> = items
        .iter()
        .map(|&(page, dim)| {
            let request = request(&snapshot, page, dim).unwrap();
            hash(&CpuBackend::default().render_to_host(&request).unwrap().0)
        })
        .collect();
    // The fixture is only meaningful if its near-equal sizes differ cold and
    // no page renders blank (two blank pages of one size would hash alike).
    assert_ne!(cold[0], cold[2], "12.00 and 12.03 pt render alike");
    let distinct: std::collections::HashSet<u64> = cold.iter().copied().collect();
    assert_eq!(distinct.len(), cold.len(), "two items render identically");

    let forward: Vec<usize> = (0..items.len()).collect();
    let reverse: Vec<usize> = forward.iter().rev().copied().collect();
    for workers in [1usize, 2, 4] {
        for (order_name, order) in [("forward", &forward), ("reverse", &reverse)] {
            for repeat in 0..2 {
                let backend: Arc<dyn RenderBackend> = Arc::new(CpuBackend::default());
                let scheduler = RenderScheduler::new(
                    backend,
                    SchedulerOptions {
                        compile_workers: workers,
                        render_workers: workers,
                        reorder_window_depth: SchedulerOptions::reorder_window_for(
                            workers, workers,
                        ),
                        ..SchedulerOptions::default()
                    },
                );
                let make_request = |snapshot: &DocumentSnapshot, position: PageIndex| {
                    let (page, dim) = items[order[position.0 as usize]];
                    request(snapshot, page, dim)
                };
                let mut got = vec![None; items.len()];
                scheduler.render_range(
                    &snapshot,
                    0..items.len() as u32,
                    &make_request,
                    None,
                    &mut |output| {
                        let item = order[output.page.0 as usize];
                        let page = output.result.expect("render");
                        let RenderedPage::Host(host) = page else {
                            panic!("host residency was requested");
                        };
                        got[item] = Some(hash(&host));
                    },
                );
                for (item, (&(page, dim), want)) in items.iter().zip(&cold).enumerate() {
                    assert_eq!(
                        got[item],
                        Some(*want),
                        "page {page} at {dim} px differs from its cold render \
                         ({workers} workers, {order_name} order, repeat {repeat})"
                    );
                }
            }
        }
    }
}
