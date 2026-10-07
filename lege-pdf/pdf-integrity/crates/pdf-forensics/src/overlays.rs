//! Explicit annotation overlays: annotations that paint over existing text.
//!
//! Annotation appearance streams are painted on top of page content, so an
//! opaque `/FreeText`, `/Stamp`, or `/Redact` appearance (or any other
//! author-supplied appearance, including image stamps) can cover — and
//! visually replace — the text beneath it. For every page carrying candidate
//! annotations this module:
//!
//! 1. renders the page with annotations off and with annotations on, and
//!    diffs the two renders pixel by pixel;
//! 2. extracts the page text without annotations (the underlying text) and
//!    the annotation-appearance text (from the appearance streams alone);
//! 3. intersects the changed pixels with the underlying glyph boxes to find
//!    which text each annotation covers and how much of its glyph area
//!    changed.
//!
//! Each covering annotation yields one [`AnnotationOverlay`]: an
//! [`EXPLICIT_ANNOTATION_OVER_TEXT`] finding carrying the subtype, the
//! annotation rectangle, the affected underlying text, the appearance-stream
//! text, and the glyph-area coverage, plus the off/on renders and their
//! diff as evidence.
//!
//! What is deliberately out of scope: text-markup annotations
//! (`/Highlight`, `/Underline`, `/Squiggly`, `/StrikeOut`) and form
//! `/Widget`s never yield a finding — markup is designed to sit over text
//! and widgets are legitimate form fields, so flagging them would report
//! ordinary documents. Hidden (`/Hidden`, `/NoView`) annotations never
//! paint and are skipped. Attribution follows the annotation `/Rect`: two
//! overlapping covering annotations on one page share changed pixels and
//! may both claim the same underlying text.

use std::sync::Arc;

use pdf_content::{PageCompiler, SemanticOp, SemanticPage};
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_page_ir::{Matrix, PaintOrigin, Point, Rect};
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_text::{CharType, TextPage, TextPageOptions};

use crate::document::ParseAssessment;
use crate::evidence::{
    AnnotationMode, EvidenceOptions, EvidenceRender, RenderDiff, diff_renders,
    evidence_device_mapping, render_page_evidence,
};
use crate::report::{AnalysisMode, EvidenceStrength, Finding, FindingCode};

/// An annotation paints over existing page text.
pub const EXPLICIT_ANNOTATION_OVER_TEXT: FindingCode =
    FindingCode::new("EXPLICIT_ANNOTATION_OVER_TEXT");

/// Subtypes that never yield a finding: text markup sits over text by
/// design, and form widgets are legitimate fields. A covering `/Highlight`
/// or `/Widget` appearance still changes pixels — the exclusion is a rule,
/// not an absence of paint.
const EXCLUDED_SUBTYPES: [&str; 5] = ["Highlight", "Underline", "Squiggly", "StrikeOut", "Widget"];

/// Options for [`detect_annotation_overlays`].
#[derive(Debug, Clone, Default)]
pub struct OverlayOptions {
    /// Evidence-render options for the per-page off/on images. The scale
    /// shapes the PNG evidence and the pixel intersection; the
    /// `annotations` field is ignored — the off render always compiles
    /// without appearances and the on render always with them.
    pub evidence: EvidenceOptions,
}

/// One annotation painting over existing page text.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationOverlay {
    /// The finding: [`EXPLICIT_ANNOTATION_OVER_TEXT`] at
    /// [`EvidenceStrength::StrongDerived`] with a stated limitation, needing
    /// [`AnalysisMode::Full`]. Its explanation names the page, the
    /// annotation object, the subtype, the rectangle, both texts, and the
    /// coverage, so the finding reads standalone in JSON.
    pub finding: Finding,
    /// Zero-based page index carrying the annotation.
    pub page: u32,
    /// Backing annotation dictionary as `N G R`; `None` for a direct inline
    /// annotation dictionary.
    pub object: Option<String>,
    /// `/Subtype`, e.g. `FreeText`; `(none)` when absent or unreadable.
    pub subtype: String,
    /// The annotation `/Rect` in thousandths of a point (`[x0, y0, x1, y1]`,
    /// normalized).
    pub rect: [i64; 4],
    /// The underlying page text covered by the annotation, in extraction
    /// order: every extractable character whose glyph box meets the
    /// annotation rectangle.
    pub affected_text: String,
    /// The text the annotation's own appearance streams set inside the
    /// annotation rectangle. Empty when the appearance paints no text (an
    /// image stamp, a bare redaction box) rather than absent.
    pub appearance_text: String,
    /// Glyph-area coverage: the fraction (0.0–1.0) of the affected
    /// characters' device-pixel glyph area whose pixels changed between the
    /// annotation-off and annotation-on renders. Always above zero — a
    /// covering rectangle whose glyph pixels did not change reports no
    /// finding.
    pub coverage: f64,
    /// The page rendered without annotation appearances.
    pub off: EvidenceRender,
    /// The page rendered with annotation appearances.
    pub on: EvidenceRender,
    /// Pixel diff of the two renders. Same dimensions by construction — the
    /// page geometry does not depend on the annotation pass.
    pub diff: RenderDiff,
}

/// A page — or the document itself — the overlay analysis could not
/// compare, with why. Skips are reported, never silent: an encrypted
/// document opened without a password, or a page that does not compile or
/// render, must read as uncompared rather than uncovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedOverlayPage {
    /// The page that could not be compared, or `None` when the document
    /// itself could not be opened.
    pub page: Option<u32>,
    /// Why the comparison did not run.
    pub reason: String,
}

/// The outcome of [`detect_annotation_overlays`]: every covering
/// annotation, in page then `/Annots` order, plus every comparison that
/// could not run.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayOutcome {
    /// One entry per annotation painting over existing text.
    pub overlays: Vec<AnnotationOverlay>,
    /// Pages that could not be compared, in the order encountered.
    pub skipped: Vec<SkippedOverlayPage>,
}

/// Find the annotations that paint over existing page text.
///
/// `parse` is the parse assessment from the same
/// [`audit_bytes`](crate::intake::audit_bytes) run over `bytes`; `password`
/// is tried against encrypted documents exactly as the audit tries it.
/// Every page carrying at least one candidate annotation is rendered with
/// annotations off and on and compared; pages without candidates cost
/// nothing, and every failure becomes a [`SkippedOverlayPage`] entry rather
/// than an error.
///
/// Never fails and never panics.
pub fn detect_annotation_overlays(
    bytes: &[u8],
    parse: &ParseAssessment,
    password: Option<&str>,
    options: &OverlayOptions,
) -> OverlayOutcome {
    let mut overlays = Vec::new();
    let mut skipped = Vec::new();
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes.to_vec()));
    let snapshot =
        match DocumentSnapshot::open_with_password(source, DocumentLimits::default(), password) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                skipped.push(SkippedOverlayPage {
                    page: None,
                    reason: format!("document cannot be opened: {err}"),
                });
                return OverlayOutcome { overlays, skipped };
            }
        };
    let recovered = parse.rests_on_recovered_structure();
    for page in 0..snapshot.page_count() {
        match detect_page(&snapshot, page, recovered, &options.evidence) {
            PageOverlays::None => {}
            PageOverlays::Found(mut found) => overlays.append(&mut found),
            PageOverlays::Skipped(reason) => skipped.push(SkippedOverlayPage {
                page: Some(page),
                reason,
            }),
        }
    }
    OverlayOutcome { overlays, skipped }
}

/// The overlay result for one page.
enum PageOverlays {
    /// No candidate annotations, no paint, or no covered text.
    None,
    /// The page's covering annotations.
    Found(Vec<AnnotationOverlay>),
    /// The page could not be compared, with why.
    Skipped(String),
}

/// Compare one page with annotations off and on.
fn detect_page(
    snapshot: &DocumentSnapshot,
    page: u32,
    recovered: bool,
    evidence: &EvidenceOptions,
) -> PageOverlays {
    let page_ref = match snapshot.page(PageIndex(page)) {
        Ok(page_ref) => page_ref,
        Err(err) => return PageOverlays::Skipped(format!("page {page} is not readable: {err}")),
    };
    // Candidates: visible annotations outside the excluded subtypes. The
    // subtype exclusion is the rule that keeps highlights and widgets
    // silent; hidden annotations never paint and are skipped with them.
    let candidates: Vec<(usize, String)> = page_ref
        .annotations
        .iter()
        .enumerate()
        .filter(|(_, annot)| !annot.hidden_for_display())
        .map(|(index, annot)| {
            let subtype = annot
                .subtype
                .map(|id| String::from_utf8_lossy(&snapshot.names().resolve(id)).into_owned())
                .unwrap_or_else(|| "(none)".to_owned());
            (index, subtype)
        })
        .filter(|(_, subtype)| !EXCLUDED_SUBTYPES.contains(&subtype.as_str()))
        .collect();
    if candidates.is_empty() {
        return PageOverlays::None;
    }

    let mut off_ctx = ParseContext::new();
    let off_semantic =
        match PageCompiler::new().compile_semantic(snapshot, PageIndex(page), &mut off_ctx) {
            Ok(semantic) => semantic,
            Err(err) => {
                return PageOverlays::Skipped(format!("page {page} did not compile: {err}"));
            }
        };
    let mut on_ctx = ParseContext::new();
    let on_semantic = match PageCompiler::new().with_annotations(true).compile_semantic(
        snapshot,
        PageIndex(page),
        &mut on_ctx,
    ) {
        Ok(semantic) => semantic,
        Err(err) => {
            return PageOverlays::Skipped(format!(
                "page {page} did not compile with annotations: {err}"
            ));
        }
    };

    let off_options = EvidenceOptions {
        scale: evidence.scale,
        annotations: AnnotationMode::None,
    };
    let off = match render_page_evidence(snapshot, page, &off_options) {
        Ok(render) => render,
        Err(err) => {
            return PageOverlays::Skipped(format!(
                "page {page} annotation-off render failed: {err}"
            ));
        }
    };
    let on_options = EvidenceOptions {
        scale: evidence.scale,
        annotations: AnnotationMode::StaticAppearances,
    };
    let on = match render_page_evidence(snapshot, page, &on_options) {
        Ok(render) => render,
        Err(err) => {
            return PageOverlays::Skipped(format!(
                "page {page} annotation-on render failed: {err}"
            ));
        }
    };
    let diff = match diff_renders(&off, &on) {
        Ok(diff) => diff,
        Err(err) => {
            return PageOverlays::Skipped(format!(
                "page {page} annotation off/on renders do not align: {err}"
            ));
        }
    };
    if diff.changed_pixels == 0 {
        return PageOverlays::None;
    }

    // The device mapping must reproduce the renders' own dimensions —
    // lowering preserves the semantic bounds, so a mismatch means this
    // analysis and the renderer disagree about the page geometry, and any
    // pixel intersection would be meaningless.
    let bounds = off_semantic.bounds;
    let (matrix, width, height) =
        match evidence_device_mapping(bounds.crop, bounds.rotate, evidence.scale) {
            Ok(mapping) => mapping,
            Err(err) => {
                return PageOverlays::Skipped(format!("page {page} has no device mapping: {err}"));
            }
        };
    if width != off.width() || height != off.height() {
        return PageOverlays::Skipped(format!(
            "page {page} device mapping ({width}x{height}) disagrees with its {}x{} renders",
            off.width(),
            off.height()
        ));
    }

    let changed = changed_mask(&off, &on);
    let underlying = TextPage::build(&off_semantic, &TextPageOptions::default());
    let appearance_semantic = appearance_only_semantic(&on_semantic);
    let appearance = TextPage::build(
        &appearance_semantic,
        &TextPageOptions {
            include_annotations: true,
            ..TextPageOptions::default()
        },
    );

    let mut found = Vec::new();
    for (index, subtype) in candidates {
        let Some(annot) = page_ref.annotations.get(index) else {
            continue;
        };
        let rect = Rect {
            x0: annot.rect[0],
            y0: annot.rect[1],
            x1: annot.rect[2],
            y1: annot.rect[3],
        }
        .normalized();
        if ![rect.x0, rect.y0, rect.x1, rect.y1]
            .iter()
            .all(|v| v.is_finite())
        {
            continue;
        }
        let Some(rect_milli) = millipoints(rect) else {
            continue;
        };
        let affected: Vec<usize> = underlying
            .chars()
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                info.char_type != CharType::NotUnicode
                    && info.unicode != 0
                    && is_finite_rect(info.char_box)
                    && intersects(info.char_box, rect)
            })
            .map(|(char_index, _)| char_index)
            .collect();
        if affected.is_empty() {
            continue;
        }
        let affected_text = chars_text(&underlying, &affected);
        if affected_text.is_empty() {
            continue;
        }
        let coverage = glyph_coverage(&underlying, &affected, &matrix, &changed, width, height);
        if coverage <= 0.0 {
            // The rectangle meets text but no glyph pixel changed: the
            // appearance paints beside the text (a border, an offset
            // stamp), not over it.
            continue;
        }
        let appearance_text = appearance.text_in_rect(rect);
        let object = annot.source.map(|id| id.to_string());
        let finding = Finding {
            code: EXPLICIT_ANNOTATION_OVER_TEXT,
            strength: EvidenceStrength::StrongDerived,
            explanation: explain(
                page,
                object.as_deref(),
                &subtype,
                rect_milli,
                &affected_text,
                &appearance_text,
                coverage,
            ),
            limitation: Some(
                "A covering annotation reports geometry and pixels, not intent: approval and \
                 notary stamps, applied redactions, and accessibility tags sit over text \
                 legitimately. Overlapping covering annotations on one page share changed \
                 pixels, so each finding's coverage is measured against its own rectangle."
                    .to_owned(),
            ),
            requires_mode: AnalysisMode::Full,
            rests_on_recovered_structure: recovered,
        };
        found.push(AnnotationOverlay {
            finding,
            page,
            object,
            subtype,
            rect: rect_milli,
            affected_text,
            appearance_text,
            coverage,
            off: off.clone(),
            on: on.clone(),
            diff: diff.clone(),
        });
    }
    if found.is_empty() {
        PageOverlays::None
    } else {
        PageOverlays::Found(found)
    }
}

/// The semantic page's annotation-appearance text runs, with their geometry
/// intact.
///
/// [`TextPage`] tracks its transform through the structural ops
/// (`Save`/`Restore`/`Concat`) and gates `ShowText` on the paint-origin
/// stack, so keeping every structural op and only the `ShowText`s issued
/// under an [`PaintOrigin::AnnotationAppearance`] scope yields exactly the
/// appearance-stream text with exactly the boxes the full build computes.
/// Page-content runs are dropped, not merely gated off, so the result holds
/// appearance text alone.
fn appearance_only_semantic(page: &SemanticPage) -> SemanticPage {
    let mut origins: Vec<PaintOrigin> = Vec::new();
    let mut ops = Vec::new();
    for op in page.ops.iter() {
        match op {
            SemanticOp::BeginPaintOrigin(origin) => {
                origins.push(*origin);
                ops.push(op.clone());
            }
            SemanticOp::EndPaintOrigin => {
                origins.pop();
                ops.push(op.clone());
            }
            SemanticOp::Save | SemanticOp::Restore | SemanticOp::Concat(_) => {
                ops.push(op.clone());
            }
            SemanticOp::ShowText(_) if origins.contains(&PaintOrigin::AnnotationAppearance) => {
                ops.push(op.clone());
            }
            _ => {}
        }
    }
    SemanticPage {
        bounds: page.bounds,
        ops: Arc::from(ops),
        paths: Arc::clone(&page.paths),
        text_runs: Arc::clone(&page.text_runs),
        images: Arc::clone(&page.images),
        fonts: Arc::clone(&page.fonts),
        shadings: Arc::clone(&page.shadings),
        tilings: Arc::clone(&page.tilings),
        uses_icc_color: page.uses_icc_color,
        uses_overprint: page.uses_overprint,
    }
}

/// The changed-pixel mask behind [`diff_renders`]: one flag per device
/// pixel, row-major, true where any RGBA byte differs.
fn changed_mask(off: &EvidenceRender, on: &EvidenceRender) -> Vec<bool> {
    off.pixels()
        .chunks_exact(4)
        .zip(on.pixels().chunks_exact(4))
        .map(|(a, b)| a != b)
        .collect()
}

/// The fraction of the affected characters' device glyph area whose pixels
/// changed: changed pixels inside the union of the characters' device boxes
/// over the union's area. Zero when no affected character maps to a device
/// pixel.
fn glyph_coverage(
    underlying: &TextPage,
    affected: &[usize],
    matrix: &Matrix,
    changed: &[bool],
    width: u32,
    height: u32,
) -> f64 {
    let pixel_count = (width as usize).saturating_mul(height as usize);
    if changed.len() != pixel_count || pixel_count == 0 {
        return 0.0;
    }
    let mut visited = vec![false; pixel_count];
    let mut area: u64 = 0;
    let mut covered: u64 = 0;
    for char_index in affected {
        let Some(info) = underlying.chars().get(*char_index) else {
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
                let seen = visited.get(at) == Some(&true);
                if seen {
                    continue;
                }
                if let Some(slot) = visited.get_mut(at) {
                    *slot = true;
                }
                area = area.saturating_add(1);
                if changed.get(at) == Some(&true) {
                    covered = covered.saturating_add(1);
                }
            }
        }
    }
    if area == 0 {
        0.0
    } else {
        covered as f64 / area as f64
    }
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
fn chars_text(underlying: &TextPage, chars: &[usize]) -> String {
    let mut units = Vec::with_capacity(chars.len());
    for char_index in chars {
        if let Some(info) = underlying.chars().get(*char_index) {
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

/// The finding's self-contained explanation: page, annotation, subtype,
/// rectangle, both texts, and coverage.
fn explain(
    page: u32,
    object: Option<&str>,
    subtype: &str,
    rect: [i64; 4],
    affected_text: &str,
    appearance_text: &str,
    coverage: f64,
) -> String {
    let object = object.unwrap_or("direct");
    let rect = format!("[{} {} {} {}]", rect[0], rect[1], rect[2], rect[3]);
    let appearance = if appearance_text.is_empty() {
        "appearance paints no extractable text".to_owned()
    } else {
        format!("appearance text {appearance_text:?}")
    };
    format!(
        "Page {page} {subtype} annotation ({object}) over text at {rect}: covers \
         {affected_text:?} (glyph-area coverage {:.1}%); {appearance}.",
        coverage * 100.0,
    )
}
