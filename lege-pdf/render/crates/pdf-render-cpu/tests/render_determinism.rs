#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! Rendering must not depend on render history. A page rendered after other
//! pages through one backend and one worker context has to produce the bytes
//! it produces cold (fresh backend, fresh context), whatever was rendered
//! before it and in whatever order.
//!
//! Each test here pins one way that history used to leak into pixels:
//!
//! - the worker-local font-program cache recognised "the same page" by the
//!   `CompiledPage`'s address, so a different page at a reused address drew
//!   with the previous page's font programs;
//! - the shared glyph cache keyed a glyph's scale at 1e-4 per design unit
//!   while a miss rasterised the exact scale, so near-equal sizes shared
//!   whichever bitmap was inserted first.

use std::sync::Arc;

use pdf_page_ir::{
    Color, CompiledPage, DeviceSize, DisplayOp, FontId, FontResource, GlyphRun, GlyphRunId, Matrix,
    PageBounds, PageComplexity, PageFeatures, Paint, PaintId, PlacedGlyph, Rect, ResourceKey,
};
use pdf_render_api::{
    AnnotationMode, Background, HostPage, OutputFormat, OutputResidency, PageTransform,
    RenderLimits, RenderQuality, RenderRequest,
};
use pdf_render_cpu::{CpuBackend, CpuWorkerContext};
use pdf_test_support::fonts::{minimal_ttf, minimal_type1};

const DIM: u32 = 160;

/// A one-glyph page: glyph `gid` of `program` at `font_size`, placed by
/// translate(10, 10), drawn in black on white.
fn page(program: Arc<[u8]>, gid: u32, font_size: f64) -> CompiledPage {
    let run = GlyphRun {
        font: FontId(0),
        font_size,
        transform: Matrix::translate(10.0, 10.0),
        glyphs: Arc::from([PlacedGlyph {
            glyph: gid,
            x: 0.0,
            y: 0.0,
        }]),
        render_mode: 0,
    };
    CompiledPage {
        schema_version: pdf_page_ir::IR_SCHEMA_VERSION,
        content_bounds: None,
        bounds: PageBounds {
            crop: Rect {
                x0: 0.0,
                y0: 0.0,
                x1: DIM as f64,
                y1: DIM as f64,
            },
            rotate: 0,
        },
        operations: Arc::from([DisplayOp::DrawGlyphRun {
            run: GlyphRunId(0),
            paint: PaintId(0),
            alpha: 1.0,
            blend: pdf_page_ir::BlendMode::Normal,
            stroke: None,
        }]),
        paths: Arc::from([]),
        paints: Arc::from([Paint::Solid(Color::from_rgb(0.0, 0.0, 0.0))]),
        stroke_styles: Arc::from([]),
        glyph_runs: Arc::from([run]),
        fonts: Arc::from([FontResource {
            key: ResourceKey {
                object_number: 0,
                generation: 0,
                variant: 0,
            },
            program,
            face_index: 0,
            synthetic_shear: 0.0,
            synthetic_embolden_em: 0.0,
        }]),
        images: Arc::from([]),
        masks: Arc::from([]),
        groups: Arc::from([]),
        shadings: Arc::from([]),
        tilings: Arc::from([]),
        features: PageFeatures::TEXT,
        complexity: PageComplexity::default(),
    }
}

fn request(page: Arc<CompiledPage>) -> RenderRequest {
    RenderRequest {
        page,
        transform: PageTransform {
            matrix: Matrix::IDENTITY,
        },
        crop: None,
        output_size: DeviceSize {
            width: DIM,
            height: DIM,
        },
        output_format: OutputFormat::Rgba8PremultipliedSrgb,
        background: Background::White,
        annotations: AnnotationMode::None,
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        quality: RenderQuality::Normal,
        limits: RenderLimits::default(),
        residency: OutputResidency::HostRequired,
    }
}

/// Render `page` on a brand-new backend and worker context.
fn cold(page: &CompiledPage) -> HostPage {
    let backend = CpuBackend::default();
    let mut ctx = CpuWorkerContext::new();
    backend
        .render_with(&request(Arc::new(page.clone())), &mut ctx)
        .unwrap()
        .0
}

/// Panic with a short message (not two pixel dumps) when the rasters differ.
fn assert_same(got: &HostPage, want: &HostPage, what: &str) {
    assert_eq!(
        (got.width, got.height),
        (want.width, want.height),
        "{what}: size"
    );
    let differing = got
        .pixels
        .iter()
        .zip(want.pixels.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differing == 0,
        "{what}: {differing} bytes differ from the cold render"
    );
}

fn ink(host: &HostPage) -> usize {
    host.pixels.chunks_exact(4).filter(|px| px[0] < 128).count()
}

/// `minimal_type1` with its cleartext `/FontMatrix` doubled: a second,
/// distinct Type 1 program whose glyph `A` is twice as large. Type 1 is the
/// kind the worker-local cache retains between renders. The replacement has
/// the same length, so nothing after it moves.
fn larger_type1() -> Vec<u8> {
    let original = minimal_type1();
    let from: &[u8] = b"/FontMatrix [0.001 0 0 0.001 0 0]";
    let to: &[u8] = b"/FontMatrix [0.002 0 0 0.002 0 0]";
    assert_eq!(from.len(), to.len());
    let at = original
        .windows(from.len())
        .position(|w| w == from)
        .expect("minimal_type1 declares its FontMatrix in cleartext");
    let mut out = original;
    out[at..at + to.len()].copy_from_slice(to);
    out
}

/// The worker-local font-program cache must not serve a page another page's
/// programs because the two pages happen to live at the same address.
///
/// The second page is written into the first page's allocation
/// (`Arc::get_mut`), which is exactly what an allocator does when it reuses a
/// freed `CompiledPage`'s address for the next one.
#[test]
fn a_page_at_a_reused_address_does_not_inherit_the_previous_pages_fonts() {
    let gid_a = pdf_font::FontProgram::parse(Arc::from(minimal_type1()))
        .unwrap()
        .gid_for_name(b"A")
        .unwrap();
    let big = page(Arc::from(larger_type1()), gid_a, 50.0);
    let small = page(Arc::from(minimal_type1()), gid_a, 50.0);

    let small_cold = cold(&small);
    assert!(
        ink(&cold(&big)) > ink(&small_cold),
        "the two programs must draw visibly different glyphs"
    );

    let backend = CpuBackend::default();
    let mut ctx = CpuWorkerContext::new();
    let mut req = request(Arc::new(big));
    backend.render_with(&req, &mut ctx).unwrap();

    *Arc::get_mut(&mut req.page).expect("the backend keeps no page reference") = small;
    let small_after_big = backend.render_with(&req, &mut ctx).unwrap().0;

    assert_same(
        &small_after_big,
        &small_cold,
        "a page rendered at the previous page's address drew with that page's font program",
    );
}

/// Two text sizes close enough to fall in the same 1e-4 key bucket of the old
/// glyph-cache key must each render their own glyph, whichever is drawn first.
#[test]
fn near_equal_glyph_sizes_do_not_share_a_cached_bitmap() {
    let ttf: Arc<[u8]> = Arc::from(minimal_ttf());
    // la = font_size / 1000 per design unit: 0.1000 and 0.1004 both quantize
    // to 1000 at the old 1e-4 step.
    let a = page(ttf.clone(), 1, 100.0);
    let b = page(ttf, 1, 100.04);
    let a_cold = cold(&a);
    let b_cold = cold(&b);
    assert_ne!(
        a_cold.pixels, b_cold.pixels,
        "the sizes must differ in their rasterised edges for this test to mean anything"
    );

    for order in [[&a, &b], [&b, &a]] {
        let backend = CpuBackend::default();
        let mut ctx = CpuWorkerContext::new();
        let first = backend
            .render_with(&request(Arc::new(order[0].clone())), &mut ctx)
            .unwrap()
            .0;
        let second = backend
            .render_with(&request(Arc::new(order[1].clone())), &mut ctx)
            .unwrap()
            .0;
        let (first_cold, second_cold) = if std::ptr::eq(order[0], &a) {
            (&a_cold, &b_cold)
        } else {
            (&b_cold, &a_cold)
        };
        assert_same(&first, first_cold, "first render");
        assert_same(
            &second,
            second_cold,
            "a glyph drew from the bitmap cached for a slightly different size",
        );
    }
}
