//! Short paragraphs share a page; one line is not one page.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use docwrite_model::{Book, ChapterStart};
use docwrite_typeset::{Face, from_book};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn five_one_line_paragraphs_fit_on_one_page() {
    let mut book = Book::new("Dialogue");
    book.insert("\"Yes,\" she said.\n\"No,\" he said.\n\"Why?\"\n\"Because.\"\nSilence.")
        .unwrap();
    assert!(book.block_ids().len() >= 5, "newlines split blocks");
    let mut template = book.chapter_templates()[0].clone();
    template.start = ChapterStart::NextPage;
    book.set_chapter_template(template).unwrap();
    let document = from_book(&book, face()).unwrap();
    assert_eq!(
        document.page_count(),
        1,
        "five one-line paragraphs on {} pages",
        document.page_count()
    );
}
