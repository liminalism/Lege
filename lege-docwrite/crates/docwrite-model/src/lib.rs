//! Canonical book model for lege-docwrite.
//!
//! The semantic model is the document; pages are derived from it by
//! `docwrite-typeset` and never stored. The planned shape:
//!
//! - `Book → Part → Chapter → Section → Block → Run`, with stable block IDs
//!   so layout caches, undo and research links survive edits and reordering.
//! - Blocks: body paragraph, chapter title, subhead, block quote, epigraph,
//!   image, caption, verse, bibliography entry, section break, and a
//!   footnote reference slot from the start.
//! - Runs carry emphasis, small caps, language, link and citation refs.
//! - Paragraph/character styles, page masters and chapter templates are
//!   three separate systems; a chapter refers to a template, so changing the
//!   template changes every chapter that uses it.
//! - Edits are transactions with undo/redo.
//!
//! This crate is a scaffold; see the project ledger (`.akr/`) for scope.
