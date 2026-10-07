//! Object roles and page associations from the resolved latest view.
//!
//! Roles answer "what is this object for": page dictionaries, content
//! streams, fonts, images, annotations, signatures, and metadata. They are
//! read through the document snapshot's resolvers — page tree, resources,
//! annotations, and form fields — never through new parsers, and they
//! always describe the latest revision: an object redefined across saves
//! keeps the role its newest definition plays.
//!
//! The walk needs an open document, so a structure-only run gets no roles.
//! That is an explicit [`ObjectRole::Unknown`], never a silent default:
//! an encrypted object is not "other", it is out of reach.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use pdf_document::{DocumentSnapshot, PageIndex, ParseContext};
use pdf_object::{Dictionary, ObjectId, PdfObject};
use pdf_read::{FontRecord, ImageRecord};
use pdf_structure::ObjectLocation;

use crate::document::ObjectRole;

/// Cap on form fields visited, so a cyclic or adversarial `/Kids` graph
/// cannot make the walk run away. Matches the fact layer's own cap.
const MAX_FORM_FIELDS: usize = 65_536;

/// Roles and page associations by object number.
///
/// Keyed by number rather than full identity: generations change across
/// redefinitions, but the role an object plays follows its number.
#[derive(Debug, Clone, Default)]
pub(crate) struct RoleMap {
    /// The most page-specific role each reached object plays.
    pub roles: BTreeMap<u32, ObjectRole>,
    /// The pages each reached object serves, ascending.
    pub pages: BTreeMap<u32, Vec<u32>>,
}

/// Walk the snapshot for roles and page associations.
///
/// `fonts` and `images` are the fact layer's compiled-page inventories:
/// a font or image the renderer reached through nested form resources —
/// past this walk's one-level `/Resources` read — still earns its role
/// from them, though without page associations.
pub(crate) fn build_role_map(
    snapshot: &DocumentSnapshot,
    fonts: &[FontRecord],
    images: &[ImageRecord],
) -> RoleMap {
    let mut map = RoleMap::default();
    let mut ctx = ParseContext::new();
    for index in 0..snapshot.page_count() {
        walk_page(snapshot, &mut ctx, &mut map, index);
    }
    walk_signatures(snapshot, &mut ctx, &mut map);
    walk_metadata(snapshot, &mut ctx, &mut map);
    for font in fonts {
        if let Some(id) = font.object {
            assign(&mut map.roles, id.number, ObjectRole::Font);
        }
    }
    for image in images {
        if let Some(id) = image.object {
            assign(&mut map.roles, id.number, ObjectRole::Image);
        }
    }
    map
}

/// The role for an object the walk did not reach: [`ObjectRole::Other`]
/// when the declared identity has a live definition that resolves,
/// [`ObjectRole::Unknown`] when it is free in the latest view or does
/// not resolve. A freed object resolves — to null — so the merged
/// location is checked first: "free" is not a role.
pub(crate) fn default_role(snapshot: &DocumentSnapshot, id: ObjectId) -> ObjectRole {
    if matches!(snapshot.structure().xref.locate(id), ObjectLocation::Free) {
        return ObjectRole::Unknown;
    }
    let mut ctx = ParseContext::new();
    match snapshot.objects().resolve(snapshot, id, &mut ctx) {
        Ok(_) => ObjectRole::Other,
        Err(_) => ObjectRole::Unknown,
    }
}

/// Record that `number` plays `role`, keeping the most page-specific role
/// when one object matches several.
fn assign(roles: &mut BTreeMap<u32, ObjectRole>, number: u32, role: ObjectRole) {
    let rank = |role: ObjectRole| match role {
        ObjectRole::Page => 0,
        ObjectRole::Contents => 1,
        ObjectRole::Annotation => 2,
        ObjectRole::Signature => 3,
        ObjectRole::Font => 4,
        ObjectRole::Image => 5,
        ObjectRole::Metadata => 6,
        ObjectRole::Other | ObjectRole::Unknown => 7,
    };
    match roles.get(&number) {
        Some(&current) if rank(current) <= rank(role) => {}
        _ => {
            roles.insert(number, role);
        }
    }
}

/// Record that `number` serves `page`.
fn serves(pages: &mut BTreeMap<u32, BTreeSet<u32>>, number: u32, page: u32) {
    pages.entry(number).or_default().insert(page);
}

/// One page's dictionary, contents, annotations, and one level of
/// resources. `/Resources` here is the snapshot's inherited value, so
/// resource dictionaries on `/Pages` ancestors are covered; resources
/// nested inside form XObjects are not (the compiled-page inventories
/// cover those roles, without page associations).
fn walk_page(snapshot: &DocumentSnapshot, ctx: &mut ParseContext, map: &mut RoleMap, index: u32) {
    let Ok(page) = snapshot.page(PageIndex(index)) else {
        return;
    };
    let mut page_sets: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    if let Some(id) = page.object {
        assign(&mut map.roles, id.number, ObjectRole::Page);
        serves(&mut page_sets, id.number, index);
    }
    match &page.contents {
        Some(PdfObject::Reference(id)) => {
            assign(&mut map.roles, id.number, ObjectRole::Contents);
            serves(&mut page_sets, id.number, index);
        }
        Some(PdfObject::Array(items)) => {
            for item in items.iter() {
                if let Some(id) = item.as_reference() {
                    assign(&mut map.roles, id.number, ObjectRole::Contents);
                    serves(&mut page_sets, id.number, index);
                }
            }
        }
        _ => {}
    }
    for annotation in page.annotations.iter() {
        if let Some(id) = annotation.source {
            assign(&mut map.roles, id.number, ObjectRole::Annotation);
            serves(&mut page_sets, id.number, index);
        }
    }
    if let Some(resources) = page
        .resources
        .as_ref()
        .and_then(|value| resolve_value(snapshot, value, ctx))
        && let Some(dict) = resources.as_dict()
    {
        walk_font_resources(snapshot, ctx, &mut map.roles, &mut page_sets, dict, index);
        walk_xobject_resources(snapshot, ctx, &mut map.roles, &mut page_sets, dict, index);
    }
    for (number, set) in page_sets {
        map.pages.entry(number).or_default().extend(set);
    }
}

/// The page's `/Resources /Font` entries: every indirect value is a font
/// serving this page.
fn walk_font_resources(
    snapshot: &DocumentSnapshot,
    ctx: &mut ParseContext,
    roles: &mut BTreeMap<u32, ObjectRole>,
    pages: &mut BTreeMap<u32, BTreeSet<u32>>,
    resources: &Dictionary,
    index: u32,
) {
    let fonts = font_or_xobject(snapshot, ctx, resources, b"Font");
    let Some(fonts) = fonts.as_ref().and_then(|fonts| fonts.as_dict()) else {
        return;
    };
    for (_, value) in fonts.iter() {
        if let Some(id) = value.as_reference() {
            assign(roles, id.number, ObjectRole::Font);
            serves(pages, id.number, index);
        }
    }
}

/// The page's `/Resources /XObject` entries: every indirect value serves
/// this page, and one whose target declares `/Subtype /Image` is an
/// image. Form XObjects keep whatever role (if any) they earn elsewhere.
fn walk_xobject_resources(
    snapshot: &DocumentSnapshot,
    ctx: &mut ParseContext,
    roles: &mut BTreeMap<u32, ObjectRole>,
    pages: &mut BTreeMap<u32, BTreeSet<u32>>,
    resources: &Dictionary,
    index: u32,
) {
    let xobjects = font_or_xobject(snapshot, ctx, resources, b"XObject");
    let Some(xobjects) = xobjects.as_ref().and_then(|xobjects| xobjects.as_dict()) else {
        return;
    };
    for (_, value) in xobjects.iter() {
        let Some(id) = value.as_reference() else {
            continue;
        };
        serves(pages, id.number, index);
        if xobject_is_image(snapshot, ctx, id) {
            assign(roles, id.number, ObjectRole::Image);
        }
    }
}

/// A `/Resources` subdictionary (`/Font`, `/XObject`), resolved when
/// indirect.
fn font_or_xobject(
    snapshot: &DocumentSnapshot,
    ctx: &mut ParseContext,
    resources: &Dictionary,
    key: &[u8],
) -> Option<Arc<PdfObject>> {
    let id = snapshot.names().lookup(key)?;
    let value = resources.get(id)?;
    resolve_value(snapshot, value, ctx)
}

/// Whether the XObject resolves to a dictionary declaring
/// `/Subtype /Image`.
fn xobject_is_image(snapshot: &DocumentSnapshot, ctx: &mut ParseContext, id: ObjectId) -> bool {
    let names = snapshot.names();
    let Ok(target) = snapshot.objects().resolve(snapshot, id, ctx) else {
        return false;
    };
    let Some(dict) = target.as_dict() else {
        return false;
    };
    let Some(subtype) = names.lookup(b"Subtype") else {
        return false;
    };
    dict.get(subtype)
        .and_then(PdfObject::as_name)
        .is_some_and(|name| names.resolve(name).as_ref() == b"Image")
}

/// Signature form fields and their value dictionaries. Document-level:
/// no pages are recorded.
fn walk_signatures(snapshot: &DocumentSnapshot, ctx: &mut ParseContext, map: &mut RoleMap) {
    let root = snapshot.structure().trailer.root;
    let Ok(catalog) = snapshot.objects().resolve(snapshot, root, ctx) else {
        return;
    };
    let Some(catalog) = catalog.as_dict() else {
        return;
    };
    let Some(fields) = snapshot
        .names()
        .lookup(b"AcroForm")
        .and_then(|key| catalog.get(key))
        .and_then(|form| resolve_value(snapshot, form, ctx))
        .and_then(|form| {
            let fields = form.as_dict()?.get(snapshot.names().lookup(b"Fields")?)?;
            resolve_value(snapshot, fields, ctx)
        })
    else {
        return;
    };
    let mut visited = HashSet::new();
    visit_field(
        snapshot,
        ctx,
        &mut map.roles,
        None,
        &fields,
        &mut visited,
        &mut 0,
    );
}

/// One form-field node: an array of fields, or a field dictionary whose
/// `/FT` may mark it a signature and whose `/Kids` recurse.
fn visit_field(
    snapshot: &DocumentSnapshot,
    ctx: &mut ParseContext,
    roles: &mut BTreeMap<u32, ObjectRole>,
    id: Option<ObjectId>,
    node: &PdfObject,
    visited: &mut HashSet<ObjectId>,
    seen: &mut usize,
) {
    if *seen >= MAX_FORM_FIELDS {
        return;
    }
    *seen += 1;
    if let PdfObject::Array(entries) = node {
        for entry in entries.iter() {
            let Some(field) = resolve_value(snapshot, entry, ctx) else {
                continue;
            };
            visit_field(
                snapshot,
                ctx,
                roles,
                entry.as_reference(),
                &field,
                visited,
                seen,
            );
        }
        return;
    }
    let Some(field) = node.as_dict() else {
        return;
    };
    if let Some(id) = id
        && !visited.insert(id)
    {
        return;
    }
    if field_is_signature(snapshot, field) {
        if let Some(id) = id {
            assign(roles, id.number, ObjectRole::Signature);
        }
        if let Some(value) = snapshot.names().lookup(b"V").and_then(|key| field.get(key))
            && let Some(target) = value.as_reference()
        {
            assign(roles, target.number, ObjectRole::Signature);
        }
    }
    let kids_value = snapshot
        .names()
        .lookup(b"Kids")
        .and_then(|key| field.get(key));
    if let Some(kids_value) = kids_value
        && let Some(kids) = resolve_value(snapshot, kids_value, ctx)
    {
        visit_field(
            snapshot,
            ctx,
            roles,
            kids_value.as_reference(),
            &kids,
            visited,
            seen,
        );
    }
}

/// Whether the field dictionary declares `/FT /Sig`.
fn field_is_signature(snapshot: &DocumentSnapshot, field: &Dictionary) -> bool {
    let names = snapshot.names();
    names
        .lookup(b"FT")
        .and_then(|key| field.get(key))
        .and_then(PdfObject::as_name)
        .is_some_and(|name| names.resolve(name).as_ref() == b"Sig")
}

/// The `/Info` dictionary and the catalog's XMP `/Metadata` stream.
/// Document-level: no pages are recorded.
fn walk_metadata(snapshot: &DocumentSnapshot, ctx: &mut ParseContext, map: &mut RoleMap) {
    if let Some(id) = snapshot.structure().trailer.info {
        assign(&mut map.roles, id.number, ObjectRole::Metadata);
    }
    let root = snapshot.structure().trailer.root;
    let Ok(catalog) = snapshot.objects().resolve(snapshot, root, ctx) else {
        return;
    };
    let Some(catalog) = catalog.as_dict() else {
        return;
    };
    let names = snapshot.names();
    if let Some(id) = names
        .lookup(b"Metadata")
        .and_then(|key| catalog.get(key))
        .and_then(PdfObject::as_reference)
    {
        assign(&mut map.roles, id.number, ObjectRole::Metadata);
    }
}

/// Resolve a possibly-indirect value to its target object.
fn resolve_value(
    snapshot: &DocumentSnapshot,
    value: &PdfObject,
    ctx: &mut ParseContext,
) -> Option<Arc<PdfObject>> {
    match value {
        PdfObject::Reference(id) => snapshot.objects().resolve(snapshot, *id, ctx).ok(),
        other => Some(Arc::new(other.clone())),
    }
}
