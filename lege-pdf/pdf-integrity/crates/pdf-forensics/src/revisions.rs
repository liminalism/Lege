//! Grouping xref sections into logical revisions.
//!
//! The structure layer walks xref sections newest first, but a section is
//! not a save: one hybrid-reference save holds two sections (a classic table
//! plus its auxiliary `/XRefStm` stream), linearized files carry a
//! first-page stub that is not an edit, and a rebuilt xref carries no
//! section structure at all. [`group_revisions`] applies those rules and
//! anchors each revision's byte range to a syntactically valid `startxref`
//! plus its following `%%EOF` — never a raw `%%EOF` search, which can hit
//! stream data.

use std::collections::{BTreeMap, BTreeSet};

use pdf_read::{XrefSectionFacts, XrefSectionKind as ReadKind};

use crate::document::{
    AuxiliarySectionFacts, BoundaryConfidence, LogicalRevisionFacts, RevisionHistoryFacts,
    XrefEntryCounts, XrefSectionKind,
};

/// The member sections of one logical revision: indexes into the
/// newest-first section slice, with the primary first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RevisionMembership {
    /// Index of the primary xref section.
    pub primary: usize,
    /// Indexes of the auxiliary `/XRefStm` sections grouped into the same
    /// save, in chain order.
    pub auxiliaries: Vec<usize>,
}

/// Assign newest-first section facts to oldest-to-newest logical revisions.
///
/// Returns the member sections per revision plus the header-relative offsets
/// of any linearization first-page stubs. Shared by
/// [`group_revisions`] and the object-change classifier so the two can
/// never disagree about which sections belong to which save.
pub(crate) fn membership(
    sections: &[XrefSectionFacts],
    linearized: bool,
) -> (Vec<RevisionMembership>, Vec<u64>) {
    // Auxiliary `/XRefStm` sections attach to the primary they name. One
    // naming no usable primary — out of range, another auxiliary, or itself —
    // is kept as a primary rather than dropped: the loader walked those
    // bytes, and a defensive demotion keeps them visible. The loader never
    // emits this shape; the branch exists so a future one cannot silently
    // lose a section here.
    let mut primaries: Vec<usize> = Vec::new();
    let mut auxiliaries: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, section) in sections.iter().enumerate() {
        let target = section
            .auxiliary_of
            .and_then(|named| usize::try_from(named).ok())
            .filter(|&named| {
                named < sections.len() && named != index && sections[named].auxiliary_of.is_none()
            });
        match target {
            Some(named) => auxiliaries.entry(named).or_default().push(index),
            None => primaries.push(index),
        }
    }

    // In a linearized file the earliest xref section is the first-page stub:
    // part of the initial save's two-section structure, not a later edit.
    // Updates always append, so the stub keeps the smallest offset for the
    // life of the file; excluding it never removes a genuine save. Recorded
    // rather than dropped, so the exclusion is visible.
    let mut stubs = Vec::new();
    if linearized && primaries.len() > 1 {
        let oldest = primaries
            .iter()
            .min_by_key(|&&index| sections[index].offset);
        if let Some(&stub) = oldest {
            stubs.push(sections[stub].offset);
            primaries.retain(|&index| index != stub);
        }
    }
    // Chain order reversed: `/Prev` links each section to its predecessor,
    // so reversed chain order is oldest first.
    primaries.reverse();

    let members = primaries
        .into_iter()
        .map(|primary| RevisionMembership {
            primary,
            auxiliaries: auxiliaries.remove(&primary).unwrap_or_default(),
        })
        .collect();
    (members, stubs)
}

/// Group newest-first section facts into oldest-to-newest logical revisions.
///
/// `bytes` is the whole file, scanned for `startxref`/`%%EOF` boundary
/// anchors; `linearized` tells whether the first object is a `/Linearized`
/// parameter dictionary.
///
/// `startxref` values compare against section offsets exactly as written —
/// both are header-relative by file convention — while the reported range
/// ends are absolute source positions, so no header shift enters anywhere.
///
/// The caller keeps rebuilt xrefs out: sections synthesized by a full object
/// scan have no source offsets, so grouping them would invent history.
#[must_use]
pub fn group_revisions(
    sections: &[XrefSectionFacts],
    bytes: &[u8],
    linearized: bool,
) -> RevisionHistoryFacts {
    let (members, stubs) = membership(sections, linearized);

    // Every walked offset, primaries and auxiliaries alike: a `/Prev` naming
    // one of these reaches walked history, anything else dangles.
    let known: BTreeSet<u64> = sections.iter().map(|s| s.offset).collect();
    let pairs = startxref_eof_pairs(bytes);

    let mut revisions = Vec::with_capacity(members.len());
    let mut previous_end: Option<u64> = None;
    for (rank, member) in members.iter().enumerate() {
        let section = &sections[member.primary];
        // Candidate ends naming this section's offset, earliest first, past
        // the previous revision's end so ranges partition the file in order.
        let floor = previous_end.unwrap_or(0);
        let mut ends: Vec<u64> = pairs
            .iter()
            .filter(|&&(value, _)| value == section.offset)
            .map(|&(_, end)| end)
            .filter(|&end| end > floor)
            .collect();
        ends.sort_unstable();
        let end = ends.first().copied();

        let mut weaknesses = Vec::new();
        if !section.recovery.is_empty() {
            weaknesses.push(format!(
                "section parsed with repair: {}",
                section.recovery.join("; ")
            ));
        }
        if ends.len() > 1 {
            weaknesses.push(format!(
                "{} valid startxref values name offset {}; earliest in-range boundary used",
                ends.len(),
                section.offset
            ));
        }
        if let Some(prev) = section.prev
            && !known.contains(&prev)
        {
            weaknesses.push(format!(
                "/Prev {prev} names no walked section; older history may exist beyond this revision"
            ));
        }

        let (boundary_confidence, boundary_note, end) = match (end, weaknesses.is_empty()) {
            (Some(end), true) => (BoundaryConfidence::High, None, Some(end)),
            (Some(end), false) => (
                BoundaryConfidence::Reduced,
                Some(weaknesses.join("; ")),
                Some(end),
            ),
            (None, _) => {
                let mut note = format!(
                    "no syntactically valid startxref names section offset {}; \
                     the revision end is not established",
                    section.offset
                );
                if !weaknesses.is_empty() {
                    note.push_str("; ");
                    note.push_str(&weaknesses.join("; "));
                }
                (BoundaryConfidence::Low, Some(note), None)
            }
        };

        let mut auxiliary_sections: Vec<AuxiliarySectionFacts> = member
            .auxiliaries
            .iter()
            .map(|&auxiliary| AuxiliarySectionFacts {
                offset: sections[auxiliary].offset,
                kind: translate_kind(sections[auxiliary].kind),
                entry_counts: translate_counts(&sections[auxiliary].entry_counts),
            })
            .collect();
        auxiliary_sections.sort_by_key(|auxiliary| auxiliary.offset);

        let start = if rank == 0 { Some(0) } else { previous_end };
        revisions.push(LogicalRevisionFacts {
            index: rank as u32,
            section_offset: section.offset,
            section_kind: translate_kind(section.kind),
            entry_counts: translate_counts(&section.entry_counts),
            auxiliary_sections,
            start,
            end,
            boundary_confidence,
            boundary_note,
            // Object-change classification is attached by the intake path,
            // which holds the bytes and the resolved-document evidence this
            // grouping never sees.
            changes: Vec::new(),
        });
        previous_end = end;
    }

    RevisionHistoryFacts {
        revisions,
        linearization_stubs: stubs,
        // Likewise attached by the intake path once changes are classified.
        object_pages: BTreeMap::new(),
    }
}

fn translate_kind(kind: ReadKind) -> XrefSectionKind {
    match kind {
        ReadKind::Classic => XrefSectionKind::Classic,
        ReadKind::Stream => XrefSectionKind::Stream,
    }
}

fn translate_counts(counts: &pdf_read::XrefEntryCounts) -> XrefEntryCounts {
    XrefEntryCounts {
        total: counts.total,
        live: counts.live,
        compressed: counts.compressed,
        free: counts.free,
    }
}

/// Every valid `startxref` in the file paired with its following `%%EOF`:
/// the value as written (header-relative, like every structural offset) and
/// the absolute source offset one past the marker's final `F`.
///
/// A pair needs all three: the whole-word `startxref` keyword, a
/// syntactically valid integer operand (ASCII digits, no sign, fits `u64`),
/// and — after whitespace only — the `%%EOF` marker terminated by
/// whitespace or end of input. Anything else is not a boundary anchor. In
/// particular a `%%EOF` with no valid `startxref` before it is never
/// collected: those bytes occur inside stream data.
fn startxref_eof_pairs(bytes: &[u8]) -> Vec<(u64, u64)> {
    const KEYWORD: &[u8] = b"startxref";
    let mut pairs = Vec::new();
    let mut at = 0;
    while at + KEYWORD.len() <= bytes.len() {
        if bytes[at..].starts_with(KEYWORD)
            && let Some(pair) = pair_at(bytes, at)
        {
            pairs.push(pair);
        }
        at += 1;
    }
    pairs
}

/// The boundary pair anchored at a `startxref` keyword occurrence, or `None`
/// when the keyword is part of a longer token or its tail is malformed.
fn pair_at(bytes: &[u8], keyword_at: usize) -> Option<(u64, u64)> {
    // Whole word on the left: `xstartxref` is not the keyword.
    if keyword_at > 0 {
        let before = bytes[keyword_at - 1];
        if !is_pdf_whitespace(before) && !is_pdf_delimiter(before) {
            return None;
        }
    }
    // A lexer reads `startxref123` as one keyword token, so the operand
    // needs at least one whitespace byte after the keyword.
    let mut pos = keyword_at + b"startxref".len();
    if !bytes.get(pos).is_some_and(|&b| is_pdf_whitespace(b)) {
        return None;
    }
    while bytes.get(pos).is_some_and(|&b| is_pdf_whitespace(b)) {
        pos += 1;
    }
    // The operand: ASCII digits only, fitting `u64`. A sign, an empty
    // operand, or an overflowing one is not a valid startxref.
    let digits_start = pos;
    let mut value: u64 = 0;
    while let Some(&b) = bytes.get(pos) {
        if !b.is_ascii_digit() {
            break;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
        pos += 1;
    }
    if pos == digits_start {
        return None;
    }
    // Whitespace only between the operand and the marker — this is what
    // makes the pairing anchored rather than a raw `%%EOF` search: no other
    // bytes may intervene.
    while bytes.get(pos).is_some_and(|&b| is_pdf_whitespace(b)) {
        pos += 1;
    }
    if !bytes[pos..].starts_with(b"%%EOF") {
        return None;
    }
    let end = pos + b"%%EOF".len();
    // The marker terminates the line: `%%EOFx` is a longer comment.
    if let Some(&after) = bytes.get(end)
        && !is_pdf_whitespace(after)
    {
        return None;
    }
    // `startxref` values are header-relative, like every structural offset:
    // compare as written, without shifting.
    Some((value, end as u64))
}

/// PDF whitespace (ISO 32000-1 Table 1).
fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// PDF delimiter characters (ISO 32000-1 Table 2).
fn is_pdf_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}
