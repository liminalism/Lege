use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use pixelkit_raster::{Painter, RasterKernel, Rect, WindowBuffer};
use pixelkit_shell::{Input, KeyInput, Modifiers, MouseButton, Scale};
use pixelkit_text::{Align, TextCache};
use pixelkit_ui::{Focus, ScrollState, TextFieldState, Ui};
use pixelkit_windowing::{ImeCursorArea, ImeEvent, PixelApp, Waker};

use crate::appearance::{self, ThemeChoice};
use crate::backend;
use crate::gui_text::{GUI_TEXT, tr};
use crate::layout::{Actions, Bands, LeftCard, RightCard, cells};
use crate::models::{
    CompressionType, DocumentItem, ImageProcessingType, OcrMode, OutputFormat, ProcessingOptions,
    ResolutionPreset,
};
use crate::worker_process::{
    WorkerHandle, WorkerProcessingStatus, WorkerProgressMetrics, WorkerProgressMode,
    WorkerProgressUpdate,
};

pub struct LegeApp {
    pub dashboard: Dashboard,
    text: TextCache,
    kernel: RasterKernel,
    input: Input,
    focus: Focus,
    modal_focus: Focus,
    pub snapshot_mode: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modal {
    About,
    Theme,
    Queue,
    Log,
    #[cfg(feature = "debug-logging")]
    Debug,
}

enum Message {
    Inputs(Vec<(PathBuf, Option<u32>, Option<bool>)>),
    Output(PathBuf),
    Notice(String),
    DialogClosed,
    Started(Vec<backend::TrackerInfo>, StartedWorkers),
    Worker(WorkerProgressUpdate),
    Finished,
}

/// A startup event may be queued when the window closes. Its worker ownership
/// must cancel on discard, including a successful send followed by receiver drop.
struct StartedWorkers(Vec<WorkerHandle>);
impl StartedWorkers {
    fn take(mut self) -> Vec<WorkerHandle> {
        std::mem::take(&mut self.0)
    }
}
impl Drop for StartedWorkers {
    fn drop(&mut self) {
        for handle in &mut self.0 {
            handle.kill();
        }
    }
}

struct Notice {
    text: String,
    until: Instant,
}
struct QueueRow {
    path: PathBuf,
    output: Option<PathBuf>,
    status: &'static str,
}

pub struct Dashboard {
    pub options: ProcessingOptions,
    queue: VecDeque<DocumentItem>,
    rows: Vec<QueueRow>,
    range: TextFieldState,
    resolution: TextFieldState,
    k: TextFieldState,
    threshold: TextFieldState,
    last_binarization_fixed: bool,
    status: (String, String, String),
    metrics: Option<WorkerProgressMetrics>,
    running: bool,
    cancelling: bool,
    handled: HashSet<u64>,
    handles: Vec<WorkerHandle>,
    notices: VecDeque<Notice>,
    modal: Option<Modal>,
    modal_scroll: ScrollState,
    log: Vec<crate::logging::LogEntry>,
    theme_index: usize,
    modal_error: Option<String>,
    dialog_open: bool,
    sender: SyncSender<Message>,
    receiver: Receiver<Message>,
    waker: Waker,
}

impl LegeApp {
    pub fn new() -> Self {
        appearance::initialize_runtime_appearance();
        Self {
            dashboard: Dashboard::new(),
            // Status text and queue paths change continuously. Keep their
            // raster cache bounded instead of retaining every past label.
            text: TextCache::with_limits(crate::theme::fonts(), 512, 2 * 1024 * 1024),
            kernel: RasterKernel::new(),
            input: Input::new(),
            focus: Focus::new(),
            modal_focus: Focus::new(),
            snapshot_mode: false,
        }
    }
}

impl PixelApp for LegeApp {
    fn render(&mut self, buffer: &mut WindowBuffer, scale: Scale) {
        self.dashboard.poll();
        let palette = appearance::active_palette();
        let theme = crate::theme::theme(palette, scale);
        let bands = Bands::new(buffer.width as i32, buffer.height as i32, scale);
        let mut background_input = Input::new();
        let modal = self.dashboard.modal.is_some();
        {
            let input = if modal {
                &mut background_input
            } else {
                &mut self.input
            };
            let mut ui = Ui::new(
                Painter::new(buffer),
                &mut self.text,
                input,
                theme,
                scale,
                &mut self.kernel,
            )
            .with_focus(&mut self.focus);
            ui.painter.clear(crate::theme::rgb(palette.app_bg));
            self.dashboard.draw(&mut ui, &bands, self.snapshot_mode);
        }
        if self.dashboard.modal.is_some() {
            let mut ui = Ui::new(
                Painter::new(buffer),
                &mut self.text,
                &mut self.input,
                theme,
                scale,
                &mut self.kernel,
            )
            .with_focus(&mut self.modal_focus);
            self.dashboard.draw_modal(&mut ui);
        }
        self.input.end_frame();
    }
    fn attach(&mut self, waker: Waker) {
        self.dashboard.waker = waker;
    }
    fn on_layout_changed(&mut self) {
        self.focus.clear();
        self.modal_focus.clear();
        self.input.cancel_pointer();
    }
    fn on_native_window(&mut self, window: &winit::window::Window) {
        if let Ok(bitmap) =
            pixelkit_raster::png::decode(include_bytes!("../../../../lege-misc/assets/icon.png"))
        {
            let rgba = bitmap
                .pixels
                .iter()
                .flat_map(|p| {
                    [
                        ((p >> 16) & 255) as u8,
                        ((p >> 8) & 255) as u8,
                        (p & 255) as u8,
                        (p >> 24) as u8,
                    ]
                })
                .collect();
            if let Ok(icon) = winit::window::Icon::from_rgba(rgba, bitmap.width, bitmap.height) {
                window.set_window_icon(Some(icon));
            }
        }
    }
    fn animation_interval(&self) -> Option<Duration> {
        // Worker/dialog threads wake the host when data arrives. Idle windows
        // have no polling timer; only timed notifications need a deadline.
        (!self.dashboard.notices.is_empty()).then_some(Duration::from_millis(100))
    }
    fn tick(&mut self) {
        self.dashboard.poll();
    }
    fn on_key(&mut self, key: &KeyInput) {
        if self.input.modifiers().accel()
            && matches!(key, KeyInput::Character('c' | 'v' | 'x' | 'C' | 'V' | 'X'))
            && self.input.clipboard().is_none()
        {
            // An unused clipboard should cost neither a thread nor its native
            // connection. Start the owner on the first clipboard shortcut.
            let waker = self.dashboard.waker.clone();
            self.input
                .attach_clipboard(pixelkit_shell::clipboard::ClipboardWorker::new(move || {
                    waker.wake()
                }));
        }
        if *key == KeyInput::Escape && self.dashboard.modal.is_some() {
            self.dashboard.close_modal();
            self.modal_focus.clear();
            return;
        }
        if self.dashboard.modal == Some(Modal::Theme) {
            match key {
                KeyInput::Left => self.dashboard.step_theme(-1),
                KeyInput::Right => self.dashboard.step_theme(1),
                KeyInput::Up | KeyInput::Down => self.dashboard.flip_theme(),
                KeyInput::Enter => self.dashboard.accept_theme(),
                _ => self.input.key(key.clone()),
            }
        } else {
            self.input.key(key.clone());
        }
    }
    fn on_modifiers(&mut self, m: Modifiers) {
        self.input.modifiers_changed(m);
    }
    fn on_cursor(&mut self, x: f32, y: f32) {
        self.input.cursor_moved(x, y);
    }
    fn on_cursor_left(&mut self) {
        self.input.cursor_left();
    }
    fn on_mouse(&mut self, b: MouseButton, pressed: bool) {
        self.input.mouse(b, pressed);
    }
    fn on_scroll(&mut self, dx: f32, dy: f32) {
        self.input.scrolled(dx, dy);
    }
    fn on_file_drop(&mut self, path: PathBuf) {
        if !self.dashboard.running && !self.dashboard.dialog_open {
            self.dashboard.probe_paths(vec![path]);
        }
    }
    fn on_focus_changed(&mut self, focused: bool) {
        if !focused {
            self.input.blur();
        }
    }
    fn on_ime(&mut self, event: &ImeEvent) {
        match event {
            ImeEvent::Preedit(text, _) => self.input.set_composition(text),
            ImeEvent::Commit(text) => {
                self.input.set_composition("");
                for c in text.chars() {
                    self.input.key(KeyInput::Character(c));
                }
            }
            ImeEvent::Disabled => self.input.set_composition(""),
            ImeEvent::Enabled => {}
        }
    }
    fn ime_allowed(&self) -> bool {
        self.ime_cursor_area().is_some()
    }
    fn ime_cursor_area(&self) -> Option<ImeCursorArea> {
        if self.dashboard.modal.is_some() || self.dashboard.running {
            return None;
        }
        [
            &self.dashboard.range,
            &self.dashboard.resolution,
            &self.dashboard.k,
            &self.dashboard.threshold,
        ]
        .iter()
        .find_map(|field| field.caret_area())
        .map(|area| ImeCursorArea {
            x: area.x as f64,
            y: area.y as f64,
            width: area.w as f64,
            height: area.h as f64,
        })
    }
    fn on_exit(&mut self) {
        self.dashboard.cancel();
        appearance::set_theme_preview(None);
    }
}

impl Dashboard {
    fn new() -> Self {
        let mut options = backend::load_saved_settings()
            .ok()
            .flatten()
            .unwrap_or_else(ProcessingOptions::new);
        if options.output_format == OutputFormat::Epub {
            options.output_format = OutputFormat::Pdf;
        }
        if let Ok(Some(preset)) = backend::load_resolution_preset() {
            options.target_height = Some(preset.height);
            options.target_width = preset.width;
        }
        options.normalize_processing_dependencies();
        let resolution = match options.target_width {
            Some(w) => format!("{}x{w}", options.target_height.unwrap_or(1200)),
            None => options.target_height.unwrap_or(1200).to_string(),
        };
        let (sender, receiver) = mpsc::sync_channel(64);
        Self {
            resolution: TextFieldState::with_text(resolution),
            range: TextFieldState::with_text(options.page_range.clone().unwrap_or_default()),
            k: TextFieldState::with_text(options.k_factor.to_string()),
            threshold: TextFieldState::with_text(options.threshold_value.to_string()),
            last_binarization_fixed: options.use_fixed_threshold,
            options,
            queue: VecDeque::new(),
            rows: Vec::new(),
            status: (String::new(), String::new(), String::new()),
            metrics: None,
            running: false,
            cancelling: false,
            handled: HashSet::new(),
            handles: Vec::new(),
            notices: VecDeque::new(),
            modal: None,
            modal_scroll: ScrollState::new(),
            log: crate::logging::load_log_entries().unwrap_or_default(),
            theme_index: 0,
            modal_error: None,
            dialog_open: false,
            sender,
            receiver,
            waker: Waker::detached(),
        }
    }
    fn notice(&mut self, text: impl Into<String>, seconds: u64) {
        while self.notices.len() >= 2 {
            self.notices.pop_front();
        }
        self.notices.push_back(Notice {
            text: text.into(),
            until: Instant::now() + Duration::from_secs(seconds),
        });
    }
    fn poll(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                Message::Inputs(paths) => {
                    if !self.running {
                        self.rows.clear();
                    }
                    for (path, pages, has_ocr) in paths {
                        if self.queue.iter().any(|item| item.file_path == path) {
                            continue;
                        }
                        if self.options.output_path.is_none() {
                            self.options.output_path = Some(if path.is_dir() {
                                path.clone()
                            } else {
                                path.parent()
                                    .unwrap_or(std::path::Path::new("."))
                                    .to_owned()
                            });
                            self.options.output_path_is_auto = true;
                            self.notice(
                                GUI_TEXT.interactive.messages.defaulted_output_dir.clone(),
                                5,
                            );
                        }
                        let mut item = DocumentItem::new(path);
                        item.page_count = pages;
                        self.queue.push_back(item);
                        if let Some(found) = has_ocr {
                            self.notice(
                                if found {
                                    GUI_TEXT.interactive.popups.ocr_detected.clone()
                                } else {
                                    GUI_TEXT.interactive.popups.no_ocr_detected.clone()
                                },
                                6,
                            );
                        }
                    }
                }
                Message::Output(path) => {
                    self.options.output_path = Some(path);
                    self.options.output_path_is_auto = false;
                }
                Message::Notice(text) => self.notice(text, 8),
                Message::DialogClosed => self.dialog_open = false,
                Message::Started(infos, handles) => {
                    self.rows = infos
                        .into_iter()
                        .map(|info| QueueRow {
                            path: info.input_path,
                            output: Some(info.output_path),
                            status: tr("queued"),
                        })
                        .collect();
                    self.handles = handles.take();
                    if self.cancelling {
                        for handle in &mut self.handles {
                            handle.kill();
                        }
                    }
                }
                Message::Worker(update) => self.worker_event(update),
                Message::Finished => {
                    self.running = false;
                    self.handles.clear();
                    if self.cancelling {
                        self.status = (
                            GUI_TEXT.interactive.status.cancelling.clone(),
                            String::new(),
                            String::new(),
                        );
                    }
                    self.log = crate::logging::load_log_entries().unwrap_or_default();
                }
            }
        }
        self.notices.retain(|notice| notice.until > Instant::now());
    }
    fn worker_event(&mut self, event: WorkerProgressUpdate) {
        let (id, metrics) = match &event {
            WorkerProgressUpdate::Status {
                task_id, metrics, ..
            }
            | WorkerProgressUpdate::Completed {
                task_id, metrics, ..
            }
            | WorkerProgressUpdate::Error {
                task_id, metrics, ..
            } => (*task_id, *metrics),
        };
        if self.handled.contains(&id) {
            return;
        }
        self.metrics = metrics.or(self.metrics);
        match event {
            WorkerProgressUpdate::Status { status, .. } => {
                if !self.cancelling {
                    self.status = status.to_gui_display_lines();
                }
                if let Some(row) = self.rows.get_mut(id as usize) {
                    row.status = tr("processing");
                }
                if let WorkerProcessingStatus::FootnotesDetected { message }
                | WorkerProcessingStatus::MarginAnalysisSummary { summary: message } = status
                {
                    self.notice(message, 6);
                }
            }
            WorkerProgressUpdate::Completed { message, .. } => {
                self.handled.insert(id);
                if self.cancelling {
                    if let Some(row) = self.rows.get_mut(id as usize) {
                        row.status = tr("failed");
                    }
                    return;
                }
                self.status = (
                    GUI_TEXT.interactive.status.complete.clone(),
                    message.clone(),
                    String::new(),
                );
                self.notice(message, 6);
                if let Some(row) = self.rows.get_mut(id as usize) {
                    row.status = tr("completed");
                    let path = row.path.clone();
                    self.queue.retain(|item| item.file_path != path);
                }
            }
            WorkerProgressUpdate::Error { error, .. } => {
                self.handled.insert(id);
                self.status = (tr("failed").into(), error.clone(), String::new());
                self.notice(error, 8);
                if let Some(row) = self.rows.get_mut(id as usize) {
                    row.status = tr("failed");
                }
            }
        }
    }
    fn dialog(&mut self, kind: u8) {
        if self.running || self.dialog_open {
            return;
        }
        self.dialog_open = true;
        let tx = self.sender.clone();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            match kind {
                0 => {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter(
                            &GUI_TEXT.interactive.popups.supported_inputs_filter,
                            &["pdf", "zip"],
                        )
                        .pick_files()
                    {
                        probe(tx.clone(), wake.clone(), paths);
                    }
                }
                1 => {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        probe(tx.clone(), wake.clone(), vec![path]);
                    }
                }
                _ => {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        let _ = tx.send(Message::Output(path));
                    }
                }
            }
            let _ = tx.send(Message::DialogClosed);
            wake.wake();
        });
    }
    fn probe_paths(&mut self, paths: Vec<PathBuf>) {
        self.dialog_open = true;
        let tx = self.sender.clone();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            probe(tx.clone(), wake.clone(), paths);
            let _ = tx.send(Message::DialogClosed);
            wake.wake();
        });
    }
    fn cancel(&mut self) {
        if self.running {
            self.cancelling = true;
            self.status.0 = GUI_TEXT.interactive.status.cancelling.clone();
            for handle in &mut self.handles {
                handle.kill();
            }
        }
    }
    fn start(&mut self) {
        if self.running {
            self.cancel();
            return;
        }
        if self.queue.is_empty() {
            self.notice(GUI_TEXT.interactive.status.no_files_in_queue.clone(), 3);
            return;
        }
        if self.options.output_path.is_none() {
            self.notice(
                GUI_TEXT.interactive.status.choose_output_directory.clone(),
                3,
            );
            return;
        }
        if let Err(error) = self.validate_fields() {
            self.notice(error, 6);
            return;
        }
        let counts: Vec<_> = self
            .queue
            .iter()
            .filter_map(|item| item.page_count)
            .collect();
        if self
            .options
            .page_range
            .as_deref()
            .is_some_and(|range| backend::validate_page_range_for_totals(range, &counts).is_err())
        {
            self.notice(tr("invalid_page_range"), 6);
            return;
        }
        self.running = true;
        self.cancelling = false;
        self.handled.clear();
        self.metrics = None;
        self.rows.clear();
        self.status = (
            GUI_TEXT.interactive.status.starting.clone(),
            String::new(),
            String::new(),
        );
        let options = self.options.clone();
        let queue = self.queue.clone();
        let tx = self.sender.clone();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(4)
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| {
                    runtime.block_on(backend::start_async_processing(queue, &options))
                });
            match result {
                Ok((infos, handles, events)) => {
                    if tx
                        .send(Message::Started(infos, StartedWorkers(handles)))
                        .is_err()
                    {
                        return;
                    }
                    wake.wake();
                    while let Ok(event) = events.recv() {
                        if tx.send(Message::Worker(event)).is_err() {
                            break;
                        }
                        wake.wake();
                    }
                }
                Err(error) => {
                    let _ = tx.send(Message::Notice(format!("{error:#}")));
                }
            }
            let _ = tx.send(Message::Finished);
            wake.wake();
        });
    }
    fn validate_fields(&mut self) -> Result<(), &'static str> {
        let (height, width) = if self.resolution.text().trim().is_empty() {
            (
                self.options.target_height.unwrap_or(1200),
                self.options.target_width,
            )
        } else {
            parse_resolution(self.resolution.text()).ok_or(tr("invalid_resolution"))?
        };
        self.options.target_height = Some(height);
        self.options.target_width = if self.options.crop_free_aspect {
            None
        } else {
            width
        };
        self.options.page_range =
            (!self.range.text().trim().is_empty()).then(|| self.range.text().trim().to_owned());
        if !self.options.grayscale_mode && !self.options.preserve_mode {
            if self.options.use_fixed_threshold {
                self.options.threshold_value = self
                    .threshold
                    .text()
                    .parse()
                    .map_err(|_| tr("invalid_threshold"))?;
            } else if self.options.use_custom_adaptive {
                let k: f32 = self.k.text().parse().map_err(|_| tr("invalid_k"))?;
                if !k.is_finite() || !(0.0..=1.0).contains(&k) {
                    return Err(tr("invalid_k"));
                }
                self.options.k_factor = k;
            }
        }
        self.options.normalize_processing_dependencies();
        Ok(())
    }
    fn close_modal(&mut self) {
        appearance::set_theme_preview(None);
        self.modal = None;
        self.modal_error = None;
    }
    fn choose_theme(&mut self) {
        self.theme_index = match appearance::active_theme_choice() {
            ThemeChoice::Default => 0,
            ThemeChoice::Sanzo { index, dark } => 1 + index * 2 + usize::from(dark),
        };
        self.modal = Some(Modal::Theme);
    }
    fn preview_theme(&mut self) {
        appearance::set_theme_preview(Some(if self.theme_index == 0 {
            ThemeChoice::Default
        } else {
            ThemeChoice::Sanzo {
                index: (self.theme_index - 1) / 2,
                dark: (self.theme_index - 1) % 2 == 1,
            }
        }));
    }
    fn step_theme(&mut self, delta: i32) {
        self.theme_index = (self.theme_index as i32 + delta)
            .rem_euclid((1 + crate::sanzowada::THEME_COUNT * 2) as i32)
            as usize;
        self.preview_theme();
    }
    fn flip_theme(&mut self) {
        if self.theme_index > 0 {
            self.theme_index = if self.theme_index % 2 == 1 {
                self.theme_index + 1
            } else {
                self.theme_index - 1
            };
            self.preview_theme();
        }
    }
    fn accept_theme(&mut self) {
        let choice = appearance::active_theme_choice();
        if let Err(error) = appearance::commit_theme_choice(choice) {
            self.notice(error.to_string(), 8);
        } else {
            self.modal = Some(Modal::About);
        }
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn probe(tx: SyncSender<Message>, wake: Waker, paths: Vec<PathBuf>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(2)
        .build()
    else {
        return;
    };
    let mut accepted = Vec::new();
    for path in paths {
        if path.is_dir() {
            match backend::get_image_files_in_directory(&path) {
                Ok(images) if !images.is_empty() => {
                    accepted.push((path, Some(images.len() as u32), None))
                }
                _ => {
                    let _ = tx.send(Message::Notice(
                        GUI_TEXT
                            .interactive
                            .popups
                            .folder_no_supported_images
                            .clone(),
                    ));
                    wake.wake();
                }
            }
        } else if backend::is_pdf_file(&path) || backend::is_zip_file(&path) {
            let data = runtime
                .block_on(crate::worker_process::probe_file_json(&path))
                .ok();
            let pages = data
                .as_ref()
                .and_then(|v| v.get("pages"))
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok());
            let ocr = data
                .as_ref()
                .and_then(|v| v.get("has_ocr"))
                .and_then(|v| v.as_bool());
            accepted.push((path, pages, ocr));
        } else {
            let _ = tx.send(Message::Notice(
                GUI_TEXT.interactive.tooltips.add_file_or_folder.clone(),
            ));
            wake.wake();
        }
    }
    let _ = tx.send(Message::Inputs(accepted));
    wake.wake();
}

fn parse_resolution(value: &str) -> Option<(u32, Option<u32>)> {
    let normalized = value.trim().replace(['x', 'X', '×'], " ");
    let values: Vec<_> = normalized.split_whitespace().collect();
    if values.is_empty() || values.len() > 2 {
        return None;
    }
    let height = values[0].parse::<u32>().ok().filter(|v| *v > 0)?;
    let width = if values.len() == 2 {
        Some(values[1].parse::<u32>().ok().filter(|v| *v > 0)?)
    } else {
        None
    };
    Some((height, width))
}

// All geometry below is in physical pixels, derived once from the host scale.
fn row(area: Rect, index: i32, height: i32) -> Rect {
    Rect::new(area.x, area.y + index * height, area.w, height)
}
fn label(ui: &mut Ui<'_>, area: Rect, text: &str) {
    ui.label(area, text, ui.theme.text, Align::Left);
}
fn small(ui: &mut Ui<'_>, area: Rect, text: &str) {
    let mut style = ui.theme.body;
    style.size *= 0.86;
    ui.label_styled(area, text, style, ui.theme.text_dim, Align::Left);
}
fn choice(ui: &mut Ui<'_>, area: Rect, text: &str, selected: bool) -> bool {
    let old = ui.theme.panel;
    if selected {
        ui.theme.panel = ui.theme.selection;
    }
    let clicked = ui.button(area, "");
    choice_label(ui, area, text);
    ui.theme.panel = old;
    clicked
}
fn tray(ui: &mut Ui<'_>, area: Rect) {
    ui.painter.rounded_rect(
        area,
        ui.theme.corner_radius,
        ui.theme.border_width,
        ui.theme.background,
        ui.theme.panel_edge,
    );
}
fn choice_label(ui: &mut Ui<'_>, area: Rect, text: &str) {
    let available = (area.w - ui.px(8)).max(1);
    let mut style = ui.theme.body;
    if ui.text.measure(text, style) <= available {
        ui.label_styled(area, text, style, ui.theme.text, Align::Centre);
        return;
    }
    // Keep equal hit areas at small widths. Two short lines are preferable to
    // clipping a mode name or assigning it a larger button than its siblings.
    if let Some((first, last)) = text.split_once(' ') {
        style.size = ui.scale.font(11.0);
        let width = ui
            .text
            .measure(first, style)
            .max(ui.text.measure(last, style));
        if width > available {
            style.size *= available as f32 / width as f32;
        }
        let h = ui.text.line_height(style).min(area.h / 2);
        let y = area.y + (area.h - 2 * h) / 2;
        for (i, line) in [first, last].into_iter().enumerate() {
            ui.label_styled(
                Rect::new(area.x, y + i as i32 * h, area.w, h),
                line,
                style,
                ui.theme.text,
                Align::Centre,
            );
        }
    } else {
        let width = ui.text.measure(text, style).max(1);
        style.size *= available as f32 / width as f32;
        ui.label_styled(area, text, style, ui.theme.text, Align::Centre);
    }
}
fn choices(ui: &mut Ui<'_>, area: Rect, texts: &[&str], selected: usize) -> Option<usize> {
    let gap = ui.px(4);
    let count = texts.len() as i32;
    let w = (area.w - gap * (count - 1)) / count;
    let mut result = None;
    for (i, text) in texts.iter().enumerate() {
        if choice(
            ui,
            Rect::new(area.x + i as i32 * (w + gap), area.y, w, area.h),
            text,
            i == selected,
        ) {
            result = Some(i);
        }
    }
    result
}
fn hover(
    ui: &Ui<'_>,
    area: Rect,
    text: &'static str,
    right: bool,
    note: &mut Option<(bool, &'static str)>,
) {
    if ui.input.hovering(area) {
        *note = Some((right, text));
    }
}

impl Dashboard {
    fn draw(&mut self, ui: &mut Ui<'_>, bands: &Bands, snapshot: bool) {
        let t = &GUI_TEXT.interactive;
        let mut note = None;
        // A running batch is a frozen options snapshot. Keep the UI visible
        // but route no editing input to file/settings controls until it ends.
        let frozen = self.running || self.dialog_open;
        let saved_input = frozen.then(|| std::mem::take(ui.input));
        let files = ui.panel(bands.files);
        let gap = ui.px(6);
        let w = (files.w - gap * 2) / 3;
        for (i, text, tooltip) in [
            (
                0,
                t.buttons.add_file.as_str(),
                t.tooltips.add_file_or_folder.as_str(),
            ),
            (
                1,
                t.buttons.add_folder.as_str(),
                t.tooltips.add_file_or_folder.as_str(),
            ),
            (
                2,
                t.buttons.output_directory.as_str(),
                t.tooltips.output_directory.as_str(),
            ),
        ] {
            let rect = Rect::new(files.x + i * (w + gap), files.y, w, files.h);
            if ui.button(rect, text) && !snapshot {
                self.dialog(i as u8);
            }
            hover(ui, rect, tooltip, i == 2, &mut note);
        }
        self.draw_left(ui, bands.left, &mut note, snapshot);
        self.draw_right(ui, bands.right, &mut note);
        if let Some(input) = saved_input {
            *ui.input = input;
        }
        let process = ui.panel(bands.process);
        let actions = Actions::new(process, ui.scale);
        let original_theme = ui.theme;
        ui.theme.body.size = ui.scale.font(16.0);
        ui.theme.panel = ui.theme.selection;
        ui.theme.panel_edge = crate::theme::rgb(appearance::active_palette().border_focus);
        let start = ui.button(
            actions.primary,
            if self.running {
                &t.buttons.cancel
            } else {
                &t.buttons.start_processing
            },
        );
        ui.theme = original_theme;
        if start && !snapshot {
            self.start();
        }
        ui.theme.body.size = ui.scale.font(12.0);
        let utilities = [
            (t.queue.about_button.clone(), Modal::About),
            (
                format!("{}: {}", tr("queue_short"), self.queue.len()),
                Modal::Queue,
            ),
            (t.queue.log_button.clone(), Modal::Log),
        ];
        for (i, (text, modal)) in utilities.into_iter().enumerate() {
            if ui.button(actions.utilities[i], &text) {
                self.modal = Some(modal);
                self.modal_scroll = ScrollState::new();
                if modal == Modal::Log {
                    self.log = crate::logging::load_log_entries().unwrap_or_default();
                }
            }
        }
        ui.theme = original_theme;
        ui.painter.fill_rect(
            Rect::new(
                process.x,
                actions.queue.y - ui.px(8),
                process.w,
                ui.scale.hairline(),
            ),
            ui.theme.panel_edge,
        );
        let count = if self.rows.is_empty() {
            self.queue.len()
        } else {
            self.rows.len()
        };
        let row_h = ui.px(20);
        let capacity = (actions.queue.h / row_h).max(1) as usize;
        let visible = if count > capacity {
            capacity.saturating_sub(1).max(1)
        } else {
            capacity
        };
        let queue_rows: Vec<_> = if self.rows.is_empty() {
            self.queue
                .iter()
                .take(visible)
                .map(|item| (tr("queued"), item.file_path.as_path()))
                .collect()
        } else {
            self.rows
                .iter()
                .take(visible)
                .map(|item| (item.status, item.path.as_path()))
                .collect()
        };
        for (index, (status, path)) in queue_rows.iter().enumerate() {
            let line = row(actions.queue, index as i32, row_h);
            let status_w = ui.px(130).min(line.w / 3);
            small(ui, Rect::new(line.x, line.y, status_w, line.h), status);
            label(
                ui,
                Rect::new(
                    line.x + status_w + ui.px(8),
                    line.y,
                    line.w - status_w - ui.px(8),
                    line.h,
                ),
                &path.to_string_lossy(),
            );
        }
        if count > queue_rows.len() {
            small(
                ui,
                row(actions.queue, queue_rows.len() as i32, row_h),
                &format!("+{} {}", count - queue_rows.len(), tr("more")),
            );
        } else if count == 0 {
            ui.label(
                actions.queue,
                &t.queue.empty_message,
                ui.theme.text_dim,
                Align::Centre,
            );
        }
        let status = ui.panel(bands.status);
        if self.running {
            label(ui, row(status, 0, ui.px(22)), &self.status.0);
            small(ui, row(status, 1, ui.px(20)), &self.status.1);
            let eta = self
                .metrics
                .and_then(|m| m.eta_seconds)
                .map(|seconds| {
                    format!(
                        "{}: {}:{:02}",
                        t.progress.eta.replace("{}", ""),
                        seconds / 60,
                        seconds % 60
                    )
                })
                .unwrap_or_else(|| self.status.2.clone());
            small(ui, row(status, 2, ui.px(20)), &eta);
            if let Some(metrics) = self.metrics {
                ui.meter(
                    Rect::new(status.x, status.bottom() - ui.px(22), status.w, ui.px(18)),
                    progress_fraction(metrics),
                    tr("progress"),
                    ui.theme.selection,
                );
            }
        } else if let Some((right, text)) = note {
            let w = ui.px(250).min(status.w);
            let area = Rect::new(
                if right { status.right() - w } else { status.x },
                status.y,
                w,
                status.h.min(ui.px(118)),
            );
            ui.surface(area, ui.theme.panel, ui.theme.panel_edge);
            wrapped(ui, area.inset(ui.px(7)), text, 6);
        } else {
            let summary = if self.queue.is_empty() {
                t.status.ready.clone()
            } else {
                let pages: Option<u64> = self
                    .queue
                    .iter()
                    .map(|item| item.page_count.map(u64::from))
                    .sum();
                match pages {
                    Some(total) => format!(
                        "{} {} · {total} {}",
                        self.queue.len(),
                        tr("queued"),
                        tr("pages")
                    ),
                    None => format!("{} {}", self.queue.len(), tr("queued")),
                }
            };
            ui.label(status, &summary, ui.theme.text_dim, Align::Centre);
            if let Some(output) = &self.options.output_path {
                small(
                    ui,
                    Rect::new(status.x, status.bottom() - ui.px(20), status.w, ui.px(18)),
                    &format!(
                        "{} {}",
                        t.buttons.output_directory,
                        backend::truncate_path(output, 90)
                    ),
                );
            }
        }
        #[cfg(feature = "debug-logging")]
        if ui.button(
            Rect::new(status.right() - ui.px(60), status.y, ui.px(60), ui.px(22)),
            &t.queue.debug_button,
        ) {
            self.modal = Some(Modal::Debug);
        }
        // At most two timed, click-to-dismiss cards, anchored bottom-right.
        let mut remove = None;
        let mut bottom = ui.painter.height() - ui.px(12);
        for (index, notice) in self.notices.iter().enumerate().rev() {
            let area = Rect::new(
                ui.painter.width() - ui.px(340),
                bottom - ui.px(68),
                ui.px(328),
                ui.px(68),
            );
            ui.surface(
                area,
                crate::theme::rgb(appearance::active_palette().info_bg),
                ui.theme.panel_edge,
            );
            wrapped(ui, area.inset(ui.px(8)), &notice.text, 3);
            if ui.input.take_click(area) {
                remove = Some(index);
            }
            bottom = area.y - ui.px(6);
        }
        if let Some(index) = remove {
            self.notices.remove(index);
        }
    }
    fn draw_left(
        &mut self,
        ui: &mut Ui<'_>,
        area: Rect,
        note: &mut Option<(bool, &'static str)>,
        snapshot: bool,
    ) {
        let t = &GUI_TEXT.interactive;
        let a = ui.panel(area);
        let card = LeftCard::new(a, ui.scale);
        ui.label_styled(
            card.heading,
            tr("pages_device"),
            ui.theme.heading,
            ui.theme.text,
            Align::Left,
        );
        small(ui, card.range_label, &t.labels.page_range);
        small(ui, card.resolution_label, tr("resolution_short"));
        ui.text_field(
            &mut self.range,
            card.range,
            &t.labels.page_range_placeholder,
            false,
            false,
        );
        let body = ui.theme.body;
        ui.theme.body.size = ui.scale.font(12.0);
        let changed = ui.text_field(&mut self.resolution, card.resolution, "1200", false, false);
        ui.theme.body = body;
        if changed && !snapshot {
            if let Some((height, width)) = parse_resolution(self.resolution.text()) {
                self.options.target_height = Some(height);
                self.options.target_width = width;
                self.options.target_device = None;
                if let Err(error) =
                    backend::save_resolution_preset(&ResolutionPreset { height, width })
                {
                    self.notice(error.to_string(), 8);
                }
                if self.options.crop_free_aspect && width.is_some() {
                    self.notice(tr("resolution_width_ignored"), 5);
                }
            }
        }
        hover(ui, card.resolution, &t.tooltips.target_height, false, note);

        // Preserve the reading order, filling rows across two columns. The
        // geometry tray and OCR slot stay anchored when conditional controls hide.
        let mut index = 0;
        if a.w < ui.px(360) {
            ui.theme.body.size = ui.scale.font(11.0);
        }
        macro_rules! check {
            ($value:expr,$label:expr,$tip:expr,$changed:expr) => {{
                let columns = cells(
                    Rect::new(
                        card.checks.x,
                        card.checks.y + (index / 2) * ui.px(25),
                        card.checks.w,
                        ui.px(25),
                    ),
                    2,
                    ui.px(10),
                );
                let rect = columns[(index % 2) as usize];
                let mut value = $value;
                if ui.checkbox(rect, &mut value, $label) {
                    $changed(value);
                }
                hover(ui, rect, $tip, false, note);
                index += 1;
            }};
        }
        if !self.options.preserve_mode {
            check!(
                self.options.invert_input,
                &t.labels.inverted_colors,
                &t.tooltips.inverted_colors,
                |v| self.options.set_invert_input(v)
            );
        }
        check!(
            self.options.use_ocr,
            &t.labels.ocr_text_layer,
            &t.tooltips.ocr_text_layer,
            |v| self.options.set_use_ocr(v)
        );
        check!(
            self.options.make_epub_also,
            &t.labels.make_epub_also,
            &t.tooltips.make_epub_also,
            |v| {
                self.options.make_epub_also = v;
                if v {
                    self.options.set_use_ocr(true);
                    self.options.ocr_mode = OcrMode::Thorough;
                }
            }
        );
        check!(
            self.options.high_quality_output,
            &t.labels.high_quality_output,
            &t.tooltips.high_quality_output,
            |v| self.options.high_quality_output = v
        );
        if self.options.output_format == OutputFormat::Pdf {
            check!(
                self.options.compression_type == CompressionType::Truetyping,
                &t.labels.truetyping_text,
                &t.tooltips.truetyping_text,
                |v| self.options.set_text_encoder(if v {
                    CompressionType::Truetyping
                } else {
                    CompressionType::Jbig2
                })
            );
            if !self.options.grayscale_mode && !self.options.preserve_mode {
                if self.options.compression_type == CompressionType::Jbig2 {
                    check!(
                        self.options.symbol_mode,
                        &t.labels.jbig2_symbol_mode,
                        &t.tooltips.jbig2_symbol_mode,
                        |v| self.options.symbol_mode = v
                    );
                }
                check!(
                    self.options.jpeg_compat,
                    &t.labels.jpeg_compatibility,
                    &t.tooltips.jpeg_compatibility,
                    |v| self.options.set_text_encoder(if v {
                        CompressionType::Ccitt4
                    } else {
                        CompressionType::Jbig2
                    })
                );
            }
        }
        let _ = index;
        ui.theme.body = body;
        if self.options.use_ocr {
            if let Some(index) = choices(
                ui,
                card.ocr,
                &[&t.labels.ocr_fast, &t.labels.ocr_thorough],
                usize::from(self.options.ocr_mode == OcrMode::Thorough),
            ) {
                self.options.ocr_mode = if index == 0 {
                    self.options.make_epub_also = false;
                    OcrMode::Fast
                } else {
                    OcrMode::Thorough
                };
            }
            hover(
                ui,
                card.ocr,
                if self.options.ocr_mode == OcrMode::Fast {
                    &t.tooltips.ocr_fast
                } else {
                    &t.tooltips.ocr_thorough
                },
                false,
                note,
            );
        }
        tray(ui, card.geometry);
        let buttons = cells(card.geometry.inset(ui.px(5)), 3, ui.px(4));
        for (index, text, selected, tooltip) in [
            (
                0,
                tr("center_margins"),
                self.options.center_margins,
                t.tooltips.margin_centering.as_str(),
            ),
            (
                1,
                tr("crop_margins"),
                self.options.crop_margins,
                t.tooltips.margin_crop_resize.as_str(),
            ),
            (
                2,
                t.labels.reflow.as_str(),
                self.options.reflow,
                t.tooltips.reflow.as_str(),
            ),
        ] {
            if choice(ui, buttons[index], text, selected) {
                self.select_geometry(index);
            }
            hover(ui, buttons[index], tooltip, false, note);
        }
    }

    fn select_geometry(&mut self, index: usize) {
        match index {
            0 => {
                let enabled = !self.options.center_margins;
                self.options.center_margins = enabled;
                self.options.crop_margins = false;
                self.options.crop_footnotes = false;
                self.options.crop_free_aspect = false;
                self.options.reflow = false;
            }
            1 => {
                let enabled = !self.options.crop_margins;
                self.options.crop_margins = enabled;
                self.options.crop_free_aspect = enabled;
                self.options.center_margins = false;
                self.options.reflow = false;
            }
            _ => self.options.set_reflow(!self.options.reflow),
        }
    }

    pub(crate) fn select_binarization(&mut self, index: usize) {
        self.options.use_custom_adaptive = index == 0;
        self.options.use_fixed_threshold = index == 1;
        self.options.use_heavy_binarization = index == 2;
        if index != 2 {
            self.last_binarization_fixed = index == 1;
        }
    }

    fn draw_right(&mut self, ui: &mut Ui<'_>, area: Rect, note: &mut Option<(bool, &'static str)>) {
        let t = &GUI_TEXT.interactive;
        let a = ui.panel(area);
        let card = RightCard::new(a, ui.scale);
        ui.label_styled(
            card.heading,
            tr("output"),
            ui.theme.heading,
            ui.theme.text,
            Align::Left,
        );
        for (index, caption) in [
            (0, tr("mode_short")),
            (1, tr("format_short")),
            (2, tr("text_short")),
            (3, tr("layout_short")),
            (4, tr("images_short")),
        ] {
            let (label_area, control) = card.setting(index);
            if index == 4 && !self.options.layout_analysis {
                continue;
            }
            label(ui, label_area, caption);
            match index {
                0 => {
                    if let Some(index) = choices(
                        ui,
                        control,
                        &[tr("preparation"), tr("preserve")],
                        usize::from(self.options.preserve_mode),
                    ) {
                        self.options.set_preserve_mode(index == 1);
                    }
                    hover(
                        ui,
                        control,
                        if self.options.preserve_mode {
                            tr("preserve_note")
                        } else {
                            tr("preparation_note")
                        },
                        true,
                        note,
                    );
                }
                1 => {
                    if let Some(index) = choices(
                        ui,
                        control,
                        &["PDF", "DjVu"],
                        usize::from(self.options.output_format == OutputFormat::Djvu),
                    ) {
                        self.options.output_format = if index == 0 {
                            OutputFormat::Pdf
                        } else {
                            OutputFormat::Djvu
                        };
                    }
                    hover(ui, control, &t.tooltips.base_format, true, note);
                }
                2 => {
                    if self.options.preserve_mode {
                        small(ui, control, tr("preserve_summary"));
                    } else if let Some(index) = choices(
                        ui,
                        control,
                        &[tr("binarized"), tr("grayscale")],
                        usize::from(self.options.grayscale_mode),
                    ) {
                        self.options.grayscale_mode = index == 1;
                        if index == 1 && self.options.jpeg_compat {
                            self.options.set_text_encoder(CompressionType::Jbig2);
                        }
                    }
                    hover(
                        ui,
                        control,
                        if self.options.preserve_mode {
                            tr("preserve_note")
                        } else if self.options.output_format == OutputFormat::Djvu {
                            tr("grayscale_djvu")
                        } else {
                            tr("grayscale_pdf")
                        },
                        true,
                        note,
                    );
                }
                3 => {
                    if let Some(index) = choices(
                        ui,
                        control,
                        &[&t.controls.on, &t.controls.off],
                        usize::from(!self.options.layout_analysis),
                    ) {
                        self.options.set_layout_analysis(index == 0);
                        if index == 1 {
                            self.notice(t.popups.layout_disabled.clone(), 5);
                        }
                    }
                    hover(ui, control, &t.tooltips.layout_detection, true, note);
                }
                _ => {
                    if self.options.preserve_mode {
                        choice(ui, control, tr("original"), true);
                    } else {
                        let mut texts = vec![tr("original"), tr("dithered")];
                        if self.options.can_select_jbig2_halftone_images() {
                            texts.push(&t.labels.jbig2_halftone);
                        }
                        let selected = if self.options.uses_jbig2_halftone_images() {
                            2
                        } else {
                            usize::from(
                                self.options.image_processing_type == ImageProcessingType::Dithered,
                            )
                        };
                        if let Some(index) = choices(ui, control, &texts, selected) {
                            match index {
                                0 => self
                                    .options
                                    .set_image_processing_type(ImageProcessingType::Original),
                                1 => self
                                    .options
                                    .set_image_processing_type(ImageProcessingType::Dithered),
                                _ => self.options.set_jbig2_halftone_images(),
                            }
                        }
                    }
                    let cells = cells(
                        control,
                        if self.options.preserve_mode {
                            1
                        } else if self.options.can_select_jbig2_halftone_images() {
                            3
                        } else {
                            2
                        },
                        ui.px(4),
                    );
                    for (i, rect) in cells.into_iter().enumerate() {
                        hover(
                            ui,
                            rect,
                            match i {
                                0 => &t.tooltips.image_output_original,
                                1 => &t.tooltips.image_output_dithered,
                                _ => &t.tooltips.jbig2_halftone,
                            },
                            true,
                            note,
                        );
                    }
                }
            }
        }
        if !self.options.grayscale_mode && !self.options.preserve_mode {
            tray(ui, card.binarization);
            let (modes, caption, value) = card.binarization_fields();
            small(
                ui,
                Rect::new(modes.x, caption.y, modes.w, caption.h),
                &t.controls.binarization,
            );
            let selected = if self.options.use_fixed_threshold {
                1
            } else if self.options.use_heavy_binarization {
                2
            } else {
                0
            };
            if let Some(index) = choices(
                ui,
                modes,
                &[
                    &t.controls.custom_adaptive,
                    &t.controls.fixed_threshold,
                    &t.controls.heavy_model,
                ],
                selected,
            ) {
                self.select_binarization(index);
            }
            let heavy = self.options.use_heavy_binarization;
            let fixed = if heavy {
                self.last_binarization_fixed
            } else {
                self.options.use_fixed_threshold
            };
            small(ui, caption, if fixed { "0–255" } else { "K" });
            let field = Rect::new(value.x, value.y + ui.px(1), value.w, value.h - ui.px(2));
            if heavy {
                ui.painter.rounded_rect(
                    field,
                    ui.theme.corner_radius,
                    ui.theme.border_width,
                    ui.theme.hover,
                    ui.theme.panel_edge,
                );
                ui.label(
                    field,
                    if fixed {
                        self.threshold.text()
                    } else {
                        self.k.text()
                    },
                    ui.theme.text_dim,
                    Align::Centre,
                );
            } else {
                let body = ui.theme.body;
                ui.theme.body.size = ui.scale.font(12.0);
                if fixed {
                    ui.text_field(&mut self.threshold, field, "180", false, false);
                } else {
                    ui.text_field(&mut self.k, field, "0.05", false, false);
                }
                ui.theme.body = body;
            }
            let mode_rects = cells(modes, 3, ui.px(4));
            for (i, rect) in mode_rects.into_iter().enumerate() {
                hover(
                    ui,
                    rect,
                    match i {
                        0 => &t.tooltips.sauvola_k_factor,
                        1 => &t.tooltips.threshold_value,
                        _ => &t.tooltips.heavy_model,
                    },
                    true,
                    note,
                );
            }
            hover(
                ui,
                value,
                if heavy {
                    &t.tooltips.heavy_model
                } else if fixed {
                    &t.tooltips.threshold_value
                } else {
                    &t.tooltips.sauvola_k_factor
                },
                true,
                note,
            );
        }
    }
    fn draw_modal(&mut self, ui: &mut Ui<'_>) {
        let Some(modal) = self.modal else {
            return;
        };
        let t = &GUI_TEXT.interactive;
        let tall = match modal {
            Modal::Queue | Modal::Log => true,
            #[cfg(feature = "debug-logging")]
            Modal::Debug => true,
            _ => false,
        };
        let w = ui.px(560).min(ui.painter.width() - ui.px(32));
        let h = ui
            .px(if tall { 520 } else { 310 })
            .min(ui.painter.height() - ui.px(32));
        let area = Rect::new(
            (ui.painter.width() - w) / 2,
            (ui.painter.height() - h) / 2,
            w,
            h,
        );
        let a = ui.panel(area);
        let row_h = ui.px(30);
        let title = match modal {
            Modal::About => t.queue.about_button.as_str(),
            Modal::Theme => tr("theme"),
            Modal::Queue => t.popups.queue_items_title.as_str(),
            Modal::Log => t.popups.processing_log_title.as_str(),
            #[cfg(feature = "debug-logging")]
            Modal::Debug => t.popups.debug_log_title.as_str(),
        };
        ui.label_styled(
            Rect::new(a.x, a.y, a.w, row_h),
            title,
            ui.theme.heading,
            ui.theme.text,
            Align::Left,
        );
        let close = Rect::new(a.right() - ui.px(90), a.bottom() - row_h, ui.px(90), row_h);
        if ui.button(close, &t.popups.close) {
            self.close_modal();
            return;
        }
        let body = Rect::new(
            a.x,
            a.y + row_h + ui.px(8),
            a.w,
            a.h - row_h * 2 - ui.px(12),
        );
        match modal {
            Modal::About => {
                label(
                    ui,
                    row(body, 0, row_h),
                    &format!("Lege v{}", crate::version::display_version()),
                );
                if ui.button(row(body, 2, row_h), tr("theme")) {
                    self.choose_theme();
                }
                if ui.button(row(body, 3, row_h), &t.popups.documentation) {
                    self.open_document("documentation.html", backend::DOCUMENTATION_HTML);
                }
                if ui.button(row(body, 4, row_h), &t.popups.licenses) {
                    self.open_document("licenses.html", backend::LICENSES_HTML);
                }
                if let Some(error) = &self.modal_error {
                    wrapped(
                        ui,
                        Rect::new(body.x, body.y + row_h * 5, body.w, row_h * 2),
                        error,
                        2,
                    );
                }
            }
            Modal::Theme => {
                let text = if self.theme_index == 0 {
                    tr("default_theme").to_owned()
                } else {
                    format!(
                        "Sanzo {} {}",
                        if (self.theme_index - 1) % 2 == 1 {
                            tr("dark")
                        } else {
                            tr("light")
                        },
                        (self.theme_index - 1) / 2 + 1
                    )
                };
                label(ui, row(body, 0, row_h), &text);
                if self.theme_index > 0 {
                    if let Some(theme) = crate::sanzowada::theme_at((self.theme_index - 1) / 2) {
                        small(ui, row(body, 1, row_h), &theme.colors.join(", "));
                    }
                }
                let r = row(body, 2, row_h);
                let w = (r.w - ui.px(12)) / 4;
                for (index, text) in ["←", "→", tr("light"), tr("dark")].into_iter().enumerate()
                {
                    if ui.button(
                        Rect::new(r.x + index as i32 * (w + ui.px(4)), r.y, w, r.h),
                        text,
                    ) {
                        match index {
                            0 => self.step_theme(-1),
                            1 => self.step_theme(1),
                            _ => {
                                if self.theme_index == 0 {
                                    self.theme_index = 1;
                                }
                                let combo = (self.theme_index - 1) / 2;
                                self.theme_index = 1 + combo * 2 + usize::from(index == 3);
                                self.preview_theme();
                            }
                        }
                    }
                }
                if ui.button(row(body, 4, row_h), tr("accept")) {
                    self.accept_theme();
                }
                if ui.button(row(body, 5, row_h), &t.buttons.cancel) {
                    appearance::set_theme_preview(None);
                    self.modal = Some(Modal::About);
                }
            }
            Modal::Queue => {
                let list = Rect::new(body.x, body.y, body.w, body.h - row_h - ui.px(6));
                let heights = vec![ui.px(48); self.queue.len()];
                ui.scroll_list(&mut self.modal_scroll, list, &heights, |ui, index, rect| {
                    let item = &self.queue[index];
                    label(
                        ui,
                        Rect::new(rect.x, rect.y, rect.w, ui.px(24)),
                        &item.file_name,
                    );
                    if let Some(output) = self
                        .rows
                        .iter()
                        .find(|row| row.path == item.file_path)
                        .and_then(|row| row.output.as_ref())
                    {
                        small(
                            ui,
                            Rect::new(rect.x, rect.y + ui.px(24), rect.w, ui.px(24)),
                            &output.to_string_lossy(),
                        );
                    }
                });
                let rect = Rect::new(body.x, body.bottom() - row_h, body.w, row_h);
                if self.running {
                    small(ui, rect, &t.buttons.clear_queue);
                } else if ui.button(rect, &t.buttons.clear_queue) {
                    self.queue.clear();
                    self.rows.clear();
                }
            }
            Modal::Log => {
                let list = Rect::new(body.x, body.y, body.w, body.h - row_h - ui.px(6));
                let entries: Vec<_> = self.log.iter().rev().take(30).collect();
                let heights = vec![ui.px(48); entries.len()];
                let mut open = None;
                ui.scroll_list(&mut self.modal_scroll, list, &heights, |ui, index, rect| {
                    let entry = entries[index];
                    label(
                        ui,
                        Rect::new(rect.x, rect.y, rect.w, ui.px(24)),
                        &format!("{} · {}", entry.status, entry.input_filename),
                    );
                    small(
                        ui,
                        Rect::new(rect.x, rect.y + ui.px(24), rect.w, ui.px(22)),
                        &entry.output_filename,
                    );
                    if ui.input.take_click(rect) {
                        open = crate::logging::run_log_path(entry);
                    }
                });
                if let Some(path) = open {
                    if let Err(error) = backend::open_with_system(&path.to_string_lossy()) {
                        self.notice(error.to_string(), 8);
                    }
                }
                if !self.running
                    && ui.button(
                        Rect::new(body.x, body.bottom() - row_h, body.w, row_h),
                        &t.popups.clear_log,
                    )
                {
                    if let Err(error) = crate::logging::clear_log_entries() {
                        self.notice(error.to_string(), 8);
                    } else {
                        self.log.clear();
                    }
                }
            }
            #[cfg(feature = "debug-logging")]
            Modal::Debug => {
                let text = std::fs::read_to_string(crate::settings::get_worker_log_path())
                    .unwrap_or_default();
                let lines: Vec<_> = text.lines().rev().take(100).collect();
                let heights = vec![ui.px(20); lines.len()];
                ui.scroll_list(&mut self.modal_scroll, body, &heights, |ui, index, rect| {
                    small(ui, rect, lines[index])
                });
            }
        }
    }
    fn open_document(&mut self, name: &str, contents: &str) {
        self.modal_error = backend::open_embedded_document(name, contents)
            .err()
            .map(|error| format!("{name}: {error}"));
    }
}

fn wrapped(ui: &mut Ui<'_>, area: Rect, text: &str, max_lines: usize) {
    ui.painter.push_clip(area);
    let style = ui.theme.body;
    let line_h = ui.text.line_height(style).max(1);
    let mut line = String::new();
    let mut lines = Vec::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && ui.text.measure(&candidate, style) > area.w {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    for (i, line) in lines.iter().take(max_lines).enumerate() {
        label(
            ui,
            Rect::new(area.x, area.y + i as i32 * line_h, area.w, line_h),
            line,
        );
    }
    ui.painter.pop_clip();
}

fn progress_fraction(m: WorkerProgressMetrics) -> f64 {
    if m.pages_total == 0 {
        return 0.0;
    }
    let (value, total) = match m.mode {
        WorkerProgressMode::Epub => (
            u64::from(m.rendered) + u64::from(m.detected) + u64::from(m.encoded),
            u64::from(m.pages_total) * 3,
        ),
        WorkerProgressMode::Layout | WorkerProgressMode::Margin => (
            u64::from(m.rendered.max(m.detected).max(m.encoded)),
            u64::from(m.pages_total),
        ),
        _ => (
            u64::from(m.rendered.max(m.encoded)),
            u64::from(m.pages_total),
        ),
    };
    (value as f64 / total as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolution_forms_and_rejected_values() {
        for value in ["1200x800", "1200 800", "1200×800"] {
            assert_eq!(parse_resolution(value), Some((1200, Some(800))));
        }
        assert_eq!(parse_resolution("1200"), Some((1200, None)));
        for value in ["", "0", "1200x0", "a", "12 13 14"] {
            assert_eq!(parse_resolution(value), None);
        }
    }
    #[test]
    fn clearing_the_editor_transiently_keeps_the_last_committed_resolution() {
        let mut dashboard = Dashboard::new();
        dashboard.options = ProcessingOptions::new();
        dashboard.options.target_height = Some(1400);
        dashboard.options.target_width = Some(800);
        dashboard.resolution.clear();
        dashboard.k.set_text("0.05");
        assert!(dashboard.validate_fields().is_ok());
        assert_eq!(dashboard.options.target_height, Some(1400));
        assert_eq!(dashboard.options.target_width, Some(800));
    }
    #[test]
    fn bounded_probe_notices_wake_the_consumer_before_the_channel_fills() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (wake_tx, wake_rx) = mpsc::channel();
        let wake = Waker::from_fn(move || {
            let _ = wake_tx.send(());
        });
        let thread = std::thread::spawn(move || {
            probe(
                sender,
                wake,
                vec![PathBuf::from("bad-one.txt"), PathBuf::from("bad-two.txt")],
            )
        });
        let mut done = false;
        for _ in 0..8 {
            wake_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("probe must wake before it fills its bounded channel");
            for message in receiver.try_iter() {
                if matches!(message, Message::Inputs(_)) {
                    done = true;
                }
            }
            if done {
                break;
            }
        }
        assert!(done);
        thread.join().unwrap();
    }
    #[test]
    fn queued_startup_handles_cancel_when_the_window_discards_the_event() {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let handle = WorkerHandle::supervisor(Arc::new(Mutex::new(Vec::new())), cancelled.clone());
        let (sender, receiver) = mpsc::sync_channel(1);
        assert!(
            sender
                .send(Message::Started(Vec::new(), StartedWorkers(vec![handle])))
                .is_ok()
        );
        drop(receiver);
        assert!(cancelled.load(Ordering::SeqCst));
    }
    #[test]
    fn epub_progress_uses_three_stages_and_never_overflows() {
        let metrics = WorkerProgressMetrics {
            pages_total: 10,
            rendered: 10,
            detected: 5,
            encoded: 0,
            mode: WorkerProgressMode::Epub,
            is_djvu: false,
            enable_layout_detection: true,
            eta_seconds: None,
        };
        assert_eq!(progress_fraction(metrics), 0.5);
        assert_eq!(
            progress_fraction(WorkerProgressMetrics {
                pages_total: u32::MAX,
                rendered: u32::MAX,
                detected: u32::MAX,
                encoded: u32::MAX,
                ..metrics
            }),
            1.0
        );
    }
    fn click(app: &mut LegeApp, buffer: &mut WindowBuffer, x: f32, y: f32) {
        app.on_cursor(x, y);
        app.on_mouse(MouseButton::Left, true);
        app.on_mouse(MouseButton::Left, false);
        app.render(buffer, Scale::ONE);
    }
    #[test]
    fn preserve_click_reaches_the_worker_and_a_modal_blocks_background_input() {
        let mut app = LegeApp::new();
        app.dashboard.options = ProcessingOptions::new();
        let mut buffer = WindowBuffer::new(940, 700);
        app.render(&mut buffer, Scale::ONE);
        let bands = Bands::new(940, 700, Scale::ONE);
        let (_, mode) = RightCard::new(bands.right.inset(8), Scale::ONE).setting(0);
        let mode_cells = cells(mode, 2, 4);
        let preserve = mode_cells[1];
        click(
            &mut app,
            &mut buffer,
            (preserve.x + preserve.w / 2) as f32,
            (preserve.y + preserve.h / 2) as f32,
        );
        assert!(app.dashboard.options.preserve_mode);
        let args = crate::worker_process::gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("out.pdf"),
            &app.dashboard.options,
            true,
        );
        assert!(args.iter().any(|arg| arg == "--preserve"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--grayscale" || arg == "--sauvola-k")
        );
        app.dashboard.modal = Some(Modal::About);
        let preparation = mode_cells[0];
        click(
            &mut app,
            &mut buffer,
            (preparation.x + preparation.w / 2) as f32,
            (preparation.y + preparation.h / 2) as f32,
        );
        assert!(
            app.dashboard.options.preserve_mode,
            "Preparation behind the modal must not receive this click"
        );
    }
    #[test]
    fn completion_after_cancel_does_not_remove_or_mark_a_queue_item_successful() {
        let mut dashboard = Dashboard::new();
        let path = PathBuf::from("input.pdf");
        dashboard.queue.push_back(DocumentItem::new(path.clone()));
        dashboard.rows.push(QueueRow {
            path,
            output: None,
            status: tr("processing"),
        });
        dashboard.running = true;
        dashboard.cancel();
        dashboard.worker_event(WorkerProgressUpdate::Completed {
            task_id: 0,
            message: "done".into(),
            metrics: None,
        });
        assert_eq!(dashboard.queue.len(), 1);
        assert_eq!(dashboard.rows[0].status, tr("failed"));
        assert!(dashboard.notices.is_empty());
    }
    #[test]
    fn geometry_buttons_are_exclusive_and_can_all_be_off() {
        let mut app = LegeApp::new();
        app.dashboard.options = ProcessingOptions::new();
        let mut buffer = WindowBuffer::new(940, 700);
        let bands = Bands::new(940, 700, Scale::ONE);
        let area = LeftCard::new(bands.left.inset(8), Scale::ONE)
            .geometry
            .inset(5);
        let buttons = cells(area, 3, 4);
        for index in [0, 1, 2, 2] {
            let rect = buttons[index];
            click(
                &mut app,
                &mut buffer,
                (rect.x + rect.w / 2) as f32,
                (rect.y + rect.h / 2) as f32,
            );
            let count = usize::from(app.dashboard.options.center_margins)
                + usize::from(app.dashboard.options.crop_margins)
                + usize::from(app.dashboard.options.reflow);
            assert!(count <= 1);
        }
        assert!(
            !app.dashboard.options.center_margins
                && !app.dashboard.options.crop_margins
                && !app.dashboard.options.reflow
        );
    }
    #[test]
    fn threshold_parameter_stays_visible_but_does_not_edit_under_heavy() {
        let mut app = LegeApp::new();
        app.dashboard.options = ProcessingOptions::new();
        app.dashboard.threshold.set_text("180");
        let mut buffer = WindowBuffer::new(940, 700);
        let bands = Bands::new(940, 700, Scale::ONE);
        let card = RightCard::new(bands.right.inset(8), Scale::ONE);
        let (modes, _, value) = card.binarization_fields();
        let buttons = cells(modes, 3, 4);
        let threshold = buttons[1];
        click(
            &mut app,
            &mut buffer,
            (threshold.x + threshold.w / 2) as f32,
            (threshold.y + threshold.h / 2) as f32,
        );
        assert!(app.dashboard.options.use_fixed_threshold);
        assert_eq!(app.dashboard.threshold.text(), "180");
        let heavy = buttons[2];
        click(
            &mut app,
            &mut buffer,
            (heavy.x + heavy.w / 2) as f32,
            (heavy.y + heavy.h / 2) as f32,
        );
        assert!(app.dashboard.options.use_heavy_binarization);
        assert!(app.dashboard.last_binarization_fixed);
        click(
            &mut app,
            &mut buffer,
            (value.x + value.w / 2) as f32,
            (value.y + value.h / 2) as f32,
        );
        app.on_key(&KeyInput::Backspace);
        app.on_key(&KeyInput::Character('9'));
        app.render(&mut buffer, Scale::ONE);
        assert_eq!(app.dashboard.threshold.text(), "180");
        assert_eq!(card.binarization_fields().2, value);
    }
    #[test]
    fn compact_resolution_field_fits_height_and_width() {
        let mut text = TextCache::new(crate::theme::fonts());
        let style = pixelkit_text::TextStyle::new(crate::theme::FACE, 12.0);
        assert!(text.measure("1200x800", style) <= 64 - 8);
    }
    fn real_worker(cancel: bool) {
        let input =
            PathBuf::from(std::env::var_os("LEGE_GUI_SMOKE_INPUT").expect("explicit test input"));
        let output = PathBuf::from(
            std::env::var_os("LEGE_GUI_SMOKE_OUTPUT").expect("explicit output directory"),
        );
        std::fs::create_dir_all(&output).unwrap();
        let mut app = LegeApp::new();
        app.dashboard.options = ProcessingOptions::new();
        app.dashboard.options.output_path = Some(output.clone());
        app.dashboard.options.output_path_is_auto = false;
        app.dashboard.options.original_cover = false;
        app.dashboard.options.set_layout_analysis(false);
        app.dashboard.options.set_preserve_mode(true);
        app.dashboard.resolution.set_text("600");
        app.dashboard
            .queue
            .push_back(DocumentItem::new(input.clone()));
        app.dashboard.start();
        assert!(app.dashboard.running);
        if cancel {
            app.dashboard.cancel();
        }
        let mut buffer = WindowBuffer::new(940, 700);
        let deadline = Instant::now() + Duration::from_secs(60);
        while app.dashboard.running {
            app.render(&mut buffer, Scale::ONE);
            if Instant::now() > deadline {
                app.dashboard.cancel();
                panic!("worker timed out");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(!app.dashboard.rows.is_empty());
        if cancel {
            assert_eq!(app.dashboard.queue.len(), 1);
            assert!(
                app.dashboard
                    .rows
                    .iter()
                    .all(|row| row.status != tr("completed"))
            );
        } else {
            assert!(
                app.dashboard
                    .rows
                    .iter()
                    .all(|row| row.status == tr("completed")),
                "{:?}",
                app.dashboard.status
            );
            assert!(app.dashboard.queue.is_empty());
            assert!(app.dashboard.metrics.is_some());
            let file = app.dashboard.rows[0].output.as_ref().unwrap();
            let bytes = std::fs::read(file).unwrap();
            assert!(bytes.starts_with(b"%PDF-"));
            assert!(
                bytes
                    .windows(b"JPXDecode".len())
                    .any(|window| window == b"JPXDecode")
            );
            assert!(
                bytes
                    .windows(b"JBIG2Decode".len())
                    .any(|window| window == b"JBIG2Decode")
            );
        }
    }
    #[test]
    #[ignore = "requires built lege on PATH and LEGE_GUI_SMOKE_INPUT/OUTPUT"]
    fn preserve_real_worker_smoke() {
        real_worker(false);
    }
    #[test]
    #[ignore = "requires built lege on PATH and LEGE_GUI_SMOKE_INPUT/OUTPUT"]
    fn cancel_real_worker_smoke() {
        real_worker(true);
    }
}
