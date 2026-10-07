//! Deterministic evidence rendering: one page, one PNG, the same bytes twice.
//!
//! Evidence artifacts must be reproducible: the same document rendered on two
//! runs — or on two machines — yields byte-identical PNGs, so a changed byte
//! later means the document changed, not the toolchain. Every choice here
//! serves that property:
//!
//! * The page is compiled with [`PageCompiler`](pdf_content::PageCompiler)
//!   *without* a system-font provider, so non-embedded fonts resolve to the
//!   bundled standard faces instead of whatever the host has installed.
//!   System-font substitution is what would make two machines disagree.
//! * Rasterization runs on the normative CPU backend
//!   ([`CpuBackend`](pdf_render_cpu::CpuBackend)) with its defaults: no
//!   hinting, normal quality, opaque white background, original colors.
//! * The PNG writer uses fixed encoder settings and writes no timestamps.
//!
//! Crate shape is preserved: page compilation (via `pdf-content`) and
//! rasterization (via `pdf-render-cpu`) are neutral facts — what the page
//! looks like — while the diff and the changed-region extraction below are
//! this crate's interpretation of two such facts.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use pdf_content::PageCompiler;
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_page_ir::{DeviceSize, Matrix, Rect};
use pdf_render_api::{
    Background, OutputFormat, OutputResidency, PageTransform, RenderLimits, RenderQuality,
    RenderRequest,
};
use pdf_render_cpu::CpuBackend;
use pdf_source::{OwnedBytesSource, PdfSource};

/// How annotations participate in an evidence render.
///
/// Re-exported from the render API so callers name the same vocabulary the
/// renderer documents: [`AnnotationMode::None`] compiles page content only,
/// [`AnnotationMode::StaticAppearances`] (the default) also bakes each
/// annotation's normal appearance stream into the page, matching what a
/// viewer shows.
pub use pdf_render_api::AnnotationMode;

/// Options for one evidence render.
#[derive(Debug, Clone)]
pub struct EvidenceOptions {
    /// Device pixels per PDF user-space unit. The default renders at 150 DPI
    /// (PDF user space is 72 units per inch).
    pub scale: f64,
    /// Whether annotation appearance streams are part of the render.
    pub annotations: AnnotationMode,
}

impl Default for EvidenceOptions {
    fn default() -> Self {
        Self {
            scale: 150.0 / 72.0,
            annotations: AnnotationMode::default(),
        }
    }
}

/// One deterministically rendered page: 8-bit RGBA, premultiplied alpha, sRGB
/// transfer — the frozen [`OutputFormat::Rgba8PremultipliedSrgb`] surface —
/// with tight rows (`pixels.len() == width * height * 4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRender {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl EvidenceRender {
    /// Build a render from caller-supplied tight RGBA rows.
    ///
    /// This is the seam the diff uses for synthetic inputs (and tests): it
    /// enforces the same invariants real renders carry — nonzero dimensions
    /// and an exactly-sized buffer — so a malformed buffer is an error, never
    /// a mis-sized image.
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, EvidenceError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|count| count.checked_mul(4));
        match expected {
            Some(len) if width > 0 && height > 0 && pixels.len() == len => Ok(Self {
                width,
                height,
                pixels,
            }),
            _ => Err(EvidenceError::DimensionMismatch {
                detail: format!(
                    "buffer of {} bytes is not {width}x{height} tight RGBA",
                    pixels.len()
                ),
            }),
        }
    }

    /// Rendered width in device pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Rendered height in device pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Tight top-down RGBA rows.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

/// Why evidence rendering, diffing, or PNG writing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceError {
    /// The document bytes did not open.
    OpenFailed {
        /// The open error, rendered.
        error: String,
    },
    /// The requested page is outside `0..page_count`.
    PageOutOfRange {
        /// The requested 0-based page number.
        page: u32,
    },
    /// The scale is not a positive finite number.
    InvalidScale,
    /// The page's crop box has no area at the requested scale.
    EmptyPage,
    /// The page did not compile.
    CompileFailed {
        /// The compile error, rendered.
        error: String,
    },
    /// The CPU backend did not render the compiled page.
    RenderFailed {
        /// The render error, rendered.
        error: String,
    },
    /// Two renders, or a buffer and its claimed dimensions, do not line up.
    DimensionMismatch {
        /// What did not match.
        detail: String,
    },
    /// PNG encoding or writing failed.
    PngFailed {
        /// The encoder or I/O error, rendered.
        error: String,
    },
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvidenceError::OpenFailed { error } => write!(f, "evidence open failed: {error}"),
            EvidenceError::PageOutOfRange { page } => {
                write!(f, "evidence page {page} out of range")
            }
            EvidenceError::InvalidScale => write!(f, "evidence scale must be positive and finite"),
            EvidenceError::EmptyPage => write!(f, "evidence page has no area at this scale"),
            EvidenceError::CompileFailed { error } => {
                write!(f, "evidence compile failed: {error}")
            }
            EvidenceError::RenderFailed { error } => {
                write!(f, "evidence render failed: {error}")
            }
            EvidenceError::DimensionMismatch { detail } => {
                write!(f, "evidence dimension mismatch: {detail}")
            }
            EvidenceError::PngFailed { error } => write!(f, "evidence PNG failed: {error}"),
        }
    }
}

impl std::error::Error for EvidenceError {}

/// The user-space to device-pixel mapping behind [`render_page_evidence`]:
/// the device matrix plus the output dimensions for `crop`/`rotate` at
/// `scale`.
///
/// This is the single source of truth for the mapping, shared by rendering
/// and by the overlay analysis that projects glyph boxes into device pixels.
/// Callers must use the same bounds the render used: lowering preserves the
/// semantic page's bounds, so a semantic compilation's bounds reproduce the
/// render's dimensions exactly.
pub(crate) fn evidence_device_mapping(
    crop: Rect,
    rotate: u16,
    scale: f64,
) -> Result<(Matrix, u32, u32), EvidenceError> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(EvidenceError::InvalidScale);
    }
    // `/Rotate` 90/270 swaps the displayed extent; the device matrix applies
    // the rotation as clockwise quarter turns (CPDF_Page::GetDisplayMatrix
    // semantics), translating the crop origin to (0, 0).
    let (width, height) = match rotate {
        90 | 270 => (
            (crop.height() * scale).ceil() as u32,
            (crop.width() * scale).ceil() as u32,
        ),
        _ => (
            (crop.width() * scale).ceil() as u32,
            (crop.height() * scale).ceil() as u32,
        ),
    };
    if width == 0 || height == 0 {
        return Err(EvidenceError::EmptyPage);
    }
    let s = scale;
    let matrix = match rotate {
        90 => Matrix {
            a: 0.0,
            b: s,
            c: s,
            d: 0.0,
            e: -crop.y0 * s,
            f: -crop.x0 * s,
        },
        180 => Matrix {
            a: -s,
            b: 0.0,
            c: 0.0,
            d: s,
            e: crop.x1 * s,
            f: -crop.y0 * s,
        },
        270 => Matrix {
            a: 0.0,
            b: -s,
            c: -s,
            d: 0.0,
            e: crop.y1 * s,
            f: crop.x1 * s,
        },
        _ => Matrix {
            a: s,
            b: 0.0,
            c: 0.0,
            d: -s,
            e: -crop.x0 * s,
            f: crop.y1 * s,
        },
    };
    Ok((matrix, width, height))
}

/// Render `page` (0-based) of an already-open document.
///
/// The page is compiled without a system-font provider — system-font
/// substitution stays disabled, so the render depends only on the document —
/// then rasterized on the CPU backend. `options.annotations` maps onto the
/// compiler's annotation pass exactly as the `pdfr` driver maps it.
pub fn render_page_evidence(
    snapshot: &DocumentSnapshot,
    page: u32,
    options: &EvidenceOptions,
) -> Result<EvidenceRender, EvidenceError> {
    if page >= snapshot.page_count() {
        return Err(EvidenceError::PageOutOfRange { page });
    }
    if !options.scale.is_finite() || options.scale <= 0.0 {
        return Err(EvidenceError::InvalidScale);
    }
    // No `with_system_fonts`: that opt-in is precisely the substitution this
    // module keeps off for determinism.
    let compiler = PageCompiler::new().with_annotations(matches!(
        options.annotations,
        AnnotationMode::StaticAppearances
    ));
    let mut ctx = ParseContext::new();
    let compiled = compiler
        .compile(snapshot, PageIndex(page), &mut ctx)
        .map_err(|err| EvidenceError::CompileFailed {
            error: err.to_string(),
        })?;
    let compiled = Arc::new(compiled);

    let (matrix, width, height) =
        evidence_device_mapping(compiled.bounds.crop, compiled.bounds.rotate, options.scale)?;
    let request = RenderRequest {
        page: Arc::clone(&compiled),
        transform: PageTransform { matrix },
        crop: None,
        output_size: DeviceSize { width, height },
        output_format: OutputFormat::Rgba8PremultipliedSrgb,
        background: Background::White,
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        annotations: options.annotations,
        quality: RenderQuality::Normal,
        limits: RenderLimits::default(),
        residency: OutputResidency::HostRequired,
    };
    // Default backend options: default codecs, no hinting. Hinting moves glyph
    // geometry, so the normative unhinted default is the deterministic one.
    let backend = CpuBackend::default();
    let (host, _stats) =
        backend
            .render_to_host(&request)
            .map_err(|err| EvidenceError::RenderFailed {
                error: err.to_string(),
            })?;

    // Tighten rows: the host surface may stride wider than the pixel row.
    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| EvidenceError::RenderFailed {
            error: "output width overflow".to_owned(),
        })?;
    if host.width != width
        || host.height != height
        || host.format != OutputFormat::Rgba8PremultipliedSrgb
        || host.stride < row_bytes
    {
        return Err(EvidenceError::RenderFailed {
            error: "backend returned an unexpected surface".to_owned(),
        });
    }
    let mut pixels = Vec::with_capacity(row_bytes.saturating_mul(height as usize));
    for y in 0..height as usize {
        let start = y.saturating_mul(host.stride);
        let row = host
            .pixels
            .get(start..start.saturating_add(row_bytes))
            .ok_or_else(|| EvidenceError::RenderFailed {
                error: "backend surface buffer is short".to_owned(),
            })?;
        pixels.extend_from_slice(row);
    }
    EvidenceRender::from_rgba(width, height, pixels)
}

/// Open `bytes` and render `page` (0-based) of it.
///
/// The same open path — and the same recovery behaviour — as a full-file
/// open; `password` is tried against an encrypted document exactly as
/// [`audit_bytes`](crate::intake::audit_bytes) tries it.
pub fn render_bytes_evidence(
    bytes: &[u8],
    page: u32,
    password: Option<&str>,
    options: &EvidenceOptions,
) -> Result<EvidenceRender, EvidenceError> {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes.to_vec()));
    let snapshot =
        DocumentSnapshot::open_with_password(source, DocumentLimits::default(), password).map_err(
            |err| EvidenceError::OpenFailed {
                error: err.to_string(),
            },
        )?;
    render_page_evidence(&snapshot, page, options)
}

/// One 4-connected component of changed pixels: its bounding box plus how many
/// of the box's pixels actually changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangedRegion {
    /// Left edge in device pixels.
    pub x: u32,
    /// Top edge in device pixels.
    pub y: u32,
    /// Box width in device pixels.
    pub width: u32,
    /// Box height in device pixels.
    pub height: u32,
    /// Changed pixels inside the box.
    pub pixels: u32,
}

/// The difference between two renders of the same dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderDiff {
    /// How many pixels differ (exact RGBA byte comparison).
    pub changed_pixels: u64,
    /// Rendered width both inputs share.
    pub width: u32,
    /// Rendered height both inputs share.
    pub height: u32,
    /// Changed-pixel components in top-to-bottom, left-to-right order.
    pub regions: Vec<ChangedRegion>,
}

/// Diff two renders pixel by pixel and extract the connected changed regions.
///
/// A pixel counts as changed when any of its four RGBA bytes differs — there
/// is no tolerance, because a tolerance would itself need justifying per
/// document. Changed pixels join one region under 4-connectivity (shared
/// edges, not corners). Regions come out ordered by top edge, then left edge,
/// so the same pair of renders always yields the same list.
pub fn diff_renders(
    before: &EvidenceRender,
    after: &EvidenceRender,
) -> Result<RenderDiff, EvidenceError> {
    if before.width != after.width || before.height != after.height {
        return Err(EvidenceError::DimensionMismatch {
            detail: format!(
                "cannot diff {}x{} against {}x{}",
                before.width, before.height, after.width, after.height
            ),
        });
    }
    let width = before.width as usize;
    let height = before.height as usize;
    let mut changed = vec![false; width.saturating_mul(height)];
    let mut changed_pixels: u64 = 0;
    for (index, (a, b)) in before
        .pixels
        .chunks_exact(4)
        .zip(after.pixels.chunks_exact(4))
        .enumerate()
    {
        if a != b {
            if let Some(slot) = changed.get_mut(index) {
                *slot = true;
            }
            changed_pixels = changed_pixels.saturating_add(1);
        }
    }

    // Flood-fill each 4-connected component in scan order, which is already
    // the documented region order.
    let mut visited = vec![false; changed.len()];
    let mut regions = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let start = y.saturating_mul(width).saturating_add(x);
            let is_change = changed.get(start) == Some(&true);
            let seen = visited.get(start) == Some(&true);
            if !is_change || seen {
                continue;
            }
            let (mut x0, mut x1, mut y0, mut y1) = (x, x, y, y);
            let mut pixels: u32 = 0;
            let mut stack = vec![(x, y)];
            if let Some(slot) = visited.get_mut(start) {
                *slot = true;
            }
            while let Some((cx, cy)) = stack.pop() {
                pixels = pixels.saturating_add(1);
                x0 = x0.min(cx);
                x1 = x1.max(cx);
                y0 = y0.min(cy);
                y1 = y1.max(cy);
                let neighbours = [
                    cx.checked_sub(1).map(|nx| (nx, cy)),
                    cx.checked_add(1).map(|nx| (nx, cy)),
                    cy.checked_sub(1).map(|ny| (cx, ny)),
                    cy.checked_add(1).map(|ny| (cx, ny)),
                ];
                for next in neighbours.into_iter().flatten() {
                    let (nx, ny) = next;
                    if nx >= width || ny >= height {
                        continue;
                    }
                    let at = ny.saturating_mul(width).saturating_add(nx);
                    let is_change = changed.get(at) == Some(&true);
                    let seen = visited.get(at) == Some(&true);
                    if is_change && !seen {
                        if let Some(slot) = visited.get_mut(at) {
                            *slot = true;
                        }
                        stack.push((nx, ny));
                    }
                }
            }
            regions.push(ChangedRegion {
                x: x0 as u32,
                y: y0 as u32,
                width: (x1 - x0 + 1) as u32,
                height: (y1 - y0 + 1) as u32,
                pixels,
            });
        }
    }
    Ok(RenderDiff {
        changed_pixels,
        width: before.width,
        height: before.height,
        regions,
    })
}

/// Paint the changed regions of `diff` as opaque-red one-pixel outlines over
/// a copy of `base` — the "diff PNG" behind [`encode_png`].
///
/// `base` is usually the after-revision render, so the overlay shows the
/// current page with exactly what changed boxed. Outlines are drawn in
/// region order; overlapping boxes union visually. Region edges are clamped
/// to the image, so a region touching the border draws a partial box rather
/// than failing.
pub fn highlight_changed_regions(
    base: &EvidenceRender,
    diff: &RenderDiff,
) -> Result<EvidenceRender, EvidenceError> {
    if base.width() != diff.width || base.height() != diff.height {
        return Err(EvidenceError::DimensionMismatch {
            detail: format!(
                "cannot overlay {}x{} regions on {}x{}",
                diff.width,
                diff.height,
                base.width(),
                base.height()
            ),
        });
    }
    let width = base.width();
    let height = base.height();
    let mut pixels = base.pixels().to_vec();
    for region in &diff.regions {
        let x0 = region.x.min(width.saturating_sub(1));
        let y0 = region.y.min(height.saturating_sub(1));
        let x1 = region
            .x
            .saturating_add(region.width.saturating_sub(1))
            .min(width.saturating_sub(1));
        let y1 = region
            .y
            .saturating_add(region.height.saturating_sub(1))
            .min(height.saturating_sub(1));
        for x in x0..=x1 {
            paint_red(&mut pixels, width, x, y0);
            paint_red(&mut pixels, width, x, y1);
        }
        for y in y0..=y1 {
            paint_red(&mut pixels, width, x0, y);
            paint_red(&mut pixels, width, x1, y);
        }
    }
    EvidenceRender::from_rgba(width, height, pixels)
}

/// Set one pixel of tight RGBA rows to opaque red, ignoring coordinates
/// outside the image rather than failing.
fn paint_red(pixels: &mut [u8], width: u32, x: u32, y: u32) {
    let at = (y as usize)
        .saturating_mul(width as usize)
        .saturating_add(x as usize)
        .saturating_mul(4);
    if let Some(pixel) = pixels.get_mut(at..at.saturating_add(4)) {
        pixel.copy_from_slice(&[255, 0, 0, 255]);
    }
}

/// Encode a render as a PNG with fixed encoder settings.
///
/// No timestamps, no ancillary chunks, one filter and compression choice, so
/// the same pixels always encode to the same bytes.
pub fn encode_png(render: &EvidenceRender) -> Result<Vec<u8>, EvidenceError> {
    let mut bytes = Vec::new();
    encode_png_to_writer(&mut bytes, render)?;
    Ok(bytes)
}

/// Write a render to `path` as a PNG (see [`encode_png`]).
pub fn write_png(path: &Path, render: &EvidenceRender) -> Result<(), EvidenceError> {
    let file = std::fs::File::create(path).map_err(|err| EvidenceError::PngFailed {
        error: format!("cannot create {}: {err}", path.display()),
    })?;
    let mut writer = std::io::BufWriter::new(file);
    encode_png_to_writer(&mut writer, render)
}

fn encode_png_to_writer<W: std::io::Write>(
    writer: W,
    render: &EvidenceRender,
) -> Result<(), EvidenceError> {
    let mut encoder = png::Encoder::new(writer, render.width, render.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|err| EvidenceError::PngFailed {
            error: format!("PNG header failed: {err}"),
        })?;
    writer
        .write_image_data(render.pixels())
        .map_err(|err| EvidenceError::PngFailed {
            error: format!("PNG pixels failed: {err}"),
        })
}
