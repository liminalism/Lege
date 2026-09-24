//! Write a sample book to try the editor with:
//! `cargo run -p docwrite-app --example demo_book -- Demo.legebook`
//! then `cargo run -p docwrite-app -- Demo.legebook`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_model::{Book, NoteKind, Position, Selection};

const PROSE: &str = "The archive kept its letters in bundles tied with string, and each \
bundle carried a date in a hand that changed over the years from careful to hurried. \
Reading them in order was like watching a clerk grow old, one season at a time, \
until the last bundle stopped mid-sentence.";

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Demo.legebook".into());
    let mut book = Book::new("The Archive");
    let chapter = |n: usize| -> String {
        (0..24)
            .map(|p| format!("{PROSE} ({n}.{p})"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    book.insert(&chapter(1)).expect("insert");
    let second = book.block_ids()[1];
    book.set_selection(Selection::collapsed(Position::new(second, 0)))
        .expect("select");
    book.attach_note(
        NoteKind::Footnote,
        "The bundles were catalogued in 1911, though the string is older than the catalogue.",
    )
    .expect("note");
    let fourth = book.block_ids()[3];
    book.set_selection(Selection::collapsed(Position::new(fourth, 0)))
        .expect("select");
    book.attach_note(
        NoteKind::Endnote,
        "See the clerk's own index, which disagrees with the dates.",
    )
    .expect("endnote");
    let last = *book.block_ids().last().expect("block");
    let len = book.block_len(last).expect("len");
    book.set_selection(Selection::collapsed(Position::new(last, len)))
        .expect("select");
    let part = book.parts()[0].id();
    for (n, title) in ["The Clerk", "Bundles", "Mid-sentence"].iter().enumerate() {
        book.add_chapter(part, *title).expect("chapter");
        let fresh = *book.block_ids().last().expect("block");
        book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
            .expect("select");
        book.insert(&chapter(n + 2)).expect("insert");
    }
    book.save_bundle(std::path::Path::new(&path)).expect("save");
    println!("wrote {path}");
}
