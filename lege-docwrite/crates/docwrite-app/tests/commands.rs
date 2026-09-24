//! The commands the window binds: clipboard, undo, caret motion by line and
//! by pointer, files, autosave and export.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use docwrite_app::Editor;
use docwrite_model::{Position, Selection};

const PROSE: &str = "The archive kept its letters in bundles tied with string, and each \
bundle carried a date in a hand that changed over the years from careful to hurried.";

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("docwrite-commands-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn editor_with(paragraphs: usize) -> Editor {
    let mut editor = Editor::new();
    let body: Vec<String> = (0..paragraphs).map(|n| format!("{n}. {PROSE}")).collect();
    let first = editor.book().block_ids()[0];
    let len = editor.book().block_len(first).unwrap();
    editor
        .book_mut()
        .set_selection(Selection {
            anchor: Position::new(first, 0),
            focus: Position::new(first, len),
        })
        .unwrap();
    editor.paste_text(&body.join("\n"));
    let mut frame = pixelkit_raster::WindowBuffer::new(1100, 800);
    editor.paint(&mut frame);
    editor
}

fn caret(editor: &Editor) -> (usize, usize) {
    let focus = editor.book().selection().focus;
    let index = editor
        .book()
        .block_ids()
        .iter()
        .position(|id| *id == focus.block)
        .unwrap();
    (index, focus.offset)
}

#[test]
fn cut_paste_undo_redo_round_trip() {
    let mut editor = editor_with(3);
    let before = editor.book().plain_text();
    assert!(
        editor.copy_text().is_none(),
        "nothing selected, nothing copied"
    );
    let first = editor.book().block_ids()[0];
    editor
        .book_mut()
        .set_selection(Selection {
            anchor: Position::new(first, 0),
            focus: Position::new(first, 3),
        })
        .unwrap();
    assert_eq!(editor.copy_text().as_deref(), Some("0. "));
    let cut = editor.cut_text().unwrap();
    assert_eq!(cut, "0. ");
    assert!(!editor.book().plain_text().starts_with("0. "));
    editor.move_end(false);
    editor.paste_text(&cut);
    editor.undo();
    editor.undo();
    assert_eq!(editor.book().plain_text(), before);
    editor.redo();
    assert!(!editor.book().plain_text().starts_with("0. "));
}

#[test]
fn up_and_down_follow_typeset_lines_across_pages() {
    let mut editor = editor_with(40);
    let first = editor.book().block_ids()[0];
    editor
        .book_mut()
        .set_selection(Selection::collapsed(Position::new(first, 10)))
        .unwrap();
    editor.refresh_layout();
    editor.move_line(true, false);
    let (block, offset) = caret(&editor);
    assert_eq!(block, 0, "the first paragraph wraps: down stays in it");
    assert!(
        offset > 40,
        "down moved onto the second line, offset {offset}"
    );
    editor.move_line(false, false);
    let (block, offset) = caret(&editor);
    assert_eq!(block, 0);
    assert!(
        (8..=12).contains(&offset),
        "up comes back near column 10, got {offset}"
    );

    // Walk down until the caret crosses onto page 2: the view follows.
    for _ in 0..200 {
        editor.move_line(true, false);
    }
    let mut frame = pixelkit_raster::WindowBuffer::new(1100, 800);
    editor.paint(&mut frame);
    assert!(
        editor.pager().scroll() >= 1.0,
        "moving down turned the page"
    );
    assert!(editor.caret_area().is_some());
    editor.move_home(false);
    assert_eq!(caret(&editor).1, 0);
    editor.move_end(true);
    assert!(!editor.book().selection().is_collapsed());
}

#[test]
fn clicking_a_line_puts_the_caret_there() {
    let mut editor = editor_with(4);
    let mut frame = pixelkit_raster::WindowBuffer::new(1100, 800);
    editor.paint(&mut frame);
    let caret_before = editor.caret_area().unwrap();
    // Click back at the start of the page's text area: the first line.
    let first = editor.book().block_ids()[0];
    editor
        .book_mut()
        .set_selection(Selection::collapsed(Position::new(first, 0)))
        .unwrap();
    editor.paint(&mut frame);
    let top_left = editor.caret_area().unwrap();
    editor.hover(caret_before.x as f32 - 1.0, caret_before.y as f32 + 4.0);
    editor.pointer(true);
    editor.pointer(false);
    editor.paint(&mut frame);
    let after = editor.caret_area().unwrap();
    assert!(
        (after.y - caret_before.y).abs() < 2.0,
        "the click landed on the clicked line: {after:?} vs {caret_before:?} (top {top_left:?})"
    );
    assert!(
        !editor.click_at(5.0, 5.0, false),
        "the sidebar is not a page"
    );
}

#[test]
fn a_new_book_saves_reopens_autosaves_and_exports() {
    let dir = scratch("files");
    let path = dir.join("Novel.legebook");
    let mut editor = Editor::open_or_create(&path).unwrap();
    assert!(editor.is_dirty(), "a new book has not been written yet");
    editor.type_text("It began in the archive.");
    editor.save_now().unwrap();
    assert!(!editor.is_dirty());
    let reopened = Editor::open_or_create(&path).unwrap();
    assert!(
        reopened
            .book()
            .plain_text()
            .contains("It began in the archive.")
    );
    assert_eq!(reopened.book().title(), "Novel");

    // Autosave waits for a pause, then writes off the typing thread.
    editor.type_text(" Later.");
    assert!(
        !editor.autosave_if_idle(Duration::from_secs(60)),
        "typing just happened"
    );
    assert!(editor.autosave_if_idle(Duration::ZERO));
    assert!(!editor.is_dirty());
    editor.save_now().unwrap(); // waits for the background save
    assert!(editor.take_save_error().is_none());
    let reopened = Editor::open_or_create(&path).unwrap();
    assert!(reopened.book().plain_text().contains("Later."));

    let pdf = dir.join("Novel.pdf");
    editor.export_pdf_to(&pdf).unwrap();
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    let md = dir.join("Novel.md");
    editor.export_markdown_to(&md).unwrap();
    assert!(
        std::fs::read_to_string(&md)
            .unwrap()
            .contains("It began in the archive.")
    );

    // The headless CLI exports the same book.
    let cli_pdf = dir.join("cli.pdf");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_lege-docwrite"))
        .arg("export")
        .arg(&path)
        .arg(&cli_pdf)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(std::fs::read(&cli_pdf).unwrap().starts_with(b"%PDF"));
    let _ = std::fs::remove_dir_all(&dir);
}
