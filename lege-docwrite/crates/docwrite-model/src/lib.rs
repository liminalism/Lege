//! Canonical book model for lege-docwrite.
//!
//! The semantic model is the document. Pages, lines and glyph positions are
//! derived later by `docwrite-typeset` and are not stored here.
//!
//! The tree is `Book → Part → Chapter → Section → Block → Run`, with stable
//! block ids. Each block's text is a [`ropey::Rope`]. Insert, delete,
//! selection, word motion, cut/copy/paste and undo/redo are transactions.
//! A block can hold one footnote or endnote id; the note body lives on the book.
//!
//! Named paragraph styles, page masters and chapter templates are a later
//! milestone. Runs already carry direct marks (bold, italic, small caps,
//! script, language, link, citation) so those systems have something to address.

mod bundle;
mod edit;
mod error;
mod ids;
mod nav;
mod publish;
mod runs;
mod tree;

pub use edit::{Direction, Fragment, Mark, Motion};
pub use error::ModelError;
pub use ids::{BlockId, ChapterId, NoteId, PartId, SectionId};
pub use publish::{
    Align, BibliographyEntry, ChapterStart, ChapterTemplate, IndexTerm, PageMaster, ParagraphStyle,
    SourceNote,
};
pub use runs::{Run, RunMarks, Script};
pub use tree::{
    Block, BlockKind, Book, Chapter, Note, NoteKind, Part, Position, Section, Selection,
};
