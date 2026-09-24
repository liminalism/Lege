//! A generated table of contents lists each chapter with the page it opens
//! on, flush right, and keeps the numbers right as the book changes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use docwrite_model::{Book, Position, Selection};
use docwrite_typeset::{Document, Face, from_book, update_from_book};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

const PROSE: &str = "The archive kept its letters in bundles tied with string, and each \
bundle carried a date in a hand that changed over the years from careful to hurried. ";

fn book() -> Book {
    let mut book = Book::new("Contents");
    book.insert(&PROSE.repeat(12)).unwrap();
    let part = book.parts()[0].id();
    for title in ["The Clerk", "Bundles"] {
        book.add_chapter(part, title).unwrap();
        let fresh = *book.block_ids().last().unwrap();
        book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
            .unwrap();
        book.insert(&PROSE.repeat(12)).unwrap();
    }
    book.set_contents(true);
    book
}

/// (title, page) as the contents page prints them.
fn entries(document: &Document) -> Vec<(String, u32)> {
    document
        .page_texts(1)
        .iter()
        .skip(1)
        .filter_map(|line| {
            let (title, page) = line.rsplit_once('\t')?;
            Some((title.to_string(), page.trim().parse().ok()?))
        })
        .collect()
}

fn opens_on(document: &Document, title: &str) -> u32 {
    (2..=document.page_count())
        .find(|page| {
            document
                .page_texts(*page)
                .first()
                .is_some_and(|line| line == title)
        })
        .unwrap_or_else(|| panic!("{title} opens no page"))
}

#[test]
fn the_contents_page_lists_chapters_at_their_pages() {
    let book = book();
    let document = from_book(&book, face()).unwrap();
    assert_eq!(document.page_texts(1)[0], "Contents");
    let listed = entries(&document);
    assert_eq!(listed.len(), 3, "{:?}", document.page_texts(1));
    for (title, page) in &listed {
        assert_eq!(*page, opens_on(&document, title), "{title}");
    }
    // Numbers sit flush right.
    let lines = document.page_painted_lines(1);
    let measure = 432.0 - 54.0 - 36.0;
    for line in &lines[1..] {
        let end = line.indent + line.glyphs.iter().map(|glyph| glyph.x_advance).sum::<f32>();
        assert!((end - measure).abs() < 0.5, "entry ends at {end}");
    }
}

#[test]
fn numbers_follow_the_chapters_through_edits() {
    let mut book = book();
    let mut document = from_book(&book, face()).unwrap();
    let before = entries(&document);
    // A long insertion early in the book pushes the later chapters on.
    let first = book.block_ids()[0];
    book.set_selection(Selection::collapsed(Position::new(first, 0)))
        .unwrap();
    book.insert(&PROSE.repeat(40)).unwrap();
    update_from_book(&mut document, &book).unwrap();
    let after = entries(&document);
    assert!(after[2].1 > before[2].1, "{before:?} -> {after:?}");
    for (title, page) in &after {
        assert_eq!(*page, opens_on(&document, title), "{title}");
    }
    let fresh = from_book(&book, face()).unwrap();
    assert_eq!(entries(&fresh), after, "incremental and fresh agree");
    book.set_contents(false);
    update_from_book(&mut document, &book).unwrap();
    assert_ne!(
        document.page_texts(1).first().map(String::as_str),
        Some("Contents")
    );
}

#[test]
fn bibliography_and_index_close_the_book() {
    use docwrite_model::BibliographyEntry;
    use docwrite_typeset::FaceStyle;
    let mut book = book();
    book.set_contents(false);
    for (key, author, title, issued) in [
        ("b", "Weber, Max", "Economy and Society", "1922"),
        ("a", "Arendt, Hannah", "The Human Condition", "1958"),
    ] {
        book.add_bibliography(BibliographyEntry {
            key: key.into(),
            kind: "book".into(),
            title: title.into(),
            author: author.into(),
            issued: issued.into(),
        });
    }
    let ids = book.block_ids();
    book.add_index_term("archive", ids[0]);
    book.add_index_term("clerk", ids[2]);
    book.add_index_term("Archive", ids[2]);
    let document =
        docwrite_typeset::from_book_with(&book, vec![face(), face(), face(), face()]).unwrap();
    let find = |heading: &str| {
        (1..=document.page_count())
            .find(|page| {
                document
                    .page_texts(*page)
                    .first()
                    .is_some_and(|line| line == heading)
            })
            .unwrap_or_else(|| panic!("no {heading} page"))
    };
    let bibliography = find("Bibliography");
    let texts = document.page_texts(bibliography);
    assert!(
        texts[1].starts_with("Arendt, Hannah. The Human Condition. 1958."),
        "{texts:?}"
    );
    assert!(texts.iter().any(|line| line.starts_with("Weber, Max.")));
    let title_glyph = document.page_painted_lines(bibliography)[1]
        .glyphs
        .iter()
        .find(|glyph| glyph.cluster as usize == "Arendt, Hannah. ".len())
        .unwrap()
        .face;
    assert_eq!(title_glyph, FaceStyle::Italic as u8, "titles are italic");

    let index = find("Index");
    assert!(index > bibliography, "the index comes last");
    let entries = document.page_texts(index);
    let page_of = |block: usize| {
        (1..=document.page_count())
            .find(|page| {
                let paragraph = document.paragraph_of(ids[block].raw()).unwrap();
                document.page_of(paragraph, 0) == Some(*page)
            })
            .unwrap()
    };
    assert_eq!(
        entries[1],
        format!("archive\t{}, {}", page_of(0), page_of(2)),
        "terms merge regardless of case, pages in order"
    );
    assert_eq!(entries[2], format!("clerk\t{}", page_of(2)));
}
