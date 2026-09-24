//! Incremental layout from real model edits matches a fresh layout.
//!
//! Every edit here goes through the book model and then
//! [`update_from_book`]; the result must equal [`from_book`] of the same
//! book, page for page, while touching only the pages the edit reaches.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use docwrite_model::{Book, ChapterStart, Direction, Motion, Position, Selection};
use docwrite_typeset::{Document, Face, from_book, update_from_book};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

const PROSE: &str = "The archive kept its letters in bundles tied with string, \
and each bundle carried a date in a hand that changed over the years from \
careful to hurried. Reading them in order was like watching a clerk grow old.";

/// `chapters` chapters of `paragraphs` multi-line paragraphs each.
fn book(chapters: usize, paragraphs: usize, recto: bool) -> Book {
    let mut book = Book::new("Archive");
    if !recto {
        let mut template = book.chapter_templates()[0].clone();
        template.start = ChapterStart::NextPage;
        book.set_chapter_template(template).unwrap();
    }
    let body = |chapter: usize| -> String {
        (0..paragraphs)
            .map(|n| format!("{chapter}.{n}. {PROSE}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    book.insert(&body(0)).unwrap();
    let part = book.parts()[0].id();
    for chapter in 1..chapters {
        book.add_chapter(part, format!("Chapter {}", chapter + 1))
            .unwrap();
        let fresh = *book.block_ids().last().unwrap();
        book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
            .unwrap();
        book.insert(&body(chapter)).unwrap();
    }
    book
}

fn snapshot(document: &Document) -> Vec<String> {
    (1..=document.page_count())
        .map(|page| {
            format!(
                "{page} blank={} folio={:?} head={:?} lines={:?} notes={:?}",
                document.is_blank_page(page),
                document.page_folio(page),
                document.page_running_head(page),
                document.page_texts(page),
                document.page_footnotes(page),
            )
        })
        .collect()
}

fn assert_matches_fresh(document: &Document, book: &Book, what: &str) {
    let fresh = from_book(book, face()).unwrap();
    let got = snapshot(document);
    let want = snapshot(&fresh);
    assert_eq!(got.len(), want.len(), "{what}: page count");
    for (got, want) in got.iter().zip(&want) {
        assert_eq!(got, want, "{what}");
    }
}

fn caret_into(book: &mut Book, block_index: usize, offset: usize) {
    let id = book.block_ids()[block_index];
    book.set_selection(Selection::collapsed(Position::new(id, offset)))
        .unwrap();
}

#[test]
fn typing_mid_book_reshapes_one_paragraph_and_converges() {
    for recto in [false, true] {
        let mut book = book(24, 60, recto);
        let mut document = from_book(&book, face()).unwrap();
        let pages = document.page_count();
        assert!(pages > 150, "a long book: {pages} pages");
        let middle = book.block_ids().len() / 2;
        caret_into(&mut book, middle, 3);
        for ch in ["k", "e", "y", " "] {
            book.insert(ch).unwrap();
            let report = update_from_book(&mut document, &book).unwrap();
            assert_eq!(report.paragraphs_shaped, 1, "recto={recto}");
            assert!(
                report.pages_laid_out.len() <= 3,
                "recto={recto}: typing laid out {:?} of {pages}",
                report.pages_laid_out
            );
        }
        assert_matches_fresh(&document, &book, &format!("typing recto={recto}"));
    }
}

#[test]
fn structural_edits_match_a_fresh_layout() {
    for recto in [false, true] {
        let mut book = book(6, 12, recto);
        let mut document = from_book(&book, face()).unwrap();
        let tag = |what: &str| format!("{what} recto={recto}");

        // Enter splits a paragraph: one new block, one reshaped.
        caret_into(&mut book, 20, 10);
        book.insert("\n").unwrap();
        let report = update_from_book(&mut document, &book).unwrap();
        assert!(report.paragraphs_shaped <= 2, "{}", tag("split"));
        assert_matches_fresh(&document, &book, &tag("split"));

        // Backspace at the start of a block merges it into the previous one.
        caret_into(&mut book, 21, 0);
        book.delete_backward().unwrap();
        update_from_book(&mut document, &book).unwrap();
        assert_matches_fresh(&document, &book, &tag("merge"));

        // A long paste grows a paragraph across several pages.
        caret_into(&mut book, 5, 0);
        book.insert(&PROSE.repeat(6)).unwrap();
        update_from_book(&mut document, &book).unwrap();
        assert_matches_fresh(&document, &book, &tag("paste"));

        // Undo and redo.
        book.undo().unwrap();
        update_from_book(&mut document, &book).unwrap();
        assert_matches_fresh(&document, &book, &tag("undo"));
        book.redo().unwrap();
        update_from_book(&mut document, &book).unwrap();
        assert_matches_fresh(&document, &book, &tag("redo"));

        // Deleting a whole selection spanning blocks.
        let ids = book.block_ids();
        book.set_selection(Selection {
            anchor: Position::new(ids[30], 4),
            focus: Position::new(ids[33], 2),
        })
        .unwrap();
        book.delete_backward().unwrap();
        update_from_book(&mut document, &book).unwrap();
        assert_matches_fresh(&document, &book, &tag("span delete"));

        // Deleting the last paragraph's text entirely.
        let last = book.block_ids().len() - 1;
        let len = book.block_len(book.block_ids()[last]).unwrap();
        caret_into(&mut book, last, len);
        for _ in 0..len {
            book.delete_backward().unwrap();
        }
        book.move_caret(Motion::Char(Direction::Backward)).ok();
        update_from_book(&mut document, &book).unwrap();
        assert_matches_fresh(&document, &book, &tag("empty tail"));

        // Reordering chapters moves blocks without changing their text.
        book.reorder_chapter(4, 1).unwrap();
        let report = update_from_book(&mut document, &book).unwrap();
        assert_eq!(report.paragraphs_shaped, 0, "{}", tag("reorder"));
        assert_matches_fresh(&document, &book, &tag("reorder"));
        book.reorder_chapter(0, 5).unwrap();
        update_from_book(&mut document, &book).unwrap();
        assert_matches_fresh(&document, &book, &tag("reorder to end"));
    }
}

#[test]
fn an_unchanged_book_lays_out_nothing() {
    let book = book(3, 10, true);
    let mut document = from_book(&book, face()).unwrap();
    let report = update_from_book(&mut document, &book).unwrap();
    assert!(report.pages_laid_out.is_empty());
    assert_eq!(report.paragraphs_shaped, 0);
}
