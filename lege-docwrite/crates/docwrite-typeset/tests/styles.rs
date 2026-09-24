//! Paragraph styles set their lines: justified, indented, centered, and in
//! the family's bold and italic faces.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use docwrite_model::{BlockKind, Book, Mark, Position, Selection};
use docwrite_typeset::{Face, FaceStyle, PaintedLine, from_book_with};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

/// Four faces from one file: the test checks which face each glyph asks for.
fn family() -> Vec<Face> {
    (0..4).map(|_| face()).collect()
}

const PROSE: &str = "The archive kept its letters in bundles tied with string, and each \
bundle carried a date in a hand that changed over the years from careful to hurried.";

fn width(line: &PaintedLine) -> f32 {
    line.glyphs.iter().map(|glyph| glyph.x_advance).sum()
}

fn lines_of(book: &Book, needle: &str) -> Vec<PaintedLine> {
    let document = from_book_with(book, family()).unwrap();
    let index = document
        .paragraphs()
        .iter()
        .position(|paragraph| paragraph.text.contains(needle))
        .unwrap();
    (1..=document.page_count())
        .flat_map(|page| document.page_painted_lines(page))
        .filter(|line| line.paragraph == index)
        .collect()
}

fn book_with(kind: Option<BlockKind>) -> Book {
    let mut book = Book::new("Styles");
    book.insert(&format!("Opening words.\n{PROSE} {PROSE}"))
        .unwrap();
    if let Some(kind) = kind {
        let second = book.block_ids()[1];
        book.set_kind(second, kind).unwrap();
    }
    book
}

#[test]
fn body_text_is_justified_with_a_ragged_last_line() {
    let lines = lines_of(&book_with(None), "The archive");
    assert!(lines.len() >= 3);
    let measure = 432.0 - 54.0 - 36.0;
    for line in &lines[..lines.len() - 1] {
        assert!(
            (line.indent + width(line) - measure).abs() < 0.5,
            "a justified line fills the measure: {} + {}",
            line.indent,
            width(line)
        );
    }
    // The last line's spaces keep their natural width: it is not stretched.
    let space = face().shape(" ", 12.0, &[]).unwrap()[0].x_advance;
    let last = lines.last().unwrap();
    let stretched = |line: &PaintedLine| {
        line.glyphs.iter().any(|glyph| {
            glyph.x_advance > space + 0.01
                && glyph.id == face().shape(" ", 12.0, &[]).unwrap()[0].id
        })
    };
    assert!(!stretched(last), "the last line is set ragged");
    assert!(stretched(&lines[0]), "earlier lines open their spaces");
}

#[test]
fn block_quotes_indent_both_sides_and_captions_center() {
    let quote = lines_of(&book_with(Some(BlockKind::BlockQuote)), "The archive");
    let measure = 432.0 - 54.0 - 36.0;
    for line in &quote[..quote.len() - 1] {
        assert_eq!(line.indent, 24.0);
        assert!((line.indent + width(line) - (measure - 24.0)).abs() < 0.5);
    }
    let mut book = Book::new("Caption");
    book.insert("Opening words.\nFig. 1, a bundle.").unwrap();
    let second = book.block_ids()[1];
    book.set_kind(second, BlockKind::Caption).unwrap();
    let caption = lines_of(&book, "Fig. 1");
    let line = &caption[0];
    let left_gap = line.indent;
    let right_gap = measure - line.indent - width(line);
    assert!(
        (left_gap - right_gap).abs() < 1.0,
        "centered: {left_gap} vs {right_gap}"
    );
}

#[test]
fn bold_and_italic_runs_and_styles_choose_their_faces() {
    let mut book = book_with(None);
    let second = book.block_ids()[1];
    book.set_selection(Selection {
        anchor: Position::new(second, 4),
        focus: Position::new(second, 11),
    })
    .unwrap();
    book.toggle_mark(Mark::Bold).unwrap();
    let lines = lines_of(&book, "The archive");
    let faces: Vec<u8> = lines[0].glyphs.iter().map(|glyph| glyph.face).collect();
    assert!(
        faces.contains(&(FaceStyle::Bold as u8)),
        "the bold run: {faces:?}"
    );
    assert!(faces.contains(&(FaceStyle::Regular as u8)));

    // An epigraph is italic; an italic run inside it turns roman.
    let mut book = book_with(Some(BlockKind::Epigraph));
    let second = book.block_ids()[1];
    book.set_selection(Selection {
        anchor: Position::new(second, 0),
        focus: Position::new(second, 3),
    })
    .unwrap();
    book.toggle_mark(Mark::Italic).unwrap();
    let lines = lines_of(&book, "The archive");
    let first = &lines[0].glyphs;
    assert_eq!(
        first[0].face,
        FaceStyle::Regular as u8,
        "emphasis in italic is roman"
    );
    assert_eq!(
        first[5].face,
        FaceStyle::Italic as u8,
        "the epigraph itself is italic"
    );
    assert_eq!(lines[0].indent, 96.0, "epigraphs hang in from the left");
}
