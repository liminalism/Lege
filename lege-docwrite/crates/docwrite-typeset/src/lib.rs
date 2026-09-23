//! Typesetting for lege-docwrite.
//!
//! Planned: shaping (harfrust), outlines and metrics (skrifa), line breaking
//! (unicode-linebreak), bidi, hyphenation (hypher), and incremental
//! pagination. After an edit, pagination proceeds forward from the changed
//! paragraph only until page geometry converges with the cached layout, so a
//! keystroke on page 147 costs pages 147–148, not the whole book.
//!
//! This crate is a scaffold; see the project ledger (`.akr/`) for scope.
