//! The table of contents follows a chapter that moves onto another page.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use docwrite_typeset::{Document, Face, Geometry, Paragraph, ParagraphStyle};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

fn title(id: u64, text: &str) -> Paragraph {
    Paragraph {
        id,
        text: text.into(),
        style: ParagraphStyle {
            name: "Chapter Title".into(),
            ..ParagraphStyle::default()
        },
        note: None,
        note_is_endnote: false,
        note_mark: String::new(),
        runs: Vec::new(),
    }
}

fn body(id: u64) -> Paragraph {
    Paragraph {
        id,
        text: format!("Paragraph {id}"),
        style: ParagraphStyle::default(),
        note: None,
        note_is_endnote: false,
        note_mark: String::new(),
        runs: Vec::new(),
    }
}

#[test]
fn contents_page_numbers_follow_a_chapter_that_moves() {
    let paragraphs = vec![title(1, "One"), body(2), title(3, "Two"), body(4)];
    let mut document =
        Document::new(face(), Geometry::one_line_pages(), paragraphs.clone()).unwrap();
    let before = document.contents();
    assert_eq!(before, vec![("One".into(), 1), ("Two".into(), 3)]);
    document.insert_paragraphs_before(2, 2, "Inserted").unwrap();
    let after = document.contents();
    assert_eq!(after[0], ("One".into(), 1));
    assert!(
        after[1].1 > before[1].1,
        "chapter Two moved from {} to {}",
        before[1].1,
        after[1].1
    );
    println!("toc before cross-page move: {before:?}");
    println!(
        "toc after cross-page move: {after:?} (chapter Two {} -> {})",
        before[1].1, after[1].1
    );
    let _ = paragraphs;
}
