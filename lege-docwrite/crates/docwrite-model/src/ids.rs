//! Stable identity for the book tree.
//!
//! Identifiers are allocated by the book and never reused, so a block id
//! remains valid across edits, undo and reordering.

/// A part in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartId(u64);

/// A chapter inside a part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChapterId(u64);

/// A section inside a chapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectionId(u64);

/// A block of text. Layout caches and note links key on this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(u64);

/// A footnote or endnote body stored on the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NoteId(u64);

macro_rules! id_impl {
    ($ty:ty, $label:literal) => {
        impl $ty {
            pub(crate) fn new(raw: u64) -> Self {
                Self(raw)
            }
        }

        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}:{}", $label, self.0)
            }
        }
    };
}

id_impl!(PartId, "part");
id_impl!(ChapterId, "chapter");
id_impl!(SectionId, "section");
id_impl!(BlockId, "block");
id_impl!(NoteId, "note");

#[derive(Clone, Debug)]
pub(crate) struct IdGen {
    next: u64,
}

impl IdGen {
    pub(crate) fn new() -> Self {
        Self { next: 1 }
    }

    fn alloc(&mut self) -> u64 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }

    pub(crate) fn part(&mut self) -> PartId {
        PartId::new(self.alloc())
    }

    pub(crate) fn chapter(&mut self) -> ChapterId {
        ChapterId::new(self.alloc())
    }

    pub(crate) fn section(&mut self) -> SectionId {
        SectionId::new(self.alloc())
    }

    pub(crate) fn block(&mut self) -> BlockId {
        BlockId::new(self.alloc())
    }

    pub(crate) fn note(&mut self) -> NoteId {
        NoteId::new(self.alloc())
    }
}
