//! M1 fixture suite: one known-answer document per triage shape.
//!
//! Each test audits a fixture built by `pdf-test-support` — or, for the
//! PDF-Verse export, the committed bytes those builders produce — and pins
//! the per-page representation classification and the document feature
//! flags. A new fixture shape belongs here, next to its siblings, rather
//! than in a one-off test.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{AuditReport, FeatureFacts, OpenState, PageRepresentation, audit_bytes};
use pdf_test_support::builder;

/// The committed PDF-Verse-style export, written by
/// `crates/pdf-forensics/examples/gen_m1_fixtures.rs`. The include fails to
/// compile when the file is missing, so the suite cannot silently skip it.
const PDFVERSE_EXPORT: &[u8] = include_bytes!("../../../fixtures/pdfverse-export.pdf");

/// Page count shared with that generator: three full-page-raster pages, no
/// fonts. The byte-equality check in `pdfverse_export_matches_its_generator`
/// keeps the two in step.
const PDFVERSE_PAGES: u32 = 3;

/// Audit `bytes` and require the report to satisfy the schema's own rules.
fn audit(bytes: &[u8]) -> AuditReport {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    report
}

/// The expected flags for a fixture that sets none of them. Written out so
/// a new flag fails to compile here rather than passing silently.
fn no_features() -> FeatureFacts {
    FeatureFacts {
        has_acroform: false,
        has_xfa: false,
        has_javascript: false,
        has_outlines: false,
        has_optional_content: false,
        uses_object_streams: false,
        has_embedded_files: false,
    }
}

fn representations(report: &AuditReport) -> Vec<PageRepresentation> {
    report
        .document
        .pages
        .value()
        .unwrap()
        .iter()
        .map(|p| p.representation)
        .collect()
}

#[test]
fn clean_vector_document() {
    let report = audit(&builder::multipage_classic(3));

    assert_eq!(report.document.parse.state, OpenState::Clean);
    assert_eq!(
        representations(&report),
        vec![
            PageRepresentation::Vector,
            PageRepresentation::Vector,
            PageRepresentation::Vector
        ]
    );
    assert_eq!(*report.document.features.value().unwrap(), no_features());
    assert_eq!(report.document.fonts.value().unwrap().len(), 1);
    assert!(report.document.images.value().unwrap().is_empty());
}

#[test]
fn ordinary_scan_is_raster_on_every_page() {
    let report = audit(&builder::raster_scan(3, false));

    assert_eq!(report.document.parse.state, OpenState::Clean);
    assert_eq!(
        representations(&report),
        vec![
            PageRepresentation::RasterOnly,
            PageRepresentation::RasterOnly,
            PageRepresentation::RasterOnly
        ]
    );
    assert_eq!(*report.document.features.value().unwrap(), no_features());
    // A scan has no fonts at all; every page carries its own image.
    assert!(report.document.fonts.value().unwrap().is_empty());
    assert_eq!(report.document.images.value().unwrap().len(), 3);
    let pages = report.document.pages.value().unwrap();
    assert!(pages.iter().all(|p| p.image_coverage_bps == 10_000));
    assert!(pages.iter().all(|p| p.effective_dpi.is_some()));
}

#[test]
fn scan_with_ocr_layer_carries_invisible_text() {
    let report = audit(&builder::raster_scan(2, true));

    assert_eq!(
        representations(&report),
        vec![
            PageRepresentation::RasterWithTextLayer,
            PageRepresentation::RasterWithTextLayer
        ]
    );
    assert_eq!(*report.document.features.value().unwrap(), no_features());
    assert_eq!(report.document.fonts.value().unwrap().len(), 1);
    assert_eq!(report.document.images.value().unwrap().len(), 2);
}

#[test]
fn pdfverse_export_matches_its_generator() {
    // Byte-equality with the builder output is the reproducibility gate: the
    // committed file must be exactly what the generator writes.
    assert_eq!(PDFVERSE_EXPORT, builder::raster_scan(PDFVERSE_PAGES, false));

    let report = audit(PDFVERSE_EXPORT);

    assert_eq!(report.document.parse.state, OpenState::Clean);
    assert_eq!(
        representations(&report),
        vec![PageRepresentation::RasterOnly; PDFVERSE_PAGES as usize]
    );
    assert_eq!(*report.document.features.value().unwrap(), no_features());
    assert!(report.document.fonts.value().unwrap().is_empty());
    assert_eq!(
        report.document.images.value().unwrap().len(),
        PDFVERSE_PAGES as usize
    );
}

#[test]
fn one_raster_page_outlier_in_a_vector_document() {
    let report = audit(&builder::vector_with_raster_outlier(3, 1));

    assert_eq!(report.document.parse.state, OpenState::Clean);
    assert_eq!(
        representations(&report),
        vec![
            PageRepresentation::Vector,
            PageRepresentation::RasterOnly,
            PageRepresentation::Vector
        ]
    );
    assert_eq!(*report.document.features.value().unwrap(), no_features());
    assert_eq!(report.document.fonts.value().unwrap().len(), 1);
    assert_eq!(report.document.images.value().unwrap().len(), 1);
}

#[test]
fn form_filled_acroform() {
    let report = audit(&builder::acroform_filled());

    assert_eq!(report.document.parse.state, OpenState::Clean);
    assert_eq!(representations(&report), vec![PageRepresentation::Vector]);
    let features = report.document.features.value().unwrap();
    assert!(features.has_acroform);
    assert!(!features.has_xfa);
    // Bare field dictionaries, not widgets: no annotations, no signatures.
    assert_eq!(report.document.annotations.value().unwrap().total(), 0);
    let signatures = report.document.signatures.value().unwrap();
    assert!(signatures.signatures.is_empty());
    assert_eq!(signatures.unsigned_fields, 0);
}

#[test]
fn annotation_heavy_document() {
    let report = audit(&builder::annotated());

    assert_eq!(report.document.parse.state, OpenState::Clean);
    assert_eq!(representations(&report), vec![PageRepresentation::Vector]);
    assert_eq!(*report.document.features.value().unwrap(), no_features());
    let annotations = report.document.annotations.value().unwrap();
    assert_eq!(annotations.total(), 3);
    for subtype in ["Text", "Highlight", "Ink"] {
        assert_eq!(
            annotations.count_per_subtype.get(subtype),
            Some(&1),
            "expected one /{subtype} annotation"
        );
    }
}

#[test]
fn malformed_but_recoverable_document() {
    let report = audit(&builder::without_xref(builder::multipage_classic(2)));

    assert_eq!(report.document.parse.state, OpenState::Recovered);
    assert!(report.document.parse.xref_rebuilt);
    assert!(!report.document.parse.recovery_events.is_empty());
    // It opened, so the inventory is full — resting on repaired structure.
    assert_eq!(
        representations(&report),
        vec![PageRepresentation::Vector, PageRepresentation::Vector]
    );
    assert_eq!(*report.document.features.value().unwrap(), no_features());
}

#[test]
fn linearized_document() {
    let bytes = builder::linearized();
    assert!(
        bytes
            .windows(b"/Linearized".len())
            .any(|w| w == b"/Linearized"),
        "the fixture must carry the linearization marker"
    );

    let report = audit(&bytes);

    assert_eq!(report.document.parse.state, OpenState::Clean);
    assert_eq!(representations(&report), vec![PageRepresentation::Vector]);
    assert_eq!(*report.document.features.value().unwrap(), no_features());
    // The first-object marker is a determined fact, not a pending gap.
    let linearization = report.document.linearization.value().unwrap();
    assert!(
        linearization.linearized,
        "the first-object /Linearized marker must be detected"
    );
    assert!(
        !report
            .document
            .pending
            .iter()
            .any(|p| p.section == "linearization"),
        "linearization is surfaced, so it must not stay pending"
    );
}

#[test]
fn hybrid_xref_document() {
    let report = audit(&builder::hybrid_fixture(2));

    assert_eq!(
        representations(&report),
        vec![PageRepresentation::Vector, PageRepresentation::Vector]
    );
    let features = report.document.features.value().unwrap();
    assert!(features.uses_object_streams);
    assert_eq!(report.document.fonts.value().unwrap().len(), 1);
}
