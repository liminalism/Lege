//! Write `/Title` and `/Author` into an existing PDF as an incremental update.
//!
//! The original bytes are kept verbatim and a new revision is appended: a
//! replacement Info dictionary (and XMP packet, when the document has one),
//! then a cross-reference section of the same kind as the newest one in the
//! file, with `/Prev` pointing at it. Nothing in the page content is touched,
//! and the previous revision remains recoverable by truncating the file.
//!
//! exprim reads the structure (trailer, sections, objects); nothing in the
//! ecosystem can edit a PDF, so the small serializer and xref writer live here.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use exprim::document::{DocumentLimits, DocumentSnapshot, ParseContext};
use exprim::object::{NameTable, ObjectId, PdfObject};
use exprim::source::{OwnedBytesSource, PdfSource};
use exprim::structure::{ObjectLocation, RecoveryEvent, XrefSectionKind};
use lege_pdf_write::serialize::{write_hex_string, write_name, write_text_string};

/// The values to write. A `None` field keeps whatever the file has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataUpdate {
    pub title: Option<String>,
    pub author: Option<String>,
}

impl MetadataUpdate {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.author.is_none()
    }
}

/// What the file looks like before any change.
#[derive(Debug, Clone)]
pub struct Inspection {
    /// Credible existing values (placeholders such as "Untitled" read as none).
    pub existing: exprim::DocumentMetadata,
    pub page_count: u32,
    /// Why the file must not be updated, if it must not.
    pub blocker: Option<String>,
}

/// The appended file plus anything worth telling the user.
#[derive(Debug)]
pub struct Updated {
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
}

pub fn inspect(bytes: &Arc<[u8]>, allow_signed: bool) -> Result<Inspection, String> {
    let session =
        exprim::RenderSession::open(Arc::clone(bytes), None).map_err(|error| error.to_string())?;
    let existing = exprim::extract_metadata(&session);
    let page_count = session.page_count();
    let snapshot = open_snapshot(bytes)?;
    Ok(Inspection {
        existing,
        page_count,
        blocker: blocker(&snapshot, allow_signed),
    })
}

fn open_snapshot(bytes: &Arc<[u8]>) -> Result<DocumentSnapshot, String> {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(Arc::clone(bytes)));
    DocumentSnapshot::open(source, DocumentLimits::default()).map_err(|error| error.to_string())
}

fn blocker(snapshot: &DocumentSnapshot, allow_signed: bool) -> Option<String> {
    if snapshot
        .security()
        .is_some_and(|security| security.is_encrypted())
    {
        return Some("encrypted".to_string());
    }
    if !allow_signed && is_signed(snapshot) {
        return Some("digitally signed (use --allow-signed to append anyway)".to_string());
    }
    None
}

/// How the new revision's cross-reference section relates to the file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XrefPlan {
    /// The file's own chain is sound: list only the new objects and link
    /// `/Prev` to the newest section, in the same encoding.
    Extend { prev: u64, kind: XrefSectionKind },
    /// The chain is broken (exprim had to rebuild it by scanning): list every
    /// object at the location the scan found, so the file reads without
    /// recovery from now on. No `/Prev`.
    Replace,
}

fn xref_plan(snapshot: &DocumentSnapshot) -> XrefPlan {
    let structure = snapshot.structure();
    let rebuilt = structure
        .recovery
        .iter()
        .any(|event| matches!(event, RecoveryEvent::XrefRebuilt));
    // The newest section that is not a hybrid file's auxiliary `/XRefStm`.
    let newest = structure
        .sections
        .iter()
        .find(|section| section.auxiliary_of.is_none())
        .filter(|section| section.offset != 0);
    match newest {
        Some(section) if !rebuilt => XrefPlan::Extend {
            prev: section.offset,
            kind: section.kind,
        },
        _ => XrefPlan::Replace,
    }
}

/// Where one object lives, as an xref entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// Header-relative offset and generation.
    Offset(u64, u16),
    /// Object-stream number and index.
    Compressed(u32, u32),
}

/// `/AcroForm /SigFlags` bit 1: the document contains at least one signature.
fn is_signed(snapshot: &DocumentSnapshot) -> bool {
    let names = snapshot.names();
    let mut ctx = ParseContext::new();
    let Some(catalog) = resolve_dict_value(
        snapshot,
        &PdfObject::Reference(snapshot.structure().trailer.root),
        &mut ctx,
    ) else {
        return false;
    };
    let Some(acroform) = catalog
        .as_dict()
        .and_then(|catalog| catalog.get(names.intern(b"AcroForm")))
        .and_then(|value| resolve_dict_value(snapshot, value, &mut ctx))
    else {
        return false;
    };
    acroform
        .as_dict()
        .and_then(|form| form.get(names.intern(b"SigFlags")))
        .and_then(|value| resolve_dict_value(snapshot, value, &mut ctx))
        .and_then(|value| value.as_int())
        .is_some_and(|flags| flags & 1 != 0)
}

fn resolve_dict_value(
    snapshot: &DocumentSnapshot,
    value: &PdfObject,
    ctx: &mut ParseContext,
) -> Option<Arc<PdfObject>> {
    match value {
        PdfObject::Reference(id) => snapshot.objects().resolve(snapshot, *id, ctx).ok(),
        other => Some(Arc::new(other.clone())),
    }
}

/// Build the updated file. `now` stamps `/ModDate` and seeds the new file ID.
pub fn apply(
    bytes: &Arc<[u8]>,
    update: &MetadataUpdate,
    now: std::time::SystemTime,
) -> Result<Updated, String> {
    let snapshot = open_snapshot(bytes)?;
    if let Some(reason) = blocker(&snapshot, true) {
        return Err(reason);
    }
    let plan = xref_plan(&snapshot);
    let structure = snapshot.structure();
    let names = snapshot.names();
    let trailer = &structure.trailer;
    let mut ctx = ParseContext::new();
    let mut warnings = Vec::new();
    let mut next_number = structure.xref.size().max(1);
    // (id, body) for every object in the new revision.
    let mut objects: Vec<(ObjectId, Vec<u8>)> = Vec::new();

    // ── Info dictionary ──
    let info_id = match trailer.info {
        Some(id) => id,
        None => {
            let id = ObjectId::new(next_number, 0);
            next_number += 1;
            id
        }
    };
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    if let Some(id) = trailer.info
        && let Ok(info) = snapshot.objects().resolve(&snapshot, id, &mut ctx)
        && let Some(dict) = info.as_dict()
    {
        for (key, value) in dict.iter() {
            let key = names.resolve(key).to_vec();
            let replaced = (key == b"Title" && update.title.is_some())
                || (key == b"Author" && update.author.is_some())
                || key == b"ModDate";
            if !replaced {
                let mut body = Vec::new();
                write_value(&mut body, value, names, 0);
                entries.push((key, body));
            }
        }
    }
    if let Some(title) = &update.title {
        let mut body = Vec::new();
        write_text_string(&mut body, title);
        entries.push((b"Title".to_vec(), body));
    }
    if let Some(author) = &update.author {
        let mut body = Vec::new();
        write_text_string(&mut body, author);
        entries.push((b"Author".to_vec(), body));
    }
    entries.push((b"ModDate".to_vec(), pdf_date(now).into_bytes()));
    let mut info = b"<<".to_vec();
    for (key, value) in &entries {
        write_name(&mut info, key);
        info.push(b' ');
        info.extend_from_slice(value);
    }
    info.extend_from_slice(b">>");
    objects.push((info_id, info));

    // ── XMP packet ──
    if let Some((xmp_id, packet)) = xmp_packet(&snapshot, &mut ctx, &mut warnings) {
        match crate::xmp::rewrite(&packet, update.title.as_deref(), update.author.as_deref()) {
            Some(rewritten) => {
                let mut body = format!(
                    "<</Type/Metadata/Subtype/XML/Length {}>>\nstream\n",
                    rewritten.len()
                )
                .into_bytes();
                body.extend_from_slice(&rewritten);
                body.extend_from_slice(b"\nendstream");
                objects.push((xmp_id, body));
            }
            None => warnings.push(
                "XMP metadata could not be edited and still holds the old title/author".to_string(),
            ),
        }
    }

    // ── append ──
    let header = structure.header_offset;
    let mut out = bytes.to_vec();
    if !matches!(out.last(), Some(b'\n' | b'\r')) {
        out.push(b'\n');
    }
    let relative = |len: usize| (len as u64).saturating_sub(header);
    let mut rows: BTreeMap<u32, Row> = BTreeMap::new();
    if plan == XrefPlan::Replace {
        warnings
            .push("the cross-reference table was damaged; wrote a complete new one".to_string());
        for entry in structure.xref.live_entries() {
            let row = match entry.location {
                ObjectLocation::Offset(offset) => Row::Offset(offset, entry.id.generation),
                ObjectLocation::InObjectStream { container, index } => {
                    Row::Compressed(container, index)
                }
                ObjectLocation::Free => continue,
            };
            rows.insert(entry.id.number, row);
        }
    }
    for (id, body) in &objects {
        rows.insert(id.number, Row::Offset(relative(out.len()), id.generation));
        let _ = writeln!(out, "{} {} obj", id.number, id.generation);
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }

    let file_id = file_id(trailer.file_id.as_ref(), bytes, now);
    let mut trailer_keys = format!(
        "/Root {} {} R/Info {} {} R",
        trailer.root.number, trailer.root.generation, info_id.number, info_id.generation
    )
    .into_bytes();
    let kind = match plan {
        XrefPlan::Extend { prev, kind } => {
            let _ = write!(trailer_keys, "/Prev {prev}");
            kind
        }
        // A classic table cannot point into object streams.
        XrefPlan::Replace if rows.values().any(|row| matches!(row, Row::Compressed(..))) => {
            XrefSectionKind::Stream
        }
        XrefPlan::Replace => XrefSectionKind::Classic,
    };
    trailer_keys.extend_from_slice(b"/ID[");
    write_hex_string(&mut trailer_keys, &file_id[0]);
    write_hex_string(&mut trailer_keys, &file_id[1]);
    trailer_keys.push(b']');
    let first_free = next_number.max(rows.keys().max().map_or(0, |max| max + 1));
    let xref_offset = relative(out.len());

    match kind {
        XrefSectionKind::Classic => {
            out.extend_from_slice(b"xref\n");
            if plan == XrefPlan::Replace && !rows.contains_key(&0) {
                out.extend_from_slice(b"0 1\n0000000000 65535 f\r\n");
            }
            for run in consecutive_runs(&rows) {
                let _ = writeln!(out, "{} {}", run[0].0, run.len());
                for (_, row) in run {
                    if let Row::Offset(offset, generation) = row {
                        let _ = write!(out, "{offset:010} {generation:05} n\r\n");
                    }
                }
            }
            let _ = write!(out, "trailer\n<</Size {first_free}");
            out.extend_from_slice(&trailer_keys);
            let _ = write!(out, ">>\nstartxref\n{xref_offset}\n%%EOF\n");
        }
        XrefSectionKind::Stream => {
            let stream_number = first_free;
            let size = stream_number + 1;
            rows.insert(stream_number, Row::Offset(xref_offset, 0));
            let (middle, last) = field_widths(rows.values());
            let mut index = String::new();
            let mut data = Vec::new();
            for run in consecutive_runs(&rows) {
                let _ = write!(index, "{} {} ", run[0].0, run.len());
                for (_, row) in run {
                    let (kind, second, third) = match row {
                        Row::Offset(offset, generation) => (1u8, offset, u64::from(generation)),
                        Row::Compressed(container, index) => {
                            (2u8, u64::from(container), u64::from(index))
                        }
                    };
                    data.push(kind);
                    data.extend_from_slice(&second.to_be_bytes()[8 - middle..]);
                    data.extend_from_slice(&third.to_be_bytes()[8 - last..]);
                }
            }
            let _ = write!(
                out,
                "{stream_number} 0 obj\n<</Type/XRef/Size {size}/W[1 {middle} {last}]/Index[{}]",
                index.trim_end()
            );
            out.extend_from_slice(&trailer_keys);
            let _ = write!(out, "/Length {}>>\nstream\n", data.len());
            out.extend_from_slice(&data);
            let _ = write!(
                out,
                "\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n"
            );
        }
    }
    Ok(Updated {
        bytes: out,
        warnings,
    })
}

/// The catalog's `/Metadata` stream: its object id and decoded packet.
fn xmp_packet(
    snapshot: &DocumentSnapshot,
    ctx: &mut ParseContext,
    warnings: &mut Vec<String>,
) -> Option<(ObjectId, Vec<u8>)> {
    let names = snapshot.names();
    let catalog = snapshot
        .objects()
        .resolve(snapshot, snapshot.structure().trailer.root, ctx)
        .ok()?;
    let reference = catalog
        .as_dict()?
        .get(names.lookup(b"Metadata")?)?
        .as_reference()?;
    let object = snapshot.objects().resolve(snapshot, reference, ctx).ok()?;
    let PdfObject::Stream(stream) = object.as_ref() else {
        return None;
    };
    match snapshot.decode_stream_data(stream, ctx) {
        Ok(packet) => Some((reference, packet)),
        Err(error) => {
            warnings.push(format!(
                "XMP metadata could not be decoded ({error}); left as is"
            ));
            None
        }
    }
}

/// Serialize a parsed object back to PDF syntax. Strings go out as hex, which
/// round-trips any byte string exactly.
fn write_value(out: &mut Vec<u8>, value: &PdfObject, names: &NameTable, depth: usize) {
    if depth > 32 {
        out.extend_from_slice(b"null");
        return;
    }
    match value {
        PdfObject::Null | PdfObject::Stream(_) => out.extend_from_slice(b"null"),
        PdfObject::Boolean(value) => {
            out.extend_from_slice(if *value { b"true" } else { b"false" });
        }
        PdfObject::Integer(value) => {
            let _ = write!(out, "{value}");
        }
        PdfObject::Real(value) => out.extend_from_slice(format_real(*value).as_bytes()),
        PdfObject::Name(name) => write_name(out, &names.resolve(*name)),
        PdfObject::String(string) => write_hex_string(out, string.as_bytes()),
        PdfObject::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b' ');
                }
                write_value(out, item, names, depth + 1);
            }
            out.push(b']');
        }
        PdfObject::Dictionary(dict) => {
            out.extend_from_slice(b"<<");
            for (key, item) in dict.iter() {
                write_name(out, &names.resolve(key));
                out.push(b' ');
                write_value(out, item, names, depth + 1);
            }
            out.extend_from_slice(b">>");
        }
        PdfObject::Reference(id) => {
            let _ = write!(out, "{} {} R", id.number, id.generation);
        }
    }
}

fn format_real(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let text = format!("{value:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" {
        "0".to_string()
    } else {
        text.to_string()
    }
}

/// `/W` widths of an xref stream's second and third fields: the largest
/// offset or container (at least the customary four bytes) and the largest
/// generation or index (at least two).
fn field_widths<'a>(rows: impl Iterator<Item = &'a Row>) -> (usize, usize) {
    let bytes_for = |value: u64| (64 - value.leading_zeros() as usize).div_ceil(8);
    let (mut middle, mut last) = (4, 2);
    for row in rows {
        let (second, third) = match *row {
            Row::Offset(offset, generation) => (offset, u64::from(generation)),
            Row::Compressed(container, index) => (u64::from(container), u64::from(index)),
        };
        middle = middle.max(bytes_for(second));
        last = last.max(bytes_for(third));
    }
    (middle, last)
}

fn consecutive_runs(entries: &BTreeMap<u32, Row>) -> Vec<Vec<(u32, Row)>> {
    let mut runs: Vec<Vec<(u32, Row)>> = Vec::new();
    for (&number, &entry) in entries {
        match runs.last_mut() {
            Some(run) if run.last().is_some_and(|(last, _)| last + 1 == number) => {
                run.push((number, entry));
            }
            _ => runs.push(vec![(number, entry)]),
        }
    }
    runs
}

/// Keep the permanent half of the file ID and give this revision a new
/// changing half (ISO 32000-1 §14.4).
fn file_id(
    existing: Option<&[exprim::object::PdfString; 2]>,
    bytes: &[u8],
    now: std::time::SystemTime,
) -> [Vec<u8>; 2] {
    use std::hash::{Hash, Hasher};
    let mut fresh = Vec::with_capacity(16);
    for salt in 0u8..2 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        salt.hash(&mut hasher);
        bytes.len().hash(&mut hasher);
        bytes[bytes.len().saturating_sub(4096)..].hash(&mut hasher);
        now.hash(&mut hasher);
        fresh.extend_from_slice(&hasher.finish().to_be_bytes());
    }
    match existing {
        Some([permanent, _]) if !permanent.as_bytes().is_empty() => {
            [permanent.as_bytes().to_vec(), fresh]
        }
        _ => [fresh.clone(), fresh],
    }
}

/// `D:YYYYMMDDHHmmSSZ` in UTC.
fn pdf_date(now: std::time::SystemTime) -> String {
    let seconds = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "(D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z)",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Reopen the updated bytes and check that they say what we meant them to.
pub fn verify(updated: &[u8], expected_pages: u32, update: &MetadataUpdate) -> Result<(), String> {
    let session = exprim::RenderSession::open(Arc::from(updated), None)
        .map_err(|error| format!("updated file does not reopen: {error}"))?;
    if session.page_count() != expected_pages {
        return Err(format!(
            "updated file has {} pages, expected {expected_pages}",
            session.page_count()
        ));
    }
    let snapshot = open_snapshot(&Arc::from(updated))?;
    if snapshot
        .structure()
        .recovery
        .iter()
        .any(|event| matches!(event, RecoveryEvent::XrefRebuilt))
    {
        return Err("updated file still needs its cross-reference table rebuilt".to_string());
    }
    let metadata = exprim::extract_metadata(&session);
    let collapse = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(title) = &update.title
        && metadata.title.as_deref() != Some(collapse(title).as_str())
    {
        return Err(format!(
            "updated file reads back title {:?}, expected {title:?}",
            metadata.title
        ));
    }
    if let Some(author) = &update.author
        && metadata.author.as_deref() != Some(collapse(author).as_str())
    {
        return Err(format!(
            "updated file reads back author {:?}, expected {author:?}",
            metadata.author
        ));
    }
    Ok(())
}

/// Replace `path` with `bytes` through a temporary file in the same folder, so a
/// crash leaves either the old file or the new one, never half of either.
pub fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let folder = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(folder).map_err(|error| error.to_string())?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use exprim::test_support::builder::PdfBuilder;

    const NOW: u64 = 1_793_000_000;

    fn now() -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW)
    }

    fn body(builder: &mut PdfBuilder, info: Option<&str>, xmp: Option<&str>) {
        let metadata = if xmp.is_some() { "/Metadata 5 0 R" } else { "" };
        builder.add_object(1, &format!("<</Type/Catalog/Pages 2 0 R{metadata}>>"));
        builder.add_object(2, "<</Type/Pages/Kids[3 0 R]/Count 1>>");
        builder.add_object(3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]>>");
        if let Some(info) = info {
            builder.add_object(4, info);
        }
        if let Some(xmp) = xmp {
            builder.add_stream(5, "/Type/Metadata/Subtype/XML", xmp.as_bytes());
        }
    }

    fn classic(info: Option<&str>, xmp: Option<&str>) -> Arc<[u8]> {
        let mut builder = PdfBuilder::new();
        body(&mut builder, info, xmp);
        let trailer = if info.is_some() {
            "/Root 1 0 R/Info 4 0 R"
        } else {
            "/Root 1 0 R"
        };
        builder.finish_classic_xref(trailer);
        Arc::from(builder.into_bytes())
    }

    fn stream(info: Option<&str>) -> Arc<[u8]> {
        let mut builder = PdfBuilder::new();
        body(&mut builder, info, None);
        let trailer = if info.is_some() {
            "/Root 1 0 R/Info 4 0 R"
        } else {
            "/Root 1 0 R"
        };
        builder.finish_xref_stream(9, trailer);
        Arc::from(builder.into_bytes())
    }

    fn update(title: &str, author: &str) -> MetadataUpdate {
        MetadataUpdate {
            title: Some(title.to_string()),
            author: Some(author.to_string()),
        }
    }

    fn info_string(bytes: &[u8], key: &[u8]) -> Option<String> {
        let snapshot = open_snapshot(&Arc::from(bytes)).unwrap();
        let mut ctx = ParseContext::new();
        let id = snapshot.structure().trailer.info?;
        let info = snapshot.objects().resolve(&snapshot, id, &mut ctx).ok()?;
        let value = info.as_dict()?.get(snapshot.names().lookup(key)?)?.clone();
        match value {
            PdfObject::String(text) => {
                Some(exprim::object::decode_text_string_lossy(text.as_bytes()))
            }
            _ => None,
        }
    }

    fn check(original: &Arc<[u8]>, wanted: &MetadataUpdate) -> Vec<u8> {
        let updated = apply(original, wanted, now()).unwrap();
        assert!(updated.bytes.starts_with(original), "original bytes kept");
        verify(&updated.bytes, 1, wanted).unwrap();
        updated.bytes
    }

    #[test]
    fn classic_file_with_info_keeps_its_other_entries() {
        let original = classic(
            Some("<</Title(Untitled)/Producer(Scanner 2.0)/CreationDate(D:20200101000000Z)>>"),
            None,
        );
        let bytes = check(&original, &update("Winter Harvest", "Clara Mendes"));
        assert_eq!(
            info_string(&bytes, b"Producer").as_deref(),
            Some("Scanner 2.0")
        );
        assert_eq!(
            info_string(&bytes, b"CreationDate").as_deref(),
            Some("D:20200101000000Z")
        );
        assert!(info_string(&bytes, b"ModDate").is_some());
    }

    #[test]
    fn classic_file_without_info_gains_one() {
        let original = classic(None, None);
        check(&original, &update("Winter Harvest", "Clara Mendes"));
    }

    #[test]
    fn xref_stream_files_get_an_xref_stream_update() {
        for info in [None, Some("<</Producer(Scanner 2.0)>>")] {
            let original = stream(info);
            let bytes = check(&original, &update("Winter Harvest", "Clara Mendes"));
            let tail = &bytes[original.len()..];
            assert!(tail.windows(9).any(|w| w == b"/Type/XRe"), "xref stream");
            assert!(!tail.starts_with(b"xref"));
        }
    }

    #[test]
    fn only_requested_fields_change() {
        let original = classic(Some("<</Title(Keep Me)/Author(Old Name)>>"), None);
        let wanted = MetadataUpdate {
            title: None,
            author: Some("New Name".to_string()),
        };
        let bytes = check(&original, &wanted);
        assert_eq!(info_string(&bytes, b"Title").as_deref(), Some("Keep Me"));
        assert_eq!(info_string(&bytes, b"Author").as_deref(), Some("New Name"));
    }

    #[test]
    fn non_ascii_values_round_trip() {
        let original = classic(None, None);
        check(&original, &update("Über die Wärme", "Zoë Ångström"));
    }

    #[test]
    fn xmp_packet_is_rewritten_too() {
        let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Untitled</rdf:li></rdf:Alt></dc:title>\
</rdf:Description></rdf:RDF></x:xmpmeta>";
        let original = classic(Some("<</Producer(x)>>"), Some(xmp));
        let bytes = check(&original, &update("Winter Harvest", "Clara Mendes"));
        let snapshot = open_snapshot(&Arc::from(bytes.as_slice())).unwrap();
        let mut ctx = ParseContext::new();
        let mut warnings = Vec::new();
        let (_, packet) = xmp_packet(&snapshot, &mut ctx, &mut warnings).unwrap();
        let packet = String::from_utf8(packet).unwrap();
        assert!(packet.contains(">Winter Harvest</rdf:li>"), "{packet}");
        assert!(packet.contains("<rdf:li>Clara Mendes</rdf:li>"), "{packet}");
        assert!(!packet.contains("Untitled"));
    }

    #[test]
    fn a_second_update_chains_onto_the_first() {
        let original = classic(Some("<</Producer(x)>>"), None);
        let first = check(&original, &update("First", "Ann Lee"));
        let second = check(&Arc::from(first.as_slice()), &update("Second", "Bo Kim"));
        assert_eq!(info_string(&second, b"Title").as_deref(), Some("Second"));
    }

    /// Shift `startxref` so it no longer lands on the table, as a tool that
    /// rewrites an object in place without fixing offsets leaves a file.
    fn with_broken_startxref(bytes: &[u8]) -> Arc<[u8]> {
        let mut bytes = bytes.to_vec();
        let at = bytes
            .windows(10)
            .rposition(|window| window == b"startxref\n")
            .unwrap()
            + 10;
        let end = at + bytes[at..].iter().position(|byte| *byte == b'\n').unwrap();
        let offset: u64 = std::str::from_utf8(&bytes[at..end])
            .unwrap()
            .parse()
            .unwrap();
        let wrong = format!("{:0width$}", offset - 40, width = end - at);
        bytes.splice(at..end, wrong.bytes());
        Arc::from(bytes)
    }

    /// Also garble the classic table's keyword, so no table can be found and
    /// the reader must rebuild the whole map by scanning.
    fn with_unfindable_table(bytes: &[u8]) -> Arc<[u8]> {
        let mut bytes = with_broken_startxref(bytes).to_vec();
        let at = bytes
            .windows(6)
            .rposition(|window| window == b"\nxref\n")
            .unwrap();
        bytes[at + 1..at + 5].copy_from_slice(b"xxxx");
        Arc::from(bytes)
    }

    fn recovery(bytes: &Arc<[u8]>) -> Vec<RecoveryEvent> {
        open_snapshot(bytes).unwrap().structure().recovery.to_vec()
    }

    #[test]
    fn a_lying_startxref_alone_is_extended_from_the_real_table() {
        let original = with_broken_startxref(&classic(Some("<</Producer(x)>>"), None));
        assert!(
            recovery(&original)
                .iter()
                .any(|event| matches!(event, RecoveryEvent::StartXrefRecovered { .. }))
        );
        let wanted = update("Winter Harvest", "Clara Mendes");
        let updated = apply(&original, &wanted, now()).unwrap();
        assert!(updated.warnings.is_empty(), "{:?}", updated.warnings);
        assert!(
            updated.bytes[original.len()..]
                .windows(5)
                .any(|w| w == b"/Prev")
        );
        verify(&updated.bytes, 1, &wanted).unwrap();
    }

    #[test]
    fn a_damaged_classic_table_is_replaced_by_a_complete_one() {
        let original = with_unfindable_table(&classic(Some("<</Producer(x)>>"), None));
        assert!(
            recovery(&original)
                .iter()
                .any(|event| matches!(event, RecoveryEvent::XrefRebuilt)),
            "fixture must need a rebuild"
        );
        let updated = apply(&original, &update("Winter Harvest", "Clara Mendes"), now()).unwrap();
        assert!(
            updated
                .warnings
                .iter()
                .any(|warning| warning.contains("damaged"))
        );
        assert!(
            !updated.bytes[original.len()..]
                .windows(5)
                .any(|w| w == b"/Prev")
        );
        // `verify` also rejects a file that still needs a rebuild.
        verify(&updated.bytes, 1, &update("Winter Harvest", "Clara Mendes")).unwrap();
        assert_eq!(
            info_string(&updated.bytes, b"Producer").as_deref(),
            Some("x")
        );
    }

    #[test]
    fn a_damaged_file_with_object_streams_gets_an_xref_stream() {
        let mut builder = PdfBuilder::new();
        builder.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
        builder.add_object_stream(
            8,
            &[
                (2, "<</Type/Pages/Kids[3 0 R]/Count 1>>"),
                (3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]>>"),
            ],
        );
        builder.finish_xref_stream(9, "/Root 1 0 R");
        let original = with_broken_startxref(&builder.into_bytes());
        assert!(
            recovery(&original)
                .iter()
                .any(|event| matches!(event, RecoveryEvent::XrefRebuilt)),
            "fixture must need a rebuild"
        );
        let wanted = update("Winter Harvest", "Clara Mendes");
        let updated = apply(&original, &wanted, now()).unwrap();
        let tail = &updated.bytes[original.len()..];
        assert!(tail.windows(9).any(|w| w == b"/Type/XRe"));
        verify(&updated.bytes, 1, &wanted).unwrap();
    }

    #[test]
    fn signed_files_are_blocked_unless_allowed() {
        let mut builder = PdfBuilder::new();
        builder.add_object(
            1,
            "<</Type/Catalog/Pages 2 0 R/AcroForm<</Fields[]/SigFlags 3>>>>",
        );
        builder.add_object(2, "<</Type/Pages/Kids[3 0 R]/Count 1>>");
        builder.add_object(3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]>>");
        builder.finish_classic_xref("/Root 1 0 R");
        let bytes: Arc<[u8]> = Arc::from(builder.into_bytes());
        assert!(inspect(&bytes, false).unwrap().blocker.is_some());
        assert!(inspect(&bytes, true).unwrap().blocker.is_none());
    }

    #[test]
    fn dates_are_utc_calendar_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(pdf_date(now()), "(D:20261026073320Z)");
    }

    #[test]
    fn reals_are_written_compactly() {
        assert_eq!(format_real(1.5), "1.5");
        assert_eq!(format_real(2.0), "2");
        assert_eq!(format_real(-0.25), "-0.25");
    }
}
