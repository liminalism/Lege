//! Reading document-level dictionaries the engine has no typed view of:
//! page labels and interactive form fields.
//!
//! Everything here is defensive in the same way the engine is: a value of
//! the wrong type is skipped, a tree is walked to a bounded depth and node
//! count, and a cycle ends the walk instead of the process.

use std::collections::HashSet;
use std::sync::Arc;

use pdf_document::{DocumentSnapshot, ParseContext};
use pdf_object::{Dictionary, ObjectId, PdfObject};

/// Nodes visited in any one tree walk.
const MAX_NODES: usize = 65_536;
/// Depth of any one tree walk.
const MAX_DEPTH: usize = 48;

/// A view of one document's objects with the lookups the walkers need.
struct Objects<'a> {
    snapshot: &'a DocumentSnapshot,
    ctx: ParseContext,
}

impl<'a> Objects<'a> {
    fn new(snapshot: &'a DocumentSnapshot) -> Self {
        Self {
            snapshot,
            ctx: crate::commands::parse_context(),
        }
    }

    fn resolve(&mut self, id: ObjectId) -> Option<Arc<PdfObject>> {
        self.snapshot
            .objects()
            .resolve(self.snapshot, id, &mut self.ctx)
            .ok()
    }

    /// `value`, followed through a reference if it is one.
    fn deref(&mut self, value: &PdfObject) -> Option<Arc<PdfObject>> {
        match value {
            PdfObject::Reference(id) => self.resolve(*id),
            other => Some(Arc::new(other.clone())),
        }
    }

    /// `dict[key]`, resolved.
    fn get(&mut self, dict: &Dictionary, key: &[u8]) -> Option<Arc<PdfObject>> {
        let key = self.snapshot.names().lookup(key)?;
        let value = dict.get(key)?.clone();
        self.deref(&value)
    }

    fn catalog(&mut self) -> Option<Arc<PdfObject>> {
        self.resolve(self.snapshot.structure().trailer.root)
    }

    fn text(&mut self, dict: &Dictionary, key: &[u8]) -> Option<String> {
        match &*self.get(dict, key)? {
            PdfObject::String(text) => Some(pdf_object::decode_text_string_lossy(text.as_bytes())),
            _ => None,
        }
    }

    fn name(&mut self, dict: &Dictionary, key: &[u8]) -> Option<String> {
        let id = self.get(dict, key)?.as_name()?;
        Some(String::from_utf8_lossy(&self.snapshot.names().resolve(id)).into_owned())
    }

    /// A value as a reader would write it down: text, a name, a number, or
    /// a list of those.
    fn display(&mut self, value: &PdfObject, depth: usize) -> Option<String> {
        let value = self.deref(value)?;
        match &*value {
            PdfObject::String(text) => Some(pdf_object::decode_text_string_lossy(text.as_bytes())),
            PdfObject::Name(id) => {
                Some(String::from_utf8_lossy(&self.snapshot.names().resolve(*id)).into_owned())
            }
            PdfObject::Integer(number) => Some(number.to_string()),
            PdfObject::Real(number) => Some(number.to_string()),
            PdfObject::Boolean(flag) => Some(flag.to_string()),
            PdfObject::Array(items) if depth < 4 => {
                let parts: Vec<String> = items
                    .iter()
                    .take(64)
                    .filter_map(|item| self.display(item, depth + 1))
                    .collect();
                Some(parts.join(", "))
            }
            _ => None,
        }
    }
}

// ── page labels ─────────────────────────────────────────────────────────────

/// A run of pages numbered the same way, from `/PageLabels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRange {
    /// Zero-based index of the first page of the run.
    pub start: u32,
    /// `D`, `R`, `r`, `A`, `a`, or `None` for a prefix with no number.
    pub style: Option<char>,
    pub prefix: String,
    /// The number the first page of the run carries.
    pub first: u32,
}

/// The document's page-label ranges, in page order. Empty when it declares
/// none, in which case pages are simply numbered from 1.
pub fn page_labels(snapshot: &DocumentSnapshot) -> Vec<LabelRange> {
    let mut objects = Objects::new(snapshot);
    let Some(catalog) = objects.catalog() else {
        return Vec::new();
    };
    let Some(root) = catalog
        .as_dict()
        .and_then(|catalog| objects.get(catalog, b"PageLabels"))
    else {
        return Vec::new();
    };
    let mut ranges = Vec::new();
    let mut budget = MAX_NODES;
    collect_labels(&mut objects, &root, 0, &mut budget, &mut ranges);
    ranges.sort_by_key(|range: &LabelRange| range.start);
    ranges.dedup_by_key(|range| range.start);
    ranges
}

fn collect_labels(
    objects: &mut Objects<'_>,
    node: &PdfObject,
    depth: usize,
    budget: &mut usize,
    out: &mut Vec<LabelRange>,
) {
    if depth > MAX_DEPTH || *budget == 0 {
        return;
    }
    *budget -= 1;
    let Some(node) = node.as_dict() else {
        return;
    };
    if let Some(nums) = objects.get(node, b"Nums")
        && let PdfObject::Array(entries) = &*nums
    {
        for pair in entries.chunks(2) {
            let [index, label] = pair else { continue };
            let Some(start) = objects
                .deref(index)
                .and_then(|index| index.as_int())
                .and_then(|index| u32::try_from(index).ok())
            else {
                continue;
            };
            let Some(label) = objects.deref(label) else {
                continue;
            };
            let Some(label) = label.as_dict() else {
                continue;
            };
            out.push(LabelRange {
                start,
                style: objects
                    .name(label, b"S")
                    .and_then(|style| style.chars().next())
                    .filter(|style| matches!(style, 'D' | 'R' | 'r' | 'A' | 'a')),
                prefix: objects.text(label, b"P").unwrap_or_default(),
                first: objects
                    .get(label, b"St")
                    .and_then(|first| first.as_int())
                    .and_then(|first| u32::try_from(first).ok())
                    .filter(|first| *first >= 1)
                    .unwrap_or(1),
            });
        }
    }
    if let Some(kids) = objects.get(node, b"Kids")
        && let PdfObject::Array(kids) = &*kids
    {
        for kid in kids.iter() {
            if let Some(kid) = objects.deref(kid) {
                collect_labels(objects, &kid, depth + 1, budget, out);
            }
        }
    }
}

/// The label the page at zero-based `index` carries, or `None` when the
/// document declares no labels (or none reaching this page).
pub fn label_for(ranges: &[LabelRange], index: u32) -> Option<String> {
    let at = ranges.partition_point(|range| range.start <= index);
    let range = ranges.get(at.checked_sub(1)?)?;
    let number = range.first.saturating_add(index - range.start);
    let numbered = match range.style {
        Some('D') => number.to_string(),
        Some('R') => roman(number),
        Some('r') => roman(number).to_lowercase(),
        Some('A') => letters(number),
        Some('a') => letters(number).to_lowercase(),
        _ => String::new(),
    };
    Some(format!("{}{numbered}", range.prefix))
}

fn roman(mut number: u32) -> String {
    const PARTS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    // Past a few thousand the numeral is a wall of Ms; nobody labels so.
    if number == 0 || number > 4999 {
        return number.to_string();
    }
    let mut out = String::new();
    for (value, numeral) in PARTS {
        while number >= value {
            out.push_str(numeral);
            number -= value;
        }
    }
    out
}

/// `A`..`Z`, then `AA`..`ZZ`, `AAA`.. — the letter repeated, per the PDF
/// reference.
fn letters(number: u32) -> String {
    if number == 0 {
        return String::new();
    }
    let letter = char::from(b'A' + ((number - 1) % 26) as u8);
    let repeat = ((number - 1) / 26 + 1).min(8) as usize;
    std::iter::repeat_n(letter, repeat).collect()
}

// ── form fields ─────────────────────────────────────────────────────────────

/// One terminal field of the document's interactive form.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FormField {
    /// Fully qualified name: the `/T` of every ancestor, joined with dots.
    pub name: String,
    /// `text`, `button`, `checkbox`, `radio`, `choice`, `signature`, or the
    /// raw `/FT` when it is none of those.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// `/TU`: the label a viewer reads out or shows as a tooltip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The choices of a list or combo box.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    pub read_only: bool,
    pub required: bool,
    /// One-based page of the field's first widget, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::views::rounded_box_opt"
    )]
    pub rect: Option<[f64; 4]>,
}

/// What a field inherits from the nodes above it.
#[derive(Clone, Default)]
struct Inherited {
    name: String,
    kind: Option<String>,
    flags: i64,
    value: Option<String>,
    default_value: Option<String>,
}

/// The terminal fields of the document's AcroForm, in tree order, and
/// whether the walk hit its node limit.
pub fn form_fields(snapshot: &DocumentSnapshot, limit: usize) -> (Vec<FormField>, bool) {
    let mut objects = Objects::new(snapshot);
    let fields = (|| {
        let catalog = objects.catalog()?;
        let acroform = objects.get(catalog.as_dict()?, b"AcroForm")?;
        objects.get(acroform.as_dict()?, b"Fields")
    })();
    let Some(fields) = fields else {
        return (Vec::new(), false);
    };

    // Widget annotation → the page it sits on and its rectangle.
    let mut widgets = std::collections::HashMap::new();
    for index in 0..snapshot.page_count() {
        let Ok(page) = snapshot.page(pdf_document::PageIndex(index)) else {
            continue;
        };
        for annotation in page.annotations.iter() {
            if let Some(source) = annotation.source {
                widgets
                    .entry(source)
                    .or_insert((index + 1, annotation.rect));
            }
        }
    }

    let mut out = Vec::new();
    let mut walk = FieldWalk {
        widgets,
        seen: HashSet::new(),
        budget: MAX_NODES,
        limit,
        cut: false,
    };
    walk.fields(
        &mut objects,
        &fields,
        None,
        &Inherited::default(),
        0,
        &mut out,
    );
    (out, walk.cut)
}

struct FieldWalk {
    widgets: std::collections::HashMap<ObjectId, (u32, [f64; 4])>,
    seen: HashSet<ObjectId>,
    budget: usize,
    limit: usize,
    cut: bool,
}

impl FieldWalk {
    fn fields(
        &mut self,
        objects: &mut Objects<'_>,
        node: &PdfObject,
        id: Option<ObjectId>,
        parent: &Inherited,
        depth: usize,
        out: &mut Vec<FormField>,
    ) {
        if depth > MAX_DEPTH || self.budget == 0 {
            self.cut = true;
            return;
        }
        self.budget -= 1;
        match node {
            PdfObject::Array(entries) => {
                for entry in entries.iter() {
                    let id = match entry {
                        PdfObject::Reference(id) => Some(*id),
                        _ => None,
                    };
                    if id.is_some_and(|id| !self.seen.insert(id)) {
                        continue;
                    }
                    if let Some(entry) = objects.deref(entry) {
                        self.fields(objects, &entry, id, parent, depth + 1, out);
                    }
                }
            }
            PdfObject::Dictionary(dict) => self.field(objects, dict, id, parent, depth, out),
            _ => {}
        }
    }

    fn field(
        &mut self,
        objects: &mut Objects<'_>,
        dict: &Dictionary,
        id: Option<ObjectId>,
        parent: &Inherited,
        depth: usize,
        out: &mut Vec<FormField>,
    ) {
        let partial = objects.text(dict, b"T");
        let mut here = parent.clone();
        if let Some(partial) = &partial {
            if !here.name.is_empty() {
                here.name.push('.');
            }
            here.name.push_str(partial);
        }
        if let Some(kind) = objects.name(dict, b"FT") {
            here.kind = Some(kind);
        }
        if let Some(flags) = objects.get(dict, b"Ff").and_then(|flags| flags.as_int()) {
            here.flags = flags;
        }
        let names = objects.snapshot.names();
        let raw = |key: &[u8]| names.lookup(key).and_then(|key| dict.get(key)).cloned();
        if let Some(value) = raw(b"V").and_then(|value| objects.display(&value, 0)) {
            here.value = Some(value);
        }
        if let Some(value) = raw(b"DV").and_then(|value| objects.display(&value, 0)) {
            here.default_value = Some(value);
        }

        // Kids that carry a `/T` are fields of their own; kids without one
        // are this field's widgets.
        let kids = objects.get(dict, b"Kids");
        if let Some(PdfObject::Array(kids)) = kids.as_deref() {
            let child_fields = kids.iter().any(|kid| {
                objects
                    .deref(kid)
                    .and_then(|kid| kid.as_dict().map(|kid| objects.text(kid, b"T").is_some()))
                    .unwrap_or(false)
            });
            if child_fields {
                let kids = PdfObject::Array(Arc::clone(kids));
                self.fields(objects, &kids, None, &here, depth + 1, out);
                return;
            }
        }
        let Some(kind) = here.kind.clone() else {
            return;
        };
        if self.limit > 0 && out.len() >= self.limit {
            self.cut = true;
            return;
        }

        // Where it is: the field itself when it is its own widget, else its
        // first widget kid.
        let mut place = id.and_then(|id| self.widgets.get(&id).copied());
        if place.is_none()
            && let Some(PdfObject::Array(kids)) = kids.as_deref()
        {
            place = kids.iter().find_map(|kid| match kid {
                PdfObject::Reference(id) => self.widgets.get(id).copied(),
                _ => None,
            });
        }

        let options = match objects.get(dict, b"Opt").as_deref() {
            Some(PdfObject::Array(options)) => options
                .iter()
                .take(256)
                .filter_map(|option| match objects.deref(option).as_deref() {
                    // An option is its text, or [export value, text].
                    Some(PdfObject::Array(pair)) => {
                        pair.last().and_then(|text| objects.display(text, 0))
                    }
                    Some(other) => objects.display(other, 0),
                    None => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let flag = |bit: u32| here.flags & (1 << (bit - 1)) != 0;
        let kind = match kind.as_str() {
            "Tx" => "text".to_owned(),
            "Ch" => "choice".to_owned(),
            "Sig" => "signature".to_owned(),
            "Btn" if flag(17) => "button".to_owned(),
            "Btn" if flag(16) => "radio".to_owned(),
            "Btn" => "checkbox".to_owned(),
            other => other.to_owned(),
        };
        out.push(FormField {
            name: here.name.clone(),
            kind,
            value: here.value.clone(),
            default_value: here.default_value.clone(),
            label: objects.text(dict, b"TU"),
            options,
            read_only: flag(1),
            required: flag(2),
            page: place.map(|(page, _)| page),
            rect: place.map(|(_, rect)| rect),
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn labels_follow_the_style_and_start_of_their_range() {
        let ranges = [
            LabelRange {
                start: 0,
                style: Some('r'),
                prefix: String::new(),
                first: 1,
            },
            LabelRange {
                start: 4,
                style: Some('D'),
                prefix: String::new(),
                first: 1,
            },
            LabelRange {
                start: 10,
                style: Some('D'),
                prefix: "A-".into(),
                first: 7,
            },
            LabelRange {
                start: 12,
                style: None,
                prefix: "Index".into(),
                first: 1,
            },
        ];
        let label = |index| label_for(&ranges, index).unwrap();
        assert_eq!(label(0), "i");
        assert_eq!(label(3), "iv");
        assert_eq!(label(4), "1");
        assert_eq!(label(9), "6");
        assert_eq!(label(11), "A-8");
        assert_eq!(label(12), "Index");
        assert_eq!(label_for(&[], 0), None);
        // A document whose first range starts late leaves the pages before
        // it unlabelled.
        assert_eq!(label_for(&ranges[1..], 2), None);
    }

    #[test]
    fn roman_numerals_and_letters() {
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(roman(49), "XLIX");
        assert_eq!(letters(1), "A");
        assert_eq!(letters(26), "Z");
        assert_eq!(letters(27), "AA");
        assert_eq!(letters(53), "AAA");
    }
}
