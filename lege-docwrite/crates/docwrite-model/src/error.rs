//! Failures a caller can recover from. Library paths do not panic.

use crate::{BlockId, ChapterId, NoteId, PartId, SectionId};
use std::fmt;

/// A refused or inconsistent edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    /// No block has this id.
    UnknownBlock(BlockId),
    /// No part has this id.
    UnknownPart(PartId),
    /// No chapter has this id.
    UnknownChapter(ChapterId),
    /// No section has this id.
    UnknownSection(SectionId),
    /// No note has this id.
    UnknownNote(NoteId),
    /// `offset` is past the block's character length.
    OffsetOutOfRange {
        /// The block the offset was applied to.
        block: BlockId,
        /// The rejected character offset.
        offset: usize,
        /// The block's character length.
        len: usize,
    },
    /// Runs do not cover the text they were applied to.
    InvalidRuns,
    /// Undo was asked for and the stack is empty.
    NothingToUndo,
    /// Redo was asked for and the stack is empty.
    NothingToRedo,
    /// An undo or redo step did not match the book. The manuscript was left
    /// as it was before that step where possible.
    EditConflict,
    /// A tree invariant failed. The static text names the check.
    Inconsistent(&'static str),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBlock(id) => write!(f, "unknown {id}"),
            Self::UnknownPart(id) => write!(f, "unknown {id}"),
            Self::UnknownChapter(id) => write!(f, "unknown {id}"),
            Self::UnknownSection(id) => write!(f, "unknown {id}"),
            Self::UnknownNote(id) => write!(f, "unknown {id}"),
            Self::OffsetOutOfRange { block, offset, len } => {
                write!(f, "offset {offset} is past the end of {block} (len {len})")
            }
            Self::InvalidRuns => write!(f, "runs do not cover the block text"),
            Self::NothingToUndo => write!(f, "nothing to undo"),
            Self::NothingToRedo => write!(f, "nothing to redo"),
            Self::EditConflict => write!(f, "edit history did not match the book"),
            Self::Inconsistent(check) => write!(f, "book is inconsistent: {check}"),
        }
    }
}

impl std::error::Error for ModelError {}
