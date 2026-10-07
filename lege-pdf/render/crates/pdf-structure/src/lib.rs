//! Document structure: cross-reference data, trailers, revisions, object
//! streams, and structural recovery.
//!
//! Everything produced here is **built once during document open and
//! immutable afterwards** (concurrency plan §"Eager immutable structural
//! index"). Workers never touch a mutable xref.

use std::sync::Arc;

use pdf_object::{Dictionary, ObjectId};

pub mod decode;
mod loader;
pub mod objstm;
pub mod predictor;

pub use loader::{StructureLimits, load_structure, load_structure_rebuilt, scan_for_object_header};

/// Where an object's bytes can be found, after all revisions are merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectLocation {
    /// Uncompressed object at a byte offset in the source.
    Offset(u64),
    /// Compressed member of an object stream.
    InObjectStream {
        /// Object number of the containing `/Type /ObjStm` stream.
        container: u32,
        /// Zero-based index within the container.
        index: u32,
    },
    /// Free entry (or object shadowed by a free entry in a later revision).
    Free,
}

/// One resolved cross-reference entry.
#[derive(Debug, Clone, Copy)]
pub struct XrefEntry {
    pub id: ObjectId,
    pub location: ObjectLocation,
}

/// The merged cross-reference map for the *current* view of the document:
/// all incremental updates applied, later revisions shadowing earlier ones.
/// Indexed by object number for O(1) lookup; dense because object numbers
/// are, in practice, dense.
#[derive(Debug)]
pub struct XrefMap {
    /// `entries[n]` is the location for object number `n`.
    entries: Box<[ObjectLocation]>,
    /// Generation currently live for each object number (0 for entries in
    /// object streams, per spec).
    generations: Box<[u16]>,
}

impl XrefMap {
    pub fn from_parts(entries: Box<[ObjectLocation]>, generations: Box<[u16]>) -> Self {
        assert_eq!(entries.len(), generations.len());
        Self {
            entries,
            generations,
        }
    }

    pub fn locate(&self, id: ObjectId) -> ObjectLocation {
        let n = id.number as usize;
        if n >= self.entries.len() || self.generations[n] != id.generation {
            return ObjectLocation::Free;
        }
        self.entries[n]
    }

    /// Iterate the current live object view in ascending object-number order.
    ///
    /// This intentionally exposes resolved identities rather than the private
    /// parallel storage. Recovery consumers occasionally need to inspect every
    /// indexed object (including object-stream members), while generation and
    /// free-entry handling must remain centralized here.
    pub fn live_entries(&self) -> impl Iterator<Item = XrefEntry> + '_ {
        self.entries
            .iter()
            .copied()
            .zip(self.generations.iter().copied())
            .enumerate()
            .filter_map(|(number, (location, generation))| {
                (!matches!(location, ObjectLocation::Free)).then_some(XrefEntry {
                    id: ObjectId::new(number as u32, generation),
                    location,
                })
            })
    }

    /// Highest object number + 1 (the trailer's `/Size`, possibly repaired).
    pub fn size(&self) -> u32 {
        self.entries.len() as u32
    }
}

/// How one xref section was encoded in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefSectionKind {
    /// A classic `xref` table followed by a `trailer` dictionary.
    Classic,
    /// A `/Type /XRef` cross-reference stream (its own dictionary is the
    /// trailer).
    Stream,
}

/// One cross-reference entry exactly as one xref section declared it: the
/// object identity and where that section claims the object lives.
///
/// Unlike [`XrefEntry`] — one entry of the merged, all-updates-applied map —
/// these are never shadowed: a section keeps every entry it declared, even
/// when a newer section redefines the same object. That is what lets audit
/// tooling diff sections into per-object change histories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionXrefEntry {
    /// Object identity (number and generation) as declared by the section.
    pub id: ObjectId,
    /// Where this section claims the object lives.
    pub location: ObjectLocation,
}

/// How many entries of each location kind one xref section declared.
///
/// Unlike [`XrefSection::entries`] — the entries themselves, materialized only
/// on the audit path — these counters are recorded on every load: four integer
/// increments cost nothing, and the facts layer reports counts, never entries.
/// Rebuild-synthesized sections carry all zeros: a full-file scan has no
/// section structure to count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XrefEntryCounts {
    /// Every entry the section declared.
    pub total: u32,
    /// Entries claiming an uncompressed object at a file offset.
    pub live: u32,
    /// Entries claiming an object-stream member.
    pub compressed: u32,
    /// Free entries.
    pub free: u32,
}

impl XrefEntryCounts {
    /// Tally one declared entry by where the section claims the object lives.
    pub(crate) fn tally(&mut self, location: ObjectLocation) {
        self.total = self.total.saturating_add(1);
        match location {
            ObjectLocation::Offset(_) => self.live = self.live.saturating_add(1),
            ObjectLocation::InObjectStream { .. } => {
                self.compressed = self.compressed.saturating_add(1);
            }
            ObjectLocation::Free => self.free = self.free.saturating_add(1),
        }
    }
}

/// One xref section walked while opening the document: a classic table, an
/// xref stream, or — with [`XrefSection::offset`] zero — a trailer
/// synthesized by a full-file rebuild.
///
/// Sections are stored newest first (chain order). They are *not* logical
/// revisions: one user-visible save can hold two sections (a hybrid-reference
/// classic table plus its auxiliary `/XRefStm` stream), and linearized files
/// carry stub sections that are not evidence of a later edit. Grouping
/// sections into logical revisions is the audit layer's job, never a raw
/// section count.
#[derive(Debug, Clone)]
pub struct XrefSection {
    /// Header-relative file offset of this section (the `startxref`/`/Prev`
    /// value that led here). Zero marks a section synthesized by a full-file
    /// rebuild, which has no source offset — a real section can never sit at
    /// zero because the `%PDF` header lives there.
    pub offset: u64,
    /// How the section was encoded.
    pub kind: XrefSectionKind,
    /// How many entries this section declared, by location kind. Recorded on
    /// both the fast and the audit path; rebuild-synthesized sections carry
    /// zeros.
    pub entry_counts: XrefEntryCounts,
    /// Every entry this section declared, in section order — including
    /// entries a newer section shadows. Materialized only on the audit path
    /// (see [`StructureLimits::preserve_section_entries`]); empty on
    /// the fast render path. Rebuild-synthesized sections (`offset == 0`)
    /// never carry entries: a full-file scan merges objects directly,
    /// without section structure.
    pub entries: Arc<[RevisionXrefEntry]>,
    /// The trailer dictionary of this section: a classic `trailer`, or the
    /// xref stream's own dictionary.
    pub trailer: Arc<Dictionary>,
    /// The `/Prev` link this section declares, when any.
    pub prev: Option<u64>,
    /// For a hybrid-reference `/XRefStm` stream: the index into
    /// [`DocumentStructure::sections`] of the classic table it extends.
    /// `None` for primary sections.
    pub auxiliary_of: Option<u32>,
    /// Recovery events attributable to parsing this section. A subset of the
    /// snapshot-wide [`DocumentStructure::recovery`] log, which additionally
    /// carries chain-level events (cycles, truncated chains, rebuilds) that
    /// belong to no single section.
    pub recovery: Arc<[RecoveryEvent]>,
}

/// The resolved trailer view of the current document.
#[derive(Debug, Clone)]
pub struct Trailer {
    pub dict: Arc<Dictionary>,
    pub root: ObjectId,
    pub info: Option<ObjectId>,
    pub encrypt: Option<ObjectId>,
    /// The two-element /ID byte strings, if present (needed for encryption
    /// key derivation and cache keying).
    pub file_id: Option<[pdf_object::PdfString; 2]>,
}

/// Non-fatal deviations tolerated (and repaired) during structure loading.
/// Recorded on the snapshot so corpus/differential tooling can observe what
/// recovery ran — repairs must be visible, not silent (blueprint §4.5).
#[derive(Debug, Clone)]
pub enum RecoveryEvent {
    /// `startxref` missing or pointing outside the file; recovered by scan.
    StartXrefRecovered { reported: Option<u64>, used: u64 },
    /// An xref entry's offset didn't land on the declared object; object was
    /// found elsewhere by scanning.
    ObjectOffsetRepaired {
        id: ObjectId,
        declared: u64,
        actual: u64,
    },
    /// A stream /Length was wrong; actual length determined by scanning for
    /// `endstream`.
    StreamLengthRepaired {
        id: ObjectId,
        declared: Option<i64>,
        actual: u64,
    },
    /// A malformed indirect reference omitted its generation (`N R`);
    /// generation zero was supplied.
    ReferenceGenerationRepaired {
        id: ObjectId,
        referenced: u32,
        offset: u64,
    },
    /// Full-file reconstruction: xref unusable, objects indexed by scanning
    /// for `N G obj` headers (PDFium's `RebuildCrossRef` equivalent).
    XrefRebuilt,
    /// Trailer /Size disagreed with actual entries.
    SizeRepaired { declared: i64, actual: u32 },
    /// Other tolerated deviation, described for logs.
    Other(String),
}

/// Fatal structural errors: the document could not be brought to a usable
/// state even with recovery.
#[derive(Debug, Clone, thiserror::Error)]
pub enum StructureError {
    #[error("not a PDF: header not found")]
    NoHeader,
    #[error("no usable cross-reference information (recovery failed)")]
    NoUsableXref,
    #[error("trailer has no /Root catalog reference")]
    NoCatalog,
    #[error("cross-reference data forms a cycle at offset {0}")]
    XrefCycle(u64),
    #[error("structural limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error(transparent)]
    Syntax(#[from] pdf_syntax::SyntaxError),
    #[error(transparent)]
    Source(#[from] pdf_source::SourceError),
    #[error(transparent)]
    Decode(#[from] decode::DecodeError),
}

/// Output of the document-open structural phase, consumed by
/// `pdf-document` to build the snapshot.
#[derive(Debug)]
pub struct DocumentStructure {
    pub version: PdfVersion,
    /// Bytes of leading garbage before `%PDF`. Every structural offset
    /// (xref entries, `XrefSection::offset`) is relative to the header,
    /// i.e. the *source* position is `header_offset + structural offset`
    /// (PDFium's `header_offset_` convention).
    pub header_offset: u64,
    pub xref: XrefMap,
    pub trailer: Trailer,
    /// Xref sections walked while opening, newest first (see [`XrefSection`]
    /// for ordering and what counts as a section).
    pub sections: Box<[XrefSection]>,
    pub recovery: Box<[RecoveryEvent]>,
}

/// PDF version from the header (possibly overridden by the catalog's
/// /Version).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PdfVersion {
    pub major: u8,
    pub minor: u8,
}

// `load_structure` (see `loader`) covers: header scan (incl. leading-garbage
// tolerance), startxref, classic xref tables, xref streams (/W decoding),
// hybrid files (/XRefStm), /Prev chains with cycle detection, and the
// rebuild scan. Object-stream layouts are in `objstm`; filter decoding in
// `decode`; predictors in `predictor`.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn xref_map_shadows_by_generation() {
        let map = XrefMap::from_parts(
            vec![ObjectLocation::Free, ObjectLocation::Offset(15)].into_boxed_slice(),
            vec![0, 0].into_boxed_slice(),
        );
        assert_eq!(map.locate(ObjectId::new(1, 0)), ObjectLocation::Offset(15));
        // Stale generation → treated as free.
        assert_eq!(map.locate(ObjectId::new(1, 1)), ObjectLocation::Free);
        // Out of range → free.
        assert_eq!(map.locate(ObjectId::new(9, 0)), ObjectLocation::Free);
    }

    #[test]
    fn live_entries_skip_free_objects_and_preserve_generations() {
        let map = XrefMap::from_parts(
            vec![
                ObjectLocation::Free,
                ObjectLocation::Offset(15),
                ObjectLocation::InObjectStream {
                    container: 9,
                    index: 2,
                },
            ]
            .into_boxed_slice(),
            vec![u16::MAX, 4, 0].into_boxed_slice(),
        );
        let entries: Vec<_> = map.live_entries().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, ObjectId::new(1, 4));
        assert_eq!(entries[0].location, ObjectLocation::Offset(15));
        assert_eq!(entries[1].id, ObjectId::new(2, 0));
        assert_eq!(
            entries[1].location,
            ObjectLocation::InObjectStream {
                container: 9,
                index: 2
            }
        );
    }
}
