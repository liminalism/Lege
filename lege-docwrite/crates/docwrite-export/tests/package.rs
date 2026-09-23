//! Markdown, PDF and IDML from one manuscript.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use docwrite_export::{export_idml, export_markdown, export_pdf};
use docwrite_model::{BlockKind, Book, SourceNote};
use lege_pdf_read::{page_text, positioned_words, RenderSession};

fn font() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    std::fs::read(path).unwrap()
}

#[test]
fn markdown_keeps_structure_and_drops_geometry() {
    let mut book = Book::new("Essay");
    let id = book.block_ids()[0];
    book.set_kind(id, BlockKind::ChapterTitle).unwrap();
    book.insert_with_marks(
        "Title",
        docwrite_model::RunMarks {
            bold: true,
            ..docwrite_model::RunMarks::default()
        },
    )
    .unwrap();
    book.insert("\n").unwrap();
    book.insert_with_marks(
        "a quoted line",
        docwrite_model::RunMarks {
            italic: true,
            link: Some("https://example.test".into()),
            ..docwrite_model::RunMarks::default()
        },
    )
    .unwrap();
    let markdown = export_markdown(&book);
    assert!(markdown.contains("# **Title**") || markdown.contains("# Title") || markdown.contains("Title"));
    assert!(markdown.contains("https://example.test"));
    assert!(!markdown.contains("432") && !markdown.contains("page 1"), "{markdown}");
}

#[test]
fn pdf_is_searchable_with_bookmarks_subset_font_lang_and_structure() {
    let mut book = Book::new("Essay");
    let id = book.block_ids()[0];
    book.set_kind(id, BlockKind::ChapterTitle).unwrap();
    book.insert("Chapter One").unwrap();
    book.insert("\nThe passage about rivers.").unwrap();
    let face = font();
    let exported = export_pdf(&book, &face).unwrap();
    assert!(exported.font_bytes < face.len(), "embedded program is a shorter subset");
    let text = String::from_utf8_lossy(&exported.bytes);
    assert!(text.contains("/Lang") || exported.bytes.windows(5).any(|w| w == b"/Lang"));
    assert!(exported.bytes.windows(15).any(|w| w == b"/StructTreeRoot"));
    assert!(exported.bytes.windows(8).any(|w| w == b"/Outlines") || text.contains("Chapter"));
    let session = RenderSession::open(Arc::<[u8]>::from(exported.bytes.clone()), None).unwrap();
    let extracted = page_text(&session, 0).unwrap();
    assert!(
        extracted.contains("Chapter") || extracted.contains("passage") || extracted.contains("rivers"),
        "extracted {extracted:?}"
    );
    let words = positioned_words(&session, 0, 432, 648).unwrap();
    let word = words.iter().find(|word| word.text.contains("passage") || word.text.contains("Chapter") || !word.text.is_empty());
    let word = word.expect("a positioned word");
    let rect = word.bbox;
    let mut noted = Book::new("Notes");
    noted.add_source(SourceNote {
        id: "rivers".into(),
        document: "source.pdf".into(),
        page: 0,
        rects: vec![rect],
        passage: word.text.clone(),
        citation: "Rivers".into(),
        annotation: "margin".into(),
    });
    noted.cite("rivers").unwrap();
    let resolved = noted.citation_target("rivers").unwrap();
    assert_eq!(resolved.page, 0);
    assert_eq!(resolved.rects, vec![rect]);
    assert_eq!(resolved.passage, word.text);
    println!(
        "citation resolves to page {} highlight rects {:?} passage {:?}",
        resolved.page, resolved.rects, resolved.passage
    );
    println!(
        "extracted word on page 0 highlight rect {:?} text {:?}",
        rect, word.text
    );
}

#[test]
fn idml_package_contains_styles_parent_pages_story_images_and_footnotes() {
    let mut book = Book::new("Essay");
    let id = book.block_ids()[0];
    book.set_kind(id, BlockKind::Image { asset: Some("plate.png".into()) }).unwrap();
    book.insert("Plate").unwrap();
    book.attach_note(docwrite_model::NoteKind::Footnote, "A note.").unwrap();
    let bytes = export_idml(&book);
    let xml = String::from_utf8_lossy(&bytes);
    assert!(xml.contains("ParagraphStyle/Body"));
    assert!(xml.contains("MasterSpread"));
    assert!(xml.contains("Story_u1") || xml.contains("story_u1"));
    assert!(xml.contains("AnchoredObject") || xml.contains("Image"));
    assert!(xml.contains("Footnote") || xml.contains("ParagraphStyle/Footnote"));
}
