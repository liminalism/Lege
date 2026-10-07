//! `pdf-forensics` — the interpretation layer of the PDF integrity checker.
//!
//! The renderer's `pdf-read` crate is the neutral facts layer: it reports what
//! a document says. This crate decides what those facts are worth. It holds the
//! report schema, the findings vocabulary, logical-revision reconstruction,
//! object-change classification, and evidence generation. The findings engine
//! never lives in the parser crates.
//!
//! Two rules shape every type here, and both are load-bearing rather than
//! stylistic:
//!
//! * **No score.** The report has no numeric fraud percentage. Findings are
//!   categorical, each carries its own evidence strength and its own stated
//!   limitation, and the document-level verdict is one of four named
//!   [`Disposition`]s. A percentage would be indefensible in the admissions and
//!   employment workflows this tool serves.
//! * **No silent gaps.** A fact the tool could not reach is
//!   [`Availability::Unavailable`] with a reason, never absent and never zero;
//!   a section it cannot yet compute at all is listed in
//!   [`DocumentFacts::pending`]; and a finding may not rest on a fact its own
//!   [`AnalysisMode`] could not have reached. An encrypted document audited
//!   without a password must not look like a clean one, and neither must a
//!   half-built audit.
//!
//! # Status
//!
//! [`audit_bytes`] works today over what the fact layer currently surfaces:
//! file identity, the parse assessment, logical revisions with per-revision
//! object-change classification, the header, linearization, lineage
//! identifiers, encryption, feature flags, per-annotation identity,
//! signatures, per-page representation classification, font and image
//! inventories, and per-page edit markers. The rest of the M1 inventory is
//! enumerated in [`DocumentFacts::pending`] rather than silently missing.
//!
//! Every full audit also runs the findings engine ([`collect_evidence`]):
//! signature byte ranges, CMS digests, post-signature DocMDP/FieldMDP policy,
//! revision text diffs, annotation overlays, and flattened occlusion become
//! [`Finding`]s, and the [`Disposition`]
//! follows from them —
//! [`Disposition::IntegrityFailure`] when a CMS digest mismatches,
//! [`Disposition::ReviewRecommended`] when any other review indicator exists,
//! [`Disposition::Inconclusive`] while any pending section is outstanding,
//! and [`Disposition::NoReviewIndicatorFound`] only for a complete, empty
//! audit. [`audit_bytes_with_evidence`] additionally returns the
//! [`AuditEvidence`] renders behind the findings, which
//! [`write_evidence_pngs`] exports as deterministic PNGs.

mod byterange;
mod changes;
mod digest;
mod docmdp;
mod document;
mod evidence;
mod findings;
mod hashes;
mod intake;
mod occlusion;
mod overlays;
mod replay;
mod report;
mod representation;
mod revision_diff;
mod revisions;
mod roles;

pub use byterange::{SIGNATURE_BYTE_RANGE_INVALID, check_signature_byte_ranges};
pub use digest::{
    CertificateFacts, DigestValidity, IssuerChainFacts, IssuerLinkFacts, SIGNATURE_DIGEST_MISMATCH,
    SignatureDigestFacts, SignerDigestFacts, TimeValidity, TimestampStatus, TrustStatus,
    check_signature_digests, verify_signature_digest,
};
pub use docmdp::{
    DOCMDP_POLICY_VIOLATION, POST_SIGNATURE_REVISION, PostSignatureChange,
    check_post_signature_policies, post_signature_changes,
};
pub use document::{
    AnnotationFacts, AnnotationIdentityFacts, AuxiliarySectionFacts, BoundaryConfidence,
    ByteRangeSpan, ByteRangeValidity, CompileOutcome, DocMdpPermission, DocMdpPolicyFacts,
    DocumentFacts, EditMarkerFacts, EncryptionFacts, FeatureFacts, FieldMdpAction,
    FieldMdpPolicyFacts, FileIdentity, FontFacts, HeaderFacts, ImageFacts, LineageFacts,
    LinearizationFacts, LogicalRevisionFacts, MetadataFacts, ObjectChangeFacts, ObjectChangeKind,
    ObjectLocationFacts, ObjectRole, OpenState, PageEditMarkers, PageFacts, PageRepresentation,
    ParseAssessment, PendingSection, RevisionHistoryFacts, SignatureFacts, SignatureInventoryFacts,
    XmpHistoryEventFacts, XmpLineageFacts, XrefEntryCounts, XrefSectionKind,
};
pub use evidence::{
    AnnotationMode, ChangedRegion, EvidenceError, EvidenceOptions, EvidenceRender, RenderDiff,
    diff_renders, encode_png, highlight_changed_regions, render_bytes_evidence,
    render_page_evidence, write_png,
};
pub use findings::{AuditEvidence, collect_evidence, decide_disposition, write_evidence_pngs};
pub use intake::{audit_bytes, audit_bytes_with_evidence};
pub use occlusion::{
    OcclusionKind, OcclusionOptions, OcclusionOutcome, PAGE_CONTENT_COVERS_TEXT, PageOcclusion,
    RASTER_PATCH_ON_VECTOR_PAGE, SkippedOcclusionPage, detect_flattened_occlusion,
};
pub use overlays::{
    AnnotationOverlay, EXPLICIT_ANNOTATION_OVER_TEXT, OverlayOptions, OverlayOutcome,
    SkippedOverlayPage, detect_annotation_overlays,
};
pub use replay::{ReplayError, open_revision};
pub use report::{
    AnalysisMode, AuditReport, Availability, CAVEAT, Disposition, EvidenceStrength, Finding,
    FindingCode, SCHEMA_VERSION, SchemaViolation, UnavailableReason,
};
pub use revision_diff::{
    RevisionTextChange, RevisionTextDiffOptions, RevisionTextDiffOutcome, SkippedRevisionPair,
    TEXT_CHANGED_BETWEEN_REVISIONS, diff_revision_text,
};
pub use revisions::group_revisions;
