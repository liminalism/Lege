//! Versioned dataset export.
//!
//! Writes a new directory `<out>/<name>/` (built as `<out>/.<name>.partial`
//! and renamed when complete, so a crash never leaves a half export under the
//! final name; an existing export is never overwritten):
//!
//! - `original/annotations.json` — COCO at original page resolution
//! - `s<S>/annotations.json` (+ `s<S>/images/...` with `--images`) — boxes
//!   and pages stretched to `S × S`
//! - `review.json` — human corrections only, original coordinates
//! - `delta.json` — corrections versus the imported layer, by class
//! - `manifest.json` — what was exported, from where, with file hashes
//!
//! Conventions preserved: `category_id = taxonomy_id + 1`, `iscrowd = 1` for
//! ignore regions, box-polygon `segmentation`, `area = w × h` recomputed in
//! the output coordinates. Source annotations and JPEGs are only read.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

use super::database::{self, PageFilter};
use super::manifest::{fnv1a_file, timestamp_compact, timestamp_iso};
use super::resizing::stretch_image;
use crate::annotation::commands::{Annotation, ReviewStatus};
use crate::annotation::geometry::{Box2, coco_polygon, coco_xywh, stretch_box};
use crate::annotation::labels::{CLASSES, class, is_taxonomy};
use crate::review::comparison::{Change, diff};

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub out_dir: PathBuf,
    pub name: Option<String>,
    pub filter: PageFilter,
    /// Square training sizes to emit, e.g. `[640]`.
    pub sizes: Vec<u32>,
    pub write_images: bool,
    pub reviewed_only: bool,
    pub jpeg_quality: u8,
    /// Refuse to export when a book appears in more than one split.
    pub strict_splits: bool,
    pub threads: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions {
            out_dir: PathBuf::from("exports"),
            name: None,
            filter: PageFilter::default(),
            sizes: vec![640],
            write_images: false,
            reviewed_only: false,
            jpeg_quality: 95,
            strict_splits: true,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportReport {
    pub dir: PathBuf,
    pub pages: usize,
    pub boxes: usize,
    pub excluded_pages: usize,
    pub skipped_unreviewed: usize,
    pub clamped_boxes: usize,
    pub dropped_boxes: usize,
    pub images_written: usize,
    pub image_errors: Vec<String>,
    pub split_leaks: Vec<String>,
    /// Pages left out because they still hold `abandon` boxes, which are not
    /// a training class: convert them first.
    pub unresolved_abandon_pages: usize,
    /// Pages of books marked "exclude from training", and those books.
    pub excluded_book_pages: usize,
    pub excluded_books: Vec<String>,
}

struct ExportPage {
    key: String,
    split: String,
    book_id: String,
    status: ReviewStatus,
    revision: i64,
    width: u32,
    height: u32,
    file_name: String,
    source_path: PathBuf,
    image_json: Map<String, Value>,
    annotations: Vec<Annotation>,
    imported: Vec<Annotation>,
}

fn load_pages(conn: &Connection, filter: &PageFilter) -> Result<Vec<String>, String> {
    database::page_keys(conn, filter)
}

fn load_page(conn: &Connection, key: &str) -> Result<ExportPage, String> {
    let (split, book_id, status, revision, width, height, file_name, source_path, image_json): (
        String,
        String,
        String,
        i64,
        i64,
        i64,
        String,
        String,
        Option<String>,
    ) = conn
        .prepare_cached(
            "SELECT split, book_id, review_status, revision, width, height, file_name, source_path, image_json \
             FROM pages WHERE page_key = ?1",
        )
        .map_err(|e| e.to_string())?
        .query_row(params![key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?))
        })
        .map_err(|e| format!("{key}: {e}"))?;
    let image_json = image_json
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    Ok(ExportPage {
        key: key.to_owned(),
        split,
        book_id,
        status: ReviewStatus::parse(&status).unwrap_or(ReviewStatus::Unseen),
        revision,
        width: width as u32,
        height: height as u32,
        file_name,
        source_path: PathBuf::from(source_path),
        image_json,
        annotations: database::annotations(conn, key)?,
        imported: database::imported_annotations(conn, key)?,
    })
}

fn categories_json() -> Value {
    Value::Array(
        CLASSES
            .iter()
            .map(|c| {
                json!({
                    "id": c.id as i64 + 1,
                    "taxonomy_id": c.id,
                    "name": c.name,
                    "supercategory": c.supercategory,
                    "pp23_export_id": c.pp23_export_id,
                })
            })
            .collect(),
    )
}

/// Streams one COCO file: header, then images and annotations one by one.
struct CocoWriter {
    out: BufWriter<std::fs::File>,
    images: Vec<String>,
    annotations: BufWriter<std::fs::File>,
    ann_path: PathBuf,
    first_ann: bool,
    next_ann_id: i64,
}

impl CocoWriter {
    fn create(path: &Path) -> Result<CocoWriter, String> {
        let out = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let ann_path = path.with_extension("annotations.part");
        let annotations =
            std::fs::File::create(&ann_path).map_err(|e| format!("{}: {e}", ann_path.display()))?;
        Ok(CocoWriter {
            out: BufWriter::new(out),
            images: Vec::new(),
            annotations: BufWriter::new(annotations),
            ann_path,
            first_ann: true,
            next_ann_id: 1,
        })
    }

    fn image(&mut self, v: Value) {
        self.images.push(v.to_string());
    }

    fn annotation(&mut self, mut v: Map<String, Value>) -> Result<(), String> {
        v.insert("id".into(), json!(self.next_ann_id));
        self.next_ann_id += 1;
        if !self.first_ann {
            self.annotations
                .write_all(b",\n")
                .map_err(|e| e.to_string())?;
        }
        self.first_ann = false;
        serde_json::to_writer(&mut self.annotations, &Value::Object(v)).map_err(|e| e.to_string())
    }

    fn finish(mut self, info: &Value, licenses: &Value) -> Result<(), String> {
        let io = |e: std::io::Error| e.to_string();
        self.annotations.flush().map_err(io)?;
        drop(self.annotations);
        writeln!(
            self.out,
            "{{\"info\":{},\"licenses\":{},\"categories\":{},\"images\":[",
            info,
            licenses,
            categories_json()
        )
        .map_err(io)?;
        for (i, img) in self.images.iter().enumerate() {
            if i > 0 {
                self.out.write_all(b",\n").map_err(io)?;
            }
            self.out.write_all(img.as_bytes()).map_err(io)?;
        }
        self.out.write_all(b"\n],\"annotations\":[\n").map_err(io)?;
        let mut part = std::fs::File::open(&self.ann_path).map_err(io)?;
        std::io::copy(&mut part, &mut self.out).map_err(io)?;
        drop(part);
        self.out.write_all(b"\n]}\n").map_err(io)?;
        self.out.flush().map_err(io)?;
        std::fs::remove_file(&self.ann_path).map_err(io)?;
        Ok(())
    }
}

/// The annotation JSON for one box at a given output scale, or `None` when
/// clamping leaves nothing.
fn annotation_json(
    a: &Annotation,
    change: Change,
    image_id: i64,
    src_w: u32,
    src_h: u32,
    target: Option<u32>,
    clamped: &mut usize,
) -> Option<Map<String, Value>> {
    let clampedb = a.bbox.clamp_to(src_w as f64, src_h as f64);
    if !clampedb.is_valid() {
        return None;
    }
    if !clampedb.approx_eq(&a.bbox, 1e-9) {
        *clamped += 1;
    }
    let b: Box2 = match target {
        Some(s) => stretch_box(clampedb, src_w, src_h, s, s),
        None => clampedb,
    };
    let mut m: Map<String, Value> = a
        .provenance_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    let source_id = m.remove("coco_id");
    let source = m.remove("_source");
    m.insert("image_id".into(), json!(image_id));
    m.insert("category_id".into(), json!(a.class_id as i64 + 1));
    m.insert("iscrowd".into(), json!(a.is_ignore as i64));
    // Unedited geometry at original resolution: the source's own numbers.
    let verbatim = match (&source, target) {
        (Some(src), None) => src
            .get("bbox")
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
            .filter(|v| {
                v.len() == 4 && Box2::from_xywh(v[0], v[1], v[2], v[3]).approx_eq(&b, 1e-9)
            }),
        _ => None,
    };
    if verbatim.is_some() {
        let src = source.as_ref().and_then(Value::as_object);
        let get = |k: &str| src.and_then(|o| o.get(k)).cloned();
        m.insert("bbox".into(), get("bbox").unwrap_or(Value::Null));
        let xywh = coco_xywh(b);
        m.insert(
            "area".into(),
            get("area").unwrap_or_else(|| json!(round6(xywh[2] * xywh[3]))),
        );
        m.insert(
            "segmentation".into(),
            get("segmentation").unwrap_or_else(|| json!([coco_polygon(b)])),
        );
    } else {
        // Computed geometry, rounded to a micro-pixel: f64 xyxy storage turns
        // 336.5 + 83.2 - 336.5 into 83.19999999999999.
        let xywh = coco_xywh(b).map(round6);
        let poly: Vec<f64> = coco_polygon(b).into_iter().map(round6).collect();
        m.insert("bbox".into(), json!(xywh));
        m.insert("area".into(), json!(round6(xywh[2] * xywh[3])));
        m.insert("segmentation".into(), json!([poly]));
    }
    m.insert(
        "annotator".into(),
        json!({ "id": a.id, "change": change.as_str(), "source_id": source_id.unwrap_or(Value::Null) }),
    );
    Some(m)
}

fn round6(v: f64) -> f64 {
    let r = (v * 1e6).round() / 1e6;
    if r == 0.0 { 0.0 } else { r }
}

fn export_info(conn: &Connection, opts: &ExportOptions) -> Result<(Value, Value), String> {
    let mut st = conn
        .prepare("SELECT set_name, source_file, info_json, licenses_json FROM imports ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, Option<String>, Option<String>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let sources: Vec<Value> = rows
        .iter()
        .map(|(set, file, info, _)| {
            json!({
                "set": set,
                "file": file,
                "info": info.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
            })
        })
        .collect();
    // A single source keeps its own info block (round-trip); several are merged.
    let base = if rows.len() == 1 {
        rows[0]
            .2
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
    } else {
        None
    };
    let mut info = match base {
        Some(Value::Object(m)) => m,
        _ => {
            let mut m = Map::new();
            m.insert(
                "description".into(),
                json!("Lege book layout dataset, exported by Liminal Annotator"),
            );
            m
        }
    };
    info.insert(
        "liminal_annotator".into(),
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "exported_at": timestamp_iso(),
            "filter": opts.filter.describe(),
            "reviewed_only": opts.reviewed_only,
            "sources": sources,
        }),
    );
    let licenses = rows
        .first()
        .and_then(|r| r.3.as_deref())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(json!([]));
    Ok((Value::Object(info), licenses))
}

pub fn export(conn: &Connection, opts: &ExportOptions) -> Result<ExportReport, String> {
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| format!("export-{}", timestamp_compact()));
    let final_dir = opts.out_dir.join(&name);
    if final_dir.exists() {
        return Err(format!(
            "{} already exists; exports are never overwritten",
            final_dir.display()
        ));
    }
    let work = opts.out_dir.join(format!(".{name}.partial"));
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    }
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let result = export_into(conn, opts, &work);
    match result {
        Ok(mut report) => {
            std::fs::rename(&work, &final_dir)
                .map_err(|e| format!("finalize {}: {e}", final_dir.display()))?;
            report.dir = final_dir;
            Ok(report)
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&work);
            Err(err)
        }
    }
}

fn export_into(
    conn: &Connection,
    opts: &ExportOptions,
    work: &Path,
) -> Result<ExportReport, String> {
    let mut report = ExportReport::default();
    let keys = load_pages(conn, &opts.filter)?;

    // Split check first: a book in two splits contaminates evaluation.
    {
        let mut splits: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut st = conn
            .prepare_cached("SELECT book_id, split FROM pages WHERE page_key = ?1")
            .map_err(|e| e.to_string())?;
        for k in &keys {
            let (b, s): (String, String) = st
                .query_row(params![k], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?;
            splits.entry(b).or_default().insert(s);
        }
        for (book, s) in splits {
            if s.len() > 1 {
                report.split_leaks.push(format!(
                    "{book}: {}",
                    s.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        if opts.strict_splits && !report.split_leaks.is_empty() {
            return Err(format!(
                "books occur in more than one split (pass --allow-split-leak to export anyway): {}",
                report.split_leaks.join("; ")
            ));
        }
    }

    let (info, licenses) = export_info(conn, opts)?;
    std::fs::create_dir_all(work.join("original")).map_err(|e| e.to_string())?;
    let mut original = CocoWriter::create(&work.join("original").join("annotations.json"))?;
    let mut sized: Vec<(u32, CocoWriter)> = Vec::new();
    for &s in &opts.sizes {
        let dir = work.join(format!("s{s}"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        sized.push((s, CocoWriter::create(&dir.join("annotations.json"))?));
    }

    let mut review_pages: Vec<Value> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    // class -> [added, deleted, modified_geometry, reclassified_from, reclassified_to, ignore_toggled]
    let mut delta: BTreeMap<&'static str, [usize; 6]> = BTreeMap::new();
    let mut reclass: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut image_jobs: Vec<(PathBuf, String)> = Vec::new();
    let mut image_id: i64 = 0;
    let mut split_counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    let excluded_books = database::excluded_books(conn)?;
    report.excluded_books = excluded_books.iter().cloned().collect();
    report.excluded_books.sort();
    for key in &keys {
        let page = load_page(conn, key)?;
        let d = diff(&page.annotations, &page.imported);
        if page.status == ReviewStatus::Excluded {
            excluded.push(page.key.clone());
            report.excluded_pages += 1;
            continue;
        }
        if opts.reviewed_only && page.status != ReviewStatus::Reviewed {
            report.skipped_unreviewed += 1;
            continue;
        }
        if excluded_books.contains(&page.book_id) {
            report.excluded_book_pages += 1;
            continue;
        }
        if page.annotations.iter().any(|a| !is_taxonomy(a.class_id)) {
            report.unresolved_abandon_pages += 1;
            continue;
        }
        image_id += 1;
        report.pages += 1;

        // Corrections bookkeeping.
        for (a, c) in page.annotations.iter().zip(&d.changes) {
            match *c {
                Change::Added => delta.entry(class(a.class_id).name).or_default()[0] += 1,
                Change::Modified {
                    geometry,
                    class: cls,
                    ignore,
                } => {
                    if geometry {
                        delta.entry(class(a.class_id).name).or_default()[2] += 1;
                    }
                    if ignore {
                        delta.entry(class(a.class_id).name).or_default()[5] += 1;
                    }
                    if cls {
                        if let Some(orig) = page.imported.iter().find(|o| o.id == a.id) {
                            delta.entry(class(orig.class_id).name).or_default()[3] += 1;
                            delta.entry(class(a.class_id).name).or_default()[4] += 1;
                            *reclass
                                .entry((
                                    class(orig.class_id).name.into(),
                                    class(a.class_id).name.into(),
                                ))
                                .or_default() += 1;
                        }
                    }
                }
                Change::Unchanged => {}
            }
        }
        for a in &d.deleted {
            delta.entry(class(a.class_id).name).or_default()[1] += 1;
        }
        if !d.is_untouched() || page.status != ReviewStatus::Unseen {
            let (u, m, a, del) = d.count();
            review_pages.push(json!({
                "page_key": page.key,
                "status": page.status.as_str(),
                "revision": page.revision,
                "width": page.width,
                "height": page.height,
                "counts": {"unchanged": u, "modified": m, "added": a, "deleted": del},
                "annotations": page.annotations.iter().zip(&d.changes)
                    .filter(|(_, c)| **c != Change::Unchanged)
                    .map(|(a, c)| json!({
                        "id": a.id, "change": c.as_str(), "class": class(a.class_id).name,
                        "category_id": a.class_id as i64 + 1, "bbox_xyxy": [a.bbox.x0, a.bbox.y0, a.bbox.x1, a.bbox.y1],
                        "iscrowd": a.is_ignore as i64,
                    }))
                    .collect::<Vec<_>>(),
                "deleted": d.deleted.iter().map(|a| json!({
                    "id": a.id, "class": class(a.class_id).name,
                    "bbox_xyxy": [a.bbox.x0, a.bbox.y0, a.bbox.x1, a.bbox.y1], "iscrowd": a.is_ignore as i64,
                })).collect::<Vec<_>>(),
            }));
        }

        // Images.
        let mut img = page.image_json.clone();
        img.insert("id".into(), json!(image_id));
        img.insert("file_name".into(), json!(page.file_name));
        img.insert("width".into(), json!(page.width));
        img.insert("height".into(), json!(page.height));
        img.insert("page_key".into(), json!(page.key));
        img.insert("review_status".into(), json!(page.status.as_str()));
        original.image(Value::Object(img.clone()));
        for (s, w) in sized.iter_mut() {
            let mut si = img.clone();
            si.insert("width".into(), json!(*s));
            si.insert("height".into(), json!(*s));
            si.insert("orig_width".into(), json!(page.width));
            si.insert("orig_height".into(), json!(page.height));
            w.image(Value::Object(si));
        }
        let entry = split_counts.entry(page.split.clone()).or_default();
        entry.0 += 1;

        for (a, c) in page.annotations.iter().zip(&d.changes) {
            let mut clamped = 0;
            match annotation_json(a, *c, image_id, page.width, page.height, None, &mut clamped) {
                Some(m) => {
                    original.annotation(m)?;
                    report.boxes += 1;
                    entry.1 += 1;
                    report.clamped_boxes += clamped;
                    for (s, w) in sized.iter_mut() {
                        let mut ignored = 0;
                        if let Some(m) = annotation_json(
                            a,
                            *c,
                            image_id,
                            page.width,
                            page.height,
                            Some(*s),
                            &mut ignored,
                        ) {
                            w.annotation(m)?;
                        }
                    }
                }
                None => report.dropped_boxes += 1,
            }
        }
        if opts.write_images {
            image_jobs.push((page.source_path.clone(), page.file_name.clone()));
        }
        let _ = (&page.book_id, &page.key);
    }

    original.finish(&info, &licenses)?;
    for (_, w) in sized {
        w.finish(&info, &licenses)?;
    }

    // Resized images, in parallel. Sources are only read.
    if opts.write_images && !opts.sizes.is_empty() {
        let jobs = std::sync::Mutex::new(image_jobs.iter());
        let errors = std::sync::Mutex::new(Vec::new());
        let written = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..opts.threads.max(1) {
                scope.spawn(|| {
                    loop {
                        let next = jobs.lock().map(|mut it| it.next().cloned()).unwrap_or(None);
                        let Some((src, rel)) = next else { break };
                        for &s in &opts.sizes {
                            let dst = work.join(format!("s{s}")).join(&rel);
                            match stretch_image(&src, &dst, s, s, opts.jpeg_quality) {
                                Ok(_) => {
                                    written.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                Err(e) => {
                                    if let Ok(mut v) = errors.lock() {
                                        v.push(e);
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });
        report.images_written = written.into_inner();
        report.image_errors = errors.into_inner().unwrap_or_default();
        if !report.image_errors.is_empty() {
            return Err(format!(
                "{} images failed to resize, first: {}",
                report.image_errors.len(),
                report.image_errors[0]
            ));
        }
    }

    // review.json and delta.json
    let write_json = |path: PathBuf, v: &Value| -> Result<(), String> {
        let f = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::to_writer_pretty(BufWriter::new(f), v).map_err(|e| e.to_string())
    };
    write_json(
        work.join("review.json"),
        &json!({ "coordinates": "original page-image pixels, xyxy, half-open", "excluded": excluded, "pages": review_pages }),
    )?;
    write_json(
        work.join("delta.json"),
        &json!({
            "by_class": delta.iter().map(|(k, v)| (k.to_string(), json!({
                "added": v[0], "deleted": v[1], "geometry_modified": v[2],
                "reclassified_away": v[3], "reclassified_to": v[4], "ignore_toggled": v[5],
            }))).collect::<Map<String, Value>>(),
            "reclassifications": reclass.iter().map(|((a, b), n)| json!({"from": a, "to": b, "count": n})).collect::<Vec<_>>(),
        }),
    )?;

    // Manifest last, with hashes of everything above.
    let mut files = Vec::new();
    for rel in ["original/annotations.json", "review.json", "delta.json"] {
        let p = work.join(rel);
        files.push(json!({ "path": rel, "bytes": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), "fnv1a64": fnv1a_file(&p).unwrap_or_default() }));
    }
    for &s in &opts.sizes {
        let rel = format!("s{s}/annotations.json");
        let p = work.join(&rel);
        files.push(json!({ "path": rel, "bytes": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), "fnv1a64": fnv1a_file(&p).unwrap_or_default() }));
    }
    let project: String = conn
        .query_row("PRAGMA database_list", [], |r| r.get::<_, String>(2))
        .unwrap_or_default();
    write_json(
        work.join("manifest.json"),
        &json!({
            "tool": "liminal-annotator",
            "version": env!("CARGO_PKG_VERSION"),
            "exported_at": timestamp_iso(),
            "project": project,
            "filter": opts.filter.describe(),
            "reviewed_only": opts.reviewed_only,
            "sizes": opts.sizes,
            "resize": "stretch to SxS (no letterbox), triangle filter (PIL BILINEAR equivalent), JPEG q".to_owned() + &opts.jpeg_quality.to_string(),
            "images_written": report.images_written,
            "pages": report.pages,
            "boxes": report.boxes,
            "excluded_pages": report.excluded_pages,
            "skipped_unreviewed": report.skipped_unreviewed,
            "clamped_boxes": report.clamped_boxes,
            "dropped_boxes": report.dropped_boxes,
            "splits": split_counts.iter().map(|(k, (p, b))| (k.clone(), json!({"pages": p, "boxes": b}))).collect::<Map<String, Value>>(),
            "split_leaks": report.split_leaks,
            "unresolved_abandon_pages": report.unresolved_abandon_pages,
            "excluded_book_pages": report.excluded_book_pages,
            "excluded_books": report.excluded_books,
            "conventions": {
                "category_id": "taxonomy_id + 1",
                "iscrowd": "1 = ignore region",
                "bbox": "xywh in the file's image coordinates",
            },
            "files": files,
        }),
    )?;
    Ok(report)
}
