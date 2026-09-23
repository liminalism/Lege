//! Typesetting for lege-docwrite.
//!
//! Text is shaped with harfrust, measured with skrifa, broken with
//! unicode-linebreak, and paginated incrementally: after an edit, only the
//! pages from the edited paragraph forward are rebuilt, and only until a
//! page break lands on the same block and line as the cached layout.

mod atlas;
mod engine;
mod error;

pub use atlas::{AtlasKey, GlyphAtlas};
pub use engine::{
    book_of_pages, book_of_repeated_line, features_for, Cursor, Document, EditReport, Face, Geometry, Glyph, PaintedLine,
    Paragraph, ParagraphStyle,
};
pub use error::TypesetError;
