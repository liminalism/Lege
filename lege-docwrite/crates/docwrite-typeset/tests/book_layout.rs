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
    let document = from_book(&book, face()).unwrap();
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
