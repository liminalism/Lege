//! Footnotes are measured into the page: set at footnote size on the page of
//! their reference, with their height taken from the text block, split by
//! lines when they do not fit, and kept there through repagination.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use docwrite_typeset::{Document, Face, Geometry, Paragraph, ParagraphStyle};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

/// A small page: a 160pt text block holds ten 16pt body lines.
fn geometry() -> Geometry {
    Geometry {
        page_width: 300.0,
        page_height: 200.0,
        margin_top: 20.0,
        margin_bottom: 20.0,
        margin_inner: 30.0,
        margin_outer: 30.0,
        font_size: 12.0,
        leading: 16.0,
    }
}

fn paragraphs(note_on: u64, note: &str, count: u64) -> Vec<Paragraph> {
    (0..count)
        .map(|index| Paragraph {
            id: index + 1,
            text: format!("Paragraph {index} of the chapter."),
            style: ParagraphStyle::default(),
            note: (index == note_on).then(|| note.to_string()),
            ..Paragraph::default()
        })
        .collect()
}

fn page_of(document: &Document, needle: &str) -> u32 {
    (1..=document.page_count())
        .find(|page| {
            document
                .page_texts(*page)
                .iter()
                .any(|line| line.contains(needle))
        })
        .unwrap_or(0)
}

#[test]
fn a_footnote_takes_its_height_from_the_reference_page() {
    let plain = Document::new(face(), geometry(), paragraphs(99, "", 20)).unwrap();
    let noted = Document::new(face(), geometry(), paragraphs(3, "A short footnote.", 20)).unwrap();
    let reference = page_of(&noted, "Paragraph 3 ");
    assert_eq!(
        noted.page_footnotes(reference),
        vec!["A short footnote.".to_string()]
    );
    assert!(
        noted.page_texts(reference).len() < plain.page_texts(reference).len(),
        "the note's lines come out of the text block: {} vs {} body lines",
        noted.page_texts(reference).len(),
        plain.page_texts(reference).len()
    );
    // Its lines sit at the foot, below the last body line, inside the margin.
    let lines = noted.page_footnote_lines(reference);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].em < 12.0,
        "footnotes are set smaller than the body"
    );
    assert!(lines[0].baseline <= 200.0 - 20.0);
    let (rule_y, _) = noted.page_footnote_rule(reference).unwrap();
    assert!(rule_y < lines[0].baseline);
}

#[test]
fn a_long_footnote_splits_by_lines_and_survives_an_edit() {
    let long = "The note goes on across the page boundary because it is much longer than \
        the space left at the foot of its reference page. "
        .repeat(8);
    let mut document =
        Document::new(face(), geometry(), paragraphs(8, long.trim_end(), 20)).unwrap();
    let reference = page_of(&document, "Paragraph 8 ");
    let first = document.page_footnotes(reference);
    assert!(!first.is_empty(), "the note starts on its reference page");
    let mut joined = Vec::new();
    let mut pages = 0;
    for page in 1..=document.page_count() {
        let lines = document.page_footnotes(page);
        if !lines.is_empty() {
            pages += 1;
        }
        joined.extend(lines);
    }
    assert!(pages >= 2, "a long note continues on the next page");
    assert_eq!(
        joined.join("").split_whitespace().collect::<Vec<_>>(),
        long.split_whitespace().collect::<Vec<_>>(),
        "every word of the note is set once, in order"
    );
    let before = document.page_footnotes(reference);
    let _ = document.edit_page(1, "x").unwrap();
    let reference_after = page_of(&document, "Paragraph 8 ");
    assert_eq!(
        document.page_footnotes(reference_after).first(),
        before.first(),
        "the note starts on its reference page after an earlier edit"
    );
}
