//! Projects made from a folder: point the annotator at a dataset folder and
//! it creates `annotator.db` inside it, imports what it finds, and imports
//! only what is new when the folder grows.
//!
//! Three layouts are recognised, in this order:
//!
//! 1. **Teacher pool** (`layout-detection` dataset v2): `teachers_raw.jsonl`
//!    beside `images/<book>/pNNNN.jpg`. Each row holds one page's
//!    PP-DocLayout-L (`paddle`) and DocLayout-YOLO (`yolo`) detections in
//!    rendered-image pixels. One teacher pre-fills the editable layer, the
//!    other becomes a prediction layer for the comparison overlay (`C`).
//!    YOLO's `abandon` boxes keep their own holding class
//!    ([`ABANDON`]) until a reviewer converts them. `books.jsonl` supplies
//!    each book's split and `image_pages.jsonl` its review tier, when present.
//! 2. **COCO data directory** (dataset v1): `annotations/*.json`, image paths
//!    relative to the folder. Each file becomes a set.
//! 3. **Image folders**: every folder of `.jpg`/`.png` pages below the root is
//!    a book, imported without boxes.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

use super::database::{self, Region, meta_get, meta_set};
use super::import_coco::{ImportOptions, import_coco};
use crate::annotation::geometry::Box2;
use crate::annotation::labels::{ABANDON, NUM_CLASSES};

/// The project file a folder's project lives in.
pub const PROJECT_FILE: &str = "annotator.db";

/// Model names of the two teachers' prediction layers.
pub const YOLO_MODEL: &str = "doclayout-yolo";
pub const PADDLE_MODEL: &str = "pp-doclayout-l";

/// Pages of one book are ordered by `seq = book ordinal × BOOK_STRIDE + page index`.
const BOOK_STRIDE: i64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefill {
    /// DocLayout-YOLO fills the editable layer (its `abandon` boxes included).
    Yolo,
    /// PP-DocLayout-L fills the editable layer.
    Paddle,
    /// Nothing is pre-filled; both teachers are prediction layers.
    Empty,
}

impl Prefill {
    pub fn parse(s: &str) -> Option<Prefill> {
        Some(match s {
            "yolo" | "doclayout-yolo" => Prefill::Yolo,
            "paddle" | "pp" | "pp-doclayout-l" => Prefill::Paddle,
            "none" | "empty" => Prefill::Empty,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Prefill::Yolo => "yolo",
            Prefill::Paddle => "paddle",
            Prefill::Empty => "none",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FolderOptions {
    pub prefill: Prefill,
    /// Lowest teacher score pre-filled into the editable layer.
    pub prefill_min_score: f64,
    /// Lowest score kept in a teacher's prediction layer.
    pub layer_min_score: f64,
    /// A YOLO `abandon` box takes the label of the PP-DocLayout-L box that
    /// overlaps it best, at this PP score or more...
    pub resolve_min_score: f64,
    /// ...and this IoU or more. Unmatched boxes stay `abandon`.
    pub resolve_iou: f64,
}

impl Default for FolderOptions {
    fn default() -> Self {
        FolderOptions {
            prefill: Prefill::Yolo,
            prefill_min_score: 0.30,
            layer_min_score: 0.30,
            // 97.5% of the pool's abandon boxes resolve; 0.15 is the PP
            // threshold of the v1 synthesis.
            resolve_min_score: 0.15,
            resolve_iou: 0.3,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FolderKind {
    Teachers { jsonl: PathBuf },
    Coco { files: Vec<PathBuf> },
    Images,
}

impl FolderKind {
    pub fn describe(&self) -> &'static str {
        match self {
            FolderKind::Teachers { .. } => "teacher pool (teachers_raw.jsonl + images/)",
            FolderKind::Coco { .. } => "COCO data directory (annotations/*.json)",
            FolderKind::Images => "image folders (no detections)",
        }
    }
}

/// What a folder holds.
pub fn detect(root: &Path) -> Result<FolderKind, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let jsonl = root.join("teachers_raw.jsonl");
    if jsonl.is_file() {
        return Ok(FolderKind::Teachers { jsonl });
    }
    let ann = root.join("annotations");
    if ann.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&ann)
            .map_err(|e| format!("{}: {e}", ann.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("json"))
                    && p.file_stem().is_some_and(|s| s != "categories")
            })
            .collect();
        files.sort();
        if !files.is_empty() {
            return Ok(FolderKind::Coco { files });
        }
    }
    Ok(FolderKind::Images)
}

/// The project for `path`: a `.db` file itself, or a folder's `annotator.db`.
pub fn project_for(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(PROJECT_FILE)
    } else {
        path.to_path_buf()
    }
}

/// Delete a project that was just created and whose first import failed,
/// so a folder that holds no dataset is left as it was.
pub fn remove_project(project: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = project.as_os_str().to_owned();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
}

/// The dataset folder a project was made from, if it was made from one.
pub fn project_root(conn: &Connection) -> Option<PathBuf> {
    meta_get(conn, "folder_root")
        .ok()
        .flatten()
        .map(PathBuf::from)
}

fn stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("{}:{t}", m.len())
        }
        Err(_) => String::new(),
    }
}

/// Whether the folder changed since the project last read it (a cheap
/// check: the size and time of the detection file).
pub fn needs_sync(conn: &Connection) -> bool {
    let Some(root) = project_root(conn) else {
        return false;
    };
    match detect(&root) {
        Ok(FolderKind::Teachers { jsonl }) => {
            meta_get(conn, "folder_stamp").ok().flatten() != Some(stamp(&jsonl))
                || (meta_get(conn, "folder_prefill").ok().flatten().as_deref() == Some("yolo")
                    && meta_get(conn, "abandon_resolved").ok().flatten().is_none())
        }
        _ => false,
    }
}

#[derive(Clone, Debug, Default)]
pub struct FolderReport {
    pub kind: String,
    pub pages_added: usize,
    pub pages_present: usize,
    pub books_added: usize,
    pub boxes: usize,
    pub abandon_boxes: usize,
    /// YOLO `abandon` boxes that took PP-DocLayout-L's label (at import, or
    /// in the one-time pass over an older project).
    pub abandon_resolved: usize,
    pub predictions: usize,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for FolderReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} pages added in {} new books ({} already present), {} boxes, {} prediction boxes; \
             {} YOLO abandon boxes took PP-DocLayout-L's label",
            self.kind,
            self.pages_added,
            self.books_added,
            self.pages_present,
            self.boxes,
            self.predictions,
            self.abandon_resolved
        )?;
        for w in self.warnings.iter().take(5) {
            write!(f, "\n  warning: {w}")?;
        }
        Ok(())
    }
}

/// Progress of a running import: a stage name and `done` of `total` units.
pub type ProgressFn<'a> = dyn FnMut(&str, u64, u64) + 'a;

/// Create or update the project at `root/annotator.db` (or `project` when
/// given) from the folder. Pages already in the project are left alone, so
/// this is also the rescan.
pub fn sync_folder(
    conn: &mut Connection,
    root: &Path,
    opts: &FolderOptions,
    progress: &mut ProgressFn<'_>,
) -> Result<FolderReport, String> {
    let root = std::path::absolute(root).map_err(|e| format!("{}: {e}", root.display()))?;
    // A prefill choice is fixed when the project is created, so later
    // rescans fill new books the same way.
    let opts = match meta_get(conn, "folder_prefill")?.and_then(|p| Prefill::parse(&p)) {
        Some(p) => FolderOptions {
            prefill: p,
            ..opts.clone()
        },
        None => opts.clone(),
    };
    let kind = detect(&root)?;
    let report = match &kind {
        FolderKind::Teachers { jsonl } => {
            let stamp_now = stamp(jsonl);
            let had_pages: bool = conn
                .query_row("SELECT EXISTS (SELECT 1 FROM pages)", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            let mut r = import_teachers(conn, &root, jsonl, &opts, progress)?;
            // A project imported now resolved its abandon boxes on the way in;
            // only one imported before needs the pass.
            if opts.prefill == Prefill::Yolo
                && had_pages
                && meta_get(conn, "abandon_resolved")?.is_none()
            {
                r.abandon_resolved += resolve_existing_abandon(conn, jsonl, &opts, progress)?;
            }
            if opts.prefill == Prefill::Yolo {
                meta_set(conn, "abandon_resolved", "pp-doclayout-l")?;
            }
            if r.pages_added > 0 || r.abandon_resolved > 0 {
                progress("counting pictures", 0, 1);
                crate::dataset::database::recount_model_figures(conn)?;
            }
            meta_set(conn, "folder_stamp", &stamp_now)?;
            r
        }
        FolderKind::Coco { files } => import_coco_dir(conn, &root, files, progress)?,
        FolderKind::Images => import_image_folders(conn, &root, progress)?,
    };
    meta_set(conn, "folder_root", &root.to_string_lossy())?;
    meta_set(conn, "folder_prefill", opts.prefill.as_str())?;
    Ok(report)
}

// --- teacher pool --------------------------------------------------------------

/// `[class, score, x0, y0, x1, y1]`
type Det = (String, f64, f64, f64, f64, f64);

#[derive(Deserialize)]
struct TeacherRow {
    image_id: String,
    width: u32,
    height: u32,
    #[serde(default)]
    paddle: Vec<Det>,
    #[serde(default)]
    yolo: Vec<Det>,
}

/// A teacher's class names to book classes.
type ClassMap = fn(&str) -> Option<u8>;

/// A teacher's detections in a row, and its class map.
fn teacher<'a>(row: &'a TeacherRow, model: &str) -> (&'a [Det], ClassMap) {
    if model == YOLO_MODEL {
        (&row.yolo, yolo_class)
    } else {
        (&row.paddle, paddle_class)
    }
}

/// `prep/common.py` `YOLO_TO_BOOK`, with `abandon` kept as the holding class.
fn yolo_class(name: &str) -> Option<u8> {
    Some(match name {
        "plain text" | "table_footnote" => 0,
        "title" => 1,
        "abandon" => ABANDON,
        "figure" => 6,
        "figure_caption" | "table_caption" => 7,
        "table" => 8,
        "isolate_formula" | "formula_caption" => 12,
        _ => return None,
    })
}

/// `prep/common.py` `PP_TO_BOOK`.
fn paddle_class(name: &str) -> Option<u8> {
    Some(match name {
        "text" | "abstract" | "algorithm" => 0,
        "paragraph_title" | "doc_title" => 1,
        "header" => 2,
        "footer" => 3,
        "number" => 4,
        "footnote" => 5,
        "image" | "seal" | "chart" => 6,
        "figure_title" | "table_title" | "chart_title" => 7,
        "table" => 8,
        "aside_text" => 9,
        "content" => 10,
        "reference" => 11,
        "formula" | "formula_number" => 12,
        "header_image" | "footer_image" => 13,
        _ => return None,
    })
}

struct Mapped {
    class_id: u8,
    score: f64,
    bbox: Box2,
    fine: String,
    /// For a YOLO `abandon` box: the PP-DocLayout-L label it took
    /// (fine class, score).
    resolved: Option<(String, f64)>,
}

/// The PP-DocLayout-L detection (already mapped) that best overlaps `b`.
fn best_overlap<'a>(b: &Box2, pp: &'a [Mapped], min_iou: f64) -> Option<&'a Mapped> {
    pp.iter()
        .map(|p| (b.iou(&p.bbox), p))
        .filter(|(v, _)| *v >= min_iou)
        .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.score.total_cmp(&b.1.score)))
        .map(|(_, p)| p)
}

/// Give each `abandon` box in `boxes` the label of the PP box covering the
/// same region. Returns how many were resolved.
fn resolve_abandon(boxes: &mut [Mapped], pp: &[Mapped], min_iou: f64) -> usize {
    let mut n = 0;
    for m in boxes.iter_mut().filter(|m| m.class_id == ABANDON) {
        if let Some(p) = best_overlap(&m.bbox, pp, min_iou) {
            m.class_id = p.class_id;
            m.resolved = Some((p.fine.clone(), p.score));
            n += 1;
        }
    }
    n
}

/// Map a teacher's detections into book classes above `min`, clamped to the
/// page, highest score first, with near-duplicates (IoU 0.8 or more, any
/// class) suppressed: PP-DocLayout-L scores one region under several classes.
fn map_dets(dets: &[Det], map: ClassMap, min: f64, w: f64, h: f64) -> Vec<Mapped> {
    let mut v: Vec<Mapped> = dets
        .iter()
        .filter(|d| d.1 >= min)
        .filter_map(|(name, score, x0, y0, x1, y1)| {
            let class_id = map(name)?;
            let bbox = Box2::new(x0.max(0.0), y0.max(0.0), x1.min(w), y1.min(h));
            (bbox.width() >= 1.0 && bbox.height() >= 1.0).then(|| Mapped {
                class_id,
                score: *score,
                bbox,
                fine: name.clone(),
                resolved: None,
            })
        })
        .collect();
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Mapped> = Vec::with_capacity(v.len());
    for m in v {
        if keep.iter().all(|k| k.bbox.iou(&m.bbox) < 0.8) {
            keep.push(m);
        }
    }
    keep
}

/// `book_X_p0003` → (`book_X`, `p0003`, 3).
fn split_image_id(id: &str) -> (String, String, Option<i64>) {
    match id.rsplit_once('_') {
        Some((book, stem)) if !book.is_empty() => {
            let idx = stem.strip_prefix('p').and_then(|n| n.parse::<i64>().ok());
            (book.to_owned(), stem.to_owned(), idx)
        }
        _ => (String::new(), id.to_owned(), None),
    }
}

/// `books.jsonl`: book id → split (the last row per book wins).
fn read_splits(root: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(f) = std::fs::File::open(root.join("books.jsonl")) else {
        return out;
    };
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let (Some(b), Some(s)) = (
            v.get("book_id").and_then(Value::as_str),
            v.get("split").and_then(Value::as_str),
        ) {
            out.insert(b.to_owned(), s.to_owned());
        }
    }
    out
}

/// `image_pages.jsonl`: image id → (queue reason, priority).
fn read_tiers(root: &Path) -> HashMap<String, (String, f64)> {
    let mut out = HashMap::new();
    let Ok(f) = std::fs::File::open(root.join("image_pages.jsonl")) else {
        return out;
    };
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = v.get("image_id").and_then(Value::as_str) else {
            continue;
        };
        let tier = v.get("tier").and_then(Value::as_str).unwrap_or("?");
        let n = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        let mut why = vec![format!("tier {tier}")];
        if n("abandon_large") > 0 {
            why.push(format!("{} large abandon", n("abandon_large")));
        }
        if v.get("full_page").and_then(Value::as_bool) == Some(true) {
            why.push("full-page picture".into());
        }
        if n("pictures") > 0 {
            why.push(format!("{} pictures", n("pictures")));
        }
        let priority = match tier {
            "C" => 2.0,
            "B" => 1.0,
            _ => 0.0,
        } + 0.25 * n("abandon_large").min(4) as f64;
        out.insert(id.to_owned(), (why.join(", "), priority));
    }
    out
}

fn import_teachers(
    conn: &mut Connection,
    root: &Path,
    jsonl: &Path,
    opts: &FolderOptions,
    progress: &mut ProgressFn<'_>,
) -> Result<FolderReport, String> {
    let mut report = FolderReport {
        kind: format!("teacher pool, prefilled from {}", opts.prefill.as_str()),
        ..Default::default()
    };
    progress("Reading the project", 0, 0);
    let existing: HashSet<String> = {
        let mut st = conn
            .prepare("SELECT page_key FROM pages")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    // Book ordinals: books already in the project keep theirs.
    let mut ordinals: HashMap<String, i64> = HashMap::new();
    {
        let mut st = conn
            .prepare("SELECT book_id, MIN(seq) FROM pages GROUP BY book_id")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (b, seq) = row.map_err(|e| e.to_string())?;
            ordinals.insert(b, seq.div_euclid(BOOK_STRIDE));
        }
    }
    let mut next_ordinal = ordinals.values().copied().max().map_or(1, |m| m + 1);
    progress("Reading books.jsonl and image_pages.jsonl", 0, 0);
    let splits = read_splits(root);
    let tiers = read_tiers(root);

    let file = std::fs::File::open(jsonl).map_err(|e| format!("{}: {e}", jsonl.display()))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut line = String::new();
    let mut read: u64 = 0;
    let (prefill_model, layer_model) = match opts.prefill {
        Prefill::Yolo => (Some(YOLO_MODEL), Some(PADDLE_MODEL)),
        Prefill::Paddle => (Some(PADDLE_MODEL), Some(YOLO_MODEL)),
        Prefill::Empty => (None, None),
    };
    let images_root = root.join("images");
    let mut seen: HashSet<String> = HashSet::new();
    let mut lineno = 0usize;
    'outer: loop {
        // One transaction per 2,000 pages: a stopped import keeps what it
        // committed and the rescan carries on from there.
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut ins_page = tx
                .prepare_cached(
                    "INSERT INTO pages (page_key, seq, book_id, split, set_name, source_path, file_name, \
                     width, height, page_index, queue_reason, priority, abandon, abandon_large, abandon_imported) \
                     VALUES (?1, ?2, ?3, ?4, 'pool', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?12)",
                )
                .map_err(|e| e.to_string())?;
            let mut ins_ann = tx
                .prepare_cached(
                    "INSERT INTO annotations (id, page_key, ord, class_id, x0, y0, x1, y1, provenance_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(|e| e.to_string())?;
            let mut ins_imp = tx
                .prepare_cached(
                    "INSERT INTO imported_annotations (id, page_key, ord, class_id, x0, y0, x1, y1, provenance_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(|e| e.to_string())?;
            let mut set_counts = tx
                .prepare_cached(&format!(
                    "UPDATE pages SET {}, fig_area = ?{} WHERE page_key = ?1",
                    (0..NUM_CLASSES)
                        .map(|c| format!("n{c} = ?{}", c + 2))
                        .collect::<Vec<_>>()
                        .join(", "),
                    NUM_CLASSES + 2
                ))
                .map_err(|e| e.to_string())?;
            let mut ins_pred = tx
                .prepare_cached(
                    "INSERT INTO predictions (model, page_key, class_id, score, x0, y0, x1, y1) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(|e| e.to_string())?;
            let mut pages_in_tx = 0;
            loop {
                line.clear();
                let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
                if n == 0 {
                    drop((ins_page, ins_ann, ins_imp, ins_pred, set_counts));
                    tx.commit().map_err(|e| e.to_string())?;
                    break 'outer;
                }
                read += n as u64;
                lineno += 1;
                if lineno % 4096 == 0 {
                    progress("Importing detections", read, total);
                }
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                let row: TeacherRow = match serde_json::from_str(text) {
                    Ok(r) => r,
                    Err(e) => {
                        if report.warnings.len() < 20 {
                            report
                                .warnings
                                .push(format!("teachers_raw.jsonl line {lineno}: {e}"));
                        }
                        continue;
                    }
                };
                if existing.contains(&row.image_id) || !seen.insert(row.image_id.clone()) {
                    report.pages_present += 1;
                    continue;
                }
                let (book, stem, index) = split_image_id(&row.image_id);
                let ordinal = *ordinals.entry(book.clone()).or_insert_with(|| {
                    report.books_added += 1;
                    next_ordinal += 1;
                    next_ordinal - 1
                });
                let seq = ordinal * BOOK_STRIDE + index.unwrap_or(0).clamp(0, BOOK_STRIDE - 1);
                let file_name = format!("images/{book}/{stem}.jpg");
                let source = images_root.join(&book).join(format!("{stem}.jpg"));
                let (reason, priority) = tiers
                    .get(&row.image_id)
                    .cloned()
                    .map_or((None, 0.0), |(r, p)| (Some(r), p));
                let (w, h) = (row.width as f64, row.height as f64);
                let mut prefill: Vec<Mapped> = match prefill_model {
                    Some(model) => {
                        let (dets, map) = teacher(&row, model);
                        map_dets(dets, map, opts.prefill_min_score, w, h)
                    }
                    None => Vec::new(),
                };
                if opts.prefill == Prefill::Yolo && prefill.iter().any(|m| m.class_id == ABANDON) {
                    let pp = map_dets(&row.paddle, paddle_class, opts.resolve_min_score, w, h);
                    report.abandon_resolved += resolve_abandon(&mut prefill, &pp, opts.resolve_iou);
                }
                let abandon = prefill.iter().filter(|m| m.class_id == ABANDON);
                let n_abandon = abandon.clone().count() as i64;
                let n_large = abandon
                    .filter(|m| Region::Large.contains(&m.bbox, w, h))
                    .count() as i64;
                ins_page
                    .execute(params![
                        row.image_id,
                        seq,
                        book,
                        splits.get(&book).map(String::as_str).unwrap_or("new"),
                        source.to_string_lossy(),
                        file_name,
                        row.width,
                        row.height,
                        index,
                        reason,
                        priority,
                        n_abandon,
                        n_large
                    ])
                    .map_err(|e| format!("{}: {e}", row.image_id))?;
                let mut counts: Vec<rusqlite::types::Value> =
                    vec![rusqlite::types::Value::Text(row.image_id.clone())];
                for c in 0..NUM_CLASSES {
                    let n = prefill.iter().filter(|m| m.class_id as usize == c).count();
                    counts.push(rusqlite::types::Value::Integer(n as i64));
                }
                // The largest figure box, as `database::FIG_AREA_SET` keeps it.
                let fig_area = prefill
                    .iter()
                    .filter(|m| m.class_id == crate::annotation::labels::FIGURE)
                    .map(|m| m.bbox.width() * m.bbox.height())
                    .fold(0.0, f64::max);
                counts.push(rusqlite::types::Value::Real(fig_area));
                set_counts
                    .execute(rusqlite::params_from_iter(counts))
                    .map_err(|e| e.to_string())?;
                if let Some(model) = prefill_model {
                    for (ord, m) in prefill.iter().enumerate() {
                        let id = format!("{}#{}", row.image_id, ord);
                        let prov = match &m.resolved {
                            Some((pp_class, pp_score)) => json!({
                                "source": model, "score": m.score, "teacher_class": m.fine,
                                "label_from": PADDLE_MODEL, "label_class": pp_class,
                                "label_score": pp_score,
                            }),
                            None => {
                                json!({"source": model, "score": m.score, "teacher_class": m.fine})
                            }
                        }
                        .to_string();
                        let b = &m.bbox;
                        let args = params![
                            id,
                            row.image_id,
                            ord as i64,
                            m.class_id as i64,
                            b.x0,
                            b.y0,
                            b.x1,
                            b.y1,
                            prov
                        ];
                        ins_ann.execute(args).map_err(|e| e.to_string())?;
                        ins_imp.execute(args).map_err(|e| e.to_string())?;
                        report.boxes += 1;
                        if m.class_id == ABANDON {
                            report.abandon_boxes += 1;
                        }
                    }
                }
                let layers: Vec<&str> = match (prefill_model, layer_model) {
                    (_, Some(l)) => vec![l],
                    (None, None) => vec![YOLO_MODEL, PADDLE_MODEL],
                    _ => vec![],
                };
                for model in layers {
                    let (dets, map) = teacher(&row, model);
                    for m in map_dets(dets, map, opts.layer_min_score, w, h) {
                        let b = &m.bbox;
                        ins_pred
                            .execute(params![
                                model,
                                row.image_id,
                                m.class_id as i64,
                                m.score,
                                b.x0,
                                b.y0,
                                b.x1,
                                b.y1
                            ])
                            .map_err(|e| e.to_string())?;
                        report.predictions += 1;
                    }
                }
                report.pages_added += 1;
                pages_in_tx += 1;
                if pages_in_tx >= 2000 {
                    break;
                }
            }
            drop((ins_page, ins_ann, ins_imp, ins_pred, set_counts));
        }
        tx.commit().map_err(|e| e.to_string())?;
    }
    progress("Importing detections", total, total);
    if report.pages_added > 0 {
        conn.execute(
            "INSERT INTO imports (set_name, source_file, images_root, imported_at, pages, boxes) \
             VALUES ('pool', ?1, ?2, ?3, ?4, ?5)",
            params![
                jsonl.to_string_lossy(),
                images_root.to_string_lossy(),
                database::now_unix(),
                report.pages_added as i64,
                report.boxes as i64
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(report)
}

/// Projects imported before abandon boxes took PP-DocLayout-L's label: every
/// box still `abandon` gets the label of the PP box covering the same region,
/// read again from `teachers_raw.jsonl` (the project keeps only PP boxes at
/// 0.30 or more). One logged bulk change, so Shift+A, U undoes it; boxes a
/// reviewer already converted are not `abandon` and are left alone.
fn resolve_existing_abandon(
    conn: &mut Connection,
    jsonl: &Path,
    opts: &FolderOptions,
    progress: &mut ProgressFn<'_>,
) -> Result<usize, String> {
    const STAGE: &str = "Labelling abandon boxes from PP-DocLayout-L";
    progress(STAGE, 0, 0);
    let pages: HashSet<String> = {
        let mut st = conn
            .prepare("SELECT DISTINCT page_key FROM annotations INDEXED BY annotations_abandon WHERE class_id = 14")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    if pages.is_empty() {
        return Ok(0);
    }
    let file = std::fs::File::open(jsonl).map_err(|e| format!("{}: {e}", jsonl.display()))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut changes: Vec<database::ClassChange> = Vec::new();
    {
        let mut boxes = conn
            .prepare(
                "SELECT id, x0, y0, x1, y1 FROM annotations INDEXED BY annotations_abandon \
                 WHERE page_key = ?1 AND class_id = 14",
            )
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        let (mut read, mut lineno) = (0u64, 0usize);
        loop {
            line.clear();
            let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            read += n as u64;
            lineno += 1;
            if lineno % 4096 == 0 {
                progress(STAGE, read, total);
            }
            let Ok(row) = serde_json::from_str::<TeacherRow>(line.trim()) else {
                continue;
            };
            if !pages.contains(&row.image_id) {
                continue;
            }
            let (w, h) = (row.width as f64, row.height as f64);
            let pp = map_dets(&row.paddle, paddle_class, opts.resolve_min_score, w, h);
            if pp.is_empty() {
                continue;
            }
            let rows = boxes
                .query_map(params![row.image_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        Box2::new(r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
                    ))
                })
                .map_err(|e| e.to_string())?;
            for b in rows {
                let (id, bbox) = b.map_err(|e| e.to_string())?;
                if let Some(p) = best_overlap(&bbox, &pp, opts.resolve_iou) {
                    changes.push(database::ClassChange {
                        id,
                        page_key: row.image_id.clone(),
                        from: ABANDON,
                        to: p.class_id,
                    });
                }
            }
        }
    }
    progress("Saving the new labels", 0, 0);
    let n = changes.len();
    database::apply_class_changes(
        conn,
        "abandon to PP-DocLayout-L's label for the same region (one-time pass)",
        &changes,
    )?;
    Ok(n)
}

// --- COCO data directory --------------------------------------------------------

fn import_coco_dir(
    conn: &mut Connection,
    root: &Path,
    files: &[PathBuf],
    progress: &mut ProgressFn<'_>,
) -> Result<FolderReport, String> {
    let mut report = FolderReport {
        kind: "COCO data directory".into(),
        ..Default::default()
    };
    let done: HashSet<String> = {
        let mut st = conn
            .prepare("SELECT source_file FROM imports")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    for (i, f) in files.iter().enumerate() {
        progress("Importing COCO files", i as u64, files.len() as u64);
        let abs = std::path::absolute(f).unwrap_or_else(|_| f.clone());
        if done.contains(&*abs.to_string_lossy()) || done.contains(&*f.to_string_lossy()) {
            continue;
        }
        let set_name = f
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "set".into());
        let r = import_coco(
            conn,
            &abs,
            ImportOptions {
                set_name,
                images_root: root.to_path_buf(),
            },
        )?;
        report.pages_added += r.pages;
        report.pages_present += r.pages_skipped_existing;
        report.boxes += r.boxes;
        report.warnings.extend(r.warnings);
    }
    progress(
        "Importing COCO files",
        files.len() as u64,
        files.len() as u64,
    );
    Ok(report)
}

// --- plain image folders ----------------------------------------------------------

fn is_image(p: &Path) -> bool {
    p.extension().is_some_and(|x| {
        let x = x.to_string_lossy().to_ascii_lowercase();
        matches!(x.as_str(), "jpg" | "jpeg" | "png")
    })
}

/// Folders holding images, depth first, skipping hidden folders and the
/// project's own exports.
fn image_dirs(root: &Path) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut images = Vec::new();
        let mut subdirs = Vec::new();
        for entry in rd.filter_map(Result::ok) {
            let p = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            match entry.file_type() {
                Ok(t) if t.is_dir() => {
                    if !name.starts_with('.') && name != "exports" {
                        subdirs.push(p);
                    }
                }
                Ok(_) if is_image(&p) => images.push(p),
                _ => {}
            }
        }
        if !images.is_empty() {
            images.sort();
            out.push((dir, images));
        }
        subdirs.sort();
        stack.extend(subdirs.into_iter().rev());
    }
    out
}

fn import_image_folders(
    conn: &mut Connection,
    root: &Path,
    progress: &mut ProgressFn<'_>,
) -> Result<FolderReport, String> {
    let mut report = FolderReport {
        kind: "image folders".into(),
        ..Default::default()
    };
    progress("Listing image folders", 0, 0);
    let dirs = image_dirs(root);
    if dirs.is_empty() {
        return Err(format!(
            "{} has no teachers_raw.jsonl, no annotations/*.json and no .jpg or .png pages",
            root.display()
        ));
    }
    let total: u64 = dirs.iter().map(|(_, v)| v.len() as u64).sum();
    let mut next_ordinal: i64 = conn
        .query_row("SELECT COALESCE(MAX(seq), 0) FROM pages", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| e.to_string())?
        .div_euclid(BOOK_STRIDE)
        + 1;
    let mut done = 0u64;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut exists = tx
            .prepare_cached("SELECT 1 FROM pages WHERE page_key = ?1")
            .map_err(|e| e.to_string())?;
        let mut ins = tx
            .prepare_cached(
                "INSERT INTO pages (page_key, seq, book_id, split, set_name, source_path, file_name, width, height, page_index) \
                 VALUES (?1, ?2, ?3, 'new', 'folder', ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(|e| e.to_string())?;
        for (dir, images) in &dirs {
            let rel = dir.strip_prefix(root).unwrap_or(dir);
            let book = if rel.as_os_str().is_empty() {
                root.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "book".into())
            } else {
                rel.to_string_lossy().replace('\\', "/")
            };
            let mut book_new = false;
            for (i, img) in images.iter().enumerate() {
                done += 1;
                if done % 256 == 0 {
                    progress("Reading image sizes", done, total);
                }
                let stem = img
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let key = format!("{book}_{stem}");
                if exists.exists(params![key]).map_err(|e| e.to_string())? {
                    report.pages_present += 1;
                    continue;
                }
                let (w, h) = match image::image_dimensions(img) {
                    Ok(d) => d,
                    Err(e) => {
                        if report.warnings.len() < 20 {
                            report.warnings.push(format!("{}: {e}", img.display()));
                        }
                        continue;
                    }
                };
                if !book_new {
                    book_new = true;
                    report.books_added += 1;
                    next_ordinal += 1;
                }
                let file_name = img
                    .strip_prefix(root)
                    .unwrap_or(img)
                    .to_string_lossy()
                    .replace('\\', "/");
                let index = stem
                    .trim_start_matches(|c: char| !c.is_ascii_digit())
                    .parse::<i64>()
                    .ok();
                ins.execute(params![
                    key,
                    (next_ordinal - 1) * BOOK_STRIDE + i as i64,
                    book,
                    img.to_string_lossy(),
                    file_name,
                    w,
                    h,
                    index
                ])
                .map_err(|e| e.to_string())?;
                report.pages_added += 1;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    progress("Reading image sizes", total, total);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_ids_split_on_the_last_underscore() {
        assert_eq!(
            split_image_id("buddhism-1-2a89_x_p0012"),
            ("buddhism-1-2a89_x".into(), "p0012".into(), Some(12))
        );
        assert_eq!(split_image_id("loose"), ("".into(), "loose".into(), None));
    }

    #[test]
    fn duplicate_scores_of_one_region_collapse() {
        let d = |c: &str, s: f64| (c.to_owned(), s, 10.0, 10.0, 100.0, 50.0);
        let m = map_dets(
            &[
                d("paragraph_title", 0.56),
                d("header", 0.55),
                d("text", 0.1),
            ],
            paddle_class,
            0.3,
            200.0,
            200.0,
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].class_id, 1);
    }

    #[test]
    fn yolo_abandon_keeps_the_holding_class() {
        assert_eq!(yolo_class("abandon"), Some(ABANDON));
        assert_eq!(yolo_class("figure"), Some(6));
        assert_eq!(yolo_class("bogus"), None);
    }
}
