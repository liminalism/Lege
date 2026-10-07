//! Neutral document facts, as they appear in a report.
//!
//! Everything here is descriptive: what the file is and what it says. Nothing
//! here decides what any of it means — that is [`crate::report`]'s job. The
//! separation is deliberate, because facts that have already been coloured by
//! suspicion cannot be re-weighed later.
//!
//! These types mirror what the fact layer currently reaches rather than what M1
//! ultimately needs. The difference is not left implicit: sections that M1
//! requires and this build cannot yet compute are listed in
//! [`DocumentFacts::pending`], so an incomplete audit reports itself as one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::report::Availability;

/// The file as bytes, independent of anything it claims about itself.
///
/// Computed over the whole file rather than through the parser, so it holds
/// even for a document that will not open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    /// Lowercase hex SHA-256 of the entire file.
    pub sha256: String,
    /// Size in bytes.
    pub size_bytes: u64,
}

/// Whether the document opened, and at what cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenState {
    /// Opened with no repair of any kind.
    Clean,
    /// Opened only after structural repair.
    Recovered,
    /// Did not open. For an encrypted document without a usable password this
    /// is expected, and the structural layer is still readable.
    Failed {
        /// The error, rendered.
        error: String,
    },
}

/// The parse assessment: how much of what follows can be trusted.
///
/// A rebuilt xref is the important case. Recovery is the right behaviour for a
/// renderer and the wrong basis for a claim about revision membership, so the
/// fact that repair happened travels with every report rather than being
/// discarded once the document is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseAssessment {
    /// Whether the document opened, and how.
    pub state: OpenState,
    /// Every repair performed, in the order performed.
    pub recovery_events: Vec<String>,
    /// The xref was rebuilt by a full object scan — the heaviest repair, and
    /// the one that invalidates revision-history claims.
    pub xref_rebuilt: bool,
    /// Number of xref sections found. Not yet the same as the count of logical
    /// revisions; see the `logical-revision` term and M2.
    pub xref_section_count: usize,
}

impl ParseAssessment {
    /// Whether any claim resting on this parse is resting on repaired
    /// structure.
    #[must_use]
    pub fn rests_on_recovered_structure(&self) -> bool {
        !self.recovery_events.is_empty()
    }
}

/// How one xref section was encoded in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XrefSectionKind {
    /// A classic `xref` table followed by a `trailer` dictionary.
    Classic,
    /// A `/Type /XRef` cross-reference stream (its own dictionary is the
    /// trailer).
    Stream,
}

/// How many entries of each location kind one xref section declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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

/// One auxiliary `/XRefStm` section grouped into a logical revision: part of
/// the same user-visible save as its classic table, never a save of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuxiliarySectionFacts {
    /// Header-relative file offset of the auxiliary stream.
    pub offset: u64,
    /// How the section was encoded: always [`XrefSectionKind::Stream`] for
    /// `/XRefStm`, reported rather than assumed.
    pub kind: XrefSectionKind,
    /// How many entries the auxiliary section declared, by location kind.
    pub entry_counts: XrefEntryCounts,
}

/// How firmly a revision's byte range is anchored.
///
/// This scores the boundary pairing only — whether a syntactically valid
/// `startxref` naming the revision's section is followed by its `%%EOF` —
/// not the health of the file around it. A revision whose range is exactly
/// established is [`BoundaryConfidence::High`] even when the run recovered
/// elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryConfidence {
    /// Exactly one valid `startxref` names the section's offset and its
    /// `%%EOF` follows; the section parsed cleanly and its `/Prev` linkage
    /// reaches only walked sections.
    High,
    /// The range is anchored but something weakens it: the section parsed
    /// with repair, several `startxref`s name the same offset, or the
    /// section's `/Prev` names a section that was never walked. The
    /// [`LogicalRevisionFacts::boundary_note`] says which.
    Reduced,
    /// No valid `startxref` names the section's offset, so the revision end
    /// is not established. A following `%%EOF` is never used without its
    /// `startxref`: `%%EOF` bytes occur inside stream data, and pairing one
    /// without an anchor would invent a boundary.
    Low,
}

/// One user-visible save of the document: the set of xref sections that
/// together represent a single incremental update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalRevisionFacts {
    /// Chronological index: 0 is the oldest save.
    pub index: u32,
    /// Header-relative file offset of the primary xref section (the
    /// `startxref`/`/Prev` value that led here).
    pub section_offset: u64,
    /// How the primary section was encoded.
    pub section_kind: XrefSectionKind,
    /// How many entries the primary section declared, by location kind.
    pub entry_counts: XrefEntryCounts,
    /// Auxiliary `/XRefStm` sections grouped into this save.
    pub auxiliary_sections: Vec<AuxiliarySectionFacts>,
    /// Absolute source offset where this save's bytes start: the previous
    /// revision's end, or zero for the oldest. `None` when the previous
    /// revision's end is itself unestablished.
    pub start: Option<u64>,
    /// Absolute source offset one past the paired `%%EOF`'s final `F`.
    /// `None` when no valid `startxref` anchors this revision's end.
    pub end: Option<u64>,
    /// How firmly [`Self::start`] and [`Self::end`] are anchored.
    pub boundary_confidence: BoundaryConfidence,
    /// Why confidence is not [`BoundaryConfidence::High`]. Always `None`
    /// for high-confidence boundaries.
    pub boundary_note: Option<String>,
    /// Every object this save's member sections declared, classified
    /// against the previous save, in ascending object-number order.
    /// Empty only when the member sections declared no entries, which a
    /// walked section never does outside a rebuild (and a rebuild reports
    /// no revisions at all).
    #[serde(default)]
    pub changes: Vec<ObjectChangeFacts>,
}

/// How one member object of a logical revision relates to the previous
/// save's view of the same object number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectChangeKind {
    /// The number was never declared before, and this save declares it
    /// live.
    Added,
    /// The number was declared before, and this save declares it live at a
    /// different location, under a different generation, or — for an
    /// object-stream member whose container changed — with different
    /// stored bytes.
    Redefined,
    /// This save declares the number free. For a number that was live this
    /// is a deletion; for a number never declared live (including the
    /// object-0 free-list head) it is the section's first free declaration.
    Freed,
    /// The number was declared before with the same generation and the
    /// same location, and — where the bytes could be compared — the same
    /// stored bytes. The save re-states an entry it did not change.
    RepeatedIdentically,
}

/// Where one revision member claims its object lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectLocationFacts {
    /// An uncompressed object at this header-relative file offset.
    Offset(u64),
    /// A compressed member of an object stream.
    InObjectStream {
        /// Object number of the containing `/Type /ObjStm` stream.
        container: u32,
        /// Zero-based index within the container.
        index: u32,
    },
    /// A free entry: the save declares the number free.
    Free,
}

/// The role one object plays in the document, as seen through the latest
/// revision's resolved structure: page tree, resources, annotations, form
/// fields, and metadata.
///
/// Roles reflect the latest view, not the revision that introduced the
/// object: an object redefined across revisions keeps the role its newest
/// definition plays. Where one object matches several roles the most
/// page-specific wins (page, then contents, annotation, signature, font,
/// image, metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectRole {
    /// A page dictionary.
    Page,
    /// A page content stream.
    Contents,
    /// A font dictionary reached through page resources or a compiled page.
    Font,
    /// An image XObject reached through page resources or a compiled page.
    Image,
    /// An annotation dictionary.
    Annotation,
    /// A signature form field or its signature value dictionary.
    Signature,
    /// The `/Info` dictionary or the XMP `/Metadata` stream.
    Metadata,
    /// Resolved, and none of the above: catalog, page tree nodes,
    /// cross-reference streams, object-stream containers, form fields,
    /// and everything else structural.
    Other,
    /// Could not be determined: the role walk needs a resolved document,
    /// which a structure-only run does not have; or the declared identity
    /// has no live definition in the latest view (freed) or did not
    /// resolve.
    Unknown,
}

/// One member object of a logical revision, classified against the
/// previous save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectChangeFacts {
    /// The declared identity as `N G R`.
    pub object: String,
    /// How this save's declaration relates to the previous save's.
    pub change: ObjectChangeKind,
    /// Where this save claims the object lives.
    pub location: ObjectLocationFacts,
    /// Lowercase hex SHA-256 over the object's bytes as stored: the exact
    /// `N G obj … endobj` span for an uncompressed object, or the member's
    /// value bytes as stored in its (decoded) container stream. `None`
    /// when the span cannot be established — the object does not parse at
    /// its declared offset, the container does not decode (including
    /// encrypted containers, which are not decrypted for hashing), or the
    /// entry is free. For an encrypted document the span covers
    /// ciphertext; identical ciphertext still means an identical object.
    pub raw_sha256: Option<String>,
    /// Lowercase hex SHA-256 over the shallow canonical form of the value
    /// this save declares: dictionaries with keys sorted, references by
    /// identity, streams with framing keys (`/Length`, `/Filter`,
    /// `/DecodeParms`) excluded and bodies decoded. `None` when the value
    /// does not parse, a stream body does not decode (including encrypted
    /// bodies, which are not decrypted for hashing), or the entry is free.
    /// A redefinition that only re-serializes keeps its semantic hash;
    /// one that re-encodes the same content keeps it too.
    pub semantic_sha256: Option<String>,
    /// The role the object plays in the document's latest view.
    pub role: ObjectRole,
    /// Zero-based pages the object serves in the latest view, ascending:
    /// a page dictionary serves itself, content and annotation objects
    /// serve their page, fonts and images serve the pages whose resources
    /// reach them. Empty for document-level objects — and whenever the
    /// role walk could not run.
    pub pages: Vec<u32>,
}

/// The document's save history: logical revisions oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RevisionHistoryFacts {
    /// One entry per user-visible save, oldest first. A hybrid-reference
    /// save (classic table plus `/XRefStm`) is one entry with an auxiliary
    /// section, not two entries.
    pub revisions: Vec<LogicalRevisionFacts>,
    /// Header-relative offsets of xref sections excluded as linearization
    /// first-page stubs. A stub is part of the initial save's two-section
    /// structure, not evidence of a later edit; it is recorded here rather
    /// than dropped so the exclusion is visible.
    pub linearization_stubs: Vec<u64>,
    /// The reverse object-to-page map: every `N G R` classified in any
    /// revision, each with the union of the pages it serves in the latest
    /// view, ascending. Mirrors the per-change [`ObjectChangeFacts::pages`]
    /// as a single lookup; empty page lists mark document-level objects
    /// and objects the role walk could not reach.
    #[serde(default)]
    pub object_pages: BTreeMap<String, Vec<u32>>,
}

/// What the `/Encrypt` dictionary declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptionFacts {
    /// `/V` — algorithm version.
    pub version: i64,
    /// `/R` — standard-handler revision.
    pub revision: i64,
    /// Human name of the method, e.g. "AES-256".
    pub method: String,
    /// Whether the empty user password genuinely validates against `/U`. True
    /// here means permissions-only encryption, which audits in full.
    pub user_password_empty: bool,
}

/// Document-level feature flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFacts {
    /// An `/AcroForm` is present.
    pub has_acroform: bool,
    /// XFA form data is present.
    pub has_xfa: bool,
    /// Document-level JavaScript is present.
    pub has_javascript: bool,
    /// An outline tree is present.
    pub has_outlines: bool,
    /// Optional content (layers) is present.
    pub has_optional_content: bool,
    /// Objects are stored in object streams.
    pub uses_object_streams: bool,
    /// The name tree carries `/EmbeddedFiles` — there are file attachments.
    pub has_embedded_files: bool,
}

/// One annotation's declared identity: where it lives and what it says
/// about itself.
///
/// Neutral facts only: these are what the annotation dictionary *says*, with
/// no judgement about whether it is telling the truth. Dates in particular
/// are verbatim writer claims, not observations.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AnnotationIdentityFacts {
    /// Zero-based page index carrying this annotation.
    pub page: u32,
    /// Backing annotation dictionary as `N G R`; `None` for a direct inline
    /// annotation dictionary.
    pub object: Option<String>,
    /// `/Subtype`, e.g. `Text`. `None` when absent or unreadable.
    pub subtype: Option<String>,
    /// `/Contents` text, decoded lossy.
    pub contents: Option<String>,
    /// `/NM` annotation name: the writer-assigned identifier meant to be
    /// unique within the document.
    pub name: Option<String>,
    /// `/M` last-modification date, verbatim and unparsed.
    pub modified: Option<String>,
    /// `/CreationDate`, verbatim and unparsed.
    pub creation_date: Option<String>,
    /// `/T` text label — for markup annotations, the author. Verbatim.
    pub author: Option<String>,
    /// `/Subj` subject: a short description of the annotation. Verbatim.
    pub subject: Option<String>,
    /// `/IT` intent name (e.g. `FreeTextTypewriter`), as authored.
    pub intent: Option<String>,
}

/// Annotation counts by `/Subtype`, plus the identity each annotation
/// declares.
///
/// Counts answer "how many"; the per-annotation list answers "which" — the
/// fields that distinguish an annotation added later from one present from
/// the start. Still facts only: nothing here decides what any of it means.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AnnotationFacts {
    /// How many of each `/Subtype`.
    pub count_per_subtype: BTreeMap<String, u32>,
    /// Every annotation in page order, with the identity it declares.
    pub annotations: Vec<AnnotationIdentityFacts>,
}

impl AnnotationFacts {
    /// Total annotations across all subtypes.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.count_per_subtype.values().sum()
    }
}

/// The `%PDF` header and where it was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderFacts {
    /// Major version, e.g. 1 in `%PDF-1.7`.
    pub major: u8,
    /// Minor version, e.g. 7 in `%PDF-1.7`.
    pub minor: u8,
    /// Bytes of leading garbage before `%PDF`. Non-zero means every
    /// structural offset in the file is shifted relative to the source.
    pub header_offset: u64,
}

/// Whether the file is linearized (fast-web-view).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearizationFacts {
    /// The first object is a valid `/Linearized` parameter dictionary.
    /// False is the ordinary case: most files are not linearized.
    pub linearized: bool,
}

/// One `xmpMM:History` entry: a save, print or conversion the writer recorded.
///
/// Every field is the writer's claim, not an observation. A tool that does not
/// record history leaves none, and a tool that does can write whatever it
/// likes — so an absent or thin history is not evidence of anything.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct XmpHistoryEventFacts {
    /// `stEvt:action`, e.g. `created`, `saved`, `converted`.
    pub action: Option<String>,
    /// `stEvt:when` — the claimed timestamp, verbatim and unparsed.
    pub when: Option<String>,
    /// `stEvt:softwareAgent` — the tool, as it named itself.
    pub software_agent: Option<String>,
    /// `stEvt:changed` — which parts the writer says changed.
    pub changed: Option<String>,
    /// `stEvt:instanceID` — the instance this event produced.
    pub instance_id: Option<String>,
}

/// The `xmpMM` media-management properties, when the packet carries any.
///
/// This is the lineage the trailer `/ID` pair only hints at. `document_id` is
/// meant to persist across a document's entire edit history while
/// `instance_id` is minted on every save, so two files sharing a `document_id`
/// with differing `instance_id`s are two saves of one lineage by the format's
/// own design — where a document regenerated from its source application would
/// mint both fresh. `derived_from_instance_id`, when present, names the
/// ancestor outright.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct XmpLineageFacts {
    /// `xmpMM:DocumentID` — stable across the edit history.
    pub document_id: Option<String>,
    /// `xmpMM:InstanceID` — changes on every save.
    pub instance_id: Option<String>,
    /// `xmpMM:OriginalDocumentID`.
    pub original_document_id: Option<String>,
    /// `stRef:documentID` inside `xmpMM:DerivedFrom`.
    pub derived_from_document_id: Option<String>,
    /// `stRef:instanceID` inside `xmpMM:DerivedFrom`.
    pub derived_from_instance_id: Option<String>,
    /// `xmpMM:History`, in document order.
    pub history: Vec<XmpHistoryEventFacts>,
}

/// `/Info` and XMP. Needs an open document: `/Info` strings and the XMP stream
/// body are encrypted like anything else.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MetadataFacts {
    /// Every `/Info` entry with a string value, including non-standard keys.
    pub info: BTreeMap<String, String>,
    /// Whether the catalog carries an XMP `/Metadata` stream.
    pub has_xmp_metadata: bool,
    /// The media-management properties inside that stream. `None` means the
    /// packet was absent, undecodable, or carried none — which is why
    /// `has_xmp_metadata` is reported separately rather than inferred from
    /// this being present.
    pub xmp: Option<XmpLineageFacts>,
}

/// What the document says about its own identity and descent.
///
/// The trailer `/ID` pair is the durable half and needs no decryption, so it
/// survives even for a document that will not open. Its first element is meant
/// to persist across a document's entire edit history while the second changes
/// on every save — which makes the pair evidence of lineage independent of any
/// metadata a writer chose to refresh.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LineageFacts {
    /// Lowercase hex of the permanent trailer `/ID[0]`, if present.
    pub id_permanent: Option<String>,
    /// Lowercase hex of the changing trailer `/ID[1]`, if present.
    pub id_changing: Option<String>,
    /// `/Info` and XMP presence, when the document opened.
    pub metadata: Availability<MetadataFacts>,
}

/// Strict classification of a signature's `/ByteRange` against the
/// original file bytes, as assessed by the fact layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteRangeValidity {
    /// Well-formed and spanning the whole file.
    Valid,
    /// Well-formed but leaving file bytes uncovered. Ordinary when later
    /// revisions were appended after signing; [`SignatureFacts::revision`]
    /// and [`SignatureFacts::ends_at_revision_end`] decide whether the gap
    /// is explained.
    GapContaining,
    /// A pair reaches past the end of the file.
    Truncated,
    /// Pairs overlap or run out of order.
    Overlapping,
    /// Wrong shape: fewer than four numbers, an odd count, a negative
    /// value, or a non-integer entry.
    Malformed,
}

/// One claimed covered run of a `/ByteRange`, in absolute source positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRangeSpan {
    /// Absolute source offset where the covered run starts.
    pub offset: u64,
    /// Length of the covered run in bytes.
    pub length: u64,
}

/// A DocMDP permission level: what later changes a certification
/// signature allows (ISO 32000 Table 254).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocMdpPermission {
    /// `/P 1`: no changes are permitted after signing.
    NoChanges,
    /// `/P 2`: only filling in forms, instantiating page templates, and
    /// signing are permitted.
    FormFillAndSign,
    /// `/P 3`: as [`Self::FormFillAndSign`], plus creating, deleting, and
    /// modifying annotations.
    FormFillSignAndAnnotate,
}

impl DocMdpPermission {
    /// The meaning of a verbatim `/P` value, or `None` when it is outside
    /// the specified 1–3.
    #[must_use]
    pub const fn from_p(p: i64) -> Option<Self> {
        match p {
            1 => Some(Self::NoChanges),
            2 => Some(Self::FormFillAndSign),
            3 => Some(Self::FormFillSignAndAnnotate),
            _ => None,
        }
    }
}

/// A DocMDP certification policy, exactly as the signature reference states
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMdpPolicyFacts {
    /// Verbatim `/P` from the DocMDP transform parameters. `None` when the
    /// entry is present but `/P` is absent or not an integer.
    pub p: Option<i64>,
    /// The meaning of [`Self::p`]. `None` when `/P` is absent or outside
    /// 1–3, in which case the permission is unestablished rather than
    /// permissive.
    pub permission: Option<DocMdpPermission>,
}

/// Which form fields a FieldMDP lock applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldMdpAction {
    /// Every form field is locked.
    All,
    /// Exactly the listed fields are locked.
    Include,
    /// Every field except the listed ones is locked.
    Exclude,
}

impl FieldMdpAction {
    /// The meaning of a verbatim `/Action` name, or `None` when it is not
    /// one of the three specified names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "All" => Some(Self::All),
            "Include" => Some(Self::Include),
            "Exclude" => Some(Self::Exclude),
            _ => None,
        }
    }
}

/// A FieldMDP lock policy, exactly as the signature reference states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldMdpPolicyFacts {
    /// Verbatim `/P` from the FieldMDP transform parameters, when present.
    /// Recorded, not evaluated: compliance follows the lock set below.
    pub p: Option<i64>,
    /// Verbatim `/Action` as specified. `None` when absent or unrecognized,
    /// in which case the lock set is unestablished rather than empty.
    pub action: Option<FieldMdpAction>,
    /// Verbatim `/Fields`: the qualified field names the action applies to.
    /// `None` when `/Fields` is absent or not an array; an empty array
    /// reads as `Some` with no names.
    pub fields: Option<Vec<String>>,
}

/// One signature field and the bytes it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureFacts {
    /// The field's `/T` name.
    pub field_name: Option<String>,
    /// Backing field dictionary as `N G R`; `None` for a direct inline field.
    #[serde(default)]
    pub field_object: Option<String>,
    /// The `/V` signature value dictionary as `N G R`; `None` for a direct
    /// inline value.
    #[serde(default)]
    pub value_object: Option<String>,
    /// `/Filter` — the handler that produced the signature.
    pub filter: Option<String>,
    /// `/SubFilter` — the signature encoding.
    pub sub_filter: Option<String>,
    /// `/M` — the claimed signing time, verbatim and unparsed. A claim by the
    /// signer, not an observation.
    pub signed_at: Option<String>,
    /// `/ByteRange`, verbatim.
    pub byte_range: Vec<i64>,
    /// Whether `/ByteRange` spans the entire file.
    ///
    /// False is the structural half of the incremental-update attack: bytes
    /// outside the signed range are unsigned however well the cryptography
    /// over that range verifies. Whether it verifies is reported separately
    /// in [`Self::crypto`].
    pub covers_whole_file: bool,
    /// Strict classification of `/ByteRange` against the original file
    /// bytes: shape, ordering, and bounds.
    pub byte_range_validity: ByteRangeValidity,
    /// The claimed covered runs, in `/ByteRange` order. Empty when the
    /// shape does not parse as offset/length pairs.
    pub covered_ranges: Vec<ByteRangeSpan>,
    /// The M2 logical revision that introduced the signature: the oldest
    /// save whose member sections declare the `/V` value object (or the
    /// field, when the value object is direct or unknown). `None` when
    /// revisions are unavailable or neither object is a member of any save.
    pub revision: Option<u32>,
    /// Whether the last covered byte ends exactly at the introducing
    /// revision's `%%EOF` — allowing whitespace (the trailing newline a
    /// writer emits after the marker) between the marker's end and the
    /// range's end. `None` when the revision is unknown, its end is
    /// unestablished, or the range claims no covered runs.
    pub ends_at_revision_end: Option<bool>,
    /// The DocMDP certification policy from the signature's `/Reference`
    /// array. `None` for an approval signature, which carries no document
    /// lock.
    pub docmdp: Option<DocMdpPolicyFacts>,
    /// The FieldMDP lock policy from the signature's `/Reference` array.
    pub fieldmdp: Option<FieldMdpPolicyFacts>,
    /// M4 CMS digest report over [`Self::covered_ranges`]. `None` when the
    /// `/Contents` bytes could not be reached (direct objects the locator
    /// does not chase, or a document that could not be reopened); an
    /// unparseable CMS is an explicit unestablished report, not `None`.
    #[serde(default)]
    pub crypto: Option<crate::digest::SignatureDigestFacts>,
}

/// Signature fields found in the `/AcroForm`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SignatureInventoryFacts {
    /// Every signed `/FT /Sig` field.
    pub signatures: Vec<SignatureFacts>,
    /// Signature fields with no `/V` — ordinary in a blank form.
    pub unsigned_fields: u32,
}

/// How a page is painted, derived from the fact layer's metrics.
///
/// This is interpretation, not a fact: the underlying counts and coverage
/// travel alongside it so a reader can disagree with the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageRepresentation {
    /// Visible text and/or vector paths; images, if any, are decorative.
    Vector,
    /// One or more images dominate the page and nothing visible is drawn
    /// on top — the PDF-Verse / scan-without-OCR case.
    RasterOnly,
    /// A dominating image plus an invisible text layer (typically `Tr 3`).
    RasterWithTextLayer,
    /// Substantial image coverage and visible vector content together.
    Mixed,
    /// The content stream compiled and painted nothing.
    Blank,
    /// The content stream did not compile, so no label is honest.
    Unknown,
}

/// One font resource the document used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontFacts {
    /// Resource name it was reached through (`F1`).
    pub resource_name: String,
    /// Backing font dictionary as `N G R`, when the resource resolved.
    pub object: Option<String>,
    /// `/Subtype`.
    pub subtype: String,
    /// `/BaseFont`.
    pub base_font: String,
    /// An embedded outline program was present and usable.
    pub embedded: bool,
}

/// One image XObject or inline image the document used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageFacts {
    /// Backing object as `N G R`; `None` for an inline image.
    pub object: Option<String>,
    /// Sample width in pixels.
    pub width: u32,
    /// Sample height in pixels.
    pub height: u32,
    /// `/BitsPerComponent`.
    pub bits_per_component: u8,
    /// `/ImageMask true`.
    pub is_mask: bool,
    /// Filter names in application order.
    pub filters: Vec<String>,
}

/// One interactive-edit marker: a marked-content span whose tag belongs to
/// the `/TouchUp_*` family Acrobat's Edit Text tool leaves behind.
///
/// Neutral facts only — the tag as written, where the span sits, and what
/// it encloses. Whether any marker amounts to evidence of tampering is a
/// question for the findings engine, not this inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditMarkerFacts {
    /// Tag name as written, without the leading slash (e.g.
    /// `"TouchUp_TextEdit"`).
    pub tag: String,
    /// Byte offset of the opening `BMC`/`BDC` keyword in the page's
    /// concatenated decoded content stream.
    pub open_offset: usize,
    /// Byte offset of the closing `EMC` keyword in that same stream, or
    /// `None` when the stream ended with the span still open.
    pub close_offset: Option<usize>,
    /// Union of the enclosed text runs' boxes, in thousandths of a point
    /// (`[x0, y0, x1, y1]`, normalized), or `None` when the span enclosed
    /// no text runs. Advance-derived and approximate: enough to say *where*
    /// on the page the re-typed run sits, not a glyph outline.
    pub bbox: Option<[i64; 4]>,
}

/// The edit markers found on one page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageEditMarkers {
    /// Zero-based page index.
    pub page: u32,
    /// Every `/TouchUp_*` span on the page, in document order.
    pub markers: Vec<EditMarkerFacts>,
}

/// Whether a page's content stream compiled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompileOutcome {
    /// Compiled cleanly.
    Ok {
        /// Number of content-stream operators.
        op_count: usize,
    },
    /// Compiled, but only with repairs along the way.
    Degraded {
        /// Number of content-stream operators.
        op_count: usize,
        /// What had to be repaired.
        detail: String,
    },
    /// Did not compile.
    Failed {
        /// The error, rendered.
        error: String,
    },
}

/// One page's facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageFacts {
    /// Zero-based page index.
    pub index: u32,
    /// Whether a `/MediaBox` is genuinely present rather than defaulted.
    pub media_box_present: bool,
    /// Whether the content stream compiled.
    pub compile: CompileOutcome,
    /// How the page is painted. [`PageRepresentation::Unknown`] when compile
    /// failed, so a failed page cannot be mistaken for a blank one.
    pub representation: PageRepresentation,
    /// Show-text runs interned on the page, visible or not.
    pub text_runs: u32,
    /// Runs that paint.
    pub visible_text_runs: u32,
    /// Distinct font resources referenced by those runs.
    pub fonts: u32,
    /// Image XObjects and inline images interned on the page.
    pub images: u32,
    /// `Fill` / `Stroke` / `FillStroke` operators.
    pub path_paints: u32,
    /// Sum of non-mask image placement areas, in basis points of the crop
    /// box (10_000 = painted area equals the page).
    pub image_coverage_bps: u16,
    /// Effective DPI of the largest-coverage non-mask placement.
    pub effective_dpi: Option<u32>,
}

/// A section M1 requires that this build cannot yet compute.
///
/// Listed in the report rather than omitted. A consumer that sees an audit with
/// a non-empty pending list knows it is reading a partial inventory, which is
/// the same discipline [`Availability`] applies to individual facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSection {
    /// Stable name of the missing section.
    pub section: String,
    /// What has to happen before it can be computed.
    pub blocked_on: String,
}

impl PendingSection {
    /// Note a missing section.
    pub fn new(section: impl Into<String>, blocked_on: impl Into<String>) -> Self {
        Self {
            section: section.into(),
            blocked_on: blocked_on.into(),
        }
    }
}

/// Everything the audit determined about the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFacts {
    /// The file as bytes.
    pub identity: FileIdentity,
    /// How the document parsed.
    pub parse: ParseAssessment,
    /// The document's save history, oldest first. Needs only the structural
    /// layer, so it survives a failed open like the header does; unavailable
    /// when the structure itself was unreadable, or — with the reason spelled
    /// out — when the xref was rebuilt by scanning and original revision
    /// membership cannot be established.
    pub revisions: Availability<RevisionHistoryFacts>,
    /// The `%PDF` header. Readable without decryption, so it survives a failed
    /// open; unavailable only when no header exists at all.
    pub header: Availability<HeaderFacts>,
    /// Whether the file is linearized. Needs only the structural layer, so
    /// it survives a failed open like the header does; unavailable only when
    /// the structure itself was unreadable.
    pub linearization: Availability<LinearizationFacts>,
    /// What the document declares about its own identity and descent.
    pub lineage: LineageFacts,
    /// Signature fields and the byte ranges they cover.
    pub signatures: Availability<SignatureInventoryFacts>,
    /// The `/Encrypt` declaration, or `None` for an unencrypted document.
    /// Unavailable only when the structural layer itself was unreadable.
    pub encryption: Availability<Option<EncryptionFacts>>,
    /// Document-level feature flags.
    pub features: Availability<FeatureFacts>,
    /// Annotation counts by subtype.
    pub annotations: Availability<AnnotationFacts>,
    /// Per-page facts.
    pub pages: Availability<Vec<PageFacts>>,
    /// Distinct font resources referenced by any compiled page.
    pub fonts: Availability<Vec<FontFacts>>,
    /// Distinct image XObjects (plus every inline image) referenced by any
    /// compiled page.
    pub images: Availability<Vec<ImageFacts>>,
    /// Per-page interactive-edit markers (`/TouchUp_*` marked-content
    /// spans). Sparse: only pages carrying at least one appear, so an
    /// available empty list is a document with none.
    pub edit_markers: Availability<Vec<PageEditMarkers>>,
    /// M1 sections this build cannot yet compute.
    pub pending: Vec<PendingSection>,
}
