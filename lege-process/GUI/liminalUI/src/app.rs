use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use liminalkit_raster::{FillRule, Painter, Path, RasterKernel, Rect, WindowBuffer};
use liminalkit_shell::{Input, KeyInput, Modifiers, MouseButton, Scale};
use liminalkit_text::{Align, Hinting, TextCache, TextRendering};
use liminalkit_ui::{Focus, ScrollState, TextFieldState, Ui};
use liminalkit_windowing::{ImeCursorArea, ImeEvent, PixelApp, Waker};

use crate::appearance::{self, ThemeChoice};
use crate::backend;
use crate::gui_text::{GUI_TEXT, tr};
use crate::layout::{Actions, Bands, LeftCard, RightCard, cells};
use crate::models::{
    BinarizationKind, CompressionType, DocumentItem, GeometryMode, ImageProcessingType, OcrMode,
    OutputFormat, ProcessingOptions, ResolutionPreset,
};
use crate::worker_process::{
    WorkerHandle, WorkerProcessingStatus, WorkerProgressMetrics, WorkerProgressMode,
    WorkerProgressUpdate, WorkerReflowStage,
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

#[derive(Clone, Copy)]
enum FileDialogKind {
    Files,
    Folder,
    Output,
}

struct InputTask {
    id: u64,
    _owner: flume::Sender<()>,
}

enum InputFlow {
    Idle,
    Selecting(InputTask),
    Probing(InputTask),
}

enum ResolutionEdit {
    Displaying,
    Editing { original: ResolutionPreset },
}

impl InputFlow {
    fn id(&self) -> Option<u64> {
        match self {
            Self::Idle => None,
            Self::Selecting(task) | Self::Probing(task) => Some(task.id),
        }
    }
}

enum Message {
    Inputs(u64, Vec<(PathBuf, Option<u32>, Option<bool>)>),
    Output(u64, PathBuf),
    ProbeStarted(u64),
    Notice(String),
    DialogClosed(u64),
    Started(u64, Vec<backend::TrackerInfo>, StartedWorkers),
    Worker(u64, WorkerProgressUpdate),
    Finished(u64),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchState {
    Idle,
    Starting,
    Running,
    Cancelling,
    Finished(BatchOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DocumentRunState {
    Queued,
    Processing {
        fraction: f64,
        reflow_phase: Option<WorkerReflowStage>,
    },
    Completed,
    Failed,
    Cancelled,
}

impl DocumentRunState {
    fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
    fn label(self) -> &'static str {
        tr(match self {
            Self::Queued => "queued",
            Self::Processing { .. } => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        })
    }
    fn fraction(self) -> f64 {
        match self {
            Self::Queued | Self::Cancelled => 0.0,
            Self::Processing { fraction, .. } => fraction,
            Self::Completed | Self::Failed => 1.0,
        }
    }
}

struct QueueRow {
    task_id: u64,
    path: PathBuf,
    output: Option<PathBuf>,
    state: DocumentRunState,
}

pub struct Dashboard {
    pub options: ProcessingOptions,
    queue: VecDeque<DocumentItem>,
    rows: Vec<QueueRow>,
    range: TextFieldState,
    resolution: TextFieldState,
    k: TextFieldState,
    threshold: TextFieldState,
    status: (String, String, String),
    metrics: Option<WorkerProgressMetrics>,
    resolution_edit: ResolutionEdit,
    blink_start: Instant,
    batch: BatchState,
    generation: u64,
    handles: Vec<WorkerHandle>,
    notices: VecDeque<Notice>,
    modal: Option<Modal>,
    modal_scroll: ScrollState,
    log: Vec<crate::logging::LogEntry>,
    theme_index: usize,
    modal_error: Option<String>,
    input_flow: InputFlow,
    input_generation: u64,
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
            // Vertical hinting puts cap height and ascenders on whole pixels;
            // `theme::crisp` sizes already have whole-pixel x-heights.
            text: TextCache::with_limits(crate::theme::fonts(), 512, 2 * 1024 * 1024)
                .with_rendering(TextRendering::DEFAULT.with_hinting(Hinting::Vertical)),
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
        let theme = crate::theme::theme(palette);
        let bands = Bands::new(buffer.width as i32, buffer.height as i32, scale);
        if self.dashboard.modal.is_none() {
            let card = LeftCard::new(bands.right.inset(scale.px(8)), scale);
            if let Some((x, y)) = self.input.press_position() {
                let outside_range =
                    self.dashboard.range.caret_area().is_some() && !card.range.contains(x, y);
                let outside_resolution = self.dashboard.resolution.caret_area().is_some()
                    && !card.resolution.contains(x, y);
                if outside_range || outside_resolution {
                    self.focus.clear();
                }
            }
        }
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
            crate::theme::snap_fonts(&mut ui.theme);
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
            crate::theme::snap_fonts(&mut ui.theme);
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
            liminalkit_raster::png::decode(include_bytes!("../../../../lege-misc/assets/icon.png"))
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
                // Windows takes the title-bar icon from this and the taskbar
                // and Alt-Tab icon separately; without the second it falls
                // back to the executable's resource, if it has one.
                #[cfg(target_os = "windows")]
                {
                    use winit::platform::windows::WindowExtWindows;
                    window.set_taskbar_icon(Some(icon.clone()));
                }
                window.set_window_icon(Some(icon));
            }
        }
    }
    fn animation_interval(&self) -> Option<Duration> {
        // Worker/dialog threads wake the host when data arrives. Idle windows
        // have no polling timer; only timed notifications need a deadline.
        (!self.dashboard.notices.is_empty()
            || self.dashboard.range.caret_area().is_some()
            || self.dashboard.resolution.caret_area().is_some())
        .then_some(Duration::from_millis(100))
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
                .attach_clipboard(liminalkit_shell::clipboard::ClipboardWorker::new(
                    move || waker.wake(),
                ));
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
        if !self.dashboard.running() && !self.dashboard.input_busy() {
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
        if self.dashboard.modal.is_some() || self.dashboard.running() {
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
    fn running(&self) -> bool {
        matches!(
            self.batch,
            BatchState::Starting | BatchState::Running | BatchState::Cancelling
        )
    }
    fn cancelling(&self) -> bool {
        self.batch == BatchState::Cancelling
    }
    fn batch_progress(&self) -> f64 {
        if self.rows.is_empty() {
            return 0.0;
        }
        self.rows
            .iter()
            .map(|row| row.state.fraction())
            .sum::<f64>()
            / self.rows.len() as f64
    }

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
            options,
            queue: VecDeque::new(),
            rows: Vec::new(),
            status: (String::new(), String::new(), String::new()),
            metrics: None,
            resolution_edit: ResolutionEdit::Displaying,
            blink_start: Instant::now(),
            batch: BatchState::Idle,
            generation: 0,
            handles: Vec::new(),
            notices: VecDeque::new(),
            modal: None,
            modal_scroll: ScrollState::new(),
            log: crate::logging::load_log_entries().unwrap_or_default(),
            theme_index: 0,
            modal_error: None,
            input_flow: InputFlow::Idle,
            input_generation: 0,
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
                Message::Inputs(id, paths) => {
                    if self.input_flow.id() != Some(id) {
                        continue;
                    }
                    if !self.running() {
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
                Message::Output(id, path) => {
                    if self.input_flow.id() != Some(id) {
                        continue;
                    }
                    self.options.output_path = Some(path);
                    self.options.output_path_is_auto = false;
                }
                Message::Notice(text) => self.notice(text, 8),
                Message::ProbeStarted(id) => {
                    if self.input_flow.id() == Some(id) {
                        self.input_flow =
                            match std::mem::replace(&mut self.input_flow, InputFlow::Idle) {
                                InputFlow::Selecting(task) => InputFlow::Probing(task),
                                state => state,
                            };
                    }
                }
                Message::DialogClosed(id) => {
                    if self.input_flow.id() == Some(id) {
                        self.input_flow = InputFlow::Idle;
                    }
                }
                Message::Started(generation, infos, handles) => {
                    if generation != self.generation || !self.running() {
                        continue;
                    }
                    if !self.cancelling() {
                        self.batch = BatchState::Running;
                    }
                    self.rows = infos
                        .into_iter()
                        .map(|info| QueueRow {
                            task_id: info.id,
                            path: info.input_path,
                            output: Some(info.output_path),
                            state: DocumentRunState::Queued,
                        })
                        .collect();
                    self.handles = handles.take();
                    if self.cancelling() {
                        for handle in &mut self.handles {
                            handle.kill();
                        }
                    }
                }
                Message::Worker(generation, update) => {
                    if generation == self.generation {
                        self.worker_event(update);
                    }
                }
                Message::Finished(generation) => {
                    if generation != self.generation || !self.running() {
                        continue;
                    }
                    let cancelled = self.cancelling();
                    for row in &mut self.rows {
                        if !row.state.terminal() {
                            row.state = if cancelled {
                                DocumentRunState::Cancelled
                            } else {
                                DocumentRunState::Failed
                            };
                        }
                    }
                    let outcome = if cancelled {
                        BatchOutcome::Cancelled
                    } else if !self.rows.is_empty()
                        && self
                            .rows
                            .iter()
                            .all(|row| row.state == DocumentRunState::Completed)
                    {
                        BatchOutcome::Completed
                    } else {
                        BatchOutcome::Failed
                    };
                    self.batch = BatchState::Finished(outcome);
                    self.handles.clear();
                    if cancelled {
                        self.status = (tr("cancelled").into(), String::new(), String::new());
                    }
                    self.log = crate::logging::load_log_entries().unwrap_or_default();
                }
            }
        }
        self.notices.retain(|notice| notice.until > Instant::now());
    }
    fn worker_event(&mut self, event: WorkerProgressUpdate) {
        if !self.running() {
            return;
        }
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
        let Some(index) = self.rows.iter().position(|row| row.task_id == id) else {
            return;
        };
        if self.rows[index].state.terminal() {
            return;
        }
        if self.rows[index].state == DocumentRunState::Queued {
            self.metrics = None;
        }
        let phase = match &event {
            WorkerProgressUpdate::Status {
                status: WorkerProcessingStatus::ReflowProgress { stage, .. },
                ..
            } => Some(*stage),
            _ => match self.rows[index].state {
                DocumentRunState::Processing { reflow_phase, .. } => reflow_phase,
                _ => None,
            },
        };
        if let Some(metrics) = metrics {
            let value = if metrics.mode == WorkerProgressMode::Reflow {
                let fraction = progress_fraction(metrics);
                match phase {
                    Some(WorkerReflowStage::SourceAnalysis) => fraction * 0.45,
                    Some(WorkerReflowStage::Compose) => 0.45,
                    Some(WorkerReflowStage::OutputPages) => 0.5 + fraction * 0.45,
                    None => fraction * 0.95,
                }
            } else {
                progress_fraction(metrics) * 0.95
            };
            self.rows[index].state = DocumentRunState::Processing {
                fraction: self.rows[index].state.fraction().max(value),
                reflow_phase: phase,
            };
            self.metrics = Some(metrics);
        }
        match event {
            WorkerProgressUpdate::Status { status, .. } => {
                if !self.cancelling() {
                    self.status = status.to_gui_display_lines();
                }
                if self.rows[index].state == DocumentRunState::Queued {
                    self.rows[index].state = DocumentRunState::Processing {
                        fraction: 0.0,
                        reflow_phase: phase,
                    };
                }
                if let WorkerProcessingStatus::FootnotesDetected { message }
                | WorkerProcessingStatus::MarginAnalysisSummary { summary: message } = status
                {
                    self.notice(message, 6);
                }
            }
            WorkerProgressUpdate::Completed { message, .. } => {
                if self.cancelling() {
                    self.rows[index].state = DocumentRunState::Cancelled;
                    return;
                }
                self.rows[index].state = DocumentRunState::Completed;
                self.status = (tr("processing").into(), message.clone(), String::new());
                self.notice(message, 6);
                let path = &self.rows[index].path;
                self.queue.retain(|item| item.file_path != *path);
            }
            WorkerProgressUpdate::Error { error, .. } => {
                self.rows[index].state = if self.cancelling() {
                    DocumentRunState::Cancelled
                } else {
                    DocumentRunState::Failed
                };
                if !self.cancelling() {
                    self.status = (tr("failed").into(), error.clone(), String::new());
                    self.notice(error, 8);
                }
            }
        }
    }
    fn input_busy(&self) -> bool {
        !matches!(self.input_flow, InputFlow::Idle)
    }

    fn begin_input(&mut self, selecting: bool) -> (u64, flume::Receiver<()>) {
        self.input_generation += 1;
        let id = self.input_generation;
        let (owner, stop) = flume::bounded(1);
        let task = InputTask { id, _owner: owner };
        self.input_flow = if selecting {
            InputFlow::Selecting(task)
        } else {
            InputFlow::Probing(task)
        };
        (id, stop)
    }

    fn dialog(&mut self, kind: FileDialogKind) {
        if self.running() || self.input_busy() {
            return;
        }
        let (id, stop) = self.begin_input(true);
        let tx = self.sender.clone();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            match kind {
                FileDialogKind::Files => {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter(
                            &GUI_TEXT.interactive.popups.supported_inputs_filter,
                            &["pdf", "zip"],
                        )
                        .pick_files()
                    {
                        let _ = tx.send(Message::ProbeStarted(id));
                        wake.wake();
                        probe(tx.clone(), wake.clone(), paths, id, stop);
                    }
                }
                FileDialogKind::Folder => {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        let _ = tx.send(Message::ProbeStarted(id));
                        wake.wake();
                        probe(tx.clone(), wake.clone(), vec![path], id, stop);
                    }
                }
                FileDialogKind::Output => {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        let _ = tx.send(Message::Output(id, path));
                    }
                }
            }
            let _ = tx.send(Message::DialogClosed(id));
            wake.wake();
        });
    }
    fn probe_paths(&mut self, paths: Vec<PathBuf>) {
        if self.running() || self.input_busy() {
            return;
        }
        let (id, stop) = self.begin_input(false);
        let tx = self.sender.clone();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            probe(tx.clone(), wake.clone(), paths, id, stop);
            let _ = tx.send(Message::DialogClosed(id));
            wake.wake();
        });
    }
    fn cancel(&mut self) {
        self.input_flow = InputFlow::Idle;
        if self.running() {
            self.batch = BatchState::Cancelling;
            self.status.0 = GUI_TEXT.interactive.status.cancelling.clone();
            for handle in &mut self.handles {
                handle.kill();
            }
        }
    }
    fn start(&mut self) {
        if self.running() {
            self.cancel();
            return;
        }
        if self.input_busy() {
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
        self.batch = BatchState::Starting;
        self.generation += 1;
        let generation = self.generation;
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
                        .send(Message::Started(generation, infos, StartedWorkers(handles)))
                        .is_err()
                    {
                        return;
                    }
                    wake.wake();
                    while let Ok(event) = events.recv() {
                        if tx.send(Message::Worker(generation, event)).is_err() {
                            break;
                        }
                        wake.wake();
                    }
                }
                Err(error) => {
                    let _ = tx.send(Message::Notice(format!("{error:#}")));
                }
            }
            let _ = tx.send(Message::Finished(generation));
            wake.wake();
        });
    }
    fn resolution_focus(&mut self, focused: bool, snapshot: bool) {
        if focused && matches!(self.resolution_edit, ResolutionEdit::Displaying) {
            self.resolution_edit = ResolutionEdit::Editing {
                original: ResolutionPreset {
                    height: self.options.target_height.unwrap_or(1200),
                    width: self.options.target_width,
                },
            };
            self.resolution.clear();
            self.blink_start = Instant::now();
        } else if !focused {
            if let ResolutionEdit::Editing { original } =
                std::mem::replace(&mut self.resolution_edit, ResolutionEdit::Displaying)
            {
                let parsed = parse_resolution(self.resolution.text());
                let (height, width) = parsed.unwrap_or((original.height, original.width));
                self.options.target_height = Some(height);
                self.options.target_width = width;
                if parsed.is_some() && (height, width) != (original.height, original.width) {
                    self.options.target_device = None;
                    if !snapshot {
                        if let Err(error) =
                            backend::save_resolution_preset(&ResolutionPreset { height, width })
                        {
                            self.notice(error.to_string(), 8);
                        }
                    }
                    if self.options.crop_free_aspect && width.is_some() {
                        self.notice(tr("resolution_width_ignored"), 5);
                    }
                }
                self.resolution.set_text(&match width {
                    Some(width) => format!("{height}x{width}"),
                    None => height.to_string(),
                });
            }
        }
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

fn probe(
    tx: SyncSender<Message>,
    wake: Waker,
    paths: Vec<PathBuf>,
    id: u64,
    stop: flume::Receiver<()>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(2)
        .build()
    else {
        return;
    };
    let mut accepted = Vec::new();
    for path in paths {
        if stop.is_disconnected() || stop.try_recv().is_ok() {
            return;
        }
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
            let data = runtime.block_on(async {
                tokio::select! {
                    result = crate::worker_process::probe_file_json(&path) => Some(result.ok()),
                    _ = stop.recv_async() => None,
                }
            });
            let Some(data) = data else {
                return;
            };
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
    let _ = tx.send(Message::Inputs(id, accepted));
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
/// A logical font size in physical pixels, snapped for crisp rendering.
fn font(ui: &Ui<'_>, logical: f32) -> f32 {
    crate::theme::crisp(ui.scale.font(logical))
}
fn small(ui: &mut Ui<'_>, area: Rect, text: &str) {
    let mut style = ui.theme.body;
    style.size = crate::theme::crisp(style.size * 0.86);
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
        style.size = font(ui, 11.0);
        let width = ui
            .text
            .measure(first, style)
            .max(ui.text.measure(last, style));
        if width > available {
            style.size = crate::theme::crisp_down(style.size * available as f32 / width as f32);
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
        style.size = crate::theme::crisp_down(style.size * available as f32 / width as f32);
        ui.label_styled(area, text, style, ui.theme.text, Align::Centre);
    }
}
/// A checkbox whose edge never disappears. liminalkit fills a checked box and
/// draws a checked or focused box's edge in `accent`: the fill should stay
/// the selected colour, so the edge is drawn again in the border colour.
fn checkbox(ui: &mut Ui<'_>, area: Rect, checked: &mut bool, label: &str) -> bool {
    let accent = ui.theme.accent;
    ui.theme.accent = ui.theme.selection;
    let clicked = ui.checkbox(area, checked, label);
    ui.theme.accent = accent;
    // The box liminalkit draws: `CHECKBOX_SIZE`, vertically centred, 4px corners.
    let size = ui.px(liminalkit_ui::CHECKBOX_SIZE).min(area.h);
    let (x, y) = (area.x as f32, (area.y + (area.h - size) / 2) as f32);
    let (size, line, radius) = (
        size as f32,
        ui.px(ui.theme.border_width) as f32,
        ui.px(4) as f32,
    );
    let mut ring = Path::new();
    ring.rounded_rect(x, y, size, size, radius);
    ring.rounded_rect(
        x + line,
        y + line,
        size - 2.0 * line,
        size - 2.0 * line,
        (radius - line).max(0.0),
    );
    let edge = ui.theme.panel_edge;
    ui.painter
        .fill_path_rule(ui.kernel, &mut ring, FillRule::EvenOdd, edge, 255);
    clicked
}
/// A dashboard card, returning the area inside its padding. Its edge is a
/// little heavier than the controls' hairlines, and anti-aliased.
fn card(ui: &mut Ui<'_>, area: Rect) -> Rect {
    let theme = ui.theme;
    let corner = ui.px(theme.corner_radius);
    let radius = corner as f32;
    let edge = ui.scale.f(1.5);
    ui.painter
        .fill_rounded_rect(area.inset(1), corner - 1, theme.panel);
    let (x, y, w, h) = (area.x as f32, area.y as f32, area.w as f32, area.h as f32);
    let mut ring = Path::new();
    ring.rounded_rect(x, y, w, h, radius);
    ring.rounded_rect(
        x + edge,
        y + edge,
        w - 2.0 * edge,
        h - 2.0 * edge,
        (radius - edge).max(0.0),
    );
    ui.painter.fill_path_rule(
        ui.kernel,
        &mut ring,
        FillRule::EvenOdd,
        theme.panel_edge,
        255,
    );
    area.inset(ui.px(theme.padding))
}
fn arrow_button(ui: &mut Ui<'_>, area: Rect, direction: i32) -> bool {
    let clicked = ui.button(area, "");
    let half = ui.scale.f(4.0);
    let (cx, cy) = (
        area.x as f32 + area.w as f32 / 2.0,
        area.y as f32 + area.h as f32 / 2.0,
    );
    let tip = cx + half * direction as f32 / 2.0;
    let tail = cx - half * direction as f32 / 2.0;
    let points = [[tail, cy - half], [tip, cy], [tail, cy + half]];
    let colour = ui.theme.text;
    ui.painter
        .stroke_polyline_aa(ui.kernel, &points, ui.scale.f(1.5), colour, 255);
    clicked
}
/// `None` when every label fits a checkbox in `columns` equal columns of
/// `width` at body size; otherwise the largest crisp size at which they do.
fn checkbox_font(ui: &mut Ui<'_>, width: i32, columns: i32, labels: &[&str]) -> Option<f32> {
    let column = (width - ui.px(2) * (columns - 1)) / columns;
    let available = column - ui.px(liminalkit_ui::CHECKBOX_SIZE) - ui.px(ui.theme.padding / 2);
    let style = ui.theme.body;
    let widest = labels
        .iter()
        .map(|label| ui.text.measure(label, style))
        .max()
        .unwrap_or(0);
    (widest > available)
        .then(|| crate::theme::crisp_down(style.size * available.max(1) as f32 / widest as f32))
}
/// The center/crop/reflow row. Equal columns, aligned with a three-column
/// grid above, while the labels fit them; otherwise columns sized to their
/// labels, which are shrunk only if even that is too narrow. May lower
/// `ui.theme.body.size`; the caller restores it.
fn geometry_cells(ui: &mut Ui<'_>, area: Rect, labels: [&str; 3]) -> Vec<Rect> {
    let gap = ui.px(2);
    if checkbox_font(ui, area.w, 3, &labels).is_none() {
        return cells(area, 3, gap);
    }
    let chrome = ui.px(liminalkit_ui::CHECKBOX_SIZE) + ui.px(ui.theme.padding / 2) + ui.px(6);
    let style = ui.theme.body;
    let text: Vec<i32> = labels.iter().map(|l| ui.text.measure(l, style)).collect();
    let fixed = 3 * chrome + 2 * gap;
    let total: i32 = text.iter().sum();
    let room = (area.w - fixed).max(1);
    if total > room {
        ui.theme.body.size = crate::theme::crisp_down(style.size * room as f32 / total as f32);
    }
    // Spare width is shared equally; text widths shrink in proportion.
    let spare = (room - total).max(0) / 3;
    let mut x = area.x;
    let mut rects = Vec::with_capacity(3);
    for (i, width) in text.iter().enumerate() {
        let w = if i == 2 {
            area.right() - x
        } else {
            chrome + spare + (*width as i64 * room.min(total) as i64 / total as i64) as i32
        };
        rects.push(Rect::new(x, area.y, w, area.h));
        x += w + gap;
    }
    rects
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
        let frozen = self.running() || self.input_busy();
        let saved_input = frozen.then(|| std::mem::take(ui.input));
        let files = card(ui, bands.files);
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
                self.dialog(match i {
                    0 => FileDialogKind::Files,
                    1 => FileDialogKind::Folder,
                    _ => FileDialogKind::Output,
                });
            }
            hover(ui, rect, tooltip, i == 2, &mut note);
        }
        self.draw_right(ui, bands.left, &mut note);
        self.draw_left(ui, bands.right, &mut note, snapshot);
        if let Some(input) = saved_input {
            *ui.input = input;
        }
        let status = card(ui, bands.status);
        let actions = Actions::new(status, ui.scale);
        let original_theme = ui.theme;
        ui.theme.body.size = font(ui, 16.0);
        ui.theme.panel = ui.theme.selection;
        ui.theme.panel_edge = crate::theme::rgb(appearance::active_palette().border_focus);
        let start = ui.button(
            actions.primary,
            if self.running() {
                &t.buttons.cancel
            } else {
                &t.buttons.start_processing
            },
        );
        ui.theme = original_theme;
        if start && !snapshot {
            self.start();
        }
        ui.theme.body.size = font(ui, 11.0);
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
                .map(|item| (item.state.label(), item.path.as_path()))
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
        // The status panel's first line shares its height with the utilities
        // (and, in debug builds, the Debug button left of them).
        #[cfg(feature = "debug-logging")]
        let debug = Rect::new(
            actions.utilities[0].x - ui.px(66),
            status.y,
            ui.px(60),
            actions.utilities[0].h,
        );
        #[cfg(feature = "debug-logging")]
        let header_right = debug.x;
        #[cfg(not(feature = "debug-logging"))]
        let header_right = actions.utilities[0].x;
        let header = Rect::new(
            status.x,
            status.y,
            header_right - status.x - ui.px(8),
            actions.utilities[0].h,
        );
        if self.running() {
            label(ui, header, &self.status.0);
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
            {
                ui.meter(
                    Rect::new(status.x, actions.primary.y - ui.px(24), status.w, ui.px(18)),
                    self.batch_progress(),
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
            // Centred over the panel, the summary would land on the queue rows
            // or the empty-queue message.
            ui.label(header, &summary, ui.theme.text_dim, Align::Left);
            if let Some(output) = &self.options.output_path {
                // Left of the process button, on its row.
                small(
                    ui,
                    Rect::new(
                        status.x,
                        actions.primary.y,
                        actions.primary.x - status.x - ui.px(8),
                        actions.primary.h,
                    ),
                    &format!(
                        "{} {}",
                        t.buttons.output_directory,
                        backend::truncate_path(output, 90)
                    ),
                );
            }
        }
        #[cfg(feature = "debug-logging")]
        if ui.button(debug, &t.queue.debug_button) {
            self.modal = Some(Modal::Debug);
        }
        // At most two timed, click-to-dismiss cards, anchored bottom-right,
        // and above the process button when a narrow window puts it beneath.
        let mut remove = None;
        let notice_x = ui.painter.width() - ui.px(340);
        let mut bottom = if notice_x < actions.primary.right() {
            actions.primary.y - ui.px(6)
        } else {
            ui.painter.height() - ui.px(12)
        };
        for (index, notice) in self.notices.iter().enumerate().rev() {
            let area = Rect::new(notice_x, bottom - ui.px(68), ui.px(328), ui.px(68));
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
        let a = card(ui, area);
        let card = LeftCard::new(a, ui.scale);
        small(ui, card.range_label, &t.labels.page_range);
        small(ui, card.resolution_label, tr("resolution_short"));
        let (_, range_focused) = ui.text_field_input(&mut self.range, card.range, false, false);
        ui.paint_text_field(
            &mut self.range,
            card.range,
            &t.labels.page_range_placeholder,
            range_focused,
            false,
        );
        let body = ui.theme.body;
        let (_, focused) = ui.text_field_input(&mut self.resolution, card.resolution, false, false);
        self.resolution_focus(focused, snapshot);
        ui.paint_text_field(
            &mut self.resolution,
            card.resolution,
            "1200",
            focused,
            false,
        );
        ui.theme.body = body;
        if self.blink_start.elapsed().as_millis() % 1200 >= 600 {
            for caret in [self.range.caret_area(), self.resolution.caret_area()]
                .into_iter()
                .flatten()
            {
                ui.painter.fill_rect(caret, ui.theme.background);
            }
        }
        hover(ui, card.resolution, &t.tooltips.target_height, true, note);

        // Preserve the reading order, filling rows across the columns. The
        // geometry tray and OCR slot stay anchored when conditional controls
        // hide. Every possible label is measured, not only the visible ones,
        // so toggling an option never reflows the grid.
        let labels = [
            t.labels.inverted_colors.as_str(),
            &t.labels.ocr_text_layer,
            &t.labels.make_epub_also,
            &t.labels.high_quality_output,
            &t.labels.truetyping_text,
            &t.labels.jbig2_symbol_mode,
            &t.labels.jpeg_compatibility,
        ];
        let columns: i32 = if checkbox_font(ui, card.checks.w, 3, &labels).is_none() {
            3
        } else {
            2
        };
        // Two columns can still be too narrow for a long translation: shrink
        // the labels rather than cut them off.
        if let Some(size) = checkbox_font(ui, card.checks.w, columns, &labels) {
            ui.theme.body.size = size;
        }
        let mut index = 0;
        macro_rules! check {
            ($value:expr,$label:expr,$tip:expr,$changed:expr) => {{
                let row = cells(
                    Rect::new(
                        card.checks.x,
                        card.checks.y + (index / columns) * card.check_row,
                        card.checks.w,
                        card.check_row,
                    ),
                    columns as usize,
                    ui.px(2),
                );
                let rect = row[(index % columns) as usize];
                let mut value = $value;
                if checkbox(ui, rect, &mut value, $label) {
                    $changed(value);
                }
                hover(ui, rect, $tip, true, note);
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
                true,
                note,
            );
        }
        let buttons = geometry_cells(
            ui,
            card.geometry,
            [tr("center_margins"), tr("crop_margins"), &t.labels.reflow],
        );
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
            let rect = buttons[index];
            let mut checked = selected;
            if checkbox(ui, rect, &mut checked, text) {
                self.select_geometry(index);
            }
            hover(ui, rect, tooltip, true, note);
        }
        ui.theme.body = body;
    }

    fn select_geometry(&mut self, index: usize) {
        let current = self.options.geometry_mode();
        let next = match index {
            0 if current == GeometryMode::Centered => GeometryMode::Unchanged,
            0 => GeometryMode::Centered,
            1 if matches!(current, GeometryMode::Cropped { .. }) => GeometryMode::Unchanged,
            1 => GeometryMode::Cropped {
                footnotes: self.options.crop_footnotes,
                free_aspect: true,
            },
            _ if current == GeometryMode::Reflow => GeometryMode::Unchanged,
            _ => GeometryMode::Reflow,
        };
        self.options.set_geometry_mode(next);
    }

    pub(crate) fn select_binarization(&mut self, index: usize) {
        let kind = match index {
            0 => BinarizationKind::Adaptive,
            1 => BinarizationKind::Fixed,
            _ => BinarizationKind::Heavy,
        };
        self.options.set_binarization_kind(kind);
    }

    fn draw_right(&mut self, ui: &mut Ui<'_>, area: Rect, note: &mut Option<(bool, &'static str)>) {
        let t = &GUI_TEXT.interactive;
        let a = card(ui, area);
        let card = RightCard::new(a, ui.scale);
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
                        false,
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
                    hover(ui, control, &t.tooltips.base_format, false, note);
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
                        false,
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
                    hover(ui, control, &t.tooltips.layout_detection, false, note);
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
                            false,
                            note,
                        );
                    }
                }
            }
        }
        if !self.options.grayscale_mode && !self.options.preserve_mode {
            let (modes, caption, value) = card.binarization_fields();
            small(
                ui,
                Rect::new(
                    card.binarization.x,
                    modes.y,
                    modes.x - card.binarization.x,
                    modes.h,
                ),
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
            let fixed = self.options.use_fixed_threshold;
            if !heavy {
                small(ui, caption, if fixed { "0–255" } else { "K" });
                let body = ui.theme.body;
                ui.theme.body.size = font(ui, 12.0);
                if fixed {
                    ui.text_field(&mut self.threshold, value, "180", false, false);
                } else {
                    ui.text_field(&mut self.k, value, "0.05", false, false);
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
                    false,
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
                false,
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
        // Dim the dashboard: the dialog shares the cards' fill and would
        // otherwise read as one more panel among them.
        let window = Rect::new(0, 0, ui.painter.width(), ui.painter.height());
        ui.painter.blend_rect(window, 0x000000, 72);
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
                // The bundled face has no arrow glyphs; draw the steppers as
                // chevrons. Light/Dark show which variant is being previewed.
                let buttons = cells(row(body, 2, row_h), 4, ui.px(4));
                if arrow_button(ui, buttons[0], -1) {
                    self.step_theme(-1);
                }
                if arrow_button(ui, buttons[1], 1) {
                    self.step_theme(1);
                }
                let dark = (self.theme_index > 0).then(|| (self.theme_index - 1) % 2 == 1);
                for (index, text) in [(2, tr("light")), (3, tr("dark"))] {
                    if choice(ui, buttons[index], text, dark == Some(index == 3)) {
                        if self.theme_index == 0 {
                            self.theme_index = 1;
                        }
                        let combo = (self.theme_index - 1) / 2;
                        self.theme_index = 1 + combo * 2 + usize::from(index == 3);
                        self.preview_theme();
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
                if self.running() {
                    small(ui, rect, &t.buttons.clear_queue);
                } else if ui.button(rect, &t.buttons.clear_queue) {
                    self.queue.clear();
                    self.rows.clear();
                }
            }
            Modal::Log => {
                let list = Rect::new(body.x, body.y, body.w, body.h - row_h - ui.px(6));
                let entries: Vec<_> = self.log.iter().rev().take(30).collect();
                if entries.is_empty() {
                    ui.label(list, tr("log_empty"), ui.theme.text_dim, Align::Centre);
                }
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
                if !self.running()
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
    let (value, stages) = match m.mode {
        WorkerProgressMode::Epub | WorkerProgressMode::Layout | WorkerProgressMode::Margin => (
            u64::from(m.rendered) + u64::from(m.detected) + u64::from(m.encoded),
            3,
        ),
        WorkerProgressMode::Reflow => (u64::from(m.encoded), 1),
        _ => (u64::from(m.rendered) + u64::from(m.encoded), 2),
    };
    (value as f64 / (u64::from(m.pages_total) * stages) as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_batch() -> Dashboard {
        let mut dashboard = Dashboard::new();
        dashboard.batch = BatchState::Running;
        dashboard.generation = 2;
        for id in 0..2 {
            let path = PathBuf::from(format!("input-{id}.pdf"));
            dashboard.queue.push_back(DocumentItem::new(path.clone()));
            dashboard.rows.push(QueueRow {
                task_id: id,
                path,
                output: None,
                state: DocumentRunState::Queued,
            });
        }
        dashboard
    }

    fn metrics(current: u32) -> WorkerProgressMetrics {
        WorkerProgressMetrics {
            pages_total: 10,
            rendered: current,
            detected: current,
            encoded: current,
            mode: WorkerProgressMode::NoLayout,
            is_djvu: false,
            enable_layout_detection: false,
            eta_seconds: None,
        }
    }

    #[test]
    fn lifecycle_batch_progress_includes_every_file_and_reserves_completion_for_terminal_events() {
        let mut dashboard = active_batch();
        dashboard.worker_event(WorkerProgressUpdate::Completed {
            task_id: 0,
            message: "done".into(),
            metrics: Some(metrics(10)),
        });
        assert_eq!(dashboard.batch_progress(), 0.5);
        dashboard.worker_event(WorkerProgressUpdate::Status {
            task_id: 1,
            status: WorkerProcessingStatus::Initializing,
            metrics: Some(metrics(1)),
        });
        assert!(dashboard.batch_progress() > 0.5 && dashboard.batch_progress() < 0.6);
        dashboard.worker_event(WorkerProgressUpdate::Status {
            task_id: 1,
            status: WorkerProcessingStatus::AssemblingOutput,
            metrics: Some(metrics(10)),
        });
        assert!(dashboard.batch_progress() < 1.0);
        dashboard.worker_event(WorkerProgressUpdate::Completed {
            task_id: 1,
            message: "done".into(),
            metrics: Some(metrics(10)),
        });
        assert_eq!(dashboard.batch_progress(), 1.0);
        assert!(dashboard.queue.is_empty());
    }

    #[test]
    fn lifecycle_progress_waits_for_downstream_stages() {
        let mut no_layout = metrics(0);
        no_layout.rendered = no_layout.pages_total;
        assert_eq!(progress_fraction(no_layout), 0.5);
        no_layout.mode = WorkerProgressMode::Layout;
        assert_eq!(progress_fraction(no_layout), 1.0 / 3.0);
    }

    #[test]
    fn lifecycle_cancel_settles_unstarted_rows_and_rejects_events_from_old_batches() {
        let mut dashboard = active_batch();
        dashboard
            .sender
            .send(Message::Worker(
                1,
                WorkerProgressUpdate::Completed {
                    task_id: 0,
                    message: "stale".into(),
                    metrics: None,
                },
            ))
            .unwrap();
        dashboard.sender.send(Message::Finished(1)).unwrap();
        dashboard.poll();
        assert!(dashboard.running());
        assert_eq!(dashboard.queue.len(), 2);
        assert_eq!(dashboard.rows[0].state, DocumentRunState::Queued);
        dashboard.cancel();
        dashboard.sender.send(Message::Finished(2)).unwrap();
        dashboard.poll();
        assert_eq!(
            dashboard.batch,
            BatchState::Finished(BatchOutcome::Cancelled)
        );
        assert!(!dashboard.running());
        assert!(!dashboard.cancelling());
        assert!(
            dashboard
                .rows
                .iter()
                .all(|row| row.state == DocumentRunState::Cancelled)
        );
        assert_eq!(dashboard.queue.len(), 2);
        assert_eq!(dashboard.status.0, tr("cancelled"));
    }

    #[test]
    fn lifecycle_input_flow_ignores_late_results_and_cancels_on_owner_drop() {
        let mut dashboard = Dashboard::new();
        let (old, old_stop) = dashboard.begin_input(true);
        dashboard.sender.send(Message::ProbeStarted(old)).unwrap();
        dashboard.poll();
        assert!(matches!(dashboard.input_flow, InputFlow::Probing(_)));
        dashboard.input_flow = InputFlow::Idle;
        assert!(old_stop.is_disconnected());
        let (current, _stop) = dashboard.begin_input(false);
        dashboard
            .sender
            .send(Message::Inputs(
                old,
                vec![(PathBuf::from("old.pdf"), Some(1), None)],
            ))
            .unwrap();
        dashboard.sender.send(Message::DialogClosed(old)).unwrap();
        dashboard.poll();
        assert!(dashboard.queue.is_empty());
        assert_eq!(dashboard.input_flow.id(), Some(current));
        dashboard
            .sender
            .send(Message::Inputs(
                current,
                vec![(PathBuf::from("current.pdf"), Some(1), None)],
            ))
            .unwrap();
        dashboard
            .sender
            .send(Message::DialogClosed(current))
            .unwrap();
        dashboard.poll();
        assert_eq!(dashboard.queue.len(), 1);
        assert!(!dashboard.input_busy());
    }

    #[test]
    fn lifecycle_resolution_draft_does_not_change_the_committed_value() {
        let mut dashboard = Dashboard::new();
        dashboard.options.target_height = Some(1200);
        dashboard.options.target_width = Some(800);
        dashboard.resolution_focus(true, true);
        dashboard.resolution.set_text("1600x1000");
        assert_eq!(dashboard.options.target_height, Some(1200));
        assert_eq!(dashboard.options.target_width, Some(800));
        dashboard.resolution_focus(false, true);
        assert_eq!(dashboard.options.target_height, Some(1600));
        assert_eq!(dashboard.options.target_width, Some(1000));
        dashboard.resolution_focus(true, true);
        dashboard.resolution.set_text("invalid");
        dashboard.resolution_focus(false, true);
        assert_eq!(dashboard.resolution.text(), "1600x1000");
        assert_eq!(dashboard.options.target_height, Some(1600));
    }
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
        let (_owner, stop) = flume::bounded(1);
        let thread = std::thread::spawn(move || {
            probe(
                sender,
                wake,
                vec![PathBuf::from("bad-one.txt"), PathBuf::from("bad-two.txt")],
                1,
                stop,
            )
        });
        let mut done = false;
        for _ in 0..8 {
            wake_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("probe must wake before it fills its bounded channel");
            for message in receiver.try_iter() {
                if matches!(message, Message::Inputs(_, _)) {
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
                .send(Message::Started(
                    0,
                    Vec::new(),
                    StartedWorkers(vec![handle])
                ))
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
        let (_, mode) = RightCard::new(bands.left.inset(8), Scale::ONE).setting(0);
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
            task_id: 0,
            path,
            output: None,
            state: DocumentRunState::Processing {
                fraction: 0.0,
                reflow_phase: None,
            },
        });
        dashboard.batch = BatchState::Running;
        dashboard.cancel();
        dashboard.worker_event(WorkerProgressUpdate::Completed {
            task_id: 0,
            message: "done".into(),
            metrics: None,
        });
        assert_eq!(dashboard.queue.len(), 1);
        assert_eq!(dashboard.rows[0].state, DocumentRunState::Cancelled);
        assert!(dashboard.notices.is_empty());
    }
    #[test]
    fn geometry_buttons_are_exclusive_and_can_all_be_off() {
        let mut app = LegeApp::new();
        app.dashboard.options = ProcessingOptions::new();
        let mut buffer = WindowBuffer::new(940, 700);
        let bands = Bands::new(940, 700, Scale::ONE);
        let area = LeftCard::new(bands.right.inset(8), Scale::ONE)
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
    fn threshold_parameter_is_inactive_under_heavy() {
        let mut app = LegeApp::new();
        app.dashboard.options = ProcessingOptions::new();
        app.dashboard.threshold.set_text("180");
        let mut buffer = WindowBuffer::new(940, 700);
        let bands = Bands::new(940, 700, Scale::ONE);
        let card = RightCard::new(bands.left.inset(8), Scale::ONE);
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
    fn resolution_focus_clears_and_clicking_blank_restores_the_saved_value() {
        let mut app = LegeApp::new();
        app.dashboard.options.target_height = Some(1200);
        app.dashboard.options.target_width = Some(800);
        app.dashboard.resolution.set_text("1200x800");
        let mut buffer = WindowBuffer::new(940, 700);
        let bands = Bands::new(940, 700, Scale::ONE);
        let field = LeftCard::new(bands.right.inset(8), Scale::ONE).resolution;
        click(
            &mut app,
            &mut buffer,
            (field.x + 10) as f32,
            (field.y + 10) as f32,
        );
        assert_eq!(app.dashboard.resolution.text(), "");
        click(&mut app, &mut buffer, 20.0, 650.0);
        assert_eq!(app.dashboard.resolution.text(), "1200x800");
        assert!(matches!(
            app.dashboard.resolution_edit,
            ResolutionEdit::Displaying
        ));
    }
    #[test]
    fn compact_resolution_field_fits_height_and_width() {
        let mut text = TextCache::new(crate::theme::fonts());
        let style = crate::theme::theme(appearance::active_palette()).body;
        let field = LeftCard::new(Rect::new(0, 0, 340, 236), Scale::ONE).resolution;
        assert!(text.measure("1200x800", style) <= field.w - 8);
    }
    fn real_worker(cancel: bool, batch: bool) {
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
        let second_input = tempfile::tempdir().unwrap();
        if batch {
            let second = second_input.path().join("second-input.pdf");
            std::fs::copy(&input, &second).unwrap();
            app.dashboard.queue.push_back(DocumentItem::new(second));
        }
        app.dashboard.start();
        assert!(app.dashboard.running());
        if cancel {
            app.dashboard.cancel();
        }
        let mut buffer = WindowBuffer::new(940, 700);
        let deadline = Instant::now() + Duration::from_secs(60);
        while app.dashboard.running() {
            app.render(&mut buffer, Scale::ONE);
            if Instant::now() > deadline {
                app.dashboard.cancel();
                panic!("worker timed out");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(!app.dashboard.rows.is_empty());
        assert_eq!(app.dashboard.rows.len(), if batch { 2 } else { 1 });
        if cancel {
            assert_eq!(app.dashboard.queue.len(), 1);
            assert!(
                app.dashboard
                    .rows
                    .iter()
                    .all(|row| row.state != DocumentRunState::Completed)
            );
        } else {
            assert!(
                app.dashboard
                    .rows
                    .iter()
                    .all(|row| row.state == DocumentRunState::Completed),
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
        real_worker(false, false);
    }
    #[test]
    #[ignore = "requires built lege on PATH and LEGE_GUI_SMOKE_INPUT/OUTPUT"]
    fn batch_real_worker_smoke() {
        real_worker(false, true);
    }
    #[test]
    #[ignore = "requires built lege on PATH and LEGE_GUI_SMOKE_INPUT/OUTPUT"]
    fn cancel_real_worker_smoke() {
        real_worker(true, false);
    }
}
