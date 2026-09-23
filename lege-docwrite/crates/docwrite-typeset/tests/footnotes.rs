//! A footnote is placed on the page of its reference and survives a repagination.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use docwrite_typeset::{book_of_pages, Document, Face, Geometry, Paragraph, ParagraphStyle};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn footnotes_stay_on_the_reference_page_through_an_edit() {
    let mut paragraphs = Vec::new();
    for index in 0..6u64 {
        paragraphs.push(Paragraph {
            id: index + 1,
            text: format!("Paragraph {index}"),
            style: ParagraphStyle::default(),
            note: (index == 2).then(|| "The note that belongs on this page.".into()),
            note_is_endnote: false,
        });
    }
    let mut document = Document::new(face(), Geometry::one_line_pages(), paragraphs.clone()).unwrap();
    assert_eq!(document.page_footnotes(3), vec!["The note that belongs on this page.".to_string()]);
    assert!(document.page_footnotes(1).is_empty());
    let long = "The note continues across the page boundary because it is longer than one footnote line. ".repeat(3);
    paragraphs[2].note = Some(long.clone());
    let split = Document::new(face(), Geometry::one_line_pages(), paragraphs).unwrap();
    let mut joined = String::new();
    let mut pieces = 0u32;
    for page in 1..=split.page_count() {
        for note in split.page_footnotes(page) {
            joined.push_str(&note);
            pieces += 1;
        }
    }
    assert!(pieces >= 2, "a long note splits across pages, got {pieces} pieces");
    assert_eq!(joined, long);
    let _ = document.edit_page(1, "x").unwrap();
    assert_eq!(
        document.page_footnotes(3),
        vec!["The note that belongs on this page.".to_string()],
        "the note did not drift when an earlier page was edited"
    );
    let _ = book_of_pages;
}
