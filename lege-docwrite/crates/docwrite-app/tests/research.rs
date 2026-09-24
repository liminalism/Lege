//! A source PDF beside the manuscript: capture a passage, quote and cite it,
//! and follow the citation back to the passage.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_app::{Editor, PromptKind};
use docwrite_model::{Book, Direction, Motion};

/// A source PDF: an exported book whose text the pane can read.
fn source_pdf() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("docwrite-research-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut book = Book::new("Letters");
    book.insert("The clerk wrote every date twice, once in ink and once in pencil.")
        .unwrap();
    let editor = Editor::with_book(book);
    let path = dir.join("Letters.pdf");
    editor.export_pdf_to(&path).unwrap();
    path
}

fn frame(editor: &mut Editor) -> pixelkit_raster::WindowBuffer {
    let mut buffer = pixelkit_raster::WindowBuffer::new(1500, 900);
    editor.paint(&mut buffer);
    buffer
}

#[test]
fn capture_quote_cite_and_follow_back() {
    let pdf = source_pdf();
    let mut editor = Editor::new();
    editor.open_source(&pdf).unwrap();
    frame(&mut editor);
    let words = editor.source_words();
    let start = words
        .iter()
        .position(|word| word == "clerk")
        .unwrap_or_else(|| panic!("the source's words: {words:?}"));
    assert_eq!(words[start + 1], "wrote");

    editor.select_source_words(start, start + 3);
    assert_eq!(
        editor.source_selection().as_deref(),
        Some("clerk wrote every date")
    );
    let id = editor.capture_source().unwrap();
    assert_eq!(
        editor.prompt().map(|prompt| prompt.kind),
        Some(PromptKind::Citation)
    );
    assert_eq!(
        editor.prompt().unwrap().text,
        "Letters, p. 1",
        "a default citation"
    );
    for _ in 0.."Letters, p. 1".len() {
        editor.prompt_backspace();
    }
    editor.prompt_type("Clerk 1911, 1");
    editor.commit_prompt();
    let source = editor.book().source(&id).unwrap().clone();
    assert_eq!(source.citation, "Clerk 1911, 1");
    assert_eq!(source.page, 0);
    assert_eq!(source.rects.len(), 4);
    assert!(
        source
            .rects
            .iter()
            .all(|rect| rect.iter().all(|v| (0.0..=1.0).contains(v)))
    );

    editor.move_end(false);
    editor.quote_source();
    let text = editor.book().plain_text();
    assert!(
        text.contains("\u{201c}clerk wrote every date\u{201d} (Clerk 1911, 1)"),
        "{text}"
    );
    let chapter_sources = editor
        .book()
        .sources_in_chapter(editor.book().parts()[0].chapters()[0].id());
    assert_eq!(
        chapter_sources.len(),
        1,
        "the chapter lists the source it cites"
    );

    // Follow the citation: close the pane, put the caret inside it, follow.
    editor.close_source();
    editor
        .book_mut()
        .move_caret(Motion::Char(Direction::Backward))
        .unwrap();
    assert!(editor.follow_citation());
    let (path, page) = editor.source_view().unwrap();
    assert_eq!(path, pdf.as_path());
    assert_eq!(page, 0);
    assert_eq!(editor.source_highlight(), source.rects.as_slice());

    // The pane draws beside the pages.
    let buffer = frame(&mut editor);
    let pane_pixel = buffer.pixels[(600 * 1500 + 1400) as usize];
    assert_ne!(
        pane_pixel,
        docwrite_app::PAPER.desk,
        "the pane covers the right side"
    );
}
