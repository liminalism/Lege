//! SQLite project database: the authoritative annotation store.
//!
//! - `pages`: one row per page image, with review status and queue order.
//! - `annotations`: the editable (human) layer, original-image coordinates.
//! - `imported_annotations`: the immutable layer as imported, for diffs.
//! - `predictions`: model outputs keyed by model name.
//! - `events`: an append-only log of committed page revisions.
//!
//! WAL mode; one writer thread commits page snapshots in order and
//! acknowledges each revision once its transaction is durable.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::annotation::commands::{Annotation, ReviewStatus};
use crate::annotation::geometry::Box2;
use crate::annotation::labels::{ABANDON, NUM_LABELS};

pub const SCHEMA_VERSION: i64 = 2;

/// The editable layer. Class 14 is the `abandon` holding class
/// ([`crate::annotation::labels::ABANDON`]); schema 1 allowed 0..=13 only.
const ANNOTATIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS annotations (
    id TEXT PRIMARY KEY,
    page_key TEXT NOT NULL REFERENCES pages(page_key),
    ord INTEGER NOT NULL,
    class_id INTEGER NOT NULL CHECK(class_id BETWEEN 0 AND 14),
    x0 REAL NOT NULL,
    y0 REAL NOT NULL,
    x1 REAL NOT NULL,
    y1 REAL NOT NULL,
    is_ignore INTEGER NOT NULL DEFAULT 0,
    provenance_json TEXT,
    CHECK(x1 > x0 AND y1 > y0)
);
"#;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pages (
    page_key TEXT PRIMARY KEY,
    seq INTEGER NOT NULL,
    book_id TEXT NOT NULL,
    split TEXT NOT NULL,
    set_name TEXT NOT NULL,
    source_path TEXT NOT NULL,
    file_name TEXT NOT NULL,
    source_hash TEXT,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    book_type TEXT,
    page_index INTEGER,
    queue_reason TEXT,
    review_status TEXT NOT NULL DEFAULT 'unseen',
    revision INTEGER NOT NULL DEFAULT 0,
    priority REAL NOT NULL DEFAULT 0,
    coco_image_id INTEGER,
    image_json TEXT,
    updated_at INTEGER,
    -- Kept in step with the editable layer (see PAGE_ABANDON_SET), so the
    -- book manager reads one covering index instead of joining every box.
    abandon INTEGER NOT NULL DEFAULT 0,
    abandon_large INTEGER NOT NULL DEFAULT 0,
    abandon_imported INTEGER NOT NULL DEFAULT 0,
    -- Boxes of each taxonomy class (n6 = figures), kept like `abandon`.
    n0 INTEGER NOT NULL DEFAULT 0, n1 INTEGER NOT NULL DEFAULT 0, n2 INTEGER NOT NULL DEFAULT 0,
    n3 INTEGER NOT NULL DEFAULT 0, n4 INTEGER NOT NULL DEFAULT 0, n5 INTEGER NOT NULL DEFAULT 0,
    n6 INTEGER NOT NULL DEFAULT 0, n7 INTEGER NOT NULL DEFAULT 0, n8 INTEGER NOT NULL DEFAULT 0,
    n9 INTEGER NOT NULL DEFAULT 0, n10 INTEGER NOT NULL DEFAULT 0, n11 INTEGER NOT NULL DEFAULT 0,
    n12 INTEGER NOT NULL DEFAULT 0, n13 INTEGER NOT NULL DEFAULT 0,
    -- The comparison model's figures (score >= 0.3), and those of them
    -- (score >= 0.5) no figure of the editable layer overlaps: see MODEL_FIG_SET.
    -- Then the area in image pixels of the largest figure box of the
    -- editable layer (0: none), kept the same way: see FIG_AREA_SET. (No
    -- comment line right above the last column: SQLite's DROP COLUMN would
    -- leave the closing parenthesis inside it.)
    model_fig INTEGER NOT NULL DEFAULT 0,
    model_only_fig INTEGER NOT NULL DEFAULT 0,
    fig_area REAL NOT NULL DEFAULT 0
);

-- Paper colour of a book, measured from a few sampled pages (see
-- `dataset::tone`): the median colour of each page's brightest pixels.
-- Per-book decisions: left out of every export, and/or marked as needing
-- significant work to come back to.
CREATE TABLE IF NOT EXISTS book_flags (
    book_id TEXT PRIMARY KEY,
    excluded INTEGER NOT NULL DEFAULT 0,
    needs_work INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS book_tone (
    book_id TEXT PRIMARY KEY,
    r INTEGER NOT NULL,
    g INTEGER NOT NULL,
    b INTEGER NOT NULL,
    lightness REAL NOT NULL,
    yellowness REAL NOT NULL,
    pages INTEGER NOT NULL,
    measured_at INTEGER NOT NULL
);


CREATE TABLE IF NOT EXISTS imported_annotations (
    id TEXT PRIMARY KEY,
    page_key TEXT NOT NULL REFERENCES pages(page_key),
    ord INTEGER NOT NULL,
    class_id INTEGER NOT NULL,
    x0 REAL NOT NULL,
    y0 REAL NOT NULL,
    x1 REAL NOT NULL,
    y1 REAL NOT NULL,
    is_ignore INTEGER NOT NULL DEFAULT 0,
    provenance_json TEXT
);

CREATE TABLE IF NOT EXISTS predictions (
    model TEXT NOT NULL,
    page_key TEXT NOT NULL REFERENCES pages(page_key),
    class_id INTEGER NOT NULL,
    score REAL NOT NULL,
    x0 REAL NOT NULL,
    y0 REAL NOT NULL,
    x1 REAL NOT NULL,
    y1 REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    page_key TEXT NOT NULL,
    at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    revision INTEGER NOT NULL,
    payload TEXT
);

CREATE TABLE IF NOT EXISTS imports (
    id INTEGER PRIMARY KEY,
    set_name TEXT NOT NULL,
    source_file TEXT NOT NULL,
    images_root TEXT NOT NULL,
    imported_at INTEGER NOT NULL,
    info_json TEXT,
    licenses_json TEXT,
    categories_json TEXT,
    pages INTEGER NOT NULL,
    boxes INTEGER NOT NULL
);

-- Bulk class conversions, kept so each can be undone as a whole.
CREATE TABLE IF NOT EXISTS bulk_ops (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    description TEXT NOT NULL,
    boxes INTEGER NOT NULL,
    pages INTEGER NOT NULL,
    undone INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS bulk_op_items (
    op_id INTEGER NOT NULL,
    annotation_id TEXT NOT NULL,
    page_key TEXT NOT NULL,
    from_class INTEGER NOT NULL,
    to_class INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS annotations_by_page ON annotations(page_key);
CREATE INDEX IF NOT EXISTS annotations_abandon ON annotations(page_key) WHERE class_id = 14;
CREATE INDEX IF NOT EXISTS imported_abandon ON imported_annotations(page_key) WHERE class_id = 14;
CREATE INDEX IF NOT EXISTS pages_by_book ON pages(book_id, seq);
CREATE INDEX IF NOT EXISTS bulk_items_by_op ON bulk_op_items(op_id);
-- Undo looks items up per box: without this a 400k-box change is quadratic.
CREATE INDEX IF NOT EXISTS bulk_items_by_op_box ON bulk_op_items(op_id, annotation_id);
CREATE INDEX IF NOT EXISTS imported_by_page ON imported_annotations(page_key);
CREATE INDEX IF NOT EXISTS predictions_by_page ON predictions(page_key, model);
CREATE INDEX IF NOT EXISTS pages_by_review ON pages(split, review_status);
CREATE INDEX IF NOT EXISTS pages_by_set ON pages(set_name, seq);
CREATE INDEX IF NOT EXISTS pages_by_seq ON pages(seq);
CREATE INDEX IF NOT EXISTS events_by_page ON events(page_key);
"#;

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub type DbResult<T> = Result<T, String>;

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

/// Open (creating if needed) a project database with the schema applied.
pub fn open(path: &Path) -> DbResult<Connection> {
    let conn = Connection::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    configure(&conn)?;
    migrate_annotations(&conn)?;
    conn.execute_batch(ANNOTATIONS_TABLE).map_err(e)?;
    conn.execute_batch(SCHEMA).map_err(e)?;
    migrate_page_counts(&conn)?;
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )
    .map_err(e)?;
    Ok(conn)
}

/// Schema 1 → 2: the editable layer's class CHECK grows to admit the
/// `abandon` holding class. SQLite cannot alter a CHECK, so the table is
/// rebuilt, in one transaction, keeping every row.
fn migrate_annotations(conn: &Connection) -> DbResult<()> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'annotations'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(e)?;
    if !sql.is_some_and(|s| s.contains("BETWEEN 0 AND 13")) {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "BEGIN IMMEDIATE;
         ALTER TABLE annotations RENAME TO annotations_schema1;
         {ANNOTATIONS_TABLE}
         INSERT INTO annotations (id, page_key, ord, class_id, x0, y0, x1, y1, is_ignore, provenance_json)
             SELECT id, page_key, ord, class_id, x0, y0, x1, y1, is_ignore, provenance_json FROM annotations_schema1;
         DROP TABLE annotations_schema1;
         COMMIT;"
    ))
    .map_err(|err| {
        let _ = conn.execute_batch("ROLLBACK");
        format!("upgrading the project to schema 2: {err}")
    })
}

/// `abandon` boxes in a page's editable layer, and those of them covering
/// 4% of the page or more (the same rule as [`Region::Large`]).
pub const PAGE_ABANDON_SET: &str = "abandon = (SELECT COUNT(*) FROM annotations a \
     WHERE a.page_key = pages.page_key AND a.class_id = 14), \
     abandon_large = (SELECT COUNT(*) FROM annotations a \
     WHERE a.page_key = pages.page_key AND a.class_id = 14 \
     AND (a.x1 - a.x0) * (a.y1 - a.y0) >= 0.04 * pages.width * pages.height)";

/// Pictures the comparison model sees: `model_fig` counts its figures of
/// score 0.3 or more (a page with one, or with a figure box, is a picture
/// page); `model_only_fig` those of score 0.5 or more that no figure of the
/// editable layer overlaps (IoU 0.3): pictures the prefill missed. On the
/// 236,801-page pool that is 5,799 pages.
pub const MODEL_FIG_SET: &str = "model_fig = (SELECT COUNT(*) FROM predictions p \
     WHERE p.page_key = pages.page_key AND p.class_id = 6 AND p.score >= 0.3), \
     model_only_fig = (SELECT COUNT(*) FROM predictions p \
     WHERE p.page_key = pages.page_key AND p.class_id = 6 AND p.score >= 0.5 \
     AND NOT EXISTS (SELECT 1 FROM annotations a WHERE a.page_key = p.page_key AND a.class_id = 6 \
     AND max(0, min(a.x1, p.x1) - max(a.x0, p.x0)) * max(0, min(a.y1, p.y1) - max(a.y0, p.y0)) \
     >= 0.3 * ((a.x1 - a.x0) * (a.y1 - a.y0) + (p.x1 - p.x0) * (p.y1 - p.y0) \
     - max(0, min(a.x1, p.x1) - max(a.x0, p.x0)) * max(0, min(a.y1, p.y1) - max(a.y0, p.y0)))))";

/// The largest figure box of a page's editable layer, as an area in image
/// pixels (0 when there is none). "A figure covering at least share `s` of
/// the page" is then `fig_area >= s * width * height`, the same arithmetic
/// as testing each box, without touching `annotations`: on a 70,502-page
/// project the large-picture queue lists in 29 ms this way and 120 ms with a
/// correlated `EXISTS` over the boxes (the Q picker's counts: 65 ms, 230 ms).
pub const FIG_AREA_SET: &str = "fig_area = (SELECT COALESCE(MAX((a.x1 - a.x0) * (a.y1 - a.y0)), 0) \
     FROM annotations a WHERE a.page_key = pages.page_key AND a.class_id = 6)";

/// The share of the page the "large pictures" queue asks of a figure box
/// unless the settings say otherwise (`large_figure_percent`).
pub const DEFAULT_LARGE_FIGURE_SHARE: f64 = 0.40;

/// `fig_area` covers at least `share` of the page.
fn figure_share_sql(share: f64) -> String {
    // A plain f64 prints as its shortest round-trip form, which SQLite reads
    // back as the same double, so the bound is exact.
    let share = if share.is_finite() {
        share.max(0.0)
    } else {
        0.0
    };
    format!("(n6 > 0 AND fig_area >= {share:?} * width * height)")
}

/// Recompute [`MODEL_FIG_SET`] for every page (after an import adds
/// predictions): about a second for the 236,801-page pool.
pub fn recount_model_figures(conn: &Connection) -> DbResult<()> {
    conn.execute(&format!("UPDATE pages SET {MODEL_FIG_SET}"), [])
        .map(|_| ())
        .map_err(e)
}

/// Every per-page count: `abandon`, `abandon_large`, `n0`..`n13`, the
/// model's figures and `fig_area`, from the page's editable layer. Used after anything
/// that changes boxes.
pub fn page_counts_set() -> &'static str {
    static SET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        let mut parts = vec![
            PAGE_ABANDON_SET.to_owned(),
            MODEL_FIG_SET.to_owned(),
            FIG_AREA_SET.to_owned(),
        ];
        for c in 0..crate::annotation::labels::NUM_CLASSES {
            parts.push(format!(
                "n{c} = (SELECT COUNT(*) FROM annotations a \
                 WHERE a.page_key = pages.page_key AND a.class_id = {c})"
            ));
        }
        parts.join(", ")
    })
}

/// The `n0, n1, …, n13` column list.
pub fn class_columns() -> String {
    (0..crate::annotation::labels::NUM_CLASSES)
        .map(|c| format!("n{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Recount the boxes of the pages matching `where_sql` (after an import).
pub fn recount_pages(
    conn: &Connection,
    where_sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> DbResult<()> {
    conn.execute(
        &format!("UPDATE pages SET {} WHERE {where_sql}", page_counts_set()),
        args,
    )
    .map(|_| ())
    .map_err(e)
}

fn has_column(conn: &Connection, name: &str) -> DbResult<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('pages') WHERE name = ?1",
        params![name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .map_err(e)
}

/// Projects made before the per-page counts get the columns and their
/// values; every project gets the book manager's covering index.
fn migrate_page_counts(conn: &Connection) -> DbResult<()> {
    if !has_column(conn, "abandon_imported")? {
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             ALTER TABLE pages ADD COLUMN abandon INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE pages ADD COLUMN abandon_large INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE pages ADD COLUMN abandon_imported INTEGER NOT NULL DEFAULT 0;
             UPDATE pages SET {PAGE_ABANDON_SET},
                 abandon_imported = (SELECT COUNT(*) FROM imported_annotations a
                                     WHERE a.page_key = pages.page_key AND a.class_id = 14);
             COMMIT;"
        ))
        .map_err(|err| {
            let _ = conn.execute_batch("ROLLBACK");
            format!("adding per-page abandon counts: {err}")
        })?;
    }
    if !has_column(conn, "n13")? {
        // One pass over the boxes into a per-page pivot, then one join: the
        // 232k-page pool takes a few seconds, once.
        let cols = class_columns();
        let add: String = (0..crate::annotation::labels::NUM_CLASSES)
            .map(|c| format!("ALTER TABLE pages ADD COLUMN n{c} INTEGER NOT NULL DEFAULT 0;\n"))
            .collect();
        let pivot: String = (0..crate::annotation::labels::NUM_CLASSES)
            .map(|c| format!("SUM(class_id = {c}) AS n{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let set: String = (0..crate::annotation::labels::NUM_CLASSES)
            .map(|c| format!("n{c} = pc.n{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             {add}
             CREATE TEMP TABLE page_class_counts AS
                 SELECT page_key, {pivot} FROM annotations GROUP BY page_key;
             UPDATE pages SET {set} FROM page_class_counts pc WHERE pc.page_key = pages.page_key;
             DROP TABLE page_class_counts;
             COMMIT;"
        ))
        .map_err(|err| {
            let _ = conn.execute_batch("ROLLBACK");
            format!("adding per-page class counts ({cols}): {err}")
        })?;
    }
    if !has_column(conn, "model_only_fig")? {
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             ALTER TABLE pages ADD COLUMN model_fig INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE pages ADD COLUMN model_only_fig INTEGER NOT NULL DEFAULT 0;
             UPDATE pages SET {MODEL_FIG_SET};
             COMMIT;"
        ))
        .map_err(|err| {
            let _ = conn.execute_batch("ROLLBACK");
            format!("adding per-page model figure counts: {err}")
        })?;
    }
    if !has_column(conn, "fig_area")? {
        // Only pages with a figure box have anything to look up: about
        // 1.4 s for the 70,502-page project, once.
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             ALTER TABLE pages ADD COLUMN fig_area REAL NOT NULL DEFAULT 0;
             UPDATE pages SET {FIG_AREA_SET} WHERE n6 > 0;
             COMMIT;"
        ))
        .map_err(|err| {
            let _ = conn.execute_batch("ROLLBACK");
            format!("adding per-page figure areas: {err}")
        })?;
    } else {
        heal_fig_area(conn)?;
    }
    meta_set(conn, "fig_area_checked_at", &now_unix().to_string())?;
    conn.execute_batch(&format!(
        "DROP INDEX IF EXISTS pages_book_stats;
         DROP INDEX IF EXISTS pages_book_stats2;
         CREATE INDEX IF NOT EXISTS pages_book_stats3 ON pages(book_id, seq, review_status, \
         abandon, abandon_large, abandon_imported, split, model_fig, model_only_fig, {});",
        class_columns()
    ))
    .map_err(e)
}

/// A build from before `fig_area` (one still running, or started again)
/// saves pages without updating it. On every open, the pages saved since the
/// last open and any page whose figure count and area disagree are measured
/// again: one scan of `pages`, 38 ms for 70,502 pages.
fn heal_fig_area(conn: &Connection) -> DbResult<()> {
    let since: i64 = meta_get(conn, "fig_area_checked_at")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    conn.execute(
        &format!(
            "UPDATE pages SET {FIG_AREA_SET} \
             WHERE (n6 > 0) <> (fig_area > 0) OR updated_at >= ?1"
        ),
        params![since - 1],
    )
    .map(|_| ())
    .map_err(e)
}

/// A project-level setting (`meta` table).
pub fn meta_get(conn: &Connection, key: &str) -> DbResult<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
        r.get(0)
    })
    .optional()
    .map_err(e)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> DbResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map(|_| ())
    .map_err(e)
}

fn configure(conn: &Connection) -> DbResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL").map_err(e)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(e)?;
    conn.pragma_update(None, "foreign_keys", "ON").map_err(e)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(e)?;
    Ok(())
}

/// Page metadata as the editor needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct PageMeta {
    pub page_key: String,
    pub seq: i64,
    pub book_id: String,
    pub split: String,
    pub set_name: String,
    pub source_path: PathBuf,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub book_type: Option<String>,
    pub page_index: Option<i64>,
    pub queue_reason: Option<String>,
    pub status: ReviewStatus,
    pub revision: i64,
}

const PAGE_COLS: &str = "page_key, seq, book_id, split, set_name, source_path, file_name, width, height, \
                         book_type, page_index, queue_reason, review_status, revision";

fn page_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PageMeta> {
    let status: String = r.get(12)?;
    Ok(PageMeta {
        page_key: r.get(0)?,
        seq: r.get(1)?,
        book_id: r.get(2)?,
        split: r.get(3)?,
        set_name: r.get(4)?,
        source_path: PathBuf::from(r.get::<_, String>(5)?),
        file_name: r.get(6)?,
        width: r.get::<_, i64>(7)? as u32,
        height: r.get::<_, i64>(8)? as u32,
        book_type: r.get(9)?,
        page_index: r.get(10)?,
        queue_reason: r.get(11)?,
        status: ReviewStatus::parse(&status).unwrap_or(ReviewStatus::Unseen),
        revision: r.get(13)?,
    })
}

pub fn page_meta(conn: &Connection, key: &str) -> DbResult<Option<PageMeta>> {
    conn.prepare_cached(&format!(
        "SELECT {PAGE_COLS} FROM pages WHERE page_key = ?1"
    ))
    .map_err(e)?
    .query_row(params![key], page_from_row)
    .optional()
    .map_err(e)
}

fn ann_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Annotation> {
    Ok(Annotation {
        id: r.get(0)?,
        class_id: r.get::<_, i64>(1)? as u8,
        bbox: Box2::new(r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?),
        is_ignore: r.get::<_, i64>(6)? != 0,
        provenance_json: r.get(7)?,
    })
}

pub fn annotations(conn: &Connection, key: &str) -> DbResult<Vec<Annotation>> {
    let mut st = conn
        .prepare_cached(
            "SELECT id, class_id, x0, y0, x1, y1, is_ignore, provenance_json \
             FROM annotations WHERE page_key = ?1 ORDER BY ord",
        )
        .map_err(e)?;
    let rows = st.query_map(params![key], ann_from_row).map_err(e)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(e)
}

pub fn imported_annotations(conn: &Connection, key: &str) -> DbResult<Vec<Annotation>> {
    let mut st = conn
        .prepare_cached(
            "SELECT id, class_id, x0, y0, x1, y1, is_ignore, provenance_json \
             FROM imported_annotations WHERE page_key = ?1 ORDER BY ord",
        )
        .map_err(e)?;
    let rows = st.query_map(params![key], ann_from_row).map_err(e)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(e)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prediction {
    pub class_id: u8,
    pub score: f64,
    pub bbox: Box2,
}

pub fn predictions(conn: &Connection, key: &str, model: &str) -> DbResult<Vec<Prediction>> {
    let mut st = conn
        .prepare_cached(
            "SELECT class_id, score, x0, y0, x1, y1 FROM predictions \
             WHERE page_key = ?1 AND model = ?2 ORDER BY score DESC",
        )
        .map_err(e)?;
    let rows = st
        .query_map(params![key, model], |r| {
            Ok(Prediction {
                class_id: r.get::<_, i64>(0)? as u8,
                score: r.get(1)?,
                bbox: Box2::new(r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?),
            })
        })
        .map_err(e)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(e)
}

pub fn models(conn: &Connection) -> DbResult<Vec<String>> {
    let mut st = conn
        .prepare("SELECT DISTINCT model FROM predictions ORDER BY model")
        .map_err(e)?;
    let rows = st.query_map([], |r| r.get(0)).map_err(e)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(e)
}

/// One page's complete committed state, as the writer stores it.
#[derive(Clone, Debug)]
pub struct PageSave {
    pub page_key: String,
    pub status: ReviewStatus,
    pub annotations: Vec<Annotation>,
    /// The editor's in-memory revision this snapshot represents.
    pub revision: u64,
    pub kind: &'static str,
}

/// Replace a page's editable layer and status in one transaction.
pub fn save_page(conn: &mut Connection, save: &PageSave) -> DbResult<()> {
    let tx = conn.transaction().map_err(e)?;
    {
        let exists: Option<i64> = tx
            .prepare_cached("SELECT 1 FROM pages WHERE page_key = ?1")
            .map_err(e)?
            .query_row(params![save.page_key], |r| r.get(0))
            .optional()
            .map_err(e)?;
        if exists.is_none() {
            return Err(format!("page {} is not in the project", save.page_key));
        }
        tx.prepare_cached("DELETE FROM annotations WHERE page_key = ?1")
            .map_err(e)?
            .execute(params![save.page_key])
            .map_err(e)?;
        let mut ins = tx
            .prepare_cached(
                "INSERT INTO annotations (id, page_key, ord, class_id, x0, y0, x1, y1, is_ignore, provenance_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )
            .map_err(e)?;
        for (ord, a) in save.annotations.iter().enumerate() {
            ins.execute(params![
                a.id,
                save.page_key,
                ord as i64,
                a.class_id as i64,
                a.bbox.x0,
                a.bbox.y0,
                a.bbox.x1,
                a.bbox.y1,
                a.is_ignore as i64,
                a.provenance_json
            ])
            .map_err(|err| format!("annotation {}: {err}", a.id))?;
        }
        let now = now_unix();
        tx.prepare_cached(&format!(
            "UPDATE pages SET review_status = ?2, revision = revision + 1, updated_at = ?3, \
             {} WHERE page_key = ?1",
            page_counts_set()
        ))
        .map_err(e)?
        .execute(params![save.page_key, save.status.as_str(), now])
        .map_err(e)?;
        tx.prepare_cached(
            "INSERT INTO events (page_key, at, kind, revision, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(e)?
        .execute(params![
            save.page_key,
            now,
            save.kind,
            save.revision as i64,
            format!(
                "{{\"status\":\"{}\",\"boxes\":{}}}",
                save.status.as_str(),
                save.annotations.len()
            )
        ])
        .map_err(e)?;
    }
    tx.commit().map_err(e)
}

/// Filter for the page queue. All fields are optional conjunctions.
#[derive(Clone, Debug, Default)]
pub struct PageFilter {
    pub set_name: Option<String>,
    pub split: Option<String>,
    pub book_id: Option<String>,
    pub statuses: Vec<ReviewStatus>,
    /// Substring of `queue_reason`.
    pub reason: Option<String>,
    /// Order by priority (descending, book-diversified) instead of import order.
    pub by_priority: bool,
    /// Only pages whose editable layer has a box of this class.
    pub has_class: Option<u8>,
    /// One of the review queues (Q).
    pub queue: Option<Queue>,
    /// Only pages whose editable layer has a figure box covering at least
    /// this share of the page (0..1; `--figure-min PCT`).
    pub figure_min: Option<f64>,
    /// The share [`Queue::LargePictures`] asks for (settings
    /// `large_figure_percent`); `None` is [`DEFAULT_LARGE_FIGURE_SHARE`].
    pub large_figure_share: Option<f64>,
}

/// The review queues the Q picker offers. Each is a condition on the
/// per-page counts, so a queue is one indexed scan; they order by page.
///
/// The imported tiers (A/B/C) are deliberately not among them: they measure
/// whether the two teachers agreed on pictures, and on the 846 pages
/// reviewed by 2026-11-09 tier A pages needed edits as often as tier C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Queue {
    /// Unseen or opened.
    ToDo,
    /// To do, with a figure in the editable layer or from the model.
    Pictures,
    /// To do, with a figure box covering a large share of the page
    /// ([`PageFilter::large_figure_share`]): page-filling plates whose box
    /// is cut short or misplaced.
    LargePictures,
    /// To do, with a confident model figure the editable layer lacks.
    ModelOnlyPictures,
    /// Any status, with `abandon` boxes left.
    Abandon,
    /// To do, without a single box.
    Empty,
    /// To do, plain text (no figure or table from anyone): every 20th page.
    TextSample,
    NeedsAttention,
    Excluded,
    Reviewed,
    All,
}

const TODO_SQL: &str = "review_status IN ('unseen', 'opened')";

impl Queue {
    pub const ALL: [Queue; 11] = [
        Queue::ToDo,
        Queue::Pictures,
        Queue::LargePictures,
        Queue::ModelOnlyPictures,
        Queue::Abandon,
        Queue::Empty,
        Queue::TextSample,
        Queue::NeedsAttention,
        Queue::Excluded,
        Queue::Reviewed,
        Queue::All,
    ];

    /// The command-line name (`--queue`).
    pub fn slug(self) -> &'static str {
        match self {
            Queue::ToDo => "todo",
            Queue::Pictures => "pictures",
            Queue::LargePictures => "large-pictures",
            Queue::ModelOnlyPictures => "model-only",
            Queue::Abandon => "abandon",
            Queue::Empty => "empty",
            Queue::TextSample => "text-sample",
            Queue::NeedsAttention => "attention",
            Queue::Excluded => "excluded",
            Queue::Reviewed => "reviewed",
            Queue::All => "all",
        }
    }

    pub fn from_slug(s: &str) -> Option<Queue> {
        Queue::ALL.into_iter().find(|q| q.slug() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Queue::ToDo => "To do",
            Queue::Pictures => "Picture pages, to do",
            Queue::LargePictures => "Large pictures, to do",
            Queue::ModelOnlyPictures => "Pictures only the model sees, to do",
            Queue::Abandon => "With abandon boxes",
            Queue::Empty => "Empty pages, to do",
            Queue::TextSample => "Plain text sample (1 in 20), to do",
            Queue::NeedsAttention => "Needs attention",
            Queue::Excluded => "Excluded pages",
            Queue::Reviewed => "Reviewed",
            Queue::All => "All pages",
        }
    }

    /// Why one would pick it.
    pub fn help(self) -> &'static str {
        match self {
            Queue::ToDo => "every page not yet reviewed, in book order",
            Queue::Pictures => "a figure in the boxes or from the model: where the edits are",
            Queue::LargePictures => {
                "a figure box covering a large part of the page: plates cut short or misaligned (G: six at a time)"
            }
            Queue::ModelOnlyPictures => {
                "the model finds a picture (score 0.5+) no figure box covers: C shows it, click it to adopt"
            }
            Queue::Abandon => "abandon boxes left, any status (export skips these pages)",
            Queue::Empty => "no box at all: blank, a plate both models missed, or a cover",
            Queue::TextSample => "text-only pages almost never need edits; check a sample",
            Queue::NeedsAttention => "pages you flagged with N",
            Queue::Excluded => "pages you excluded with X",
            Queue::Reviewed => "pages accepted with Right",
            Queue::All => "every page",
        }
    }

    /// The label with the share the large-picture queue uses.
    pub fn title(self, large_share: f64) -> String {
        match self {
            Queue::LargePictures => format!(
                "Large pictures ({}%+ of the page), to do",
                percent(large_share)
            ),
            q => q.label().to_owned(),
        }
    }

    /// The condition, with `large_share` for [`Queue::LargePictures`].
    pub fn sql(self, large_share: f64) -> String {
        let empty = format!(
            "abandon = 0 AND {} = 0",
            (0..crate::annotation::labels::NUM_CLASSES)
                .map(|c| format!("n{c}"))
                .collect::<Vec<_>>()
                .join(" + ")
        );
        match self {
            Queue::ToDo => TODO_SQL.into(),
            Queue::Pictures => format!("{TODO_SQL} AND (n6 > 0 OR model_fig > 0)"),
            Queue::LargePictures => format!("{TODO_SQL} AND {}", figure_share_sql(large_share)),
            Queue::ModelOnlyPictures => format!("{TODO_SQL} AND model_only_fig > 0"),
            Queue::Abandon => "abandon > 0".into(),
            Queue::Empty => format!("{TODO_SQL} AND {empty}"),
            Queue::TextSample => format!(
                "{TODO_SQL} AND n6 = 0 AND n8 = 0 AND model_fig = 0 AND abandon = 0 AND seq % 20 = 0"
            ),
            Queue::NeedsAttention => "review_status = 'needs_attention'".into(),
            Queue::Excluded => "review_status = 'excluded'".into(),
            Queue::Reviewed => "review_status = 'reviewed'".into(),
            Queue::All => "1".into(),
        }
    }
}

/// Pages in each of [`Queue::ALL`], within `scope` (its book or set; its
/// statuses and queue are ignored). One pass.
pub fn queue_counts(conn: &Connection, scope: &PageFilter) -> DbResult<Vec<i64>> {
    let mut f = scope.clone();
    f.statuses.clear();
    f.has_class = None;
    f.queue = None;
    f.by_priority = false;
    let (w, args) = f.where_clause();
    let share = f.large_share();
    let sums = Queue::ALL
        .iter()
        .map(|q| format!("SUM({})", q.sql(share)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {sums} FROM pages {w}");
    conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |r| {
        (0..Queue::ALL.len())
            .map(|i| Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0)))
            .collect()
    })
    .map_err(e)
}

/// A share as a whole percentage where it is one (`0.4` → `40`).
pub fn percent(share: f64) -> String {
    let p = share * 100.0;
    if (p - p.round()).abs() < 1e-9 {
        format!("{}", p.round())
    } else {
        format!("{p:.1}")
    }
}

impl PageFilter {
    /// The share [`Queue::LargePictures`] uses.
    pub fn large_share(&self) -> f64 {
        self.large_figure_share
            .filter(|s| s.is_finite() && *s >= 0.0)
            .unwrap_or(DEFAULT_LARGE_FIGURE_SHARE)
    }

    fn where_clause(&self) -> (String, Vec<String>) {
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        if let Some(s) = &self.set_name {
            args.push(s.clone());
            clauses.push(format!("set_name = ?{}", args.len()));
        }
        if let Some(s) = &self.split {
            args.push(s.clone());
            clauses.push(format!("split = ?{}", args.len()));
        }
        if let Some(s) = &self.book_id {
            args.push(s.clone());
            clauses.push(format!("book_id = ?{}", args.len()));
        }
        if let Some(s) = &self.reason {
            args.push(format!("%{s}%"));
            clauses.push(format!("queue_reason LIKE ?{}", args.len()));
        }
        if !self.statuses.is_empty() {
            let mut ors = Vec::new();
            for st in &self.statuses {
                args.push(st.as_str().to_owned());
                ors.push(format!("?{}", args.len()));
            }
            clauses.push(format!("review_status IN ({})", ors.join(", ")));
        }
        if let Some(c) = self.has_class {
            // A number, not user text: inlined so the partial index on
            // abandon boxes can serve it.
            clauses.push(format!(
                "page_key IN (SELECT page_key FROM annotations WHERE class_id = {c})"
            ));
        }
        if let Some(q) = self.queue {
            clauses.push(format!("({})", q.sql(self.large_share())));
        }
        if let Some(share) = self.figure_min {
            clauses.push(figure_share_sql(share));
        }
        let w = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        (w, args)
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = &self.set_name {
            parts.push(s.clone());
        }
        if let Some(s) = &self.split {
            parts.push(format!("split={s}"));
        }
        if let Some(s) = &self.book_id {
            parts.push(format!("book={s}"));
        }
        if let Some(s) = &self.reason {
            parts.push(format!("reason~{s}"));
        }
        if !self.statuses.is_empty() {
            parts.push(
                self.statuses
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("|"),
            );
        }
        if let Some(c) = self.has_class {
            parts.push(format!("with {}", crate::annotation::labels::class(c).name));
        }
        if let Some(share) = self.figure_min {
            parts.push(format!("figure {}%+", percent(share)));
        }
        if let Some(q) = self.queue {
            parts.push(q.title(self.large_share()));
        }
        if self.by_priority {
            parts.push("by priority".into());
        }
        if parts.is_empty() {
            "All pages".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// The queue: page keys in order. Keys only — never images, never boxes.
pub fn page_keys(conn: &Connection, filter: &PageFilter) -> DbResult<Vec<String>> {
    let (w, args) = filter.where_clause();
    if filter.by_priority {
        let sql = format!("SELECT page_key, book_id, priority FROM pages {w} ORDER BY seq");
        let mut st = conn.prepare(&sql).map_err(e)?;
        let rows = st
            .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })
            .map_err(e)?;
        let rows = rows.collect::<Result<Vec<_>, _>>().map_err(e)?;
        return Ok(crate::review::queue::diversify(rows));
    }
    let sql = format!("SELECT page_key FROM pages {w} ORDER BY seq");
    let mut st = conn.prepare(&sql).map_err(e)?;
    let rows = st
        .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get(0))
        .map_err(e)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(e)
}

pub fn status_counts(conn: &Connection, filter: &PageFilter) -> DbResult<Vec<(ReviewStatus, i64)>> {
    let mut f = filter.clone();
    f.statuses.clear();
    let (w, args) = f.where_clause();
    let sql = format!("SELECT review_status, COUNT(*) FROM pages {w} GROUP BY review_status");
    let mut st = conn.prepare(&sql).map_err(e)?;
    let rows = st
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(e)?;
    let mut out = Vec::new();
    for r in rows {
        let (s, n) = r.map_err(e)?;
        if let Some(s) = ReviewStatus::parse(&s) {
            out.push((s, n));
        }
    }
    Ok(out)
}

// --- books -----------------------------------------------------------------

/// One book (image folder) of the project, as the book manager lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookStats {
    pub book_id: String,
    pub split: String,
    pub pages: i64,
    /// Reviewed or excluded.
    pub done: i64,
    pub needs_attention: i64,
    /// `abandon` boxes still in the editable layer, and the pages carrying them.
    pub abandon_boxes: i64,
    pub abandon_pages: i64,
    /// Of `abandon_boxes`, those covering 4% of the page or more. Mostly
    /// footnote blocks: PP-DocLayout-L calls 81% of them footnotes.
    pub abandon_large: i64,
    /// `abandon` boxes as imported, before any conversion.
    pub abandon_imported: i64,
    /// Boxes of each class in the editable layer (index = class id; the
    /// last is `abandon`), and the pages that have at least one.
    pub class_boxes: [i64; NUM_LABELS],
    pub class_pages: [i64; NUM_LABELS],
    /// Paper colour, once measured.
    pub tone: Option<Tone>,
    /// Left out of training (every export skips the book).
    pub excluded: bool,
    /// Marked as needing significant work, to come back to.
    pub needs_work: bool,
    /// Pages not yet reviewed (unseen or opened).
    pub todo: i64,
    /// Pages with a picture (a figure box, or a figure from the model), and
    /// those of them still to do.
    pub picture_pages: i64,
    pub picture_todo: i64,
    /// Pages where the model sees a picture no figure box covers, to do.
    pub model_only_todo: i64,
}

/// A book's paper colour (see `dataset::tone`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tone {
    pub rgb: [u8; 3],
    /// CIE L*, 0 (black) to 100 (white).
    pub lightness: f64,
    /// CIE b*: about 0 for white or grey paper, 10-25 for yellowed paper.
    pub yellowness: f64,
    pub pages: i64,
}

impl BookStats {
    /// Boxes of class `c` per page.
    pub fn per_page(&self, c: usize) -> f64 {
        self.class_boxes[c] as f64 / self.pages.max(1) as f64
    }

    /// Share of pages with at least one box of class `c`, 0..=1.
    pub fn page_share(&self, c: usize) -> f64 {
        self.class_pages[c] as f64 / self.pages.max(1) as f64
    }

    pub fn abandon_page_share(&self) -> f64 {
        self.abandon_pages as f64 / self.pages.max(1) as f64
    }

    /// Unconverted `abandon` boxes per page. A running head and a page
    /// number on every page make about 1.5; footnotes, marginal notes and
    /// dictionary-style furniture push a book well above that.
    pub fn abandon_per_page(&self) -> f64 {
        self.abandon_boxes as f64 / self.pages.max(1) as f64
    }

    pub fn large_per_page(&self) -> f64 {
        self.abandon_large as f64 / self.pages.max(1) as f64
    }
}

/// Per-book counts, in import order: one pass over the `pages_book_stats3`
/// covering index (the per-page counts are kept on `pages`), and the
/// measured paper tones.
pub fn book_stats(conn: &Connection) -> DbResult<Vec<BookStats>> {
    let n = crate::annotation::labels::NUM_CLASSES;
    let sums: String = (0..n)
        .map(|c| format!(", SUM(n{c}), SUM(n{c} > 0)"))
        .collect();
    let mut st = conn
        .prepare(&format!(
            "SELECT book_id, MAX(split), COUNT(*), \
             SUM(review_status IN ('reviewed', 'excluded')), \
             SUM(review_status = 'needs_attention'), \
             SUM(abandon), SUM(abandon > 0), SUM(abandon_large), SUM(abandon_imported), \
             SUM({TODO_SQL}), SUM(n6 > 0 OR model_fig > 0), \
             SUM({TODO_SQL} AND (n6 > 0 OR model_fig > 0)), \
             SUM({TODO_SQL} AND model_only_fig > 0){sums} \
             FROM pages INDEXED BY pages_book_stats3 GROUP BY book_id ORDER BY MIN(seq)"
        ))
        .map_err(e)?;
    let tones = book_tones(conn)?;
    let rows = st
        .query_map([], |r| {
            let mut b = BookStats {
                book_id: r.get(0)?,
                split: r.get(1)?,
                pages: r.get(2)?,
                done: r.get(3)?,
                needs_attention: r.get(4)?,
                abandon_boxes: r.get(5)?,
                abandon_pages: r.get(6)?,
                abandon_large: r.get(7)?,
                abandon_imported: r.get(8)?,
                todo: r.get(9)?,
                picture_pages: r.get(10)?,
                picture_todo: r.get(11)?,
                model_only_todo: r.get(12)?,
                ..BookStats::default()
            };
            for c in 0..n {
                b.class_boxes[c] = r.get(13 + 2 * c)?;
                b.class_pages[c] = r.get(14 + 2 * c)?;
            }
            b.class_boxes[ABANDON as usize] = b.abandon_boxes;
            b.class_pages[ABANDON as usize] = b.abandon_pages;
            Ok(b)
        })
        .map_err(e)?;
    let mut out = rows.collect::<Result<Vec<_>, _>>().map_err(e)?;
    let flags = book_flags(conn)?;
    for b in &mut out {
        b.tone = tones.get(&b.book_id).copied();
        if let Some(f) = flags.get(&b.book_id) {
            b.excluded = f.excluded;
            b.needs_work = f.needs_work;
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BookFlags {
    pub excluded: bool,
    pub needs_work: bool,
}

/// Every book with a flag set.
pub fn book_flags(conn: &Connection) -> DbResult<std::collections::HashMap<String, BookFlags>> {
    let mut st = conn
        .prepare("SELECT book_id, excluded, needs_work FROM book_flags")
        .map_err(e)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                BookFlags {
                    excluded: r.get::<_, i64>(1)? != 0,
                    needs_work: r.get::<_, i64>(2)? != 0,
                },
            ))
        })
        .map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

pub fn get_book_flags(conn: &Connection, book: &str) -> DbResult<BookFlags> {
    conn.query_row(
        "SELECT excluded, needs_work FROM book_flags WHERE book_id = ?1",
        params![book],
        |r| {
            Ok(BookFlags {
                excluded: r.get::<_, i64>(0)? != 0,
                needs_work: r.get::<_, i64>(1)? != 0,
            })
        },
    )
    .optional()
    .map(Option::unwrap_or_default)
    .map_err(e)
}

pub fn set_book_flags(conn: &Connection, book: &str, f: BookFlags) -> DbResult<()> {
    conn.execute(
        "INSERT INTO book_flags (book_id, excluded, needs_work, updated_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(book_id) DO UPDATE SET excluded = ?2, needs_work = ?3, updated_at = ?4",
        params![book, f.excluded as i64, f.needs_work as i64, now_unix()],
    )
    .map(|_| ())
    .map_err(e)
}

/// Put every page of `book` in `split`. Returns the pages moved.
pub fn set_book_split(conn: &Connection, book: &str, split: &str) -> DbResult<usize> {
    if split.trim().is_empty() {
        return Err("a split needs a name".into());
    }
    conn.execute(
        "UPDATE pages SET split = ?2 WHERE book_id = ?1 AND split <> ?2",
        params![book, split],
    )
    .map_err(e)
}

/// The split names in use, most pages first.
pub fn split_names(conn: &Connection) -> DbResult<Vec<String>> {
    let mut st = conn
        .prepare("SELECT split FROM pages GROUP BY split ORDER BY COUNT(*) DESC")
        .map_err(e)?;
    let rows = st.query_map([], |r| r.get(0)).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

/// A stable number in 0..1 for a book id (FNV-1a, mixed), so the same books land
/// in the same split on every machine and every run.
pub fn book_fraction(book: &str) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in book.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    // FNV's high bits barely change between similar names (book-1,
    // book-2, …): a SplitMix64 finish spreads them.
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// How [`assign_splits`] divides books.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitPlan {
    /// Books in this split are reassigned (`new`, by default).
    pub from: String,
    pub dev: (String, f64),
    pub test: (String, f64),
    pub train: String,
}

impl Default for SplitPlan {
    fn default() -> SplitPlan {
        SplitPlan {
            from: "new".into(),
            dev: ("dev".into(), 0.10),
            test: ("final_test".into(), 0.10),
            train: "train".into(),
        }
    }
}

/// Give every book of split `plan.from` a split by a stable hash of its id
/// (whole books, so no book straddles two splits). Returns (book, split)
/// for each book; with `dry_run` nothing is written.
pub fn assign_splits(
    conn: &mut Connection,
    plan: &SplitPlan,
    dry_run: bool,
) -> DbResult<Vec<(String, String)>> {
    let books: Vec<String> = {
        let mut st = conn
            .prepare("SELECT DISTINCT book_id FROM pages WHERE split = ?1 ORDER BY book_id")
            .map_err(e)?;
        let rows = st.query_map(params![plan.from], |r| r.get(0)).map_err(e)?;
        rows.collect::<Result<_, _>>().map_err(e)?
    };
    let out: Vec<(String, String)> = books
        .into_iter()
        .map(|b| {
            let f = book_fraction(&b);
            let s = if f < plan.dev.1 {
                plan.dev.0.clone()
            } else if f < plan.dev.1 + plan.test.1 {
                plan.test.0.clone()
            } else {
                plan.train.clone()
            };
            (b, s)
        })
        .collect();
    if !dry_run {
        let tx = conn.transaction().map_err(e)?;
        {
            let mut st = tx
                .prepare("UPDATE pages SET split = ?2 WHERE book_id = ?1")
                .map_err(e)?;
            for (b, s) in &out {
                st.execute(params![b, s]).map_err(e)?;
            }
        }
        tx.commit().map_err(e)?;
    }
    Ok(out)
}

/// The status a page had before it was excluded or flagged: the last saved
/// status other than those two, from the page's event log.
pub fn status_before_flag(conn: &Connection, key: &str) -> DbResult<Option<ReviewStatus>> {
    let mut st = conn
        .prepare_cached("SELECT payload FROM events WHERE page_key = ?1 ORDER BY id DESC")
        .map_err(e)?;
    let rows = st
        .query_map(params![key], |r| r.get::<_, Option<String>>(0))
        .map_err(e)?;
    for p in rows {
        let Some(p) = p.map_err(e)? else { continue };
        let status = serde_json::from_str::<serde_json::Value>(&p)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_owned))
            .and_then(|s| ReviewStatus::parse(&s));
        if let Some(s) = status {
            if !matches!(s, ReviewStatus::Excluded | ReviewStatus::NeedsAttention) {
                return Ok(Some(s));
            }
        }
    }
    Ok(None)
}

/// Books left out of training.
pub fn excluded_books(conn: &Connection) -> DbResult<std::collections::HashSet<String>> {
    Ok(book_flags(conn)?
        .into_iter()
        .filter(|(_, f)| f.excluded)
        .map(|(b, _)| b)
        .collect())
}

// --- boxes that repeat on every page -------------------------------------------

/// How closely a box must match the selected one in position and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tolerance {
    Tight,
    Normal,
    Loose,
}

impl Tolerance {
    pub fn label(self) -> &'static str {
        match self {
            Tolerance::Tight => "tight",
            Tolerance::Normal => "normal",
            Tolerance::Loose => "loose",
        }
    }

    /// (vertical centre within this many box heights, height ratio, width ratio)
    fn limits(self) -> (f64, f64, f64) {
        match self {
            Tolerance::Tight => (0.5, 1.3, 2.0),
            Tolerance::Normal => (1.0, 1.6, 3.0),
            Tolerance::Loose => (2.0, 2.2, 5.0),
        }
    }
}

/// The classes a running head or page number gets confused with: heading,
/// running head, running foot, page number, and `abandon`.
pub const FURNITURE_FAMILY: [u8; 5] = [1, 2, 3, 4, ABANDON];

/// A box elsewhere in the book that sits where `template` sits.
#[derive(Clone, Debug, PartialEq)]
pub struct SimilarBox {
    pub id: String,
    pub page_key: String,
    pub class_id: u8,
}

/// Whether `b` on a `w`×`h` page matches `t` on a `tw`×`th` page: the same
/// vertical position (relative to the page) and a similar height, and a width
/// in the same range. Horizontal position is free: running heads alternate
/// sides on facing pages, and their titles differ in length.
pub fn is_similar(t: &Box2, tw: f64, th: f64, b: &Box2, w: f64, h: f64, tol: Tolerance) -> bool {
    let (ky, rh, rw) = tol.limits();
    let (tcy, bcy) = ((t.y0 + t.y1) / 2.0 / th, (b.y0 + b.y1) / 2.0 / h);
    let (tht, bh) = (t.height() / th, b.height() / h);
    let (twd, bw) = (t.width() / tw, b.width() / w);
    if tht <= 0.0 || twd <= 0.0 || bh <= 0.0 || bw <= 0.0 {
        return false;
    }
    let ratio = |a: f64, b: f64| if a > b { a / b } else { b / a };
    (tcy - bcy).abs() <= ky * tht.max(0.01) && ratio(tht, bh) <= rh && ratio(twd, bw) <= rw
}

/// Boxes in `book` of `classes` that match `template` (on a `tw`×`th` page).
pub fn similar_boxes(
    conn: &Connection,
    book: &str,
    template: &Box2,
    tw: f64,
    th: f64,
    classes: &[u8],
    tol: Tolerance,
) -> DbResult<Vec<SimilarBox>> {
    let list = classes
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mut st = conn
        .prepare(&format!(
            "SELECT a.id, a.page_key, a.class_id, a.x0, a.y0, a.x1, a.y1, p.width, p.height \
             FROM pages p INDEXED BY pages_by_book JOIN annotations a ON a.page_key = p.page_key \
             WHERE p.book_id = ?1 AND a.class_id IN ({list}) AND a.is_ignore = 0 ORDER BY p.seq"
        ))
        .map_err(e)?;
    let rows = st
        .query_map(params![book], |r| {
            Ok((
                SimilarBox {
                    id: r.get(0)?,
                    page_key: r.get(1)?,
                    class_id: r.get::<_, i64>(2)? as u8,
                },
                Box2::new(r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?),
                r.get::<_, f64>(7)?,
                r.get::<_, f64>(8)?,
            ))
        })
        .map_err(e)?;
    let mut out = Vec::new();
    for row in rows {
        let (s, b, w, h) = row.map_err(e)?;
        if is_similar(template, tw, th, &b, w, h, tol) {
            out.push(s);
        }
    }
    Ok(out)
}

/// Every measured paper tone, by book.
pub fn book_tones(conn: &Connection) -> DbResult<std::collections::HashMap<String, Tone>> {
    let mut st = conn
        .prepare("SELECT book_id, r, g, b, lightness, yellowness, pages FROM book_tone")
        .map_err(e)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Tone {
                    rgb: [r.get(1)?, r.get(2)?, r.get(3)?],
                    lightness: r.get(4)?,
                    yellowness: r.get(5)?,
                    pages: r.get(6)?,
                },
            ))
        })
        .map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

pub fn set_book_tone(conn: &Connection, book: &str, t: &Tone) -> DbResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO book_tone (book_id, r, g, b, lightness, yellowness, pages, measured_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            book,
            t.rgb[0],
            t.rgb[1],
            t.rgb[2],
            t.lightness,
            t.yellowness,
            t.pages,
            now_unix()
        ],
    )
    .map(|_| ())
    .map_err(e)
}

/// Books with no measured tone, each with up to `per_book` page images
/// spread over the middle of the book (covers and end papers left out).
pub fn tone_samples(conn: &Connection, per_book: usize) -> DbResult<Vec<(String, Vec<PathBuf>)>> {
    let mut st = conn
        .prepare(
            "SELECT book_id, source_path FROM pages INDEXED BY pages_by_book \
             WHERE book_id NOT IN (SELECT book_id FROM book_tone) ORDER BY book_id, seq",
        )
        .map_err(e)?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(e)?;
    let mut out: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    let flush = |c: Option<(String, Vec<String>)>, out: &mut Vec<(String, Vec<PathBuf>)>| {
        if let Some((book, paths)) = c {
            let n = paths.len();
            let k = per_book.min(n).max(1);
            let picks: Vec<PathBuf> = (0..k)
                .map(|i| {
                    // Evenly over 15%..85% of the book.
                    let f = if k == 1 {
                        0.5
                    } else {
                        0.15 + 0.7 * i as f64 / (k - 1) as f64
                    };
                    PathBuf::from(&paths[((n - 1) as f64 * f).round() as usize])
                })
                .collect();
            out.push((book, picks));
        }
    };
    for row in rows {
        let (book, path) = row.map_err(e)?;
        match &mut cur {
            Some((b, v)) if *b == book => v.push(path),
            _ => {
                flush(cur.take(), &mut out);
                cur = Some((book, vec![path]));
            }
        }
    }
    flush(cur.take(), &mut out);
    Ok(out)
}

// --- bulk class conversion ---------------------------------------------------

/// Where on the page a box sits. Narrows a bulk conversion to boxes that are
/// all the same thing: YOLO files running heads, page numbers, footnotes and
/// marginal notes under the one `abandon` class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    All,
    /// Centre in the top 15% of the page (and not large).
    Top,
    /// Centre in the bottom 15% of the page (and not large).
    Bottom,
    /// Under a quarter of the page wide, centred in the outer fifth on
    /// either side, and in neither band.
    Margins,
    /// At least 4% of the page area: mostly footnote blocks.
    Large,
    /// None of the above.
    Other,
}

impl Region {
    pub const ALL: [Region; 6] = [
        Region::All,
        Region::Top,
        Region::Bottom,
        Region::Margins,
        Region::Large,
        Region::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Region::All => "everywhere",
            Region::Top => "top band",
            Region::Bottom => "bottom band",
            Region::Margins => "side margins",
            Region::Large => "large (4%+ of page)",
            Region::Other => "elsewhere",
        }
    }

    /// Whether a box on a `w`×`h` page is in this region; the same rule as
    /// [`Region::sql`].
    pub fn contains(self, b: &Box2, w: f64, h: f64) -> bool {
        let top = b.y0 + b.y1 < 0.30 * h;
        let bottom = b.y0 + b.y1 > 1.70 * h;
        let margins = b.x1 - b.x0 < 0.25 * w && (b.x0 + b.x1 < 0.40 * w || b.x0 + b.x1 > 1.60 * w);
        let large = (b.x1 - b.x0) * (b.y1 - b.y0) >= 0.04 * w * h;
        match self {
            Region::All => true,
            Region::Top => top && !large,
            Region::Bottom => bottom && !large,
            Region::Margins => margins && !top && !bottom && !large,
            Region::Large => large,
            Region::Other => !(top || bottom || margins || large),
        }
    }

    /// The rule over `a` (annotation) and `p` (page).
    fn sql(self) -> String {
        const TOP: &str = "(a.y0 + a.y1 < 0.30 * p.height)";
        const BOTTOM: &str = "(a.y0 + a.y1 > 1.70 * p.height)";
        const MARGINS: &str = "(a.x1 - a.x0 < 0.25 * p.width AND (a.x0 + a.x1 < 0.40 * p.width OR a.x0 + a.x1 > 1.60 * p.width))";
        const LARGE: &str = "((a.x1 - a.x0) * (a.y1 - a.y0) >= 0.04 * p.width * p.height)";
        match self {
            Region::All => "1".into(),
            Region::Top => format!("({TOP} AND NOT {LARGE})"),
            Region::Bottom => format!("({BOTTOM} AND NOT {LARGE})"),
            Region::Margins => {
                format!("({MARGINS} AND NOT {TOP} AND NOT {BOTTOM} AND NOT {LARGE})")
            }
            Region::Large => LARGE.into(),
            Region::Other => format!("NOT ({TOP} OR {BOTTOM} OR {MARGINS} OR {LARGE})"),
        }
    }
}

/// The pages a bulk operation may touch: the queue's filter without its
/// status and class parts (a conversion is about boxes, whatever the page's
/// review status).
fn scope_sql(filter: &PageFilter) -> (String, Vec<String>) {
    let mut f = filter.clone();
    f.statuses.clear();
    f.by_priority = false;
    f.has_class = None;
    f.where_clause()
}

/// Boxes of class `from` within the scope: `(region, boxes, pages)` for each
/// region, `Region::All` first. One pass over the class's boxes.
pub fn count_by_region(
    conn: &Connection,
    filter: &PageFilter,
    from: u8,
) -> DbResult<Vec<(Region, i64, i64)>> {
    let (w, args) = scope_sql(filter);
    let cols: Vec<String> = Region::ALL
        .iter()
        .map(|r| {
            let c = r.sql();
            format!("SUM({c}), COUNT(DISTINCT CASE WHEN {c} THEN a.page_key END)")
        })
        .collect();
    let index = if from == crate::annotation::labels::ABANDON {
        "INDEXED BY annotations_abandon"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {} FROM annotations a {index} JOIN pages p ON p.page_key = a.page_key \
         WHERE a.class_id = {from} AND a.page_key IN (SELECT page_key FROM pages {w})",
        cols.join(", ")
    );
    conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
        let mut out = Vec::new();
        for (i, r) in Region::ALL.iter().enumerate() {
            let n: Option<i64> = row.get(2 * i)?;
            out.push((*r, n.unwrap_or(0), row.get(2 * i + 1)?));
        }
        Ok(out)
    })
    .map_err(e)
}

#[derive(Clone, Debug, PartialEq)]
pub struct BulkResult {
    pub op_id: i64,
    pub boxes: i64,
    pub pages: i64,
}

/// Convert every `from` box in the scope and region to `to` in one
/// transaction, logged in `bulk_ops` so [`undo_bulk`] can restore it. Each
/// page touched gets a new revision and an event. The caller flushes its
/// writer first and reloads any page it holds in memory afterwards.
pub fn convert_class(
    conn: &mut Connection,
    filter: &PageFilter,
    region: Region,
    from: u8,
    to: u8,
) -> DbResult<BulkResult> {
    let (w, args) = scope_sql(filter);
    let tx = conn.transaction().map_err(e)?;
    let now = now_unix();
    let desc = format!(
        "{} to {} · {} · {}",
        crate::annotation::labels::class(from).name,
        crate::annotation::labels::class(to).name,
        region.label(),
        filter.describe()
    );
    tx.execute(
        "INSERT INTO bulk_ops (at, description, boxes, pages) VALUES (?1, ?2, 0, 0)",
        params![now, desc],
    )
    .map_err(e)?;
    let op_id = tx.last_insert_rowid();
    let boxes = tx
        .execute(
            &format!(
                "INSERT INTO bulk_op_items (op_id, annotation_id, page_key, from_class, to_class) \
                 SELECT {op_id}, a.id, a.page_key, a.class_id, {to} FROM annotations a \
                 JOIN pages p ON p.page_key = a.page_key \
                 WHERE a.class_id = {from} AND {} AND a.page_key IN (SELECT page_key FROM pages {w})",
                region.sql()
            ),
            rusqlite::params_from_iter(args.iter()),
        )
        .map_err(e)? as i64;
    tx.execute(
        "UPDATE annotations SET class_id = ?2 \
         WHERE id IN (SELECT annotation_id FROM bulk_op_items WHERE op_id = ?1)",
        params![op_id, to as i64],
    )
    .map_err(e)?;
    let pages = touch_pages(&tx, op_id, now, "bulk-convert")?;
    tx.execute(
        "UPDATE bulk_ops SET boxes = ?2, pages = ?3 WHERE id = ?1",
        params![op_id, boxes, pages],
    )
    .map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(BulkResult {
        op_id,
        boxes,
        pages,
    })
}

/// One box's class change in a bulk relabel.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassChange {
    pub id: String,
    pub page_key: String,
    pub from: u8,
    pub to: u8,
}

/// Apply class changes as one logged bulk change (undoable with
/// [`undo_bulk`]); a box whose class is no longer `from` is skipped.
pub fn apply_class_changes(
    conn: &mut Connection,
    description: &str,
    changes: &[ClassChange],
) -> DbResult<BulkResult> {
    let tx = conn.transaction().map_err(e)?;
    let now = now_unix();
    tx.execute(
        "INSERT INTO bulk_ops (at, description, boxes, pages) VALUES (?1, ?2, 0, 0)",
        params![now, description],
    )
    .map_err(e)?;
    let op_id = tx.last_insert_rowid();
    let mut boxes = 0i64;
    {
        let mut up = tx
            .prepare("UPDATE annotations SET class_id = ?2 WHERE id = ?1 AND class_id = ?3")
            .map_err(e)?;
        let mut item = tx
            .prepare(
                "INSERT INTO bulk_op_items (op_id, annotation_id, page_key, from_class, to_class) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(e)?;
        for c in changes.iter().filter(|c| c.from != c.to) {
            if up
                .execute(params![c.id, c.to as i64, c.from as i64])
                .map_err(e)?
                == 1
            {
                item.execute(params![op_id, c.id, c.page_key, c.from as i64, c.to as i64])
                    .map_err(e)?;
                boxes += 1;
            }
        }
    }
    let pages = touch_pages(&tx, op_id, now, "bulk-convert")?;
    tx.execute(
        "UPDATE bulk_ops SET boxes = ?2, pages = ?3 WHERE id = ?1",
        params![op_id, boxes, pages],
    )
    .map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(BulkResult {
        op_id,
        boxes,
        pages,
    })
}

/// New revision and an event for every page in a bulk operation.
fn touch_pages(tx: &rusqlite::Transaction<'_>, op_id: i64, now: i64, kind: &str) -> DbResult<i64> {
    tx.execute(
        &format!(
            "UPDATE pages SET revision = revision + 1, updated_at = ?2, {} \
             WHERE page_key IN (SELECT DISTINCT page_key FROM bulk_op_items WHERE op_id = ?1)",
            page_counts_set()
        ),
        params![op_id, now],
    )
    .map_err(e)?;
    let payload = format!("{{\"bulk_op\":{op_id}}}");
    tx.execute(
        "INSERT INTO events (page_key, at, kind, revision, payload) \
         SELECT page_key, ?2, ?3, revision, ?4 FROM pages \
         WHERE page_key IN (SELECT DISTINCT page_key FROM bulk_op_items WHERE op_id = ?1)",
        params![op_id, now, kind, payload],
    )
    .map_err(e)
    .map(|n| n as i64)
}

/// The most recent bulk conversion not yet undone: `(id, description, boxes)`.
pub fn last_bulk_op(conn: &Connection) -> DbResult<Option<(i64, String, i64)>> {
    conn.query_row(
        "SELECT id, description, boxes FROM bulk_ops WHERE undone = 0 ORDER BY id DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
    .map_err(e)
}

/// Undo a bulk conversion. Each box it converted that still has the class it
/// was given goes back to its old class; a box reclassified since keeps the
/// later edit, and a deleted box stays deleted. Returns the boxes restored.
pub fn undo_bulk(conn: &mut Connection, op_id: i64) -> DbResult<i64> {
    let tx = conn.transaction().map_err(e)?;
    let n = tx
        .execute(
            "UPDATE annotations SET class_id = \
                 (SELECT i.from_class FROM bulk_op_items i \
                  WHERE i.op_id = ?1 AND i.annotation_id = annotations.id) \
             WHERE id IN (SELECT i.annotation_id FROM bulk_op_items i \
                          WHERE i.op_id = ?1 AND i.to_class = annotations.class_id)",
            params![op_id],
        )
        .map_err(e)? as i64;
    touch_pages(&tx, op_id, now_unix(), "bulk-undo")?;
    tx.execute(
        "UPDATE bulk_ops SET undone = 1 WHERE id = ?1",
        params![op_id],
    )
    .map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(n)
}

// --- writer thread ---------------------------------------------------------

pub enum WriteRequest {
    Save(PageSave),
    /// Reply once everything before this request is committed.
    Flush(Sender<()>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum WriteAck {
    Saved {
        page_key: String,
        revision: u64,
    },
    Failed {
        page_key: String,
        revision: u64,
        error: String,
    },
}

/// The dedicated writer: commits page snapshots in order, coalescing
/// queued snapshots of the same page (only the newest is written), and
/// acknowledges each committed revision.
pub struct Writer {
    tx: Option<Sender<WriteRequest>>,
    pub acks: Receiver<WriteAck>,
    handle: Option<JoinHandle<()>>,
}

impl Writer {
    pub fn spawn(path: &Path, wake: impl Fn() + Send + 'static) -> DbResult<Writer> {
        let mut conn = open(path)?;
        let (tx, rx) = channel::<WriteRequest>();
        let (ack_tx, acks) = channel::<WriteAck>();
        let handle = std::thread::Builder::new()
            .name("annotator-writer".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    // Gather everything already queued, newest snapshot per page wins,
                    // order of first appearance kept.
                    let mut batch: Vec<WriteRequest> = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let mut saves: Vec<PageSave> = Vec::new();
                    let mut flushes: Vec<Sender<()>> = Vec::new();
                    for req in batch {
                        match req {
                            WriteRequest::Save(s) => {
                                if let Some(existing) =
                                    saves.iter_mut().find(|x| x.page_key == s.page_key)
                                {
                                    if s.revision >= existing.revision {
                                        *existing = s;
                                    }
                                } else {
                                    saves.push(s);
                                }
                            }
                            WriteRequest::Flush(reply) => flushes.push(reply),
                        }
                    }
                    for s in saves {
                        let ack = match save_page(&mut conn, &s) {
                            Ok(()) => WriteAck::Saved {
                                page_key: s.page_key,
                                revision: s.revision,
                            },
                            Err(error) => WriteAck::Failed {
                                page_key: s.page_key,
                                revision: s.revision,
                                error,
                            },
                        };
                        let _ = ack_tx.send(ack);
                        wake();
                    }
                    for f in flushes {
                        let _ = f.send(());
                    }
                }
                // Channel closed: checkpoint so the WAL does not grow unbounded.
                let _ = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
            })
            .map_err(|err| err.to_string())?;
        Ok(Writer {
            tx: Some(tx),
            acks,
            handle: Some(handle),
        })
    }

    pub fn save(&self, save: PageSave) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(WriteRequest::Save(save));
        }
    }

    /// Block until every save sent so far is committed.
    pub fn flush(&self) {
        if let Some(tx) = &self.tx {
            let (reply, wait) = channel();
            if tx.send(WriteRequest::Flush(reply)).is_ok() {
                let _ = wait.recv();
            }
        }
    }

    /// Commit everything and stop the thread.
    pub fn shutdown(&mut self) {
        self.tx.take();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
