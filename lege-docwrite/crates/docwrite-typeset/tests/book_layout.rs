//! Masters, styles, and notes travel from the book into pagination.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    // The body paragraph, found by its block id: the chapter title set from
    // the chapter's metadata comes before it in the layout.
    let body_block = book.block_ids()[1].raw();
    let small = from_book(&book, face()).unwrap();
    let small_em = small
        .paragraph_em(small.paragraph_of(body_block).unwrap())
        .unwrap();
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
    let large_em = large
        .paragraph_em(large.paragraph_of(body_block).unwrap())
        .unwrap();
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
    let opener_block = book.block_ids()[0].raw();
    let opener = large
        .paragraph_em(large.paragraph_of(opener_block).unwrap())
        .unwrap();
    // A two-line drop cap on the 12/16 First Paragraph style: its cap
    // height spans one leading plus the body's cap height.
    let ratio = face().cap_height_ratio();
    let expected = (16.0 + 12.0 * ratio) / ratio;
    assert!(
        (opener - expected).abs() < 0.1,
        "first paragraph keeps its two-line drop cap, em {opener} (expected {expected})"
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
    // The endnote is not at the foot of its page: it is in the Notes section
    // after the last chapter, numbered as its reference mark is.
    let body_page = page_with(&document, "Chapter body");
    assert!(
        !document
            .page_footnotes(body_page)
            .iter()
            .any(|note| note.contains("Endnote")),
        "an endnote is not a footnote"
    );
    let notes_page = page_with(&document, "Notes");
    assert!(notes_page > body_page, "Notes follows the last chapter");
    let texts: Vec<String> = (notes_page..=document.page_count())
        .flat_map(|page| document.page_texts(page))
        .collect();
    let joined = texts.join(" ");
    assert!(joined.starts_with("Notes 1. Endnote body"), "{joined}");
    assert_eq!(
        joined.split_whitespace().skip(2).collect::<Vec<_>>(),
        long.split_whitespace().collect::<Vec<_>>(),
        "the endnote is set whole"
    );
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
    assert!(
        document.page_count() >= 2,
        "pages {}",
        document.page_count()
    );
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
    book.attach_note(NoteKind::Footnote, "Second note body.")
        .unwrap();
    let mut document = from_book(&book, face()).unwrap();
    let joined = note_text(&document);
    assert!(
        joined.contains(&first),
        "first note missing from {joined:?}"
    );
    assert!(
        joined.contains("Second note body."),
        "second note missing from {joined:?}"
    );
    document.edit_page(1, "x").unwrap();
    let after = note_text(&document);
    assert!(
        after.contains(&first),
        "edit dropped the split note: {after:?}"
    );
    assert!(
        after.contains("Second note body."),
        "edit dropped the second note: {after:?}"
    );
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
    // Untitled chapters: this is about note flow, and an opener title would
    // take one of these tiny pages' lines.
    let opening = book.parts()[0].chapters()[0].id();
    book.rename_chapter(opening, "").unwrap();
    // A footnote far longer than the small page's foot: most of it is still
    // pending when chapter two wants to open on the next recto.
    let first = "Carried footnote words run on. ".repeat(14);
    book.insert("One.").unwrap();
    book.attach_note(NoteKind::Footnote, first.trim_end())
        .unwrap();
    let part = book.parts()[0].id();
    book.add_chapter(part, "").unwrap();
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
        joined.starts_with("1\u{2002}Carried footnote"),
        "the note is numbered: {joined:?}"
    );
    assert_eq!(
        joined.split_whitespace().skip(1).collect::<Vec<_>>(),
        first.split_whitespace().collect::<Vec<_>>(),
        "footnote did not reassemble"
    );
    assert!(
        !document.page_footnotes(1).is_empty() && !document.page_footnotes(3).is_empty(),
        "the note starts on its reference page and continues after the blank verso"
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

#[test]
fn next_page_chapters_start_a_page_and_templates_bring_their_masters() {
    use docwrite_model::ChapterStart;
    let mut book = Book::new("Rules");
    let mut template = book.chapter_templates()[0].clone();
    template.start = ChapterStart::NextPage;
    book.set_chapter_template(template).unwrap();
    book.insert("A short first chapter.").unwrap();
    let part = book.parts()[0].id();
    book.add_chapter(part, "Second").unwrap();
    let fresh = *book.block_ids().last().unwrap();
    book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
        .unwrap();
    book.insert("The second chapter.").unwrap();

    let document = from_book(&book, face()).unwrap();
    let second = page_with(&document, "Second");
    assert_eq!(second, 2, "a NextPage chapter starts the very next page");
    assert!(!document.is_blank_page(1));

    // An appendix template on its own master: wider inner margin, no heads.
    let mut master = book.page_masters()[0].clone();
    master.name = "Appendix Body".into();
    master.margin_inner = 100.0;
    master.margin_outer = 60.0;
    master.running_head = false;
    book.add_page_master(master).unwrap();
    let mut appendix = book.chapter_templates()[0].clone();
    appendix.name = "Appendix".into();
    appendix.master = "Appendix Body".into();
    book.add_chapter_template(appendix).unwrap();
    let chapter = book.parts()[0].chapters()[1].id();
    book.apply_template(chapter, "Appendix").unwrap();
    let fresh_block = *book.block_ids().last().unwrap();
    book.set_selection(Selection::collapsed(Position::new(fresh_block, 0)))
        .unwrap();
    book.insert(&"Appendix words run on and on. ".repeat(120))
        .unwrap();

    let document = from_book(&book, face()).unwrap();
    assert_eq!(
        document.page_content_inset(1),
        54.0,
        "chapter one keeps its master"
    );
    // Facing pages: the inner margin is on the left of a recto (odd page)
    // and on the right of a verso, where the outer margin is on the left.
    let inset = |page: u32| if page % 2 == 1 { 100.0 } else { 60.0 };
    let appendix_page = page_with(&document, "Second");
    assert_eq!(
        document.page_content_inset(appendix_page),
        inset(appendix_page)
    );
    let later = appendix_page + 2;
    assert!(later <= document.page_count(), "the appendix runs on");
    assert_eq!(document.page_content_inset(later), inset(later));
    assert_eq!(
        document.page_running_head(later),
        None,
        "its master has no running head"
    );
    assert!(document.page_folio(later).is_some(), "but keeps folios");
    let widest = |page: u32| {
        document
            .page_painted_lines(page)
            .iter()
            .map(|line| line.glyphs.iter().map(|glyph| glyph.x_advance).sum::<f32>())
            .fold(0.0_f32, f32::max)
    };
    assert!(
        widest(later) <= 432.0 - 100.0 - 60.0 + 0.5,
        "appendix lines fit its narrower measure: {}",
        widest(later)
    );
}
