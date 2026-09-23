//! Reopen restores the manuscript, styles, and caret. One template edit
//! changes every chapter that uses it. Drag reorder changes book order.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;

use docwrite_model::{Book, SourceNote};

#[test]
fn ten_chapters_follow_one_template_and_reorder_and_reopen() {
    let mut book = Book::new("Essay");
    let part = book.parts()[0].id();
    book.insert("Opening paragraph.").unwrap();
    for index in 2..=10 {
        book.add_chapter(part, format!("Chapter {index}")).unwrap();
        book.insert("Body.").unwrap();
    }
    assert_eq!(book.chapter_openers().len(), 10);
    assert!(book.chapter_openers().iter().all(|opener| opener.starts_with("Chapter ")));

    let mut template = book.chapter_templates()[0].clone();
    template.opener = "Lesson".into();
    book.set_chapter_template(template).unwrap();
    assert!(
        book.chapter_openers().iter().all(|opener| opener.starts_with("Lesson ")),
        "{:?}",
        book.chapter_openers()
    );

    let before = book.parts()[0].chapters()[0].title().to_string();
    book.reorder_chapter(0, 9).unwrap();
    assert_ne!(book.parts()[0].chapters()[0].title(), before);

    let mut body = book
        .paragraph_styles()
        .iter()
        .find(|style| style.name == "Body")
        .unwrap()
        .clone();
    body.size_pt = 14.0;
    book.set_paragraph_style(body).unwrap();

    book.remember_position();
    let dir = env::temp_dir().join(format!("legebook-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    book.save_bundle(&dir).unwrap();
    let opened = Book::load_bundle(&dir).unwrap();
    assert_eq!(opened.title(), "Essay");
    assert!(opened.plain_text().contains("Opening paragraph."));
    assert_eq!(
        opened.paragraph_styles().iter().find(|s| s.name == "Body").unwrap().size_pt,
        14.0
    );
    assert_eq!(opened.chapter_templates()[0].opener, "Lesson");
    assert!(opened.saved_position().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_named_snapshot_reopens_the_earlier_manuscript_styles_and_selection() {
    use docwrite_model::{Position, Selection};

    let mut book = Book::new("Essay");
    book.insert("Opening paragraph.").unwrap();
    let id = book.block_ids()[0];
    book.set_selection(Selection {
        anchor: Position::new(id, 0),
        focus: Position::new(id, 7),
    })
    .unwrap();
    let mut body = book
        .paragraph_styles()
        .iter()
        .find(|style| style.name == "Body")
        .unwrap()
        .clone();
    body.size_pt = 14.0;
    book.set_paragraph_style(body).unwrap();
    let mut template = book.chapter_templates()[0].clone();
    template.opener = "Lesson".into();
    book.set_chapter_template(template).unwrap();

    let dir = env::temp_dir().join(format!("legebook-snap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    book.save_snapshot(&dir, "dawn").unwrap();
    book.insert(" Later.").unwrap();
    let mut template = book.chapter_templates()[0].clone();
    template.opener = "Chapter".into();
    book.set_chapter_template(template).unwrap();
    book.save_bundle(&dir).unwrap();

    let snap = Book::load_snapshot(&dir, "dawn").unwrap();
    assert!(snap.plain_text().contains("Opening paragraph."));
    assert!(!snap.plain_text().contains("Later"));
    assert_eq!(
        snap.paragraph_styles().iter().find(|style| style.name == "Body").unwrap().size_pt,
        14.0
    );
    assert_eq!(snap.chapter_templates()[0].opener, "Lesson");
    let selection = snap.selection();
    assert_eq!(selection.anchor.offset, 0);
    assert_eq!(selection.focus.offset, 7);
    assert!(!selection.is_collapsed());

    let live = Book::load_bundle(&dir).unwrap();
    assert!(live.plain_text().contains("Later"));
    assert_eq!(live.chapter_templates()[0].opener, "Chapter");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bibliography_index_and_csl_bibtex_round_trip() {
    let mut book = Book::new("Essay");
    book.import_csl_json(
        r#"[{"id":"rivers","type":"book","title":"Rivers","author":"Ada","issued":"1999"}]"#,
    )
    .unwrap();
    let exported = book.export_csl_json();
    let mut again = Book::new("Essay");
    again.import_csl_json(&exported).unwrap();
    assert_eq!(again.bibliography()[0].title, "Rivers");
    let bib = again.export_bibtex();
    assert!(bib.contains("@book{rivers"));
    let mut third = Book::new("Essay");
    third.import_bibtex(&bib).unwrap();
    assert_eq!(third.bibliography()[0].author, "Ada");
    let block = book.block_ids()[0];
    book.add_index_term("river", block);
    assert_eq!(book.index_terms()[0].term, "river");
    book.add_source(SourceNote {
        id: "rivers".into(),
        document: "source.pdf".into(),
        page: 1,
        rects: vec![[1.0, 2.0, 3.0, 4.0]],
        passage: "a river".into(),
        citation: "Rivers".into(),
        annotation: "margin".into(),
    });
    book.cite("rivers").unwrap();
    let chapter = book.parts()[0].chapters()[0].id();
    let used = book.sources_in_chapter(chapter);
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].rects, vec![[1.0, 2.0, 3.0, 4.0]]);
    println!("csl title: {}", again.bibliography()[0].title);
    println!("bibtex author: {}", third.bibliography()[0].author);
    println!("index term: {}", book.index_terms()[0].term);
    println!(
        "sources in chapter: {} page {} rects {:?}",
        used.len(),
        used[0].page,
        used[0].rects
    );
}
