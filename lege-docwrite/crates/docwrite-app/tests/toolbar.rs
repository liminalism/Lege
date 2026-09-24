//! The toolbar's buttons, its prompts, and Book Map navigation.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_app::{Action, Editor, PromptKind};
use docwrite_model::{BlockKind, Mark, NoteKind, Position, Selection};

fn frame(editor: &mut Editor) {
    let mut buffer = pixelkit_raster::WindowBuffer::new(1400, 800);
    editor.paint(&mut buffer);
}

fn click(editor: &mut Editor, action: Action) {
    let button = editor
        .toolbar_buttons(1400)
        .into_iter()
        .find(|button| button.action == action)
        .unwrap_or_else(|| panic!("no {action:?} button"));
    editor.hover(
        (button.x + button.w / 2) as f32,
        (button.y + button.h / 2) as f32,
    );
    editor.pointer(true);
    editor.pointer(false);
}

#[test]
fn buttons_format_the_selection_and_show_its_state() {
    let mut editor = Editor::new();
    frame(&mut editor);
    let first = editor.book().block_ids()[0];
    editor
        .book_mut()
        .set_selection(Selection {
            anchor: Position::new(first, 0),
            focus: Position::new(first, 4),
        })
        .unwrap();
    frame(&mut editor);
    let bold = |editor: &Editor| {
        editor
            .toolbar_buttons(1400)
            .into_iter()
            .find(|button| button.action == Action::Mark(Mark::Bold))
            .unwrap()
            .active
    };
    assert!(!bold(&editor));
    click(&mut editor, Action::Mark(Mark::Bold));
    assert!(editor.book().selection_has(Mark::Bold));
    assert!(bold(&editor), "the button shows the selection is bold");
    editor.undo();
    assert!(!editor.book().selection_has(Mark::Bold));

    assert_eq!(editor.current_style_label(), "Body");
    click(&mut editor, Action::CycleStyle);
    assert_eq!(editor.current_style_label(), "Subhead");
    let kind = editor.book().block(first).unwrap().kind().clone();
    assert_eq!(kind, BlockKind::Subhead);
    editor.set_block_kind_number(2);
    assert_eq!(editor.current_style_label(), "Block Quote");

    let before = editor
        .book()
        .paragraph_styles()
        .iter()
        .find(|style| style.name == "Body")
        .unwrap()
        .size_pt;
    click(&mut editor, Action::Larger);
    let after = editor
        .book()
        .paragraph_styles()
        .iter()
        .find(|style| style.name == "Body")
        .unwrap()
        .size_pt;
    assert_eq!(after, before + 0.5);
}

#[test]
fn prompts_add_and_rename_chapters_and_attach_notes() {
    let mut editor = Editor::new();
    frame(&mut editor);
    click(&mut editor, Action::NewChapter);
    assert_eq!(
        editor.prompt().map(|prompt| prompt.kind),
        Some(PromptKind::NewChapter)
    );
    editor.prompt_type("The Clerk");
    assert!(editor.commit_prompt());
    assert!(editor.prompt().is_none());
    let titles: Vec<String> = editor.book().parts()[0]
        .chapters()
        .iter()
        .map(|chapter| chapter.title().to_string())
        .collect();
    assert_eq!(
        titles,
        vec!["Chapter 1".to_string(), "The Clerk".to_string()]
    );
    editor.type_text("Words of the new chapter.");
    assert!(
        editor
            .book()
            .plain_text()
            .ends_with("Words of the new chapter.")
    );

    editor.open_prompt(PromptKind::RenameChapter);
    assert_eq!(
        editor.prompt().unwrap().text,
        "The Clerk",
        "rename starts from the title"
    );
    for _ in 0.."Clerk".len() {
        editor.prompt_backspace();
    }
    editor.prompt_type("Archivist");
    editor.commit_prompt();
    assert_eq!(
        editor.book().parts()[0].chapters()[1].title(),
        "The Archivist"
    );

    editor.open_prompt(PromptKind::Footnote);
    editor.prompt_type("A note on the archivist.");
    editor.commit_prompt();
    let focus = editor.book().selection().focus.block;
    let note = editor.book().block(focus).unwrap().note().unwrap();
    assert_eq!(editor.book().note(note).unwrap().kind(), NoteKind::Footnote);

    editor.open_prompt(PromptKind::Endnote);
    editor.prompt_type("ignored");
    assert!(editor.cancel_prompt());
    assert_eq!(
        editor.book().note(note).unwrap().text(),
        "A note on the archivist."
    );
}

#[test]
fn a_chapter_row_in_the_book_map_goes_to_the_chapter() {
    let mut editor = Editor::new();
    editor.type_text(&" Body text for the first chapter.".repeat(60));
    editor.new_chapter("Second");
    editor.type_text("Second chapter text.");
    frame(&mut editor);
    editor.jump_to_page(1);
    frame(&mut editor);
    assert_eq!(editor.pager().scroll(), 0.0);
    let rows = editor.map_rows();
    let second = rows.iter().position(|row| row.title == "Second").unwrap();
    let (x, y) = Editor::map_row_center(second);
    editor.hover(x, y);
    editor.pointer(true);
    editor.pointer(false);
    frame(&mut editor);
    assert!(
        editor.pager().scroll() >= 1.0,
        "the view moved to the second chapter"
    );
    let focus = editor.book().selection().focus.block;
    let text = editor.book().block(focus).unwrap().text();
    assert!(text.starts_with("Second chapter"));
}

#[test]
fn prompts_index_terms_and_build_the_bibliography() {
    let mut editor = Editor::new();
    editor.open_prompt(PromptKind::IndexTerm);
    editor.prompt_type("opening");
    editor.commit_prompt();
    assert_eq!(editor.book().index_terms()[0].term, "opening");

    editor.open_prompt(PromptKind::BibliographyEntry);
    editor.prompt_type("Arendt, Hannah; The Human Condition; 1958");
    editor.commit_prompt();
    let entry = &editor.book().bibliography()[0];
    assert_eq!(entry.author, "Arendt, Hannah");
    assert_eq!(entry.title, "The Human Condition");
    assert_eq!(entry.issued, "1958");
    assert_eq!(entry.key, "arendt1958");

    let dir = std::env::temp_dir().join(format!("docwrite-bib-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bib = dir.join("refs.bib");
    std::fs::write(
        &bib,
        "@book{weber1922,\n  author = {Weber, Max},\n  title = {Economy and Society},\n  year = {1922}\n}\n",
    )
    .unwrap();
    editor.open_prompt(PromptKind::ImportBibliography);
    editor.prompt_type(bib.to_str().unwrap());
    editor.commit_prompt();
    assert_eq!(
        editor.book().bibliography().len(),
        2,
        "{:?}",
        editor.book().bibliography()
    );
    frame(&mut editor);
    let document = editor.document().unwrap();
    let headings: Vec<String> = (1..=document.page_count())
        .filter_map(|page| document.page_texts(page).first().cloned())
        .collect();
    assert!(headings.iter().any(|h| h == "Bibliography"), "{headings:?}");
    assert!(headings.iter().any(|h| h == "Index"), "{headings:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
