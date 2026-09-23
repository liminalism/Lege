//! Masters, styles, and notes travel from the book into pagination.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use docwrite_model::{Book, NoteKind, Position, Selection};
use docwrite_typeset::{Face, from_book};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn next_recto_inserts_a_blank_verso_with_a_folio_and_running_head() {
    let mut book = Book::new("Essay");
    book.insert("One.").unwrap();
    let part = book.parts()[0].id();
    book.add_chapter(part, "Second").unwrap();
    let fresh = *book.block_ids().last().unwrap();
    book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
        .unwrap();
    book.insert("Two.").unwrap();
    let mut document = from_book(&book, face()).unwrap();
    let mut two_page = None;
    for page in 1..=document.page_count() {
        if document
            .page_texts(page)
            .iter()
            .any(|line| line.contains("Two."))
        {
            two_page = Some(page);
        }
    }
    let two_page = two_page.expect("chapter two is on a page");
    assert_eq!(
        two_page % 2,
        1,
        "NextRecto opens on an odd page, got {two_page}"
    );
    assert!(two_page > 1, "a blank verso sits before chapter two");
    assert!(
        document.is_blank_page(two_page - 1),
        "page {} is the blank verso",
        two_page - 1
    );
    let verso = two_page - 1;
    assert_eq!(
        document.page_folio(verso).as_deref(),
        Some(verso.to_string().as_str())
    );
    assert_eq!(
        document.page_running_head(verso).as_deref(),
        Some("Chapter 1")
    );
    document.edit_page(1, "x").unwrap();
    let mut two_after = None;
    for page in 1..=document.page_count() {
        if document
            .page_texts(page)
            .iter()
            .any(|line| line.contains("Two."))
        {
            two_after = Some(page);
        }
    }
    let two_after = two_after.expect("chapter two remains after the edit");
    assert!(
        document.is_blank_page(two_after - 1),
        "the edit dropped the blank verso before page {two_after}"
    );
    println!(
        "blank verso page {verso} folio {:?} running head {:?}",
        document.page_folio(verso),
        document.page_running_head(verso)
    );
}

#[test]
fn body_style_metrics_reflow_and_match_a_fresh_layout() {
    let mut book = Book::new("Measure");
    book.insert("Opener.\n").unwrap();
    book.insert(&"word ".repeat(40)).unwrap();
    let small = from_book(&book, face()).unwrap();
    let small_em = small.paragraph_em(1).unwrap();
    let mut body = book
        .paragraph_styles()
        .iter()
        .find(|style| style.name == "Body")
        .unwrap()
        .clone();
    body.size_pt = 28.0;
    body.leading_pt = 34.0;
    book.set_paragraph_style(body).unwrap();
    let large = from_book(&book, face()).unwrap();
    let fresh = from_book(&book, face()).unwrap();
    let large_em = large.paragraph_em(1).unwrap();
    assert!(
        (small_em - 12.0).abs() < 0.1,
        "body em before restyle was {small_em}"
    );
    assert!(
        (large_em - 28.0).abs() < 0.1,
        "body em after restyle was {large_em}"
    );
    let line_count = |document: &docwrite_typeset::Document| {
        (1..=document.page_count())
            .map(|page| document.page_texts(page).len())
            .sum::<usize>()
    };
    assert!(
        line_count(&large) > line_count(&small),
        "a larger body must reflow"
    );
    let large_pages: Vec<_> = (1..=large.page_count())
        .map(|page| large.page_texts(page))
        .collect();
    let fresh_pages: Vec<_> = (1..=fresh.page_count())
        .map(|page| fresh.page_texts(page))
        .collect();
    assert_eq!(large_pages, fresh_pages);
    let opener = large.paragraph_em(0).unwrap();
    assert!(
        (opener - 24.0).abs() < 0.1,
        "first paragraph keeps its drop cap, em {opener}"
    );
    println!("body em {small_em} -> {large_em}; first paragraph em {opener}");
}

#[test]
fn footnotes_and_endnotes_come_from_the_book() {
    let mut book = Book::new("Notes");
    book.insert("The reference sentence.").unwrap();
    book.attach_note(NoteKind::Footnote, "Foot text on the reference page.")
        .unwrap();
    let part = book.parts()[0].id();
    book.add_chapter(part, "After").unwrap();
    let fresh = *book.block_ids().last().unwrap();
    book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
        .unwrap();
    book.insert("Chapter body.").unwrap();
    let long = "Endnote body that follows the chapter. ".repeat(3);
    book.attach_note(NoteKind::Endnote, &long).unwrap();
    let document = from_book(&book, face()).unwrap();
    let mut footnote_page = None;
    for page in 1..=document.page_count() {
        if document
            .page_footnotes(page)
            .iter()
            .any(|note| note.contains("Foot text"))
        {
            footnote_page = Some(page);
        }
    }
    let footnote_page = footnote_page.expect("footnote landed");
    assert!(
        document
            .page_texts(footnote_page)
            .iter()
            .any(|line| line.contains("reference")),
        "footnote shares the reference page"
    );
    let mut body_page = None;
    for page in 1..=document.page_count() {
        if document
            .page_texts(page)
            .iter()
            .any(|line| line.contains("Chapter body"))
        {
            body_page = Some(page);
        }
    }
    let body_page = body_page.expect("chapter body is on a page");
    let mut acc = String::new();
    let mut end_page = None;
    for page in 1..=document.page_count() {
        for note in document.page_footnotes(page) {
            let next = format!("{acc}{note}");
            if long.starts_with(&next) {
                if end_page.is_none() {
                    end_page = Some(page);
                }
                acc = next;
            }
        }
    }
    assert_eq!(acc, long, "endnote pieces did not reassemble");
    assert_eq!(end_page, Some(body_page), "endnote did not start on the reference page");
    println!("footnote on page {footnote_page}; endnote starts on reference page {body_page} and reassembles");
}

#[test]
fn a_verso_page_insets_the_outer_margin() {
    let mut book = Book::new("Facing");
    let mut right = book
        .page_masters()
        .iter()
        .find(|master| master.name == "Right Body")
        .unwrap()
        .clone();
    right.height_pt = 64.0;
    right.margin_top = 4.0;
    right.margin_bottom = 4.0;
    right.margin_inner = 54.0;
    right.margin_outer = 20.0;
    right.facing = true;
    book.set_page_master(right).unwrap();
    book.insert(&"word ".repeat(80)).unwrap();
    let document = from_book(&book, face()).unwrap();
    assert!(document.page_count() >= 2, "pages {}", document.page_count());
    let recto = document.page_content_inset(1);
    let verso = document.page_content_inset(2);
    assert!((recto - 54.0).abs() < 0.1, "recto inset {recto}");
    assert!((verso - 20.0).abs() < 0.1, "verso inset {verso}");
    assert_ne!(recto, verso);
    println!("recto inset {recto} verso inset {verso}");
}

#[test]
fn a_second_note_survives_a_split_and_an_edit() {
    let mut book = Book::new("Notes");
    let mut right = book
        .page_masters()
        .iter()
        .find(|master| master.name == "Right Body")
        .unwrap()
        .clone();
    right.height_pt = 80.0;
    right.margin_top = 8.0;
    right.margin_bottom = 8.0;
    right.facing = true;
    book.set_page_master(right).unwrap();
    let first = "N".repeat(49);
    book.insert("First reference.").unwrap();
    book.attach_note(NoteKind::Footnote, &first).unwrap();
    book.insert("\nSecond reference.").unwrap();
    book.attach_note(NoteKind::Footnote, "Second note body.").unwrap();
    let mut document = from_book(&book, face()).unwrap();
    let joined = note_text(&document);
    assert!(joined.contains(&first), "first note missing from {joined:?}");
    assert!(joined.contains("Second note body."), "second note missing from {joined:?}");
    document.edit_page(1, "x").unwrap();
    let after = note_text(&document);
    assert!(after.contains(&first), "edit dropped the split note: {after:?}");
    assert!(after.contains("Second note body."), "edit dropped the second note: {after:?}");
    println!("both notes survive the split and the edit");
}

#[test]
fn a_pending_footnote_still_blanks_the_verso_before_the_next_recto() {
    let mut book = Book::new("Carry");
    let mut right = book
        .page_masters()
        .iter()
        .find(|master| master.name == "Right Body")
        .unwrap()
        .clone();
    right.height_pt = 80.0;
    right.margin_top = 8.0;
    right.margin_bottom = 8.0;
    right.facing = true;
    book.set_page_master(right).unwrap();
    let first = "N".repeat(49);
    book.insert("One.").unwrap();
    book.attach_note(NoteKind::Footnote, &first).unwrap();
    book.insert("\nSecond reference.").unwrap();
    book.attach_note(NoteKind::Footnote, "Second note body.").unwrap();
    let part = book.parts()[0].id();
    book.add_chapter(part, "Second").unwrap();
    let fresh = *book.block_ids().last().unwrap();
    book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
        .unwrap();
    book.insert("Two.").unwrap();
    let mut document = from_book(&book, face()).unwrap();
    assert_note_recto(&document, &first);
    document.edit_page(1, "x").unwrap();
    assert_note_recto(&document, &first);
    let two = page_with(&document, "Two.");
    document.edit_page(two, "y").unwrap();
    assert_note_recto(&document, &first);
    println!(
        "pending note keeps page 2 blank; Two. opens on page {}",
        page_with(&document, "Two.")
    );
}

fn page_with(document: &docwrite_typeset::Document, needle: &str) -> u32 {
    (1..=document.page_count())
        .find(|page| {
            document
                .page_texts(*page)
                .iter()
                .any(|line| line.contains(needle))
        })
        .unwrap_or(0)
}

fn assert_note_recto(document: &docwrite_typeset::Document, first: &str) {
    assert!(
        document.is_blank_page(2),
        "page 2 texts {:?} notes {:?} blank {}",
        document.page_texts(2),
        document.page_footnotes(2),
        document.is_blank_page(2)
    );
    assert!(
        document.page_texts(2).is_empty(),
        "verso held {:?}",
        document.page_texts(2)
    );
    assert!(
        document.page_footnotes(2).is_empty(),
        "a blank verso placed notes {:?}",
        document.page_footnotes(2)
    );
    let two = page_with(document, "Two.");
    assert_ne!(two, 0, "chapter two is missing");
    assert_eq!(two % 2, 1, "Two. opened on even page {two}");
    assert!(two > 2, "Two. is on page {two}");
    let joined = note_text(document);
    assert!(
        joined.contains(first),
        "footnote did not reassemble: {joined:?}"
    );
    assert!(
        joined.contains("Second note body."),
        "second note missing from {joined:?}"
    );
}

fn note_text(document: &docwrite_typeset::Document) -> String {
    let mut joined = String::new();
    for page in 1..=document.page_count() {
        for note in document.page_footnotes(page) {
            joined.push_str(&note);
        }
    }
    joined
}
