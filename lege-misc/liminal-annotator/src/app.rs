//! The annotator window: a [`PixelApp`] that ties the canvas, the editor
//! state machine, the project database, the writer and the image loader
//! together.
//!
//! Frame structure: drain the ordered pointer queue (routing through
//! capture), refit the view if needed, re-render the page raster only if the
//! page/zoom/pan/canvas changed, copy it into the window buffer, then draw
//! annotation outlines and the HUD on top.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use liminalkit_raster::{Painter, Rect, WindowBuffer};
use liminalkit_shell::{Modifiers, MouseButton, Scale};
use liminalkit_text::{Align, TextCache, TextStyle};
use liminalkit_windowing::{CursorShape, KeyEvent, PixelApp, ScrollEvent, Waker};
use rusqlite::Connection;

use crate::annotation::commands::{Annotation, Edit, PageDoc, ReviewStatus};
use crate::annotation::geometry::{Box2, Handle, Point};
use crate::annotation::labels::{IGNORE_COLOR, NUM_CLASSES, class};
use crate::canvas::draw::{
    CANVAS_BG, PageRaster, RasterKey, blit_raster, dashed_outline, outline, pixel_rect,
    render_page, scroll_page, text_on,
};
use crate::canvas::interaction::{Editor, Outcome, Params, ToolState};
use crate::canvas::pointer::{PointerCapture, PointerEvent, PointerPhase, PointerQueue};
use crate::canvas::shortcuts::{Action, map_key};
use crate::canvas::viewport::{CanvasRect, ViewTransform, ZoomMode};
use crate::dataset::database::{
    self, PageFilter, PageMeta, PageSave, Prediction, WriteAck, Writer,
};
use crate::fonts;
use crate::image_io::cache::ImageCache;
use crate::image_io::decode::PageImage;
use crate::image_io::prefetch::{Job, Loader};
use crate::review::comparison::{Change, PageDiff, diff};
use crate::settings::{Resolved, Settings};

const SURFACE_CANVAS: u64 = 1;
const SURFACE_TOOLBAR: u64 = 2;
const SURFACE_POPUP: u64 = 3;

/// How many recently visited pages keep their in-memory document (and undo
/// history) after their last save was acknowledged.
const RECENT_DOCS: usize = 64;

const HUD_BG: u32 = 0x0017_191c;
const HUD_EDGE: u32 = 0x002e_3238;
const HUD_TEXT: u32 = 0x00e6_e8eb;
const HUD_DIM: u32 = 0x0098_9ea6;
const ACCENT: u32 = 0x004d_9bff;
const DIFF_MODIFIED: u32 = 0x00ff_a040;
const DIFF_DELETED: u32 = 0x00ff_4a4a;
const DIFF_ADDED: u32 = 0x0040_e070;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub project: PathBuf,
    pub filter: PageFilter,
    pub start_at: Option<String>,
    /// Prediction score shown in prediction overlays.
    pub prediction_threshold: f64,
}

#[derive(Clone)]
pub enum ImageState {
    None,
    Loading,
    Ready(Arc<PageImage>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RevertTo {
    Opened,
    Imported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    Off,
    Imported,
    Prediction(usize),
    Diff,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ToolbarItem {
    Class(u8),
    More,
    Compare,
    Revert,
}

/// Rolling timings for the F12 overlay and `LIMINAL_PERF=1` logging.
#[derive(Default)]
pub struct Perf {
    pub frame_ms: VecDeque<f64>,
    pub raster_ms: VecDeque<f64>,
    pub switch_ms: VecDeque<f64>,
    pub decode_ms: VecDeque<f64>,
    pub frames: u64,
    pub raster_renders: u64,
    /// Of `raster_renders`, those served by shifting the cached raster.
    pub raster_scrolls: u64,
    /// Host repaints that drew nothing because nothing visible changed.
    pub skipped: u64,
}

fn push_ring(q: &mut VecDeque<f64>, v: f64) {
    if q.len() >= 600 {
        q.pop_front();
    }
    q.push_back(v);
}

pub fn percentile(q: &VecDeque<f64>, p: f64) -> f64 {
    if q.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f64> = q.iter().copied().collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

type WakeSlot = Arc<Mutex<Option<Waker>>>;

pub struct AnnotatorApp {
    pub project_path: PathBuf,
    conn: Connection,
    writer: Writer,
    loader: Loader,
    cache: ImageCache,
    wake_slot: WakeSlot,
    pub settings: Resolved,
    settings_warnings: Vec<String>,

    // queue
    pub filter: PageFilter,
    /// Statuses the command line asked for; `Q` cycles over this.
    base_statuses: Vec<ReviewStatus>,
    queue_mode: usize,
    pub keys: Vec<String>,
    pub pos: usize,
    reviewed_in_queue: usize,

    // current page
    pub meta: Option<PageMeta>,
    pub doc: Option<PageDoc>,
    imported: Vec<Annotation>,
    /// The boxes as they were when the page was opened on this visit: the
    /// first target of a page revert.
    opened_snapshot: Vec<Annotation>,
    predictions: Vec<Prediction>,
    page_diff: Option<PageDiff>,
    pub image: ImageState,
    generation: u64,
    nav_started: Option<Instant>,

    // persistence bookkeeping
    docs: HashMap<String, PageDoc>,
    recent: VecDeque<String>,
    sent: HashMap<String, u64>,
    acked: HashMap<String, u64>,
    save_error: Option<String>,

    // view and input
    pub view: ViewTransform,
    pub canvas: CanvasRect,
    pub editor: Editor,
    pointer: PointerQueue,
    capture: PointerCapture,
    pressed_toolbar: Option<ToolbarItem>,
    toolbar_hits: Vec<(Rect, ToolbarItem)>,
    popup_hits: Vec<(Rect, u8)>,
    popup_rect: Option<Rect>,
    scale: Scale,
    raster: PageRaster,

    // ui state
    text: TextCache,
    pub hud: bool,
    help: bool,
    palette: bool,
    perf_overlay: bool,
    pub compare: Compare,
    models: Vec<String>,
    prediction_threshold: f64,
    message: Option<(String, Instant, bool)>,
    title: Option<String>,
    fullscreen: Option<bool>,
    is_fullscreen: bool,
    exit: bool,
    pub perf: Perf,
    perf_log: bool,
    render_threads: usize,
    /// Something visible changed since the last frame. The host repaints on
    /// every cursor move; frames that would draw the same pixels are skipped
    /// (the window buffer keeps the previous frame).
    dirty: bool,
    last_size: (u32, u32),
}

impl AnnotatorApp {
    pub fn new(config: AppConfig) -> Result<AnnotatorApp, String> {
        let conn = database::open(&config.project)?;
        let (raw, warn) = Settings::load_or_create(&Settings::path_for(&config.project));
        let mut warnings: Vec<String> = warn.into_iter().collect();
        let (settings, w2) = raw.resolve();
        warnings.extend(w2);
        let wake_slot: WakeSlot = Arc::new(Mutex::new(None));
        let ws = wake_slot.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Ok(w) = ws.lock() {
                if let Some(w) = w.as_ref() {
                    w.wake();
                }
            }
        });
        let w2 = wake.clone();
        let writer = Writer::spawn(&config.project, move || w2())?;
        let loader = Loader::spawn(settings.raw.decode_threads, wake);
        let cache = ImageCache::new(settings.raw.cache_mb.max(32) * 1024 * 1024);
        let keys = database::page_keys(&conn, &config.filter)?;
        let models = database::models(&conn).unwrap_or_default();
        let params = Params::for_dpi(1.0, settings.raw.hit_slop);
        let mut app = AnnotatorApp {
            project_path: config.project.clone(),
            conn,
            writer,
            loader,
            cache,
            wake_slot,
            settings,
            settings_warnings: warnings,
            base_statuses: config.filter.statuses.clone(),
            queue_mode: 0,
            filter: config.filter,
            keys,
            pos: 0,
            reviewed_in_queue: 0,
            meta: None,
            doc: None,
            imported: Vec::new(),
            opened_snapshot: Vec::new(),
            predictions: Vec::new(),
            page_diff: None,
            image: ImageState::None,
            generation: 0,
            nav_started: None,
            docs: HashMap::new(),
            recent: VecDeque::new(),
            sent: HashMap::new(),
            acked: HashMap::new(),
            save_error: None,
            view: ViewTransform::default(),
            canvas: CanvasRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            editor: Editor::new(params),
            pointer: PointerQueue::new(),
            capture: PointerCapture::default(),
            pressed_toolbar: None,
            toolbar_hits: Vec::new(),
            popup_hits: Vec::new(),
            popup_rect: None,
            scale: Scale::ONE,
            raster: PageRaster::default(),
            text: TextCache::new(fonts::font_set()),
            hud: true,
            help: false,
            palette: false,
            perf_overlay: false,
            compare: Compare::Off,
            models,
            prediction_threshold: config.prediction_threshold,
            message: None,
            title: None,
            fullscreen: None,
            is_fullscreen: false,
            exit: false,
            perf: Perf::default(),
            perf_log: std::env::var_os("LIMINAL_PERF").is_some_and(|v| v != "0"),
            render_threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(8),
            dirty: true,
            last_size: (0, 0),
        };
        app.editor.active_class = app.settings.digit[0].unwrap_or(0);
        app.refresh_counts();
        let start = config
            .start_at
            .as_ref()
            .and_then(|k| app.keys.iter().position(|x| x == k))
            .or_else(|| app.first_unreviewed())
            .unwrap_or(0);
        if !app.keys.is_empty() {
            app.open_page(start);
        }
        if let Some(w) = app.settings_warnings.first().cloned() {
            app.flash(&w, true);
        }
        Ok(app)
    }

    // --- queue -------------------------------------------------------------

    fn refresh_counts(&mut self) {
        let counts = database::status_counts(&self.conn, &self.filter).unwrap_or_default();
        self.reviewed_in_queue = counts
            .iter()
            .filter(|(s, _)| matches!(s, ReviewStatus::Reviewed | ReviewStatus::Excluded))
            .map(|(_, n)| *n as usize)
            .sum();
    }

    /// First page in queue order still to do. One indexed query, not one
    /// query per page (a 100k-page queue would otherwise cost seconds).
    fn first_unreviewed(&self) -> Option<usize> {
        let mut todo = self.filter.clone();
        todo.statuses = vec![ReviewStatus::Unseen, ReviewStatus::Opened];
        todo.by_priority = false;
        let keys = database::page_keys(&self.conn, &todo).ok()?;
        let todo: std::collections::HashSet<String> = keys.into_iter().collect();
        self.keys.iter().position(|k| todo.contains(k))
    }

    fn flash(&mut self, msg: &str, error: bool) {
        self.message = Some((msg.to_owned(), Instant::now(), error));
        self.dirty = true;
    }

    // --- page lifecycle ----------------------------------------------------

    fn update_title(&mut self) {
        let t = match &self.meta {
            Some(m) => format!(
                "Liminal Annotator — {} {}/{} — {}",
                self.filter.describe(),
                self.pos + 1,
                self.keys.len(),
                m.page_key
            ),
            None => "Liminal Annotator".to_owned(),
        };
        self.title = Some(t);
    }

    /// Send the current document to the writer if it changed since the last
    /// send. Never blocks.
    fn commit(&mut self, kind: &'static str) {
        let Some(doc) = &self.doc else { return };
        let sent = self.sent.get(&doc.page_key).copied();
        if sent == Some(doc.revision) {
            return;
        }
        self.writer.save(PageSave {
            page_key: doc.page_key.clone(),
            status: doc.status,
            annotations: doc.annotations.clone(),
            revision: doc.revision,
            kind,
        });
        self.sent.insert(doc.page_key.clone(), doc.revision);
        self.page_diff = Some(diff(&doc.annotations, &self.imported));
    }

    fn is_pending(&self, key: &str) -> bool {
        match (self.sent.get(key), self.acked.get(key)) {
            (Some(s), Some(a)) => s > a,
            (Some(_), None) => true,
            _ => false,
        }
    }

    pub fn unsaved(&self) -> usize {
        self.sent.keys().filter(|k| self.is_pending(k)).count()
    }

    fn stash_current(&mut self) {
        if let Some(doc) = self.doc.take() {
            let key = doc.page_key.clone();
            self.docs.insert(key.clone(), doc);
            self.recent.retain(|k| k != &key);
            self.recent.push_back(key);
            self.trim_docs();
        }
    }

    /// Forget in-memory documents beyond the recent window, but never one
    /// whose last save is not yet acknowledged.
    fn trim_docs(&mut self) {
        while self.recent.len() > RECENT_DOCS {
            let Some(pos) = self.recent.iter().position(|k| !self.is_pending(k)) else {
                break;
            };
            if let Some(k) = self.recent.remove(pos) {
                self.docs.remove(&k);
            }
        }
    }

    fn open_page(&mut self, pos: usize) {
        if self.keys.is_empty() {
            return;
        }
        self.stash_current();
        self.dirty = true;
        let pos = pos.min(self.keys.len() - 1);
        self.pos = pos;
        let key = self.keys[pos].clone();
        self.generation += 1;
        self.nav_started = Some(Instant::now());
        self.editor.reset_for_page();
        self.editor.zoom_mode = ZoomMode::Fit;
        self.raster.key = None;
        let meta = match database::page_meta(&self.conn, &key) {
            Ok(Some(m)) => m,
            Ok(None) => {
                self.flash(&format!("{key}: no longer in the project"), true);
                return;
            }
            Err(e) => {
                self.flash(&e, true);
                return;
            }
        };
        let doc = match self.docs.remove(&key) {
            Some(d) => d,
            None => {
                let anns = database::annotations(&self.conn, &key).unwrap_or_default();
                PageDoc::new(
                    key.clone(),
                    meta.width as f64,
                    meta.height as f64,
                    anns,
                    meta.status,
                )
            }
        };
        self.imported = database::imported_annotations(&self.conn, &key).unwrap_or_default();
        self.predictions = match self.compare {
            Compare::Prediction(i) => self
                .models
                .get(i)
                .and_then(|m| database::predictions(&self.conn, &key, m).ok())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        self.page_diff = Some(diff(&doc.annotations, &self.imported));
        self.opened_snapshot = doc.annotations.clone();
        self.meta = Some(meta);
        self.doc = Some(doc);
        if let Some(d) = &mut self.doc {
            if d.status == ReviewStatus::Unseen {
                d.set_status_untracked(ReviewStatus::Opened);
            }
        }
        self.commit("open");
        self.image = match self.cache.get(&key) {
            Some(img) => self.check_image(img),
            None => ImageState::Loading,
        };
        self.request_images();
        self.update_title();
    }

    fn check_image(&self, img: Arc<PageImage>) -> ImageState {
        match &self.meta {
            Some(m) if (img.width(), img.height()) != (m.width, m.height) => {
                ImageState::Failed(format!(
                    "image is {}x{} but the project records {}x{}; boxes would be misplaced",
                    img.width(),
                    img.height(),
                    m.width,
                    m.height
                ))
            }
            _ => ImageState::Ready(img),
        }
    }

    fn request_images(&mut self) {
        let n = self.keys.len();
        if n == 0 {
            return;
        }
        let mut order = vec![self.pos];
        for i in 1..=self.settings.raw.prefetch_ahead {
            if self.pos + i < n {
                order.push(self.pos + i);
            }
        }
        for i in 1..=self.settings.raw.prefetch_behind {
            if self.pos >= i {
                order.push(self.pos - i);
            }
        }
        let mut jobs = Vec::new();
        for p in order {
            let key = &self.keys[p];
            if self.cache.contains(key) {
                continue;
            }
            let path = if p == self.pos {
                self.meta.as_ref().map(|m| m.source_path.clone())
            } else {
                database::page_meta(&self.conn, key)
                    .ok()
                    .flatten()
                    .map(|m| m.source_path)
            };
            if let Some(path) = path {
                jobs.push(Job {
                    page_key: key.clone(),
                    path,
                    generation: self.generation,
                });
            }
        }
        self.loader.want(jobs);
    }

    fn pinned(&self) -> Vec<String> {
        let n = self.keys.len();
        let lo = self.pos.saturating_sub(self.settings.raw.prefetch_behind);
        let hi = (self.pos + self.settings.raw.prefetch_ahead).min(n.saturating_sub(1));
        (lo..=hi)
            .filter_map(|i| self.keys.get(i).cloned())
            .collect()
    }

    /// Move to `pos`. With `accept`, the current page is first marked
    /// reviewed — but only if its image actually loaded.
    pub fn navigate(&mut self, pos: usize, accept: bool) {
        if self.editor.is_busy() {
            return;
        }
        if accept {
            let loaded = matches!(self.image, ImageState::Ready(_));
            if let Some(doc) = &mut self.doc {
                if !loaded {
                    let msg = format!("{}: image did not load — not marked reviewed", doc.page_key);
                    self.flash(&msg, true);
                } else if matches!(doc.status, ReviewStatus::Unseen | ReviewStatus::Opened) {
                    doc.set_status_untracked(ReviewStatus::Reviewed);
                    self.reviewed_in_queue += 1;
                }
            }
        }
        self.commit(if accept { "accept" } else { "navigate" });
        if pos >= self.keys.len() {
            if accept {
                self.flash(
                    "End of queue — every page in this queue has been visited",
                    false,
                );
            }
            return;
        }
        if pos != self.pos || self.doc.is_none() {
            self.open_page(pos);
        }
    }

    // --- background results -------------------------------------------------

    pub fn poll_background(&mut self) {
        while let Ok(loaded) = self.loader.results.try_recv() {
            self.dirty = true;
            push_ring(
                &mut self.perf.decode_ms,
                loaded.decode_time.as_secs_f64() * 1000.0,
            );
            let is_current = self
                .keys
                .get(self.pos)
                .is_some_and(|k| *k == loaded.page_key);
            match loaded.result {
                Ok(img) => {
                    let pinned = self.pinned();
                    let pinned: Vec<&str> = pinned.iter().map(String::as_str).collect();
                    self.cache
                        .insert(loaded.page_key.clone(), img.clone(), &pinned);
                    if is_current && !matches!(self.image, ImageState::Ready(_)) {
                        self.image = self.check_image(img);
                        self.raster.key = None;
                    }
                }
                Err(e) => {
                    if is_current {
                        self.image = ImageState::Failed(e);
                    }
                }
            }
        }
        while let Ok(ack) = self.writer.acks.try_recv() {
            self.dirty = true;
            match ack {
                WriteAck::Saved { page_key, revision } => {
                    let e = self.acked.entry(page_key).or_insert(0);
                    *e = (*e).max(revision);
                    self.save_error = None;
                }
                WriteAck::Failed {
                    page_key, error, ..
                } => {
                    self.save_error = Some(format!("{page_key}: {error}"));
                    // Allow a retry on the next commit.
                    self.sent.remove(&page_key);
                }
            }
        }
        self.trim_docs();
    }

    /// Block until the current page's image is resolved (tests, bench).
    pub fn wait_for_image(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.poll_background();
            if !matches!(self.image, ImageState::Loading) {
                return matches!(self.image, ImageState::Ready(_));
            }
            if Instant::now() > deadline {
                return false;
            }
            if let Some(l) = self.loader.wait(Duration::from_millis(20)) {
                // Put it back through the normal path.
                let key = l.page_key.clone();
                let is_current = self.keys.get(self.pos).is_some_and(|k| *k == key);
                match l.result {
                    Ok(img) => {
                        let pinned = self.pinned();
                        let pinned: Vec<&str> = pinned.iter().map(String::as_str).collect();
                        self.cache.insert(key, img.clone(), &pinned);
                        if is_current {
                            self.image = self.check_image(img);
                            self.raster.key = None;
                        }
                    }
                    Err(e) if is_current => self.image = ImageState::Failed(e),
                    Err(_) => {}
                }
            }
        }
    }

    /// Commit and wait for the writer (shutdown, tests).
    pub fn flush(&mut self) {
        self.commit("flush");
        self.writer.flush();
        self.poll_background();
    }

    // --- editing helpers ----------------------------------------------------

    fn after_edit(&mut self, out: Outcome) {
        if out.edited {
            self.commit("edit");
        }
        // A view change needs no explicit raster invalidation: `RasterKey`
        // holds the image, origin, scale and canvas rect, so the next frame
        // sees the change and can shift the cached raster for a pure pan.
    }

    fn image_ok(&self) -> bool {
        matches!(self.image, ImageState::Ready(_))
    }

    fn set_fit(&mut self) {
        if let Some(doc) = &self.doc {
            let margin = (8.0 * self.scale.0 as f64).round();
            self.view = ViewTransform::fit(self.canvas, doc.width, doc.height, margin);
            self.editor.zoom_mode = ZoomMode::Fit;
        }
    }

    fn zoom_by(&mut self, factor: f64, at: Option<Point>) {
        let Some(doc) = &self.doc else { return };
        let anchor = at
            .filter(|p| self.canvas.contains(*p))
            .unwrap_or(Point::new(
                self.canvas.x + self.canvas.w / 2.0,
                self.canvas.y + self.canvas.h / 2.0,
            ));
        self.dirty = true;
        self.view.zoom_at(anchor, factor);
        self.view
            .constrain(self.canvas, doc.width, doc.height, 64.0);
        self.editor.zoom_mode = ZoomMode::Free;
        // An in-progress drag continues: its start point is in image
        // coordinates, so the next move maps through the new transform.
        let p = self.editor.pointer;
        if self.editor.is_busy() {
            let ev = PointerEvent {
                phase: PointerPhase::Move,
                position: (p.x, p.y),
                button: None,
                modifiers: self.pointer.modifiers(),
                pointer_id: 0,
            };
            self.dispatch_canvas(&ev);
        }
    }

    fn dispatch_canvas(&mut self, ev: &PointerEvent) {
        let image_ok = self.image_ok();
        let Some(doc) = self.doc.as_mut() else { return };
        let out = self
            .editor
            .pointer(ev, doc, &mut self.view, self.canvas, image_ok);
        if out.redraw || out.view_changed || out.edited {
            self.dirty = true;
        }
        self.after_edit(out);
    }

    fn act(&mut self, action: Action) {
        self.dirty = true;
        // Overlays first.
        if self.help && !matches!(action, Action::SpaceDown | Action::SpaceUp) {
            self.help = false;
            if matches!(action, Action::Help | Action::Cancel) {
                return;
            }
        }
        if self.palette && action == Action::Cancel {
            self.palette = false;
            return;
        }
        match action {
            Action::SpaceDown => self.editor.space_held = true,
            Action::SpaceUp => self.editor.space_held = false,
            Action::PrevPage => {
                if self.pos > 0 {
                    self.navigate(self.pos - 1, false);
                }
            }
            Action::AcceptAndNext => self.navigate(self.pos + 1, true),
            Action::NextPage => self.navigate(self.pos + 1, false),
            Action::JumpBack10 => self.navigate(self.pos.saturating_sub(10), false),
            Action::JumpFwd10 => self.navigate(
                (self.pos + 10).min(self.keys.len().saturating_sub(1)),
                false,
            ),
            Action::FirstUnreviewed => {
                self.commit("navigate");
                // Statuses in the DB lag the in-memory current page by at most one commit.
                self.writer.flush();
                match self.first_unreviewed() {
                    Some(p) => self.navigate(p, false),
                    None => self.flash("No unreviewed pages in this queue", false),
                }
            }
            Action::Class(c) => {
                self.palette = false;
                if let Some(doc) = self.doc.as_mut() {
                    let out = self.editor.choose_class(c, doc);
                    self.after_edit(out);
                } else {
                    self.editor.active_class = c;
                }
            }
            Action::Fit => self.set_fit(),
            Action::Actual => {
                let p = self.editor.pointer;
                let anchor = if self.canvas.contains(p) {
                    p
                } else {
                    Point::new(
                        self.canvas.x + self.canvas.w / 2.0,
                        self.canvas.y + self.canvas.h / 2.0,
                    )
                };
                self.view.actual_at(anchor);
                // Whole-pixel origin: 100% is then an exact copy of the page.
                self.view.origin =
                    Point::new(self.view.origin.x.round(), self.view.origin.y.round());
                if let Some(doc) = &self.doc {
                    self.view
                        .constrain(self.canvas, doc.width, doc.height, 64.0);
                }
                self.editor.zoom_mode = ZoomMode::Actual;
            }
            Action::ZoomIn => {
                self.zoom_by(self.settings.raw.wheel_zoom_step, Some(self.editor.pointer))
            }
            Action::ZoomOut => self.zoom_by(
                1.0 / self.settings.raw.wheel_zoom_step,
                Some(self.editor.pointer),
            ),
            Action::Undo | Action::Redo => {
                if self.editor.is_busy() {
                    self.editor.cancel();
                }
                if let Some(doc) = self.doc.as_mut() {
                    let done = if action == Action::Undo {
                        doc.undo()
                    } else {
                        doc.redo()
                    };
                    match done {
                        Some(_) => {
                            self.editor.reconcile(doc);
                            self.commit("undo");
                        }
                        None => self.flash(
                            if action == Action::Undo {
                                "Nothing to undo"
                            } else {
                                "Nothing to redo"
                            },
                            false,
                        ),
                    }
                }
            }
            Action::Delete => {
                if let Some(doc) = self.doc.as_mut() {
                    let out = self.editor.delete_selected(doc);
                    self.after_edit(out);
                }
            }
            Action::ToggleIgnore => {
                if let Some(doc) = self.doc.as_mut() {
                    let out = self.editor.toggle_ignore(doc);
                    self.after_edit(out);
                }
            }
            Action::ToggleExclude | Action::ToggleAttention => {
                let target = if action == Action::ToggleExclude {
                    ReviewStatus::Excluded
                } else {
                    ReviewStatus::NeedsAttention
                };
                if let Some(doc) = self.doc.as_mut() {
                    let before = doc.status;
                    let after = if before == target {
                        ReviewStatus::Opened
                    } else {
                        target
                    };
                    doc.apply(Edit::Status { before, after });
                    let msg = match after {
                        ReviewStatus::Excluded => "Page excluded (X restores)",
                        ReviewStatus::NeedsAttention => "Page flagged: needs attention (N clears)",
                        _ => "Page restored",
                    };
                    self.flash(msg, false);
                    self.commit("status");
                    self.refresh_counts();
                }
            }
            Action::ToggleOutlines => {
                self.editor.show_outlines = !self.editor.show_outlines;
                self.editor.hover = None;
            }
            Action::ToggleTags => self.settings.raw.show_tags = !self.settings.raw.show_tags,
            Action::ToggleHud => self.hud = !self.hud,
            Action::CycleCompare => self.cycle_compare(),
            Action::CycleQueue => self.cycle_queue(),
            Action::Cancel => {
                if !self.editor.cancel() {
                    self.editor.selected = None;
                }
            }
            Action::NextBox | Action::PrevBox => {
                if let Some(doc) = &self.doc {
                    let out = self.editor.cycle_selection(doc, action == Action::NextBox);
                    self.after_edit(out);
                }
            }
            Action::Nudge { dx, dy, resize } => {
                if let Some(doc) = self.doc.as_mut() {
                    let out = self.editor.nudge(doc, dx as f64, dy as f64, resize);
                    self.after_edit(out);
                }
            }
            Action::Help => self.help = !self.help,
            Action::Fullscreen => {
                self.is_fullscreen = !self.is_fullscreen;
                self.fullscreen = Some(self.is_fullscreen);
            }
            Action::PerfOverlay => self.perf_overlay = !self.perf_overlay,
            Action::RevertPage => self.revert_page(),
            Action::SaveNotice => {
                self.flush();
                self.flash(
                    "Every edit is saved automatically when you release the mouse",
                    false,
                );
            }
        }
    }

    fn cycle_compare(&mut self) {
        self.compare = match self.compare {
            Compare::Off => Compare::Imported,
            Compare::Imported => {
                if self.models.is_empty() {
                    Compare::Diff
                } else {
                    Compare::Prediction(0)
                }
            }
            Compare::Prediction(i) if i + 1 < self.models.len() => Compare::Prediction(i + 1),
            Compare::Prediction(_) => Compare::Diff,
            Compare::Diff => Compare::Off,
        };
        self.predictions = match (self.compare, self.keys.get(self.pos)) {
            (Compare::Prediction(i), Some(k)) => {
                database::predictions(&self.conn, k, &self.models[i]).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let name = self.compare_label();
        self.flash(&format!("Comparison: {name}"), false);
    }

    /// What a revert would restore now: the page as opened on this visit,
    /// or — when it already looks like that — the imported layer. `None`
    /// when both equal the current boxes.
    fn revert_target(&self) -> Option<(Vec<Annotation>, RevertTo)> {
        let doc = self.doc.as_ref()?;
        if doc.annotations != self.opened_snapshot {
            return Some((self.opened_snapshot.clone(), RevertTo::Opened));
        }
        if doc.annotations != self.imported {
            return Some((self.imported.clone(), RevertTo::Imported));
        }
        None
    }

    /// Ctrl+C / the Revert button: one undoable edit back to the page as it
    /// was opened; pressed again, back to the imported (prefilled) boxes.
    fn revert_page(&mut self) {
        if self.editor.is_busy() {
            self.editor.cancel();
        }
        let Some((target, to)) = self.revert_target() else {
            self.flash(
                "Nothing to revert: the page matches the imported boxes",
                false,
            );
            return;
        };
        let Some(doc) = self.doc.as_mut() else { return };
        let before = doc.annotations.clone();
        let n = target.len();
        doc.apply(Edit::Replace {
            before,
            after: target,
        });
        self.editor.reconcile(doc);
        self.commit("revert");
        let msg = match to {
            RevertTo::Opened => format!(
                "Reverted to the page as you opened it ({n} boxes). Ctrl+C again: imported boxes; Ctrl+Z: undo"
            ),
            RevertTo::Imported => format!("Reverted to the imported boxes ({n}). Ctrl+Z: undo"),
        };
        self.flash(&msg, false);
    }

    /// Cycle the queue's status filter, keeping the current page when it is
    /// still in the new queue.
    fn cycle_queue(&mut self) {
        use ReviewStatus::*;
        const MODES: [(&str, &[ReviewStatus]); 5] = [
            ("all", &[]),
            ("to do", &[Unseen, Opened]),
            ("needs attention", &[NeedsAttention]),
            ("excluded", &[Excluded]),
            ("reviewed", &[Reviewed]),
        ];
        if self.editor.is_busy() {
            return;
        }
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        let current = self.current_key().map(str::to_owned);
        for _ in 0..MODES.len() {
            self.queue_mode = (self.queue_mode + 1) % MODES.len();
            let (name, statuses) = MODES[self.queue_mode];
            self.filter.statuses = if self.queue_mode == 0 {
                self.base_statuses.clone()
            } else {
                statuses.to_vec()
            };
            let keys = database::page_keys(&self.conn, &self.filter).unwrap_or_default();
            if keys.is_empty() && self.queue_mode != 0 {
                continue; // nothing to show in this mode; try the next
            }
            self.keys = keys;
            self.refresh_counts();
            let pos = current
                .as_ref()
                .and_then(|k| self.keys.iter().position(|x| x == k));
            match pos {
                Some(p) => {
                    self.pos = p;
                    self.update_title();
                    self.request_images();
                }
                None => self.open_page(0),
            }
            let n = self.keys.len();
            self.flash(&format!("Queue: {name} ({n} pages)"), false);
            return;
        }
    }

    fn compare_label(&self) -> String {
        match self.compare {
            Compare::Off => "off".into(),
            Compare::Imported => "imported (teacher) layer".into(),
            Compare::Prediction(i) => format!(
                "model {}",
                self.models.get(i).map(String::as_str).unwrap_or("?")
            ),
            Compare::Diff => "differences vs imported".into(),
        }
    }

    // --- pointer routing ----------------------------------------------------

    fn route_pointer(&mut self) {
        let events: Vec<PointerEvent> = self.pointer.drain().collect();
        for ev in events {
            let p = Point::new(ev.position.0, ev.position.1);
            if ev.phase != PointerPhase::Move {
                self.dirty = true;
            }
            match ev.phase {
                PointerPhase::Down => {
                    if self.help {
                        self.help = false;
                        continue;
                    }
                    if let Some(popup) = self.popup_rect.filter(|_| self.palette) {
                        if popup.contains(p.x as i32, p.y as i32) {
                            self.capture.capture(SURFACE_POPUP);
                            continue;
                        }
                        self.palette = false;
                    }
                    let in_toolbar = self.hud && p.y < self.canvas.y;
                    if in_toolbar || (self.hud && p.y >= self.canvas.y + self.canvas.h) {
                        if self.capture.capture(SURFACE_TOOLBAR) {
                            self.pressed_toolbar = self.toolbar_hit(p);
                        }
                    } else if self.capture.capture(SURFACE_CANVAS) {
                        self.dispatch_canvas(&ev);
                    }
                }
                PointerPhase::Move => match self.capture.owner() {
                    Some(SURFACE_CANVAS) => self.dispatch_canvas(&ev),
                    Some(_) => {}
                    None => {
                        self.editor.pointer = p;
                        if self.canvas.contains(p) {
                            self.dispatch_canvas(&ev);
                        } else if self.editor.hover.take().is_some() {
                            self.dirty = true;
                        }
                    }
                },
                PointerPhase::Up | PointerPhase::Cancel => match self.capture.owner() {
                    Some(SURFACE_CANVAS) => {
                        self.dispatch_canvas(&ev);
                        // Release capture only when the gesture's button is up.
                        if !self.editor.is_busy() {
                            self.capture.release(SURFACE_CANVAS);
                        }
                    }
                    Some(SURFACE_TOOLBAR) => {
                        let hit = self.toolbar_hit(p);
                        if ev.phase == PointerPhase::Up
                            && hit.is_some()
                            && hit == self.pressed_toolbar
                        {
                            match hit {
                                Some(ToolbarItem::Class(c)) => self.act(Action::Class(c)),
                                Some(ToolbarItem::More) => self.palette = !self.palette,
                                Some(ToolbarItem::Compare) => self.cycle_compare(),
                                Some(ToolbarItem::Revert) => self.act(Action::RevertPage),
                                None => {}
                            }
                        }
                        self.pressed_toolbar = None;
                        self.capture.release(SURFACE_TOOLBAR);
                    }
                    Some(SURFACE_POPUP) => {
                        if ev.phase == PointerPhase::Up {
                            let hit = self
                                .popup_hits
                                .iter()
                                .find(|(r, _)| r.contains(p.x as i32, p.y as i32))
                                .map(|(_, c)| *c);
                            if let Some(c) = hit {
                                self.act(Action::Class(c));
                            }
                        }
                        self.capture.release(SURFACE_POPUP);
                    }
                    _ => {
                        self.capture.clear();
                    }
                },
            }
        }
    }

    fn toolbar_hit(&self, p: Point) -> Option<ToolbarItem> {
        self.toolbar_hits
            .iter()
            .find(|(r, _)| r.contains(p.x as i32, p.y as i32))
            .map(|(_, i)| *i)
    }

    fn cursor(&self) -> CursorShape {
        let corner = |h: Handle| match h {
            Handle::TopLeft | Handle::BottomRight => CursorShape::ResizeDiagonalTlBr,
            Handle::TopRight | Handle::BottomLeft => CursorShape::ResizeDiagonalTrBl,
            Handle::Move => CursorShape::Pointer,
        };
        match &self.editor.state {
            ToolState::Editing { handle, .. } => corner(*handle),
            ToolState::Panning { .. } => CursorShape::Pointer,
            ToolState::Drawing { .. } => CursorShape::Default,
            ToolState::Idle => {
                if self.editor.space_held {
                    CursorShape::Pointer
                } else if let Some((_, h)) = &self.editor.hover {
                    corner(*h)
                } else {
                    CursorShape::Default
                }
            }
        }
    }

    // --- drawing ------------------------------------------------------------

    fn style(&self, logical: f32) -> TextStyle {
        TextStyle::new(fonts::FACE, logical * self.scale.0)
    }

    fn layout(&mut self, w: u32, h: u32) {
        let s = self.scale.0 as f64;
        let (top, bottom) = if self.hud {
            ((40.0 * s).round(), (24.0 * s).round())
        } else {
            (0.0, 0.0)
        };
        let canvas = CanvasRect {
            x: 0.0,
            y: top,
            w: w as f64,
            h: (h as f64 - top - bottom).max(1.0),
        };
        let changed = canvas != self.canvas;
        self.canvas = canvas;
        if changed {
            self.dirty = true;
            self.raster.key = None;
            match self.editor.zoom_mode {
                ZoomMode::Fit => self.set_fit(),
                _ => {
                    if let Some(doc) = &self.doc {
                        self.view
                            .constrain(self.canvas, doc.width, doc.height, 64.0);
                    }
                }
            }
        }
    }

    fn render_frame(&mut self, buffer: &mut WindowBuffer) {
        let t0 = Instant::now();
        let (bw, bh) = (buffer.width, buffer.height);
        if bw == 0 || bh == 0 {
            return;
        }
        self.layout(bw, bh);
        self.route_pointer();
        if self.editor.zoom_mode == ZoomMode::Fit && self.doc.is_some() {
            let before = self.view;
            self.set_fit();
            if before != self.view {
                self.raster.key = None;
                self.dirty = true;
            }
        }
        if let Some((_, at, err)) = &self.message {
            if at.elapsed() >= Duration::from_secs(if *err { 8 } else { 3 }) {
                self.message = None;
                self.dirty = true;
            }
        }
        if !self.dirty && (bw, bh) == self.last_size {
            self.perf.skipped += 1;
            return;
        }
        self.dirty = false;
        self.last_size = (bw, bh);

        // Page raster (cached).
        let cx = self.canvas.x as i32;
        let cy = self.canvas.y as i32;
        let cw = self.canvas.w as u32;
        let ch = self.canvas.h as u32;
        match &self.image {
            ImageState::Ready(img) => {
                let key = RasterKey {
                    image_id: Arc::as_ptr(img) as usize,
                    origin_x: self.view.origin.x,
                    origin_y: self.view.origin.y,
                    scale: self.view.scale,
                    w: cw,
                    h: ch,
                    x: cx,
                    y: cy,
                };
                if self.raster.key != Some(key) {
                    let r0 = Instant::now();
                    let img = img.clone();
                    // A pure pan by whole pixels shifts the cached raster and
                    // resamples only the exposed strips.
                    let scrolled = match self.raster.key {
                        Some(old)
                            if RasterKey {
                                origin_x: key.origin_x,
                                origin_y: key.origin_y,
                                ..old
                            } == key =>
                        {
                            scroll_page(
                                &mut self.raster,
                                &img,
                                &self.view,
                                cx,
                                cy,
                                key.origin_x - old.origin_x,
                                key.origin_y - old.origin_y,
                                self.render_threads,
                            )
                        }
                        _ => false,
                    };
                    if !scrolled {
                        render_page(
                            &mut self.raster,
                            &img,
                            &self.view,
                            cx,
                            cy,
                            cw,
                            ch,
                            self.render_threads,
                        );
                    }
                    self.raster.key = Some(key);
                    self.perf.raster_renders += 1;
                    self.perf.raster_scrolls += u64::from(scrolled);
                    push_ring(
                        &mut self.perf.raster_ms,
                        r0.elapsed().as_secs_f64() * 1000.0,
                    );
                }
                blit_raster(
                    &mut buffer.pixels,
                    bw as usize,
                    bh as usize,
                    &self.raster,
                    cx,
                    cy,
                );
                if let Some(t) = self.nav_started.take() {
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    push_ring(&mut self.perf.switch_ms, ms);
                    if self.perf_log {
                        eprintln!(
                            "[perf] page {} shown {ms:.1} ms after navigation",
                            self.pos + 1
                        );
                    }
                }
            }
            _ => {
                let mut p = Painter::new(buffer);
                p.fill_rect(Rect::new(cx, cy, cw as i32, ch as i32), CANVAS_BG);
            }
        }

        let mut painter = Painter::new(buffer);
        painter.push_clip(Rect::new(cx, cy, cw as i32, ch as i32));
        self.draw_overlays(&mut painter);
        self.draw_status_banner(&mut painter);
        self.draw_canvas_messages(&mut painter);
        painter.pop_clip();
        if self.hud {
            self.draw_top_bar(&mut painter, bw as i32);
            self.draw_bottom_bar(&mut painter, bw as i32, bh as i32);
        } else {
            self.toolbar_hits.clear();
        }
        if self.palette {
            self.draw_palette(&mut painter);
        } else {
            self.popup_rect = None;
        }
        if self.help {
            self.draw_help(&mut painter, bw as i32, bh as i32);
        }
        if self.perf_overlay {
            self.draw_perf(&mut painter, bw as i32);
        }
        self.perf.frames += 1;
        push_ring(&mut self.perf.frame_ms, t0.elapsed().as_secs_f64() * 1000.0);
    }

    fn draw_status_banner(&mut self, p: &mut Painter<'_>) {
        let Some(doc) = &self.doc else { return };
        let (label, color) = match doc.status {
            ReviewStatus::Excluded => ("EXCLUDED  ·  X restores", DIFF_DELETED),
            ReviewStatus::NeedsAttention => ("NEEDS ATTENTION  ·  N clears", DIFF_MODIFIED),
            _ => return,
        };
        let st = self.style(12.0);
        let tw = self.text.measure(label, st);
        let lh = self.text.line_height(st);
        let pad = self.scale.px(10);
        let r = Rect::new(
            self.canvas.x as i32 + (self.canvas.w as i32 - tw) / 2 - pad,
            self.canvas.y as i32 + self.scale.px(8),
            tw + 2 * pad,
            lh + self.scale.px(6),
        );
        p.fill_rect(r, color);
        self.text
            .draw(p, label, r.x + pad, r.y + self.scale.px(3), st, 0x00101010);
        if doc.status == ReviewStatus::Excluded {
            // Dim the page: it will not be exported.
            let c = Rect::new(
                self.canvas.x as i32,
                self.canvas.y as i32,
                self.canvas.w as i32,
                self.canvas.h as i32,
            );
            p.blend_rect(c, 0x000000, 70);
            p.fill_rect(r, color);
            self.text
                .draw(p, label, r.x + pad, r.y + self.scale.px(3), st, 0x00101010);
        }
    }

    fn draw_canvas_messages(&mut self, p: &mut Painter<'_>) {
        let msg = match (&self.image, self.keys.is_empty()) {
            (_, true) => Some(("No pages match this queue".to_owned(), HUD_DIM)),
            (ImageState::Loading, _) => Some(("Loading page…".to_owned(), HUD_DIM)),
            (ImageState::Failed(e), _) => {
                Some((format!("Image failed to load: {e}"), DIFF_DELETED))
            }
            _ => None,
        };
        if let Some((m, color)) = msg {
            let st = self.style(15.0);
            let area = Rect::new(
                self.canvas.x as i32,
                self.canvas.y as i32,
                self.canvas.w as i32,
                self.canvas.h as i32,
            );
            let lh = self.text.line_height(st);
            self.text.draw_fitted(
                p,
                &m,
                area.inset(self.scale.px(16)),
                area.y + (area.h - lh) / 2,
                st,
                color,
                Align::Centre,
            );
        }
    }

    /// A class tag for the box at `r`: above it when that spot covers no
    /// other box (`others`), else inside its top-left corner, translucent.
    fn draw_tag(&mut self, p: &mut Painter<'_>, r: Rect, label: &str, color: u32, others: &[Rect]) {
        let st = self.style(11.0);
        let tw = self.text.measure(label, st);
        let tag_h = self.text.line_height(st);
        let pad = self.scale.px(3);
        let tag_w = tw + 2 * pad;
        if r.w < self.scale.px(6) {
            return;
        }
        let above = Rect::new(r.x, r.y - tag_h, tag_w, tag_h);
        let free = above.y >= self.canvas.y as i32
            && !others
                .iter()
                .any(|o| *o != r && !o.intersect(above).is_empty());
        let (tag, alpha) = if free {
            (above, 225)
        } else {
            (Rect::new(r.x + 1, r.y + 1, tag_w, tag_h), 160)
        };
        p.blend_rect(tag, color, alpha);
        self.text
            .draw(p, label, tag.x + pad, tag.y, st, text_on(color));
    }

    fn draw_overlays(&mut self, p: &mut Painter<'_>) {
        let Some(doc) = &self.doc else { return };
        let annotations = doc.annotations.clone();
        let drawing = self.editor.drawing_box(doc);
        if !self.image_ok() {
            return;
        }
        let view = self.view;
        let colors = self.settings.colors;
        let s = self.scale.0 as f64;
        let canvas_rect = Rect::new(
            self.canvas.x as i32,
            self.canvas.y as i32,
            self.canvas.w as i32,
            self.canvas.h as i32,
        );

        // Comparison layers underneath.
        match self.compare {
            Compare::Imported => {
                let imported = self.imported.clone();
                for a in &imported {
                    let r = pixel_rect(view.box_to_screen(a.bbox));
                    if canvas_rect.intersect(r.inset(-2)).is_empty() {
                        continue;
                    }
                    let c = if a.is_ignore {
                        IGNORE_COLOR
                    } else {
                        colors[a.class_id as usize]
                    };
                    dashed_outline(p, r.inset(-1), c, 3, 3);
                }
            }
            Compare::Prediction(_) => {
                let preds = self.predictions.clone();
                let thr = self.prediction_threshold;
                for pr in preds.iter().filter(|x| x.score >= thr) {
                    let r = pixel_rect(view.box_to_screen(pr.bbox));
                    if canvas_rect.intersect(r.inset(-2)).is_empty() {
                        continue;
                    }
                    let c = colors[pr.class_id as usize];
                    dashed_outline(p, r, c, 6, 3);
                    if self.settings.raw.show_tags {
                        let label = format!("{} {:.2}", class(pr.class_id).short, pr.score);
                        let br = Rect::new(r.x, r.bottom(), r.w, 0);
                        let st = self.style(10.0);
                        let tw = self.text.measure(&label, st);
                        let lh = self.text.line_height(st);
                        let tag = Rect::new(br.right() - tw - 4, br.y, tw + 4, lh);
                        p.blend_rect(tag, 0x000000, 160);
                        self.text.draw(p, &label, tag.x + 2, tag.y, st, c);
                    }
                }
            }
            Compare::Diff => {
                if let Some(d) = self.page_diff.clone() {
                    let deleted = d.deleted.clone();
                    for a in &deleted {
                        let r = pixel_rect(view.box_to_screen(a.bbox));
                        dashed_outline(p, r, DIFF_DELETED, 5, 3);
                        self.draw_tag(
                            p,
                            r,
                            &format!("deleted {}", class(a.class_id).short),
                            DIFF_DELETED,
                            &[],
                        );
                    }
                    let originals: Vec<Box2> = annotations
                        .iter()
                        .zip(&d.changes)
                        .filter(|(_, c)| matches!(c, Change::Modified { geometry: true, .. }))
                        .filter_map(|(a, _)| {
                            self.imported.iter().find(|o| o.id == a.id).map(|o| o.bbox)
                        })
                        .collect();
                    for b in originals {
                        dashed_outline(p, pixel_rect(view.box_to_screen(b)), DIFF_MODIFIED, 4, 3);
                    }
                }
            }
            Compare::Off => {}
        }

        if !self.editor.show_outlines {
            if let Some((c, b)) = drawing {
                outline(p, pixel_rect(view.box_to_screen(b)), colors[c as usize], 1);
            }
            return;
        }

        // Draw order: large first so nested small boxes sit on top; selection last.
        let mut order: Vec<usize> = (0..annotations.len()).collect();
        order.sort_by(|&a, &b| {
            annotations[b]
                .bbox
                .area()
                .total_cmp(&annotations[a].bbox.area())
        });
        let sel = self.editor.selected.clone();
        if let Some(sel) = &sel {
            if let Some(pos) = order.iter().position(|&i| &annotations[i].id == sel) {
                let i = order.remove(pos);
                order.push(i);
            }
        }
        let changes = self.page_diff.as_ref().map(|d| d.changes.clone());
        let anns: Vec<(Annotation, Box2)> = order
            .iter()
            .map(|&i| {
                let a = annotations[i].clone();
                let b = self.editor.preview_for(&a.id).unwrap_or(a.bbox);
                (a, b)
            })
            .collect();
        let change_of = |id: &str| -> Option<Change> {
            let idx = annotations.iter().position(|a| a.id == id)?;
            changes.as_ref().and_then(|c| c.get(idx).copied())
        };
        let hover = self.editor.hover.clone();
        let show_tags = self.settings.raw.show_tags;
        let fill_alpha = self.settings.raw.selection_fill_alpha;
        let compare = self.compare;
        let mut tags: Vec<(Rect, String, u32)> = Vec::new();
        let tag_h = self.text.line_height(self.style(11.0));
        for (a, b) in &anns {
            let r = pixel_rect(view.box_to_screen(*b));
            if canvas_rect.intersect(r.inset(-12)).is_empty() {
                continue;
            }
            let color = if a.is_ignore {
                IGNORE_COLOR
            } else {
                colors[a.class_id as usize]
            };
            let selected = sel.as_deref() == Some(a.id.as_str());
            let hovered = hover.as_ref().is_some_and(|(id, _)| id == &a.id);
            if selected {
                p.blend_rect(r, color, fill_alpha);
            } else if hovered && matches!(hover, Some((_, Handle::Move))) {
                p.blend_rect(r, color, fill_alpha / 2);
            }
            if a.is_ignore {
                dashed_outline(p, r, color, 5, 3);
            } else {
                outline(p, r, color, 1);
            }
            if selected {
                // Corner marks show the four grab targets.
                let k = (5.0 * s).round().max(3.0) as i32;
                for (x, y) in [
                    (r.x, r.y),
                    (r.right() - k, r.y),
                    (r.x, r.bottom() - k),
                    (r.right() - k, r.bottom() - k),
                ] {
                    p.fill_rect(Rect::new(x, y, k, k), color);
                }
            }
            if let Some((id, h)) = &hover {
                if id == &a.id && h.is_corner() && self.editor.state == ToolState::Idle {
                    let k = (9.0 * s).round() as i32;
                    let (x, y) = match h {
                        Handle::TopLeft => (r.x, r.y),
                        Handle::TopRight => (r.right(), r.y),
                        Handle::BottomLeft => (r.x, r.bottom()),
                        _ => (r.right(), r.bottom()),
                    };
                    let m = Rect::new(x - k / 2, y - k / 2, k, k);
                    p.fill_rect(m, 0xffffff);
                    outline(p, m, color, 1);
                }
            }
            if compare == Compare::Diff {
                if let Some(Change::Added) = change_of(&a.id) {
                    tags.push((r, "new".into(), DIFF_ADDED));
                    continue;
                }
                if let Some(Change::Modified { class: true, .. }) = change_of(&a.id) {
                    let was = self
                        .imported
                        .iter()
                        .find(|o| o.id == a.id)
                        .map(|o| class(o.class_id).short)
                        .unwrap_or("?");
                    tags.push((
                        r,
                        format!("{} (was {was})", class(a.class_id).short),
                        DIFF_MODIFIED,
                    ));
                    continue;
                }
            }
            // Colour already says the class; a tag is drawn where it does not
            // bury the text it labels (box at least about as tall as the tag),
            // and always on the box being worked on.
            if show_tags && (selected || hovered || r.h * 5 >= tag_h * 4) {
                let label = if a.is_ignore {
                    format!("{} · ignore", class(a.class_id).short)
                } else {
                    class(a.class_id).short.to_owned()
                };
                tags.push((r, label, color));
            }
        }
        let rects: Vec<Rect> = anns
            .iter()
            .map(|(_, b)| pixel_rect(view.box_to_screen(*b)))
            .collect();
        for (r, label, color) in tags {
            self.draw_tag(p, r, &label, color, &rects);
        }
        if let Some((c, b)) = drawing {
            let r = pixel_rect(view.box_to_screen(b));
            p.blend_rect(r, colors[c as usize], 24);
            outline(p, r, colors[c as usize], 1);
        }
    }

    fn draw_top_bar(&mut self, p: &mut Painter<'_>, w: i32) {
        let s = self.scale;
        let h = self.canvas.y as i32;
        p.fill_rect(Rect::new(0, 0, w, h), HUD_BG);
        p.fill_rect(Rect::new(0, h - 1, w, 1), HUD_EDGE);
        self.toolbar_hits.clear();
        let body = self.style(13.0);
        let small = self.style(11.5);
        let lh = self.text.line_height(body);
        let ty = (h - lh) / 2;
        let mut x = s.px(12);
        x += self.text.draw(p, "Liminal Annotator", x, ty, body, HUD_DIM) + s.px(16);

        let n = self.keys.len();
        let queue = format!(
            "{}  {} / {} · {}%",
            self.filter.describe(),
            if n == 0 { 0 } else { self.pos + 1 },
            n,
            (self.reviewed_in_queue * 100).checked_div(n).unwrap_or(0)
        );
        x += self.text.draw(p, &queue, x, ty, body, HUD_TEXT) + s.px(18);

        // Right side first, so chips know how much room they have.
        let right = match &self.meta {
            Some(m) => {
                let file = Path::new(&m.file_name)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                format!("{} · {} · {}×{} px", m.book_id, file, m.width, m.height)
            }
            None => String::new(),
        };
        let zoom = match self.editor.zoom_mode {
            ZoomMode::Fit => format!("Fit {:.0}%", self.view.scale * 100.0),
            ZoomMode::Actual => "100%".to_owned(),
            ZoomMode::Free => format!("{:.0}%", self.view.scale * 100.0),
        };
        let rw = self.text.measure(&right, small);
        let zw = self.text.measure(&zoom, body);
        let right_x = w - s.px(12) - rw.max(zw);
        let lh_small = self.text.line_height(small);
        self.text.draw(
            p,
            &right,
            w - s.px(12) - rw,
            (h / 2 - lh_small).max(0),
            small,
            HUD_DIM,
        );
        self.text
            .draw(p, &zoom, w - s.px(12) - zw, h / 2, small, HUD_TEXT);

        // Class chips: "1 Text", ...
        let chip_h = s.px(24);
        let cy = (h - chip_h) / 2;
        let mut shown = 0;
        let entries: Vec<(String, u8)> = self
            .settings
            .digit
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.map(|c| (format!("{}", i + 1), c)))
            .chain(
                self.settings
                    .shift_digit
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.map(|c| (format!("S{}", i + 1), c))),
            )
            .collect();
        let total = entries.len();
        let more_w = s.px(90);
        for (key, c) in &entries {
            let label = format!("{key} {}", class(*c).short);
            let cw = self.text.measure(&label, small) + s.px(22);
            if x + cw + more_w + s.px(16) > right_x {
                break;
            }
            let r = Rect::new(x, cy, cw, chip_h);
            let color = self.settings.colors[*c as usize];
            let active = self.editor.active_class == *c;
            if active {
                p.fill_rect(r, 0x0033_3a44);
                outline(p, r, color, 1);
            }
            p.fill_rect(
                Rect::new(
                    r.x + s.px(6),
                    r.y + (chip_h - s.px(8)) / 2,
                    s.px(8),
                    s.px(8),
                ),
                color,
            );
            self.text.draw(
                p,
                &label,
                r.x + s.px(18),
                r.y + (chip_h - lh_small) / 2,
                small,
                if active { HUD_TEXT } else { HUD_DIM },
            );
            self.toolbar_hits.push((r, ToolbarItem::Class(*c)));
            x += cw + s.px(4);
            shown += 1;
        }
        if shown < total {
            let active_hidden = !entries
                .iter()
                .take(shown)
                .any(|(_, c)| *c == self.editor.active_class);
            let label = if active_hidden {
                format!(
                    "+{} · {}",
                    total - shown,
                    class(self.editor.active_class).short
                )
            } else {
                format!("+ {} more", total - shown)
            };
            let cw = self.text.measure(&label, small) + s.px(16);
            let r = Rect::new(x, cy, cw, chip_h);
            if active_hidden {
                outline(
                    p,
                    r,
                    self.settings.colors[self.editor.active_class as usize],
                    1,
                );
            } else {
                outline(p, r, HUD_EDGE, 1);
            }
            self.text.draw(
                p,
                &label,
                r.x + s.px(8),
                r.y + (chip_h - lh_small) / 2,
                small,
                HUD_TEXT,
            );
            self.toolbar_hits.push((r, ToolbarItem::More));
        }
    }

    fn draw_bottom_bar(&mut self, p: &mut Painter<'_>, w: i32, h: i32) {
        let s = self.scale;
        let top = (self.canvas.y + self.canvas.h) as i32;
        let bh = h - top;
        p.fill_rect(Rect::new(0, top, w, bh), HUD_BG);
        p.fill_rect(Rect::new(0, top, w, 1), HUD_EDGE);
        let st = self.style(11.5);
        let lh = self.text.line_height(st);
        let ty = top + (bh - lh) / 2;
        let mut x = s.px(12);

        // Status badge.
        if let Some(doc) = &self.doc {
            let (label, color) = match doc.status {
                ReviewStatus::Unseen => ("Unseen", HUD_DIM),
                ReviewStatus::Opened => ("Opened", 0x00c8_cdd4),
                ReviewStatus::Reviewed => ("Reviewed", DIFF_ADDED),
                ReviewStatus::Excluded => ("Excluded", DIFF_DELETED),
                ReviewStatus::NeedsAttention => ("Needs attention", DIFF_MODIFIED),
            };
            let bw = self.text.measure(label, st) + s.px(12);
            let r = Rect::new(x, top + s.px(3), bw, bh - s.px(6));
            outline(p, r, color, 1);
            self.text.draw(p, label, r.x + s.px(6), ty, st, color);
            x += bw + s.px(10);
        }
        // Save state.
        let pending = self.unsaved();
        let (save, sc) = match (&self.save_error, pending) {
            (Some(e), _) => (format!("save failed: {e}"), DIFF_DELETED),
            (None, 0) => ("saved".to_owned(), HUD_DIM),
            (None, n) => (format!("saving {n}…"), DIFF_MODIFIED),
        };
        x += self.text.draw(p, &save, x, ty, st, sc) + s.px(14);

        // Selection / page info, or the transient message.
        let info = if let Some((m, at, err)) = &self.message {
            if at.elapsed() < Duration::from_secs(if *err { 8 } else { 3 }) {
                Some((m.clone(), if *err { DIFF_DELETED } else { HUD_TEXT }))
            } else {
                None
            }
        } else {
            None
        };
        let info = info.or_else(|| {
            let doc = self.doc.as_ref()?;
            if let Some(sel) = &self.editor.selected {
                let a = doc.get(sel)?;
                let b = self.editor.preview_for(sel).unwrap_or(a.bbox);
                let prov = a
                    .provenance_json
                    .as_deref()
                    .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
                    .map(|v| {
                        let src = v.get("source").and_then(|x| x.as_str()).unwrap_or("");
                        let score = v
                            .get("score")
                            .and_then(|x| x.as_f64())
                            .map(|x| format!(" {x:.2}"))
                            .unwrap_or_default();
                        format!(" · {src}{score}")
                    })
                    .unwrap_or_else(|| " · drawn".into());
                Some((
                    format!(
                        "{}{} · x {:.0} y {:.0} · {:.0}×{:.0}{}",
                        class(a.class_id).name,
                        if a.is_ignore { " (ignore)" } else { "" },
                        b.x0,
                        b.y0,
                        b.width(),
                        b.height(),
                        prov
                    ),
                    HUD_TEXT,
                ))
            } else {
                let reason = self
                    .meta
                    .as_ref()
                    .and_then(|m| m.queue_reason.clone())
                    .map(|r| format!(" · {r}"))
                    .unwrap_or_default();
                let counts = self
                    .page_diff
                    .as_ref()
                    .map(|d| {
                        let (_, m, a, del) = d.count();
                        if m + a + del == 0 {
                            String::new()
                        } else {
                            format!(" · edits: {m} changed, {a} new, {del} deleted")
                        }
                    })
                    .unwrap_or_default();
                Some((
                    format!("{} boxes{reason}{counts}", doc.annotations.len()),
                    HUD_DIM,
                ))
            }
        });
        let right = format!("Compare: {} (C) · F1 help", self.compare_label());
        let rw = self.text.measure(&right, st);
        // Revert button, left of the compare label. Dim when there is nothing to revert to.
        let (revert_label, revert_live) = match self.revert_target() {
            Some((_, RevertTo::Opened)) => ("Revert page (Ctrl+C)", true),
            Some((_, RevertTo::Imported)) => ("Revert to imported (Ctrl+C)", true),
            None => ("Revert page (Ctrl+C)", false),
        };
        let bw2 = self.text.measure(revert_label, st) + s.px(14);
        let button = Rect::new(
            w - s.px(12) - rw - s.px(14) - bw2,
            top + s.px(3),
            bw2,
            bh - s.px(6),
        );
        let bc = if revert_live { DIFF_MODIFIED } else { HUD_EDGE };
        outline(p, button, bc, 1);
        self.text.draw(
            p,
            revert_label,
            button.x + s.px(7),
            ty,
            st,
            if revert_live { HUD_TEXT } else { HUD_DIM },
        );
        self.toolbar_hits.push((button, ToolbarItem::Revert));
        let rw = rw + s.px(14) + bw2;
        if let Some((m, c)) = info {
            let area = Rect::new(x, top, (w - rw - s.px(24) - x).max(0), bh);
            self.text.draw_fitted(p, &m, area, ty, st, c, Align::Left);
        }
        let cw = self.text.measure(&right, st);
        self.text
            .draw(p, &right, w - s.px(12) - cw, ty, st, HUD_DIM);
        self.toolbar_hits.push((
            Rect::new(w - s.px(12) - cw, top, cw, bh),
            ToolbarItem::Compare,
        ));
    }

    fn draw_palette(&mut self, p: &mut Painter<'_>) {
        let s = self.scale;
        let st = self.style(13.0);
        let lh = self.text.line_height(st);
        let row = lh + s.px(10);
        let mut items: Vec<(String, u8)> = Vec::new();
        for (i, c) in self.settings.digit.iter().enumerate() {
            if let Some(c) = c {
                items.push((format!("{}", i + 1), *c));
            }
        }
        for (i, c) in self.settings.shift_digit.iter().enumerate() {
            if let Some(c) = c {
                items.push((format!("Shift+{}", i + 1), *c));
            }
        }
        let w = s.px(260);
        let h = row * items.len() as i32 + s.px(12);
        let x = self
            .toolbar_hits
            .iter()
            .find(|(_, i)| *i == ToolbarItem::More)
            .map(|(r, _)| r.x)
            .unwrap_or(s.px(200));
        let y = self.canvas.y as i32 + s.px(4);
        let panel = Rect::new(x, y, w, h);
        p.fill_rect(panel, HUD_BG);
        outline(p, panel, HUD_EDGE, 1);
        self.popup_hits.clear();
        for (i, (key, c)) in items.iter().enumerate() {
            let r = Rect::new(x + s.px(6), y + s.px(6) + row * i as i32, w - s.px(12), row);
            let color = self.settings.colors[*c as usize];
            if self.editor.active_class == *c {
                p.fill_rect(r, 0x0033_3a44);
            }
            p.fill_rect(
                Rect::new(
                    r.x + s.px(6),
                    r.y + (row - s.px(10)) / 2,
                    s.px(10),
                    s.px(10),
                ),
                color,
            );
            self.text
                .draw(p, key, r.x + s.px(24), r.y + (row - lh) / 2, st, HUD_DIM);
            self.text.draw(
                p,
                class(*c).name,
                r.x + s.px(90),
                r.y + (row - lh) / 2,
                st,
                HUD_TEXT,
            );
            self.popup_hits.push((r, *c));
        }
        self.popup_rect = Some(panel);
    }

    fn draw_help(&mut self, p: &mut Painter<'_>, w: i32, h: i32) {
        let s = self.scale;
        let st = self.style(13.0);
        let lh = self.text.line_height(st) + s.px(4);
        let lines: &[(&str, &str)] = &[
            (
                "Right / Shift+Right",
                "accept page and advance / skip without accepting",
            ),
            ("Left", "previous page (status unchanged)"),
            (
                "1–9, Shift+1–5",
                "label for new boxes; reclassify the selected box (S1 = Shift+1)",
            ),
            (
                "Drag on blank page",
                "draw a new box (Shift+drag: draw even over a box)",
            ),
            (
                "Drag a box's edge",
                "resize that corner (left half of top edge = top-left, …)",
            ),
            ("Alt + edge drag", "move only one edge"),
            ("Drag inside a box", "move it"),
            ("Wheel, + / -", "zoom around the cursor"),
            ("Space+drag, middle drag", "pan"),
            ("F / Ctrl+1", "fit page / actual pixels"),
            ("Ctrl+Z / Ctrl+Y", "undo / redo (one gesture each)"),
            (
                "Ctrl+C",
                "revert page: to how you opened it, again: to the imported boxes",
            ),
            ("Delete", "delete selected box"),
            ("U", "toggle selected box as ignore region"),
            ("X / N", "exclude page / flag needs attention"),
            ("Tab / Shift+Tab", "select next / previous box"),
            ("Ctrl+arrows (+Shift)", "nudge selected box 1 px (resize)"),
            ("V / T / H", "outlines / class tags / toolbars"),
            ("C", "compare: imported, model predictions, differences"),
            (
                "Q",
                "queue: all, to do, needs attention, excluded, reviewed",
            ),
            ("Home, PgUp/PgDn", "first unreviewed page, jump 10"),
            ("Escape", "cancel the current drag, then clear selection"),
            ("F11 / F12", "full screen / performance overlay"),
        ];
        let pw = s.px(640).min(w - s.px(40));
        let ph = lh * (lines.len() as i32 + 2) + s.px(16);
        let panel = Rect::new((w - pw) / 2, (h - ph) / 2, pw, ph);
        p.blend_rect(Rect::new(0, 0, w, h), 0x000000, 120);
        p.fill_rect(panel, HUD_BG);
        outline(p, panel, HUD_EDGE, 1);
        let mut y = panel.y + s.px(10);
        self.text
            .draw(p, "Keyboard and mouse", panel.x + s.px(16), y, st, HUD_TEXT);
        y += lh + s.px(4);
        for (k, v) in lines {
            self.text.draw(p, k, panel.x + s.px(16), y, st, ACCENT);
            let area = Rect::new(panel.x + s.px(210), y, pw - s.px(226), lh);
            self.text
                .draw_fitted(p, v, area, y, st, HUD_TEXT, Align::Left);
            y += lh;
        }
    }

    fn draw_perf(&mut self, p: &mut Painter<'_>, w: i32) {
        let s = self.scale;
        let st = self.style(11.5);
        let lh = self.text.line_height(st);
        let f = &self.perf;
        let lines = [
            format!(
                "frame   p50 {:.2}  p95 {:.2}  max {:.2} ms",
                percentile(&f.frame_ms, 0.5),
                percentile(&f.frame_ms, 0.95),
                percentile(&f.frame_ms, 1.0)
            ),
            format!(
                "raster  p50 {:.2}  p95 {:.2} ms  ({} renders)",
                percentile(&f.raster_ms, 0.5),
                percentile(&f.raster_ms, 0.95),
                f.raster_renders
            ),
            format!(
                "switch  p50 {:.1}  p95 {:.1}  max {:.1} ms",
                percentile(&f.switch_ms, 0.5),
                percentile(&f.switch_ms, 0.95),
                percentile(&f.switch_ms, 1.0)
            ),
            format!(
                "decode  p50 {:.1}  p95 {:.1} ms",
                percentile(&f.decode_ms, 0.5),
                percentile(&f.decode_ms, 0.95)
            ),
            format!(
                "cache   {} pages, {:.0} MiB",
                self.cache.len(),
                self.cache.used_bytes() as f64 / 1048576.0
            ),
        ];
        let pw = s.px(360);
        let r = Rect::new(
            w - pw - s.px(10),
            self.canvas.y as i32 + s.px(10),
            pw,
            lh * lines.len() as i32 + s.px(10),
        );
        p.blend_rect(r, 0x000000, 190);
        for (i, l) in lines.iter().enumerate() {
            self.text.draw(
                p,
                l,
                r.x + s.px(8),
                r.y + s.px(5) + lh * i as i32,
                st,
                0x00a0_ffa0,
            );
        }
    }

    /// Commit and stop background threads; called on exit.
    pub fn shutdown(&mut self) {
        self.commit("exit");
        self.writer.shutdown();
        if self.perf_log {
            let f = &self.perf;
            eprintln!(
                "[perf] frames {} | frame p50 {:.2} p95 {:.2} ms | raster p50 {:.2} p95 {:.2} ms | switch p50 {:.1} p95 {:.1} ms",
                f.frames,
                percentile(&f.frame_ms, 0.5),
                percentile(&f.frame_ms, 0.95),
                percentile(&f.raster_ms, 0.5),
                percentile(&f.raster_ms, 0.95),
                percentile(&f.switch_ms, 0.5),
                percentile(&f.switch_ms, 0.95)
            );
        }
    }

    /// Send the current page to the writer now (tests).
    pub fn commit_now(&mut self) {
        self.commit("edit");
    }

    /// Fit the page now (bench, tests).
    pub fn act_fit_for_bench(&mut self) {
        self.set_fit();
    }

    /// Drop the cached page raster and schedule a redraw, so the next frame
    /// is a full render (tests).
    pub fn invalidate_raster_for_test(&mut self) {
        self.raster.key = None;
        self.dirty = true;
    }

    /// Run a keyboard action as if its key were pressed (tests).
    pub fn run_action(&mut self, action: Action) {
        self.act(action);
    }

    pub fn class_color(&self, c: u8) -> u32 {
        self.settings.colors[(c as usize).min(NUM_CLASSES - 1)]
    }

    pub fn current_key(&self) -> Option<&str> {
        self.keys.get(self.pos).map(String::as_str)
    }

    /// Feed a pointer event as the host would (tests).
    pub fn inject_cursor(&mut self, x: f64, y: f64) {
        self.on_cursor(x as f32, y as f32);
    }
}

impl PixelApp for AnnotatorApp {
    fn render(&mut self, buffer: &mut WindowBuffer, scale: Scale) {
        if scale != self.scale {
            self.dirty = true;
            self.scale = scale;
            self.editor.params = Params::for_dpi(scale.0 as f64, self.settings.raw.hit_slop);
            self.canvas = CanvasRect {
                x: -1.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            }; // force relayout
        }
        self.poll_background();
        self.render_frame(buffer);
    }

    fn on_scale(&mut self, scale: Scale) {
        self.scale = scale;
        self.editor.params = Params::for_dpi(scale.0 as f64, self.settings.raw.hit_slop);
        self.raster.key = None;
    }

    fn attach(&mut self, waker: Waker) {
        if let Ok(mut w) = self.wake_slot.lock() {
            *w = Some(waker);
        }
    }

    fn animation_interval(&self) -> Option<Duration> {
        // Only while a transient message is fading out or a page is loading.
        let msg = self
            .message
            .as_ref()
            .is_some_and(|(_, at, _)| at.elapsed() < Duration::from_secs(9));
        if msg {
            Some(Duration::from_millis(500))
        } else {
            None
        }
    }

    fn tick(&mut self) {
        self.poll_background();
    }

    fn on_key_event(&mut self, event: &KeyEvent) {
        if let Some(action) = map_key(event, &self.settings) {
            self.act(action);
        }
    }

    fn on_focus_changed(&mut self, focused: bool) {
        self.dirty = true;
        if !focused {
            self.editor.space_held = false;
            self.pointer.cancel_all();
        }
    }

    fn poll_title(&mut self) -> Option<String> {
        self.title.take()
    }

    fn poll_fullscreen(&mut self) -> Option<bool> {
        self.fullscreen.take()
    }

    fn cursor_shape(&self) -> CursorShape {
        self.cursor()
    }

    fn on_exit(&mut self) {
        self.shutdown();
    }

    fn on_host_error(&mut self, error: &str) {
        eprintln!("liminal-annotator: host error: {error}");
        self.shutdown();
    }

    fn on_modifiers(&mut self, modifiers: Modifiers) {
        self.pointer.set_modifiers(modifiers);
    }

    fn on_cursor(&mut self, x: f32, y: f32) {
        self.pointer.moved(x as f64, y as f64);
    }

    fn on_cursor_left(&mut self) {
        self.pointer.left();
        if self.capture.owner().is_none() && self.editor.hover.take().is_some() {
            self.dirty = true;
        }
    }

    fn on_mouse(&mut self, button: MouseButton, pressed: bool) {
        self.pointer.button(button, pressed);
    }

    fn on_scroll_event(&mut self, event: &ScrollEvent) {
        let step = self.settings.raw.wheel_zoom_step;
        let notches = match event.line_dy {
            Some(l) => l as f64,
            None => event.pixel_dy as f64 / 60.0,
        };
        if notches == 0.0 {
            return;
        }
        let (x, y) = self.pointer.position();
        self.zoom_by(step.powf(notches), Some(Point::new(x, y)));
    }

    fn should_exit(&self) -> bool {
        self.exit
    }
}

/// Run the annotator window.
pub fn run(config: AppConfig) -> Result<(), String> {
    let app = AnnotatorApp::new(config)?;
    let wc = liminalkit_windowing::WindowConfig::new("Liminal Annotator", 1360.0, 900.0)
        .min_size(720.0, 480.0);
    liminalkit_windowing::run_app(app, wc).map_err(|e| e.to_string())
}
