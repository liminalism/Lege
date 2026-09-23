//! Edit transactions against the M1 model gate: insert, delete, selection,
//! word motion, cut/copy/paste and undo/redo, including across paragraphs.
//! Pages are derived and are not stored, so a span that starts in one chapter
//! and ends in another is the model-level page-boundary case.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_model::{
    BlockKind, Book, Direction, ModelError, Motion, NoteKind, Position, RunMarks, Script, Selection,
};

fn edited(text: &str) -> Book {
    let mut book = Book::new("Essay");
    book.insert(text).unwrap();
    book
}

#[test]
fn insert_delete_and_undo_round_trip() {
    let mut book = edited("Hello");
    assert_eq!(book.plain_text(), "Hello");
    book.delete_backward().unwrap();
    assert_eq!(book.plain_text(), "Hell");
    book.undo().unwrap();
    assert_eq!(book.plain_text(), "Hello");
    book.redo().unwrap();
    assert_eq!(book.plain_text(), "Hell");
    book.undo().unwrap();
    book.undo().unwrap();
    assert_eq!(book.plain_text(), "");
    assert!(matches!(book.undo(), Err(ModelError::NothingToUndo)));
    book.redo().unwrap();
    book.redo().unwrap();
    assert_eq!(book.plain_text(), "Hell");
    assert!(matches!(book.redo(), Err(ModelError::NothingToRedo)));
}

#[test]
fn newline_splits_and_backspace_merges_keeping_ids() {
    let mut book = Book::new("Essay");
    let original = book.block_ids()[0];
    book.insert("Hello\nWorld").unwrap();
    let ids = book.block_ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], original);
    assert_eq!(book.block(ids[0]).unwrap().text(), "Hello");
    assert_eq!(book.block(ids[1]).unwrap().text(), "World");

    book.set_selection(Selection::collapsed(Position::new(ids[1], 0)))
        .unwrap();
    book.delete_backward().unwrap();
    assert_eq!(book.block_ids(), vec![original]);
    assert_eq!(book.plain_text(), "HelloWorld");

    book.undo().unwrap();
    assert_eq!(book.block_ids(), ids);
    assert_eq!(book.plain_text(), "Hello\nWorld");
    book.redo().unwrap();
    assert_eq!(book.plain_text(), "HelloWorld");
}

#[test]
fn carriage_return_is_one_paragraph_break() {
    let book = edited("A\r\nB\rC");
    assert_eq!(book.plain_text(), "A\nB\nC");
    assert_eq!(book.block_ids().len(), 3);
}

#[test]
fn grapheme_motion_keeps_combining_marks() {
    let mut book = edited("e\u{0301}x");
    let id = book.block_ids()[0];
    book.set_selection(Selection::collapsed(Position::new(id, 0)))
        .unwrap();
    book.move_caret(Motion::Char(Direction::Forward)).unwrap();
    assert_eq!(book.selection().focus.offset, 2);
    book.move_caret(Motion::Char(Direction::Backward)).unwrap();
    assert_eq!(book.selection().focus.offset, 0);
    book.move_caret(Motion::Char(Direction::Forward)).unwrap();
    book.delete_backward().unwrap();
    assert_eq!(book.plain_text(), "x");
}

#[test]
fn word_motion_crosses_paragraphs_and_selection_extends() {
    let mut book = edited("Alpha beta.\nGamma delta.");
    let ids = book.block_ids();
    book.set_selection(Selection::collapsed(Position::new(ids[0], 0)))
        .unwrap();
    book.move_caret(Motion::Word(Direction::Forward)).unwrap();
    assert_eq!(book.selection().focus, Position::new(ids[0], 5));
    book.move_caret(Motion::Word(Direction::Forward)).unwrap();
    assert_eq!(book.selection().focus, Position::new(ids[0], 10));
    book.move_caret(Motion::Word(Direction::Forward)).unwrap();
    assert_eq!(
        book.selection().focus.offset,
        book.block_len(ids[0]).unwrap()
    );
    book.move_caret(Motion::Word(Direction::Forward)).unwrap();
    assert_eq!(book.selection().focus, Position::new(ids[1], 0));

    book.extend_selection(Motion::Word(Direction::Backward))
        .unwrap();
    let selection = book.selection();
    assert_eq!(selection.anchor.block, ids[1]);
    assert_eq!(selection.focus.block, ids[0]);
    assert!(book.selection().anchor.block != book.selection().focus.block);

    book.move_caret(Motion::Word(Direction::Backward)).unwrap();
    assert!(book.selection().is_collapsed());
    assert_eq!(book.selection().focus.block, ids[0]);
}

#[test]
fn cut_copy_paste_and_undo_round_trip_across_paragraphs() {
    let mut book = edited("Alpha beta.\nGamma delta.\nEpsilon zeta.");
    let original = book.plain_text();
    let ids = book.block_ids();
    let end = book.block_len(ids[2]).unwrap();
    book.set_selection(Selection {
        anchor: Position::new(ids[0], 6),
        focus: Position::new(ids[2], end),
    })
    .unwrap();
    let copied = book.copy().unwrap();
    let cut = book.cut().unwrap();
    assert_eq!(cut.plain_text(), copied.plain_text());
    assert_eq!(cut.plain_text(), "beta.\nGamma delta.\nEpsilon zeta.");
    let after_cut = book.plain_text();
    assert_eq!(after_cut, "Alpha ");

    book.paste(&cut).unwrap();
    assert_eq!(book.plain_text(), original);

    book.undo().unwrap();
    assert_eq!(book.plain_text(), after_cut);
    book.undo().unwrap();
    assert_eq!(book.plain_text(), original);
    assert_eq!(book.block_ids(), ids);

    book.redo().unwrap();
    book.redo().unwrap();
    assert_eq!(book.plain_text(), original);
}

#[test]
fn edits_cross_a_chapter_boundary() {
    // A page boundary is a cut through this same block sequence. Crossing
    // from one chapter into the next is that case while pages are still derived.
    let mut book = Book::new("Essay");
    let part = book.parts()[0].id();
    book.add_chapter(part, "Next").unwrap();
    let ids = book.block_ids();
    assert_eq!(ids.len(), 2);
    book.set_selection(Selection::collapsed(Position::new(ids[0], 0)))
        .unwrap();
    book.insert("Left side").unwrap();
    book.set_selection(Selection::collapsed(Position::new(ids[1], 0)))
        .unwrap();
    book.insert("Right side").unwrap();
    assert_eq!(book.plain_text(), "Left side\nRight side");

    let right_len = book.block_len(ids[1]).unwrap();
    book.set_selection(Selection {
        anchor: Position::new(ids[0], 5),
        focus: Position::new(ids[1], right_len),
    })
    .unwrap();
    let clip = book.cut().unwrap();
    assert_eq!(clip.plain_text(), "side\nRight side");
    assert_eq!(book.plain_text(), "Left ");
    assert_eq!(book.block_ids(), vec![ids[0]]);

    book.paste(&clip).unwrap();
    assert_eq!(book.plain_text(), "Left side\nRight side");
    book.undo().unwrap();
    book.undo().unwrap();
    assert_eq!(book.plain_text(), "Left side\nRight side");
    assert_eq!(book.block_ids(), ids);
}

#[test]
fn run_marks_survive_edits_and_undo() {
    let mut book = Book::new("Essay");
    let bold = RunMarks {
        bold: true,
        ..RunMarks::default()
    };
    book.insert_with_marks("Hello", bold.clone()).unwrap();
    book.insert("!").unwrap();
    let id = book.block_ids()[0];
    let runs = book.block(id).unwrap().runs();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].marks.bold);
    assert_eq!(runs[0].end, 6);

    book.set_selection(Selection::collapsed(Position::new(id, 0)))
        .unwrap();
    book.insert_with_marks(
        "X",
        RunMarks {
            italic: true,
            script: Script::Super,
            ..RunMarks::default()
        },
    )
    .unwrap();
    let runs = book.block(id).unwrap().runs();
    assert!(runs[0].marks.italic);
    assert_eq!(runs[0].marks.script, Script::Super);
    assert!(runs[1].marks.bold);

    book.undo().unwrap();
    assert_eq!(book.block(id).unwrap().text(), "Hello!");
    assert_eq!(book.block(id).unwrap().runs().len(), 1);
    // Undo restored the caret to the start, where the italic insert began.
    book.insert("\n").unwrap();
    let ids = book.block_ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(book.block(ids[0]).unwrap().text(), "");
    assert!(book.block(ids[0]).unwrap().runs()[0].marks.bold);
    assert_eq!(book.block(ids[1]).unwrap().text(), "Hello!");
    assert!(book.block(ids[1]).unwrap().runs()[0].marks.bold);
}

#[test]
fn block_quote_kind_round_trips_through_cut_and_paste() {
    let mut book = edited("Quoted");
    let id = book.block_ids()[0];
    book.set_kind(id, BlockKind::BlockQuote).unwrap();
    let len = book.block_len(id).unwrap();
    book.set_selection(Selection {
        anchor: Position::new(id, 0),
        focus: Position::new(id, len),
    })
    .unwrap();
    let clip = book.cut().unwrap();
    assert_eq!(clip.block_kinds(), vec![BlockKind::BlockQuote]);
    assert_eq!(book.block(id).unwrap().kind(), &BlockKind::BlockQuote);
    assert_eq!(book.plain_text(), "");
    book.paste(&clip).unwrap();
    assert_eq!(book.plain_text(), "Quoted");
    assert_eq!(book.block(id).unwrap().kind(), &BlockKind::BlockQuote);
    book.undo().unwrap();
    assert_eq!(book.plain_text(), "");
    book.undo().unwrap();
    assert_eq!(book.plain_text(), "Quoted");
}

#[test]
fn footnote_slot_survives_undo_and_a_merge() {
    let mut book = edited("Hello\nWorld");
    let ids = book.block_ids();
    book.set_selection(Selection::collapsed(Position::new(ids[1], 0)))
        .unwrap();
    let note = book.attach_note(NoteKind::Footnote, "A source.").unwrap();
    assert_eq!(book.block(ids[1]).unwrap().note(), Some(note));
    assert_eq!(book.note(note).unwrap().text(), "A source.");
    assert_eq!(book.note(note).unwrap().kind(), NoteKind::Footnote);
    assert_eq!(book.block(ids[0]).unwrap().note(), None);

    book.undo().unwrap();
    assert_eq!(book.block(ids[1]).unwrap().note(), None);
    assert!(matches!(book.note(note), Err(ModelError::UnknownNote(_))));
    book.redo().unwrap();
    assert_eq!(book.block(ids[1]).unwrap().note(), Some(note));

    book.delete_backward().unwrap();
    assert_eq!(book.plain_text(), "HelloWorld");
    assert_eq!(book.block_ids(), vec![ids[0]]);
    // The joined-away block took its note with it. Undo puts both back.
    assert_eq!(book.block(ids[0]).unwrap().note(), None);
    book.undo().unwrap();
    assert_eq!(book.block(ids[1]).unwrap().note(), Some(note));
    assert_eq!(book.note(note).unwrap().text(), "A source.");
}

#[test]
fn offset_past_the_end_is_refused() {
    let mut book = edited("Hi");
    let id = book.block_ids()[0];
    let err = book
        .set_selection(Selection::collapsed(Position::new(id, 9)))
        .unwrap_err();
    assert!(matches!(
        err,
        ModelError::OffsetOutOfRange {
            offset: 9,
            len: 2,
            ..
        }
    ));
    assert!(book.check_consistency().is_ok());
}
