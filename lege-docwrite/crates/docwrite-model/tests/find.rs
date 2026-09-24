//! Find ignores case and wraps; replace-all is one undo step.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_model::{Book, Position, Selection};

fn book() -> Book {
    let mut book = Book::new("Find");
    book.insert("The clerk wrote.\nA second clerk, the Clerk of Letters.\nNo match here.")
        .unwrap();
    book
}

#[test]
fn find_next_walks_matches_and_wraps() {
    let mut book = book();
    let ids = book.block_ids();
    book.set_selection(Selection::collapsed(Position::new(ids[0], 0)))
        .unwrap();
    let mut seen = Vec::new();
    for _ in 0..4 {
        assert!(book.find_next("CLERK"));
        let selection = book.selection();
        let block = ids
            .iter()
            .position(|id| *id == selection.anchor.block)
            .unwrap();
        seen.push((block, selection.anchor.offset, selection.focus.offset));
    }
    assert_eq!(
        seen,
        vec![(0, 4, 9), (1, 9, 14), (1, 20, 25), (0, 4, 9)],
        "wraps round"
    );
    assert!(!book.find_next("absent"));
}

#[test]
fn replace_all_is_one_undo_step_and_does_not_chase_its_own_output() {
    let mut book = book();
    let before = book.plain_text();
    let count = book.replace_all("clerk", "clerk-clerk").unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        book.plain_text(),
        "The clerk-clerk wrote.\nA second clerk-clerk, the clerk-clerk of Letters.\nNo match here."
    );
    book.undo().unwrap();
    assert_eq!(
        book.plain_text(),
        before,
        "one undo reverses every replacement"
    );
    assert_eq!(book.replace_all("zzz", "y").unwrap(), 0);
}
