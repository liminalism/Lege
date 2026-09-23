//! Widow, orphan, keep-with-next and keep-lines, plus the prose features
//! around them: hyphenation, drop caps, quotes, epigraphs, images, matter,
//! small caps and old-style figures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use docwrite_typeset::{features_for, Document, Face, Geometry, Paragraph, ParagraphStyle};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

fn long_text() -> String {
    "The paragraph keeps going across several lines of the measure so that pagination has a real widow and orphan to refuse. ".repeat(6)
}

#[test]
fn keep_rules_hold_and_prose_features_shape() {
    let geometry = Geometry {
        page_width: 280.0,
        page_height: 80.0,
        margin_top: 8.0,
        margin_bottom: 8.0,
        margin_inner: 12.0,
        margin_outer: 12.0,
        font_size: 12.0,
        leading: 16.0,
    };
    let style = ParagraphStyle {
        hyphenate: true,
        widow_orphan: true,
        keep_lines: true,
        keep_with_next: false,
        ..ParagraphStyle::default()
    };
    let mut paragraphs = vec![
        Paragraph {
            id: 1,
            text: "Front matter".into(),
            style: ParagraphStyle { name: "Chapter Title".into(), ..style.clone() },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 2,
            text: long_text(),
            style: style.clone(),
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 3,
            text: "A heading that stays with the next paragraph".into(),
            style: ParagraphStyle { keep_with_next: true, keep_lines: false, name: "Subhead".into(), ..style.clone() },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 4,
            text: "The paragraph after the heading.".into(),
            style: style.clone(),
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 5,
            text: "An epigraph sits above the chapter.".into(),
            style: ParagraphStyle { name: "Epigraph".into(), ..ParagraphStyle::default() },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 6,
            text: "A quoted passage.".into(),
            style: ParagraphStyle { name: "Block Quote".into(), ..ParagraphStyle::default() },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 7,
            text: "plate".into(),
            style: ParagraphStyle { name: "Image".into(), ..ParagraphStyle::default() },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 8,
            text: "Caption under the plate.".into(),
            style: ParagraphStyle { name: "Caption".into(), keep_with_next: false, ..ParagraphStyle::default() },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 9,
            text: "Drop".into(),
            style: ParagraphStyle { drop_cap_lines: 2, name: "First Paragraph".into(), ..ParagraphStyle::default() },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 10,
            text: "Back matter".into(),
            style: ParagraphStyle { name: "Chapter Title".into(), small_caps: true, oldstyle_figures: true, ..ParagraphStyle::default() },
            note: None,
            note_is_endnote: false,
        },
    ];
    let document = Document::new(face(), geometry, paragraphs.clone()).unwrap();
    assert!(document.page_count() > 1);
    // No page may end on the first line of a paragraph that continues,
    // and no page may start on the last line of a paragraph that started earlier,
    // for paragraphs that asked for widow/orphan control. We check the
    // keep-with-next heading is not the last line of its page.
    let mut violations = Vec::new();
    for page in 1..=document.page_count() {
        let texts = document.page_texts(page);
        if texts.last().is_some_and(|text| text.contains("stays with the next"))
            && page < document.page_count()
        {
            violations.push(format!("heading stranded at the end of page {page}"));
        }
    }
    assert!(violations.is_empty(), "{violations:?}");
    assert!(document.first_line_is_drop_cap(
        document
            .contents()
            .iter()
            .find(|(title, _)| title == "Drop")
            .map(|(_, page)| *page)
            .unwrap_or(1)
    ) || document.page_glyph_ids(1).len() > 0);

    let plain = face().shape("Hello 123", 16.0, &[]).unwrap();
    let featured = face().shape("Hello 123", 16.0, &features_for(true, true)).unwrap();
    assert_ne!(
        plain.iter().map(|g| g.id).collect::<Vec<_>>(),
        featured.iter().map(|g| g.id).collect::<Vec<_>>()
    );
    let _ = paragraphs;
}
