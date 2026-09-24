//! The sidebar the window hit-tests: collapse, and a drag that reorders chapters.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_app::{Editor, MapKind};

fn titles(editor: &Editor) -> Vec<String> {
    editor
        .book()
        .parts()
        .iter()
        .flat_map(|part| {
            part.chapters()
                .iter()
                .map(|chapter| chapter.title().to_string())
        })
        .collect()
}

fn press(editor: &mut Editor, index: usize) {
    let (x, y) = Editor::map_row_center(index);
    editor.hover(x, y);
    editor.pointer(true);
}

fn release(editor: &mut Editor, index: usize) {
    let (x, y) = Editor::map_row_center(index);
    editor.hover(x, y);
    editor.pointer(false);
}

#[test]
fn the_map_lists_matter_and_a_drag_reorders_chapters() {
    let mut editor = Editor::new();
    let body = editor.book().parts()[0].id();
    let alpha = editor.book_mut().add_chapter(body, "Alpha").unwrap();
    editor.book_mut().add_section(alpha, "Interlude").unwrap();
    editor.book_mut().add_chapter(body, "Beta").unwrap();
    let front = editor.book_mut().add_part("Front matter");
    editor.book_mut().add_chapter(front, "Preface").unwrap();
    let back = editor.book_mut().add_part("Back matter");
    editor.book_mut().add_chapter(back, "Index").unwrap();

    let rows = editor.map_rows();
    assert!(
        rows.iter()
            .any(|row| row.kind == MapKind::FrontMatter && row.title == "Front matter")
    );
    assert!(
        rows.iter()
            .any(|row| row.kind == MapKind::BackMatter && row.title == "Back matter")
    );
    assert!(
        rows.iter()
            .any(|row| row.kind == MapKind::Chapter && row.title == "Alpha")
    );
    assert!(
        rows.iter()
            .any(|row| row.kind == MapKind::Section && row.title == "Interlude")
    );
    assert!(
        rows.iter().all(|row| row.title != "Section"),
        "a chapter's one implicit section is not listed"
    );

    let body_row = rows
        .iter()
        .position(|row| row.kind == MapKind::Part)
        .unwrap();
    press(&mut editor, body_row);
    release(&mut editor, body_row);
    assert!(
        editor
            .map_rows()
            .iter()
            .all(|row| row.title != "Chapter 1" && row.title != "Alpha"),
        "collapsing the body hides its chapters: {:?}",
        editor.map_rows()
    );
    assert!(editor.map_rows().iter().any(|row| row.title == "Preface"));
    press(&mut editor, body_row);
    release(&mut editor, body_row);

    let rows = editor.map_rows();
    let from = rows.iter().position(|row| row.title == "Beta").unwrap();
    let to = rows
        .iter()
        .position(|row| row.title == "Chapter 1")
        .unwrap();
    press(&mut editor, from);
    release(&mut editor, to);
    assert_eq!(titles(&editor)[0], "Beta");
    assert!(titles(&editor).contains(&"Preface".to_string()));
    assert_eq!(
        editor
            .book()
            .parts()
            .iter()
            .find(|part| part.title() == "Front matter")
            .unwrap()
            .chapters()[0]
            .title(),
        "Preface",
        "a drag inside the body leaves front matter in its part"
    );
}

#[test]
fn typing_does_not_write_the_bundle_until_autosave() {
    let dir = std::env::temp_dir().join(format!("lege-autosave-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut editor = Editor::new();
    editor.open_bundle(&dir);
    editor.type_text(" Hello");
    assert!(
        !docwrite_model::Book::is_bundle(&dir),
        "typing must not autosave"
    );
    editor.autosave().unwrap();
    let on_disk = || {
        docwrite_model::Book::load_bundle(&dir)
            .unwrap()
            .plain_text()
    };
    let saved = on_disk();
    assert!(saved.contains("Hello"));
    editor.save_named_snapshot("dawn").unwrap();
    editor.type_text(" MORE");
    let still = on_disk();
    assert!(
        !still.contains("MORE"),
        "the later keystrokes stay off the disk"
    );
    editor.autosave().unwrap();
    editor.restore_named_snapshot("dawn").unwrap();
    assert!(editor.book().plain_text().contains("Hello"));
    assert!(!editor.book().plain_text().contains("MORE"));
    let _ = std::fs::remove_dir_all(&dir);
}
