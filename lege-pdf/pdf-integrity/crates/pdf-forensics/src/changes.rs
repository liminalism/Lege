//! Per-revision object-change classification.
//!
//! Each logical revision's member sections declare a set of objects; this
//! module classifies every declared object against the previous save's
//! view of the same number — [`ObjectChangeKind::Added`],
//! [`ObjectChangeKind::Redefined`], [`ObjectChangeKind::Freed`], or
//! [`ObjectChangeKind::RepeatedIdentically`] — and attaches the evidence
//! each classification rests on: where the save claims the object lives,
//! the raw-byte and semantic hashes of the declared definition, the role
//! the object plays, and the pages it serves.
//!
//! Classification itself is structural and runs in every mode. Hashes are
//! parsed from the file bytes at each entry's declared location (see
//! [`crate::hashes`]); roles and page associations come from the resolved
//! latest view (see [`crate::roles`]) and degrade to
//! [`ObjectRole::Unknown`] when the document did not open.

use std::collections::{BTreeMap, BTreeSet};

use pdf_document::DocumentSnapshot;
use pdf_read::{FontRecord, ImageRecord, XrefEntryFacts, XrefEntryLocation, XrefSectionFacts};

use crate::document::{
    ObjectChangeFacts, ObjectChangeKind, ObjectLocationFacts, ObjectRole, RevisionHistoryFacts,
};
use crate::hashes::{ByteHashes, HashSources, LocationKey};
use crate::revisions::membership;
use crate::roles::{self, RoleMap};

/// Everything the classifier needs beyond the grouped history.
pub(crate) struct ChangeInputs<'a> {
    /// The walked xref sections, newest first, with entries preserved.
    pub sections: &'a [XrefSectionFacts],
    /// The whole file, for hashing declared definitions.
    pub bytes: &'a [u8],
    /// Bytes of leading garbage before `%PDF`, for placing
    /// header-relative offsets. `None` when no header was found.
    pub header_offset: Option<u64>,
    /// Whether the first object is a `/Linearized` parameter dictionary.
    pub linearized: bool,
    /// The fact layer's compiled-page font inventory, for roles.
    pub fonts: &'a [FontRecord],
    /// The fact layer's compiled-page image inventory, for roles.
    pub images: &'a [ImageRecord],
    /// The open document, when the run reached one. Roles, page
    /// associations, and the resolved/unresolved distinction all need it;
    /// without it every change reports [`ObjectRole::Unknown`].
    pub snapshot: Option<&'a DocumentSnapshot>,
}

/// What the previous save knew about one object number.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PriorEntry {
    /// The generation the previous save declared.
    generation: u16,
    /// Where the previous save claimed the object lives.
    location: LocationKey,
    /// The raw-byte hash of the previous save's declaration, when its
    /// bytes could be established.
    raw: Option<String>,
}

/// Classify every member object of every logical revision, attach the
/// per-revision changes to `history`, and build the reverse
/// object-to-page map.
///
/// `history` must be the grouping of `inputs.sections` with
/// `inputs.linearized`: membership is recomputed from the same inputs,
/// so the changes land on the revisions whose sections declared them.
pub(crate) fn attach_changes(history: &mut RevisionHistoryFacts, inputs: ChangeInputs<'_>) {
    let (members, _) = membership(inputs.sections, inputs.linearized);
    let resolved = inputs.snapshot.map(|snapshot| {
        (
            snapshot,
            roles::build_role_map(snapshot, inputs.fonts, inputs.images),
        )
    });
    let mut hashes = HashSources::new(inputs.bytes, inputs.header_offset);
    let mut prior: BTreeMap<u32, PriorEntry> = BTreeMap::new();
    let mut object_pages: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();

    for (member, revision) in members.iter().zip(history.revisions.iter_mut()) {
        // The revision's declared objects: primary section first, then
        // auxiliaries. A number declared twice in one save keeps its
        // first declaration; hybrid saves are disjoint by construction,
        // so a duplicate is a malformed section either way.
        let mut declared: BTreeMap<u32, &XrefEntryFacts> = BTreeMap::new();
        let mut sections = Vec::with_capacity(1 + member.auxiliaries.len());
        sections.push(member.primary);
        sections.extend(member.auxiliaries.iter().copied());
        for index in sections {
            let Some(section) = inputs.sections.get(index) else {
                continue;
            };
            for entry in &section.entries {
                declared.entry(entry.id.number).or_insert(entry);
            }
        }
        // Locations live as of this save — the previous saves' plus this
        // save's own — for decoding object-stream members through the
        // container definition this save actually sees.
        let mut live: BTreeMap<u32, LocationKey> = prior
            .iter()
            .map(|(&number, entry)| (number, entry.location))
            .collect();
        for (&number, entry) in &declared {
            live.insert(number, translate_location(entry.location).0);
        }

        let mut changes = Vec::with_capacity(declared.len());
        for (&number, entry) in &declared {
            let (location, reported) = translate_location(entry.location);
            let ByteHashes { raw, semantic } = match location {
                LocationKey::Free => ByteHashes {
                    raw: None,
                    semantic: None,
                },
                LocationKey::Offset(offset) => hashes.hash_offset(offset, number),
                LocationKey::InStream { container, index } => {
                    hashes.hash_member(container, index, number, &live)
                }
            };
            let change = classify(prior.get(&number), entry, location, raw.as_ref());
            let (role, pages) = role_and_pages(&resolved, number, entry);
            object_pages
                .entry(entry.id.to_string())
                .or_default()
                .extend(pages.iter().copied());
            changes.push(ObjectChangeFacts {
                object: entry.id.to_string(),
                change,
                location: reported,
                raw_sha256: raw.clone(),
                semantic_sha256: semantic,
                role,
                pages,
            });
            prior.insert(
                number,
                PriorEntry {
                    generation: entry.id.generation,
                    location,
                    raw,
                },
            );
        }
        revision.changes = changes;
    }

    history.object_pages = object_pages
        .into_iter()
        .map(|(object, pages)| (object, pages.into_iter().collect()))
        .collect();
}

/// The location in this crate's terms, plus the reported form.
fn translate_location(location: XrefEntryLocation) -> (LocationKey, ObjectLocationFacts) {
    match location {
        XrefEntryLocation::Offset(offset) => (
            LocationKey::Offset(offset),
            ObjectLocationFacts::Offset(offset),
        ),
        XrefEntryLocation::InObjectStream { container, index } => (
            LocationKey::InStream { container, index },
            ObjectLocationFacts::InObjectStream { container, index },
        ),
        XrefEntryLocation::Free => (LocationKey::Free, ObjectLocationFacts::Free),
    }
}

/// Classify one declared entry against what the previous save knew about
/// its number. `raw` is this save's raw-byte hash, when its bytes could
/// be established.
fn classify(
    previous: Option<&PriorEntry>,
    entry: &XrefEntryFacts,
    location: LocationKey,
    raw: Option<&String>,
) -> ObjectChangeKind {
    let Some(previous) = previous else {
        return match location {
            LocationKey::Free => ObjectChangeKind::Freed,
            LocationKey::Offset(_) | LocationKey::InStream { .. } => ObjectChangeKind::Added,
        };
    };
    if matches!(location, LocationKey::Free) {
        return match previous.location {
            LocationKey::Free => ObjectChangeKind::RepeatedIdentically,
            LocationKey::Offset(_) | LocationKey::InStream { .. } => ObjectChangeKind::Freed,
        };
    }
    if matches!(previous.location, LocationKey::Free) {
        // A freed number reused for a live object: the number is old,
        // the definition is new.
        return ObjectChangeKind::Redefined;
    }
    let same_declaration =
        previous.generation == entry.id.generation && previous.location == location;
    // Same file offset means same bytes: incremental updates append, so
    // earlier bytes never change. A member's bytes instead live inside
    // its container, which may itself have been redefined — only equal
    // stored hashes prove the member identical, and unhashable members
    // stay redefined rather than claimed identical.
    let same_bytes = match location {
        LocationKey::Offset(_) => true,
        LocationKey::InStream { .. } => previous.raw.is_some() && previous.raw.as_ref() == raw,
        LocationKey::Free => false,
    };
    if same_declaration && same_bytes {
        ObjectChangeKind::RepeatedIdentically
    } else {
        ObjectChangeKind::Redefined
    }
}

/// The role and pages for one changed object number: the resolved
/// latest view's answer, or unknown when the run never opened.
fn role_and_pages(
    resolved: &Option<(&DocumentSnapshot, RoleMap)>,
    number: u32,
    entry: &XrefEntryFacts,
) -> (ObjectRole, Vec<u32>) {
    let Some((snapshot, map)) = resolved else {
        return (ObjectRole::Unknown, Vec::new());
    };
    let role = map.roles.get(&number).copied().unwrap_or_else(|| {
        // Reached by no walk: other when it resolves at all, unknown
        // when even that fails.
        roles::default_role(snapshot, entry.id)
    });
    let pages = map.pages.get(&number).cloned().unwrap_or_default();
    (role, pages)
}
