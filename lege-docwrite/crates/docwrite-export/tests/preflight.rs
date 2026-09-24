//! Preflight names the problems it was asked to find.

#![allow(clippy::unwrap_used)]

use docwrite_export::{AssetFacts, preflight};
use docwrite_model::Book;

#[test]
fn preflight_reports_fonts_images_and_broken_links() {
    let mut book = Book::new("Essay");
    book.insert_with_marks(
        "See",
        docwrite_model::RunMarks {
            link: Some("missing-figure".into()),
            ..docwrite_model::RunMarks::default()
        },
    )
    .unwrap();
    let facts = AssetFacts {
        fonts_on_disk: &["Body".into()],
        embeddable: &[],
        images: &[("plate", 72)],
        linked_targets_that_exist: &[],
    };
    let report = preflight(&book, &facts);
    assert!(report.missing_fonts.iter().any(|font| font == "Noto Sans"));
    assert!(report.unembeddable_fonts.iter().any(|font| font == "Body"));
    assert!(
        report
            .low_resolution_images
            .iter()
            .any(|image| image == "plate")
    );
    assert!(
        report
            .broken_references
            .iter()
            .any(|link| link == "missing-figure")
    );
    assert!(!report.font_embedding_warnings.is_empty());
    println!("preflight missing fonts: {:?}", report.missing_fonts);
    println!(
        "preflight unembeddable fonts: {:?}",
        report.unembeddable_fonts
    );
    println!(
        "preflight low-resolution images: {:?}",
        report.low_resolution_images
    );
    println!(
        "preflight broken references: {:?}",
        report.broken_references
    );
    println!(
        "preflight font-embedding warnings: {:?}",
        report.font_embedding_warnings
    );
}
