//! Flattened occlusion candidates: page-content paint covering earlier text.
//!
//! Where [`detect_annotation_overlays`](crate::detect_annotation_overlays)
//! compares annotation-off and annotation-on renders, this module reads a
//! single flattened page: an opaque rectangle, form, or image painted over
//! earlier text inside the page content stream itself, optionally with
//! replacement text on top. For every page this module:
//!
//! 1. compiles the page with the audit sidecar
//!    ([`PageCompiler::compile_semantic_traced`](pdf_content::PageCompiler::compile_semantic_traced))
//!    and walks the semantic ops in painter order, tracking the graphics
//!    state (fill/stroke paint and alpha, blend mode, line width, soft
//!    masks, transparency groups) to find opaque path and image paints;
//! 2. renders the page's diagnostic attribution map
//!    (`pdf_render_cpu::attribution`) plus the glyph-stamp mask, and
//!    intersects both with the extracted glyph boxes to find which earlier
//!    text each opaque paint covers and how much of its glyph area later
//!    paint won;
//! 3. splits the text inside each cover by painter order — runs shown
//!    before the cover are the covered text, runs shown after it the
//!    replacement text.
//!
//! Each covering paint yields one [`PageOcclusion`]: a
//! [`PAGE_CONTENT_COVERS_TEXT`] (path or form paint) or
//! [`RASTER_PATCH_ON_VECTOR_PAGE`] (image patch) finding at
//! [`EvidenceStrength::ContextualIndicator`] with the page-trace
//! provenance (operator, source stream object, byte span, paint origin,
//! nested-object stack), the covered and replacement texts, and the
//! glyph-area coverage, plus the page render as evidence.
//!
//! What is deliberately out of scope: translucent paint, non-normal blend
//! modes, soft masks, shadings, and pattern fills never yield a finding —
//! only solid paint at full alpha and normal blend is provably opaque —
//! and text lowered as outlines or placement boxes (non-fill render modes,
//! rotated or oversized runs) is invisible to the glyph-pixel
//! intersection, which bounds recall, never precision.

use std::collections::HashMap;
use std::sync::Arc;

use pdf_content::semantic::{ImageId, PathId, TextRunId};
use pdf_content::{PageCompiler, PageTrace, SemColor, SemanticOp, SemanticPage};
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_page_ir::{BlendMode, DeviceSize, Matrix, PaintLeaf, PaintOrigin, Point, Rect};
use pdf_render_api::{
    AnnotationMode, Background, OutputFormat, OutputResidency, PageTransform, RenderLimits,
    RenderQuality, RenderRequest,
};
use pdf_render_cpu::CpuBackend;
use pdf_render_cpu::attribution::{AttributionMap, glyph_stamp_mask, render_attribution};
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_text::{CharInfo, CharType, TextPage, TextPageOptions};

use crate::document::ParseAssessment;
use crate::evidence::{
    EvidenceOptions, EvidenceRender, evidence_device_mapping, render_page_evidence,
};
use crate::report::{AnalysisMode, EvidenceStrength, Finding, FindingCode};

/// Opaque path or form paint inside the page content covers earlier text.
pub const PAGE_CONTENT_COVERS_TEXT: FindingCode = FindingCode::new("PAGE_CONTENT_COVERS_TEXT");

/// A raster image patch inside the page content covers earlier vector text.
pub const RASTER_PATCH_ON_VECTOR_PAGE: FindingCode =
    FindingCode::new("RASTER_PATCH_ON_VECTOR_PAGE");

/// Minimum glyph-area coverage for a candidate to report: one glyph pixel in
/// ten won by later paint. A rule or underline kissing descenders stays
/// below it; a whiteout box or image patch lands near 1.0.
const MIN_COVERAGE: f64 = 0.1;

/// The shared limitation every occlusion finding carries: geometry and
/// painter order report no intent, the opacity model is stated, and shared
/// pixels between overlapping covers are disclosed.
const LIMITATION: &str = "A covering paint reports geometry and painter order, not intent: \
    flattened form fields and stamps, applied redactions, and design elements cover text \
    legitimately. Only solid paint at full alpha and normal blend without a soft mask is \
    considered — translucent paint, blend modes, soft masks, shadings, and pattern fills \
    never yield a finding — and coverage counts glyph pixels won by later path or image \
    paint, so overlapping covers on one page share pixels and each finding's coverage is \
    measured against its own rectangle.";

/// Options for [`detect_flattened_occlusion`].
#[derive(Debug, Clone, Default)]
pub struct OcclusionOptions {
    /// Evidence-render options for the per-page image. The scale shapes the
    /// PNG evidence and the attribution sampling; the `annotations` field is
    /// ignored — the analysis always compiles page content only.
    pub evidence: EvidenceOptions,
}

/// What kind of paint covers the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcclusionKind {
    /// An opaque path fill or stroke ([`PAGE_CONTENT_COVERS_TEXT`]).
    Path,
    /// An opaque image patch ([`RASTER_PATCH_ON_VECTOR_PAGE`]).
    Image,
}

/// One page-content paint covering earlier text.
#[derive(Debug, Clone, PartialEq)]
pub struct PageOcclusion {
    /// The finding: [`PAGE_CONTENT_COVERS_TEXT`] for path paint or
    /// [`RASTER_PATCH_ON_VECTOR_PAGE`] for image paint, at
    /// [`EvidenceStrength::ContextualIndicator`] with a stated limitation,
    /// needing [`AnalysisMode::Full`]. Its explanation names the page, the
    /// covering operator and its provenance, the covered text ops, both
    /// texts, and the coverage, so the finding reads standalone in JSON.
    pub finding: Finding,
    /// Zero-based page index carrying the cover.
    pub page: u32,
    /// Whether a path or an image covers the text.
    pub kind: OcclusionKind,
    /// Semantic op index of the covering paint, in painter order.
    pub covering_op: usize,
    /// The covering operator keyword as written (`f`, `Do`), or
    /// `(synthesized)` when the paint op carries no source operator.
    pub operator: String,
    /// Execution sequence number of the covering content operator:
    /// 1-based, in execution order across the page and its nested streams.
    pub content_op_index: u64,
    /// The source stream object as `N G R`, or `None` for a direct stream
    /// or a synthesized op.
    pub stream: Option<String>,
    /// Byte span of the covering operator within its decoded source
    /// stream, or `(0, 0)` for a synthesized op.
    pub span: (usize, usize),
    /// The paint origin of the cover: `page-content` or `form-xobject`.
    pub origin: String,
    /// The nested-object stack that produced the cover, outermost first,
    /// as `form-xobject 61 0 R` entries; empty for page-level paint.
    pub stack: Vec<String>,
    /// The covering paint's user-space bounds in thousandths of a point
    /// (`[x0, y0, x1, y1]`, normalized).
    pub rect: [i64; 4],
    /// The earlier text the cover hides, in extraction order.
    pub covered_text: String,
    /// Text shown after the cover inside its rectangle, in extraction
    /// order; empty when the cover carries no replacement text.
    pub replacement_text: String,
    /// Glyph-area coverage: the fraction (0.0–1.0) of the covered
    /// characters' glyph-stamped device pixels whose attribution winner is
    /// later path or image paint. Always at least one glyph pixel in ten —
    /// a cover that wins no glyph pixel reports no finding.
    pub coverage: f64,
    /// The page rendered without annotation appearances.
    pub render: EvidenceRender,
}

/// A page — or the document itself — the occlusion analysis could not
/// compare, with why. Skips are reported, never silent: an encrypted
/// document opened without a password, or a page that does not compile or
/// render, must read as uncompared rather than uncovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedOcclusionPage {
    /// The page that could not be compared, or `None` when the document
    /// itself could not be opened.
    pub page: Option<u32>,
    /// Why the comparison did not run.
    pub reason: String,
}

/// The outcome of [`detect_flattened_occlusion`]: every covering paint, in
/// page then painter order, plus every comparison that could not run.
#[derive(Debug, Clone, PartialEq)]
pub struct OcclusionOutcome {
    /// One entry per page-content paint covering earlier text.
    pub occlusions: Vec<PageOcclusion>,
    /// Pages that could not be compared, in the order encountered.
    pub skipped: Vec<SkippedOcclusionPage>,
}

/// Find the page-content paints that cover earlier text.
///
/// `parse` is the parse assessment from the same
/// [`audit_bytes`](crate::intake::audit_bytes) run over `bytes`; `password`
/// is tried against encrypted documents exactly as the audit tries it.
/// Every page whose content holds an opaque paint over earlier text is
/// rendered and attributed; pages without a geometric candidate cost no
/// rasterization, and every failure becomes a [`SkippedOcclusionPage`]
/// entry rather than an error.
///
/// Never fails and never panics.
pub fn detect_flattened_occlusion(
    bytes: &[u8],
    parse: &ParseAssessment,
    password: Option<&str>,
    options: &OcclusionOptions,
) -> OcclusionOutcome {
    let mut occlusions = Vec::new();
    let mut skipped = Vec::new();
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes.to_vec()));
    let snapshot =
        match DocumentSnapshot::open_with_password(source, DocumentLimits::default(), password) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                skipped.push(SkippedOcclusionPage {
                    page: None,
                    reason: format!("document cannot be opened: {err}"),
                });
                return OcclusionOutcome {
                    occlusions,
                    skipped,
                };
            }
        };
    let recovered = parse.rests_on_recovered_structure();
    for page in 0..snapshot.page_count() {
        match detect_page(&snapshot, page, recovered, &options.evidence) {
            PageOcclusions::None => {}
            PageOcclusions::Found(mut found) => occlusions.append(&mut found),
            PageOcclusions::Skipped(reason) => skipped.push(SkippedOcclusionPage {
                page: Some(page),
                reason,
            }),
        }
    }
    OcclusionOutcome {
        occlusions,
        skipped,
    }
}

/// The occlusion result for one page.
enum PageOcclusions {
    /// No opaque paint covers earlier text on this page.
    None,
    /// The page's covering paints, in painter order.
    Found(Vec<PageOcclusion>),
    /// The page could not be compared, with why.
    Skipped(String),
}

/// Compare one page's painter-ordered content against its attribution map.
fn detect_page(
    snapshot: &DocumentSnapshot,
    page: u32,
    recovered: bool,
    evidence: &EvidenceOptions,
) -> PageOcclusions {
    // Cheap geometry first: the traced semantic compile plus the opacity
    // walk gate every page, and only a page with an opaque paint over
    // earlier text pays for lowering, attribution, and the evidence render.
    let mut traced_ctx = ParseContext::new();
    let (semantic, trace) = match PageCompiler::new().compile_semantic_traced(
        snapshot,
        PageIndex(page),
        &mut traced_ctx,
    ) {
        Ok(compiled) => compiled,
        Err(err) => {
            return PageOcclusions::Skipped(format!("page {page} did not compile: {err}"));
        }
    };
    let text = TextPage::build(&semantic, &TextPageOptions::default());
    let (candidates, run_op) = collect_candidates(&semantic, &trace);
    let mut gated: Vec<(CoverCandidate, Vec<usize>)> = Vec::new();
    for candidate in candidates {
        let earlier = chars_on_side(&text, &semantic, &run_op, &candidate, false);
        if !earlier.is_empty() {
            gated.push((candidate, earlier));
        }
    }
    if gated.is_empty() {
        return PageOcclusions::None;
    }

    let mut compiled_ctx = ParseContext::new();
    let compiled = match PageCompiler::new().compile(snapshot, PageIndex(page), &mut compiled_ctx) {
        Ok(compiled) => Arc::new(compiled),
        Err(err) => {
            return PageOcclusions::Skipped(format!(
                "page {page} did not lower for attribution: {err}"
            ));
        }
    };
    let (matrix, width, height) =
        match evidence_device_mapping(compiled.bounds.crop, compiled.bounds.rotate, evidence.scale)
        {
            Ok(mapping) => mapping,
            Err(err) => {
                return PageOcclusions::Skipped(format!(
                    "page {page} has no device mapping: {err}"
                ));
            }
        };
    // The attribution request mirrors the evidence render exactly — same
    // page, matrix, and dimensions — so every sampled pixel lines up.
    let request = RenderRequest {
        page: Arc::clone(&compiled),
        transform: PageTransform { matrix },
        crop: None,
        output_size: DeviceSize { width, height },
        output_format: OutputFormat::Rgba8PremultipliedSrgb,
        background: Background::White,
        color_policy: pdf_render_api::RenderColorPolicy::Original,
        annotations: AnnotationMode::None,
        quality: RenderQuality::Normal,
        limits: RenderLimits::default(),
        residency: OutputResidency::HostRequired,
    };
    let backend = CpuBackend::default();
    let prepared = match backend.prepare_attribution(&request) {
        Ok(prepared) => prepared,
        Err(err) => {
            return PageOcclusions::Skipped(format!(
                "page {page} attribution preparation failed: {err}"
            ));
        }
    };
    let map = render_attribution(&prepared);
    let glyphs = glyph_stamp_mask(&prepared, map.coverage_threshold);
    let off_options = EvidenceOptions {
        scale: evidence.scale,
        annotations: AnnotationMode::None,
    };
    let render = match render_page_evidence(snapshot, page, &off_options) {
        Ok(render) => render,
        Err(err) => {
            return PageOcclusions::Skipped(format!("page {page} evidence render failed: {err}"));
        }
    };
    if map.width != render.width() || map.height != render.height() {
        return PageOcclusions::Skipped(format!(
            "page {page} attribution map ({}x{}) disagrees with its {}x{} render",
            map.width,
            map.height,
            render.width(),
            render.height()
        ));
    }

    let mut found = Vec::new();
    for (candidate, earlier) in gated {
        let coverage = cover_coverage(&text, &earlier, &matrix, &map, &glyphs, width, height);
        if coverage < MIN_COVERAGE {
            // The paint meets the text's boxes but wins too few glyph
            // pixels: a rule kissing descenders, a clipped edge, or a
            // cover whose winners are text or pattern paint.
            continue;
        }
        let covered_text = chars_text(&text, &earlier);
        if covered_text.is_empty() {
            continue;
        }
        let Some(rect_milli) = millipoints(candidate.bbox) else {
            continue;
        };
        let later = chars_on_side(&text, &semantic, &run_op, &candidate, true);
        let replacement_text = chars_text(&text, &later);
        let range = op_range(&text, &earlier, &run_op, candidate.op_index);
        let code = match candidate.kind {
            OcclusionKind::Path => PAGE_CONTENT_COVERS_TEXT,
            OcclusionKind::Image => RASTER_PATCH_ON_VECTOR_PAGE,
        };
        let finding = Finding {
            code,
            strength: EvidenceStrength::ContextualIndicator,
            explanation: explain(
                page,
                &candidate,
                rect_milli,
                &covered_text,
                &replacement_text,
                coverage,
                range,
            ),
            limitation: Some(LIMITATION.to_owned()),
            requires_mode: AnalysisMode::Full,
            rests_on_recovered_structure: recovered,
        };
        found.push(PageOcclusion {
            finding,
            page,
            kind: candidate.kind,
            covering_op: candidate.op_index,
            operator: candidate.operator.clone(),
            content_op_index: candidate.content_op_index,
            stream: candidate.stream.clone(),
            span: candidate.span,
            origin: candidate.origin.name().to_owned(),
            stack: candidate.stack.clone(),
            rect: rect_milli,
            covered_text,
            replacement_text,
            coverage,
            render: render.clone(),
        });
    }
    if found.is_empty() {
        PageOcclusions::None
    } else {
        PageOcclusions::Found(found)
    }
}

/// One opaque paint op that could cover text, with its trace provenance.
struct CoverCandidate {
    /// Semantic op index of the covering paint, in painter order.
    op_index: usize,
    /// Whether a path or an image covers the text.
    kind: OcclusionKind,
    /// User-space bounds of the covering paint.
    bbox: Rect,
    /// The covering operator keyword as written, or `(synthesized)` when
    /// the paint op carries no source operator.
    operator: String,
    /// Execution sequence number of the covering content operator.
    content_op_index: u64,
    /// The source stream object as `N G R`, or `None` for a direct stream
    /// or a synthesized op.
    stream: Option<String>,
    /// Byte span of the covering operator within its decoded source
    /// stream, or `(0, 0)` for a synthesized op.
    span: (usize, usize),
    /// The paint origin of the cover: page content or form XObject.
    origin: PaintOrigin,
    /// The nested-object stack that produced the cover, outermost first.
    stack: Vec<String>,
}

/// The replayed subset of the graphics state the opacity walk needs: the
/// CTM for paint bounds, the current paints and alphas, the blend mode,
/// the line width for stroke bounds, and whether a derived soft mask
/// modulates paint. Everything here is save/restore-scoped, exactly as the
/// interpreter scopes it.
#[derive(Debug, Clone)]
struct CoverState {
    ctm: Matrix,
    fill: SemColor,
    fill_alpha: f32,
    stroke: SemColor,
    stroke_alpha: f32,
    blend: BlendMode,
    line_width: f64,
    soft_mask: bool,
}

impl Default for CoverState {
    /// The PDF default graphics state: black fills and strokes at full
    /// alpha and normal blend.
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            fill: SemColor::DeviceGray(0.0),
            fill_alpha: 1.0,
            stroke: SemColor::DeviceGray(0.0),
            stroke_alpha: 1.0,
            blend: BlendMode::Normal,
            line_width: 1.0,
            soft_mask: false,
        }
    }
}

/// Walk the semantic ops in painter order, collecting the opaque path and
/// image paints plus the op index behind every shown text run.
fn collect_candidates(
    page: &SemanticPage,
    trace: &PageTrace,
) -> (Vec<CoverCandidate>, HashMap<TextRunId, usize>) {
    let mut state = CoverState::default();
    let mut stack: Vec<CoverState> = Vec::new();
    let mut groups: Vec<(f32, BlendMode)> = Vec::new();
    let mut soft_content: u32 = 0;
    let mut run_op: HashMap<TextRunId, usize> = HashMap::new();
    let mut candidates = Vec::new();
    for (index, op) in page.ops.iter().enumerate() {
        match op {
            SemanticOp::Save => stack.push(state.clone()),
            SemanticOp::Restore => {
                if let Some(popped) = stack.pop() {
                    state = popped;
                }
            }
            SemanticOp::Concat(matrix) => state.ctm = matrix.then(state.ctm),
            SemanticOp::SetLineWidth(width) => state.line_width = *width,
            SemanticOp::SetFillColor(color) => state.fill = color.clone(),
            SemanticOp::SetStrokeColor(color) => state.stroke = color.clone(),
            SemanticOp::SetFillAlpha(alpha) => state.fill_alpha = *alpha,
            SemanticOp::SetStrokeAlpha(alpha) => state.stroke_alpha = *alpha,
            SemanticOp::SetBlendMode(blend) => state.blend = *blend,
            SemanticOp::ShowText(id) => {
                run_op.entry(*id).or_insert(index);
            }
            SemanticOp::Fill { path, .. } => {
                if paint_is_opaque(&state, &groups, soft_content, true, false) {
                    push_path_candidate(page, trace, &mut candidates, index, *path, &state, false);
                }
            }
            SemanticOp::Stroke { path } => {
                if paint_is_opaque(&state, &groups, soft_content, false, true) {
                    push_path_candidate(page, trace, &mut candidates, index, *path, &state, true);
                }
            }
            SemanticOp::FillStroke { path, .. } => {
                if paint_is_opaque(&state, &groups, soft_content, true, true) {
                    push_path_candidate(page, trace, &mut candidates, index, *path, &state, true);
                }
            }
            SemanticOp::DrawImage(id) => {
                if image_is_opaque(page, &state, &groups, soft_content, *id) {
                    push_image_candidate(trace, &mut candidates, index, &state);
                }
            }
            SemanticOp::BeginGroup { opacity, blend, .. } => {
                groups.push((*opacity, *blend));
                // Inside a transparency group the constant alpha and blend
                // reset to their defaults; the outer values become the
                // group's composite parameters, carried on the group stack
                // above. Mirrors the lowering replay.
                state.fill_alpha = 1.0;
                state.stroke_alpha = 1.0;
                state.blend = BlendMode::Normal;
            }
            SemanticOp::EndGroup => {
                groups.pop();
            }
            SemanticOp::BeginSoftMask { .. } => {
                soft_content = soft_content.saturating_add(1);
            }
            SemanticOp::EndSoftMask => {
                soft_content = soft_content.saturating_sub(1);
                // Past the mask content the derived mask modulates
                // subsequent paint until the scope ends or clears it.
                state.soft_mask = true;
            }
            SemanticOp::ClearSoftMask => {
                state.soft_mask = false;
            }
            SemanticOp::Clip { .. }
            | SemanticOp::ClipText { .. }
            | SemanticOp::BeginPaintOrigin(_)
            | SemanticOp::EndPaintOrigin
            | SemanticOp::PaintShading(_)
            | SemanticOp::SetLineCap(_)
            | SemanticOp::SetLineJoin(_)
            | SemanticOp::SetMiterLimit(_)
            | SemanticOp::SetDash { .. } => {}
        }
    }
    (candidates, run_op)
}

/// Whether the fill and/or stroke half of a path paint is provably opaque:
/// normal blend, no soft mask in scope, every enclosing group opaque at
/// normal blend, and at least one considered half solid at full alpha.
/// Pattern and shading paints are never solid — a patterned fill may paint
/// sparsely, so it cannot cover by construction.
fn paint_is_opaque(
    state: &CoverState,
    groups: &[(f32, BlendMode)],
    soft_content: u32,
    consider_fill: bool,
    consider_stroke: bool,
) -> bool {
    if !shared_is_opaque(state, groups, soft_content) {
        return false;
    }
    let fill = consider_fill && is_solid(&state.fill) && state.fill_alpha == 1.0;
    let stroke = consider_stroke && is_solid(&state.stroke) && state.stroke_alpha == 1.0;
    fill || stroke
}

/// Whether an image draw is provably opaque: the shared conditions, full
/// fill alpha (lowering paints images at the fill alpha), and no mask of
/// any kind — soft mask, explicit hard mask, in-codestream alpha, or a
/// degraded tint pipeline. A stencil mask additionally needs a solid fill,
/// which is what colors its painted samples.
fn image_is_opaque(
    page: &SemanticPage,
    state: &CoverState,
    groups: &[(f32, BlendMode)],
    soft_content: u32,
    id: ImageId,
) -> bool {
    if !shared_is_opaque(state, groups, soft_content) || state.fill_alpha != 1.0 {
        return false;
    }
    let Some(image) = page.images.get(id.index()) else {
        return false;
    };
    if image.smask.is_some()
        || image.mask.is_some()
        || image.smask_in_data != 0
        || image.lowering_degraded
    {
        return false;
    }
    !image.is_mask || is_solid(&state.fill)
}

/// The opacity conditions every covering paint shares: normal blend, no
/// derived soft mask in scope, outside any soft-mask content, and every
/// enclosing transparency group opaque at normal blend.
fn shared_is_opaque(state: &CoverState, groups: &[(f32, BlendMode)], soft_content: u32) -> bool {
    state.blend == BlendMode::Normal
        && !state.soft_mask
        && soft_content == 0
        && groups
            .iter()
            .all(|(opacity, blend)| *opacity == 1.0 && *blend == BlendMode::Normal)
}

/// Whether a color paints solidly. Device colors and parameterized spaces
/// do; pattern and shading paints may paint sparsely and never qualify.
fn is_solid(color: &SemColor) -> bool {
    matches!(
        color,
        SemColor::DeviceGray(_)
            | SemColor::DeviceRgb(..)
            | SemColor::DeviceCmyk(..)
            | SemColor::Components { .. }
    )
}

/// Record a path paint as a candidate when it has finite nonzero bounds
/// and page-content or form provenance. Strokes expand the path bounds by
/// half the line width; the attribution intersection below is the exact
/// test either way.
fn push_path_candidate(
    page: &SemanticPage,
    trace: &PageTrace,
    candidates: &mut Vec<CoverCandidate>,
    index: usize,
    path: PathId,
    state: &CoverState,
    is_stroke: bool,
) {
    let Some(data) = page.paths.get(path.index()) else {
        return;
    };
    if !state.ctm.is_finite() {
        return;
    }
    let mut bounds: Option<Rect> = None;
    for point in data.points.iter() {
        if !point.x.is_finite() || !point.y.is_finite() {
            continue;
        }
        let mapped = state.ctm.apply(*point);
        if !mapped.x.is_finite() || !mapped.y.is_finite() {
            return;
        }
        let here = Rect {
            x0: mapped.x,
            y0: mapped.y,
            x1: mapped.x,
            y1: mapped.y,
        };
        bounds = Some(bounds.map_or(here, |acc| acc.union(here)));
    }
    let Some(mut bbox) = bounds else {
        return;
    };
    if is_stroke {
        // `max` drops NaN to zero and negatives to zero; a non-finite
        // width below still widens generously, which only widens the
        // gating the attribution intersection then checks exactly.
        let by = state.line_width.max(0.0) / 2.0;
        if by.is_finite() && by > 0.0 {
            bbox = Rect {
                x0: bbox.x0 - by,
                y0: bbox.y0 - by,
                x1: bbox.x1 + by,
                y1: bbox.y1 + by,
            };
        }
    }
    push_candidate(trace, candidates, index, OcclusionKind::Path, bbox);
}

/// Record an image draw as a candidate when its unit square maps to finite
/// nonzero bounds with page-content or form provenance.
fn push_image_candidate(
    trace: &PageTrace,
    candidates: &mut Vec<CoverCandidate>,
    index: usize,
    state: &CoverState,
) {
    if !state.ctm.is_finite() {
        return;
    }
    let corners = [
        Point { x: 0.0, y: 0.0 },
        Point { x: 0.0, y: 1.0 },
        Point { x: 1.0, y: 0.0 },
        Point { x: 1.0, y: 1.0 },
    ];
    let mut bounds: Option<Rect> = None;
    for corner in corners {
        let mapped = state.ctm.apply(corner);
        if !mapped.x.is_finite() || !mapped.y.is_finite() {
            return;
        }
        let here = Rect {
            x0: mapped.x,
            y0: mapped.y,
            x1: mapped.x,
            y1: mapped.y,
        };
        bounds = Some(bounds.map_or(here, |acc| acc.union(here)));
    }
    let Some(bbox) = bounds else {
        return;
    };
    push_candidate(trace, candidates, index, OcclusionKind::Image, bbox);
}

/// Record a paint op as a candidate: nonzero finite bounds plus a trace
/// entry under page content or a form XObject. Soft-mask content,
/// annotation appearances, and glyph procedures never qualify — the first
/// paints offscreen, the second is compiled off, and the third is glyph
/// rendering rather than page paint.
fn push_candidate(
    trace: &PageTrace,
    candidates: &mut Vec<CoverCandidate>,
    index: usize,
    kind: OcclusionKind,
    bbox: Rect,
) {
    let normalized = bbox.normalized();
    if !is_finite_rect(normalized)
        || normalized.x0 >= normalized.x1
        || normalized.y0 >= normalized.y1
    {
        return;
    }
    let Some(entry) = trace.for_op(index) else {
        return;
    };
    if !matches!(
        entry.origin,
        PaintOrigin::PageContent | PaintOrigin::FormXObject
    ) {
        return;
    }
    let operator = if entry.operator.is_empty() {
        if entry.synthesized {
            "(synthesized)".to_owned()
        } else {
            "(unknown)".to_owned()
        }
    } else {
        String::from_utf8_lossy(&entry.operator).into_owned()
    };
    candidates.push(CoverCandidate {
        op_index: index,
        kind,
        bbox: normalized,
        operator,
        content_op_index: entry.content_op_index,
        stream: entry.stream.map(|id| id.to_string()),
        span: entry.span,
        origin: entry.origin,
        stack: entry
            .stack
            .iter()
            .map(|frame| match frame.object {
                Some(id) => format!("{} {id}", frame.kind.name()),
                None => frame.kind.name().to_owned(),
            })
            .collect(),
    });
}

/// The characters meeting a cover's rectangle on one painter-order side:
/// shown before the cover (`later == false`, the covered text) or after it
/// (`later == true`, the replacement text). Only painted, extractable text
/// joins either side; generated layout characters carry borrowed run
/// identity and no paint, so they never join.
fn chars_on_side(
    text: &TextPage,
    semantic: &SemanticPage,
    run_op: &HashMap<TextRunId, usize>,
    candidate: &CoverCandidate,
    later: bool,
) -> Vec<usize> {
    text.chars()
        .iter()
        .enumerate()
        .filter(|(_, info)| {
            is_cover_text(info, semantic)
                && intersects(info.char_box, candidate.bbox)
                && run_op.get(&info.text_object).is_some_and(|shown| {
                    if later {
                        *shown > candidate.op_index
                    } else {
                        *shown < candidate.op_index
                    }
                })
        })
        .map(|(char_index, _)| char_index)
        .collect()
}

/// Whether a character is painted, extractable text: not a layout
/// generation, mapped to Unicode, boxed finitely, and shown in a render
/// mode that paints (fill and/or stroke — modes 0, 1, 2, 4, 5, 6).
/// Invisible (3) and clip-only (7) runs were never visible, so no paint
/// can cover them.
fn is_cover_text(info: &CharInfo, semantic: &SemanticPage) -> bool {
    info.char_type != CharType::NotUnicode
        && info.char_type != CharType::Generated
        && info.unicode != 0
        && is_finite_rect(info.char_box)
        && semantic
            .text_runs
            .get(info.text_object.index())
            .is_some_and(|run| matches!(run.render_mode, 0 | 1 | 2 | 4 | 5 | 6))
}

/// The op-index span of the runs behind the covered characters: the
/// painter-order leg of the finding's justification. Falls back to the
/// covering op itself when no covered character maps to a run, which the
/// caller only reaches defensively — every covered character mapped to an
/// earlier op to be covered at all.
fn op_range(
    text: &TextPage,
    covered: &[usize],
    run_op: &HashMap<TextRunId, usize>,
    cover_op: usize,
) -> (usize, usize) {
    let mut range: Option<(usize, usize)> = None;
    for char_index in covered {
        let Some(info) = text.chars().get(*char_index) else {
            continue;
        };
        let Some(shown) = run_op.get(&info.text_object) else {
            continue;
        };
        range = Some(range.map_or((*shown, *shown), |(lo, hi)| {
            (lo.min(*shown), hi.max(*shown))
        }));
    }
    range.unwrap_or((cover_op, cover_op))
}

/// The fraction of the covered characters' glyph-stamped device pixels
/// whose attribution winner is later page-content path or image paint,
/// measured over the union of the characters' device boxes. The glyph mask
/// proves the order: the text stamped first, so a path or image winner
/// painted later. Zero when no covered character maps to a stamped glyph
/// pixel.
fn cover_coverage(
    text: &TextPage,
    covered: &[usize],
    matrix: &Matrix,
    map: &AttributionMap,
    glyphs: &[bool],
    width: u32,
    height: u32,
) -> f64 {
    let pixel_count = (width as usize).saturating_mul(height as usize);
    if map.leaf.len() != pixel_count || glyphs.len() != pixel_count || pixel_count == 0 {
        return 0.0;
    }
    let mut visited = vec![false; pixel_count];
    let mut area: u64 = 0;
    let mut won: u64 = 0;
    for char_index in covered {
        let Some(info) = text.chars().get(*char_index) else {
            continue;
        };
        let Some((x0, y0, x1, y1)) = device_bbox(matrix, info.char_box, width, height) else {
            continue;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let at = (y as usize)
                    .saturating_mul(width as usize)
                    .saturating_add(x as usize);
                if visited.get(at) == Some(&true) {
                    continue;
                }
                if let Some(slot) = visited.get_mut(at) {
                    *slot = true;
                }
                if glyphs.get(at) != Some(&true) {
                    continue;
                }
                area = area.saturating_add(1);
                if is_cover_winner(map, at) {
                    won = won.saturating_add(1);
                }
            }
        }
    }
    if area == 0 {
        0.0
    } else {
        won as f64 / area as f64
    }
}

/// Whether a pixel's attribution winner is page-content path or image
/// paint: solid covering paint, as opposed to text, shading or pattern
/// winners, annotation paint, or nothing.
fn is_cover_winner(map: &AttributionMap, at: usize) -> bool {
    let leaf = map.leaf.get(at).copied().unwrap_or(0);
    let origin = map.origin.get(at).copied().unwrap_or(0);
    let solid = leaf == PaintLeaf::Path as u8 || leaf == PaintLeaf::Image as u8;
    let content =
        origin == PaintOrigin::PageContent as u8 || origin == PaintOrigin::FormXObject as u8;
    solid && content
}

/// A user-space box mapped into device pixels: the axis-aligned bound of
/// the mapped corners, clamped to the image. `None` for a non-finite box
/// or one covering no pixel.
fn device_bbox(
    matrix: &Matrix,
    rect: Rect,
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32, u32)> {
    if !is_finite_rect(rect) {
        return None;
    }
    let corners = [
        Point {
            x: rect.x0,
            y: rect.y0,
        },
        Point {
            x: rect.x0,
            y: rect.y1,
        },
        Point {
            x: rect.x1,
            y: rect.y0,
        },
        Point {
            x: rect.x1,
            y: rect.y1,
        },
    ];
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for corner in corners {
        let mapped = matrix.apply(corner);
        if !mapped.x.is_finite() || !mapped.y.is_finite() {
            return None;
        }
        x0 = x0.min(mapped.x);
        y0 = y0.min(mapped.y);
        x1 = x1.max(mapped.x);
        y1 = y1.max(mapped.y);
    }
    let clamp = |value: f64, extent: u32| -> u32 {
        value.floor().clamp(0.0, f64::from(extent)).round() as u32
    };
    let (x0, y0) = (clamp(x0, width), clamp(y0, height));
    let (x1, y1) = (
        x1.ceil().clamp(0.0, f64::from(width)).round() as u32,
        y1.ceil().clamp(0.0, f64::from(height)).round() as u32,
    );
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    Some((x0, y0, x1, y1))
}

/// The text of `chars` in ascending (extraction) order, mirroring how
/// [`TextPage`] joins character unicodes.
fn chars_text(text: &TextPage, chars: &[usize]) -> String {
    let mut units = Vec::with_capacity(chars.len());
    for char_index in chars {
        if let Some(info) = text.chars().get(*char_index) {
            units.push(info.unicode as u16);
        }
    }
    String::from_utf16_lossy(&units)
}

/// Whether two normalized boxes meet, edges included — the same rule text
/// extraction uses to gather the characters inside a rectangle.
fn intersects(a: Rect, b: Rect) -> bool {
    let a = a.normalized();
    let b = b.normalized();
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

/// Whether a box is finite on every corner.
fn is_finite_rect(rect: Rect) -> bool {
    [rect.x0, rect.y0, rect.x1, rect.y1]
        .iter()
        .all(|v| v.is_finite())
}

/// User-space points to thousandths of a point, normalized. `None` for a
/// non-finite box, which this crate will not bless into a finding.
fn millipoints(bbox: Rect) -> Option<[i64; 4]> {
    if !is_finite_rect(bbox) {
        return None;
    }
    let normalized = bbox.normalized();
    Some([
        (normalized.x0 * 1000.0).round() as i64,
        (normalized.y0 * 1000.0).round() as i64,
        (normalized.x1 * 1000.0).round() as i64,
        (normalized.y1 * 1000.0).round() as i64,
    ])
}

/// The finding's self-contained explanation: page, covering paint and its
/// provenance, the covered text ops, both texts, and coverage.
#[allow(clippy::too_many_arguments)]
fn explain(
    page: u32,
    candidate: &CoverCandidate,
    rect: [i64; 4],
    covered_text: &str,
    replacement_text: &str,
    coverage: f64,
    range: (usize, usize),
) -> String {
    let kind = match candidate.kind {
        OcclusionKind::Path => "opaque path paint",
        OcclusionKind::Image => "opaque image patch",
    };
    let rect = format!("[{} {} {} {}]", rect[0], rect[1], rect[2], rect[3]);
    let stream = candidate
        .stream
        .as_deref()
        .map_or_else(|| "a direct stream".to_owned(), |id| format!("stream {id}"));
    let span = if candidate.span == (0, 0) {
        String::new()
    } else {
        format!(", bytes {}..{}", candidate.span.0, candidate.span.1)
    };
    let shown = if range.0 == range.1 {
        format!("text op {}", range.0)
    } else {
        format!("text ops {}..={}", range.0, range.1)
    };
    let inside = if candidate.stack.is_empty() {
        String::new()
    } else {
        format!(" inside {}", candidate.stack.join(", "))
    };
    let replacement = if replacement_text.is_empty() {
        "no replacement text over the cover".to_owned()
    } else {
        format!("replacement text {replacement_text:?} painted over the cover")
    };
    format!(
        "Page {page} {kind} covers earlier text at {rect}: operator `{op}` (semantic op \
         #{cover}, content operator #{seq}, {stream}{span}) painted after the covered text \
         ({shown}) under {origin}{inside}; covers {covered_text:?} (glyph-area coverage \
         {pct:.1}%); {replacement}.",
        op = candidate.operator,
        cover = candidate.op_index,
        seq = candidate.content_op_index,
        origin = candidate.origin.name(),
        pct = coverage * 100.0,
    )
}
