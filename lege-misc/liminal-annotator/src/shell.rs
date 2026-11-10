//! The window: owns the open project's editor, or the start screen when
//! there is none, and everything that switches projects: the folder dialog
//! (Ctrl+O), a folder or `.db` dropped onto the window, the recent list, and
//! the import that turns a dataset folder into a project.
//!
//! Opening a folder opens `<folder>/annotator.db`, creating it from the
//! folder's contents first (see [`crate::dataset::import_folder`]); when the
//! folder has new detections since the last visit, only those are imported.
//! A folder project opens on the book manager.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use liminalkit_raster::{Painter, Rect, WindowBuffer};
use liminalkit_shell::{KeyInput, Modifiers, MouseButton, Scale};
use liminalkit_text::{Align, TextCache};
use liminalkit_windowing::{CursorShape, KeyEvent, PixelApp, ScrollEvent, Waker};

use crate::app::{AnnotatorApp, AppConfig, ShellRequest};
use crate::canvas::draw::outline;
use crate::dataset::database::{self, PageFilter};
use crate::dataset::import_folder::{self, FolderOptions, FolderReport, PROJECT_FILE, project_for};
use crate::fonts;
use crate::recent;
use crate::ui::{self, ACCENT, BAD, HUD_BG, HUD_DIM, HUD_EDGE, HUD_TEXT, PANEL_BG, ROW_SELECTED};

/// How the window starts.
#[derive(Clone, Debug)]
pub struct ShellConfig {
    /// A dataset folder or a project `.db` to open; `None` reopens the most
    /// recent project, or shows the start screen.
    pub open: Option<PathBuf>,
    /// Queue options from the command line, for the first project opened.
    pub filter: PageFilter,
    pub start_at: Option<String>,
    pub prediction_threshold: f64,
    /// How a new folder project is filled.
    pub folder: FolderOptions,
    /// Open the first project in the six-page view (`--grid`).
    pub grid: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        ShellConfig {
            open: None,
            filter: PageFilter::default(),
            start_at: None,
            prediction_threshold: 0.3,
            folder: FolderOptions::default(),
            grid: false,
        }
    }
}

enum JobMsg {
    Progress(String, u64, u64),
    Done(Result<FolderReport, String>),
}

struct ImportJob {
    root: PathBuf,
    project: PathBuf,
    creating: bool,
    rx: Receiver<JobMsg>,
    stage: String,
    done: u64,
    total: u64,
    started: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    OpenFolder,
    Recent(usize),
}

type WakeSlot = Arc<Mutex<Option<Waker>>>;

fn wake(slot: &WakeSlot) {
    if let Ok(w) = slot.lock() {
        if let Some(w) = w.as_ref() {
            w.wake();
        }
    }
}

pub struct Shell {
    app: Option<AnnotatorApp>,
    cfg: ShellConfig,
    /// The command line's queue options apply to the first project only.
    first_open: bool,
    wake_slot: WakeSlot,
    scale: Scale,
    text: TextCache,
    job: Option<ImportJob>,
    picker: Option<Receiver<Option<PathBuf>>>,
    recent: Vec<PathBuf>,
    error: Option<String>,
    hits: Vec<(Rect, Hit)>,
    cursor: (f32, f32),
    title: Option<String>,
    dirty: bool,
    last_size: (u32, u32),
    exit: bool,
}

impl Shell {
    pub fn new(cfg: ShellConfig) -> Shell {
        let mut shell = Shell {
            app: None,
            first_open: true,
            wake_slot: Arc::new(Mutex::new(None)),
            scale: Scale::ONE,
            text: TextCache::new(fonts::font_set()),
            job: None,
            picker: None,
            recent: recent::load(),
            error: None,
            hits: Vec::new(),
            cursor: (0.0, 0.0),
            title: Some("Liminal Annotator".into()),
            dirty: true,
            last_size: (0, 0),
            exit: false,
            cfg,
        };
        match shell.cfg.open.clone() {
            Some(p) => shell.open_path(&p, false),
            None => {
                if let Some(last) = shell.recent.first().cloned() {
                    shell.open_path(&last, true);
                }
            }
        }
        shell
    }

    /// The editor, when a project is open (tests).
    pub fn app(&mut self) -> Option<&mut AnnotatorApp> {
        self.app.as_mut()
    }

    pub fn importing(&self) -> bool {
        self.job.is_some()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Open a folder (creating or updating its project) or a project file.
    /// `browse`: a project file opens on the book manager rather than on a page.
    pub fn open_path(&mut self, path: &Path, browse: bool) {
        self.dirty = true;
        self.error = None;
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if path.is_file() {
            if path.file_name().is_some_and(|n| n == PROJECT_FILE)
                && path
                    .parent()
                    .is_some_and(|d| self.folder_needs_sync(&path, d))
            {
                let root = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.start_import(root, path, false);
                return;
            }
            let books = browse || path.file_name().is_some_and(|n| n == PROJECT_FILE);
            self.open_project(&path, books);
            return;
        }
        if !path.is_dir() {
            self.error = Some(format!("{} does not exist", path.display()));
            return;
        }
        let project = project_for(&path);
        if project.is_file() {
            if self.folder_needs_sync(&project, &path) {
                self.start_import(path, project, false);
            } else {
                self.open_project(&project, true);
            }
        } else {
            self.start_import(path, project, true);
        }
    }

    fn folder_needs_sync(&self, project: &Path, _root: &Path) -> bool {
        database::open(project)
            .map(|c| import_folder::needs_sync(&c))
            .unwrap_or(false)
    }

    fn close_project(&mut self) {
        if let Some(mut app) = self.app.take() {
            app.shutdown();
        }
    }

    fn open_project(&mut self, project: &Path, books: bool) {
        self.close_project();
        self.dirty = true;
        // A queue named on the command line opens the editor on it.
        let grid = self.first_open && self.cfg.grid;
        let explicit = self.first_open
            && (self.cfg.filter.book_id.is_some()
                || self.cfg.filter.set_name.is_some()
                || self.cfg.start_at.is_some()
                || self.cfg.grid);
        let books = books && !explicit;
        let mut filter = if self.first_open {
            self.cfg.filter.clone()
        } else {
            PageFilter::default()
        };
        let start_at = if self.first_open {
            self.cfg.start_at.clone()
        } else {
            None
        };
        self.first_open = false;
        // Reopen the book the reviewer was last in.
        if filter.book_id.is_none() && filter.set_name.is_none() {
            filter.book_id = database::open(project)
                .ok()
                .and_then(|c| database::meta_get(&c, "last_book").ok().flatten())
                .filter(|b| !b.is_empty());
        }
        let config = AppConfig {
            project: project.to_path_buf(),
            filter,
            start_at,
            prediction_threshold: self.cfg.prediction_threshold,
        };
        match AnnotatorApp::new(config) {
            Ok(mut app) => {
                if let Ok(w) = self.wake_slot.lock() {
                    if let Some(w) = w.as_ref() {
                        app.attach(w.clone());
                    }
                }
                if books || app.keys.is_empty() {
                    app.show_books();
                } else if grid {
                    app.show_grid();
                }
                recent::touch(project);
                self.recent = recent::load();
                self.app = Some(app);
            }
            Err(e) => {
                self.error = Some(format!("{}: {e}", project.display()));
            }
        }
    }

    fn start_import(&mut self, root: PathBuf, project: PathBuf, creating: bool) {
        self.close_project();
        let (tx, rx) = channel();
        let slot = self.wake_slot.clone();
        let opts = self.cfg.folder.clone();
        let (r, p) = (root.clone(), project.clone());
        let spawned = std::thread::Builder::new()
            .name("annotator-import".into())
            .spawn(move || {
                let result = (|| {
                    let mut conn = database::open(&p)?;
                    let mut last = Instant::now();
                    let mut progress = |stage: &str, done: u64, total: u64| {
                        // At most ~30 updates a second reach the window.
                        if last.elapsed() >= Duration::from_millis(33) || done == total {
                            last = Instant::now();
                            let _ = tx.send(JobMsg::Progress(stage.to_owned(), done, total));
                            wake(&slot);
                        }
                    };
                    import_folder::sync_folder(&mut conn, &r, &opts, &mut progress)
                })();
                if result.is_err() && creating {
                    import_folder::remove_project(&p);
                }
                let _ = tx.send(JobMsg::Done(result));
                wake(&slot);
            });
        match spawned {
            Ok(_) => {
                self.job = Some(ImportJob {
                    root,
                    project,
                    creating,
                    rx,
                    stage: "Starting".into(),
                    done: 0,
                    total: 0,
                    started: Instant::now(),
                });
                self.title = Some("Liminal Annotator — importing".into());
            }
            Err(e) => self.error = Some(format!("could not start the import: {e}")),
        }
        self.dirty = true;
    }

    fn pick_folder(&mut self) {
        if self.picker.is_some() || self.job.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let slot = self.wake_slot.clone();
        let start = self
            .app
            .as_ref()
            .and_then(|a| a.project_root())
            .and_then(|r| r.parent().map(Path::to_path_buf))
            .or_else(|| {
                self.recent
                    .first()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent())
                    .map(Path::to_path_buf)
            });
        let _ = std::thread::Builder::new()
            .name("annotator-folder-dialog".into())
            .spawn(move || {
                let mut d = rfd::FileDialog::new().set_title("Open a dataset folder");
                if let Some(dir) = start.filter(|d| d.is_dir()) {
                    d = d.set_directory(dir);
                }
                let _ = tx.send(d.pick_folder());
                wake(&slot);
            });
        self.picker = Some(rx);
    }

    /// Background results: the folder dialog, the import.
    fn poll(&mut self) {
        if let Some(rx) = &self.picker {
            if let Ok(choice) = rx.try_recv() {
                self.picker = None;
                if let Some(dir) = choice {
                    self.open_path(&dir, true);
                }
                self.dirty = true;
            }
        }
        let mut finished = None;
        if let Some(job) = &mut self.job {
            while let Ok(msg) = job.rx.try_recv() {
                self.dirty = true;
                match msg {
                    JobMsg::Progress(stage, done, total) => {
                        job.stage = stage;
                        job.done = done;
                        job.total = total;
                    }
                    JobMsg::Done(r) => finished = Some(r),
                }
            }
        }
        if let Some(result) = finished {
            let job = self.job.take();
            match (result, job) {
                (Ok(report), Some(job)) => {
                    let secs = job.started.elapsed().as_secs_f64();
                    self.open_project(&job.project, true);
                    if let Some(app) = &mut self.app {
                        let what = if job.creating {
                            "Project created"
                        } else {
                            "Folder rescanned"
                        };
                        app.notify(&format!("{what} in {secs:.0} s · {report}"), false);
                    }
                }
                (Err(e), Some(job)) => {
                    self.error = Some(format!("{}: {e}", job.root.display()));
                    // A rescan that failed leaves the project as it was: reopen it.
                    if !job.creating && job.project.is_file() {
                        let msg = self.error.clone().unwrap_or_default();
                        self.open_project(&job.project, true);
                        if let Some(app) = &mut self.app {
                            app.notify(&msg, true);
                        }
                    }
                }
                _ => {}
            }
        }
        // Requests the editor cannot carry out itself.
        let request = self.app.as_mut().and_then(|a| a.request.take());
        match request {
            Some(ShellRequest::OpenFolder) => self.pick_folder(),
            Some(ShellRequest::Rescan) => {
                let target = self
                    .app
                    .as_ref()
                    .map(|a| (a.project_root(), a.project_path.clone()));
                match target {
                    Some((Some(root), project)) => self.start_import(root, project, false),
                    Some((None, _)) => {
                        if let Some(app) = &mut self.app {
                            app.notify(
                                "This project was not made from a folder, so there is nothing to rescan",
                                false,
                            );
                        }
                    }
                    None => {}
                }
            }
            None => {}
        }
    }

    fn draw_import(&mut self, buffer: &mut WindowBuffer) {
        let s = self.scale;
        let (w, h) = (buffer.width as i32, buffer.height as i32);
        let mut p = Painter::new(buffer);
        p.fill_rect(Rect::new(0, 0, w, h), HUD_BG);
        let Some(job) = &self.job else { return };
        let title = ui::style(s, 17.0);
        let body = ui::style(s, 13.0);
        let pw = s.px(720).min(w - s.px(40));
        let x = (w - pw) / 2;
        let mut y = h / 2 - s.px(90);
        let head = if job.creating {
            "Creating a project from the folder"
        } else {
            "Importing what is new in the folder"
        };
        self.text.draw(&mut p, head, x, y, title, HUD_TEXT);
        y += self.text.line_height(title) + s.px(8);
        let lh = self.text.line_height(body);
        ui::cell(
            &mut p,
            &mut self.text,
            &job.root.display().to_string(),
            x,
            y,
            pw,
            lh,
            body,
            HUD_DIM,
            Align::Left,
        );
        y += lh + s.px(18);
        let bar = Rect::new(x, y, pw, s.px(10));
        p.fill_rect(bar, PANEL_BG);
        outline(&mut p, bar, HUD_EDGE, 1);
        if job.total > 0 {
            let f = (job.done as f64 / job.total as f64).clamp(0.0, 1.0);
            p.fill_rect(
                Rect::new(
                    bar.x + 1,
                    bar.y + 1,
                    ((bar.w - 2) as f64 * f) as i32,
                    bar.h - 2,
                ),
                ACCENT,
            );
        }
        y += bar.h + s.px(12);
        let secs = job.started.elapsed().as_secs();
        let detail = if job.total > 0 {
            let pct = job.done * 100 / job.total.max(1);
            if job.stage.contains("detections") {
                format!(
                    "{} · {pct}% ({} of {} MB) · {secs} s",
                    job.stage,
                    job.done / 1_000_000,
                    job.total / 1_000_000
                )
            } else {
                format!("{} · {} of {} · {secs} s", job.stage, job.done, job.total)
            }
        } else {
            format!("{} · {secs} s", job.stage)
        };
        ui::cell(
            &mut p,
            &mut self.text,
            &detail,
            x,
            y,
            pw,
            lh,
            body,
            HUD_TEXT,
            Align::Left,
        );
        y += lh + s.px(10);
        ui::cell(
            &mut p,
            &mut self.text,
            "Closing the window is safe: what was imported is kept, and opening the folder again continues.",
            x,
            y,
            pw,
            lh,
            ui::style(s, 11.5),
            HUD_DIM,
            Align::Left,
        );
    }

    fn draw_start(&mut self, buffer: &mut WindowBuffer) {
        self.hits.clear();
        let s = self.scale;
        let (w, h) = (buffer.width as i32, buffer.height as i32);
        let mut p = Painter::new(buffer);
        p.fill_rect(Rect::new(0, 0, w, h), HUD_BG);
        let title = ui::style(s, 20.0);
        let body = ui::style(s, 13.0);
        let small = ui::style(s, 11.5);
        let pw = s.px(760).min(w - s.px(40));
        let x = (w - pw) / 2;
        let mut y = s.px(60).max(h / 6);
        self.text
            .draw(&mut p, "Liminal Annotator", x, y, title, HUD_TEXT);
        y += self.text.line_height(title) + s.px(18);
        let r = ui::button(
            &mut p,
            &mut self.text,
            s,
            x,
            y,
            "Open a dataset folder…  Ctrl+O",
            body,
            ACCENT,
            Some(ROW_SELECTED),
        );
        self.hits.push((r, Hit::OpenFolder));
        y = r.bottom() + s.px(12);
        let lh = self.text.line_height(small);
        for line in [
            "Or drop a folder, or a project .db, onto this window.",
            "A folder with teachers_raw.jsonl and images/<book>/ (the layout dataset pool), a COCO data folder",
            "(annotations/*.json), or plain folders of page images. The project is created inside it as annotator.db.",
        ] {
            ui::cell(
                &mut p,
                &mut self.text,
                line,
                x,
                y,
                pw,
                lh,
                small,
                HUD_DIM,
                Align::Left,
            );
            y += lh + s.px(4);
        }
        if let Some(e) = &self.error {
            y += s.px(10);
            let e = e.clone();
            ui::cell(
                &mut p,
                &mut self.text,
                &e,
                x,
                y,
                pw,
                lh,
                small,
                BAD,
                Align::Left,
            );
            y += lh;
        }
        if self.picker.is_some() {
            y += s.px(10);
            ui::cell(
                &mut p,
                &mut self.text,
                "Choosing a folder…",
                x,
                y,
                pw,
                lh,
                small,
                HUD_TEXT,
                Align::Left,
            );
            y += lh;
        }
        if !self.recent.is_empty() {
            y += s.px(24);
            self.text
                .draw(&mut p, "Recent projects", x, y, body, HUD_TEXT);
            y += self.text.line_height(body) + s.px(8);
            let row_h = self.text.line_height(body) + s.px(10);
            for (i, path) in self.recent.clone().iter().enumerate() {
                if y + row_h > h - s.px(10) {
                    break;
                }
                let r = Rect::new(x, y, pw, row_h);
                outline(&mut p, r, HUD_EDGE, 1);
                let label = match path.parent() {
                    Some(dir) if path.file_name().is_some_and(|n| n == PROJECT_FILE) => {
                        dir.display().to_string()
                    }
                    _ => path.display().to_string(),
                };
                ui::cell(
                    &mut p,
                    &mut self.text,
                    &label,
                    x + s.px(10),
                    y + s.px(5),
                    pw - s.px(20),
                    row_h,
                    body,
                    HUD_TEXT,
                    Align::Left,
                );
                self.hits.push((r, Hit::Recent(i)));
                y += row_h + s.px(4);
            }
        }
    }

    fn start_click(&mut self) {
        let (x, y) = (self.cursor.0 as i32, self.cursor.1 as i32);
        match self
            .hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| *h)
        {
            Some(Hit::OpenFolder) => self.pick_folder(),
            Some(Hit::Recent(i)) => {
                if let Some(p) = self.recent.get(i).cloned() {
                    self.open_path(&p, true);
                }
            }
            None => {}
        }
        self.dirty = true;
    }
}

impl PixelApp for Shell {
    fn render(&mut self, buffer: &mut WindowBuffer, scale: Scale) {
        if scale != self.scale {
            self.scale = scale;
            self.dirty = true;
        }
        self.poll();
        if self.job.is_some() {
            self.draw_import(buffer);
            return;
        }
        if let Some(app) = &mut self.app {
            app.render(buffer, scale);
            // A request raised during this frame (a click on a button) is
            // served now rather than at the next event.
            if app.request.is_some() {
                self.poll();
                if self.job.is_some() {
                    self.draw_import(buffer);
                }
            }
            return;
        }
        let size = (buffer.width, buffer.height);
        if self.dirty || size != self.last_size {
            self.dirty = false;
            self.last_size = size;
            self.draw_start(buffer);
        }
    }

    fn on_scale(&mut self, scale: Scale) {
        self.scale = scale;
        self.dirty = true;
        if let Some(app) = &mut self.app {
            app.on_scale(scale);
        }
    }

    fn attach(&mut self, waker: Waker) {
        if let Ok(mut w) = self.wake_slot.lock() {
            *w = Some(waker.clone());
        }
        if let Some(app) = &mut self.app {
            app.attach(waker);
        }
    }

    fn animation_interval(&self) -> Option<Duration> {
        if self.job.is_some() {
            return Some(Duration::from_millis(500));
        }
        self.app.as_ref().and_then(|a| a.animation_interval())
    }

    fn tick(&mut self) {
        self.poll();
        if let Some(app) = &mut self.app {
            app.tick();
        }
    }

    fn on_key_event(&mut self, event: &KeyEvent) {
        if self.job.is_some() {
            return;
        }
        if let Some(app) = &mut self.app {
            app.on_key_event(event);
            if app.request.is_some() {
                self.poll();
            }
            return;
        }
        if !event.pressed {
            return;
        }
        let ctrl = event.modifiers.control || event.modifiers.logo;
        match event.key {
            KeyInput::Character(c) if ctrl && c.eq_ignore_ascii_case(&'o') => self.pick_folder(),
            KeyInput::Enter => {
                if let Some(p) = self.recent.first().cloned() {
                    self.open_path(&p, true);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn on_focus_changed(&mut self, focused: bool) {
        self.dirty = true;
        if let Some(app) = &mut self.app {
            app.on_focus_changed(focused);
        }
    }

    fn poll_title(&mut self) -> Option<String> {
        if let Some(app) = &mut self.app {
            if self.job.is_none() {
                if let Some(t) = app.poll_title() {
                    return Some(t);
                }
            }
        }
        self.title.take()
    }

    fn poll_fullscreen(&mut self) -> Option<bool> {
        self.app.as_mut().and_then(|a| a.poll_fullscreen())
    }

    fn cursor_shape(&self) -> CursorShape {
        match &self.app {
            Some(app) if self.job.is_none() => app.cursor_shape(),
            _ => CursorShape::Default,
        }
    }

    fn on_exit(&mut self) {
        self.close_project();
    }

    fn on_host_error(&mut self, error: &str) {
        eprintln!("liminal-annotator: host error: {error}");
        self.close_project();
    }

    fn on_modifiers(&mut self, modifiers: Modifiers) {
        if let Some(app) = &mut self.app {
            app.on_modifiers(modifiers);
        }
    }

    fn on_cursor(&mut self, x: f32, y: f32) {
        self.cursor = (x, y);
        if let Some(app) = &mut self.app {
            app.on_cursor(x, y);
        }
    }

    fn on_cursor_left(&mut self) {
        if let Some(app) = &mut self.app {
            app.on_cursor_left();
        }
    }

    fn on_mouse(&mut self, button: MouseButton, pressed: bool) {
        if self.job.is_some() {
            return;
        }
        if let Some(app) = &mut self.app {
            app.on_mouse(button, pressed);
            return;
        }
        if pressed && button == MouseButton::Left {
            self.start_click();
        }
    }

    fn on_file_drop(&mut self, path: PathBuf) {
        if self.job.is_some() {
            return;
        }
        self.open_path(&path, true);
    }

    fn on_scroll_event(&mut self, event: &ScrollEvent) {
        if self.job.is_some() {
            return;
        }
        if let Some(app) = &mut self.app {
            app.on_scroll_event(event);
        }
    }

    fn should_exit(&self) -> bool {
        self.exit || self.app.as_ref().is_some_and(|a| a.should_exit())
    }
}

/// Run the annotator window.
pub fn run(cfg: ShellConfig) -> Result<(), String> {
    let shell = Shell::new(cfg);
    let wc = liminalkit_windowing::WindowConfig::new("Liminal Annotator", 1360.0, 900.0)
        .min_size(720.0, 480.0);
    liminalkit_windowing::run_app(shell, wc).map_err(|e| e.to_string())
}
