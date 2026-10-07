//! M3 flattened occlusion: opaque page-content paint covering earlier
//! text reports the covering operator, its provenance, the covered and
//! replacement texts, and the glyph-area coverage — while elsewhere,
//! translucent, and non-covering paint stays silent.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{
    AnalysisMode, AuditReport, Disposition, EvidenceOptions, EvidenceStrength, OcclusionKind,
    OcclusionOptions, OcclusionOutcome, PAGE_CONTENT_COVERS_TEXT, RASTER_PATCH_ON_VECTOR_PAGE,
    audit_bytes, detect_flattened_occlusion, diff_renders, render_bytes_evidence,
};
use pdf_test_support::builder;

/// Audit `bytes` and run the occlusion analysis over the same parse.
fn detect(bytes: &[u8]) -> (AuditReport, OcclusionOutcome) {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    let outcome = detect_flattened_occlusion(
        bytes,
        &report.document.parse,
        None,
        &OcclusionOptions::default(),
    );
    (report, outcome)
}

#[test]
fn white_rect_replacement_reports_cover_provenance_texts_and_coverage() {
    let (report, outcome) = detect(&builder::white_rect_replacement());

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.occlusions.len(), 1);
    let occlusion = &outcome.occlusions[0];
    assert_eq!(occlusion.page, 0);
    assert_eq!(occlusion.kind, OcclusionKind::Path);
    assert_eq!(occlusion.operator, "f");
    assert!(occlusion.content_op_index > 0);
    assert_eq!(occlusion.stream.as_deref(), Some("11 0 R"));
    assert!(occlusion.span.1 > occlusion.span.0);
    assert_eq!(occlusion.origin, "page-content");
    assert!(occlusion.stack.is_empty());
    assert_eq!(occlusion.rect, [70_000, 705_000, 265_000, 735_000]);
    assert_eq!(occlusion.covered_text, "Cumulative GPA: 2.84");
    assert_eq!(occlusion.replacement_text, "3.84");
    assert!(occlusion.coverage > 0.5 && occlusion.coverage <= 1.0);

    assert_eq!(occlusion.finding.code, PAGE_CONTENT_COVERS_TEXT);
    assert_eq!(
        occlusion.finding.strength,
        EvidenceStrength::ContextualIndicator
    );
    assert!(occlusion.finding.limitation.is_some());
    assert_eq!(occlusion.finding.requires_mode, AnalysisMode::Full);
    assert!(!occlusion.finding.rests_on_recovered_structure);
    assert!(occlusion.finding.explanation.contains("operator `f`"));
    assert!(
        occlusion
            .finding
            .explanation
            .contains("Cumulative GPA: 2.84")
    );
    assert!(occlusion.finding.explanation.contains("3.84"));
    assert!(occlusion.finding.explanation.contains("11 0 R"));

    assert!(occlusion.render.width() > 0 && occlusion.render.height() > 0);

    // The emitted finding satisfies the schema's own rules.
    let finding_report = AuditReport::new_with_facts(
        AnalysisMode::Full,
        Disposition::ReviewRecommended,
        vec![occlusion.finding.clone()],
        report.document.clone(),
    );
    finding_report.validate().unwrap();
}

#[test]
fn image_patch_over_text_reports_raster_patch_without_replacement() {
    let (_, outcome) = detect(&builder::image_patch_over_text());

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.occlusions.len(), 1);
    let occlusion = &outcome.occlusions[0];
    assert_eq!(occlusion.kind, OcclusionKind::Image);
    assert_eq!(occlusion.operator, "Do");
    assert_eq!(occlusion.stream.as_deref(), Some("11 0 R"));
    assert_eq!(occlusion.rect, [70_000, 705_000, 265_000, 735_000]);
    assert_eq!(occlusion.covered_text, "Cumulative GPA: 2.84");
    assert!(occlusion.replacement_text.is_empty());
    assert!(occlusion.coverage > 0.5);
    assert_eq!(occlusion.finding.code, RASTER_PATCH_ON_VECTOR_PAGE);
    assert_eq!(
        occlusion.finding.strength,
        EvidenceStrength::ContextualIndicator
    );
    assert!(occlusion.finding.explanation.contains("operator `Do`"));
}

#[test]
fn form_rect_cover_reports_form_origin_and_stack() {
    let (_, outcome) = detect(&builder::form_rect_replacement());

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.occlusions.len(), 1);
    let occlusion = &outcome.occlusions[0];
    assert_eq!(occlusion.kind, OcclusionKind::Path);
    assert_eq!(occlusion.finding.code, PAGE_CONTENT_COVERS_TEXT);
    assert_eq!(occlusion.operator, "f");
    assert_eq!(occlusion.origin, "form-xobject");
    assert_eq!(occlusion.stack, vec!["form-xobject 61 0 R".to_owned()]);
    assert_eq!(occlusion.stream.as_deref(), Some("61 0 R"));
    assert_eq!(occlusion.covered_text, "Cumulative GPA: 2.84");
    assert_eq!(occlusion.replacement_text, "3.84");
    assert!(occlusion.coverage > 0.5);
}

/// An opaque rectangle far from any text paints but reports nothing.
#[test]
fn grey_rect_elsewhere_yields_no_finding() {
    let bytes = builder::grey_rect_elsewhere();
    let (_, outcome) = detect(&bytes);

    assert!(outcome.skipped.is_empty());
    assert!(outcome.occlusions.is_empty());

    // It really does paint — the silence is geometry, not an empty diff.
    assert_paints_over_plain(&bytes);
}

/// A half-alpha rectangle over text paints over the glyphs and must still
/// stay silent: translucency is not an occlusion candidate.
#[test]
fn translucent_rect_over_text_yields_no_finding() {
    let bytes = builder::translucent_rect_over_text();
    let (_, outcome) = detect(&bytes);

    assert!(outcome.skipped.is_empty());
    assert!(outcome.occlusions.is_empty());

    // The translucent rectangle really does repaint the glyph pixels.
    assert_paints_over_plain(&bytes);
}

/// Render page 0 of `bytes` and of the plain GPA text page and require the
/// renders to differ: the control proving a silent outcome is a rule, not
/// no paint.
fn assert_paints_over_plain(bytes: &[u8]) {
    let options = EvidenceOptions::default();
    let plain = render_bytes_evidence(&builder::gpa_text_page(), 0, None, &options).unwrap();
    let covered = render_bytes_evidence(bytes, 0, None, &options).unwrap();
    assert!(diff_renders(&plain, &covered).unwrap().changed_pixels > 0);
}
