//! Typesetting for lege-docwrite.
//!
//! Text is shaped with harfrust, measured with skrifa, broken with
//! unicode-linebreak, and paginated incrementally: after an edit, only the
//! pages from the edited paragraph forward are rebuilt, and only until a
//! page break lands on the same block and line as the cached layout.

mod atlas;
mod book;
mod engine;
mod error;

pub use atlas::{AtlasKey, GlyphAtlas, GlyphBitmap};
pub use book::{
    CHAPTER_TITLE_ID, CONTENTS_ID, ENDNOTES_ID, from_book, from_book_with, update_from_book,
};
pub use engine::{
    Alignment, Cursor, Document, EditReport, Face, FaceStyle, FootnoteLine, Geometry, Glyph,
    HYPHEN_CLUSTER, NOTE_MARK_CLUSTER, NoteLine, PaintedLine, Paragraph, ParagraphStyle, StyledRun,
    book_of_pages, book_of_repeated_line, features_for,
};
pub use error::TypesetError;
