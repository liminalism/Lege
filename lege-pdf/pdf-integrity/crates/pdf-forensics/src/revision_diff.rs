//! Semantic text comparison between consecutive logical revisions.
//!
//! Each pair of neighbouring saves is replayed through
//! [`open_revision`](crate::replay::open_revision) — the ordinary open path
//! over a [`PrefixSource`](pdf_source::PrefixSource) truncated at the
//! revision's established end — and every page the two replays share is
//! compared at the semantic level: extracted words with their glyph
//! identities and font resources, via
//! [`PageCompiler::compile_semantic`](pdf_content::PageCompiler::compile_semantic)
//! and [`TextPage`](pdf_text::TextPage). A page whose word sequence differs
//! in text, glyphs, or fonts yields one [`RevisionTextChange`]: a
//! [`TEXT_CHANGED_BETWEEN_REVISIONS`] finding with the exact previous and
//! current page strings, the union rectangle of the changed words, the
//! content-stream object ids behind the page, and the before/after
//! [`EvidenceRender`](crate::evidence::EvidenceRender)s plus their
//! [`RenderDiff`](crate::evidence::RenderDiff).
//!
//! The comparison is semantic, not byte-level: a redefinition that only
//! re-serializes a content stream keeps its words and reports nothing, while
//! a single re-typed digit reports exactly the two page strings. Raw-byte
//! and semantic object hashes already live on the M2 change records; this
//! module answers what the edit *said*.
//!
//! What is deliberately out of scope: annotation appearance streams are not
//! part of the semantic comparison (the compiler runs with appearances off,
//! so a text edit inside an appearance reports nothing here — though it
//! still shows in the before/after renders when the evidence options render
//! appearances); pages added or removed between revisions have no
//! counterpart to compare against; and a change that keeps every word,
//! glyph, and font identical — pure repositioning, or paint outside
//! extracted text — is not a text change and reports nothing.

use std::collections::BTreeSet;
use std::ops::Range;

use pdf_content::{PageCompiler, SemanticPage, semantic::SemFont};
use pdf_document::{DocumentSnapshot, PageIndex, ParseContext};
use pdf_object::PdfObject;
use pdf_page_ir::Rect;
use pdf_text::{TextPage, TextPageOptions, TextWord};

use crate::document::{ParseAssessment, RevisionHistoryFacts};
use crate::evidence::{
    EvidenceOptions, EvidenceRender, RenderDiff, diff_renders, render_page_evidence,
};
use crate::report::{AnalysisMode, EvidenceStrength, Finding, FindingCode};

/// A page's extracted text differs between two consecutive saves.
pub const TEXT_CHANGED_BETWEEN_REVISIONS: FindingCode =
    FindingCode::new("TEXT_CHANGED_BETWEEN_REVISIONS");

/// Options for [`diff_revision_text`].
#[derive(Debug, Clone, Default)]
pub struct RevisionTextDiffOptions {
    /// Evidence-render options for the before/after page images carried by
    /// every change. The semantic comparison itself always runs without
    /// system fonts and without annotation appearances; this only shapes
    /// the PNG evidence.
    pub evidence: EvidenceOptions,
}

/// One page whose extracted text differs between two consecutive revisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionTextChange {
    /// The finding: [`TEXT_CHANGED_BETWEEN_REVISIONS`] at
    /// [`EvidenceStrength::StrongDerived`] with a stated limitation, needing
    /// [`AnalysisMode::Full`]. Its explanation names the page, the revision
    /// pair, the content objects, the rectangle, and both strings, so the
    /// finding reads standalone in JSON.
    pub finding: Finding,
    /// Zero-based page index, shared by both revisions.
    pub page: u32,
    /// Chronological index of the earlier revision: what the page said
    /// before.
    pub previous_revision: u32,
    /// Chronological index of the later revision: the save that introduced
    /// the change.
    pub revision: u32,
    /// The page's full extracted text under the earlier revision, verbatim.
    pub previous_text: String,
    /// The page's full extracted text under the later revision, verbatim.
    pub current_text: String,
    /// Union of the changed words' boxes from both sides, in thousandths of
    /// a point (`[x0, y0, x1, y1]`, normalized), or `None` when no changed
    /// word carried a finite box — defensive only; extraction always
    /// produces finite boxes.
    pub rect: Option<[i64; 4]>,
    /// Content-stream object ids behind the page in either revision, as
    /// `N G R`, ascending and de-duplicated. Empty only when neither
    /// replayed page dict carried a `/Contents` reference.
    pub objects: Vec<String>,
    /// The page as the earlier revision rendered it.
    pub before: EvidenceRender,
    /// The page as the later revision renders it.
    pub after: EvidenceRender,
    /// Pixel diff of the two renders, or `None` when they differ in
    /// dimensions — the page geometry itself changed, so a pixel diff is
    /// meaningless while the two images still stand as evidence.
    pub diff: Option<RenderDiff>,
}

/// A revision pair — or one of its pages — the text diff could not compare,
/// with why. Skips are reported, never silent: an encrypted document
/// replayed without a password, or a save whose end was never established,
/// must read as uncompared rather than unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRevisionPair {
    /// Chronological index of the earlier revision.
    pub previous_revision: u32,
    /// Chronological index of the later revision.
    pub revision: u32,
    /// The page that could not be compared, or `None` when the pair itself
    /// could not be replayed (or its page counts differ).
    pub page: Option<u32>,
    /// Why the comparison did not run.
    pub reason: String,
}

/// The outcome of [`diff_revision_text`]: every page that changed, in
/// revision-pair then page order, plus every comparison that could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionTextDiffOutcome {
    /// One entry per page whose word sequence differs between two
    /// consecutive revisions.
    pub changes: Vec<RevisionTextChange>,
    /// Pairs and pages that could not be compared, in the order encountered.
    pub skipped: Vec<SkippedRevisionPair>,
}

/// Compare the extracted page text of every consecutive revision pair.
///
/// `history` is the M2 logical-revision grouping (oldest first) and `parse`
/// the parse assessment both come from the same [`audit_bytes`](crate::intake::audit_bytes)
/// run over `bytes`; `password` is tried against encrypted documents exactly
/// as the audit tries it. Each pair replays both revisions and compares
/// every page the two replays share; pages past the common prefix, pairs
/// that refuse replay, and pages that do not compile or render are recorded
/// in [`RevisionTextDiffOutcome::skipped`].
///
/// Never fails and never panics: a document with fewer than two revisions
/// yields no pairs, and every per-pair or per-page failure becomes a skip
/// entry rather than an error.
pub fn diff_revision_text(
    bytes: &[u8],
    history: &RevisionHistoryFacts,
    parse: &ParseAssessment,
    password: Option<&str>,
    options: &RevisionTextDiffOptions,
) -> RevisionTextDiffOutcome {
    let context = DiffContext {
        compiler: &PageCompiler::new(),
        text: &TextPageOptions::default(),
        evidence: &options.evidence,
        recovered: parse.rests_on_recovered_structure(),
    };
    let mut changes = Vec::new();
    let mut skipped = Vec::new();
    for pair in history.revisions.windows(2) {
        let Some(previous) = pair.first() else {
            continue;
        };
        let Some(current) = pair.get(1) else {
            continue;
        };
        let before_snapshot = match crate::replay::open_revision(bytes, previous, password) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                skipped.push(SkippedRevisionPair {
                    previous_revision: previous.index,
                    revision: current.index,
                    page: None,
                    reason: format!(
                        "previous revision {} cannot be replayed: {err}",
                        previous.index
                    ),
                });
                continue;
            }
        };
        let after_snapshot = match crate::replay::open_revision(bytes, current, password) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                skipped.push(SkippedRevisionPair {
                    previous_revision: previous.index,
                    revision: current.index,
                    page: None,
                    reason: format!("revision {} cannot be replayed: {err}", current.index),
                });
                continue;
            }
        };
        let before_count = before_snapshot.page_count();
        let after_count = after_snapshot.page_count();
        if before_count != after_count {
            skipped.push(SkippedRevisionPair {
                previous_revision: previous.index,
                revision: current.index,
                page: None,
                reason: format!(
                    "page count changed {before_count} to {after_count} between revisions {} and {}; \
                     pages past the common prefix are not text-compared",
                    previous.index, current.index
                ),
            });
        }
        for page in 0..before_count.min(after_count) {
            match diff_page(
                &context,
                &before_snapshot,
                &after_snapshot,
                previous.index,
                current.index,
                page,
            ) {
                PageDiff::Same => {}
                PageDiff::Changed(change) => changes.push(*change),
                PageDiff::Skipped(reason) => skipped.push(SkippedRevisionPair {
                    previous_revision: previous.index,
                    revision: current.index,
                    page: Some(page),
                    reason,
                }),
            }
        }
    }
    RevisionTextDiffOutcome { changes, skipped }
}

/// Shared inputs for one page comparison.
struct DiffContext<'a> {
    /// Semantic compiler: no system fonts, no annotation appearances.
    compiler: &'a PageCompiler,
    /// Text extraction options: hidden text participates, so an edit to an
    /// invisible layer still reads as a text change.
    text: &'a TextPageOptions,
    /// Evidence-render options for the before/after images.
    evidence: &'a EvidenceOptions,
    /// Whether the full-file parse recovered — what the finding's
    /// `rests_on_recovered_structure` reports.
    recovered: bool,
}

/// The comparison result for one page.
enum PageDiff {
    /// The word sequences match in text, glyphs, and fonts.
    Same,
    /// The page said something different.
    Changed(Box<RevisionTextChange>),
    /// The page could not be compared, with why.
    Skipped(String),
}

/// Compare one page across two replayed revisions.
fn diff_page(
    context: &DiffContext<'_>,
    before_snapshot: &DocumentSnapshot,
    after_snapshot: &DocumentSnapshot,
    previous_revision: u32,
    revision: u32,
    page: u32,
) -> PageDiff {
    let mut before_ctx = ParseContext::new();
    let before_semantic =
        match context
            .compiler
            .compile_semantic(before_snapshot, PageIndex(page), &mut before_ctx)
        {
            Ok(semantic) => semantic,
            Err(err) => {
                return PageDiff::Skipped(format!(
                    "revision {previous_revision} page {page} did not compile: {err}"
                ));
            }
        };
    let mut after_ctx = ParseContext::new();
    let after_semantic =
        match context
            .compiler
            .compile_semantic(after_snapshot, PageIndex(page), &mut after_ctx)
        {
            Ok(semantic) => semantic,
            Err(err) => {
                return PageDiff::Skipped(format!(
                    "revision {revision} page {page} did not compile: {err}"
                ));
            }
        };
    let before_text = TextPage::build(&before_semantic, context.text);
    let after_text = TextPage::build(&after_semantic, context.text);
    let before_words = before_text.words();
    let after_words = after_text.words();
    let before_signatures = word_signatures(&before_semantic, &before_text, &before_words);
    let after_signatures = word_signatures(&after_semantic, &after_text, &after_words);
    if before_signatures == after_signatures {
        return PageDiff::Same;
    }
    let (before_span, after_span) = changed_spans(&before_signatures, &after_signatures);
    let rect = changed_rect(&before_words, &after_words, before_span, after_span);
    let previous_text = before_text.all_text();
    let current_text = after_text.all_text();

    // The PNG evidence is part of the change record: a text difference
    // without its renders is reported as a skip, honestly, rather than as a
    // render-less change.
    let before = match render_page_evidence(before_snapshot, page, context.evidence) {
        Ok(render) => render,
        Err(err) => {
            return PageDiff::Skipped(format!(
                "revision {previous_revision} page {page} text differs but its evidence render failed: {err}"
            ));
        }
    };
    let after = match render_page_evidence(after_snapshot, page, context.evidence) {
        Ok(render) => render,
        Err(err) => {
            return PageDiff::Skipped(format!(
                "revision {revision} page {page} text differs but its evidence render failed: {err}"
            ));
        }
    };
    // A geometry change (crop, rotation, media box) legitimately resizes the
    // render; the pixel diff is then meaningless, but both images stand.
    let diff = diff_renders(&before, &after).ok();

    let mut objects = BTreeSet::new();
    collect_content_objects(before_snapshot, page, &mut objects);
    collect_content_objects(after_snapshot, page, &mut objects);
    let objects: Vec<String> = objects.into_iter().collect();
    let finding = Finding {
        code: TEXT_CHANGED_BETWEEN_REVISIONS,
        strength: EvidenceStrength::StrongDerived,
        explanation: explain(
            page,
            previous_revision,
            revision,
            &objects,
            rect,
            &previous_text,
            &current_text,
        ),
        limitation: Some(
            "A text difference reports what changed between two saves, not why: an innocent \
             correction and a dishonest edit read the same, and a change that keeps every \
             extracted word, glyph, and font identical — pure repositioning, or paint outside \
             extracted text — is not reported."
                .to_owned(),
        ),
        requires_mode: AnalysisMode::Full,
        rests_on_recovered_structure: context.recovered,
    };
    PageDiff::Changed(Box::new(RevisionTextChange {
        finding,
        page,
        previous_revision,
        revision,
        previous_text,
        current_text,
        rect,
        objects,
        before,
        after,
        diff,
    }))
}

/// One extracted word's comparison identity: what it says, what font said
/// it, and which glyphs it set. A re-typed run that keeps the same Unicode
/// but swaps the font — or resolves the same codes to different glyph ids —
/// still differs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WordSignature {
    /// The word's extracted text.
    text: String,
    /// The backing font dictionary, subtype, and base font of the word's
    /// first character.
    font: String,
    /// Glyph ids of the word's characters, in order.
    glyphs: Vec<u32>,
}

/// The comparison identity of every extracted word on the page.
fn word_signatures(
    semantic: &SemanticPage,
    text: &TextPage,
    words: &[TextWord],
) -> Vec<WordSignature> {
    words
        .iter()
        .map(|word| {
            let end = word
                .first_char
                .saturating_add(word.char_count)
                .min(text.char_count());
            let range = text.chars().get(word.first_char..end).unwrap_or(&[]);
            let font = range
                .first()
                .and_then(|info| semantic.fonts.get(info.font.index()))
                .map(font_key)
                .unwrap_or_else(|| "unknown".to_owned());
            WordSignature {
                text: word.text.clone(),
                font,
                glyphs: range.iter().map(|info| info.glyph_id).collect(),
            }
        })
        .collect()
}

/// The font identity behind one word: backing object, subtype, base font.
fn font_key(font: &SemFont) -> String {
    let object = font
        .object
        .map(|id| id.to_string())
        .unwrap_or_else(|| "direct".to_owned());
    format!(
        "{}|{}|{}",
        object,
        String::from_utf8_lossy(&font.subtype),
        String::from_utf8_lossy(&font.base_font)
    )
}

/// The changed word spans on each side: the middle that remains after
/// trimming the common prefix and the common suffix. Single-edit saves —
/// the tamper shape — land exactly; several separated edits on one page
/// merge into one span covering first to last.
fn changed_spans(
    before: &[WordSignature],
    after: &[WordSignature],
) -> (Range<usize>, Range<usize>) {
    let mut prefix = 0;
    while prefix < before.len() && prefix < after.len() && before.get(prefix) == after.get(prefix) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < before.len().saturating_sub(prefix)
        && suffix < after.len().saturating_sub(prefix)
        && before.get(before.len().saturating_sub(1).saturating_sub(suffix))
            == after.get(after.len().saturating_sub(1).saturating_sub(suffix))
    {
        suffix += 1;
    }
    (
        prefix..before.len().saturating_sub(suffix),
        prefix..after.len().saturating_sub(suffix),
    )
}

/// Union of the changed words' boxes from both sides, in thousandths of a
/// point. `None` when neither span carries a word with a finite box.
fn changed_rect(
    before_words: &[TextWord],
    after_words: &[TextWord],
    before_span: Range<usize>,
    after_span: Range<usize>,
) -> Option<[i64; 4]> {
    let mut rect: Option<Rect> = None;
    let changed = before_words
        .get(before_span)
        .unwrap_or(&[])
        .iter()
        .chain(after_words.get(after_span).unwrap_or(&[]));
    for word in changed {
        rect = Some(rect.map_or(word.bbox, |acc| acc.union(word.bbox)));
    }
    rect.and_then(millipoints)
}

/// User-space points to thousandths of a point, normalized. `None` for a
/// non-finite box, which extraction never emits but this crate will not
/// bless into a finding either.
fn millipoints(bbox: Rect) -> Option<[i64; 4]> {
    let values = [bbox.x0, bbox.y0, bbox.x1, bbox.y1];
    if !values.iter().all(|v| v.is_finite()) {
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

/// The `/Contents` object ids behind one replayed page: a single reference,
/// or every reference in a contents array.
fn collect_content_objects(snapshot: &DocumentSnapshot, page: u32, objects: &mut BTreeSet<String>) {
    let page_ref = match snapshot.page(PageIndex(page)) {
        Ok(page_ref) => page_ref,
        Err(_) => return,
    };
    match page_ref.contents.as_ref() {
        Some(PdfObject::Reference(id)) => {
            objects.insert(id.to_string());
        }
        Some(PdfObject::Array(items)) => {
            for item in items.iter() {
                if let PdfObject::Reference(id) = item {
                    objects.insert(id.to_string());
                }
            }
        }
        _ => {}
    }
}

/// The finding's self-contained explanation: page, revision pair, content
/// objects, rectangle, and both verbatim strings.
fn explain(
    page: u32,
    previous_revision: u32,
    revision: u32,
    objects: &[String],
    rect: Option<[i64; 4]>,
    previous_text: &str,
    current_text: &str,
) -> String {
    let objects = if objects.is_empty() {
        "unknown".to_owned()
    } else {
        objects.join(", ")
    };
    let rect = rect.map_or_else(
        || "rect unknown".to_owned(),
        |r| format!("[{} {} {} {}]", r[0], r[1], r[2], r[3]),
    );
    format!(
        "Page {page} text changed between revision {previous_revision} and revision {revision} \
         (content {objects}; {rect}): previous {previous_text:?}, current {current_text:?}."
    )
}
