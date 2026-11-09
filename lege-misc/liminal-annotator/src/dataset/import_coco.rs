//! Streaming COCO import.
//!
//! The file is visited element by element: each `images[]` entry becomes a
//! page row and each `annotations[]` entry an annotation row as it is
//! parsed, so an 86 MB `train.json` never exists as one in-memory tree.
//! Everything the editor does not interpret is kept verbatim (`image_json`,
//! `provenance_json`) and written back out by the exporter.

use std::collections::HashMap;
use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, params};
use serde::Deserializer;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use super::database::{self, now_unix};
use crate::annotation::labels::{NUM_CLASSES, class_by_name};

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub set_name: String,
    pub pages: usize,
    pub pages_skipped_existing: usize,
    pub boxes: usize,
    pub ignore_regions: usize,
    pub boxes_skipped: usize,
    pub warnings: Vec<String>,
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} pages ({} already present, skipped), {} boxes ({} ignore regions), {} boxes skipped",
            self.set_name,
            self.pages,
            self.pages_skipped_existing,
            self.boxes,
            self.ignore_regions,
            self.boxes_skipped
        )?;
        for w in self.warnings.iter().take(20) {
            write!(f, "\n  warning: {w}")?;
        }
        if self.warnings.len() > 20 {
            write!(f, "\n  ... {} more warnings", self.warnings.len() - 20)?;
        }
        Ok(())
    }
}

pub struct ImportOptions {
    /// Name of the set (queue); defaults to the file stem.
    pub set_name: String,
    /// Directory `file_name` is relative to (the dataset's `data/`).
    pub images_root: PathBuf,
}

enum Item {
    Info(Value),
    Licenses(Value),
    Categories(Value),
    Image(Map<String, Value>),
    Annotation(Map<String, Value>),
}

/// Visits the top-level COCO object, streaming the two big arrays.
struct TopSeed<'a, F: FnMut(Item) -> Result<(), String>> {
    sink: &'a mut F,
}

impl<'de, F: FnMut(Item) -> Result<(), String>> DeserializeSeed<'de> for TopSeed<'_, F> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de, F: FnMut(Item) -> Result<(), String>> Visitor<'de> for TopSeed<'_, F> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a COCO object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "images" => map.next_value_seed(ArraySeed {
                    sink: &mut *self.sink,
                    images: true,
                })?,
                "annotations" => map.next_value_seed(ArraySeed {
                    sink: &mut *self.sink,
                    images: false,
                })?,
                other => {
                    let v: Value = map.next_value()?;
                    let item = match other {
                        "info" => Item::Info(v),
                        "licenses" => Item::Licenses(v),
                        "categories" => Item::Categories(v),
                        _ => continue,
                    };
                    (self.sink)(item).map_err(serde::de::Error::custom)?;
                }
            }
        }
        Ok(())
    }
}

struct ArraySeed<'a, F: FnMut(Item) -> Result<(), String>> {
    sink: &'a mut F,
    images: bool,
}

impl<'de, F: FnMut(Item) -> Result<(), String>> DeserializeSeed<'de> for ArraySeed<'_, F> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de, F: FnMut(Item) -> Result<(), String>> Visitor<'de> for ArraySeed<'_, F> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of objects")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(obj) = seq.next_element::<Map<String, Value>>()? {
            let item = if self.images {
                Item::Image(obj)
            } else {
                Item::Annotation(obj)
            };
            (self.sink)(item).map_err(serde::de::Error::custom)?;
        }
        Ok(())
    }
}

/// Stream a COCO file through `sink`.
fn stream_coco(
    path: &Path,
    mut sink: impl FnMut(Item) -> Result<(), String>,
) -> Result<(), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut de = serde_json::Deserializer::from_reader(reader);
    TopSeed { sink: &mut sink }
        .deserialize(&mut de)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    de.end()
        .map_err(|e| format!("{}: trailing data: {e}", path.display()))
}

const ANN_CORE_KEYS: [&str; 7] = [
    "id",
    "image_id",
    "category_id",
    "bbox",
    "area",
    "iscrowd",
    "segmentation",
];

fn as_f64(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

fn as_str(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
                .join(","),
        ),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

struct ImportState<'t> {
    tx: Transaction<'t>,
    opts: ImportOptions,
    report: ImportReport,
    /// COCO image id → page key (None = page skipped).
    image_ids: HashMap<i64, Option<String>>,
    /// COCO category id → taxonomy class id.
    categories: HashMap<i64, u8>,
    next_seq: i64,
    ord: HashMap<String, i64>,
    pending_annotations: Vec<Map<String, Value>>,
    info: Option<Value>,
    licenses: Option<Value>,
    categories_json: Option<Value>,
}

impl ImportState<'_> {
    fn categories_from(&mut self, v: &Value) {
        if let Some(arr) = v.as_array() {
            for c in arr {
                let Some(id) = c.get("id").and_then(Value::as_i64) else {
                    continue;
                };
                let tax = c
                    .get("taxonomy_id")
                    .and_then(Value::as_i64)
                    .filter(|t| (0..NUM_CLASSES as i64).contains(t))
                    .map(|t| t as u8)
                    .or_else(|| {
                        c.get("name")
                            .and_then(Value::as_str)
                            .and_then(class_by_name)
                    });
                match tax {
                    Some(t) => {
                        self.categories.insert(id, t);
                    }
                    None => self.report.warnings.push(format!(
                        "category {id} ({:?}) does not map to the taxonomy",
                        c.get("name")
                    )),
                }
            }
        }
    }

    fn class_for(&self, category_id: i64) -> Option<u8> {
        if let Some(&c) = self.categories.get(&category_id) {
            return Some(c);
        }
        // No categories block: the dataset convention, category = taxonomy + 1.
        if self.categories.is_empty() && (1..=NUM_CLASSES as i64).contains(&category_id) {
            return Some((category_id - 1) as u8);
        }
        None
    }

    fn image(&mut self, img: Map<String, Value>) -> Result<(), String> {
        let id = img
            .get("id")
            .and_then(Value::as_i64)
            .ok_or("image without integer id")?;
        let file_name =
            as_str(img.get("file_name")).ok_or_else(|| format!("image {id}: no file_name"))?;
        let width = as_f64(img.get("width")).ok_or_else(|| format!("image {id}: no width"))? as i64;
        let height =
            as_f64(img.get("height")).ok_or_else(|| format!("image {id}: no height"))? as i64;
        let fpath = Path::new(&file_name);
        let page_key = as_str(img.get("page_key")).unwrap_or_else(|| {
            let parent = fpath
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let stem = fpath
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if parent.is_empty() {
                stem
            } else {
                format!("{parent}_{stem}")
            }
        });
        let book_id = as_str(img.get("book_id")).unwrap_or_else(|| {
            fpath
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into())
        });
        let split = as_str(img.get("split")).unwrap_or_else(|| self.opts.set_name.clone());
        let source_path = if fpath.is_absolute() {
            fpath.to_path_buf()
        } else {
            self.opts.images_root.join(fpath)
        };
        let exists: bool = self
            .tx
            .prepare_cached("SELECT 1 FROM pages WHERE page_key = ?1")
            .map_err(|e| e.to_string())?
            .exists(params![page_key])
            .map_err(|e| e.to_string())?;
        if exists {
            self.report.pages_skipped_existing += 1;
            self.image_ids.insert(id, None);
            return Ok(());
        }
        if width <= 0 || height <= 0 {
            self.report.warnings.push(format!(
                "{page_key}: invalid size {width}x{height}; skipped"
            ));
            self.image_ids.insert(id, None);
            return Ok(());
        }
        self.tx
            .prepare_cached(
                "INSERT INTO pages (page_key, seq, book_id, split, set_name, source_path, file_name, width, height, \
                 book_type, page_index, queue_reason, coco_image_id, image_json, source_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )
            .map_err(|e| e.to_string())?
            .execute(params![
                page_key,
                self.next_seq,
                book_id,
                split,
                self.opts.set_name,
                source_path.to_string_lossy(),
                file_name,
                width,
                height,
                as_str(img.get("book_type")),
                img.get("page_index").and_then(Value::as_i64),
                as_str(img.get("queue_reason")),
                id,
                Value::Object(img.clone()).to_string(),
                as_str(img.get("pdf_sha1")),
            ])
            .map_err(|e| format!("{page_key}: {e}"))?;
        self.next_seq += 1;
        self.report.pages += 1;
        self.image_ids.insert(id, Some(page_key));
        Ok(())
    }

    fn annotation(&mut self, a: Map<String, Value>) -> Result<(), String> {
        let image_id = a
            .get("image_id")
            .and_then(Value::as_i64)
            .ok_or("annotation without image_id")?;
        let page_key = match self.image_ids.get(&image_id) {
            Some(Some(k)) => k.clone(),
            Some(None) => return Ok(()), // page skipped
            None => {
                // Annotation before its image: hold it until the end.
                self.pending_annotations.push(a);
                return Ok(());
            }
        };
        let ann_id = a.get("id").map(|v| v.to_string()).unwrap_or_default();
        let cat = a.get("category_id").and_then(Value::as_i64).unwrap_or(-1);
        let Some(class_id) = self.class_for(cat) else {
            self.report.boxes_skipped += 1;
            self.report
                .warnings
                .push(format!("{page_key} ann {ann_id}: unknown category {cat}"));
            return Ok(());
        };
        let bbox: Vec<f64> = a
            .get("bbox")
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if bbox.len() != 4 || !(bbox[2] > 0.0 && bbox[3] > 0.0) {
            self.report.boxes_skipped += 1;
            self.report
                .warnings
                .push(format!("{page_key} ann {ann_id}: degenerate bbox {bbox:?}"));
            return Ok(());
        }
        let (x0, y0, x1, y1) = (bbox[0], bbox[1], bbox[0] + bbox[2], bbox[1] + bbox[3]);
        let is_ignore = a.get("iscrowd").and_then(Value::as_i64).unwrap_or(0) != 0;
        let extras: Map<String, Value> = a
            .iter()
            .filter(|(k, _)| !ANN_CORE_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut extras = extras;
        extras.insert(
            "coco_id".into(),
            a.get("id").cloned().unwrap_or(Value::Null),
        );
        // The exact source geometry, so an unedited box exports byte-identical
        // numbers (the source may carry float32 artefacts like 23.200000762939453).
        let mut source = Map::new();
        for k in ["bbox", "area", "segmentation"] {
            if let Some(v) = a.get(k) {
                source.insert(k.into(), v.clone());
            }
        }
        extras.insert("_source".into(), Value::Object(source));
        let provenance = Value::Object(extras).to_string();
        let id = format!("{}/{}", self.opts.set_name, ann_id);
        let ord = self.ord.entry(page_key.clone()).or_insert(0);
        let this_ord = *ord;
        *ord += 1;
        for table in ["annotations", "imported_annotations"] {
            self.tx
                .prepare_cached(&format!(
                    "INSERT INTO {table} (id, page_key, ord, class_id, x0, y0, x1, y1, is_ignore, provenance_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
                ))
                .map_err(|e| e.to_string())?
                .execute(params![id, page_key, this_ord, class_id as i64, x0, y0, x1, y1, is_ignore as i64, provenance])
                .map_err(|e| format!("{page_key} ann {ann_id}: {e}"))?;
        }
        self.report.boxes += 1;
        if is_ignore {
            self.report.ignore_regions += 1;
        }
        Ok(())
    }
}

/// Import one COCO file into the project. One transaction: a failed import
/// leaves the project unchanged.
pub fn import_coco(
    conn: &mut Connection,
    path: &Path,
    mut opts: ImportOptions,
) -> Result<ImportReport, String> {
    // Stored paths must not depend on the importer's working directory.
    opts.images_root = std::path::absolute(&opts.images_root)
        .map_err(|e| format!("{}: {e}", opts.images_root.display()))?;
    if !opts.images_root.is_dir() {
        return Err(format!(
            "images root {} is not a directory",
            opts.images_root.display()
        ));
    }
    let next_seq: i64 = conn
        .query_row("SELECT COALESCE(MAX(seq), -1) + 1 FROM pages", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut st = ImportState {
        tx,
        report: ImportReport {
            set_name: opts.set_name.clone(),
            ..Default::default()
        },
        opts,
        image_ids: HashMap::new(),
        categories: HashMap::new(),
        next_seq,
        ord: HashMap::new(),
        pending_annotations: Vec::new(),
        info: None,
        licenses: None,
        categories_json: None,
    };
    stream_coco(path, |item| match item {
        Item::Info(v) => {
            st.info = Some(v);
            Ok(())
        }
        Item::Licenses(v) => {
            st.licenses = Some(v);
            Ok(())
        }
        Item::Categories(v) => {
            st.categories_from(&v);
            st.categories_json = Some(v);
            Ok(())
        }
        Item::Image(img) => st.image(img),
        Item::Annotation(a) => st.annotation(a),
    })?;
    let pending = std::mem::take(&mut st.pending_annotations);
    for a in pending {
        if st.image_ids.contains_key(
            &a.get("image_id")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MIN),
        ) {
            st.annotation(a)?;
        } else {
            st.report.boxes_skipped += 1;
        }
    }
    let ImportState {
        tx,
        opts,
        report,
        info,
        licenses,
        categories_json,
        ..
    } = st;
    tx.execute(
        "INSERT INTO imports (set_name, source_file, images_root, imported_at, info_json, licenses_json, categories_json, pages, boxes) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            opts.set_name,
            path.to_string_lossy(),
            opts.images_root.to_string_lossy(),
            now_unix(),
            info.map(|v| v.to_string()),
            licenses.map(|v| v.to_string()),
            categories_json.map(|v| v.to_string()),
            report.pages as i64,
            report.boxes as i64
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(report)
}

/// Import model predictions. Accepts a COCO *results* list
/// (`[{image_id|page_key, category_id, bbox, score}]`) or a full COCO file
/// whose annotations carry `score`. `image_id`s are resolved through the
/// project's imported COCO ids for `id_set` (or any set when unique).
/// `scale_from` un-stretches predictions made at an `S×S` model input.
pub fn import_predictions(
    conn: &mut Connection,
    path: &Path,
    model: &str,
    id_set: Option<&str>,
    scale_from: Option<u32>,
    min_score: f64,
) -> Result<(usize, usize), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let list: Vec<Value> = match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("annotations") {
            Some(Value::Array(a)) => a,
            _ => return Err("expected a results array or a COCO object with annotations".into()),
        },
        _ => return Err("expected a results array".into()),
    };
    // image id → (page_key, w, h)
    let mut ids: HashMap<i64, (String, f64, f64)> = HashMap::new();
    let ambiguous: std::collections::HashSet<i64>;
    let mut keys: HashMap<String, (f64, f64)> = HashMap::new();
    {
        let mut st = conn
            .prepare("SELECT page_key, coco_image_id, set_name, width, height FROM pages")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut dup = std::collections::HashSet::new();
        for r in rows {
            let (k, id, set, w, h) = r.map_err(|e| e.to_string())?;
            keys.insert(k.clone(), (w, h));
            if let Some(id) = id {
                if id_set.is_none_or(|s| s == set) && ids.insert(id, (k, w, h)).is_some() {
                    dup.insert(id);
                }
            }
        }
        for d in &dup {
            ids.remove(d);
        }
        ambiguous = dup;
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM predictions WHERE model = ?1", params![model])
        .map_err(|e| e.to_string())?;
    let mut n = 0;
    let mut skipped = 0;
    {
        let mut ins = tx
            .prepare("INSERT INTO predictions (model, page_key, class_id, score, x0, y0, x1, y1) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")
            .map_err(|e| e.to_string())?;
        for p in list {
            let score = p.get("score").and_then(Value::as_f64).unwrap_or(1.0);
            if score < min_score {
                continue;
            }
            let target = if let Some(k) = p.get("page_key").and_then(Value::as_str) {
                keys.get(k).map(|&(w, h)| (k.to_owned(), w, h))
            } else {
                let id = p.get("image_id").and_then(Value::as_i64);
                if let Some(i) = id.filter(|i| ambiguous.contains(i)) {
                    return Err(format!(
                        "COCO image id {i} occurs in more than one set; pass --set to say which set the ids belong to"
                    ));
                }
                id.and_then(|i| ids.get(&i).cloned())
            };
            let cat = p.get("category_id").and_then(Value::as_i64).unwrap_or(0);
            let bbox: Vec<f64> = p
                .get("bbox")
                .and_then(Value::as_array)
                .map(|v| v.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            let (Some((key, w, h)), true, true) = (
                target,
                (1..=NUM_CLASSES as i64).contains(&cat),
                bbox.len() == 4,
            ) else {
                skipped += 1;
                continue;
            };
            let (sx, sy) = match scale_from {
                Some(s) => (w / s as f64, h / s as f64),
                None => (1.0, 1.0),
            };
            let (x0, y0) = (bbox[0] * sx, bbox[1] * sy);
            let (x1, y1) = ((bbox[0] + bbox[2]) * sx, (bbox[1] + bbox[3]) * sy);
            ins.execute(params![model, key, cat - 1, score, x0, y0, x1, y1])
                .map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((n, skipped))
}

/// Re-export for callers that only need to open a project.
pub use database::open;
