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
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::annotation::commands::{Annotation, ReviewStatus};
use crate::annotation::geometry::Box2;

pub const SCHEMA_VERSION: i64 = 1;

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
    updated_at INTEGER
);

CREATE TABLE IF NOT EXISTS annotations (
    id TEXT PRIMARY KEY,
    page_key TEXT NOT NULL REFERENCES pages(page_key),
    ord INTEGER NOT NULL,
    class_id INTEGER NOT NULL CHECK(class_id BETWEEN 0 AND 13),
    x0 REAL NOT NULL,
    y0 REAL NOT NULL,
    x1 REAL NOT NULL,
    y1 REAL NOT NULL,
    is_ignore INTEGER NOT NULL DEFAULT 0,
    provenance_json TEXT,
    CHECK(x1 > x0 AND y1 > y0)
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

CREATE INDEX IF NOT EXISTS annotations_by_page ON annotations(page_key);
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
    conn.execute_batch(SCHEMA).map_err(e)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )
    .map_err(e)?;
    Ok(conn)
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

const PAGE_COLS: &str =
    "page_key, seq, book_id, split, set_name, source_path, file_name, width, height, \
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
        tx.prepare_cached(
            "UPDATE pages SET review_status = ?2, revision = revision + 1, updated_at = ?3 WHERE page_key = ?1",
        )
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
}

impl PageFilter {
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
