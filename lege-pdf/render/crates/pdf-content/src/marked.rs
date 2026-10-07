//! Neutral marked-content inventory (ISO 32000-1 §14.6).
//!
//! A marked-content span is a `BMC`/`BDC`…`EMC` range carrying a tag name.
//! Most tags are structural (`/Span`, `/P`, `/OC`) and of no interest here,
//! but interactive editors leave their own: Acrobat's Edit Text tool wraps
//! re-typed runs in `/TouchUp_TextEdit` (and related `/TouchUp_*` tags).
//! This module records every span neutrally — tag, position, and enclosed
//! text bounds — without judging what any tag means. Deciding which tags
//! count as edit markers is the consumer's job.

use pdf_page_ir::Rect;

/// One `BMC`/`BDC`…`EMC` marked-content span from a page content stream.
///
/// Neutral facts only: the tag as written, where the span sits in the
/// decoded content bytes, and the user-space bounds of the text shown
/// inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkedContentSpan {
    /// Tag name as written, without the leading slash (e.g.
    /// `"TouchUp_TextEdit"`).
    pub tag: String,
    /// Byte offset of the opening `BMC`/`BDC` keyword in the page's
    /// concatenated decoded content stream (see
    /// [`PageCompiler::marked_content_spans`](crate::PageCompiler::marked_content_spans)).
    pub open_offset: usize,
    /// Byte offset of the `EMC` keyword closing the span in that same
    /// stream, or `None` when the stream ended with the span still open.
    pub close_offset: Option<usize>,
    /// Union, in user space, of the advance-derived boxes of the text runs
    /// shown directly by the page content stream inside the span, or `None`
    /// when the span enclosed no text runs.
    ///
    /// Approximate by construction: each run contributes the rectangle from
    /// its origin to its total advance, `font_size` tall, mapped through
    /// the run's text matrix and the CTM — glyph outlines are not consulted,
    /// and text painted by nested content streams (form XObjects, annotation
    /// appearances) is excluded.
    pub bbox: Option<Rect>,
}
