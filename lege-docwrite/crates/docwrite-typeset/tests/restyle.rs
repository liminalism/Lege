//! Changing Body and laying out again matches a fresh layout of the same text.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use docwrite_typeset::{Document, Face, Geometry, Paragraph, ParagraphStyle};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

fn paras(text: &str) -> Vec<Paragraph> {
    (0..12)
        .map(|index| Paragraph {
            id: index,
            text: format!("{text} {index}"),
            style: ParagraphStyle::default(),
            note: None,
            note_is_endnote: false,
        })
        .collect()
}

#[test]
fn body_restyle_matches_a_fresh_layout() {
    let geometry = Geometry::one_line_pages();
    let mut edited = Document::new(face(), geometry, paras("Body")).unwrap();
    let restyled = edited.restyle_body(18.0, 32.0).unwrap();
    let mut fresh_geometry = geometry;
    fresh_geometry.font_size = 18.0;
    fresh_geometry.leading = 32.0;
    let fresh = Document::new(face(), fresh_geometry, paras("Body")).unwrap();
    let fresh_pages: Vec<_> = (1..=fresh.page_count())
        .map(|page| fresh.page_texts(page))
        .collect();
    assert_eq!(restyled, fresh_pages);
    println!(
        "body restyle at 18pt matches a fresh layout across {} pages",
        fresh.page_count()
    );
}
