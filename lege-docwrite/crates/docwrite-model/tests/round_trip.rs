//! A bundle round trip keeps everything: structure, ids, block kinds, marks,
//! notes, styles, masters, templates and research records.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_model::{
    BibliographyEntry, BlockKind, Book, ChapterStart, NoteKind, Position, RunMarks, Selection,
    SourceNote,
};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("docwrite-roundtrip-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("Book.legebook")
}

fn rich_book() -> Book {
    let mut book = Book::new("The Archive");
    book.insert("# Not a heading, and\n@block not a directive either.")
        .unwrap();
    book.insert_with_marks(
        " Bold italic link.",
        RunMarks {
            bold: true,
            italic: true,
            link: Some("https://example.test".into()),
            citation: Some("smith1999".into()),
            ..RunMarks::default()
        },
    )
    .unwrap();
    book.attach_note(NoteKind::Footnote, "A footnote body.")
        .unwrap();
    let quote = book.block_ids()[0];
    book.set_kind(quote, BlockKind::BlockQuote).unwrap();

    let part = book.add_part("Second Part");
    let chapter = book.add_chapter(part, "Appendix").unwrap();
    let fresh = *book.block_ids().last().unwrap();
    book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
        .unwrap();
    book.insert("Appendix text.").unwrap();
    book.attach_note(NoteKind::Endnote, "An endnote body.")
        .unwrap();

    let mut master = book.page_masters()[0].clone();
    master.name = "Appendix Body".into();
    master.margin_inner = 90.0;
    master.running_head = false;
    book.add_page_master(master).unwrap();
    let mut template = book.chapter_templates()[0].clone();
    template.name = "Appendix".into();
    template.master = "Appendix Body".into();
    template.start = ChapterStart::NextPage;
    book.add_chapter_template(template).unwrap();
    book.apply_template(chapter, "Appendix").unwrap();

    let mut body = book
        .paragraph_styles()
        .iter()
        .find(|style| style.name == "Body")
        .unwrap()
        .clone();
    body.size_pt = 11.5;
    body.hyphenate = false;
    book.set_paragraph_style(body).unwrap();

    book.add_source(SourceNote {
        id: "src1".into(),
        document: "/tmp/letters.pdf".into(),
        page: 12,
        rects: vec![[10.0, 20.0, 110.0, 32.0]],
        passage: "a passage".into(),
        citation: "Smith 1999, 12".into(),
        annotation: "check the date".into(),
    });
    book.add_bibliography(BibliographyEntry {
        key: "smith1999".into(),
        kind: "book".into(),
        title: "Letters".into(),
        author: "Smith".into(),
        issued: "1999".into(),
    });
    book.add_index_term("archive", quote);
    book.set_selection(Selection {
        anchor: Position::new(quote, 2),
        focus: Position::new(quote, 9),
    })
    .unwrap();
    book
}

#[test]
fn a_saved_book_reopens_identical() {
    let path = scratch("rich");
    let mut book = rich_book();
    book.save_bundle(&path).unwrap();
    let back = Book::load_bundle(&path).unwrap();

    assert_eq!(back.title(), book.title());
    assert_eq!(
        back.parts(),
        book.parts(),
        "parts, chapters, sections, blocks, runs, ids"
    );
    assert_eq!(back.block_ids(), book.block_ids());
    assert_eq!(back.paragraph_styles(), book.paragraph_styles());
    assert_eq!(back.page_masters(), book.page_masters());
    assert_eq!(back.chapter_templates(), book.chapter_templates());
    assert_eq!(back.bibliography(), book.bibliography());
    assert_eq!(back.index_terms(), book.index_terms());
    assert_eq!(back.source("src1"), book.source("src1"));
    assert_eq!(back.selection(), book.selection());
    let notes = |book: &Book| -> Vec<(NoteKind, String)> {
        book.blocks()
            .filter_map(|block| block.note())
            .map(|id| {
                let note = book.note(id).unwrap();
                (note.kind(), note.text())
            })
            .collect()
    };
    assert_eq!(notes(&back), notes(&book));
    assert_eq!(notes(&back).len(), 2);
    assert!(
        back.plain_text()
            .starts_with("# Not a heading, and\n@block not")
    );

    // New ids keep counting past the reopened ones.
    let mut back = back;
    let part = back.parts()[0].id();
    back.add_chapter(part, "Later").unwrap();
    let ids = back.block_ids();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "no id is reused after reopening");
}

#[test]
fn a_format_1_bundle_still_opens() {
    let path = scratch("legacy");
    std::fs::create_dir_all(path.join("chapters")).unwrap();
    std::fs::write(
        path.join("manifest.txt"),
        "title Old Book\nbody-size 13\ntemplate-opener Chapter\ncaret 0:0\nselection 0:0 0:0\n",
    )
    .unwrap();
    std::fs::write(
        path.join("chapters/0000.txt"),
        "# First\n@template Chapter\n@block Body\nOld words.\n@block Body\nMore.\n",
    )
    .unwrap();
    let book = Book::load_bundle(&path).unwrap();
    assert_eq!(book.title(), "Old Book");
    assert_eq!(book.plain_text(), "Old words.\nMore.");
    // Saving writes format 2.
    let mut book = book;
    book.save_bundle(&path).unwrap();
    assert!(path.join("book.json").is_file());
    assert!(!path.join("manifest.txt").exists());
    assert_eq!(
        Book::load_bundle(&path).unwrap().plain_text(),
        "Old words.\nMore."
    );
}

#[test]
fn an_interrupted_save_leaves_the_previous_bundle_openable() {
    let path = scratch("swap");
    let mut book = rich_book();
    book.save_bundle(&path).unwrap();
    // Simulate a crash between stepping the old bundle aside and renaming
    // the new one into place.
    let old = path.with_file_name(".Book.legebook.old");
    std::fs::rename(&path, &old).unwrap();
    let back = Book::load_bundle(&path).unwrap();
    assert_eq!(back.parts(), book.parts());
}
