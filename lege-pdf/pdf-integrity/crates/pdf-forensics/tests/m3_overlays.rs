//! M3 annotation overlays: a covering FreeText/Stamp/Redact appearance
//! reports the subtype, rectangle, affected underlying text,
//! appearance-stream text, and glyph-area coverage — while highlights,
//! widgets, and off-text annotations stay silent.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{
    AnalysisMode, AnnotationMode, AuditReport, Disposition, EvidenceOptions, EvidenceStrength,
    OverlayOptions, OverlayOutcome, audit_bytes, detect_annotation_overlays, diff_renders,
    render_bytes_evidence,
};
use pdf_forensics::{EXPLICIT_ANNOTATION_OVER_TEXT, encode_png};
use pdf_test_support::builder;

/// Audit `bytes` and run the overlay analysis over the same parse.
fn detect(bytes: &[u8]) -> (AuditReport, OverlayOutcome) {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    let outcome = detect_annotation_overlays(
        bytes,
        &report.document.parse,
        None,
        &OverlayOptions::default(),
    );
    (report, outcome)
}

#[test]
fn freetext_covering_text_reports_subtype_rect_texts_and_coverage() {
    let (report, outcome) = detect(&builder::freetext_overlay());

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.overlays.len(), 1);
    let overlay = &outcome.overlays[0];
    assert_eq!(overlay.page, 0);
    assert_eq!(overlay.object.as_deref(), Some("60 0 R"));
    assert_eq!(overlay.subtype, "FreeText");
    assert_eq!(overlay.rect, [70_000, 705_000, 265_000, 735_000]);
    assert_eq!(overlay.affected_text, "Cumulative GPA: 2.84");
    assert_eq!(overlay.appearance_text, "3.84");
    assert!(overlay.coverage > 0.0 && overlay.coverage <= 1.0);

    assert_eq!(overlay.finding.code, EXPLICIT_ANNOTATION_OVER_TEXT);
    assert_eq!(overlay.finding.strength, EvidenceStrength::StrongDerived);
    assert!(overlay.finding.limitation.is_some());
    assert_eq!(overlay.finding.requires_mode, AnalysisMode::Full);
    assert!(!overlay.finding.rests_on_recovered_structure);
    assert!(overlay.finding.explanation.contains("FreeText"));
    assert!(overlay.finding.explanation.contains("Cumulative GPA: 2.84"));
    assert!(overlay.finding.explanation.contains("3.84"));

    assert_eq!(
        (overlay.off.width(), overlay.off.height()),
        (overlay.on.width(), overlay.on.height())
    );
    assert!(overlay.diff.changed_pixels > 0);
    assert_ne!(
        encode_png(&overlay.off).unwrap(),
        encode_png(&overlay.on).unwrap()
    );

    // The emitted finding satisfies the schema's own rules.
    let finding_report = AuditReport::new_with_facts(
        AnalysisMode::Full,
        Disposition::ReviewRecommended,
        vec![overlay.finding.clone()],
        report.document.clone(),
    );
    finding_report.validate().unwrap();
}

#[test]
fn stamp_image_covering_text_reports_with_empty_appearance_text() {
    let (_, outcome) = detect(&builder::stamp_image_overlay());

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.overlays.len(), 1);
    let overlay = &outcome.overlays[0];
    assert_eq!(overlay.subtype, "Stamp");
    assert_eq!(overlay.object.as_deref(), Some("60 0 R"));
    assert_eq!(overlay.affected_text, "Cumulative GPA: 2.84");
    assert!(overlay.appearance_text.is_empty());
    assert!(overlay.coverage > 0.0);
    assert_eq!(overlay.finding.code, EXPLICIT_ANNOTATION_OVER_TEXT);
}

#[test]
fn redact_covering_text_reports() {
    let (_, outcome) = detect(&builder::redact_overlay());

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.overlays.len(), 1);
    let overlay = &outcome.overlays[0];
    assert_eq!(overlay.subtype, "Redact");
    assert_eq!(overlay.affected_text, "Cumulative GPA: 2.84");
    assert!(overlay.coverage > 0.0);
}

/// A covering appearance far from any text paints but reports nothing.
#[test]
fn freetext_elsewhere_yields_no_finding() {
    let bytes = builder::freetext_elsewhere();
    let (_, outcome) = detect(&bytes);

    assert!(outcome.skipped.is_empty());
    assert!(outcome.overlays.is_empty());

    // It really does paint — the silence is geometry, not an empty diff.
    assert_off_on_differ(&bytes);
}

/// A highlight over text changes glyph pixels and must still stay silent.
#[test]
fn highlight_over_text_yields_no_finding() {
    let bytes = builder::highlight_over_text();
    let (_, outcome) = detect(&bytes);

    assert!(outcome.skipped.is_empty());
    assert!(outcome.overlays.is_empty());

    // The synthesized markup fill really does paint over the glyphs.
    assert_off_on_differ(&bytes);
}

/// A widget with an opaque appearance over text must stay silent.
#[test]
fn widget_over_text_yields_no_finding() {
    let bytes = builder::widget_over_text();
    let (_, outcome) = detect(&bytes);

    assert!(outcome.skipped.is_empty());
    assert!(outcome.overlays.is_empty());

    // The widget appearance really does erase glyph pixels.
    assert_off_on_differ(&bytes);
}

/// Render page 0 with annotations off and on and require the renders to
/// differ: the control proving a silent outcome is a rule, not no paint.
fn assert_off_on_differ(bytes: &[u8]) {
    let off = render_bytes_evidence(
        bytes,
        0,
        None,
        &EvidenceOptions {
            annotations: AnnotationMode::None,
            ..EvidenceOptions::default()
        },
    )
    .unwrap();
    let on = render_bytes_evidence(bytes, 0, None, &EvidenceOptions::default()).unwrap();
    assert!(diff_renders(&off, &on).unwrap().changed_pixels > 0);
}
