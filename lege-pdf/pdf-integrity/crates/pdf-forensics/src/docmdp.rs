//! Post-signature change policy: DocMDP and FieldMDP compliance.
//!
//! A signature's introducing revision (see [`crate::byterange`]) divides the
//! M2 save history into what the signer saw and what came after. This module
//! lists the exact post-signature object changes — the M2 change records of
//! every later revision, minus restated entries and first free declarations,
//! which change nothing — and evaluates them against the DocMDP and FieldMDP
//! policies the signature declares.
//!
//! The semantics in one paragraph: an approval signature (no DocMDP, no
//! FieldMDP) constrains nothing, so its later revisions are the ordinary
//! workflow and stay silent here — byte-range validity and revision text
//! diffs already cover them. A certification policy whose later changes it
//! permits is likewise silent: a permitted form fill is the expected
//! workflow, not a review indicator, and the facts (revision list, signature
//! association) still record it. Only forbidden changes — or a policy too
//! malformed to evaluate — become findings: [`POST_SIGNATURE_REVISION`]
//! states the structural observation (which later revisions changed which
//! objects under which policy) and [`DOCMDP_POLICY_VIOLATION`] the policy
//! judgement (which of those changes the policy forbids).
//!
//! What the evaluator knows: DocMDP `/P` 1–3 per ISO 32000 Table 254, and
//! FieldMDP lock sets from `/Action` plus `/Fields`. Form-fill changes are
//! the `/AcroForm` field tree (fields, widget kids, and one level of widget
//! appearance streams) plus the `/AcroForm` dictionary itself; new signature
//! objects are signing, permitted under DocMDP `/P` 2–3 and under any
//! FieldMDP lock. Everything else — page dictionaries, content streams,
//! fonts, images, metadata, non-field annotations (under `/P` 1–2), object
//! deletions, and changes whose role cannot be established — is forbidden
//! under a certification policy.
//!
//! Two honest gaps, both stated on the violation finding: page-dictionary
//! bookkeeping for an otherwise permitted annotation or signature addition
//! (the `/Annots` array the addition extends lives on a page dictionary
//! whose redefinition reads as a content change) is not specially
//! recognized, and FieldMDP `/P` is recorded but not evaluated. Catalog
//! `/Perms` document locks are out of scope: only the policies the
//! signature itself declares are evaluated.
//!
//! No cryptography lives here: only the policies the signatures claim and
//! the object changes the later saves declare.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use pdf_document::{DocumentLimits, DocumentSnapshot, ParseContext};
use pdf_object::{Dictionary, ObjectId, PdfObject, decode_text_string_lossy};
use pdf_source::{OwnedBytesSource, PdfSource};

use crate::document::{
    DocMdpPermission, DocumentFacts, FieldMdpAction, FieldMdpPolicyFacts, ObjectChangeKind,
    ObjectLocationFacts, ObjectRole, SignatureFacts,
};
use crate::report::{AnalysisMode, EvidenceStrength, Finding, FindingCode};

/// Later revisions changed objects under a signature carrying a DocMDP or
/// FieldMDP lock — either beyond what the policy permits, or under a policy
/// too malformed to evaluate.
pub const POST_SIGNATURE_REVISION: FindingCode = FindingCode::new("POST_SIGNATURE_REVISION");

/// Post-signature object changes exceed the DocMDP or FieldMDP policy the
/// signature declares.
pub const DOCMDP_POLICY_VIOLATION: FindingCode = FindingCode::new("DOCMDP_POLICY_VIOLATION");

/// What the violation finding cannot establish, carried on every one of them.
const VIOLATION_LIMITATION: &str = "Permission is evaluated from the M2 object-change records \
    and latest-revision object roles: page-dictionary bookkeeping for an otherwise permitted \
    annotation or signature addition is not specially recognized, FieldMDP /P is recorded but \
    not evaluated, catalog /Perms locks are out of scope, and document-security-store and \
    timestamp updates are not specially recognized. This finding says nothing about whether \
    the signature's cryptographic digest verifies.";

/// Cap on form-tree nodes visited, so a cyclic or adversarial `/Kids` graph
/// cannot make the walk run away. Matches the fact layer's own cap.
const MAX_FORM_FIELDS: usize = 65_536;

/// One object change from a revision later than a signature's introducing
/// revision: the M2 change record plus which later save introduced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostSignatureChange {
    /// Chronological index of the later revision that introduced the change.
    pub revision: u32,
    /// The changed object as `N G R`.
    pub object: String,
    /// How the later save's declaration relates to the previous save's.
    /// Never [`ObjectChangeKind::RepeatedIdentically`]: a restated entry
    /// changed nothing.
    pub change: ObjectChangeKind,
    /// The role the object plays in the latest view.
    pub role: ObjectRole,
    /// Zero-based pages the object serves in the latest view.
    pub pages: Vec<u32>,
}

/// List the exact post-signature object changes for one signature: the M2
/// change records of every revision later than its introducing revision.
///
/// Restated entries ([`ObjectChangeKind::RepeatedIdentically`]) are not
/// changes and are skipped, as are first free declarations — a later save
/// declaring free a number that was never live changes no object. A number
/// freed after being live is a deletion and is listed.
///
/// Empty when revisions are unavailable, the signature's introducing
/// revision is unknown, or no later save changed anything.
#[must_use]
pub fn post_signature_changes(
    facts: &DocumentFacts,
    signature: &SignatureFacts,
) -> Vec<PostSignatureChange> {
    let Some(history) = facts.revisions.value() else {
        return Vec::new();
    };
    let Some(signed) = signature.revision else {
        return Vec::new();
    };
    // Whether each number is live as revisions are walked oldest first. A
    // number appears at most once per revision, so checking before
    // recording the current change reads the state the previous saves left.
    let mut live: BTreeMap<u32, bool> = BTreeMap::new();
    let mut post = Vec::new();
    for revision in &history.revisions {
        let later = revision.index > signed;
        for change in &revision.changes {
            let number = change_number(change.object.as_str());
            if later {
                let first_free = change.change == ObjectChangeKind::Freed
                    && number.is_some_and(|n| !live.get(&n).copied().unwrap_or(false));
                if change.change != ObjectChangeKind::RepeatedIdentically && !first_free {
                    post.push(PostSignatureChange {
                        revision: revision.index,
                        object: change.object.clone(),
                        change: change.change,
                        role: change.role,
                        pages: change.pages.clone(),
                    });
                }
            }
            if let Some(number) = number {
                live.insert(
                    number,
                    !matches!(change.location, ObjectLocationFacts::Free),
                );
            }
        }
    }
    post
}

/// Check every signed signature's later revisions against the DocMDP and
/// FieldMDP policies it declares.
///
/// An approval signature (no policy) and a certification policy whose later
/// changes it permits are both silent; only forbidden changes — or a policy
/// too malformed to evaluate — become [`POST_SIGNATURE_REVISION`] and
/// [`DOCMDP_POLICY_VIOLATION`] findings, in signature order with at most one
/// pair per signature.
///
/// Never fails and never panics: unavailable signatures or revisions, an
/// unknown introducing revision, and a document that cannot be reopened for
/// the form-field walk all yield fewer findings, never an error.
#[must_use]
pub fn check_post_signature_policies(
    bytes: &[u8],
    password: Option<&str>,
    facts: &DocumentFacts,
) -> Vec<Finding> {
    let Some(signatures) = facts.signatures.value() else {
        return Vec::new();
    };
    if facts.revisions.value().is_none() {
        return Vec::new();
    }
    let snapshot = open_snapshot(bytes, password);
    let form = snapshot.as_ref().map(form_inventory);
    let form = form.as_ref();
    let rests_on_recovered = facts.parse.rests_on_recovered_structure();
    let mut findings = Vec::new();
    for signature in &signatures.signatures {
        let changes = post_signature_changes(facts, signature);
        if changes.is_empty() {
            continue;
        }
        if signature.docmdp.is_none() && signature.fieldmdp.is_none() {
            // Approval signature: later revisions are the ordinary workflow.
            continue;
        }
        // Defensive: `changes` is non-empty only with a known revision, but
        // the explanations below print it, so establish it rather than
        // default it.
        let Some(signed) = signature.revision else {
            continue;
        };
        let mut forbidden: Vec<(PostSignatureChange, String)> = Vec::new();
        let mut unestablished: Vec<String> = Vec::new();
        if let Some(policy) = &signature.docmdp {
            match policy.permission {
                Some(permission) => {
                    for change in &changes {
                        match evaluate_docmdp(change, permission, form) {
                            ChangeVerdict::Permitted => {}
                            ChangeVerdict::Forbidden(reason) => {
                                push_forbidden(&mut forbidden, change, reason);
                            }
                            ChangeVerdict::Unestablished(reason) => {
                                push_unique(&mut unestablished, reason);
                            }
                        }
                    }
                }
                None => unestablished.push(
                    "DocMDP permission is unestablished (/P is missing or outside 1-3)".to_owned(),
                ),
            }
        }
        if let Some(policy) = &signature.fieldmdp {
            match lock_set(policy) {
                Some(lock) => {
                    for change in &changes {
                        match evaluate_fieldmdp(change, &lock, form) {
                            ChangeVerdict::Permitted => {}
                            ChangeVerdict::Forbidden(reason) => {
                                push_forbidden(&mut forbidden, change, reason);
                            }
                            ChangeVerdict::Unestablished(reason) => {
                                push_unique(&mut unestablished, reason);
                            }
                        }
                    }
                }
                None => unestablished.push(fieldmdp_unestablished_reason(policy)),
            }
        }
        if forbidden.is_empty() && unestablished.is_empty() {
            continue;
        }
        let name = signature.field_name.as_deref().unwrap_or("(unnamed)");
        findings.push(Finding {
            code: POST_SIGNATURE_REVISION,
            strength: EvidenceStrength::StructuralFact,
            explanation: explain_post(signature, name, signed, &changes, &unestablished),
            limitation: None,
            requires_mode: AnalysisMode::Full,
            rests_on_recovered_structure: rests_on_recovered,
        });
        if !forbidden.is_empty() {
            findings.push(Finding {
                code: DOCMDP_POLICY_VIOLATION,
                strength: EvidenceStrength::StrongDerived,
                explanation: explain_violation(signature, name, signed, &forbidden),
                limitation: Some(VIOLATION_LIMITATION.to_owned()),
                requires_mode: AnalysisMode::Full,
                rests_on_recovered_structure: rests_on_recovered,
            });
        }
    }
    findings
}

/// What one post-signature change means under one policy.
enum ChangeVerdict {
    /// The policy permits the change.
    Permitted,
    /// The policy forbids the change, with why.
    Forbidden(String),
    /// Permission could not be decided, with why. Never silently treated as
    /// either of the above.
    Unestablished(String),
}

/// One post-signature change under a DocMDP permission.
fn evaluate_docmdp(
    change: &PostSignatureChange,
    permission: DocMdpPermission,
    form: Option<&FormInventory>,
) -> ChangeVerdict {
    use ChangeVerdict::{Forbidden, Permitted, Unestablished};
    use ObjectRole::{
        Annotation, Contents, Font, Image, Metadata, Other, Page, Signature, Unknown,
    };
    if permission == DocMdpPermission::NoChanges {
        return Forbidden("no changes are permitted under DocMDP P=1".to_owned());
    }
    let p = permission_p(permission);
    if change.change == ObjectChangeKind::Freed {
        return Forbidden(format!(
            "object deletions are not permitted under DocMDP P={p}"
        ));
    }
    match change.role {
        Signature => Permitted,
        Contents | Page | Font | Image => Forbidden(format!(
            "page content changes are not permitted under DocMDP P={p}"
        )),
        Metadata => Forbidden(format!(
            "document metadata changes are not permitted under DocMDP P={p}"
        )),
        Unknown => Forbidden(format!(
            "changes that cannot be classified are not permitted under DocMDP P={p}"
        )),
        Annotation if permission == DocMdpPermission::FormFillSignAndAnnotate => Permitted,
        Annotation | Other => {
            let membership = form_membership(form, change.object.as_str());
            match membership {
                Some(true) => Permitted,
                Some(false) if change.role == Annotation => Forbidden(
                    "annotation changes are not permitted under DocMDP P=2 (they require P=3)"
                        .to_owned(),
                ),
                Some(false) => Forbidden(format!(
                    "non-form changes are not permitted under DocMDP P={p}"
                )),
                None => Unestablished(membership_unknown_reason(form, change.object.as_str())),
            }
        }
    }
}

/// Whether the changed object belongs to the form: the `/AcroForm` field
/// tree, the `/AcroForm` dictionary itself, or a field widget's appearance
/// stream. `None` when the walk never ran or the identity is unreadable.
fn form_membership(form: Option<&FormInventory>, object: &str) -> Option<bool> {
    let inventory = form?;
    let number = change_number(object)?;
    Some(
        inventory.fields.contains(&number)
            || inventory.acroform == Some(number)
            || inventory.appearances.contains(&number),
    )
}

/// Why form membership could not be decided for one change.
fn membership_unknown_reason(form: Option<&FormInventory>, object: &str) -> String {
    if form.is_none() {
        "form-field membership could not be established (the document could not be reopened \
         for the field walk)"
            .to_owned()
    } else {
        format!(
            "form-field membership of {object} could not be established (its object identity \
             is unreadable)"
        )
    }
}

/// An established FieldMDP lock set: the action plus the field names it
/// applies to (`None` for `/All`, which needs no list).
struct LockSet {
    /// Which fields the lock applies to.
    action: FieldMdpAction,
    /// The qualified field names, for `/Include` and `/Exclude`.
    fields: Option<Vec<String>>,
}

/// The lock set of a FieldMDP policy, or `None` when it cannot be
/// established: an absent or unrecognized `/Action`, or `/Include` /
/// `/Exclude` without `/Fields`.
fn lock_set(policy: &FieldMdpPolicyFacts) -> Option<LockSet> {
    let action = policy.action?;
    match action {
        FieldMdpAction::All => Some(LockSet {
            action,
            fields: None,
        }),
        FieldMdpAction::Include | FieldMdpAction::Exclude => Some(LockSet {
            action,
            fields: Some(policy.fields.clone()?),
        }),
    }
}

/// Why a FieldMDP lock set could not be established.
fn fieldmdp_unestablished_reason(policy: &FieldMdpPolicyFacts) -> String {
    if policy.action.is_none() {
        "FieldMDP lock set is unestablished (/Action is missing or unrecognized)".to_owned()
    } else {
        "FieldMDP lock set is unestablished (/Fields is missing for /Action /Include or \
         /Exclude)"
            .to_owned()
    }
}

/// Whether the lock covers a field of this qualified name.
fn locked(lock: &LockSet, name: &str) -> bool {
    let fields: &[String] = lock.fields.as_deref().unwrap_or(&[]);
    match lock.action {
        FieldMdpAction::All => true,
        FieldMdpAction::Include => fields.iter().any(|field| field == name),
        FieldMdpAction::Exclude => !fields.iter().any(|field| field == name),
    }
}

/// One post-signature change under an established FieldMDP lock set: locked
/// fields and everything outside form filling are forbidden; unlocked fields
/// are permitted.
fn evaluate_fieldmdp(
    change: &PostSignatureChange,
    lock: &LockSet,
    form: Option<&FormInventory>,
) -> ChangeVerdict {
    use ChangeVerdict::{Forbidden, Permitted, Unestablished};
    use ObjectRole::{
        Annotation, Contents, Font, Image, Metadata, Other, Page, Signature, Unknown,
    };
    if change.change == ObjectChangeKind::Freed {
        return Forbidden("object deletions are not permitted under the FieldMDP lock".to_owned());
    }
    match change.role {
        Signature => Permitted,
        Contents | Page | Font | Image => {
            Forbidden("page content changes are not permitted under the FieldMDP lock".to_owned())
        }
        Metadata => Forbidden(
            "document metadata changes are not permitted under the FieldMDP lock".to_owned(),
        ),
        Unknown => Forbidden(
            "changes that cannot be classified are not permitted under the FieldMDP lock"
                .to_owned(),
        ),
        Annotation | Other => {
            let Some(inventory) = form else {
                return Unestablished(membership_unknown_reason(form, change.object.as_str()));
            };
            let Some(number) = change_number(change.object.as_str()) else {
                return Unestablished(membership_unknown_reason(form, change.object.as_str()));
            };
            if inventory.acroform == Some(number) || inventory.appearances.contains(&number) {
                return Permitted;
            }
            if !inventory.fields.contains(&number) {
                let what = if change.role == Annotation {
                    "annotation"
                } else {
                    "non-form"
                };
                return Forbidden(format!(
                    "{what} changes are not permitted under the FieldMDP lock"
                ));
            }
            let Some(name) = inventory.names.get(&number) else {
                return Unestablished(format!(
                    "the lock status of changed object {} could not be established (its field \
                     name is unreadable)",
                    change.object
                ));
            };
            if locked(lock, name) {
                Forbidden(format!(
                    "form field '{name}' is locked by the FieldMDP policy"
                ))
            } else {
                Permitted
            }
        }
    }
}

/// Record a forbidden change, keeping the first reason when both policies
/// forbid the same object.
fn push_forbidden(
    forbidden: &mut Vec<(PostSignatureChange, String)>,
    change: &PostSignatureChange,
    reason: String,
) {
    if !forbidden
        .iter()
        .any(|(known, _)| known.revision == change.revision && known.object == change.object)
    {
        forbidden.push((change.clone(), reason));
    }
}

/// Record an unestablished reason once.
fn push_unique(reasons: &mut Vec<String>, reason: String) {
    if !reasons.iter().any(|known| known == &reason) {
        reasons.push(reason);
    }
}

/// What the `/AcroForm` field tree holds in the latest view: which objects
/// are form filling when they change.
#[derive(Debug, Default)]
struct FormInventory {
    /// Object numbers of the field tree: fields, sub-fields, and widget
    /// kids alike. A field dictionary redefined to carry a new `/V` is the
    /// form fill itself.
    fields: BTreeSet<u32>,
    /// Qualified field names (`a.b.c` from `/T` partial names) by object
    /// number, when every level on the path carried a readable name.
    names: BTreeMap<u32, String>,
    /// The `/AcroForm` dictionary's own object number, when indirect.
    acroform: Option<u32>,
    /// Appearance streams reachable from field widgets' `/AP`: filling a
    /// form regenerates these, so they are form filling, not smuggled
    /// content.
    appearances: BTreeSet<u32>,
}

/// Reopen the document for the form-field walk, exactly as the audit opened
/// it. `None` when the reopen fails; the caller degrades to unestablished
/// rather than guessing.
fn open_snapshot(bytes: &[u8], password: Option<&str>) -> Option<DocumentSnapshot> {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes.to_vec()));
    DocumentSnapshot::open_with_password(source, DocumentLimits::default(), password).ok()
}

/// Walk the `/AcroForm` field tree for membership, qualified names, and
/// widget appearance streams.
fn form_inventory(snapshot: &DocumentSnapshot) -> FormInventory {
    let mut walk = FieldWalk {
        snapshot,
        ctx: ParseContext::new(),
        inventory: FormInventory::default(),
        visited: HashSet::new(),
        seen: 0,
    };
    let root = snapshot.structure().trailer.root;
    let Ok(catalog_object) = snapshot.objects().resolve(snapshot, root, &mut walk.ctx) else {
        return walk.inventory;
    };
    let Some(catalog) = catalog_object.as_dict() else {
        return walk.inventory;
    };
    let Some(acroform_key) = snapshot.names().lookup(b"AcroForm") else {
        return walk.inventory;
    };
    let Some(form_ref) = catalog.get(acroform_key) else {
        return walk.inventory;
    };
    if let Some(id) = form_ref.as_reference() {
        walk.inventory.acroform = Some(id.number);
    }
    let Some(form_object) = walk.resolve(form_ref) else {
        return walk.inventory;
    };
    let Some(form) = form_object.as_dict() else {
        return walk.inventory;
    };
    let Some(fields_key) = snapshot.names().lookup(b"Fields") else {
        return walk.inventory;
    };
    let Some(fields_ref) = form.get(fields_key) else {
        return walk.inventory;
    };
    let Some(fields) = walk.resolve(fields_ref) else {
        return walk.inventory;
    };
    walk.node(&fields, None, String::new());
    walk.inventory
}

/// The state of one `/AcroForm` field-tree walk.
struct FieldWalk<'a> {
    /// The open document.
    snapshot: &'a DocumentSnapshot,
    /// Resolver context for the walk.
    ctx: ParseContext,
    /// The inventory being built.
    inventory: FormInventory,
    /// Field identities already visited: the cycle guard.
    visited: HashSet<ObjectId>,
    /// Nodes visited: the runaway guard.
    seen: usize,
}

impl FieldWalk<'_> {
    /// Resolve a possibly-indirect value to its target object.
    fn resolve(&mut self, value: &PdfObject) -> Option<Arc<PdfObject>> {
        match value {
            PdfObject::Reference(id) => self
                .snapshot
                .objects()
                .resolve(self.snapshot, *id, &mut self.ctx)
                .ok(),
            other => Some(Arc::new(other.clone())),
        }
    }

    /// One field-tree node: an array of fields, or a field dictionary whose
    /// `/T` extends the qualified name and whose `/Kids` recurse.
    fn node(&mut self, node: &PdfObject, id: Option<ObjectId>, prefix: String) {
        if self.seen >= MAX_FORM_FIELDS {
            return;
        }
        self.seen += 1;
        if let PdfObject::Array(entries) = node {
            for entry in entries.iter() {
                let reference = entry.as_reference();
                let Some(field) = self.resolve(entry) else {
                    continue;
                };
                self.node(&field, reference, prefix.clone());
            }
            return;
        }
        let Some(dict) = node.as_dict() else {
            return;
        };
        if let Some(id) = id {
            if !self.visited.insert(id) {
                return;
            }
            self.inventory.fields.insert(id.number);
        }
        let partial = self.partial_name(dict);
        let qualified = match (prefix.is_empty(), partial) {
            (true, partial) => partial,
            (false, Some(partial)) => Some(format!("{prefix}.{partial}")),
            (false, None) => Some(prefix.clone()),
        };
        let child_prefix = qualified.clone().unwrap_or_else(|| prefix.clone());
        if let (Some(id), Some(qualified)) = (id, qualified) {
            self.inventory.names.insert(id.number, qualified);
        }
        if let Some(appearance) = self.named(dict, b"AP") {
            self.appearances(&appearance);
        }
        if let Some(kids_key) = self.snapshot.names().lookup(b"Kids")
            && let Some(kids_ref) = dict.get(kids_key)
        {
            let reference = kids_ref.as_reference();
            if let Some(kids) = self.resolve(kids_ref) {
                self.node(&kids, reference, child_prefix);
            }
        }
    }

    /// A dictionary entry resolved to its target object, when the key exists
    /// and the value resolves.
    fn named(&mut self, dict: &Dictionary, key: &[u8]) -> Option<Arc<PdfObject>> {
        let key = self.snapshot.names().lookup(key)?;
        let value = dict.get(key)?;
        self.resolve(value)
    }

    /// A field dictionary's `/T` partial name, resolved and decoded lossy.
    fn partial_name(&mut self, dict: &Dictionary) -> Option<String> {
        let resolved = self.named(dict, b"T")?;
        match resolved.as_ref() {
            PdfObject::String(text) => Some(decode_text_string_lossy(text.as_bytes())),
            _ => None,
        }
    }

    /// Object numbers reachable from a widget's `/AP` dictionary: appearance
    /// streams, plus one level of appearance-state subdictionaries.
    fn appearances(&mut self, appearance: &PdfObject) {
        let Some(dict) = appearance.as_dict() else {
            return;
        };
        for (_, value) in dict.iter() {
            if let Some(id) = value.as_reference() {
                self.inventory.appearances.insert(id.number);
                continue;
            }
            let Some(resolved) = self.resolve(value) else {
                continue;
            };
            let Some(states) = resolved.as_dict() else {
                continue;
            };
            for (_, state) in states.iter() {
                if let Some(id) = state.as_reference() {
                    self.inventory.appearances.insert(id.number);
                }
            }
        }
    }
}

/// The object number of a change record's `N G R` identity.
fn change_number(identity: &str) -> Option<u32> {
    identity.split_whitespace().next()?.parse().ok()
}

/// The `/P` number behind a permission.
fn permission_p(permission: DocMdpPermission) -> i64 {
    match permission {
        DocMdpPermission::NoChanges => 1,
        DocMdpPermission::FormFillAndSign => 2,
        DocMdpPermission::FormFillSignAndAnnotate => 3,
    }
}

/// What a DocMDP permission allows, as a noun phrase.
fn docmdp_allows(permission: DocMdpPermission) -> &'static str {
    match permission {
        DocMdpPermission::NoChanges => "no changes",
        DocMdpPermission::FormFillAndSign => "only form filling and signing",
        DocMdpPermission::FormFillSignAndAnnotate => {
            "only form filling, signing, and annotation changes"
        }
    }
}

/// The role as a noun: `contents`, `annotation`, and so on.
fn role_name(role: ObjectRole) -> &'static str {
    match role {
        ObjectRole::Page => "page",
        ObjectRole::Contents => "contents",
        ObjectRole::Font => "font",
        ObjectRole::Image => "image",
        ObjectRole::Annotation => "annotation",
        ObjectRole::Signature => "signature",
        ObjectRole::Metadata => "metadata",
        ObjectRole::Other => "other",
        ObjectRole::Unknown => "unknown",
    }
}

/// The change kind as a verb. Restated entries never reach a post-signature
/// list, but the phrase exists so the arm names rather than panics.
fn kind_name(kind: ObjectChangeKind) -> &'static str {
    match kind {
        ObjectChangeKind::Added => "added",
        ObjectChangeKind::Redefined => "redefined",
        ObjectChangeKind::Freed => "freed",
        ObjectChangeKind::RepeatedIdentically => "repeated identically",
    }
}

/// The policies one signature declares, as a noun phrase: `DocMDP P=2 (only
/// form filling and signing)` and/or `FieldMDP (fields ["a"] locked)`, with
/// unestablished policies named as such.
fn policy_summary(signature: &SignatureFacts) -> String {
    let mut parts = Vec::new();
    if let Some(policy) = &signature.docmdp {
        match policy.permission {
            Some(permission) => parts.push(format!(
                "DocMDP P={} ({})",
                permission_p(permission),
                docmdp_allows(permission)
            )),
            None => parts.push("DocMDP with unestablished permission".to_owned()),
        }
    }
    if let Some(policy) = &signature.fieldmdp {
        parts.push(fieldmdp_summary(policy));
    }
    parts.join(" and ")
}

/// The FieldMDP lock as a noun phrase.
fn fieldmdp_summary(policy: &FieldMdpPolicyFacts) -> String {
    match (policy.action, policy.fields.as_deref()) {
        (Some(FieldMdpAction::All), _) => "FieldMDP (all fields locked)".to_owned(),
        (Some(FieldMdpAction::Include), Some(fields)) => {
            format!("FieldMDP (fields {fields:?} locked)")
        }
        (Some(FieldMdpAction::Exclude), Some(fields)) => {
            format!("FieldMDP (all fields except {fields:?} locked)")
        }
        _ => "FieldMDP with unestablished lock set".to_owned(),
    }
}

/// The post-signature changes grouped by later revision: `rev 1: 11 0 R
/// redefined (contents), 12 0 R added (other); rev 2: ...`.
fn describe_changes(changes: &[PostSignatureChange]) -> String {
    describe_grouped(changes, |change| {
        format!(
            "{} {} ({})",
            change.object,
            kind_name(change.change),
            role_name(change.role)
        )
    })
}

/// The forbidden changes grouped by later revision, each with its reason:
/// `rev 1: 11 0 R redefined (contents): page content changes are not
/// permitted under DocMDP P=2`.
fn describe_forbidden(forbidden: &[(PostSignatureChange, String)]) -> String {
    let changes: Vec<PostSignatureChange> =
        forbidden.iter().map(|(change, _)| change.clone()).collect();
    let mut reasons: BTreeMap<(u32, String), &str> = BTreeMap::new();
    for (change, reason) in forbidden {
        reasons
            .entry((change.revision, change.object.clone()))
            .or_insert(reason.as_str());
    }
    describe_grouped(changes.as_slice(), |change| {
        let reason = reasons
            .get(&(change.revision, change.object.clone()))
            .copied()
            .unwrap_or("not permitted");
        format!(
            "{} {} ({}): {reason}",
            change.object,
            kind_name(change.change),
            role_name(change.role)
        )
    })
}

/// Group revision-ordered changes as `rev N: item, item; rev M: item`.
fn describe_grouped(
    changes: &[PostSignatureChange],
    mut item: impl FnMut(&PostSignatureChange) -> String,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<u32> = None;
    let mut items: Vec<String> = Vec::new();
    for change in changes {
        if current != Some(change.revision) {
            if let Some(revision) = current {
                parts.push(format!("rev {revision}: {}", items.join(", ")));
                items.clear();
            }
            current = Some(change.revision);
        }
        items.push(item(change));
    }
    if let Some(revision) = current {
        parts.push(format!("rev {revision}: {}", items.join(", ")));
    }
    parts.join("; ")
}

/// The structural observation: which later revisions changed which objects
/// under which policy.
fn explain_post(
    signature: &SignatureFacts,
    name: &str,
    signed: u32,
    changes: &[PostSignatureChange],
    unestablished: &[String],
) -> String {
    let mut out = format!(
        "signature '{name}' (revision {signed}) declaring {} has later revisions with object \
         changes: {}",
        policy_summary(signature),
        describe_changes(changes)
    );
    if !unestablished.is_empty() {
        out.push_str("; permission could not be fully established: ");
        out.push_str(unestablished.join("; ").as_str());
    }
    out
}

/// The policy judgement: which post-signature changes the policy forbids.
fn explain_violation(
    signature: &SignatureFacts,
    name: &str,
    signed: u32,
    forbidden: &[(PostSignatureChange, String)],
) -> String {
    format!(
        "signature '{name}' (revision {signed}) declares {}, but later revisions changed: {}",
        policy_summary(signature),
        describe_forbidden(forbidden)
    )
}
