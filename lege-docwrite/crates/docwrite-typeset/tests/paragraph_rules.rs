//! Widow, orphan, keep-with-next and keep-lines, plus the prose features
//! around them: hyphenation, drop caps, quotes, epigraphs, images, matter,
//! small caps and old-style figures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use docwrite_typeset::{Document, Face, Geometry, Paragraph, ParagraphStyle, features_for};

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
            style: ParagraphStyle {
                name: "Chapter Title".into(),
                ..style.clone()
            },
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
            style: ParagraphStyle {
                keep_with_next: true,
                keep_lines: false,
                name: "Subhead".into(),
                ..style.clone()
            },
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
            style: ParagraphStyle {
                name: "Epigraph".into(),
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 6,
            text: "A quoted passage.".into(),
            style: ParagraphStyle {
                name: "Block Quote".into(),
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 7,
            text: "plate".into(),
            style: ParagraphStyle {
                name: "Image".into(),
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 8,
            text: "Caption under the plate.".into(),
            style: ParagraphStyle {
                name: "Caption".into(),
                keep_with_next: false,
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 9,
            text: "Drop".into(),
            style: ParagraphStyle {
                drop_cap_lines: 2,
                name: "First Paragraph".into(),
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        },
        Paragraph {
            id: 10,
            text: "Back matter".into(),
            style: ParagraphStyle {
                name: "Chapter Title".into(),
                small_caps: true,
                oldstyle_figures: true,
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        },
    ];
    let document = Document::new(face(), geometry, paragraphs.clone()).unwrap();
    assert!(document.page_count() > 1);
    let violations = document.rule_violations();
    assert!(violations.is_empty(), "{violations:?}");
    println!("keep/widow violations: {}", violations.len());
    let pages: Vec<String> = (1..=document.page_count())
        .flat_map(|page| document.page_texts(page))
        .collect();
    let has = |needle: &str| pages.iter().any(|line| line.contains(needle));
    assert!(has("Front matter") && has("epigraph") && has("quoted passage"));
    assert!(has("plate") && has("Caption") && has("Back matter"));
    println!("front matter: {}", has("Front matter"));
    println!("epigraph: {}", has("epigraph"));
    println!("block quote: {}", has("quoted passage"));
    println!("image: {}", has("plate"));
    println!("caption: {}", has("Caption"));
    println!("back matter: {}", has("Back matter"));
    assert!(!document.opens_with_widow());
    assert!(!document.ends_with_orphan());
    let em = document.drop_cap_em().expect("a drop cap was requested");
    assert!(
        em > geometry.font_size,
        "drop cap em {em} should exceed body {}",
        geometry.font_size
    );
    println!("drop cap em {em} > body {}", geometry.font_size);

    let plain = face().shape("Hello 123", 16.0, &[]).unwrap();
    let featured = face()
        .shape("Hello 123", 16.0, &features_for(true, true))
        .unwrap();
    let plain_ids: Vec<_> = plain.iter().map(|glyph| glyph.id).collect();
    let featured_ids: Vec<_> = featured.iter().map(|glyph| glyph.id).collect();
    assert_ne!(plain_ids, featured_ids);
    println!("opentype smcp+onum glyph ids {plain_ids:?} -> {featured_ids:?}");
    let _ = paragraphs;
}

fn layout(text: &str, geometry: Geometry, control: bool) -> Document {
    let style = ParagraphStyle {
        hyphenate: false,
        widow_orphan: control,
        keep_lines: control,
        keep_with_next: false,
        ..ParagraphStyle::default()
    };
    Document::new(
        face(),
        geometry,
        vec![Paragraph {
            id: 1,
            text: text.into(),
            style,
            note: None,
            note_is_endnote: false,
        }],
    )
    .unwrap()
}

#[test]
fn widow_and_orphan_are_refused_when_the_style_asks() {
    let geometry = Geometry {
        page_width: 220.0,
        page_height: 80.0,
        margin_top: 8.0,
        margin_bottom: 8.0,
        margin_inner: 12.0,
        margin_outer: 12.0,
        font_size: 12.0,
        leading: 16.0,
    };
    let mut found = false;
    for words in 4..80 {
        let text = "measure ".repeat(words);
        let loose = layout(&text, geometry, false);
        if !loose.opens_with_widow() && !loose.ends_with_orphan() {
            continue;
        }
        let held = layout(&text, geometry, true);
        assert!(
            !held.opens_with_widow(),
            "widow survived on {words} words: {:?}",
            (1..=held.page_count())
                .map(|page| held.page_texts(page))
                .collect::<Vec<_>>()
        );
        assert!(!held.ends_with_orphan(), "orphan survived on {words} words");
        assert!(
            held.rule_violations().is_empty(),
            "{:?}",
            held.rule_violations()
        );
        println!(
            "keep/widow violations after control: {} (loose layout had a widow or orphan at {words} words)",
            held.rule_violations().len()
        );
        found = true;
        break;
    }
    assert!(
        found,
        "the corpus never produced a widow or an orphan to refuse"
    );
}

#[test]
fn a_long_word_breaks_with_a_visible_hyphen() {
    let geometry = Geometry {
        page_width: 70.0,
        page_height: 200.0,
        margin_top: 4.0,
        margin_bottom: 4.0,
        margin_inner: 4.0,
        margin_outer: 4.0,
        font_size: 16.0,
        leading: 20.0,
    };
    let document = Document::new(
        face(),
        geometry,
        vec![Paragraph {
            id: 1,
            text: "extraordinary".into(),
            style: ParagraphStyle {
                hyphenate: true,
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        }],
    )
    .unwrap();
    let lines: Vec<String> = (1..=document.page_count())
        .flat_map(|page| document.page_texts(page))
        .collect();
    assert!(
        lines.iter().any(|line| line.contains('-')),
        "expected a hyphen in {lines:?}"
    );
    println!("hyphenated lines: {lines:?}");
}
