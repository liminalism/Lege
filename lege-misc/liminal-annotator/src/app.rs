//! The annotator window: a [`PixelApp`] that ties the canvas, the editor
//! state machine, the project database, the writer and the image loader
//! together.
//!
//! Frame structure: drain the ordered pointer queue (routing through
//! capture), refit the view if needed, re-render the page raster only if the
//! page/zoom/pan/canvas changed, copy it into the window buffer, then draw
//! annotation outlines and the HUD on top.

use std::collections::{HashMap, HashSet, VecDeque};
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
use crate::annotation::labels::{ABANDON, IGNORE_COLOR, NUM_LABELS, class};
use crate::books::{BooksAction, BooksView};
use crate::canvas::draw::{
    CANVAS_BG, PageRaster, RasterKey, blit_raster, dashed_outline, outline, pixel_rect,
    render_page, scroll_page, text_on,
};
use crate::canvas::interaction::{Editor, Outcome, Params, ToolState};
use crate::canvas::pointer::{PointerCapture, PointerEvent, PointerPhase, PointerQueue};
use crate::canvas::shortcuts::{Action, map_key};
use crate::canvas::viewport::{CanvasRect, ViewTransform, ZoomMode};
use crate::convert::{ConvertAction, ConvertDialog, Scope};
use crate::dataset::database::{
    self, PageFilter, PageMeta, PageSave, Prediction, Region, WriteAck, Writer,
};
use crate::fonts;
use crate::grid::{self, CellImage, GridView};
use crate::image_io::cache::ImageCache;
use crate::image_io::decode::PageImage;
use crate::image_io::prefetch::{Job, Loaded, Loader};
use crate::queues::{QueueAction, QueuePicker};
use crate::review::comparison::{Change, PageDiff, diff};
use crate::settings::{Resolved, Settings};
use crate::similar::{SimilarAction, SimilarDialog};

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

/// Something only the window shell can do (it owns the folder dialog and
/// the import thread); the editor leaves it here and the shell picks it up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRequest {
    OpenFolder,
    Rescan,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ToolbarItem {
    Books,
    Class(u8),
    More,
    Compare,
    Revert,
    Similar,
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

/// Prefilled boxes scored below this are drawn dashed until edited.
pub const LOW_CONFIDENCE: f64 = 0.5;

/// Pages accepted per hour of work, this session.
#[derive(Clone, Debug, Default)]
pub struct Pace {
    last: Option<Instant>,
    /// Accepts that followed another within five minutes, and the time
    /// between them.
    steps: u32,
    active: Duration,
}

impl Pace {
    const BREAK: Duration = Duration::from_secs(300);

    pub fn accepted(&mut self, now: Instant) {
        self.accepted_n(now, 1);
    }

    /// `n` pages accepted at once (the six-page view): the time since the
    /// last accept was spent on all of them.
    pub fn accepted_n(&mut self, now: Instant, n: u32) {
        if n == 0 {
            return;
        }
        if let Some(last) = self.last {
            let gap = now.saturating_duration_since(last);
            if gap <= Self::BREAK {
                self.steps += n;
                self.active += gap;
            }
        }
        self.last = Some(now);
    }

    /// Pages per hour, once there are a few to go on.
    pub fn per_hour(&self) -> Option<f64> {
        (self.steps >= 3 && !self.active.is_zero())
            .then(|| self.steps as f64 * 3600.0 / self.active.as_secs_f64())
    }
}

/// The status bar's word on a page after X or N.
fn flag_message(after: ReviewStatus) -> &'static str {
    match after {
        ReviewStatus::Excluded => "Page excluded (X restores)",
        ReviewStatus::NeedsAttention => "Page flagged: needs attention (N clears)",
        ReviewStatus::Reviewed => "Page restored: reviewed",
        _ => "Page restored: to do",
    }
}

/// The comparison model's score of an imported box, from its provenance:
/// PP's score for a box that took PP's label, else the prefill score.
fn prefill_score(prov: Option<&str>) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(prov?).ok()?;
    v.get("label_score")
        .and_then(serde_json::Value::as_f64)
        .or_else(|| v.get("score").and_then(serde_json::Value::as_f64))
}

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
    /// The label chips need their own toolbar row.
    toolbar_two_rows: bool,
    /// Paper-tone measurement for the book manager, while it runs.
    tone_job: Option<crate::dataset::tone::ToneJob>,
    /// The book manager, when it is showing (it covers the editor).
    books: Option<BooksView>,
    /// The bulk-convert dialog, when open.
    convert: Option<ConvertDialog>,
    /// The "same on every page" dialog, when open.
    similar: Option<SimilarDialog>,
    /// Flags of the book of the page shown.
    book_flags: database::BookFlags,
    /// The queue picker (Q), when open.
    queues: Option<QueuePicker>,
    /// The page whose Right was refused because `abandon` boxes are left: a
    /// second Right on it moves on without marking it reviewed.
    abandon_warned: Option<String>,
    /// A click on a comparison box that, if it ends without a drag, copies
    /// the box into the editable layer: (press position, the box).
    adopt: Option<(Point, Annotation)>,
    /// Imported boxes the prefill model was unsure of (score below
    /// [`LOW_CONFIDENCE`]), by id: drawn dashed while unchanged.
    low_conf: HashMap<String, (u8, Box2)>,
    /// Review pace: pages accepted with Right and the active time between
    /// them (gaps over five minutes are breaks, not work).
    pace: Pace,
    pub request: Option<ShellRequest>,
    /// The six-page view, while it shows (it covers the editor).
    grid: Option<GridView>,
    /// The editor was entered from the six-page view: G or Escape go back.
    grid_excursion: bool,
    /// Pages shown in the editor since it was entered from the view.
    grid_visited: Vec<String>,
    /// Pages opened from the view and left without accepting (this
    /// session): Right in the view leaves them to do.
    grid_held: HashSet<String>,
    /// Pages whose image failed to decode, with the error.
    failed_images: HashMap<String, String>,
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
        let mut config = config;
        if config.filter.large_figure_share.is_none() {
            config.filter.large_figure_share = Some(settings.raw.large_figure_share());
        }
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
            toolbar_two_rows: false,
            tone_job: None,
            books: None,
            convert: None,
            similar: None,
            book_flags: database::BookFlags::default(),
            queues: None,
            abandon_warned: None,
            adopt: None,
            low_conf: HashMap::new(),
            pace: Pace::default(),
            request: None,
            grid: None,
            grid_excursion: false,
            grid_visited: Vec::new(),
            grid_held: HashSet::new(),
            failed_images: HashMap::new(),
        };
        app.editor.active_class = app.settings.digit[0].unwrap_or(0);
        if app.filter.queue == Some(database::Queue::ModelOnlyPictures) && !app.models.is_empty() {
            app.compare = Compare::Prediction(0);
        }
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

    // --- books and bulk conversion -------------------------------------------

    /// Show the book manager over the editor.
    pub fn show_books(&mut self) {
        if self.editor.is_busy() {
            self.editor.cancel();
        }
        // Statuses in the database lag the open page by one commit.
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        let stats = database::book_stats(&self.conn).unwrap_or_else(|e| {
            self.flash(&e, true);
            Vec::new()
        });
        let mut view = BooksView::new(
            stats,
            self.settings.raw.abandon_flag_per_page,
            self.settings.raw.abandon_flag_min_boxes,
            self.filter.book_id.clone(),
            self.project_label(),
        );
        view.can_close = self.doc.is_some() || self.grid.is_some();
        view.split_names = database::split_names(&self.conn).unwrap_or_default();
        if self.tone_job.is_none() {
            let ws = self.wake_slot.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if let Ok(w) = ws.lock() {
                    if let Some(w) = w.as_ref() {
                        w.wake();
                    }
                }
            });
            let threads = std::thread::available_parallelism()
                .map(|n| n.get() / 2)
                .unwrap_or(2)
                .clamp(1, 6);
            match crate::dataset::tone::ToneJob::start(&self.project_path, threads, wake) {
                Ok(job) => self.tone_job = job,
                Err(e) => self.flash(&format!("paper tone: {e}"), true),
            }
        }
        view.paper_progress = self.tone_job.as_ref().map(|j| (j.done(), j.total));
        self.books = Some(view);
        self.convert = None;
        self.dirty = true;
        self.title = Some(format!(
            "Liminal Annotator — books — {}",
            self.project_label()
        ));
    }

    pub fn books_showing(&self) -> bool {
        self.books.is_some()
    }

    /// The project file, and the folder it was made from when that differs.
    pub fn project_label(&self) -> String {
        match crate::dataset::import_folder::project_root(&self.conn) {
            Some(root) => format!("{}  ·  {}", root.display(), self.project_path.display()),
            None => self.project_path.display().to_string(),
        }
    }

    /// The dataset folder this project was made from, if any.
    pub fn project_root(&self) -> Option<PathBuf> {
        crate::dataset::import_folder::project_root(&self.conn)
    }

    /// The book last opened in this project, if any.
    pub fn last_book(&self) -> Option<String> {
        database::meta_get(&self.conn, "last_book")
            .ok()
            .flatten()
            .filter(|b| !b.is_empty())
    }

    /// Make `book` (or every book) the queue and open its first page to do.
    pub fn open_book(&mut self, book: Option<String>) {
        if self.editor.is_busy() {
            self.editor.cancel();
        }
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        self.filter.book_id = book.clone();
        self.filter.has_class = None;
        if self.filter.queue.is_none() {
            self.filter.statuses = self.base_statuses.clone();
        }
        self.keys = database::page_keys(&self.conn, &self.filter).unwrap_or_default();
        if self.keys.is_empty() && self.filter.queue.is_some() {
            // The queue picked has nothing in this book: show all of it.
            self.filter.queue = None;
            self.filter.statuses = self.base_statuses.clone();
            self.keys = database::page_keys(&self.conn, &self.filter).unwrap_or_default();
        }
        self.refresh_counts();
        let _ = database::meta_set(&self.conn, "last_book", book.as_deref().unwrap_or(""));
        self.books = None;
        self.convert = None;
        self.dirty = true;
        self.stash_current();
        if self.keys.is_empty() {
            self.meta = None;
            self.image = ImageState::None;
            self.grid_goto(0);
            self.update_title();
            self.flash("No pages in this book", true);
            return;
        }
        let pos = self.first_unreviewed().unwrap_or(0);
        if self.grid.is_some() {
            self.grid_goto(pos);
        } else {
            self.open_page(pos);
        }
        let todo = self.keys.len().saturating_sub(self.reviewed_in_queue);
        let name = book.as_deref().unwrap_or("All books");
        let msg = match self.filter.queue {
            Some(q) => format!(
                "{name}: {} pages in \"{}\" · Q: other queues · B: books",
                self.keys.len(),
                q.label()
            ),
            None => format!(
                "{name}: {} pages, {todo} to do · Q: queues · B: books",
                self.keys.len()
            ),
        };
        self.flash(&msg, false);
    }

    fn handle_books(&mut self, action: BooksAction) {
        match action {
            BooksAction::None => {}
            BooksAction::Redraw => self.dirty = true,
            BooksAction::Open(book) => self.open_book(book),
            BooksAction::OpenFolder => self.request = Some(ShellRequest::OpenFolder),
            BooksAction::Rescan => self.request = Some(ShellRequest::Rescan),
            BooksAction::SetFlags(book, flags) => {
                match database::set_book_flags(&self.conn, &book, flags) {
                    Ok(()) => {
                        if self.meta.as_ref().is_some_and(|m| m.book_id == book) {
                            self.book_flags = flags;
                        }
                    }
                    Err(e) => self.flash(&e, true),
                }
                self.dirty = true;
            }
            BooksAction::SetSplit(book, split) => {
                match database::set_book_split(&self.conn, &book, &split) {
                    Ok(n) => {
                        if let Some(v) = &mut self.books {
                            v.say(format!("{book}: {n} pages moved to split \"{split}\""));
                        }
                        if let Some(m) = self.meta.as_mut().filter(|m| m.book_id == book) {
                            m.split = split;
                        }
                    }
                    Err(e) => {
                        if let Some(v) = &mut self.books {
                            v.say(e);
                        }
                    }
                }
                self.dirty = true;
            }
            BooksAction::AssignSplits => {
                let plan = database::SplitPlan::default();
                match database::assign_splits(&mut self.conn, &plan, false) {
                    Ok(moved) => {
                        let count = |s: &str| moved.iter().filter(|(_, x)| x == s).count();
                        let msg = format!(
                            "{} books given a split: {} train, {} dev, {} final_test",
                            moved.len(),
                            count(&plan.train),
                            count(&plan.dev.0),
                            count(&plan.test.0)
                        );
                        let stats = database::book_stats(&self.conn).unwrap_or_default();
                        if let Some(m) = self.meta.as_mut() {
                            if let Some((_, s)) = moved.iter().find(|(b, _)| *b == m.book_id) {
                                m.split = s.clone();
                            }
                        }
                        if let Some(v) = &mut self.books {
                            v.update_stats(stats);
                            v.split_names = database::split_names(&self.conn).unwrap_or_default();
                            v.say(msg);
                        }
                    }
                    Err(e) => {
                        if let Some(v) = &mut self.books {
                            v.say(e);
                        }
                    }
                }
                self.dirty = true;
            }
            BooksAction::Close => {
                self.books = None;
                self.dirty = true;
                self.update_title();
            }
        }
    }

    /// `A`: select the next `abandon` box on the page.
    fn select_next_abandon(&mut self) {
        let Some(doc) = &self.doc else { return };
        let ids: Vec<String> = doc
            .annotations
            .iter()
            .filter(|a| a.class_id == ABANDON)
            .map(|a| a.id.clone())
            .collect();
        if ids.is_empty() {
            self.flash(
                "No abandon boxes on this page · Shift+A: convert in bulk · Q: queue of pages with abandon",
                false,
            );
            return;
        }
        let next = self
            .editor
            .selected
            .as_ref()
            .and_then(|s| ids.iter().position(|i| i == s))
            .map_or(0, |p| (p + 1) % ids.len());
        self.editor.selected = Some(ids[next].clone());
        let msg = format!(
            "Abandon box {} of {} selected: a class key converts it · A: next",
            next + 1,
            ids.len()
        );
        self.flash(&msg, false);
    }

    /// The queue's scope without its status and class parts, as a label.
    fn scope_label(&self) -> String {
        if let Some(b) = &self.filter.book_id {
            return format!("Book {b}");
        }
        let mut f = self.filter.clone();
        f.statuses.clear();
        f.has_class = None;
        f.by_priority = false;
        format!("Queue: {}", f.describe())
    }

    /// `Shift+A`: the bulk-convert dialog, with counts for this page and the
    /// queue's scope.
    pub fn open_convert(&mut self) {
        if self.editor.is_busy() {
            self.editor.cancel();
        }
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        let page_counts: Vec<i64> = match &self.doc {
            Some(doc) => Region::ALL
                .iter()
                .map(|r| {
                    doc.annotations
                        .iter()
                        .filter(|a| {
                            a.class_id == ABANDON && r.contains(&a.bbox, doc.width, doc.height)
                        })
                        .count() as i64
                })
                .collect(),
            None => vec![0; Region::ALL.len()],
        };
        let queue_counts = match database::count_by_region(&self.conn, &self.filter, ABANDON) {
            Ok(c) => c.into_iter().map(|(_, n, p)| (n, p)).collect(),
            Err(e) => {
                self.flash(&e, true);
                return;
            }
        };
        let last = database::last_bulk_op(&self.conn).ok().flatten();
        self.convert = Some(ConvertDialog::new(
            page_counts,
            queue_counts,
            self.scope_label(),
            last,
        ));
        self.dirty = true;
    }

    /// Convert the dialog's selection of `abandon` boxes to class `to`.
    fn convert_to(&mut self, to: u8) {
        let Some(dlg) = &self.convert else { return };
        let (region, scope) = (dlg.region(), dlg.scope);
        if dlg.selected_boxes() == 0 {
            self.flash("No abandon boxes there to convert", false);
            return;
        }
        match scope {
            Scope::Page => {
                let Some(doc) = self.doc.as_mut() else { return };
                let (w, h) = (doc.width, doc.height);
                let before = doc.annotations.clone();
                let mut after = before.clone();
                let mut n = 0;
                for a in &mut after {
                    if a.class_id == ABANDON && region.contains(&a.bbox, w, h) {
                        a.class_id = to;
                        n += 1;
                    }
                }
                doc.apply(Edit::Replace { before, after });
                self.editor.reconcile(doc);
                self.commit("convert");
                self.convert = None;
                let msg = format!(
                    "Converted {n} abandon boxes on this page to {}. Ctrl+Z undoes it.",
                    class(to).name
                );
                self.flash(&msg, false);
            }
            Scope::Queue => {
                self.commit("navigate");
                self.writer.flush();
                self.poll_background();
                match database::convert_class(&mut self.conn, &self.filter, region, ABANDON, to) {
                    Ok(r) => {
                        self.reload_after_bulk();
                        self.convert = None;
                        let msg = format!(
                            "Converted {} abandon boxes ({}) on {} pages to {}. Shift+A, then U, undoes it.",
                            r.boxes,
                            region.label(),
                            r.pages,
                            class(to).name
                        );
                        self.flash(&msg, false);
                    }
                    Err(e) => self.flash(&e, true),
                }
            }
        }
    }

    fn undo_bulk(&mut self, op_id: i64) {
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        match database::undo_bulk(&mut self.conn, op_id) {
            Ok(n) => {
                self.reload_after_bulk();
                self.convert = None;
                self.flash(
                    &format!("Bulk conversion undone: {n} boxes restored"),
                    false,
                );
            }
            Err(e) => self.flash(&e, true),
        }
    }

    /// After a bulk change in the database every save has been acknowledged
    /// (the writer was flushed first), so the in-memory documents are
    /// dropped and the pages reload from the database. Per-page undo
    /// history before the bulk change is gone; the bulk change has its own.
    fn reload_after_bulk(&mut self) {
        let current = self.current_key().map(str::to_owned);
        self.doc = None;
        self.docs.clear();
        self.recent.clear();
        self.sent.clear();
        self.acked.clear();
        self.keys = database::page_keys(&self.conn, &self.filter).unwrap_or_default();
        self.refresh_counts();
        if !self.keys.is_empty() {
            let pos = current
                .and_then(|k| self.keys.iter().position(|x| *x == k))
                .unwrap_or(self.pos.min(self.keys.len() - 1));
            if self.grid.is_some() {
                self.grid_goto(pos);
            } else {
                self.open_page(pos);
            }
        } else {
            self.meta = None;
            self.image = ImageState::None;
        }
        if self.books.is_some() {
            let stats = database::book_stats(&self.conn).unwrap_or_default();
            if let Some(b) = &mut self.books {
                b.update_stats(stats);
            }
        }
        self.dirty = true;
    }

    /// `M`: every box of the book in the selected box's place.
    pub fn open_similar(&mut self) {
        if self.editor.is_busy() {
            self.editor.cancel();
        }
        let (Some(doc), Some(meta)) = (&self.doc, &self.meta) else {
            return;
        };
        let Some(t) = self
            .editor
            .selected
            .as_ref()
            .and_then(|id| doc.get(id))
            .cloned()
        else {
            self.flash(
                "Select a running head, heading or page number first (click it or Tab), then M",
                false,
            );
            return;
        };
        let (book, w, h) = (meta.book_id.clone(), doc.width, doc.height);
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        let pages: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pages WHERE book_id = ?1",
                [&book],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut d = SimilarDialog::new(
            book,
            t.bbox,
            t.class_id,
            w,
            h,
            Vec::new(),
            pages,
            database::last_bulk_op(&self.conn).ok().flatten(),
        );
        self.find_similar(&mut d, database::Tolerance::Normal);
        self.similar = Some(d);
        self.dirty = true;
    }

    fn find_similar(&mut self, d: &mut SimilarDialog, tol: database::Tolerance) {
        let mut classes = database::FURNITURE_FAMILY.to_vec();
        if !classes.contains(&d.template_class) {
            classes.push(d.template_class);
        }
        d.tolerance = tol;
        match database::similar_boxes(
            &self.conn,
            &d.book,
            &d.template,
            d.page_w,
            d.page_h,
            &classes,
            tol,
        ) {
            Ok(m) => d.matches = m,
            Err(e) => self.flash(&e, true),
        }
    }

    fn handle_similar(&mut self, a: SimilarAction) {
        match a {
            SimilarAction::None => {}
            SimilarAction::Redraw => self.dirty = true,
            SimilarAction::Retolerance(t) => {
                if let Some(mut d) = self.similar.take() {
                    self.find_similar(&mut d, t);
                    self.similar = Some(d);
                }
                self.dirty = true;
            }
            SimilarAction::ApplyTemplateClass => {
                if let Some(c) = self.similar.as_ref().map(|d| d.template_class) {
                    self.similar_apply(c);
                }
            }
            SimilarAction::Undo(id) => {
                self.similar = None;
                self.undo_bulk(id);
            }
            SimilarAction::Close => {
                self.similar = None;
                self.dirty = true;
            }
        }
    }

    /// Make every match of the dialog class `to`, as one logged change.
    fn similar_apply(&mut self, to: u8) {
        let Some(d) = self.similar.take() else { return };
        let changes: Vec<database::ClassChange> = d
            .matches
            .iter()
            .filter(|m| m.class_id != to)
            .map(|m| database::ClassChange {
                id: m.id.clone(),
                page_key: m.page_key.clone(),
                from: m.class_id,
                to,
            })
            .collect();
        if changes.is_empty() {
            self.flash(&format!("Every match is already {}", class(to).name), false);
            return;
        }
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        let desc = format!(
            "same place as the selected {} ({} match): {} to {} · book {}",
            class(d.template_class).name,
            d.tolerance.label(),
            changes.len(),
            class(to).name,
            d.book
        );
        match database::apply_class_changes(&mut self.conn, &desc, &changes) {
            Ok(r) => {
                self.reload_after_bulk();
                let msg = format!(
                    "{} boxes on {} pages are now {}. M, then U, undoes it.",
                    r.boxes,
                    r.pages,
                    class(to).name
                );
                self.flash(&msg, false);
            }
            Err(e) => self.flash(&e, true),
        }
    }

    /// Show a message in the status bar (the shell reports imports here).
    pub fn notify(&mut self, msg: &str, error: bool) {
        self.flash(msg, error);
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
        if let Some(g) = &self.grid {
            let n = self.keys.len();
            self.title = Some(format!(
                "Liminal Annotator — {} — pages {}–{} of {}",
                self.filter.describe(),
                g.start + 1,
                (g.start + grid::PER_VIEW).min(n),
                n
            ));
            return;
        }
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
        self.abandon_warned = None;
        let pos = pos.min(self.keys.len() - 1);
        self.pos = pos;
        let key = self.keys[pos].clone();
        if self.grid_excursion && !self.grid_visited.contains(&key) {
            self.grid_visited.push(key.clone());
        }
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
            None => self.fresh_doc(&key, &meta),
        };
        self.imported = database::imported_annotations(&self.conn, &key).unwrap_or_default();
        self.low_conf = self
            .imported
            .iter()
            .filter(|a| {
                prefill_score(a.provenance_json.as_deref()).is_some_and(|s| s < LOW_CONFIDENCE)
            })
            .map(|a| (a.id.clone(), (a.class_id, a.bbox)))
            .collect();
        self.adopt = None;
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
        self.book_flags = database::get_book_flags(&self.conn, &meta.book_id).unwrap_or_default();
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

    /// A page's document read from the database. Its revision continues from
    /// any earlier document of the page this session: `commit` skips a
    /// revision already sent, so a page reloaded after its document was
    /// dropped from memory must not start again at 0.
    fn fresh_doc(&self, key: &str, meta: &PageMeta) -> PageDoc {
        let anns = database::annotations(&self.conn, key).unwrap_or_default();
        let mut doc = PageDoc::new(
            key.to_owned(),
            meta.width as f64,
            meta.height as f64,
            anns,
            meta.status,
        );
        doc.revision = self
            .sent
            .get(key)
            .copied()
            .unwrap_or(0)
            .max(self.acked.get(key).copied().unwrap_or(0));
        doc
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

    /// Queue positions to keep decoded, most wanted first: the page and its
    /// neighbours, or in the six-page view the six shown, the next six and
    /// the six before.
    fn wanted_positions(&self) -> Vec<usize> {
        let n = self.keys.len();
        if let Some(g) = &self.grid {
            let s = g.start;
            let k = grid::PER_VIEW;
            return (s..s + 2 * k)
                .chain(s.saturating_sub(k)..s)
                .filter(|&p| p < n)
                .collect();
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
        order
    }

    fn request_images(&mut self) {
        let n = self.keys.len();
        if n == 0 {
            return;
        }
        let order = self.wanted_positions();
        let mut jobs = Vec::new();
        for p in order {
            let key = &self.keys[p];
            if self.cache.contains(key) {
                continue;
            }
            let path = if p == self.pos && self.grid.is_none() {
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
        if self.grid.is_some() {
            return self
                .wanted_positions()
                .into_iter()
                .filter_map(|i| self.keys.get(i).cloned())
                .collect();
        }
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
            let mut refused = false;
            if let Some(doc) = &mut self.doc {
                let abandon = doc
                    .annotations
                    .iter()
                    .filter(|a| a.class_id == ABANDON)
                    .count();
                let todo = matches!(doc.status, ReviewStatus::Unseen | ReviewStatus::Opened);
                if !loaded {
                    let msg = format!("{}: image did not load — not marked reviewed", doc.page_key);
                    self.flash(&msg, true);
                } else if todo && abandon > 0 {
                    // Export skips a page with abandon boxes, so marking it
                    // reviewed would lose it quietly.
                    if self.abandon_warned.as_deref() == Some(doc.page_key.as_str()) {
                        let msg = format!(
                            "{}: left to do ({abandon} abandon boxes) — not marked reviewed",
                            doc.page_key
                        );
                        self.flash(&msg, true);
                    } else {
                        self.abandon_warned = Some(doc.page_key.clone());
                        refused = true;
                    }
                } else if todo {
                    doc.set_status_untracked(ReviewStatus::Reviewed);
                    self.reviewed_in_queue += 1;
                    self.pace.accepted(Instant::now());
                }
            }
            if refused {
                let n = self.doc.as_ref().map_or(0, |d| {
                    d.annotations
                        .iter()
                        .filter(|a| a.class_id == ABANDON)
                        .count()
                });
                self.flash(
                    &format!(
                        "{n} abandon box{} left: export skips this page. A selects the next, a class key labels it · \
                         Right again: move on without marking it reviewed",
                        if n == 1 { "" } else { "es" }
                    ),
                    true,
                );
                return;
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
        if let Some(job) = &self.tone_job {
            let mut got = Vec::new();
            while let Ok(r) = job.results.try_recv() {
                got.push(r);
            }
            let progress = (job.done(), job.total);
            let finished = job.finished();
            if let Some(b) = &mut self.books {
                for (book, tone) in got {
                    b.set_tone(&book, tone);
                }
                b.paper_progress = (!finished).then_some(progress);
                self.dirty = true;
            }
            if finished {
                self.tone_job = None;
            }
        }
        while let Ok(loaded) = self.loader.results.try_recv() {
            self.take_loaded(loaded);
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

    /// A decoded (or failed) page from the loader: into the cache, and onto
    /// the screen when it is the page shown.
    fn take_loaded(&mut self, loaded: Loaded) {
        self.dirty = true;
        push_ring(
            &mut self.perf.decode_ms,
            loaded.decode_time.as_secs_f64() * 1000.0,
        );
        let is_current = self.grid.is_none()
            && self
                .keys
                .get(self.pos)
                .is_some_and(|k| *k == loaded.page_key);
        match loaded.result {
            Ok(img) => {
                self.failed_images.remove(&loaded.page_key);
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
                    self.image = ImageState::Failed(e.clone());
                }
                self.failed_images.insert(loaded.page_key, e);
            }
        }
    }

    /// Block until the current page's image is resolved, or in the six-page
    /// view every image shown (tests, bench, screenshots).
    pub fn wait_for_image(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.poll_background();
            if self.grid.is_some() {
                let images = self.grid_images();
                if !images.iter().any(|i| matches!(i, CellImage::Loading)) {
                    return images.iter().all(|i| matches!(i, CellImage::Ready(_)));
                }
            } else if !matches!(self.image, ImageState::Loading) {
                return matches!(self.image, ImageState::Ready(_));
            }
            if Instant::now() > deadline {
                return false;
            }
            if let Some(l) = self.loader.wait(Duration::from_millis(20)) {
                // Put it back through the normal path.
                self.take_loaded(l);
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
        if ev.phase == PointerPhase::Down {
            self.adopt = if image_ok {
                self.adopt_candidate(ev)
            } else {
                None
            };
        }
        let Some(doc) = self.doc.as_mut() else { return };
        let out = self
            .editor
            .pointer(ev, doc, &mut self.view, self.canvas, image_ok);
        if out.redraw || out.view_changed || out.edited {
            self.dirty = true;
        }
        self.after_edit(out);
        match ev.phase {
            PointerPhase::Up => {
                if let Some((start, ann)) = self.adopt.take() {
                    let moved = (ev.position.0 - start.x).hypot(ev.position.1 - start.y);
                    if !out.edited && moved <= self.editor.params.slop {
                        self.adopt_box(ann);
                    }
                }
            }
            PointerPhase::Cancel => self.adopt = None,
            _ => {}
        }
    }

    /// The comparison layer's boxes, as boxes one could adopt: the
    /// imported layer's, or the model's above the overlay threshold.
    fn reference_boxes(&self) -> Vec<(Annotation, String)> {
        match self.compare {
            Compare::Imported => self
                .imported
                .iter()
                .filter(|a| !a.is_ignore && crate::annotation::labels::is_taxonomy(a.class_id))
                .map(|a| (a.clone(), "the imported layer".to_owned()))
                .collect(),
            Compare::Prediction(i) => {
                let model = self.models.get(i).cloned().unwrap_or_default();
                self.predictions
                    .iter()
                    .filter(|p| {
                        p.score >= self.prediction_threshold
                            && (!self.figures_only() || p.class_id == 6)
                    })
                    .map(|p| {
                        let prov = serde_json::json!({
                            "source": "adopted", "from": model, "score": p.score,
                        })
                        .to_string();
                        (
                            Annotation {
                                id: String::new(),
                                class_id: p.class_id,
                                bbox: p.bbox,
                                is_ignore: false,
                                provenance_json: Some(prov),
                            },
                            format!("{model} {:.2}", p.score),
                        )
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// In the "only the model sees" queue the model's overlay shows its
    /// figures alone: they are what the page is in the queue for.
    fn figures_only(&self) -> bool {
        self.filter.queue == Some(database::Queue::ModelOnlyPictures)
    }

    /// A left click (no Shift, no Space) inside a comparison box, where no
    /// smaller box of yours is: that box, to adopt if the click ends
    /// without a drag.
    fn adopt_candidate(&self, ev: &PointerEvent) -> Option<(Point, Annotation)> {
        if !matches!(self.compare, Compare::Imported | Compare::Prediction(_))
            || ev.button.unwrap_or(MouseButton::Left) != MouseButton::Left
            || ev.modifiers.shift
            || self.editor.space_held
            || self.editor.state != ToolState::Idle
        {
            return None;
        }
        let doc = self.doc.as_ref()?;
        let screen = Point::new(ev.position.0, ev.position.1);
        let at = self.view.screen_to_image(screen);
        let (cand, _) = self
            .reference_boxes()
            .into_iter()
            .filter(|(a, _)| a.bbox.contains(at))
            .min_by(|a, b| a.0.bbox.area().total_cmp(&b.0.bbox.area()))?;
        // A box of yours under the click that is no bigger is what the click
        // is for (select, move); a bigger one (a text block round a figure)
        // is not.
        let hit = crate::canvas::hit_test::pick(
            &doc.annotations,
            self.editor.selected.as_deref(),
            &self.view,
            screen,
            self.editor.params.slop,
        );
        if hit.is_some_and(|h| doc.annotations[h.index].bbox.area() <= cand.bbox.area() * 1.2) {
            return None;
        }
        Some((screen, cand))
    }

    /// Copy a comparison box into the editable layer (one undoable edit)
    /// and select it.
    fn adopt_box(&mut self, mut ann: Annotation) {
        let label = self.compare_label();
        let source = match (self.compare, prefill_score(ann.provenance_json.as_deref())) {
            (Compare::Prediction(_), Some(s)) => format!("{label} ({s:.2})"),
            _ => label,
        };
        if self.compare == Compare::Imported {
            ann.provenance_json = Some(
                serde_json::json!({"source": "adopted", "from": "imported layer"}).to_string(),
            );
        }
        let Some(doc) = self.doc.as_mut() else { return };
        if doc
            .annotations
            .iter()
            .any(|a| a.class_id == ann.class_id && a.bbox.iou(&ann.bbox) >= 0.8)
        {
            self.flash("That box is already in your boxes", false);
            return;
        }
        ann.id = crate::annotation::commands::new_annotation_id();
        ann.bbox = ann.bbox.clamp_to(doc.width, doc.height);
        let id = ann.id.clone();
        let name = class(ann.class_id).name;
        let index = doc.annotations.len();
        if doc.apply(Edit::Add { index, ann }) {
            self.editor.selected = Some(id);
            self.commit("edit");
            self.flash(
                &format!("Added the {name} from {source} · a class key changes it, Ctrl+Z undoes"),
                false,
            );
        }
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
            Action::FirstPage => self.navigate(0, false),
            Action::LastPage => self.navigate(self.keys.len().saturating_sub(1), false),
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
            Action::Class(c) if self.convert.is_some() => self.convert_to(c),
            Action::Class(c) if self.similar.is_some() => self.similar_apply(c),
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
                let Some((key, before)) = self.doc.as_ref().map(|d| (d.page_key.clone(), d.status))
                else {
                    return;
                };
                let after = self.status_after_flag_key(&key, before, target);
                if let Some(doc) = self.doc.as_mut() {
                    doc.apply(Edit::Status { before, after });
                    self.flash(flag_message(after), false);
                    self.commit("status");
                    self.refresh_counts();
                }
            }
            Action::Grid => self.show_grid(),
            Action::ToggleOutlines => {
                self.editor.show_outlines = !self.editor.show_outlines;
                self.editor.hover = None;
            }
            Action::ToggleTags => self.settings.raw.show_tags = !self.settings.raw.show_tags,
            Action::ToggleHud => self.hud = !self.hud,
            Action::CycleCompare => self.cycle_compare(),
            Action::Queues => self.open_queues(),
            Action::Cancel => {
                if !self.editor.cancel() {
                    if self.editor.selected.is_some() {
                        self.editor.selected = None;
                    } else if self.grid_excursion {
                        // Nothing to cancel or clear: back to the six pages
                        // this page was opened from.
                        self.show_grid();
                    }
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
            Action::Books => self.show_books(),
            Action::NextAbandon => self.select_next_abandon(),
            Action::ConvertAbandon => {
                self.similar = None;
                self.open_convert();
            }
            Action::MatchSimilar => {
                self.convert = None;
                self.open_similar();
            }
            Action::OpenFolder => self.request = Some(ShellRequest::OpenFolder),
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

    /// `Q`: the queue picker, with page counts for the book (or all books).
    pub fn open_queues(&mut self) {
        if self.editor.is_busy() {
            return;
        }
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        self.similar = None;
        self.convert = None;
        let counts = database::queue_counts(&self.conn, &self.filter).unwrap_or_default();
        let entries: Vec<(database::Queue, i64)> = database::Queue::ALL
            .into_iter()
            .zip(counts.into_iter().chain(std::iter::repeat(0)))
            .collect();
        let scope = match &self.filter.book_id {
            Some(b) => format!("Book {b}"),
            None => "All books".to_owned(),
        };
        let model = self.models.first().cloned().unwrap_or_default();
        let mut picker = QueuePicker::new(entries, self.filter.queue, scope, model);
        picker.large_share = self.filter.large_share();
        self.queues = Some(picker);
        self.dirty = true;
    }

    fn handle_queues(&mut self, a: QueueAction) {
        match a {
            QueueAction::None => {}
            QueueAction::Redraw => self.dirty = true,
            QueueAction::Close => {
                self.queues = None;
                self.dirty = true;
            }
            QueueAction::Pick(q) => {
                self.queues = None;
                self.choose_queue(q);
            }
        }
    }

    /// Show queue `q`, keeping the current page when it is in it.
    pub fn choose_queue(&mut self, q: database::Queue) {
        if self.editor.is_busy() {
            return;
        }
        self.dirty = true;
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        let mut f = self.filter.clone();
        f.has_class = None;
        if q == database::Queue::All {
            f.queue = None;
            f.statuses = self.base_statuses.clone();
        } else {
            f.queue = Some(q);
            f.statuses.clear();
        }
        let keys = database::page_keys(&self.conn, &f).unwrap_or_default();
        if keys.is_empty() {
            self.flash(&format!("Queue \"{}\" has no pages here", q.label()), false);
            return;
        }
        self.filter = f;
        let current = self.current_key().map(str::to_owned);
        self.keys = keys;
        self.refresh_counts();
        if q == database::Queue::ModelOnlyPictures && !self.models.is_empty() {
            // The pictures are the model's: show its boxes to click.
            self.compare = Compare::Prediction(0);
        }
        if self.grid.is_some() {
            // The six with the first page still to do.
            let p = self.first_unreviewed().unwrap_or(0);
            self.grid_goto(p);
        } else {
            self.show_queue_page(current);
        }
        let msg = format!(
            "Queue: {} ({} pages) · Q: other queues",
            q.title(self.filter.large_share()),
            self.keys.len()
        );
        self.flash(&msg, false);
    }

    /// After a queue change: stay on page `current` when the queue has it,
    /// else open the queue's first page.
    fn show_queue_page(&mut self, current: Option<String>) {
        match current.and_then(|k| self.keys.iter().position(|x| *x == k)) {
            Some(p) => {
                self.pos = p;
                self.predictions = match self.compare {
                    Compare::Prediction(i) => self
                        .models
                        .get(i)
                        .and_then(|m| database::predictions(&self.conn, &self.keys[p], m).ok())
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                self.update_title();
                self.request_images();
            }
            None => self.open_page(0),
        }
    }

    /// Cycle the queue's status filter, keeping the current page when it is
    /// still in the new queue. (Kept for the command line's `--keys`.)
    #[allow(dead_code)]
    fn cycle_queue(&mut self) {
        use ReviewStatus::*;
        const MODES: [(&str, &[ReviewStatus], bool); 6] = [
            ("all", &[], false),
            ("to do", &[Unseen, Opened], false),
            ("with abandon boxes", &[], true),
            ("needs attention", &[NeedsAttention], false),
            ("excluded", &[Excluded], false),
            ("reviewed", &[Reviewed], false),
        ];
        if self.editor.is_busy() {
            return;
        }
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        let current = self.current_key().map(str::to_owned);
        let mut mode = 0;
        for _ in 0..MODES.len() {
            mode = (mode + 1) % MODES.len();
            let (name, statuses, abandon) = MODES[mode];
            self.filter.statuses = if mode == 0 {
                self.base_statuses.clone()
            } else {
                statuses.to_vec()
            };
            self.filter.has_class = abandon.then_some(ABANDON);
            let keys = database::page_keys(&self.conn, &self.filter).unwrap_or_default();
            if keys.is_empty() && mode != 0 {
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

    // --- six-page view -------------------------------------------------------

    /// X or N on page `key`, whose status is `current`: the flag, or, when
    /// the page already has it, the status from before it was set (a
    /// reviewed page stays reviewed instead of rejoining the to-do queue).
    fn status_after_flag_key(
        &mut self,
        key: &str,
        current: ReviewStatus,
        target: ReviewStatus,
    ) -> ReviewStatus {
        if current != target {
            return target;
        }
        self.commit("navigate");
        self.writer.flush();
        self.poll_background();
        database::status_before_flag(&self.conn, key)
            .ok()
            .flatten()
            .map(|s| {
                if s == ReviewStatus::Unseen {
                    ReviewStatus::Opened
                } else {
                    s
                }
            })
            .unwrap_or(ReviewStatus::Opened)
    }

    /// The document of page `key` when it is not the editor's page: the one
    /// in memory (it may hold edits not yet written), else the database's.
    fn take_doc(&mut self, key: &str) -> Option<PageDoc> {
        if let Some(d) = self.docs.remove(key) {
            return Some(d);
        }
        let meta = database::page_meta(&self.conn, key).ok().flatten()?;
        Some(self.fresh_doc(key, &meta))
    }

    /// Send `doc` to the writer if it changed since its last send, and keep
    /// it in memory as a recently visited page.
    fn save_doc(&mut self, doc: PageDoc, kind: &'static str) {
        if self.sent.get(&doc.page_key) != Some(&doc.revision) {
            self.writer.save(PageSave {
                page_key: doc.page_key.clone(),
                status: doc.status,
                annotations: doc.annotations.clone(),
                revision: doc.revision,
                kind,
            });
            self.sent.insert(doc.page_key.clone(), doc.revision);
        }
        let key = doc.page_key.clone();
        self.docs.insert(key.clone(), doc);
        self.recent.retain(|k| k != &key);
        self.recent.push_back(key);
        self.trim_docs();
    }

    /// The status of page `key` as this session knows it.
    fn page_status(&self, key: &str) -> Option<ReviewStatus> {
        if let Some(d) = self.doc.as_ref().filter(|d| d.page_key == key) {
            return Some(d.status);
        }
        if let Some(d) = self.docs.get(key) {
            return Some(d.status);
        }
        database::page_meta(&self.conn, key)
            .ok()
            .flatten()
            .map(|m| m.status)
    }

    /// Whether the six-page view is showing.
    pub fn grid_showing(&self) -> bool {
        self.grid.is_some()
    }

    /// Queue position of the first page of the six shown.
    pub fn grid_start(&self) -> Option<usize> {
        self.grid.as_ref().map(|g| g.start)
    }

    /// The six shown: page key, status, and whether Right leaves it alone
    /// because it was opened from the view and not accepted.
    pub fn grid_pages(&self) -> Vec<(String, ReviewStatus, bool)> {
        self.grid
            .as_ref()
            .map(|g| {
                g.cells
                    .iter()
                    .map(|c| (c.key.clone(), c.status, c.held))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The window layout of the view, once a frame has been drawn.
    pub fn grid_layout(&self) -> Option<grid::Layout> {
        self.grid.as_ref().and_then(|g| g.layout.clone())
    }

    /// `G`: six pages of the queue at once, the six holding the current page.
    pub fn show_grid(&mut self) {
        if self.keys.is_empty() {
            self.flash("No pages in this queue", true);
            return;
        }
        if self.editor.is_busy() {
            self.editor.cancel();
        }
        self.commit("navigate");
        // Pages looked at in the editor since it was opened from the view,
        // and left there without accepting, stay out of the view's Right.
        if self.grid_excursion {
            for key in std::mem::take(&mut self.grid_visited) {
                let todo = self
                    .page_status(&key)
                    .is_some_and(|s| matches!(s, ReviewStatus::Unseen | ReviewStatus::Opened));
                if todo {
                    self.grid_held.insert(key);
                }
            }
        }
        self.grid_excursion = false;
        self.grid_visited.clear();
        self.stash_current();
        self.image = ImageState::None;
        self.queues = None;
        self.convert = None;
        self.similar = None;
        self.palette = false;
        self.editor.hover = None;
        self.capture.clear();
        let start = grid::group_start(self.pos);
        self.grid = Some(GridView::new(start));
        self.grid_goto(start);
        self.flash(
            "Six pages: Right accepts those still to do · click or 1–6 opens one · G: one page · F1: keys",
            false,
        );
    }

    /// Show the six holding queue position `pos`.
    fn grid_goto(&mut self, pos: usize) {
        let n = self.keys.len();
        let Some(g) = self.grid.as_mut() else { return };
        if n == 0 {
            g.cells.clear();
            self.dirty = true;
            return;
        }
        let start = grid::group_start(pos.min(n - 1));
        if g.start != start {
            g.abandon_warned = None;
        }
        g.start = start;
        self.pos = start;
        self.load_grid_cells();
        self.request_images();
        self.update_title();
        self.dirty = true;
    }

    /// Read the six shown: boxes and status from the page's document when
    /// one is in memory, else from the database.
    fn load_grid_cells(&mut self) {
        let Some(start) = self.grid.as_ref().map(|g| g.start) else {
            return;
        };
        let end = (start + grid::PER_VIEW).min(self.keys.len());
        let mut cells = Vec::new();
        for key in &self.keys[start..end] {
            let meta = database::page_meta(&self.conn, key).ok().flatten();
            let Some(meta) = meta else {
                self.failed_images
                    .insert(key.clone(), "no longer in the project".into());
                cells.push(grid::Cell {
                    key: key.clone(),
                    book: "?".into(),
                    file: key.clone(),
                    width: 0,
                    height: 0,
                    status: ReviewStatus::Excluded,
                    annotations: Vec::new(),
                    held: false,
                });
                continue;
            };
            let (annotations, status) = match self.docs.get(key) {
                Some(d) => (d.annotations.clone(), d.status),
                None => (
                    database::annotations(&self.conn, key).unwrap_or_default(),
                    meta.status,
                ),
            };
            let file = Path::new(&meta.file_name)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| meta.file_name.clone());
            cells.push(grid::Cell {
                key: key.clone(),
                book: meta.book_id,
                file,
                width: meta.width,
                height: meta.height,
                status,
                annotations,
                held: self.grid_held.contains(key),
            });
        }
        if let Some(g) = self.grid.as_mut() {
            g.cells = cells;
        }
    }

    /// Each shown page's image: decoded, still loading, or failed.
    fn grid_images(&mut self) -> Vec<CellImage> {
        let cells: Vec<(String, u32, u32)> = self
            .grid
            .as_ref()
            .map(|g| {
                g.cells
                    .iter()
                    .map(|c| (c.key.clone(), c.width, c.height))
                    .collect()
            })
            .unwrap_or_default();
        cells
            .into_iter()
            .map(|(key, w, h)| {
                if let Some(e) = self.failed_images.get(&key) {
                    return CellImage::Failed(e.clone());
                }
                match self.cache.get(&key) {
                    Some(img) if (img.width(), img.height()) == (w, h) => CellImage::Ready(img),
                    Some(img) => CellImage::Failed(format!(
                        "image is {}x{} but the project records {w}x{h}",
                        img.width(),
                        img.height()
                    )),
                    None => CellImage::Loading,
                }
            })
            .collect()
    }

    /// Open page `i` of the six in the editor. G or Escape there come back.
    fn grid_open(&mut self, i: usize) {
        let Some(g) = &self.grid else { return };
        let pos = g.start + i;
        if i >= g.cells.len() || pos >= self.keys.len() {
            return;
        }
        self.grid = None;
        self.help = false;
        self.grid_excursion = true;
        self.grid_visited.clear();
        self.open_page(pos);
        self.flash(
            "G or Esc: back to the six pages. Leave without Right and Right there passes this page by",
            false,
        );
    }

    /// Right in the view: every page shown that is still to do becomes
    /// reviewed, then the next six. Left to do: pages opened from the view
    /// and not accepted, pages whose image failed, and (after one warning)
    /// pages with `abandon` boxes. Nothing is accepted while a page to do is
    /// still loading: it has not been seen.
    fn grid_accept(&mut self) {
        let images = self.grid_images();
        let Some(g) = &self.grid else { return };
        let start = g.start;
        let (mut loading, mut failed, mut held) = (0usize, 0usize, 0usize);
        let mut abandon = Vec::new();
        let mut accept = Vec::new();
        for (cell, img) in g.cells.iter().zip(&images) {
            if !cell.todo() {
                continue;
            }
            if cell.held {
                held += 1;
                continue;
            }
            match img {
                CellImage::Loading => loading += 1,
                CellImage::Failed(_) => failed += 1,
                CellImage::Ready(_) if cell.abandon() > 0 => abandon.push(cell.key.clone()),
                CellImage::Ready(_) => accept.push(cell.key.clone()),
            }
        }
        if loading > 0 {
            let msg = format!(
                "{loading} of these pages {} still loading: nothing accepted yet",
                if loading == 1 { "is" } else { "are" }
            );
            self.flash(&msg, true);
            return;
        }
        if !abandon.is_empty() && g.abandon_warned != Some(start) {
            if let Some(g) = self.grid.as_mut() {
                g.abandon_warned = Some(start);
            }
            let n = abandon.len();
            let msg = format!(
                "{n} page{} here still {} abandon boxes, which export skips: click one to label them · \
                 Right again: accept the others and leave {} to do",
                if n == 1 { "" } else { "s" },
                if n == 1 { "has" } else { "have" },
                if n == 1 { "it" } else { "them" }
            );
            self.flash(&msg, true);
            return;
        }
        for key in &accept {
            if let Some(mut doc) = self.take_doc(key) {
                doc.set_status_untracked(ReviewStatus::Reviewed);
                self.save_doc(doc, "accept");
            }
        }
        let n = accept.len();
        self.reviewed_in_queue += n;
        self.pace.accepted_n(Instant::now(), n as u32);
        let mut left = Vec::new();
        if held > 0 {
            left.push(format!("{held} opened and not accepted"));
        }
        if !abandon.is_empty() {
            left.push(format!("{} with abandon boxes", abandon.len()));
        }
        if failed > 0 {
            left.push(format!("{failed} whose image did not load"));
        }
        let mut msg = format!("Accepted {n} page{}", if n == 1 { "" } else { "s" });
        if !left.is_empty() {
            msg.push_str(&format!(" · left to do: {}", left.join(", ")));
        }
        let next = start + grid::PER_VIEW;
        if next < self.keys.len() {
            self.grid_goto(next);
        } else {
            self.load_grid_cells();
            msg.push_str(" · end of queue: every page in it has been shown");
        }
        self.flash(&msg, failed > 0);
    }

    /// X or N in the view: the page under the pointer.
    fn grid_flag(&mut self, target: ReviewStatus) {
        let hovered = self.grid.as_ref().and_then(|g| {
            g.hover
                .and_then(|i| g.cells.get(i).map(|c| (i, c.key.clone())))
        });
        let Some((i, key)) = hovered else {
            self.flash(
                "Point at a page first: X and N mark the page under the pointer",
                false,
            );
            return;
        };
        let Some(mut doc) = self.take_doc(&key) else {
            return;
        };
        let before = doc.status;
        let after = self.status_after_flag_key(&key, before, target);
        doc.apply(Edit::Status { before, after });
        self.save_doc(doc, "status");
        if let Some(c) = self.grid.as_mut().and_then(|g| g.cells.get_mut(i)) {
            c.status = after;
        }
        self.refresh_counts();
        self.flash(&format!("Page {}: {}", i + 1, flag_message(after)), false);
    }

    fn grid_key(&mut self, ev: &KeyEvent) {
        let m = ev.modifiers;
        if ev.pressed && !(m.control || m.alt || m.logo || m.shift) {
            let digit = ev
                .physical
                .strip_prefix("Digit")
                .or_else(|| ev.physical.strip_prefix("Numpad"))
                .and_then(|d| d.parse::<usize>().ok());
            if let Some(d) = digit.filter(|d| (1..=grid::PER_VIEW).contains(d)) {
                self.grid_open(d - 1);
                return;
            }
        }
        let Some(action) = map_key(ev, &self.settings) else {
            return;
        };
        self.dirty = true;
        if self.help && !matches!(action, Action::SpaceDown | Action::SpaceUp) {
            self.help = false;
            if matches!(action, Action::Help | Action::Cancel) {
                return;
            }
        }
        let Some(start) = self.grid_start() else {
            return;
        };
        let k = grid::PER_VIEW;
        let n = self.keys.len();
        let last = grid::group_start(n.saturating_sub(1));
        match action {
            Action::AcceptAndNext => self.grid_accept(),
            Action::NextPage => {
                if start + k < n {
                    self.grid_goto(start + k);
                } else {
                    self.flash("These are the last pages of the queue", false);
                }
            }
            Action::PrevPage => {
                if start > 0 {
                    self.grid_goto(start - k);
                }
            }
            Action::FirstPage => self.grid_goto(0),
            Action::LastPage => self.grid_goto(last),
            Action::JumpBack10 => self.grid_goto(start.saturating_sub(10 * k)),
            Action::JumpFwd10 => self.grid_goto((start + 10 * k).min(last)),
            Action::FirstUnreviewed => {
                self.writer.flush();
                self.poll_background();
                match self.first_unreviewed() {
                    Some(p) => self.grid_goto(p),
                    None => self.flash("No pages to do in this queue", false),
                }
            }
            Action::ToggleExclude => self.grid_flag(ReviewStatus::Excluded),
            Action::ToggleAttention => self.grid_flag(ReviewStatus::NeedsAttention),
            Action::Grid => {
                // The page pointed at, else the first still to do.
                let i = self.grid.as_ref().and_then(|g| {
                    g.hover
                        .or_else(|| g.cells.iter().position(|c| c.todo()))
                        .or(Some(0))
                });
                if let Some(i) = i {
                    self.grid_open(i);
                }
            }
            Action::Help
            | Action::Fullscreen
            | Action::PerfOverlay
            | Action::Queues
            | Action::Books
            | Action::OpenFolder
            | Action::SaveNotice
            | Action::SpaceDown
            | Action::SpaceUp => self.act(action),
            _ => {}
        }
    }

    fn grid_pointer(&mut self, ev: &PointerEvent) {
        let (x, y) = (ev.position.0 as i32, ev.position.1 as i32);
        if ev.phase == PointerPhase::Down && self.help {
            self.help = false;
            self.dirty = true;
            return;
        }
        let Some(g) = self.grid.as_mut() else { return };
        let at = g
            .layout
            .as_ref()
            .and_then(|l| l.cell_at(x, y))
            .filter(|&i| i < g.cells.len());
        if g.hover != at {
            g.hover = at;
            self.dirty = true;
        }
        match ev.phase {
            PointerPhase::Move => {}
            PointerPhase::Down => {
                let left = ev.button.unwrap_or(MouseButton::Left) == MouseButton::Left;
                g.pressed = if left { at } else { None };
            }
            PointerPhase::Up => {
                let pressed = g.pressed.take();
                if let Some(i) = pressed.filter(|_| pressed == at) {
                    self.grid_open(i);
                }
            }
            PointerPhase::Cancel => g.pressed = None,
        }
    }

    fn render_grid(&mut self, buffer: &mut WindowBuffer) {
        let images = self.grid_images();
        let unrequested = self.grid.as_ref().is_some_and(|g| {
            g.cells
                .iter()
                .zip(&images)
                .any(|(c, i)| matches!(i, CellImage::Loading) && !self.loader.is_loading(&c.key))
        });
        if unrequested {
            self.request_images();
        }
        let n = self.keys.len();
        let Some(mut g) = self.grid.take() else {
            return;
        };
        let left = format!(
            "{}  ·  pages {}–{} of {}  ·  {}% reviewed",
            self.filter.describe(),
            g.start + 1,
            g.start + g.cells.len(),
            n,
            (self.reviewed_in_queue * 100).checked_div(n).unwrap_or(0)
        );
        let right = "Right: accept these · Shift+Right / Left: next / previous six · click or 1–6: open · \
                     X / N: page pointed at · G: one page · F1: keys";
        let message = self
            .message
            .as_ref()
            .filter(|(_, at, err)| at.elapsed() < Duration::from_secs(if *err { 8 } else { 3 }))
            .map(|(m, _, err)| (m.clone(), *err));
        g.draw(
            buffer,
            &mut self.text,
            self.scale,
            &images,
            &self.settings.colors,
            &grid::Header {
                left: &left,
                right,
                message: message.as_ref().map(|(m, e)| (m.as_str(), *e)),
            },
            self.render_threads,
        );
        let header_h = g.layout.as_ref().map_or(0, |l| l.header.h);
        self.grid = Some(g);
        let (bw, bh) = (buffer.width as i32, buffer.height as i32);
        let mut p = Painter::new(buffer);
        if let Some(d) = &mut self.queues {
            let area = Rect::new(0, header_h, bw, bh - header_h);
            d.draw(&mut p, &mut self.text, self.scale, area);
        }
        if self.help {
            self.draw_help(&mut p, bw, bh);
        }
        if self.perf_overlay {
            self.draw_perf(&mut p, bw);
        }
    }

    fn compare_label(&self) -> String {
        match self.compare {
            Compare::Off => "off".into(),
            Compare::Imported => "imported (teacher) layer".into(),
            Compare::Prediction(i) => format!(
                "model {}{}",
                self.models.get(i).map(String::as_str).unwrap_or("?"),
                if self.figures_only() { ", figures" } else { "" }
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
            if self.books.is_some() {
                let (x, y) = (p.x as i32, p.y as i32);
                match ev.phase {
                    PointerPhase::Down => {
                        let action = self
                            .books
                            .as_mut()
                            .map_or(BooksAction::None, |b| b.click(x, y));
                        self.dirty = true;
                        self.handle_books(action);
                    }
                    PointerPhase::Move => {
                        if self.books.as_mut().is_some_and(|b| b.pointer_move(x, y)) {
                            self.dirty = true;
                        }
                    }
                    PointerPhase::Up | PointerPhase::Cancel => {
                        if self.books.as_mut().is_some_and(|b| b.pointer_up(x, y)) {
                            self.dirty = true;
                        }
                    }
                }
                continue;
            }
            if ev.phase == PointerPhase::Down && self.capture.owner().is_none() {
                if let Some(d) = &mut self.queues {
                    let a = if d.contains(p.x as i32, p.y as i32) {
                        d.click(p.x as i32, p.y as i32)
                    } else {
                        QueueAction::Close
                    };
                    self.handle_queues(a);
                    self.dirty = true;
                    continue;
                }
                if let Some(d) = &mut self.similar {
                    if d.contains(p.x as i32, p.y as i32) {
                        let a = d.click(p.x as i32, p.y as i32);
                        self.handle_similar(a);
                        self.dirty = true;
                        continue;
                    }
                    let in_toolbar =
                        self.hud && (p.y < self.canvas.y || p.y >= self.canvas.y + self.canvas.h);
                    if !in_toolbar {
                        self.similar = None;
                        self.dirty = true;
                    }
                }
                if let Some(d) = &mut self.convert {
                    if d.contains(p.x as i32, p.y as i32) {
                        match d.click(p.x as i32, p.y as i32) {
                            Some(ConvertAction::Undo(id)) => self.undo_bulk(id),
                            Some(ConvertAction::Close) => self.convert = None,
                            _ => {}
                        }
                        self.dirty = true;
                        continue;
                    }
                    // A click on the page, not a class chip, closes the dialog.
                    let in_toolbar =
                        self.hud && (p.y < self.canvas.y || p.y >= self.canvas.y + self.canvas.h);
                    if !in_toolbar {
                        self.convert = None;
                        self.dirty = true;
                    }
                }
            }
            if self.grid.is_some() {
                self.grid_pointer(&ev);
                continue;
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
                                Some(ToolbarItem::Books) => self.act(Action::Books),
                                Some(ToolbarItem::Class(c)) => self.act(Action::Class(c)),
                                Some(ToolbarItem::More) => self.palette = !self.palette,
                                Some(ToolbarItem::Compare) => self.cycle_compare(),
                                Some(ToolbarItem::Revert) => self.act(Action::RevertPage),
                                Some(ToolbarItem::Similar) => self.act(Action::MatchSimilar),
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

    /// The label chips of the top bar: (key, class).
    fn chip_entries(&self) -> Vec<(String, u8)> {
        self.settings
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
            .collect()
    }

    /// Whether every label chip fits beside the queue and page info on one
    /// row. The queue text is measured at its widest (`9999 / 9999 · 100%`)
    /// so paging never flips the layout.
    fn toolbar_fits_one_row(&mut self, w: u32) -> bool {
        let s = self.scale;
        let body = self.style(13.0);
        let small = self.style(11.5);
        let digits = self.keys.len().max(1).to_string().len();
        let n = "9".repeat(digits);
        let queue = format!("{}  {n} / {n} · 100%", self.filter.describe());
        let left = s.px(8)
            + self.text.measure("Books (B)", body)
            + s.px(30)
            + self.text.measure(&queue, body)
            + s.px(18);
        let right_text = match &self.meta {
            Some(m) => format!(
                "{} · {} · {}×{} px",
                m.book_id, m.file_name, m.width, m.height
            ),
            None => String::new(),
        };
        let right = self.text.measure(&right_text, small).max(s.px(80)) + s.px(12);
        let chips: i32 = self
            .chip_entries()
            .iter()
            .map(|(k, c)| {
                self.text
                    .measure(&format!("{k} {}", class(*c).short), small)
                    + s.px(26)
            })
            .sum();
        left + chips + right + s.px(16) <= w as i32
    }

    fn layout(&mut self, w: u32, h: u32) {
        let s = self.scale.0 as f64;
        self.toolbar_two_rows = self.hud && !self.toolbar_fits_one_row(w);
        let row = if self.toolbar_two_rows { 72.0 } else { 40.0 };
        let (top, bottom) = if self.hud {
            ((row * s).round(), (24.0 * s).round())
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

        if self.grid.is_some() && self.books.is_none() {
            self.render_grid(buffer);
            self.perf.frames += 1;
            push_ring(&mut self.perf.frame_ms, t0.elapsed().as_secs_f64() * 1000.0);
            return;
        }

        if let Some(mut books) = self.books.take() {
            let mut p = Painter::new(buffer);
            books.draw(&mut p, &mut self.text, self.scale, bw as i32, bh as i32);
            self.books = Some(books);
            if let Some((m, _, err)) = self.message.clone() {
                let st = self.style(12.0);
                let lh = self.text.line_height(st);
                let r = Rect::new(
                    0,
                    bh as i32 - lh - self.scale.px(40),
                    bw as i32,
                    lh + self.scale.px(8),
                );
                p.fill_rect(r, if err { 0x0040_1818 } else { 0x0020_2a36 });
                self.text.draw_fitted(
                    &mut p,
                    &m,
                    r.inset(self.scale.px(8)),
                    r.y + self.scale.px(4),
                    st,
                    HUD_TEXT,
                    Align::Centre,
                );
            }
            self.perf.frames += 1;
            return;
        }

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
        if let Some(d) = &mut self.convert {
            let area = Rect::new(cx, cy, cw as i32, ch as i32);
            d.draw(&mut painter, &mut self.text, self.scale, area);
        }
        if let Some(d) = &mut self.similar {
            let area = Rect::new(cx, cy, cw as i32, ch as i32);
            d.draw(&mut painter, &mut self.text, self.scale, area);
        }
        if let Some(d) = &mut self.queues {
            let area = Rect::new(cx, cy, cw as i32, ch as i32);
            d.draw(&mut painter, &mut self.text, self.scale, area);
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
                let figures_only = self.figures_only();
                for pr in preds
                    .iter()
                    .filter(|x| x.score >= thr && (!figures_only || x.class_id == 6))
                {
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
        let low_conf = &self.low_conf;
        let unsure = |a: &Annotation| {
            low_conf
                .get(&a.id)
                .is_some_and(|(c, b)| *c == a.class_id && *b == a.bbox)
        };
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
            } else if unsure(a) && !selected {
                // The prefill model was unsure: long dashes until edited.
                dashed_outline(p, r, color, 9, 4);
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
        let full = self.canvas.y as i32;
        p.fill_rect(Rect::new(0, 0, w, full), HUD_BG);
        p.fill_rect(Rect::new(0, full - 1, w, 1), HUD_EDGE);
        // With two rows, the first holds the queue and page info, the second
        // every label chip.
        let h = if self.toolbar_two_rows {
            s.px(40)
        } else {
            full
        };
        self.toolbar_hits.clear();
        let body = self.style(13.0);
        let small = self.style(11.5);
        let lh = self.text.line_height(body);
        let ty = (h - lh) / 2;
        let mut x = s.px(8);
        {
            let label = "Books (B)";
            let bw = self.text.measure(label, body) + s.px(16);
            let r = Rect::new(x, (h - s.px(26)) / 2, bw, s.px(26));
            outline(p, r, HUD_EDGE, 1);
            self.text.draw(p, label, x + s.px(8), ty, body, HUD_TEXT);
            self.toolbar_hits.push((r, ToolbarItem::Books));
            x += bw + s.px(14);
        }

        let n = self.keys.len();
        let queue = format!(
            "{}  {} / {} · {}%",
            self.filter.describe(),
            if n == 0 { 0 } else { self.pos + 1 },
            n,
            (self.reviewed_in_queue * 100).checked_div(n).unwrap_or(0)
        );
        x += self.text.draw(p, &queue, x, ty, body, HUD_TEXT) + s.px(12);
        for (on, label, color) in [
            (self.book_flags.excluded, "not for training", DIFF_DELETED),
            (self.book_flags.needs_work, "needs work", DIFF_MODIFIED),
        ] {
            if on {
                let tw = self.text.measure(label, small) + s.px(12);
                let r = Rect::new(x, (h - s.px(20)) / 2, tw, s.px(20));
                outline(p, r, color, 1);
                let sl = self.text.line_height(small);
                self.text
                    .draw(p, label, x + s.px(6), r.y + (r.h - sl) / 2, small, color);
                x += tw + s.px(8);
            }
        }
        if let Some(rate) = self.pace.per_hour() {
            let label = format!("{rate:.0} pages/h");
            x += self.text.draw(p, &label, x, ty, small, HUD_DIM) + s.px(10);
        }
        x += s.px(6);

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
        let (mut x, cy, right_x) = if self.toolbar_two_rows {
            p.fill_rect(Rect::new(0, h, w, 1), HUD_EDGE);
            (s.px(8), h + (full - h - chip_h) / 2, w - s.px(4))
        } else {
            (x, (h - chip_h) / 2, right_x)
        };
        let mut shown = 0;
        let entries = self.chip_entries();
        let total = entries.len();
        // Room for a "+N more" chip is kept only if not every chip fits.
        let all_w: i32 = entries
            .iter()
            .map(|(k, c)| {
                self.text
                    .measure(&format!("{k} {}", class(*c).short), small)
                    + s.px(26)
            })
            .sum();
        let more_w = if x + all_w + s.px(16) <= right_x {
            0
        } else {
            s.px(90)
        };
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
                        let label = match (
                            v.get("label_class").and_then(|x| x.as_str()),
                            v.get("teacher_class").and_then(|x| x.as_str()),
                        ) {
                            (Some(pp), Some(yolo)) => {
                                let s = v
                                    .get("label_score")
                                    .and_then(|x| x.as_f64())
                                    .map(|x| format!(" {x:.2}"))
                                    .unwrap_or_default();
                                format!(" ({yolo}; label from PP-DocLayout-L {pp}{s})")
                            }
                            _ => String::new(),
                        };
                        format!(" · {src}{score}{label}")
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
                let abandon = doc
                    .annotations
                    .iter()
                    .filter(|a| a.class_id == ABANDON)
                    .count();
                if abandon > 0 {
                    return Some((
                        format!(
                            "{} boxes, {abandon} abandon (A: select next, Shift+A: convert){reason}{counts}",
                            doc.annotations.len()
                        ),
                        DIFF_MODIFIED,
                    ));
                }
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
        let mut rw = rw + s.px(14) + bw2;
        if self.editor.selected.is_some() {
            let label = "Same on every page (M)";
            let bw3 = self.text.measure(label, st) + s.px(14);
            let b3 = Rect::new(button.x - s.px(8) - bw3, top + s.px(3), bw3, bh - s.px(6));
            outline(p, b3, ACCENT, 1);
            self.text.draw(p, label, b3.x + s.px(7), ty, st, HUD_TEXT);
            self.toolbar_hits.push((b3, ToolbarItem::Similar));
            rw += s.px(8) + bw3;
        }
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
            ("Double click a figure", "make it the whole page"),
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
                "review queues: picture pages, pictures only the model sees, empty, text sample, …",
            ),
            (
                "C, then click",
                "with the comparison shown, a click on one of its boxes copies it into your boxes",
            ),
            ("Home / End, PgUp/PgDn", "first / last page, jump 10"),
            ("Ctrl+Home", "first page still to do"),
            ("B, Ctrl+O", "book manager; open a dataset folder"),
            (
                "M",
                "same on every page: relabel boxes in the selected box's place, whole book",
            ),
            (
                "A / Shift+A",
                "select next abandon box / convert abandon boxes in bulk",
            ),
            (
                "Escape",
                "cancel the drag, clear the selection, then back to the six-page view",
            ),
            (
                "G",
                "six pages at once; from a page opened there, back to them (also Escape)",
            ),
            ("F11 / F12", "full screen / performance overlay"),
        ];
        let grid_lines: &[(&str, &str)] = &[
            (
                "Right",
                "accept the six: each page still to do is marked reviewed,",
            ),
            (
                "",
                "then the next six. Left to do and marked in their label:",
            ),
            ("", "pages opened from here and left without Right"),
            (
                "",
                "(\"opened, not accepted\"; Right in the editor accepts them),",
            ),
            ("", "pages whose image did not load, and pages with abandon"),
            (
                "",
                "boxes (the first Right warns; Right again accepts the rest)",
            ),
            (
                "Shift+Right / Left",
                "next / previous six, statuses unchanged",
            ),
            ("Home / End", "first / last six of the queue"),
            ("PgUp / PgDn", "sixty pages back / forward"),
            ("Ctrl+Home", "the six with the first page still to do"),
            (
                "Click, 1–6",
                "open that page in the editor; G or Escape there: back here",
            ),
            (
                "X / N",
                "exclude / flag the page under the pointer; again: undo",
            ),
            (
                "G",
                "the editor, on the page pointed at (else the first to do)",
            ),
            ("Q / B", "review queues / book manager"),
            ("F11 / F12", "full screen / performance overlay"),
        ];
        let (title, lines) = if self.grid.is_some() {
            ("Six-page view", grid_lines)
        } else {
            ("Keyboard and mouse", lines)
        };
        let pw = s.px(640).min(w - s.px(40));
        let ph = lh * (lines.len() as i32 + 2) + s.px(16);
        let panel = Rect::new((w - pw) / 2, (h - ph) / 2, pw, ph);
        p.blend_rect(Rect::new(0, 0, w, h), 0x000000, 120);
        p.fill_rect(panel, HUD_BG);
        outline(p, panel, HUD_EDGE, 1);
        let mut y = panel.y + s.px(10);
        self.text
            .draw(p, title, panel.x + s.px(16), y, st, HUD_TEXT);
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
        self.settings.colors[(c as usize).min(NUM_LABELS - 1)]
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
        if let Some(books) = &mut self.books {
            let action = books.key(event);
            self.handle_books(action);
            return;
        }
        if let Some(d) = &mut self.queues {
            let a = d.key(event);
            self.handle_queues(a);
            return;
        }
        if let Some(d) = &mut self.similar {
            let a = d.key(event);
            if a != SimilarAction::None {
                self.handle_similar(a);
                return;
            }
            match map_key(event, &self.settings) {
                Some(Action::Class(c)) => self.similar_apply(c),
                Some(Action::Cancel) => {
                    self.similar = None;
                    self.dirty = true;
                }
                Some(a @ (Action::SpaceDown | Action::SpaceUp)) => self.act(a),
                _ => {}
            }
            return;
        }
        if let Some(d) = &mut self.convert {
            match d.key(event) {
                ConvertAction::None => {}
                ConvertAction::Redraw => {
                    self.dirty = true;
                    return;
                }
                ConvertAction::Undo(id) => {
                    self.undo_bulk(id);
                    return;
                }
                ConvertAction::Close => {
                    self.convert = None;
                    self.dirty = true;
                    return;
                }
            }
            // Class keys convert; Escape closes; everything else waits.
            match map_key(event, &self.settings) {
                Some(Action::Class(c)) => self.convert_to(c),
                Some(Action::Cancel) => {
                    self.convert = None;
                    self.dirty = true;
                }
                Some(a @ (Action::SpaceDown | Action::SpaceUp)) => self.act(a),
                _ => {}
            }
            return;
        }
        if self.grid.is_some() {
            self.grid_key(event);
            return;
        }
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
        // Off the window: the book table's scroll bar narrows again (unless
        // its thumb is being dragged).
        if let Some(b) = &mut self.books {
            if !b.dragging_scrollbar() && b.pointer_move(-1, -1) {
                self.dirty = true;
            }
        }
        self.pointer.left();
        if let Some(g) = &mut self.grid {
            if g.hover.take().is_some() {
                self.dirty = true;
            }
        }
        if self.capture.owner().is_none() && self.editor.hover.take().is_some() {
            self.dirty = true;
        }
    }

    fn on_mouse(&mut self, button: MouseButton, pressed: bool) {
        self.pointer.button(button, pressed);
    }

    fn on_scroll_event(&mut self, event: &ScrollEvent) {
        if let Some(books) = &mut self.books {
            let dy = event
                .line_dy
                .map_or(event.pixel_dy as f64 / 60.0, |l| l as f64);
            let dx = event
                .line_dx
                .map_or(event.pixel_dx as f64 / 60.0, |l| l as f64);
            if self.pointer.modifiers().shift && dx == 0.0 {
                books.hscroll_by(if dy > 0.0 { -1 } else { 1 });
            } else if dx.abs() > dy.abs() {
                books.hscroll_by(if dx > 0.0 { -1 } else { 1 });
            } else if dy != 0.0 {
                books.scroll_by(dy);
            }
            self.dirty = true;
            return;
        }
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
