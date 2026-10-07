//! Signature `/ByteRange` checks against the M2 logical revisions.
//!
//! Each signature is associated with the revision that introduced it, and
//! its range's end is verified against that revision's `%%EOF`. Ranges
//! that are structurally bad — malformed, overlapping, or truncated — and
//! well-formed ranges that do not end where their own save ends become
//! [`SIGNATURE_BYTE_RANGE_INVALID`] findings. A gap-containing range whose
//! end matches its introducing revision is the legitimate post-signing
//! update, not a finding.
//!
//! No cryptography lives here: only the ranges the signatures claim, over
//! the original bytes.

use crate::document::{ByteRangeSpan, ByteRangeValidity, DocumentFacts, RevisionHistoryFacts};
use crate::report::{AnalysisMode, EvidenceStrength, Finding, FindingCode};

/// A signature's `/ByteRange` is structurally invalid, or does not end at
/// its introducing revision's `%%EOF`.
pub const SIGNATURE_BYTE_RANGE_INVALID: FindingCode =
    FindingCode::new("SIGNATURE_BYTE_RANGE_INVALID");

/// Check every signed signature's `/ByteRange` against the logical
/// revisions.
///
/// Never fails and never panics: an unavailable signatures section or an
/// unverifiable range yields no finding, and the facts themselves carry
/// the gap (`revision` / `ends_at_revision_end` as `None`).
#[must_use]
pub fn check_signature_byte_ranges(facts: &DocumentFacts) -> Vec<Finding> {
    let Some(signatures) = facts.signatures.value() else {
        return Vec::new();
    };
    let rests_on_recovered = facts.parse.rests_on_recovered_structure();
    let mut findings = Vec::new();
    for signature in &signatures.signatures {
        let name = signature.field_name.as_deref().unwrap_or("(unnamed)");
        if !matches!(
            signature.byte_range_validity,
            ByteRangeValidity::Valid | ByteRangeValidity::GapContaining
        ) {
            findings.push(Finding {
                code: SIGNATURE_BYTE_RANGE_INVALID,
                strength: EvidenceStrength::StructuralFact,
                explanation: format!(
                    "signature '{name}' has an invalid /ByteRange {:?}: {}",
                    signature.byte_range,
                    validity_phrase(signature.byte_range_validity),
                ),
                limitation: None,
                requires_mode: AnalysisMode::Full,
                rests_on_recovered_structure: rests_on_recovered,
            });
        } else if signature.ends_at_revision_end == Some(false) {
            findings.push(Finding {
                code: SIGNATURE_BYTE_RANGE_INVALID,
                strength: EvidenceStrength::StructuralFact,
                explanation: format!(
                    "signature '{name}' /ByteRange ends at byte {} but its revision ends at {}",
                    range_end(signature.covered_ranges.as_slice()).unwrap_or(0),
                    revision_end(facts, signature.revision).unwrap_or(0),
                ),
                limitation: None,
                requires_mode: AnalysisMode::Full,
                rests_on_recovered_structure: rests_on_recovered,
            });
        }
    }
    findings
}

/// Plain-language phrase for a structurally invalid classification.
fn validity_phrase(validity: ByteRangeValidity) -> &'static str {
    match validity {
        ByteRangeValidity::Valid | ByteRangeValidity::GapContaining => "well-formed",
        ByteRangeValidity::Truncated => "a covered run reaches past the end of the file",
        ByteRangeValidity::Overlapping => "covered runs overlap or run out of order",
        ByteRangeValidity::Malformed => {
            "wrong shape: at least four numbers in offset/length pairs, all non-negative integers"
        }
    }
}

/// One past the last covered byte, when the claimed runs establish one.
fn range_end(covered: &[ByteRangeSpan]) -> Option<u64> {
    covered.last()?.offset.checked_add(covered.last()?.length)
}

/// The end of logical revision `index`, when established.
fn revision_end(facts: &DocumentFacts, index: Option<u32>) -> Option<u64> {
    let history = facts.revisions.value()?;
    let index = usize::try_from(index?).ok()?;
    history.revisions.get(index)?.end
}

/// The M2 logical revision that introduced the signature: the oldest save
/// (revisions run oldest first) whose member objects declare the `/V`
/// value object number — or the field number, when the value object is
/// direct or unknown. The value object is preferred because the range
/// lives in it: a field signed by a later save was introduced by that
/// save, not by the save that drew the empty field.
pub(crate) fn introducing_revision(
    field_number: Option<u32>,
    value_number: Option<u32>,
    history: &RevisionHistoryFacts,
) -> Option<u32> {
    let number = value_number.or(field_number)?;
    history
        .revisions
        .iter()
        .find(|revision| {
            revision
                .changes
                .iter()
                .any(|change| change_number(change.object.as_str()) == Some(number))
        })
        .map(|revision| revision.index)
}

/// Whether the range's end matches the introducing revision's `%%EOF`
/// end: exactly, or past it by whitespace only (the trailing newline a
/// writer emits after the marker). `None` when the revision end is
/// unestablished or the range claims no covered runs.
pub(crate) fn ends_at_revision_end(
    covered: &[ByteRangeSpan],
    revision_end: Option<u64>,
    bytes: &[u8],
) -> Option<bool> {
    let last = covered.last()?;
    let range_end = last.offset.checked_add(last.length)?;
    let end = revision_end?;
    let end_at = usize::try_from(end).ok()?;
    let range_at = usize::try_from(range_end).ok()?;
    if range_end < end || range_at > bytes.len() {
        return Some(false);
    }
    Some(
        bytes[end_at..range_at]
            .iter()
            .all(|byte| is_pdf_whitespace(*byte)),
    )
}

/// The object number of a change record's `N G R` identity.
fn change_number(identity: &str) -> Option<u32> {
    identity.split_whitespace().next()?.parse().ok()
}

/// PDF whitespace (ISO 32000-1 Table 1).
fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}
