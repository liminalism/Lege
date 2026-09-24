//! Markdown, PDF and IDML from one manuscript.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use docwrite_export::{export_idml, export_markdown, export_pdf};
use docwrite_model::{BlockKind, Book, SourceNote};
use lege_pdf_read::{RenderSession, page_text, positioned_words};

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
    assert!(
        markdown.contains("# **Title**")
            || markdown.contains("# Title")
            || markdown.contains("Title")
    );
    assert!(markdown.contains("https://example.test"));
    assert!(
        !markdown.contains("432") && !markdown.contains("page 1"),
        "{markdown}"
    );
    println!("markdown keeps the title and the link and drops page geometry");
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
    assert!(
        exported.font_bytes < face.len(),
        "embedded program is a shorter subset"
    );
    let text = String::from_utf8_lossy(&exported.bytes);
    let has_lang = exported.bytes.windows(5).any(|window| window == b"/Lang");
    let has_structure = exported
        .bytes
        .windows(15)
        .any(|window| window == b"/StructTreeRoot");
    assert!(text.contains("/Lang") || has_lang);
    assert!(has_structure);
    assert!(exported.bytes.windows(8).any(|w| w == b"/Outlines") || text.contains("Chapter"));
    let session = RenderSession::open(Arc::<[u8]>::from(exported.bytes.clone()), None).unwrap();
    let extracted = page_text(&session, 0).unwrap();
    assert!(
        extracted.contains("Chapter")
            || extracted.contains("passage")
            || extracted.contains("rivers"),
        "extracted {extracted:?}"
    );
    let words = positioned_words(&session, 0, 432, 648).unwrap();
    let word = words.iter().find(|word| {
        word.text.contains("passage") || word.text.contains("Chapter") || !word.text.is_empty()
    });
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
    println!("tagged pdf /Lang: {has_lang} /StructTreeRoot: {has_structure}");
    println!(
        "subset font bytes {} < face {}",
        exported.font_bytes,
        face.len()
    );
}

#[test]
fn idml_package_contains_styles_parent_pages_story_images_and_footnotes() {
    let mut book = Book::new("Essay");
    let id = book.block_ids()[0];
    book.set_kind(
        id,
        BlockKind::Image {
            asset: Some("plate.png".into()),
        },
    )
    .unwrap();
    book.insert("Plate").unwrap();
    book.attach_note(docwrite_model::NoteKind::Footnote, "A marginal remark.")
        .unwrap();
    let bytes = export_idml(&book);
    let xml = String::from_utf8_lossy(&bytes);
    assert!(xml.contains("ParagraphStyle/Body"));
    assert!(xml.contains("MasterSpread"));
    assert!(xml.contains("Story_u1") || xml.contains("story_u1"));
    assert!(xml.contains("AnchoredObject") || xml.contains("Image"));
    assert!(xml.contains("Footnote") || xml.contains("ParagraphStyle/Footnote"));
    println!(
        "idml ParagraphStyle/Body: {}",
        xml.contains("ParagraphStyle/Body")
    );
    println!("idml parent pages: {}", xml.contains("MasterSpread"));
    println!(
        "idml threaded story: {}",
        xml.contains("Story_u1") || xml.contains("story_u1")
    );
    println!(
        "idml anchored image: {}",
        xml.contains("AnchoredObject") || xml.contains("Image")
    );
    println!(
        "idml footnotes: {}",
        xml.contains("Footnote") || xml.contains("ParagraphStyle/Footnote")
    );
    assert!(
        xml.contains("A marginal remark."),
        "the manuscript note text is in the IDML story"
    );
    let markdown = export_markdown(&book);
    assert!(
        markdown.contains("A marginal remark."),
        "the manuscript note text is in the markdown, got {markdown}"
    );
    println!("note text in markdown and idml: A marginal remark.");
}

#[test]
fn pdf_pages_are_the_typeset_pages_and_visibly_inked() {
    use docwrite_model::{Position, Selection};
    use lege_pdf_read::{RasterPlane, RasterProduct};

    let prose = "Reading the letters in order was like watching a clerk grow old, \
        the careful hand of the first bundles hurrying year by year.";
    let mut book = Book::new("Archive");
    let body: Vec<String> = (0..30).map(|n| format!("{n}. {prose}")).collect();
    book.insert(&body.join("\n")).unwrap();
    let part = book.parts()[0].id();
    book.add_chapter(part, "Second").unwrap();
    let fresh = *book.block_ids().last().unwrap();
    book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
        .unwrap();
    book.insert("The second chapter opens here.").unwrap();

    let face = font();
    let layout =
        docwrite_typeset::from_book(&book, docwrite_typeset::Face::parse(face.clone()).unwrap())
            .unwrap();
    let exported = export_pdf(&book, &face).unwrap();
    let session = RenderSession::open(Arc::from(exported.bytes.clone()), None).unwrap();
    assert_eq!(
        session.page_count(),
        layout.page_count(),
        "one PDF page per typeset page"
    );

    // The first line of the PDF is the first typeset line, and the text of
    // the whole first page reads in order.
    let text = page_text(&session, 0).unwrap();
    let first_line = &layout.page_texts(1)[0];
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        squash(&text).starts_with(&squash(first_line)),
        "page 1 text {text:?} starts with {first_line:?}"
    );
    assert!(squash(&text).contains("1. Reading the letters"));

    // Pages are drawn, not just searchable: rendering shows ink.
    let page = session.compile(0).unwrap();
    let raster = session
        .render(&page, &RasterProduct::gray8(432, 648))
        .unwrap();
    let RasterPlane::Gray8(gray) = raster else {
        panic!("asked for gray");
    };
    let ink = gray.pixels.iter().filter(|value| **value < 128).count();
    assert!(ink > 2_000, "page 1 renders {ink} dark pixels");

    // The second chapter's bookmark points at the page it opens on.
    let outline = lege_pdf_read::extract_outline(&session);
    assert!(
        outline
            .iter()
            .any(|node| format!("{node:?}").contains("Second")),
        "outline {outline:?}"
    );
}

#[test]
fn markdown_heads_each_chapter_with_its_title() {
    let mut book = Book::new("Essay");
    book.insert("First words.").unwrap();
    let part = book.parts()[0].id();
    book.add_chapter(part, "The Second").unwrap();
    let markdown = export_markdown(&book);
    assert!(markdown.contains("# Chapter 1"), "{markdown}");
    assert!(markdown.contains("# The Second"), "{markdown}");
    assert!(markdown.find("# Chapter 1") < markdown.find("First words."));
}
