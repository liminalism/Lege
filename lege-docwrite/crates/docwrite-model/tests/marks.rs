//! Bold, italic, small caps and scripts toggle over a selection or for the
//! text typed next, as one undo step each.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_model::{Book, Mark, Position, Script, Selection};

fn marks_at(book: &Book, block: usize, offset: usize) -> docwrite_model::RunMarks {
    let id = book.block_ids()[block];
    let block = book.block(id).unwrap();
    block
        .runs()
        .iter()
        .find(|run| run.start <= offset && offset < run.end)
        .map(|run| run.marks.clone())
        .unwrap()
}

#[test]
fn a_selection_toggles_on_off_and_undoes_in_one_step() {
    let mut book = Book::new("Marks");
    book.insert("plain words here\nsecond line").unwrap();
    let ids = book.block_ids();
    book.set_selection(Selection {
        anchor: Position::new(ids[0], 6),
        focus: Position::new(ids[1], 6),
    })
    .unwrap();
    assert!(!book.selection_has(Mark::Bold));
    book.toggle_mark(Mark::Bold).unwrap();
    assert!(book.selection_has(Mark::Bold));
    assert!(
        !marks_at(&book, 0, 2).bold,
        "before the selection stays plain"
    );
    assert!(marks_at(&book, 0, 8).bold);
    assert!(marks_at(&book, 1, 3).bold);
    assert!(
        !marks_at(&book, 1, 8).bold,
        "after the selection stays plain"
    );
    assert_eq!(
        book.plain_text(),
        "plain words here\nsecond line",
        "text untouched"
    );

    book.toggle_mark(Mark::Superscript).unwrap();
    assert_eq!(marks_at(&book, 0, 8).script, Script::Super);
    book.toggle_mark(Mark::Subscript).unwrap();
    assert_eq!(
        marks_at(&book, 0, 8).script,
        Script::Sub,
        "sub replaces super"
    );

    book.undo().unwrap();
    book.undo().unwrap();
    assert!(marks_at(&book, 0, 8).bold);
    book.undo().unwrap();
    assert!(!marks_at(&book, 0, 8).bold, "each toggle is one undo step");
    book.redo().unwrap();
    assert!(marks_at(&book, 0, 8).bold);

    // All-bold selection toggles off.
    book.toggle_mark(Mark::Bold).unwrap();
    assert!(!marks_at(&book, 0, 8).bold);
}

#[test]
fn with_nothing_selected_the_toggle_applies_to_typing() {
    let mut book = Book::new("Typing");
    book.insert("Roman ").unwrap();
    book.toggle_mark(Mark::Italic).unwrap();
    assert!(book.selection_has(Mark::Italic));
    book.insert("italic").unwrap();
    book.toggle_mark(Mark::Italic).unwrap();
    book.insert(" roman").unwrap();
    assert!(!marks_at(&book, 0, 2).italic);
    assert!(marks_at(&book, 0, 8).italic);
    assert!(!marks_at(&book, 0, 14).italic);
}
