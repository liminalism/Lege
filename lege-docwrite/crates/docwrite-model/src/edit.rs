//! Transactions: insert, delete, selection, word motion, clipboard, undo.
//!
//! One public call is one undo step. Caret motion is not recorded. Builders
//! on [`Book`] (`add_part`, `add_chapter`, `add_section`) are not transactions
//! either; they clear this history.

use std::cmp::Ordering;

use crate::error::ModelError;
use crate::ids::{BlockId, NoteId};
use crate::nav;
use crate::runs::{self, Run, RunMarks};
use crate::tree::{Block, BlockKind, BlockLoc, Book, Note, NoteKind, Position, Selection};

/// Which way a motion or a neighbor step moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Toward the start of the book.
    Backward,
    /// Toward the end of the book.
    Forward,
}

/// A caret step. Character motion uses grapheme clusters. Word motion uses
/// Unicode word boundaries. Block motion moves by paragraph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// One grapheme, crossing into the neighboring block at either end.
    Char(Direction),
    /// To the end (forward) or start (backward) of a word. From the edge of a
    /// block, the step lands on the neighboring block instead of skipping a word there.
    Word(Direction),
    /// Backward moves to the start of this block, then to the previous block.
    /// Forward moves to the start of the next block.
    Block(Direction),
}

/// Text cut or copied out of the book.
///
/// Paragraph breaks are blocks. A trailing empty block means the selection
/// included the break before the following paragraph, whose characters were
/// not themselves selected. Kinds and runs travel with the blocks that a cut
/// removes; the block the caret remains in keeps its own kind and note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    blocks: Vec<FragmentBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FragmentBlock {
    kind: BlockKind,
    text: String,
    runs: Vec<Run>,
    note: Option<NoteId>,
}

impl Fragment {
    /// Blocks joined by newlines, including a trailing newline when the
    /// fragment ends on a paragraph break.
    pub fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// True when nothing was selected.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// How many paragraph blocks the fragment holds.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Kind of each fragment block, in order.
    pub fn block_kinds(&self) -> Vec<BlockKind> {
        self.blocks.iter().map(|block| block.kind.clone()).collect()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Transaction {
    changes: Vec<Change>,
    selection_before: Selection,
    selection_after: Selection,
}

#[derive(Clone, Debug)]
enum Change {
    ReplaceText {
        block: BlockId,
        start: usize,
        old: String,
        new: String,
        old_runs: Vec<Run>,
        new_runs: Vec<Run>,
    },
    InsertBlock {
        loc: BlockLoc,
        block: Block,
    },
    RemoveBlock {
        loc: BlockLoc,
        block: Block,
    },
    SetKind {
        block: BlockId,
        old: BlockKind,
        new: BlockKind,
    },
    SetNote {
        block: BlockId,
        old: Option<NoteId>,
        new: Option<NoteId>,
    },
    CreateNote {
        note: Note,
    },
}

struct Piece {
    text: String,
    runs: Vec<Run>,
    kind: Option<BlockKind>,
    note: Option<NoteId>,
}

impl Book {
    /// Move the focus. A range selection collapses toward the motion first.
    pub fn move_caret(&mut self, motion: Motion) -> Result<(), ModelError> {
        let origin = self.motion_origin(motion)?;
        let focus = self.position_after(origin, motion)?;
        self.selection = Selection::collapsed(focus);
        Ok(())
    }

    /// Move the focus and leave the anchor where it is.
    pub fn extend_selection(&mut self, motion: Motion) -> Result<(), ModelError> {
        let focus = self.position_after(self.selection.focus, motion)?;
        self.selection.focus = focus;
        Ok(())
    }

    /// Replace the selection. Both ends must already be real positions.
    pub fn set_selection(&mut self, selection: Selection) -> Result<(), ModelError> {
        self.ensure_position(selection.anchor)?;
        self.ensure_position(selection.focus)?;
        self.selection = selection;
        Ok(())
    }

    /// Insert text at the caret, replacing a selection. Newlines split blocks.
    /// `\r\n` and `\r` count as a single break. Typed text inherits the marks
    /// before the caret.
    pub fn insert(&mut self, text: &str) -> Result<(), ModelError> {
        self.insert_text(text, None)
    }

    /// [`Self::insert`], but the inserted characters take `marks` instead of
    /// inheriting them. Marks inside an existing block are left alone.
    pub fn insert_with_marks(&mut self, text: &str, marks: RunMarks) -> Result<(), ModelError> {
        self.insert_text(text, Some(marks))
    }

    /// Delete the selection, or the previous grapheme. At the start of a block,
    /// join with the previous block.
    pub fn delete_backward(&mut self) -> Result<(), ModelError> {
        self.transact(|book, changes| {
            if !book.selection.is_collapsed() {
                let (start, end) = book.normalized()?;
                return book.delete_range(start, end, changes);
            }
            let pos = book.selection.focus;
            if pos.offset == 0 {
                let Some(prev) = book.neighbor(pos.block, Direction::Backward)? else {
                    return Ok(());
                };
                let prev_len = book.block_len(prev)?;
                return book.delete_range(Position::new(prev, prev_len), pos, changes);
            }
            let text = book.block_ref(pos.block)?.text();
            let from = nav::prev_grapheme(&text, pos.offset);
            book.delete_range(Position::new(pos.block, from), pos, changes)
        })
    }

    /// Delete the selection, or the next grapheme. At the end of a block, join
    /// with the next block.
    pub fn delete_forward(&mut self) -> Result<(), ModelError> {
        self.transact(|book, changes| {
            if !book.selection.is_collapsed() {
                let (start, end) = book.normalized()?;
                return book.delete_range(start, end, changes);
            }
            let pos = book.selection.focus;
            let len = book.block_len(pos.block)?;
            if pos.offset >= len {
                let Some(next) = book.neighbor(pos.block, Direction::Forward)? else {
                    return Ok(());
                };
                return book.delete_range(pos, Position::new(next, 0), changes);
            }
            let text = book.block_ref(pos.block)?.text();
            let to = nav::next_grapheme(&text, pos.offset);
            book.delete_range(pos, Position::new(pos.block, to), changes)
        })
    }

    /// Copy the selection. A collapsed caret copies nothing.
    pub fn copy(&self) -> Result<Fragment, ModelError> {
        let (start, end) = self.normalized()?;
        if start == end {
            return Ok(Fragment { blocks: Vec::new() });
        }
        if start.block == end.block {
            let block = self.block_ref(start.block)?;
            let text = block
                .text_rope()
                .slice(start.offset..end.offset)
                .to_string();
            let runs = runs::slice_runs(block.runs(), start.offset, end.offset);
            return Ok(Fragment {
                blocks: vec![FragmentBlock {
                    kind: block.kind().clone(),
                    text,
                    runs,
                    note: None,
                }],
            });
        }
        let ids = self.ids_from_to(start.block, end.block)?;
        let mut blocks = Vec::new();
        let last = ids.len() - 1;
        for (index, id) in ids.iter().enumerate() {
            if index == last && end.offset == 0 {
                let block = self.block_ref(*id)?;
                blocks.push(FragmentBlock {
                    kind: block.kind().clone(),
                    text: String::new(),
                    runs: Vec::new(),
                    note: block.note(),
                });
                break;
            }
            let block = self.block_ref(*id)?;
            let (from, to, note) = if index == 0 {
                (start.offset, block.len_chars(), None)
            } else if index == last {
                (0, end.offset, block.note())
            } else {
                (0, block.len_chars(), block.note())
            };
            let text = block.text_rope().slice(from..to).to_string();
            let runs = runs::slice_runs(block.runs(), from, to);
            blocks.push(FragmentBlock {
                kind: block.kind().clone(),
                text,
                runs,
                note,
            });
        }
        Ok(Fragment { blocks })
    }

    /// Copy the selection and delete it.
    pub fn cut(&mut self) -> Result<Fragment, ModelError> {
        let fragment = self.copy()?;
        if fragment.is_empty() {
            return Ok(fragment);
        }
        self.transact(|book, changes| {
            let (start, end) = book.normalized()?;
            book.delete_range(start, end, changes)
        })?;
        Ok(fragment)
    }

    /// Insert a fragment, replacing the selection. Kinds of blocks after the
    /// first are kept. The first block merges into the block at the caret;
    /// when that block is empty, it takes the fragment's kind and note.
    pub fn paste(&mut self, fragment: &Fragment) -> Result<(), ModelError> {
        if fragment.is_empty() {
            return Ok(());
        }
        self.transact(|book, changes| {
            if !book.selection.is_collapsed() {
                let (start, end) = book.normalized()?;
                book.delete_range(start, end, changes)?;
            }
            let at = book.selection.focus;
            if book.block_len(at.block)? == 0
                && let Some(first) = fragment.blocks.first()
            {
                book.set_kind_change(at.block, first.kind.clone(), changes)?;
                if let Some(note) = first.note {
                    book.set_note_change(at.block, Some(note), changes)?;
                }
            }
            let inherited = {
                let block = book.block_ref(at.block)?;
                runs::marks_before(block.runs(), at.offset)
            };
            let pieces = fragment
                .blocks
                .iter()
                .map(|block| Piece {
                    text: block.text.clone(),
                    runs: block.runs.clone(),
                    kind: Some(block.kind.clone()),
                    note: block.note,
                })
                .collect::<Vec<_>>();
            book.insert_pieces(at, &pieces, &inherited, changes)
        })
    }

    /// [`Self::insert`] of plain text. Paragraph breaks become blocks.
    pub fn paste_text(&mut self, text: &str) -> Result<(), ModelError> {
        self.insert(text)
    }

    /// Reverse the latest transaction and remember it for [`Self::redo`].
    pub fn undo(&mut self) -> Result<(), ModelError> {
        let Some(txn) = self.undo.pop() else {
            return Err(ModelError::NothingToUndo);
        };
        for change in txn.changes.iter().rev() {
            if let Err(err) = self.apply(change, true) {
                self.undo.push(txn);
                return Err(err);
            }
        }
        self.selection = txn.selection_before;
        self.redo.push(txn);
        self.check_consistency()
    }

    /// Reapply the latest undone transaction.
    pub fn redo(&mut self) -> Result<(), ModelError> {
        let Some(txn) = self.redo.pop() else {
            return Err(ModelError::NothingToRedo);
        };
        for change in &txn.changes {
            if let Err(err) = self.apply(change, false) {
                self.redo.push(txn);
                return Err(err);
            }
        }
        self.selection = txn.selection_after;
        self.undo.push(txn);
        self.check_consistency()
    }

    /// Anchor a new footnote or endnote to the focus block.
    ///
    /// The note is a slot on the block, not a character in the rope. The body
    /// is stored on the book. Undo removes the note and restores the previous slot.
    pub fn attach_note(&mut self, kind: NoteKind, body: &str) -> Result<NoteId, ModelError> {
        let block = self.selection.focus.block;
        self.ensure_position(self.selection.focus)?;
        let id = self.ids.note();
        let note = Note::from_text(id, kind, body);
        self.transact(|book, changes| {
            let create = Change::CreateNote { note: note.clone() };
            book.apply(&create, false)?;
            changes.push(create);
            book.set_note_change(block, Some(id), changes)
        })?;
        Ok(id)
    }

    /// Change a block's kind. Recorded as its own undo step.
    pub fn set_kind(&mut self, block: BlockId, kind: BlockKind) -> Result<(), ModelError> {
        let _ = self.block_ref(block)?;
        self.transact(|book, changes| book.set_kind_change(block, kind, changes))
    }

    fn insert_text(&mut self, text: &str, marks: Option<RunMarks>) -> Result<(), ModelError> {
        self.transact(|book, changes| {
            if !book.selection.is_collapsed() {
                let (start, end) = book.normalized()?;
                book.delete_range(start, end, changes)?;
            }
            let at = book.selection.focus;
            let inherited = if let Some(marks) = marks {
                marks
            } else {
                let block = book.block_ref(at.block)?;
                runs::marks_before(block.runs(), at.offset)
            };
            let pieces = split_lines(text)
                .into_iter()
                .map(|segment| Piece {
                    runs: if segment.is_empty() {
                        Vec::new()
                    } else {
                        runs::covering(runs::char_count(&segment), inherited.clone())
                    },
                    text: segment,
                    kind: None,
                    note: None,
                })
                .collect::<Vec<_>>();
            book.insert_pieces(at, &pieces, &inherited, changes)
        })
    }

    fn transact<F>(&mut self, body: F) -> Result<(), ModelError>
    where
        F: FnOnce(&mut Self, &mut Vec<Change>) -> Result<(), ModelError>,
    {
        let before = self.selection;
        let mut changes = Vec::new();
        if let Err(err) = body(self, &mut changes) {
            for change in changes.iter().rev() {
                let _ = self.apply(change, true);
            }
            self.selection = before;
            return Err(err);
        }
        if !changes.is_empty() {
            self.redo.clear();
            self.undo.push(Transaction {
                changes,
                selection_before: before,
                selection_after: self.selection,
            });
        }
        self.check_consistency()
    }

    fn motion_origin(&self, motion: Motion) -> Result<Position, ModelError> {
        if self.selection.is_collapsed() {
            return Ok(self.selection.focus);
        }
        let (start, end) = self.normalized()?;
        let forward = match motion {
            Motion::Char(direction) | Motion::Word(direction) | Motion::Block(direction) => {
                direction == Direction::Forward
            }
        };
        Ok(if forward { end } else { start })
    }

    fn position_after(&self, pos: Position, motion: Motion) -> Result<Position, ModelError> {
        self.ensure_position(pos)?;
        let text = self.block_ref(pos.block)?.text();
        match motion {
            Motion::Char(Direction::Forward) => {
                let next = nav::next_grapheme(&text, pos.offset);
                if next > pos.offset {
                    Ok(Position::new(pos.block, next))
                } else {
                    self.step_block(pos, Direction::Forward, false)
                }
            }
            Motion::Char(Direction::Backward) => {
                if pos.offset == 0 {
                    self.step_block(pos, Direction::Backward, true)
                } else {
                    Ok(Position::new(
                        pos.block,
                        nav::prev_grapheme(&text, pos.offset),
                    ))
                }
            }
            Motion::Word(Direction::Forward) => match nav::word_forward(&text, pos.offset) {
                Some(offset) => Ok(Position::new(pos.block, offset)),
                None => self.step_block(pos, Direction::Forward, false),
            },
            Motion::Word(Direction::Backward) => match nav::word_backward(&text, pos.offset) {
                Some(offset) => Ok(Position::new(pos.block, offset)),
                None => self.step_block(pos, Direction::Backward, true),
            },
            Motion::Block(Direction::Backward) => {
                if pos.offset > 0 {
                    Ok(Position::new(pos.block, 0))
                } else {
                    self.step_block(pos, Direction::Backward, false)
                }
            }
            Motion::Block(Direction::Forward) => self.step_block(pos, Direction::Forward, false),
        }
    }

    /// `to_end` lands on the last character boundary of the neighbor; otherwise
    /// on its first.
    fn step_block(
        &self,
        pos: Position,
        direction: Direction,
        to_end: bool,
    ) -> Result<Position, ModelError> {
        let Some(id) = self.neighbor(pos.block, direction)? else {
            return Ok(pos);
        };
        let offset = if to_end { self.block_len(id)? } else { 0 };
        Ok(Position::new(id, offset))
    }

    fn normalized(&self) -> Result<(Position, Position), ModelError> {
        let anchor = self.selection.anchor;
        let focus = self.selection.focus;
        self.ensure_position(anchor)?;
        self.ensure_position(focus)?;
        if self.cmp_pos(anchor, focus)? == Ordering::Greater {
            Ok((focus, anchor))
        } else {
            Ok((anchor, focus))
        }
    }

    fn delete_range(
        &mut self,
        start: Position,
        end: Position,
        changes: &mut Vec<Change>,
    ) -> Result<(), ModelError> {
        self.ensure_position(start)?;
        self.ensure_position(end)?;
        if self.cmp_pos(start, end)? == Ordering::Greater {
            return Err(ModelError::EditConflict);
        }
        if start == end {
            self.selection = Selection::collapsed(start);
            return Ok(());
        }
        if start.block == end.block {
            let delete_len = end.offset - start.offset;
            self.replace_span(start.block, start.offset, delete_len, "", &[], changes)?;
            self.selection = Selection::collapsed(start);
            return Ok(());
        }

        let (tail_text, tail_runs) = {
            let block = self.block_ref(end.block)?;
            let len = block.len_chars();
            let text = block.text_rope().slice(end.offset..len).to_string();
            let sliced = runs::slice_runs(block.runs(), end.offset, len);
            (text, sliced)
        };
        let tail_len = runs::char_count(&tail_text);
        if tail_len > 0 && !runs::runs_cover(&tail_runs, tail_len) {
            return Err(ModelError::InvalidRuns);
        }
        let ids = self.ids_from_to(start.block, end.block)?;
        let start_len = self.block_len(start.block)?;
        if start.offset < start_len {
            self.replace_span(
                start.block,
                start.offset,
                start_len - start.offset,
                "",
                &[],
                changes,
            )?;
        }
        for id in ids.iter().skip(1).rev() {
            self.remove_block(*id, changes)?;
        }
        if tail_len > 0 {
            let at = self.block_len(start.block)?;
            self.replace_span(start.block, at, 0, &tail_text, &tail_runs, changes)?;
        }
        self.selection = Selection::collapsed(start);
        Ok(())
    }

    fn insert_pieces(
        &mut self,
        at: Position,
        pieces: &[Piece],
        inherited: &RunMarks,
        changes: &mut Vec<Change>,
    ) -> Result<(), ModelError> {
        self.ensure_position(at)?;
        if pieces.is_empty() {
            self.selection = Selection::collapsed(at);
            return Ok(());
        }
        if pieces.len() == 1 {
            let piece = &pieces[0];
            if !piece.text.is_empty() {
                let piece_runs = resolve_runs(piece, inherited)?;
                self.replace_span(at.block, at.offset, 0, &piece.text, &piece_runs, changes)?;
            }
            self.selection = Selection::collapsed(Position::new(
                at.block,
                at.offset + runs::char_count(&piece.text),
            ));
            return Ok(());
        }

        let (tail_text, tail_runs) = {
            let block = self.block_ref(at.block)?;
            let len = block.len_chars();
            let text = block.text_rope().slice(at.offset..len).to_string();
            let sliced = runs::slice_runs(block.runs(), at.offset, len);
            (text, sliced)
        };
        let tail_len = runs::char_count(&tail_text);
        if tail_len > 0 && !runs::runs_cover(&tail_runs, tail_len) {
            return Err(ModelError::InvalidRuns);
        }
        let block_len = self.block_len(at.block)?;
        if at.offset < block_len {
            self.replace_span(at.block, at.offset, block_len - at.offset, "", &[], changes)?;
        }
        let first = &pieces[0];
        if !first.text.is_empty() {
            let piece_runs = resolve_runs(first, inherited)?;
            self.replace_span(at.block, at.offset, 0, &first.text, &piece_runs, changes)?;
        }

        let mut after = at.block;
        let mut caret_block = at.block;
        let mut caret_offset = at.offset + runs::char_count(&first.text);
        let last = pieces.len() - 1;
        for (index, piece) in pieces.iter().enumerate().skip(1) {
            let piece_len = runs::char_count(&piece.text);
            let mut text = piece.text.clone();
            let mut piece_runs = resolve_runs(piece, inherited)?;
            if index == last && tail_len > 0 {
                let base = runs::char_count(&text);
                text.push_str(&tail_text);
                for run in &tail_runs {
                    piece_runs.push(Run::span(
                        run.start + base,
                        run.end + base,
                        run.marks.clone(),
                    ));
                }
                piece_runs = runs::coalesce(piece_runs, inherited.clone());
            }
            if text.is_empty() {
                piece_runs = runs::covering(0, inherited.clone());
            } else if !runs::runs_cover(&piece_runs, runs::char_count(&text)) {
                return Err(ModelError::InvalidRuns);
            }
            let kind = piece.kind.clone().unwrap_or_default();
            let id = self.ids.block();
            let mut block = Block::new(id, kind, &text, piece_runs);
            if let Some(note) = piece.note {
                block.set_note(Some(note));
            }
            self.insert_block_after(after, block, changes)?;
            after = id;
            caret_block = id;
            caret_offset = piece_len;
        }
        self.selection = Selection::collapsed(Position::new(caret_block, caret_offset));
        Ok(())
    }

    fn replace_span(
        &mut self,
        block: BlockId,
        start: usize,
        delete_len: usize,
        insert: &str,
        insert_runs: &[Run],
        changes: &mut Vec<Change>,
    ) -> Result<(), ModelError> {
        if delete_len == 0 && insert.is_empty() {
            return Ok(());
        }
        let current = self.block_ref(block)?;
        let len = current.len_chars();
        let Some(end) = start.checked_add(delete_len) else {
            return Err(ModelError::EditConflict);
        };
        if end > len {
            return Err(ModelError::OffsetOutOfRange {
                block,
                offset: end,
                len,
            });
        }
        let old = current.text_rope().slice(start..end).to_string();
        let old_runs = current.runs().to_vec();
        let new_runs = runs::rewrite_runs(&old_runs, start, delete_len, insert_runs)?;
        let new_len = len - delete_len + runs::char_count(insert);
        if !runs::runs_cover(&new_runs, new_len) {
            return Err(ModelError::InvalidRuns);
        }
        let change = Change::ReplaceText {
            block,
            start,
            old,
            new: insert.to_string(),
            old_runs,
            new_runs,
        };
        self.apply(&change, false)?;
        changes.push(change);
        Ok(())
    }

    fn remove_block(&mut self, id: BlockId, changes: &mut Vec<Change>) -> Result<(), ModelError> {
        let loc = self.loc(id)?;
        let block = self.block_ref(id)?.clone();
        let change = Change::RemoveBlock { loc, block };
        self.apply(&change, false)?;
        changes.push(change);
        Ok(())
    }

    fn insert_block_after(
        &mut self,
        after: BlockId,
        block: Block,
        changes: &mut Vec<Change>,
    ) -> Result<(), ModelError> {
        let loc = self.loc(after)?;
        let new_loc = BlockLoc {
            block: loc.block + 1,
            ..loc
        };
        let change = Change::InsertBlock {
            loc: new_loc,
            block,
        };
        self.apply(&change, false)?;
        changes.push(change);
        Ok(())
    }

    fn set_kind_change(
        &mut self,
        block: BlockId,
        kind: BlockKind,
        changes: &mut Vec<Change>,
    ) -> Result<(), ModelError> {
        let old = self.block_ref(block)?.kind().clone();
        if old == kind {
            return Ok(());
        }
        let change = Change::SetKind {
            block,
            old,
            new: kind,
        };
        self.apply(&change, false)?;
        changes.push(change);
        Ok(())
    }

    fn set_note_change(
        &mut self,
        block: BlockId,
        note: Option<NoteId>,
        changes: &mut Vec<Change>,
    ) -> Result<(), ModelError> {
        if let Some(id) = note
            && self.notes().get(&id).is_none()
        {
            return Err(ModelError::UnknownNote(id));
        }
        let old = self.block_ref(block)?.note();
        if old == note {
            return Ok(());
        }
        let change = Change::SetNote {
            block,
            old,
            new: note,
        };
        self.apply(&change, false)?;
        changes.push(change);
        Ok(())
    }

    fn ids_from_to(&self, start: BlockId, end: BlockId) -> Result<Vec<BlockId>, ModelError> {
        let ids = self.block_ids();
        let start_index = ids
            .iter()
            .position(|id| *id == start)
            .ok_or(ModelError::UnknownBlock(start))?;
        let end_index = ids
            .iter()
            .position(|id| *id == end)
            .ok_or(ModelError::UnknownBlock(end))?;
        if start_index > end_index {
            return Err(ModelError::EditConflict);
        }
        Ok(ids[start_index..=end_index].to_vec())
    }

    fn apply(&mut self, change: &Change, backward: bool) -> Result<(), ModelError> {
        match change {
            Change::ReplaceText {
                block,
                start,
                old,
                new,
                old_runs,
                new_runs,
            } => {
                let (from, to, runs) = if backward {
                    (new.as_str(), old.as_str(), old_runs.clone())
                } else {
                    (old.as_str(), new.as_str(), new_runs.clone())
                };
                let target = self.block_mut(*block)?;
                rope_replace(target.text_rope_mut(), *start, from, to)?;
                *target.runs_mut() = runs;
                Ok(())
            }
            Change::InsertBlock { loc, block } => {
                if backward {
                    self.remove_at(*loc, block.id())
                } else {
                    self.insert_at(*loc, block)
                }
            }
            Change::RemoveBlock { loc, block } => {
                if backward {
                    self.insert_at(*loc, block)
                } else {
                    self.remove_at(*loc, block.id())
                }
            }
            Change::SetKind { block, old, new } => {
                let expected = if backward { new } else { old };
                let assign = if backward { old.clone() } else { new.clone() };
                let current = self.block_ref(*block)?.kind().clone();
                if &current != expected {
                    return Err(ModelError::EditConflict);
                }
                self.block_mut(*block)?.set_kind(assign);
                Ok(())
            }
            Change::SetNote { block, old, new } => {
                let expected = if backward { new } else { old };
                let assign = if backward { *old } else { *new };
                let current = self.block_ref(*block)?.note();
                if &current != expected {
                    return Err(ModelError::EditConflict);
                }
                self.block_mut(*block)?.set_note(assign);
                Ok(())
            }
            Change::CreateNote { note } => {
                let id = note.id();
                if backward {
                    if self.notes().get(&id).is_none() {
                        return Err(ModelError::EditConflict);
                    }
                    self.notes_mut().remove(&id);
                } else {
                    if self.notes().contains_key(&id) {
                        return Err(ModelError::EditConflict);
                    }
                    self.notes_mut().insert(id, note.clone());
                }
                Ok(())
            }
        }
    }

    fn insert_at(&mut self, loc: BlockLoc, block: &Block) -> Result<(), ModelError> {
        {
            let section = self.section_mut(loc)?;
            if loc.block > section.blocks().len() {
                return Err(ModelError::EditConflict);
            }
            section.blocks_mut().insert(loc.block, block.clone());
        }
        self.reindex();
        Ok(())
    }

    fn remove_at(&mut self, loc: BlockLoc, expected: BlockId) -> Result<(), ModelError> {
        {
            let section = self.section_mut(loc)?;
            match section.blocks().get(loc.block) {
                Some(existing) if existing.id() == expected => {}
                _ => return Err(ModelError::EditConflict),
            }
            section.blocks_mut().remove(loc.block);
        }
        self.reindex();
        Ok(())
    }
}

fn resolve_runs(piece: &Piece, inherited: &RunMarks) -> Result<Vec<Run>, ModelError> {
    if piece.text.is_empty() {
        return Ok(Vec::new());
    }
    let runs = if piece.runs.is_empty() {
        runs::covering(runs::char_count(&piece.text), inherited.clone())
    } else {
        piece.runs.clone()
    };
    if runs::runs_cover(&runs, runs::char_count(&piece.text)) {
        Ok(runs)
    } else {
        Err(ModelError::InvalidRuns)
    }
}

fn rope_replace(
    rope: &mut ropey::Rope,
    start: usize,
    from: &str,
    to: &str,
) -> Result<(), ModelError> {
    let from_len = from.chars().count();
    let Some(end) = start.checked_add(from_len) else {
        return Err(ModelError::EditConflict);
    };
    if end > rope.len_chars() || rope.slice(start..end) != from {
        return Err(ModelError::EditConflict);
    }
    if from_len > 0 {
        rope.remove(start..end);
    }
    if !to.is_empty() {
        rope.insert(start, to);
    }
    Ok(())
}

fn split_lines(text: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            normalized.push('\n');
            if chars.peek() == Some(&'\n') {
                let _ = chars.next();
            }
        } else {
            normalized.push(ch);
        }
    }
    if !normalized.contains('\n') {
        return vec![normalized];
    }
    normalized.split('\n').map(ToString::to_string).collect()
}
