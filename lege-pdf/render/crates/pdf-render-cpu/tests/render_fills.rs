#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! Phase 4a tests: the CPU backend paints solid fills from a CompiledPage,
//! anti-aliased, with correct alpha compositing and deterministic output.

use std::sync::Arc;

use pdf_page_ir::{
    Color, CompiledPage, DeviceSize, DisplayOp, FillRule, Matrix, PageBounds, PageComplexity,
    PageFeatures, Paint, PathData, PathVerb, Point, Rect,
};
use pdf_render_api::{
    AnnotationMode, Background, HostPage, OutputFormat, OutputResidency, PageTransform,
    RenderLimits, RenderQuality, RenderRequest,
};
use pdf_render_cpu::CpuBackend;

/// Build a CompiledPage with one solid-fill rectangle `[x0,y0]..[x1,y1]`.
#[allow(clippy::too_many_arguments)]
fn rect_page(
    size: f64,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    color: Color,
    alpha: f32,
    rule: FillRule,
) -> CompiledPage {
    let verbs: Arc<[PathVerb]> = Arc::from([
        PathVerb::MoveTo,
        PathVerb::LineTo,
        PathVerb::LineTo,
        PathVerb::LineTo,
        PathVerb::Close,
    ]);
    let points: Arc<[Point]> = Arc::from([
        Point { x: x0, y: y0 },
        Point { x: x1, y: y0 },
        Point { x: x1, y: y1 },
        Point { x: x0, y: y1 },
    ]);
    CompiledPage {
        schema_version: pdf_page_ir::IR_SCHEMA_VERSION,
        content_bounds: None,
        bounds: PageBounds {
            crop: Rect {
                x0: 0.0,
                y0: 0.0,
                x1: size,
                y1: size,
            },
            rotate: 0,
        },
        operations: Arc::from([DisplayOp::FillPath {
            path: pdf_page_ir::PathId(0),
            paint: pdf_page_ir::PaintId(0),
            rule,
            alpha,
            blend: pdf_page_ir::BlendMode::Normal,
        }]),
        paths: Arc::from([PathData { verbs, points }]),
        paints: Arc::from([Paint::Solid(color)]),
        stroke_styles: Arc::from([]),
        glyph_runs: Arc::from([]),
        fonts: Arc::from([]),
        images: Arc::from([]),
        masks: Arc::from([]),
        groups: Arc::from([]),
        shadings: Arc::from([]),
        tilings: Arc::from([]),
        features: PageFeatures::BASIC_PATHS,
        complexity: PageComplexity::default(),
    }
}

fn request(page: CompiledPage, dim: u32, background: Background) -> RenderRequest {
    RenderRequest {
        page: Arc::new(page),
        // Identity transform: IR user space maps 1:1 to device pixels (y-down).
        transform: PageTransform {
            matrix: Matrix::IDENTITY,
        },
        crop: None,
        output_size: DeviceSize {
            width: dim,
            height: dim,
        },
        output_format: OutputFormat::Rgba8PremultipliedSrgb,
        background,
        annotations: AnnotationMode::None,
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        quality: RenderQuality::Normal,
        limits: RenderLimits::default(),
        residency: OutputResidency::HostRequired,
    }
}

fn px(host: &HostPage, x: usize, y: usize) -> [u8; 4] {
    let i = y * host.stride + x * 4;
    [
        host.pixels[i],
        host.pixels[i + 1],
        host.pixels[i + 2],
        host.pixels[i + 3],
    ]
}

#[test]
fn opaque_fill_paints_interior_and_leaves_background() {
    let page = rect_page(
        10.0,
        2.0,
        2.0,
        8.0,
        8.0,
        Color::from_rgb(1.0, 0.0, 0.0),
        1.0,
        FillRule::NonZero,
    );
    let backend = CpuBackend::default();
    let (host, stats) = backend
        .render_to_host(&request(page, 10, Background::White))
        .unwrap();

    assert_eq!(px(&host, 5, 5), [255, 0, 0, 255], "interior is opaque red");
    assert_eq!(px(&host, 0, 0), [255, 255, 255, 255], "corner stays white");
    assert_eq!(stats.commands, 1);
    assert_eq!(stats.ops_painted, 1);
    // Integer-aligned + opaque → the OpaqueRect fast path (direct row fill, no
    // coverage generation), so no edges are rasterized.
    assert_eq!(stats.edges, 0, "opaque integer rect uses the fast path");
}

#[test]
fn night_policy_maps_vector_black_to_soft_text_color() {
    let page = rect_page(
        4.0,
        0.0,
        0.0,
        4.0,
        4.0,
        Color::BLACK,
        1.0,
        FillRule::NonZero,
    );
    let mut request = request(
        page,
        4,
        Background::Solid(Color::from_rgb(
            0x25 as f32 / 255.0,
            0x25 as f32 / 255.0,
            0x25 as f32 / 255.0,
        )),
    );
    request.color_policy = pdf_render_api::RenderColorPolicy::Night {
        paper_rgb: [0x25, 0x25, 0x25],
        text_rgb: [0xd8, 0xd1, 0xc4],
    };
    let (host, _) = CpuBackend::default().render_to_host(&request).unwrap();
    let pixel = px(&host, 2, 2);
    assert!((i16::from(pixel[0]) - 0xd8).abs() <= 1);
    assert!((i16::from(pixel[1]) - 0xd1).abs() <= 1);
    assert!((i16::from(pixel[2]) - 0xc4).abs() <= 1);
}

#[test]
fn constant_alpha_composites_over_white() {
    // 50% red over white → premultiplied [255,128,128,255] (pink).
    let page = rect_page(
        10.0,
        2.0,
        2.0,
        8.0,
        8.0,
        Color::from_rgb(1.0, 0.0, 0.0),
        0.5,
        FillRule::NonZero,
    );
    let backend = CpuBackend::default();
    let (host, _) = backend
        .render_to_host(&request(page, 10, Background::White))
        .unwrap();
    let p = px(&host, 5, 5);
    assert_eq!(p[0], 255);
    assert!((p[1] as i32 - 128).abs() <= 1, "g={}", p[1]);
    assert!((p[2] as i32 - 128).abs() <= 1, "b={}", p[2]);
    assert_eq!(p[3], 255);
}

#[test]
fn edges_are_antialiased() {
    // A rectangle with a fractional edge at x=4.5 leaves the boundary column
    // partially covered — neither fully red nor fully white.
    let page = rect_page(
        10.0,
        2.0,
        2.0,
        4.5,
        8.0,
        Color::from_rgb(1.0, 0.0, 0.0),
        1.0,
        FillRule::NonZero,
    );
    let backend = CpuBackend::default();
    let (host, _) = backend
        .render_to_host(&request(page, 10, Background::White))
        .unwrap();
    // Column 4 straddles the edge (covers x in [4.0,4.5]) → ~50% red.
    let p = px(&host, 4, 5);
    assert!(p[0] == 255, "r stays 255 (red over white)");
    assert!(p[1] > 100 && p[1] < 200, "partial coverage g={}", p[1]);
}

#[test]
fn output_is_deterministic() {
    let make = || {
        rect_page(
            20.0,
            3.0,
            3.0,
            17.0,
            15.0,
            Color::from_rgb(0.1, 0.4, 0.9),
            0.8,
            FillRule::NonZero,
        )
    };
    let backend = CpuBackend::default();
    let (a, _) = backend
        .render_to_host(&request(make(), 20, Background::White))
        .unwrap();
    let (b, _) = backend
        .render_to_host(&request(make(), 20, Background::White))
        .unwrap();
    assert_eq!(a.pixels, b.pixels);
}

/// A 612x792 page: a full-page grey rectangle, a 24-unit black stroke
/// across it, and a red triangle with a corrupt 1e9 coordinate.
fn page_spanning_page() -> CompiledPage {
    use pdf_page_ir::{LineCap, LineJoin, PaintId, PathId, StrokeStyle, StrokeStyleId};
    let path = |pts: &[(f64, f64)], close: bool| {
        let mut verbs = vec![PathVerb::MoveTo];
        verbs.extend(std::iter::repeat_n(PathVerb::LineTo, pts.len() - 1));
        if close {
            verbs.push(PathVerb::Close);
        }
        PathData {
            verbs: verbs.into(),
            points: pts.iter().map(|&(x, y)| Point { x, y }).collect(),
        }
    };
    let mut page = rect_page(
        612.0,
        0.3,
        0.3,
        611.7,
        791.7,
        Color::from_rgb(0.6, 0.6, 0.6),
        1.0,
        FillRule::NonZero,
    );
    page.bounds.crop.y1 = 792.0;
    page.paths = Arc::from([
        path(
            &[(0.3, 0.3), (611.7, 0.3), (611.7, 791.7), (0.3, 791.7)],
            true,
        ),
        path(&[(0.0, 18.0), (640.0, 846.0)], false),
        path(&[(100.0, 100.0), (1.0e9, 120.0), (100.0, 140.0)], true),
    ]);
    page.paints = Arc::from([
        Paint::Solid(Color::from_rgb(0.6, 0.6, 0.6)),
        Paint::Solid(Color::from_rgb(0.0, 0.0, 0.0)),
        Paint::Solid(Color::from_rgb(1.0, 0.0, 0.0)),
    ]);
    page.stroke_styles = Arc::from([StrokeStyle {
        width: 24.0,
        cap: LineCap::Butt,
        join: LineJoin::Miter,
        miter_limit: 10.0,
        dash_pattern: Vec::new().into(),
        dash_phase: 0.0,
    }]);
    page.operations = Arc::from([
        DisplayOp::FillPath {
            path: PathId(0),
            paint: PaintId(0),
            rule: FillRule::NonZero,
            alpha: 1.0,
            blend: pdf_page_ir::BlendMode::Normal,
        },
        DisplayOp::StrokePath {
            path: PathId(1),
            paint: PaintId(1),
            style: StrokeStyleId(0),
            alpha: 1.0,
            blend: pdf_page_ir::BlendMode::Normal,
        },
        DisplayOp::FillPath {
            path: PathId(2),
            paint: PaintId(2),
            rule: FillRule::NonZero,
            alpha: 1.0,
            blend: pdf_page_ir::BlendMode::Normal,
        },
    ]);
    page
}

/// B11: an 8x8 window far from the origin (a region crop: the request
/// translates the page and shrinks the output) paints the page-spanning
/// rectangle and stroke exactly as the full render does. The corrupt-geometry
/// limit used to be 64 x the *request* size (512 px here), so both
/// primitives, whose corners lie ~3000 px from the window, were dropped and
/// the window stayed white. Corrupt geometry is still dropped.
#[test]
fn small_crop_window_keeps_page_spanning_paths() {
    let (scale, wx, wy, n) = (4.0, 2400usize, 3100usize, 8usize);
    let mut full = request(page_spanning_page(), 0, Background::White);
    full.transform.matrix = Matrix::scale(scale, scale);
    full.output_size = DeviceSize {
        width: 2448,
        height: 3168,
    };
    let mut window = request(page_spanning_page(), n as u32, Background::White);
    window.transform.matrix =
        Matrix::scale(scale, scale).then(Matrix::translate(-(wx as f64), -(wy as f64)));
    let backend = CpuBackend::default();
    let (full, _) = backend.render_to_host(&full).unwrap();
    let (window, _) = backend.render_to_host(&window).unwrap();
    let mut grey_or_black = 0;
    for y in 0..n {
        for x in 0..n {
            let (w, f) = (px(&window, x, y), px(&full, wx + x, wy + y));
            assert_eq!(
                w,
                f,
                "window pixel ({x},{y}) vs full ({},{})",
                wx + x,
                wy + y
            );
            assert_ne!(w, [255, 0, 0, 255], "corrupt geometry painted");
            if w != [255, 255, 255, 255] {
                grey_or_black += 1;
            }
        }
    }
    assert_eq!(grey_or_black, n * n, "the window is covered by the page");
    // The stroke crosses the window: it is not all page grey.
    let stroke = (0..n)
        .flat_map(|y| (0..n).map(move |x| (x, y)))
        .filter(|&(x, y)| px(&window, x, y)[0] < 150)
        .count();
    assert!(stroke > 0, "the stroke reaches the window");
}
