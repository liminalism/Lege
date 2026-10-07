//! `pdf-read` — a qpdf-style **document doctor**, read-only over the
//! existing machinery.
//!
//! [`examine`] opens a document (tolerating failure), probes structure,
//! linearization, encryption, per-page compilability, annotations, and
//! feature flags, and returns a [`DocumentReport`]. Nothing here parses PDF
//! syntax itself:
//! every byte is read through `pdf-structure`/`pdf-document` resolvers, and
//! page content through `pdf-content`'s compiler. No new parsers.
//!
//! The report never panics and never fails: a document that cannot even be
//! structurally loaded still yields a report whose `open` is
//! [`OpenOutcome::Failed`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use pdf_content::PageCompiler;
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_object::{Dictionary, NameId, NameTable, ObjectId, PdfObject, decode_text_string_lossy};
use pdf_source::PdfSource;
use pdf_structure::{ObjectLocation, RecoveryEvent};

mod byterange;
mod metrics;
mod xmp;
pub use byterange::{
    ByteRangeAssessment, ByteRangeSpan, ByteRangeValidity, assess as assess_byte_range,
};
pub use metrics::{FontRecord, ImageRecord, PageMetrics};
pub use xmp::{XmpHistoryEvent, XmpLineage};

/// How the document opened.
#[derive(Debug, Clone)]
pub enum OpenOutcome {
    /// Clean open: no recovery of any kind was needed.
    Ok,
    /// Opened, but only after structural repair; `how` lists each repair.
    Recovered { how: Vec<String> },
    /// Could not be opened; the typed error, rendered.
    Failed { error: String },
}

/// What the `/Encrypt` dictionary declares (plus whether the empty user
/// password opens the file).
#[derive(Debug, Clone)]
pub struct EncryptionInfo {
    /// `/V` — algorithm version (0 when unreadable).
    pub version: i64,
    /// `/R` — standard-handler revision (0 when unreadable).
    pub revision: i64,
    /// Human name of the method ("RC4-40", "RC4-128", "AES-128", "AES-256",
    /// or "unknown").
    pub method: String,
    /// Whether the empty user password genuinely validates against `/U`
    /// (the common permissions-only encryption) — computed via
    /// `StandardHandler::verify_password(enc, "")`, independent of how (or
    /// whether) the document actually opened.
    pub user_password_empty: bool,
}

/// The `%PDF` header, and how far into the file it was found.
#[derive(Debug, Clone)]
pub struct HeaderInfo {
    /// `/Version` major from the header, e.g. 1 in `%PDF-1.7`.
    pub major: u8,
    /// `/Version` minor from the header, e.g. 7 in `%PDF-1.7`.
    pub minor: u8,
    /// Bytes of leading garbage before `%PDF`. Zero for a well-formed file;
    /// non-zero means every structural offset in the file is shifted by this
    /// much relative to the source.
    pub header_offset: u64,
}

/// Identity and provenance the document declares about itself.
///
/// Neutral facts only: these are what the file *says*, with no judgement about
/// whether it is telling the truth. The trailer `/ID` pair is the interesting
/// one — its first element is meant to persist across a document's whole edit
/// history while the second changes on each save, so the pair carries lineage
/// that survives independently of any metadata a writer chose to update.
#[derive(Debug, Clone, Default)]
pub struct DocumentIdentity {
    /// Lowercase hex of the permanent trailer `/ID[0]`, if present.
    pub id_permanent: Option<String>,
    /// Lowercase hex of the changing trailer `/ID[1]`, if present.
    pub id_changing: Option<String>,
    /// `/Info` dictionary entries with string values, by key. Includes the
    /// conventional `/Producer`, `/Creator`, `/CreationDate` and `/ModDate`,
    /// and any non-standard keys a writer added, which are often the more
    /// telling ones.
    pub info: BTreeMap<String, String>,
    /// Whether the catalog carries an XMP `/Metadata` stream.
    pub has_xmp_metadata: bool,
    /// The `xmpMM` media-management properties read out of that stream, when
    /// it is present, decodable, and carries any.
    ///
    /// This is the lineage the trailer `/ID` pair only hints at:
    /// `xmpMM:DocumentID` is meant to survive a document's entire edit
    /// history while `xmpMM:InstanceID` changes on every save, and
    /// `xmpMM:DerivedFrom` names the instance a file was derived from
    /// outright.
    pub xmp: Option<XmpLineage>,
}

/// One signature field and what it covers.
#[derive(Debug, Clone)]
pub struct SignatureInfo {
    /// The field's `/T` name, if it has one.
    pub field_name: Option<String>,
    /// Backing field dictionary, when the field is (as usual) an indirect
    /// reference; `None` for a direct inline field dictionary.
    pub field_object: Option<ObjectId>,
    /// The `/V` signature value dictionary, when indirect; `None` for a
    /// direct inline value.
    pub value_object: Option<ObjectId>,
    /// `/Filter` — the handler that produced the signature, e.g. `Adobe.PPKLite`.
    pub filter: Option<String>,
    /// `/SubFilter` — the signature encoding, e.g. `ETSI.CAdES.detached`.
    pub sub_filter: Option<String>,
    /// `/M` — the claimed signing time, verbatim and unparsed.
    pub signed_at: Option<String>,
    /// `/ByteRange` integer entries, verbatim and in order. Conventionally
    /// four numbers: two offset/length pairs straddling the signature
    /// itself. Non-integer entries are not representable here; their
    /// presence makes [`Self::byte_range_validity`] malformed instead.
    pub byte_range: Vec<i64>,
    /// Whether `/ByteRange` spans the entire file — that is, starts at zero
    /// and its last pair ends at the final byte.
    ///
    /// A signature covering less than the whole file is the structural half of
    /// the classic incremental-update attack: content appended after the
    /// signed range is not signed, however valid the cryptography over the
    /// range turns out to be. This says nothing about whether the signature
    /// verifies; that is a separate, later question.
    pub covers_whole_file: bool,
    /// Strict classification of `/ByteRange` against the original file
    /// bytes: shape, ordering, and bounds. See [`assess_byte_range`].
    pub byte_range_validity: ByteRangeValidity,
    /// The claimed covered runs, in `/ByteRange` order. Empty when the
    /// shape does not parse as offset/length pairs.
    pub covered_ranges: Vec<ByteRangeSpan>,
    /// Why the range is not valid. Always `None` for valid ranges.
    pub byte_range_detail: Option<String>,
    /// The DocMDP certification policy from the signature's `/Reference`
    /// array, when one of its entries declares
    /// `/TransformMethod /DocMDP`. `None` for an approval signature, which
    /// carries no document lock.
    pub docmdp: Option<DocMdpInfo>,
    /// The FieldMDP lock policy from the signature's `/Reference` array,
    /// when one of its entries declares `/TransformMethod /FieldMDP`.
    pub fieldmdp: Option<FieldMdpInfo>,
}

/// A DocMDP certification policy, exactly as the signature reference states
/// it: what later changes the signer permits.
#[derive(Debug, Clone)]
pub struct DocMdpInfo {
    /// Verbatim `/P` from the DocMDP transform parameters: 1 (no changes),
    /// 2 (form filling and signing), or 3 (additionally annotations).
    /// `None` when `/P` is absent or not an integer.
    pub p: Option<i64>,
}

/// A FieldMDP lock policy, exactly as the signature reference states it:
/// which form fields the signer locked.
#[derive(Debug, Clone)]
pub struct FieldMdpInfo {
    /// Verbatim `/P` from the FieldMDP transform parameters, when present.
    pub p: Option<i64>,
    /// Verbatim `/Action`: `All`, `Include`, or `Exclude`. `None` when
    /// absent or not a name.
    pub action: Option<String>,
    /// Verbatim `/Fields`: the qualified field names the action applies to.
    /// `None` when `/Fields` is absent or not an array; an empty array
    /// reads as `Some` with no names, which is a degenerate but parseable
    /// policy rather than a missing one.
    pub fields: Option<Vec<String>>,
}

/// Signature fields found in the `/AcroForm`.
#[derive(Debug, Clone, Default)]
pub struct SignatureInventory {
    /// Every `/FT /Sig` field, in `/Fields` order.
    pub signatures: Vec<SignatureInfo>,
    /// Signature fields present but carrying no `/V` signature dictionary —
    /// an unsigned signature field, which is ordinary in a blank form.
    pub unsigned_fields: u32,
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

/// How many entries of each location kind one xref section declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// Where one xref section entry claims its object lives, as neutral facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntryLocation {
    /// An uncompressed object at this header-relative file offset.
    Offset(u64),
    /// A compressed member of an object stream.
    InObjectStream {
        /// Object number of the containing `/Type /ObjStm` stream.
        container: u32,
        /// Zero-based index within the container.
        index: u32,
    },
    /// A free entry.
    Free,
}

/// One cross-reference entry exactly as one xref section declared it: the
/// object identity and where that section claims the object lives.
///
/// Unlike the merged xref map — where later revisions shadow earlier ones —
/// these are never shadowed: a section keeps every entry it declared, even
/// when a newer section redefines the same object. That is what lets audit
/// tooling diff sections into per-object change histories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrefEntryFacts {
    /// Object identity (number and generation) as declared by the section.
    pub id: ObjectId,
    /// Where this section claims the object lives.
    pub location: XrefEntryLocation,
}

/// One xref section walked while opening the document, as neutral facts.
#[derive(Debug, Clone)]
pub struct XrefSectionFacts {
    /// Header-relative file offset of this section (the `startxref`/`/Prev`
    /// value that led here). Zero marks a section synthesized by a full-file
    /// rebuild, which has no source offset.
    pub offset: u64,
    /// How the section was encoded.
    pub kind: XrefSectionKind,
    /// How many entries the section declared, by location kind.
    /// Rebuild-synthesized sections carry zeros.
    pub entry_counts: XrefEntryCounts,
    /// Every entry this section declared, in section order — including
    /// entries a newer section shadows. Empty only for rebuild-synthesized
    /// sections, which have no section structure to preserve.
    pub entries: Vec<XrefEntryFacts>,
    /// The `/Prev` link this section declares, when any.
    pub prev: Option<u64>,
    /// For a hybrid-reference `/XRefStm` stream: the index into the
    /// [`XrefHealth::sections`] vector (same newest-first order) of the
    /// classic table it extends. `None` for primary sections.
    pub auxiliary_of: Option<u32>,
    /// Recovery events attributable to parsing this section, rendered. A
    /// subset of the snapshot-wide recovery log, which additionally carries
    /// chain-level events that belong to no single section.
    pub recovery: Vec<String>,
}

/// Cross-reference health.
#[derive(Debug, Clone, Default)]
pub struct XrefHealth {
    /// Any structural repair ran during open.
    pub recovery_used: bool,
    /// The xref was rebuilt by a full object scan (the heaviest repair).
    pub rebuilt: bool,
    /// Number of xref sections walked (classic tables plus xref streams,
    /// including auxiliary `/XRefStm` sections and synthetic rebuild
    /// sections). A structural count, not a count of user edits: one save
    /// can hold two sections, and stub sections are not edits.
    pub revision_count: usize,
    /// The walked sections themselves, newest first (chain order). Grouping
    /// them into logical revisions is the consumer's job, never a raw count.
    pub sections: Vec<XrefSectionFacts>,
}

/// Per-page compile outcome.
#[derive(Debug, Clone)]
pub enum CompileStatus {
    Ok {
        op_count: usize,
    },
    /// Compiled, but only with lazy repairs along the way.
    Degraded {
        op_count: usize,
        detail: String,
    },
    Failed {
        error: String,
    },
}

/// One marked-content span from a page content stream: the tag as written,
/// where the span sits in the decoded content bytes, and the user-space
/// bounds of the text shown inside it.
///
/// Neutral facts only — every tag is reported, structural (`/Span`, `/OC`)
/// or otherwise; deciding which tags count as edit markers is the
/// consumer's job.
#[derive(Debug, Clone)]
pub struct MarkedContentRecord {
    /// Tag name as written, without the leading slash (e.g.
    /// `"TouchUp_TextEdit"`).
    pub tag: String,
    /// Byte offset of the opening `BMC`/`BDC` keyword in the page's
    /// concatenated decoded content stream.
    pub open_offset: usize,
    /// Byte offset of the closing `EMC` keyword in that same stream, or
    /// `None` when the stream ended with the span still open.
    pub close_offset: Option<usize>,
    /// Union of the enclosed text runs' boxes in user-space points,
    /// `[x0, y0, x1, y1]`, or `None` when the span enclosed no text runs.
    /// Advance-derived and approximate; see
    /// [`pdf_content::MarkedContentSpan`].
    pub bbox: Option<[f64; 4]>,
}

/// One page's health.
#[derive(Debug, Clone)]
pub struct PageStatus {
    pub index: u32,
    /// Whether a `/MediaBox` is actually present on the page or an ancestor
    /// (as opposed to defaulted during page-tree flattening).
    pub media_box_present: bool,
    pub compile: CompileStatus,
    /// Representation metrics from the compiled semantic page. `None` when
    /// compilation failed, so a failed page cannot be mistaken for a blank one.
    pub metrics: Option<PageMetrics>,
    /// Marked-content spans from a second interpretation pass over the same
    /// content. Empty when the page carries no marked content, or when the
    /// marker pass itself failed — the compile status above carries any
    /// failure, so an empty list beside [`CompileStatus::Failed`] is a gap,
    /// not a clean bill.
    pub marked_content: Vec<MarkedContentRecord>,
}

/// One annotation and the identity it declares about itself.
///
/// Neutral facts only: these are what the annotation dictionary *says*, with
/// no judgement about whether it is telling the truth.
#[derive(Debug, Clone)]
pub struct AnnotationRecord {
    /// Zero-based page index carrying this annotation.
    pub page: u32,
    /// Backing annotation dictionary, when the `/Annots` entry is (as the
    /// spec requires) an indirect reference; `None` for a direct inline
    /// annotation dictionary.
    pub object: Option<ObjectId>,
    /// `/Subtype`, e.g. `Text`. `None` when absent or unreadable.
    pub subtype: Option<String>,
    /// `/Contents` text, decoded lossy.
    pub contents: Option<String>,
    /// `/NM` annotation name: the writer-assigned identifier meant to be
    /// unique within the document.
    pub name: Option<String>,
    /// `/M` last-modification date, verbatim and unparsed. A writer's claim,
    /// not an observation.
    pub modified: Option<String>,
    /// `/CreationDate`, verbatim and unparsed. Like [`Self::modified`], a
    /// claim by whatever wrote the annotation.
    pub creation_date: Option<String>,
    /// `/T` text label — for markup annotations, the author. Verbatim.
    pub author: Option<String>,
    /// `/Subj` subject: a short description of the annotation. Verbatim.
    pub subject: Option<String>,
    /// `/IT` intent name (e.g. `FreeTextTypewriter`), as authored.
    pub intent: Option<String>,
}

/// Count of annotations per `/Subtype`, plus the identity each declares.
#[derive(Debug, Clone, Default)]
pub struct AnnotationInventory {
    pub count_per_subtype: BTreeMap<String, u32>,
    /// Every annotation in page order, with the identity it declares.
    pub annotations: Vec<AnnotationRecord>,
}

impl AnnotationInventory {
    pub fn total(&self) -> u32 {
        self.count_per_subtype.values().sum()
    }
}

/// Document-level feature flags an embedder cares about.
#[derive(Debug, Clone, Copy, Default)]
pub struct FeatureFlags {
    pub has_acroform: bool,
    pub has_xfa: bool,
    /// Document-level JavaScript (`/Names /JavaScript` name tree).
    pub has_javascript: bool,
    pub has_outlines: bool,
    pub has_optional_content: bool,
    pub uses_object_streams: bool,
    /// The name tree carries `/EmbeddedFiles` — the document has file
    /// attachments.
    pub has_embedded_files: bool,
}

/// The full examination result.
#[derive(Debug, Clone)]
pub struct DocumentReport {
    pub open: OpenOutcome,
    /// The `%PDF` header. `None` only when no header could be found at all,
    /// which is also the reason the document failed to open.
    pub header: Option<HeaderInfo>,
    /// Whether the file is linearized (fast-web-view): its first object is
    /// a valid `/Linearized` parameter dictionary. `false` for every
    /// ordinary file, and for files with no readable structure at all.
    pub linearized: bool,
    pub encryption: Option<EncryptionInfo>,
    pub xref: XrefHealth,
    /// What the document declares about its own identity and provenance.
    pub identity: DocumentIdentity,
    pub pages: Vec<PageStatus>,
    pub annotations: AnnotationInventory,
    /// Signature fields and the byte ranges they cover.
    pub signatures: SignatureInventory,
    pub features: FeatureFlags,
    /// Distinct font resources referenced by any compiled page.
    pub fonts: Vec<FontRecord>,
    /// Distinct image XObjects (plus every inline image) referenced by any
    /// compiled page.
    pub images: Vec<ImageRecord>,
}

/// Cap on `/Parent` hops when walking inherited attributes (cycle guard).
const MAX_PARENT_HOPS: usize = 64;

/// Examine `source` and report. Never fails, never panics: an unopenable
/// document yields a mostly-empty report with `open: Failed`.
pub fn examine(source: Arc<dyn PdfSource>, limits: DocumentLimits) -> DocumentReport {
    examine_with_password(source, limits, None)
}

/// [`examine`] with an optional user/owner password for encrypted documents
/// (threaded into [`DocumentSnapshot::open_with_password`]).
pub fn examine_with_password(
    source: Arc<dyn PdfSource>,
    limits: DocumentLimits,
    password: Option<&str>,
) -> DocumentReport {
    // The doctor is audit tooling: per-section entries are retained so
    // consumers can diff sections into per-object change histories. The
    // render path keeps the default off.
    let mut audit_limits = limits.clone();
    audit_limits.preserve_section_entries = true;
    match DocumentSnapshot::open_with_password(Arc::clone(&source), audit_limits, password) {
        Ok(snapshot) => examine_open(&snapshot),
        Err(err) => examine_failed(source, limits, err.to_string()),
    }
}

/// The happy path: the snapshot opened (possibly with recovery).
fn examine_open(snapshot: &DocumentSnapshot) -> DocumentReport {
    let recovery = snapshot.recovery_events();
    let open = if recovery.is_empty() {
        OpenOutcome::Ok
    } else {
        OpenOutcome::Recovered {
            how: recovery.iter().map(describe_recovery).collect(),
        }
    };
    let xref = XrefHealth {
        recovery_used: !recovery.is_empty(),
        rebuilt: recovery
            .iter()
            .any(|e| matches!(e, RecoveryEvent::XrefRebuilt)),
        revision_count: snapshot.structure().sections.len(),
        sections: translate_sections(&snapshot.structure().sections),
    };

    let mut ctx = ParseContext::new();
    let encryption = read_encryption(snapshot, &mut ctx);
    let linearized = detect_linearized(snapshot, &mut ctx);
    let (pages, fonts, images) = examine_pages(snapshot);
    let annotations = inventory_annotations(snapshot, &mut ctx);
    let features = read_features(snapshot, &mut ctx);
    let identity = read_identity(snapshot, &mut ctx);
    let signatures = inventory_signatures(snapshot, &mut ctx);

    DocumentReport {
        open,
        header: Some(read_header(snapshot.structure())),
        linearized,
        encryption,
        xref,
        identity,
        pages,
        annotations,
        signatures,
        features,
        fonts,
        images,
    }
}

/// Open failed: retry the structural layer alone so the report can still
/// carry xref health and the declared encryption scheme (e.g. a document
/// whose password we don't have).
fn examine_failed(
    source: Arc<dyn PdfSource>,
    limits: DocumentLimits,
    error: String,
) -> DocumentReport {
    let mut report = DocumentReport {
        open: OpenOutcome::Failed { error },
        header: None,
        linearized: false,
        encryption: None,
        xref: XrefHealth::default(),
        identity: DocumentIdentity::default(),
        pages: Vec::new(),
        annotations: AnnotationInventory::default(),
        signatures: SignatureInventory::default(),
        features: FeatureFlags::default(),
        fonts: Vec::new(),
        images: Vec::new(),
    };

    let names = NameTable::new();
    let structure_limits = pdf_structure::StructureLimits {
        syntax: limits.syntax.clone(),
        max_revisions: limits.max_revisions,
        max_objects: limits.max_objects,
        max_decoded_bytes: limits.max_decoded_bytes_per_context,
        // The failed-open path still reports per-section entries: revision
        // membership is structural, and a structure-only audit needs it.
        preserve_section_entries: true,
    };
    let Ok(structure) = pdf_structure::load_structure(source.as_ref(), &names, &structure_limits)
    else {
        return report;
    };
    report.xref = XrefHealth {
        recovery_used: !structure.recovery.is_empty(),
        rebuilt: structure
            .recovery
            .iter()
            .any(|e| matches!(e, RecoveryEvent::XrefRebuilt)),
        revision_count: structure.sections.len(),
        sections: translate_sections(&structure.sections),
    };
    report.features.uses_object_streams = xref_uses_object_streams(&structure.xref);
    report.header = Some(read_header(&structure));
    // The trailer `/ID` pair is never encrypted, so lineage survives even for
    // a document we cannot open. `/Info` does not: its strings are encrypted
    // like any others, so it is left to the open path.
    read_file_id(&structure.trailer, &mut report.identity);

    // A security-less snapshot over the loaded structure lets the existing
    // resolver read the (never-encrypted) /Encrypt dictionary for us.
    let snapshot = DocumentSnapshot::from_parts(
        source,
        structure,
        names,
        pdf_document::PageTreeIndex::default(),
        None,
        limits,
    );
    let mut ctx = ParseContext::new();
    report.encryption = read_encryption(&snapshot, &mut ctx);
    // The first object needs no decryption to identify (and a linearization
    // parameter dictionary is itself never encrypted), so this is determined
    // even for a document that would not open.
    report.linearized = detect_linearized(&snapshot, &mut ctx);
    report
}

fn describe_recovery(event: &RecoveryEvent) -> String {
    match event {
        RecoveryEvent::StartXrefRecovered { reported, used } => match reported {
            Some(r) => format!("startxref pointed at {r}; recovered chain at {used}"),
            None => format!("startxref missing; recovered chain at {used}"),
        },
        RecoveryEvent::ObjectOffsetRepaired {
            id,
            declared,
            actual,
        } => {
            format!("object {id} xref offset {declared} wrong; found at {actual}")
        }
        RecoveryEvent::StreamLengthRepaired {
            id,
            declared,
            actual,
        } => {
            format!("stream {id} /Length {declared:?} wrong; actual {actual}")
        }
        RecoveryEvent::ReferenceGenerationRepaired {
            id,
            referenced,
            offset,
        } => {
            format!(
                "object {id} reference {referenced} R at offset {offset} omitted generation; assumed 0"
            )
        }
        RecoveryEvent::XrefRebuilt => "cross-reference table rebuilt by full object scan".into(),
        RecoveryEvent::SizeRepaired { declared, actual } => {
            format!("trailer /Size {declared} wrong; actual {actual}")
        }
        RecoveryEvent::Other(s) => s.clone(),
    }
}

/// The walked xref sections as neutral facts, in the structure layer's
/// newest-first order.
fn translate_sections(sections: &[pdf_structure::XrefSection]) -> Vec<XrefSectionFacts> {
    sections
        .iter()
        .map(|section| XrefSectionFacts {
            offset: section.offset,
            kind: match section.kind {
                pdf_structure::XrefSectionKind::Classic => XrefSectionKind::Classic,
                pdf_structure::XrefSectionKind::Stream => XrefSectionKind::Stream,
            },
            entry_counts: XrefEntryCounts {
                total: section.entry_counts.total,
                live: section.entry_counts.live,
                compressed: section.entry_counts.compressed,
                free: section.entry_counts.free,
            },
            entries: section
                .entries
                .iter()
                .map(|entry| XrefEntryFacts {
                    id: entry.id,
                    location: match entry.location {
                        ObjectLocation::Offset(offset) => XrefEntryLocation::Offset(offset),
                        ObjectLocation::InObjectStream { container, index } => {
                            XrefEntryLocation::InObjectStream { container, index }
                        }
                        ObjectLocation::Free => XrefEntryLocation::Free,
                    },
                })
                .collect(),
            prev: section.prev,
            auxiliary_of: section.auxiliary_of,
            recovery: section.recovery.iter().map(describe_recovery).collect(),
        })
        .collect()
}

// ── helpers over the resolver ───────────────────────────────────────────────

/// Resolve `id` through the snapshot, mapping any failure to `None`.
fn resolve(
    snapshot: &DocumentSnapshot,
    id: ObjectId,
    ctx: &mut ParseContext,
) -> Option<Arc<PdfObject>> {
    snapshot.objects().resolve(snapshot, id, ctx).ok()
}

/// Resolve a possibly-indirect value to its target object.
fn resolve_value(
    snapshot: &DocumentSnapshot,
    value: &PdfObject,
    ctx: &mut ParseContext,
) -> Option<Arc<PdfObject>> {
    match value {
        PdfObject::Reference(id) => resolve(snapshot, *id, ctx),
        other => Some(Arc::new(other.clone())),
    }
}

/// `dict[key]`, resolved if indirect.
fn dict_get_resolved(
    snapshot: &DocumentSnapshot,
    dict: &Dictionary,
    key: NameId,
    ctx: &mut ParseContext,
) -> Option<Arc<PdfObject>> {
    resolve_value(snapshot, dict.get(key)?, ctx)
}

// ── header and identity ─────────────────────────────────────────────────────

fn read_header(structure: &pdf_structure::DocumentStructure) -> HeaderInfo {
    HeaderInfo {
        major: structure.version.major,
        minor: structure.version.minor,
        header_offset: structure.header_offset,
    }
}

// ── linearization ──────────────────────────────────────────────────────────

/// Whether the file is linearized: its first object is a valid `/Linearized`
/// parameter dictionary.
///
/// The linearization parameter dictionary is required to be the first object
/// in the file, so the xref's lowest-offset direct object is the only
/// candidate. It must resolve — through the existing resolver, like every
/// other read here — to a dictionary carrying `/Linearized`. Anything else
/// (no direct objects, an unresolvable first object, no marker) is an
/// ordinary file. A `/Linearized` entry anywhere but the first object does
/// not count.
fn detect_linearized(snapshot: &DocumentSnapshot, ctx: &mut ParseContext) -> bool {
    let first = snapshot
        .structure()
        .xref
        .live_entries()
        .filter_map(|entry| match entry.location {
            ObjectLocation::Offset(offset) => Some((offset, entry.id)),
            ObjectLocation::InObjectStream { .. } | ObjectLocation::Free => None,
        })
        .min_by_key(|(offset, _)| *offset);
    let Some((_, id)) = first else {
        return false;
    };
    let Some(object) = resolve(snapshot, id, ctx) else {
        return false;
    };
    let Some(dict) = object.as_dict() else {
        return false;
    };
    let key = snapshot.names().intern(b"Linearized");
    dict.contains_key(key)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// The trailer `/ID` pair, which needs no decryption.
fn read_file_id(trailer: &pdf_structure::Trailer, identity: &mut DocumentIdentity) {
    if let Some([permanent, changing]) = trailer.file_id.as_ref() {
        identity.id_permanent = Some(hex(permanent.as_bytes()));
        identity.id_changing = Some(hex(changing.as_bytes()));
    }
}

fn read_identity(snapshot: &DocumentSnapshot, ctx: &mut ParseContext) -> DocumentIdentity {
    let mut identity = DocumentIdentity::default();
    let structure = snapshot.structure();
    read_file_id(&structure.trailer, &mut identity);

    let names = snapshot.names();

    // Every /Info entry with a string value, standard or not. Non-standard
    // keys are kept deliberately: a writer that adds its own is identifying
    // itself as surely as /Producer does.
    if let Some(info_id) = structure.trailer.info
        && let Some(info) = resolve(snapshot, info_id, ctx)
        && let Some(info) = info.as_dict()
    {
        for (key, value) in info.iter() {
            let Some(value) = resolve_value(snapshot, value, ctx) else {
                continue;
            };
            let PdfObject::String(text) = value.as_ref() else {
                continue;
            };
            let key = String::from_utf8_lossy(&names.resolve(key)).into_owned();
            identity
                .info
                .insert(key, String::from_utf8_lossy(text.as_bytes()).into_owned());
        }
    }

    let root = structure.trailer.root;
    if let Some(catalog) = resolve(snapshot, root, ctx)
        && let Some(catalog) = catalog.as_dict()
        && let Some(metadata_key) = names.lookup(b"Metadata")
        && let Some(metadata) = dict_get_resolved(snapshot, catalog, metadata_key, ctx)
        && !matches!(metadata.as_ref(), PdfObject::Null)
    {
        identity.has_xmp_metadata = true;
        // The packet is a stream like any other; an undecodable one leaves
        // `xmp` as None while `has_xmp_metadata` still records that it exists,
        // so "no XMP" and "XMP we could not read" stay distinguishable.
        if let PdfObject::Stream(stream) = metadata.as_ref()
            && let Ok(packet) = snapshot.decode_stream_data(stream, ctx)
        {
            let lineage = xmp::parse(&packet);
            if !lineage.is_empty() {
                identity.xmp = Some(lineage);
            }
        }
    }

    identity
}

// ── signatures ──────────────────────────────────────────────────────────────

/// Walk `/AcroForm` `/Fields` for `/FT /Sig` fields and read what each covers.
///
/// Reads the signature dictionaries only; nothing here validates cryptography.
/// The `/Contents` blob is deliberately not extracted — structural coverage
/// comes first, and the cryptographic layer is a separate concern.
fn inventory_signatures(snapshot: &DocumentSnapshot, ctx: &mut ParseContext) -> SignatureInventory {
    let mut inventory = SignatureInventory::default();
    let names = snapshot.names();

    let root = snapshot.structure().trailer.root;
    let Some(catalog) = resolve(snapshot, root, ctx) else {
        return inventory;
    };
    let Some(catalog) = catalog.as_dict() else {
        return inventory;
    };
    let Some(acroform_key) = names.lookup(b"AcroForm") else {
        return inventory;
    };
    let Some(acroform) = dict_get_resolved(snapshot, catalog, acroform_key, ctx) else {
        return inventory;
    };
    let Some(acroform) = acroform.as_dict() else {
        return inventory;
    };
    let Some(fields_key) = names.lookup(b"Fields") else {
        return inventory;
    };
    let Some(fields) = dict_get_resolved(snapshot, acroform, fields_key, ctx) else {
        return inventory;
    };

    let source_len = snapshot.source().len();
    let mut seen = 0usize;
    collect_signature_fields(
        snapshot,
        &fields,
        None,
        source_len,
        &mut inventory,
        &mut seen,
        ctx,
    );
    inventory
}

/// Cap on fields visited, so a cyclic or adversarial `/Kids` graph cannot make
/// the doctor run away.
const MAX_FORM_FIELDS: usize = 65_536;

fn collect_signature_fields(
    snapshot: &DocumentSnapshot,
    node: &PdfObject,
    field_id: Option<ObjectId>,
    source_len: u64,
    inventory: &mut SignatureInventory,
    seen: &mut usize,
    ctx: &mut ParseContext,
) {
    if *seen >= MAX_FORM_FIELDS {
        return;
    }
    let names = snapshot.names();

    if let PdfObject::Array(entries) = node {
        for entry in entries.iter() {
            let Some(field) = resolve_value(snapshot, entry, ctx) else {
                continue;
            };
            collect_signature_fields(
                snapshot,
                &field,
                entry.as_reference(),
                source_len,
                inventory,
                seen,
                ctx,
            );
        }
        return;
    }

    let Some(field) = node.as_dict() else {
        return;
    };
    *seen += 1;

    // A field's type may be inherited from its parent, but a signature field
    // that declares nothing is not one we can identify; read what is here.
    let is_signature = names
        .lookup(b"FT")
        .and_then(|k| field.get(k))
        .and_then(PdfObject::as_name)
        .is_some_and(|n| names.resolve(n).as_ref() == b"Sig");

    if is_signature {
        let value_id = names
            .lookup(b"V")
            .and_then(|k| field.get(k))
            .and_then(PdfObject::as_reference);
        match names
            .lookup(b"V")
            .and_then(|k| dict_get_resolved(snapshot, field, k, ctx))
            .filter(|v| !matches!(v.as_ref(), PdfObject::Null))
        {
            Some(value) => {
                if let Some(sig) = value.as_dict() {
                    inventory.signatures.push(read_signature(
                        snapshot, field, field_id, sig, value_id, source_len, ctx,
                    ));
                }
            }
            None => inventory.unsigned_fields += 1,
        }
    }

    // Descend into /Kids for nested field trees.
    if let Some(kids) = names
        .lookup(b"Kids")
        .and_then(|k| dict_get_resolved(snapshot, field, k, ctx))
    {
        collect_signature_fields(snapshot, &kids, None, source_len, inventory, seen, ctx);
    }
}

fn read_signature(
    snapshot: &DocumentSnapshot,
    field: &Dictionary,
    field_id: Option<ObjectId>,
    sig: &Dictionary,
    value_id: Option<ObjectId>,
    source_len: u64,
    ctx: &mut ParseContext,
) -> SignatureInfo {
    let names = snapshot.names();

    let text = |dict: &Dictionary, key: &[u8], ctx: &mut ParseContext| -> Option<String> {
        let k = names.lookup(key)?;
        let value = dict_get_resolved(snapshot, dict, k, ctx)?;
        match value.as_ref() {
            PdfObject::String(s) => Some(String::from_utf8_lossy(s.as_bytes()).into_owned()),
            PdfObject::Name(n) => Some(String::from_utf8_lossy(&names.resolve(*n)).into_owned()),
            _ => None,
        }
    };

    // Every entry in order, `None` for one that is not an integer: the
    // strict assessment needs to see non-integers, not have them filtered.
    let entries: Vec<Option<i64>> = names
        .lookup(b"ByteRange")
        .and_then(|k| dict_get_resolved(snapshot, sig, k, ctx))
        .and_then(|v| match v.as_ref() {
            PdfObject::Array(entries) => Some(
                entries
                    .iter()
                    .map(|e| resolve_value(snapshot, e, ctx).and_then(|resolved| resolved.as_int()))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let byte_range: Vec<i64> = entries.iter().filter_map(|entry| *entry).collect();
    // Offsets in `/ByteRange` are absolute source positions, so the
    // assessment compares against the source length rather than the
    // post-header length.
    let assessment = byterange::assess(&entries, source_len);

    let (docmdp, fieldmdp) = read_transform_policies(snapshot, sig, ctx);

    SignatureInfo {
        field_name: text(field, b"T", ctx),
        field_object: field_id,
        value_object: value_id,
        filter: text(sig, b"Filter", ctx),
        sub_filter: text(sig, b"SubFilter", ctx),
        signed_at: text(sig, b"M", ctx),
        byte_range,
        covers_whole_file: assessment.validity == ByteRangeValidity::Valid,
        byte_range_validity: assessment.validity,
        covered_ranges: assessment.covered,
        byte_range_detail: assessment.detail,
        docmdp,
        fieldmdp,
    }
}

/// The DocMDP and FieldMDP policies from a signature value dictionary's
/// `/Reference` array: the first `/DocMDP` entry's and the first
/// `/FieldMDP` entry's transform parameters, each resolved whether direct
/// or indirect. Anything else in the array — timestamps, `/UR` entries,
/// unresolvable elements — is not a document lock and is skipped.
fn read_transform_policies(
    snapshot: &DocumentSnapshot,
    sig: &Dictionary,
    ctx: &mut ParseContext,
) -> (Option<DocMdpInfo>, Option<FieldMdpInfo>) {
    let names = snapshot.names();
    let mut docmdp = None;
    let mut fieldmdp = None;
    let Some(reference_key) = names.lookup(b"Reference") else {
        return (None, None);
    };
    let Some(reference) = dict_get_resolved(snapshot, sig, reference_key, ctx) else {
        return (None, None);
    };
    let PdfObject::Array(entries) = reference.as_ref() else {
        return (None, None);
    };
    for entry in entries.iter() {
        let Some(sig_ref) = resolve_value(snapshot, entry, ctx) else {
            continue;
        };
        let Some(sig_ref) = sig_ref.as_dict() else {
            continue;
        };
        let method = names
            .lookup(b"TransformMethod")
            .and_then(|key| sig_ref.get(key))
            .and_then(PdfObject::as_name)
            .map(|name| names.resolve(name));
        let Some(method) = method else {
            continue;
        };
        if method.as_ref() == b"DocMDP" && docmdp.is_none() {
            docmdp = Some(DocMdpInfo {
                p: read_docmdp_p(snapshot, sig_ref, ctx),
            });
        } else if method.as_ref() == b"FieldMDP" && fieldmdp.is_none() {
            fieldmdp = Some(read_fieldmdp_params(snapshot, sig_ref, ctx));
        }
    }
    (docmdp, fieldmdp)
}

/// The `/TransformParams` dictionary of one signature reference entry,
/// resolved whether direct or indirect.
fn transform_params(
    snapshot: &DocumentSnapshot,
    sig_ref: &Dictionary,
    ctx: &mut ParseContext,
) -> Option<Arc<PdfObject>> {
    let key = snapshot.names().lookup(b"TransformParams")?;
    let params = dict_get_resolved(snapshot, sig_ref, key, ctx)?;
    if params.as_dict().is_some() {
        Some(params)
    } else {
        None
    }
}

/// Verbatim `/P` from a DocMDP signature reference's transform parameters.
/// `None` when anything on the path is missing or unreadable — the entry's
/// presence is still reported by the caller, so a present-but-unparseable
/// policy never reads as no policy at all.
fn read_docmdp_p(
    snapshot: &DocumentSnapshot,
    sig_ref: &Dictionary,
    ctx: &mut ParseContext,
) -> Option<i64> {
    let params = transform_params(snapshot, sig_ref, ctx)?;
    let params = params.as_dict()?;
    snapshot
        .names()
        .lookup(b"P")
        .and_then(|key| dict_get_resolved(snapshot, params, key, ctx))
        .and_then(|value| value.as_int())
}

/// The `/Action`, `/Fields`, and `/P` of a FieldMDP signature reference's
/// transform parameters. Every field degrades to `None` independently when
/// its path is missing or unreadable; the entry's presence is reported by
/// the caller either way.
fn read_fieldmdp_params(
    snapshot: &DocumentSnapshot,
    sig_ref: &Dictionary,
    ctx: &mut ParseContext,
) -> FieldMdpInfo {
    let mut info = FieldMdpInfo {
        p: None,
        action: None,
        fields: None,
    };
    let Some(params) = transform_params(snapshot, sig_ref, ctx) else {
        return info;
    };
    let Some(params) = params.as_dict() else {
        return info;
    };
    let names = snapshot.names();
    info.action = names
        .lookup(b"Action")
        .and_then(|key| params.get(key))
        .and_then(PdfObject::as_name)
        .map(|name| String::from_utf8_lossy(&names.resolve(name)).into_owned());
    info.fields = names.lookup(b"Fields").and_then(|key| {
        let value = dict_get_resolved(snapshot, params, key, ctx)?;
        let PdfObject::Array(entries) = value.as_ref() else {
            return None;
        };
        let mut names_out = Vec::new();
        for entry in entries.iter() {
            let Some(field) = resolve_value(snapshot, entry, ctx) else {
                continue;
            };
            if let PdfObject::String(text) = field.as_ref() {
                names_out.push(decode_text_string_lossy(text.as_bytes()));
            }
        }
        Some(names_out)
    });
    info.p = names
        .lookup(b"P")
        .and_then(|key| dict_get_resolved(snapshot, params, key, ctx))
        .and_then(|value| value.as_int());
    info
}

// ── encryption ──────────────────────────────────────────────────────────────

fn read_encryption(snapshot: &DocumentSnapshot, ctx: &mut ParseContext) -> Option<EncryptionInfo> {
    let encrypt_ref = snapshot.structure().trailer.encrypt;
    // Either the trailer names an /Encrypt object, or the dictionary is
    // direct in the trailer (rare but legal) — the resolver handled the
    // former at open; here we only need to *read* it (it is never itself
    // encrypted; the resolver exempts it).
    let names = snapshot.names();
    let dict: Option<Arc<PdfObject>> = match encrypt_ref {
        Some(id) => resolve(snapshot, id, ctx),
        None => None,
    };
    let dict = dict?;
    let dict = dict.as_dict()?;

    let get_int = |key: &[u8], ctx: &mut ParseContext| -> i64 {
        names
            .lookup(key)
            .and_then(|k| dict_get_resolved(snapshot, dict, k, ctx))
            .and_then(|v| v.as_int())
            .unwrap_or(0)
    };
    let version = get_int(b"V", ctx);
    let revision = get_int(b"R", ctx);
    let length_bits = {
        let l = get_int(b"Length", ctx);
        if l == 0 { 40 } else { l }
    };

    let method = match (version, revision) {
        (5, _) | (_, 5 | 6) => "AES-256".to_owned(),
        (4, _) => {
            // V4: the crypt-filter method decides RC4 vs AES-128.
            if crypt_filter_is_aes(snapshot, dict, ctx) {
                "AES-128".to_owned()
            } else {
                "RC4-128".to_owned()
            }
        }
        (1, _) => "RC4-40".to_owned(),
        (2, _) => format!("RC4-{length_bits}"),
        _ => "unknown".to_owned(),
    };

    // Genuine `/U` validation of the empty user password — not an artifact
    // of how this snapshot happened to open (it may have opened with a
    // caller-supplied password, or not at all).
    let user_password_empty = {
        let file_id = snapshot
            .structure()
            .trailer
            .file_id
            .as_ref()
            .map(|id| id[0].as_bytes().to_vec())
            .unwrap_or_default();
        let (enc, _scheme) = pdf_document::parse_encrypt_dict(dict, file_id, names);
        pdf_security::StandardHandler::verify_password(&enc, "").is_some()
    };

    Some(EncryptionInfo {
        version,
        revision,
        method,
        user_password_empty,
    })
}

/// For V4/V5: whether the default stream crypt filter (`/CF` → `/StdCF` →
/// `/CFM`) names an AES method.
fn crypt_filter_is_aes(
    snapshot: &DocumentSnapshot,
    encrypt: &Dictionary,
    ctx: &mut ParseContext,
) -> bool {
    let names = snapshot.names();
    let Some(cf_key) = names.lookup(b"CF") else {
        return false;
    };
    let Some(cf) = dict_get_resolved(snapshot, encrypt, cf_key, ctx) else {
        return false;
    };
    let Some(cf) = cf.as_dict() else { return false };
    let Some(stdcf_key) = names.lookup(b"StdCF") else {
        return false;
    };
    let Some(stdcf) = dict_get_resolved(snapshot, cf, stdcf_key, ctx) else {
        return false;
    };
    let Some(stdcf) = stdcf.as_dict() else {
        return false;
    };
    let Some(cfm_key) = names.lookup(b"CFM") else {
        return false;
    };
    let Some(cfm) = stdcf.get(cfm_key).and_then(PdfObject::as_name) else {
        return false;
    };
    let cfm = names.resolve(cfm);
    cfm.as_ref() == b"AESV2" || cfm.as_ref() == b"AESV3"
}

// ── pages ───────────────────────────────────────────────────────────────────

fn examine_pages(
    snapshot: &DocumentSnapshot,
) -> (Vec<PageStatus>, Vec<FontRecord>, Vec<ImageRecord>) {
    // Annotations on: the render default is `AnnotationMode::StaticAppearances`,
    // so the doctor's per-page compile health must exercise the same path a
    // default render would (annotation appearance streams included).
    let compiler = PageCompiler::new().with_annotations(true);
    let mut out = Vec::with_capacity(snapshot.page_count() as usize);
    // One inventory for the whole document: it carries the dedup indexes and
    // the `/FontFile*` answers across pages, so a font shared by every page is
    // keyed and resolved once rather than once per page.
    let mut inventory = metrics::Inventory::new();
    for index in 0..snapshot.page_count() {
        // Fresh context per page: repairs and budgets attribute to the page.
        let mut ctx = ParseContext::new();
        let media_box_present = page_has_media_box(snapshot, index, &mut ctx);
        let repairs_before = ctx.recovery.len();
        let (compile, metrics) =
            match compiler.compile_semantic(snapshot, PageIndex(index), &mut ctx) {
                Ok(page) => {
                    let op_count = page.ops.len();
                    let metrics = inventory.absorb_page(snapshot, &page);
                    let compile = if ctx.recovery.len() > repairs_before {
                        let detail = ctx.recovery[repairs_before..]
                            .iter()
                            .map(describe_recovery)
                            .collect::<Vec<_>>()
                            .join("; ");
                        CompileStatus::Degraded { op_count, detail }
                    } else {
                        CompileStatus::Ok { op_count }
                    };
                    (compile, Some(metrics))
                }
                Err(e) => (
                    CompileStatus::Failed {
                        error: e.to_string(),
                    },
                    None,
                ),
            };
        // The marker inventory is a second pass over the same content (the
        // accessor does not touch the semantic product). A fresh context:
        // the semantic pass above already attributed this page's repairs.
        let mut marker_ctx = ParseContext::new();
        let marked_content =
            match compiler.marked_content_spans(snapshot, PageIndex(index), &mut marker_ctx) {
                Ok(spans) => spans
                    .into_iter()
                    .map(|span| MarkedContentRecord {
                        tag: span.tag,
                        open_offset: span.open_offset,
                        close_offset: span.close_offset,
                        bbox: span.bbox.map(|b| [b.x0, b.y0, b.x1, b.y1]),
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
        out.push(PageStatus {
            index,
            media_box_present,
            compile,
            metrics,
            marked_content,
        });
    }
    let (fonts, images) = inventory.into_parts();
    (out, fonts, images)
}

/// Whether `/MediaBox` is genuinely present on the page or an inherited
/// ancestor (`PageRef::media_box` is always populated, defaulting when
/// absent — this distinguishes the two).
fn page_has_media_box(snapshot: &DocumentSnapshot, index: u32, ctx: &mut ParseContext) -> bool {
    let Ok(page_ref) = snapshot.page(PageIndex(index)) else {
        return false;
    };
    let names = snapshot.names();
    let media_box = names.known.media_box;
    let parent = names.known.parent;
    // A direct (inline-dict) page has no object id to re-resolve or walk a
    // `/Parent` chain from; report conservatively (its own MediaBox already
    // landed in `PageRef::media_box` via inheritance at open time).
    let Some(object) = page_ref.object else {
        return false;
    };
    let mut current = resolve(snapshot, object, ctx);
    for _ in 0..MAX_PARENT_HOPS {
        let Some(obj) = current else { return false };
        let Some(dict) = obj.as_dict() else {
            return false;
        };
        if dict.contains_key(media_box) {
            return true;
        }
        current = dict
            .get(parent)
            .and_then(|p| resolve_value(snapshot, p, ctx));
    }
    false
}

// ── annotations ─────────────────────────────────────────────────────────────

/// Count annotations per `/Subtype` by reading each page's `/Annots` array
/// directly (read-only; deliberately independent of any higher-level
/// annotation API), and record the identity each annotation declares.
fn inventory_annotations(
    snapshot: &DocumentSnapshot,
    ctx: &mut ParseContext,
) -> AnnotationInventory {
    let names = snapshot.names();
    let annots_key = names.intern(b"Annots");
    let mut inventory = AnnotationInventory::default();
    for index in 0..snapshot.page_count() {
        let Ok(page_ref) = snapshot.page(PageIndex(index)) else {
            continue;
        };
        // A direct (inline-dict) page has no id to re-resolve; its annotations
        // were already parsed into `PageRef::annotations` at open, so the raw
        // doctor inventory simply skips it.
        let Some(page_id) = page_ref.object else {
            continue;
        };
        let Some(page_obj) = resolve(snapshot, page_id, ctx) else {
            continue;
        };
        let Some(page_dict) = page_obj.as_dict() else {
            continue;
        };
        let Some(annots) = dict_get_resolved(snapshot, page_dict, annots_key, ctx) else {
            continue;
        };
        let PdfObject::Array(entries) = annots.as_ref() else {
            continue;
        };
        for entry in entries.iter() {
            let object = entry.as_reference();
            let Some(annot) = resolve_value(snapshot, entry, ctx) else {
                continue;
            };
            let Some(annot) = annot.as_dict() else {
                continue;
            };
            let subtype = annot
                .get(names.known.subtype)
                .and_then(PdfObject::as_name)
                .map(|n| String::from_utf8_lossy(&names.resolve(n)).into_owned());
            *inventory
                .count_per_subtype
                .entry(subtype.clone().unwrap_or_else(|| "(none)".to_owned()))
                .or_insert(0) += 1;
            inventory.annotations.push(AnnotationRecord {
                page: index,
                object,
                subtype,
                contents: annot_text(snapshot, annot, b"Contents", ctx),
                name: annot_text(snapshot, annot, b"NM", ctx),
                modified: annot_text(snapshot, annot, b"M", ctx),
                creation_date: annot_text(snapshot, annot, b"CreationDate", ctx),
                author: annot_text(snapshot, annot, b"T", ctx),
                subject: annot_text(snapshot, annot, b"Subj", ctx),
                intent: annot_intent(snapshot, annot, ctx),
            });
        }
    }
    inventory
}

/// An annotation's text-string field: resolved when indirect, decoded lossy.
/// Anything that is not a string — including a missing key — stays `None`.
fn annot_text(
    snapshot: &DocumentSnapshot,
    dict: &Dictionary,
    key: &[u8],
    ctx: &mut ParseContext,
) -> Option<String> {
    let names = snapshot.names();
    let key = names.lookup(key)?;
    let value = dict_get_resolved(snapshot, dict, key, ctx)?;
    match value.as_ref() {
        PdfObject::String(text) => Some(decode_text_string_lossy(text.as_bytes())),
        _ => None,
    }
}

/// An annotation's `/IT` intent name: resolved when indirect, read as the
/// name's bytes. Anything that is not a name stays `None`.
fn annot_intent(
    snapshot: &DocumentSnapshot,
    dict: &Dictionary,
    ctx: &mut ParseContext,
) -> Option<String> {
    let names = snapshot.names();
    let key = names.lookup(b"IT")?;
    let value = dict_get_resolved(snapshot, dict, key, ctx)?;
    match value.as_ref() {
        PdfObject::Name(id) => Some(String::from_utf8_lossy(&names.resolve(*id)).into_owned()),
        _ => None,
    }
}

// ── features ────────────────────────────────────────────────────────────────

fn read_features(snapshot: &DocumentSnapshot, ctx: &mut ParseContext) -> FeatureFlags {
    let names = snapshot.names();
    let mut flags = FeatureFlags {
        uses_object_streams: xref_uses_object_streams(&snapshot.structure().xref),
        ..FeatureFlags::default()
    };

    let root = snapshot.structure().trailer.root;
    let Some(catalog) = resolve(snapshot, root, ctx) else {
        return flags;
    };
    let Some(catalog) = catalog.as_dict() else {
        return flags;
    };

    let has = |snapshot: &DocumentSnapshot,
               dict: &Dictionary,
               key: &[u8],
               ctx: &mut ParseContext|
     -> Option<Arc<PdfObject>> {
        let k = names.lookup(key)?;
        let v = dict_get_resolved(snapshot, dict, k, ctx)?;
        if matches!(v.as_ref(), PdfObject::Null) {
            None
        } else {
            Some(v)
        }
    };

    if let Some(acroform) = has(snapshot, catalog, b"AcroForm", ctx) {
        flags.has_acroform = true;
        if let Some(acroform) = acroform.as_dict() {
            flags.has_xfa = has(snapshot, acroform, b"XFA", ctx).is_some();
        }
    }
    flags.has_outlines = has(snapshot, catalog, b"Outlines", ctx).is_some();
    flags.has_optional_content = has(snapshot, catalog, b"OCProperties", ctx).is_some();
    if let Some(names_dict) = has(snapshot, catalog, b"Names", ctx)
        && let Some(names_dict) = names_dict.as_dict()
    {
        flags.has_javascript = has(snapshot, names_dict, b"JavaScript", ctx).is_some();
        flags.has_embedded_files = has(snapshot, names_dict, b"EmbeddedFiles", ctx).is_some();
    }
    flags
}

fn xref_uses_object_streams(xref: &pdf_structure::XrefMap) -> bool {
    // In-stream members always carry generation 0 (spec), so probing with
    // generation 0 sees every one of them.
    (0..xref.size()).any(|n| {
        matches!(
            xref.locate(ObjectId::new(n, 0)),
            ObjectLocation::InObjectStream { .. }
        )
    })
}

// ── rendering ───────────────────────────────────────────────────────────────

impl DocumentReport {
    /// Human-readable multi-line rendering of the report.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        match &self.open {
            OpenOutcome::Ok => writeln_ok(&mut s, "open: ok"),
            OpenOutcome::Recovered { how } => {
                writeln_ok(
                    &mut s,
                    &format!("open: recovered ({} repair(s))", how.len()),
                );
                for h in how {
                    writeln_ok(&mut s, &format!("  - {h}"));
                }
            }
            OpenOutcome::Failed { error } => writeln_ok(&mut s, &format!("open: FAILED: {error}")),
        }
        match &self.header {
            None => writeln_ok(&mut s, "header: not found"),
            Some(h) => {
                let line = format!("header: %PDF-{}.{}", h.major, h.minor);
                let line = if h.header_offset == 0 {
                    line
                } else {
                    format!("{line} [{} bytes of leading garbage]", h.header_offset)
                };
                writeln_ok(&mut s, &line);
            }
        }
        writeln_ok(&mut s, &format!("linearized: {}", self.linearized));
        writeln_ok(
            &mut s,
            &format!(
                "xref: revisions={} recovery_used={} rebuilt={}",
                self.xref.revision_count, self.xref.recovery_used, self.xref.rebuilt
            ),
        );
        match &self.encryption {
            None => writeln_ok(&mut s, "encryption: none"),
            Some(e) => writeln_ok(
                &mut s,
                &format!(
                    "encryption: {} (V={} R={}) empty-user-password={}",
                    e.method, e.version, e.revision, e.user_password_empty
                ),
            ),
        }
        let f = &self.features;
        writeln_ok(
            &mut s,
            &format!(
                "features: acroform={} xfa={} javascript={} outlines={} \
                 optional-content={} object-streams={} embedded-files={}",
                f.has_acroform,
                f.has_xfa,
                f.has_javascript,
                f.has_outlines,
                f.has_optional_content,
                f.uses_object_streams,
                f.has_embedded_files
            ),
        );
        let id = &self.identity;
        if let (Some(permanent), Some(changing)) = (&id.id_permanent, &id.id_changing) {
            writeln_ok(
                &mut s,
                &format!("id: permanent={permanent} changing={changing}"),
            );
        }
        if id.has_xmp_metadata {
            writeln_ok(&mut s, "metadata: /Info + XMP");
        }
        for (key, value) in &id.info {
            writeln_ok(&mut s, &format!("  /{key}: {value}"));
        }
        if let Some(xmp) = &id.xmp {
            writeln_ok(&mut s, "xmp lineage:");
            for (label, value) in [
                ("DocumentID", &xmp.document_id),
                ("InstanceID", &xmp.instance_id),
                ("OriginalDocumentID", &xmp.original_document_id),
                ("DerivedFrom.documentID", &xmp.derived_from_document_id),
                ("DerivedFrom.instanceID", &xmp.derived_from_instance_id),
            ] {
                if let Some(value) = value {
                    writeln_ok(&mut s, &format!("  {label}: {value}"));
                }
            }
            for event in &xmp.history {
                let action = event.action.as_deref().unwrap_or("(no action)");
                let when = event.when.as_deref().unwrap_or("(no time)");
                let agent = event.software_agent.as_deref().unwrap_or("(no agent)");
                writeln_ok(&mut s, &format!("  history: {action} {when} by {agent}"));
            }
        }
        writeln_ok(&mut s, &format!("pages: {}", self.pages.len()));
        for p in &self.pages {
            let line = match &p.compile {
                CompileStatus::Ok { op_count } => {
                    format!("  page {}: ok ({op_count} ops)", p.index)
                }
                CompileStatus::Degraded { op_count, detail } => {
                    format!("  page {}: degraded ({op_count} ops): {detail}", p.index)
                }
                CompileStatus::Failed { error } => {
                    format!("  page {}: FAILED: {error}", p.index)
                }
            };
            let line = if p.media_box_present {
                line
            } else {
                format!("{line} [no /MediaBox — defaulted]")
            };
            let line = match &p.metrics {
                Some(m) => format!(
                    "{line} text={} vis={} images={} fonts={} coverage={}/10000",
                    m.text_runs, m.visible_text_runs, m.images, m.fonts, m.image_coverage_bps
                ),
                None => line,
            };
            writeln_ok(&mut s, &line);
        }
        writeln_ok(
            &mut s,
            &format!(
                "inventories: {} font(s), {} image(s)",
                self.fonts.len(),
                self.images.len()
            ),
        );
        if self.annotations.count_per_subtype.is_empty() {
            writeln_ok(&mut s, "annotations: none");
        } else {
            writeln_ok(
                &mut s,
                &format!("annotations: {} total", self.annotations.total()),
            );
            for (subtype, count) in &self.annotations.count_per_subtype {
                writeln_ok(&mut s, &format!("  {subtype}: {count}"));
            }
        }
        let sigs = &self.signatures;
        if sigs.signatures.is_empty() && sigs.unsigned_fields == 0 {
            writeln_ok(&mut s, "signatures: none");
        } else {
            writeln_ok(
                &mut s,
                &format!(
                    "signatures: {} signed, {} unsigned field(s)",
                    sigs.signatures.len(),
                    sigs.unsigned_fields
                ),
            );
            for sig in &sigs.signatures {
                let name = sig.field_name.as_deref().unwrap_or("(unnamed)");
                let sub_filter = sig.sub_filter.as_deref().unwrap_or("(no /SubFilter)");
                writeln_ok(&mut s, &format!("  {name}: {sub_filter}"));
                let coverage = if sig.covers_whole_file {
                    "covers whole file".to_owned()
                } else {
                    format!("DOES NOT cover whole file; /ByteRange {:?}", sig.byte_range)
                };
                writeln_ok(&mut s, &format!("    {coverage}"));
            }
        }
        s
    }
}

/// `writeln!` into a `String` cannot fail; this wrapper keeps the
/// `unwrap_used` lint clean.
fn writeln_ok(s: &mut String, line: &str) {
    let _ = writeln!(s, "{line}");
}
