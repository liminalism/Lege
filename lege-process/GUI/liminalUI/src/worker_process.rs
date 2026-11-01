use std::collections::VecDeque;
/// Subprocess-based processing worker for the GUI.
///
/// Each queue item spawns a hidden `lege` CLI process in `--gui-worker` mode.
/// Progress events arrive as newline-delimited JSON on the child's stdout and
/// are deserialized into local protocol types, then forwarded to a merged
/// flume channel that the GUI progress loop consumes exactly as before.
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Result;

use crate::models::{
    BinarizationKind, CompressionType, CoverImageType, ImageProcessingType, OcrMode, OutputFormat,
    PageTreatment, ProcessingOptions,
};

// ── Protocol types ────────────────────────────────────────────────────────────
// These mirror lege::progress::* exactly so that serde can round-trip the
// newline-delimited JSON emitted by `lege --gui-worker`.

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub enum WorkerProgressMode {
    Unknown,
    NoLayout,
    Layout,
    Margin,
    HeavySequential,
    Reflow,
    Epub,
}

impl Default for WorkerProgressMode {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub struct WorkerProgressMetrics {
    pub pages_total: u32,
    pub rendered: u32,
    pub detected: u32,
    pub encoded: u32,
    pub mode: WorkerProgressMode,
    pub is_djvu: bool,
    pub enable_layout_detection: bool,
    pub eta_seconds: Option<u32>,
}

/// Mirror of lege::progress::ProcessingStatus — variants must match the
/// `#[serde(tag = "kind", rename_all = "snake_case")]` representation.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerProcessingStatus {
    Initializing,
    AssemblingOutput,
    Complete {
        message: String,
    },
    Error {
        error: String,
    },
    FootnotesDetected {
        message: String,
    },
    OcrLayerDetected {
        has_ocr: bool,
    },
    MarginPass1Analyzing,
    MarginAnalysisSummary {
        summary: String,
    },
    PipelineMessage {
        stage: String,
        message: String,
    },
    PdfAppend {
        current: usize,
        total: usize,
    },
    PdfAppendMargin {
        current: usize,
        total: usize,
    },
    ReflowProgress {
        stage: WorkerReflowStage,
        current: usize,
        total: usize,
        eta: Option<String>,
    },
    EpubProgress {
        rendered: usize,
        detected: usize,
        ocr: usize,
        total: usize,
        eta: Option<String>,
    },
    LayoutProgress {
        rendered: usize,
        detected: usize,
        encoded: usize,
        total: usize,
        enable_layout_detection: bool,
        eta: Option<String>,
    },
    NoLayoutProgress {
        rendered: usize,
        encoded: usize,
        total: usize,
        eta: Option<String>,
    },
    MarginProgress {
        pass1_rendered: usize,
        pass1_detected: usize,
        pass2_processed: usize,
        total: usize,
        enable_layout_detection: bool,
        eta: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerReflowStage {
    SourceAnalysis,
    Compose,
    OutputPages,
}

impl WorkerProcessingStatus {
    /// Returns three display lines for the GUI (no ANSI codes).
    pub fn to_gui_display_lines(&self) -> (String, String, String) {
        match self {
            Self::Initializing => (
                "[Initializing]".into(),
                "Preparing pipeline...".into(),
                String::new(),
            ),
            Self::AssemblingOutput => (
                "[Finalizing]".into(),
                "Assembling output file...".into(),
                "This may take a moment.".into(),
            ),
            Self::Complete { message } => (
                "[Complete]".into(),
                message.clone(),
                "Ready for next task.".into(),
            ),
            Self::Error { error } => ("[Error]".into(), "An error occurred.".into(), error.clone()),
            Self::FootnotesDetected { message } => (
                "[Margin Analysis]".into(),
                "Margin Analysis: note".into(),
                format!("{} | Footnotes preserved (centering used).", message),
            ),
            Self::OcrLayerDetected { has_ocr } => {
                if *has_ocr {
                    (
                        "[OCR Detection]".into(),
                        "OCR layer detected in document".into(),
                        "Leave OCR disabled to preserve existing text layer.".into(),
                    )
                } else {
                    (
                        "[OCR Detection]".into(),
                        "No OCR layer found".into(),
                        "Enable OCR if you want to add text layer.".into(),
                    )
                }
            }
            Self::MarginPass1Analyzing => (
                "[Margin Analysis - Pass 1]".into(),
                "Preparing document-wide margin analysis...".into(),
                "Progress will update as pages are analyzed.".into(),
            ),
            Self::MarginAnalysisSummary { summary } => (
                "[Margin Analysis - Pass 1]".into(),
                "Margin Analysis: complete".into(),
                summary.clone(),
            ),
            Self::PipelineMessage { stage, message } => {
                (format!("[{stage}]"), message.clone(), String::new())
            }
            Self::PdfAppend { .. } | Self::PdfAppendMargin { .. } => {
                (String::new(), String::new(), String::new())
            }
            Self::ReflowProgress {
                stage,
                current,
                total,
                eta,
            } => {
                let detail = match stage {
                    WorkerReflowStage::SourceAnalysis => {
                        format!("Rendering source pages and detecting layout: {current}/{total}")
                    }
                    WorkerReflowStage::Compose => {
                        "Building reflow plan from detected regions, rows, and words...".into()
                    }
                    WorkerReflowStage::OutputPages => {
                        format!("Rasterizing and encoding reflowed output pages: {current}/{total}")
                    }
                };
                let title = match stage {
                    WorkerReflowStage::SourceAnalysis => "[Reflow - Source Analysis]",
                    WorkerReflowStage::Compose => "[Reflow - Compose]",
                    WorkerReflowStage::OutputPages => "[Reflow - Output Pages]",
                };
                (
                    title.into(),
                    detail,
                    eta.as_ref()
                        .map(|eta| format!("Estimated time remaining: {eta}"))
                        .unwrap_or_default(),
                )
            }
            Self::EpubProgress {
                rendered,
                detected,
                ocr,
                total,
                eta,
            } => {
                let detail = format!(
                    "Render {rendered}/{total} | Layout {detected}/{total} | OCR {ocr}/{total}"
                );
                (
                    "[EPUB]".into(),
                    detail,
                    eta.as_ref()
                        .map(|eta| format!("Estimated time remaining: {eta}"))
                        .unwrap_or_default(),
                )
            }
            Self::LayoutProgress {
                rendered,
                detected,
                encoded,
                total,
                eta,
                ..
            } => {
                let detail = format!(
                    "Render {rendered}/{total} | Infer {detected}/{total} | Encode {encoded}/{total}"
                );
                (
                    "[Layout Mode]".into(),
                    detail,
                    eta.as_ref()
                        .map(|eta| format!("Estimated time remaining: {eta}"))
                        .unwrap_or_default(),
                )
            }
            Self::NoLayoutProgress {
                rendered,
                encoded,
                total,
                eta,
            } => {
                let detail = format!("Render {rendered}/{total} | Encode {encoded}/{total}");
                (
                    "[No-Layout Mode]".into(),
                    detail,
                    eta.as_ref()
                        .map(|eta| format!("Estimated time remaining: {eta}"))
                        .unwrap_or_default(),
                )
            }
            Self::MarginProgress {
                pass1_rendered,
                pass1_detected,
                pass2_processed,
                total,
                eta,
                ..
            } => {
                let detail = format!(
                    "Analyze {pass1_rendered}/{total} | Infer {pass1_detected}/{total} | Process {pass2_processed}/{total}"
                );
                (
                    "[Margin Mode]".into(),
                    detail,
                    eta.as_ref()
                        .map(|eta| format!("Estimated time remaining: {eta}"))
                        .unwrap_or_default(),
                )
            }
        }
    }
}

/// Mirror of lege::progress::ProgressUpdate.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerProgressUpdate {
    Status {
        task_id: u64,
        status: WorkerProcessingStatus,
        metrics: Option<WorkerProgressMetrics>,
    },
    Completed {
        task_id: u64,
        message: String,
        metrics: Option<WorkerProgressMetrics>,
    },
    Error {
        task_id: u64,
        error: String,
        metrics: Option<WorkerProgressMetrics>,
    },
}

impl WorkerProgressUpdate {
    /// Rewrite the task_id embedded in the event (so GUI-assigned IDs match tracker_infos).
    pub fn with_task_id(self, new_id: u64) -> Self {
        match self {
            Self::Status {
                status, metrics, ..
            } => Self::Status {
                task_id: new_id,
                status,
                metrics,
            },
            Self::Completed {
                message, metrics, ..
            } => Self::Completed {
                task_id: new_id,
                message,
                metrics,
            },
            Self::Error { error, metrics, .. } => Self::Error {
                task_id: new_id,
                error,
                metrics,
            },
        }
    }
}

// ── CLI arg generation ────────────────────────────────────────────────────────

/// Map GUI ProcessingOptions to CLI arguments.
/// Set `gui_worker = true` for JSON-event mode; `false` for the normal human CLI.
pub fn gui_options_to_cli_args(
    input_path: &PathBuf,
    output_path: &PathBuf,
    options: &ProcessingOptions,
    gui_worker: bool,
) -> Vec<OsString> {
    let mut normalized_options = options.clone();
    normalized_options.normalize_processing_dependencies();
    let options = &normalized_options;
    let mut args: Vec<OsString> = Vec::new();

    if gui_worker {
        args.push("--gui-worker".into());
    }
    args.push(input_path.as_os_str().into());
    args.push("--output".into());
    // Pass the full output file path; the CLI passes through paths with extensions.
    args.push(output_path.as_os_str().into());

    // Text / output format
    let text_format = options.effective_text_format();
    args.push("--text-format".into());
    args.push(text_format.into());

    if options.make_epub_also && !matches!(options.output_format, OutputFormat::Epub) {
        args.push("--epub-sidecar-output".into());
        args.push(output_path.with_extension("epub").as_os_str().into());
    }

    // Cover / image format
    let preserve_cover = options.original_cover
        && !options.no_front_cover
        && !matches!(options.cover_image_type, CoverImageType::None);
    let cover_format = match (&options.cover_image_type, preserve_cover) {
        (_, false) | (CoverImageType::None, _) => "none",
        (CoverImageType::Jpeg, true) => "jpeg",
        (CoverImageType::Jpeg2000, true) => "jp2",
    };
    if matches!(
        options.output_format,
        OutputFormat::Pdf | OutputFormat::Djvu
    ) {
        args.push("--cover-format".into());
        args.push(cover_format.into());
        if !preserve_cover {
            args.push("--no-cover".into());
        }
    }

    // Layout detection
    if !options.layout_analysis {
        args.push("--no-layout".into());
    }
    if options.layout_analysis
        && let Some(ref pages) = options.layout_exclusion_pages
    {
        if !pages.trim().is_empty() {
            args.push("--exclude-layout".into());
            args.push(pages.trim().into());
        }
    }

    // OCR
    if options.use_ocr {
        args.push("--ocr-mode".into());
        args.push(
            match options.ocr_mode {
                OcrMode::Fast => "fast",
                OcrMode::Thorough => "best",
            }
            .into(),
        );
        // In the desktop GUI, document analysis is one OCR feature: both
        // recognition modes generate the search overlay, synthesized outline,
        // and inferred metadata.
        args.push("--auto-toc".into());
    } else {
        args.push("--no-ocr".into());
    }

    // Reflow
    if options.reflow {
        args.push("--reflow".into());
    }

    // Image processing
    // JBIG2 halftone is an image-region codec choice, independent of whether
    // the page's text plane is truetyping or JBIG2.
    let use_jbig2_halftone = matches!(options.output_format, OutputFormat::Pdf)
        && options.layout_analysis
        && options.uses_jbig2_halftone_images();
    if use_jbig2_halftone {
        args.push("--halftone".into());
    } else if matches!(options.image_processing_type, ImageProcessingType::Dithered)
        && options.layout_analysis
    {
        args.push("--dither".into());
    }

    // Quality
    if options.jpeg_compat {
        args.push("--jpeg-compat".into());
    }
    // Mirrors the toggle's own visibility condition: symbol substitution only
    // exists while JBIG2 is the text encoder, so the flag is only sent then.
    // Without it the worker writes the bit-exact generic region.
    if options.symbol_mode
        && matches!(options.output_format, OutputFormat::Pdf)
        && !options.jpeg_compat
        && !options.grayscale_mode
        && matches!(options.compression_type, CompressionType::Jbig2)
    {
        args.push("--sym-unify".into());
    }
    if options.high_quality_output {
        args.push("--high-quality".into());
    }
    if options.invert_input {
        args.push("--invert".into());
    }

    // Margin processing
    if options.center_margins {
        args.push("--center-margins".into());
    } else if options.crop_margins {
        args.push("--crop-margins".into());
    }
    if options.crop_margins || options.crop_free_aspect {
        args.push("--crop-free-aspect".into());
    }
    if options.crop_footnotes {
        args.push("--force-crop".into());
    }

    match options.page_treatment() {
        PageTreatment::Preserved => args.push("--preserve".into()),
        PageTreatment::Grayscale => args.push("--grayscale".into()),
        PageTreatment::Binarized(BinarizationKind::Fixed) => {
            args.extend([
                "--binarization".into(),
                "fixed".into(),
                "--threshold".into(),
                options.threshold_value.to_string().into(),
            ]);
        }
        PageTreatment::Binarized(BinarizationKind::Heavy) => {
            args.extend(["--binarization".into(), "heavy".into()]);
        }
        PageTreatment::Binarized(BinarizationKind::Adaptive) => {
            args.extend([
                "--binarization".into(),
                "adaptive".into(),
                "--sauvola-k".into(),
                options.k_factor.to_string().into(),
            ]);
        }
    }

    // Target dimensions via explicit flags. The trailing-positional form is
    // ambiguous: the CLI reads a bare number below 500 as a *page number*, so a
    // small target height (e.g. a stale preset "300") silently became the page
    // range "300-300" and processed nothing (or crashed on short documents).
    if let Some(height) = options.target_height {
        args.push("--target-height".into());
        args.push(OsString::from(height.to_string()));
        if let Some(width) = options.target_width.filter(|_| !options.crop_free_aspect) {
            args.push("--target-width".into());
            args.push(OsString::from(width.to_string()));
        }
    }

    // Page range stays positional (its format is unambiguous).
    if let Some(ref range) = options.page_range {
        if !range.trim().is_empty() {
            args.push(OsString::from(range.as_str()));
        }
    }

    args
}

// ── Worker handle and subprocess spawning ────────────────────────────────────

/// A cancellation address for one child. Only its owner thread signals the
/// OS process, so a delayed cancellation can never target a reused PID.
#[derive(Clone)]
pub struct WorkerStop {
    pid: u32,
    sender: flume::Sender<()>,
}

impl WorkerStop {
    pub fn pid(&self) -> u32 {
        self.pid
    }
    fn request(&self) {
        let _ = self.sender.try_send(());
    }
}

pub enum WorkerHandle {
    Process(WorkerStop),
    Supervisor {
        active: Arc<Mutex<Vec<WorkerStop>>>,
        cancelled: Arc<AtomicBool>,
    },
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.kill();
    }
}

impl WorkerHandle {
    pub fn supervisor(active: Arc<Mutex<Vec<WorkerStop>>>, cancelled: Arc<AtomicBool>) -> Self {
        Self::Supervisor { active, cancelled }
    }
    pub fn pid(&self) -> u32 {
        match self {
            Self::Process(stop) => stop.pid,
            Self::Supervisor { .. } => 0,
        }
    }
    pub fn stop_handle(&self) -> WorkerStop {
        match self {
            Self::Process(stop) => stop.clone(),
            Self::Supervisor { .. } => unreachable!("only a process has a stop handle"),
        }
    }
    pub fn kill(&mut self) {
        match self {
            Self::Process(stop) => stop.request(),
            Self::Supervisor { active, cancelled } => {
                cancelled.store(true, Ordering::SeqCst);
                for stop in active
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .iter()
                {
                    stop.request();
                }
            }
        }
    }
}

/// Child ownership persists through graceful termination and forced kill.
fn wait_for_child(
    child: &mut std::process::Child,
    stop: flume::Receiver<()>,
    deadline: Option<std::time::Instant>,
) -> std::io::Result<std::process::ExitStatus> {
    enum Phase {
        Running,
        Stopping(std::time::Instant),
        Killed,
    }
    let mut phase = Phase::Running;
    let mut timed_out = false;
    loop {
        if let Some(status) = child.try_wait()? {
            return if timed_out {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "worker probe timed out",
                ))
            } else {
                Ok(status)
            };
        }
        timed_out |= deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        let request = timed_out
            || match stop.recv_timeout(std::time::Duration::from_millis(20)) {
                Ok(()) => true,
                Err(flume::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    true
                }
                Err(flume::RecvTimeoutError::Timeout) => false,
            };
        if timed_out {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        match phase {
            Phase::Running if request => {
                kill_pid(child.id());
                phase = Phase::Stopping(std::time::Instant::now());
            }
            Phase::Stopping(start) if start.elapsed() >= std::time::Duration::from_secs(3) => {
                let _ = child.kill();
                phase = Phase::Killed;
            }
            _ => {}
        }
    }
}

fn kill_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

/// Append a line to the persistent worker diagnostics log
/// (`<app-data>/Lege/worker.log`).
///
/// A Finder-launched macOS bundle has no terminal, so worker stderr and spawn
/// failures are otherwise invisible; this file is the after-the-fact record a
/// tester can send back. Best-effort: logging must never break processing.
/// The file is truncated once it grows beyond ~2 MB.
pub fn append_worker_log(line: &str) {
    use std::io::Write;

    let path = crate::settings::get_worker_log_path();
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 2 * 1024 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "[{timestamp}] {line}");
}

fn resolve_cli_path() -> Result<PathBuf> {
    // Look for lege.exe next to lege-gui.exe first (installed / release layout).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(if cfg!(windows) { "lege.exe" } else { "lege" });
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Dev fallback: assume `lege` is on PATH (cargo run layout).
    Ok(PathBuf::from(if cfg!(windows) {
        "lege.exe"
    } else {
        "lege"
    }))
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn macos_bundle_contents_dir(cli_path: &Path) -> Option<PathBuf> {
    let executable_dir = cli_path.parent()?;
    if executable_dir.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    executable_dir
        .parent()
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("Contents"))
        .map(Path::to_path_buf)
}

fn configure_cli_command(cmd: &mut std::process::Command, cli_path: &Path) {
    if let Some(executable_dir) = cli_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty() && path.is_dir())
    {
        cmd.current_dir(executable_dir);
        if std::env::var_os("LEGE_ASSET_DIR").is_none() {
            cmd.env("LEGE_ASSET_DIR", executable_dir);
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(contents_dir) = macos_bundle_contents_dir(cli_path) {
        let resources_dir = contents_dir.join("Resources");

        cmd.env("LEGE_ASSET_DIR", &resources_dir);
        cmd.env("LEGE_DATA_DIR", &resources_dir);
    }
}

/// Append a timestamped line to a run's own full log file. Best-effort like
/// `append_worker_log`; the file is what a click on the run's history row
/// opens, so it must never block or fail processing.
fn append_run_log(path: &std::path::Path, line: &str) {
    use std::io::Write;

    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "[{timestamp}] {line}");
}

/// Spawn a hidden `lege --gui-worker` child process and stream its
/// newline-delimited JSON progress events into `events_tx`.
pub fn spawn_lege_worker(
    gui_task_id: u64,
    input_path: PathBuf,
    output_path: PathBuf,
    options: &ProcessingOptions,
    events_tx: flume::Sender<WorkerProgressUpdate>,
    log_tx: Option<flume::Sender<String>>,
) -> Result<WorkerHandle> {
    let cli_path = resolve_cli_path()?;
    let cli_args = gui_options_to_cli_args(&input_path, &output_path, options, true);

    // One full log file per run, keyed by the (unique) output name. History
    // rows in the log viewer open this file; pruned with the entry list.
    let run_log_path =
        lege_ipc::processing_log::run_log_path_for_output(&output_path.to_string_lossy());
    if let Some(parent) = run_log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Fresh file per run: a rerun to the same output replaces the stale log.
    let _ = std::fs::remove_file(&run_log_path);

    let spawn_line = format!(
        "task {gui_task_id}: spawning {} {}",
        cli_path.display(),
        cli_args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );
    append_worker_log(&spawn_line);
    append_run_log(&run_log_path, &spawn_line);

    let mut cmd = std::process::Command::new(&cli_path);
    configure_cli_command(&mut cmd, &cli_path);
    cmd.args(&cli_args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd.spawn().map_err(|e| {
        append_worker_log(&format!(
            "task {gui_task_id}: FAILED to launch lege worker ({}): {e}",
            cli_path.display()
        ));
        anyhow::anyhow!("Failed to launch lege worker ({:?}): {}", cli_path, e)
    })?;

    monitor_worker(child, gui_task_id, events_tx, log_tx, run_log_path)
}

/// stdout owns the protocol state; the process owner publishes its one final
/// outcome only after process exit and complete stdout/stderr drainage.
enum WorkerProtocolState {
    Streaming,
    Terminal(WorkerProgressUpdate),
}

fn monitor_worker(
    mut child: std::process::Child,
    task_id: u64,
    events: flume::Sender<WorkerProgressUpdate>,
    log_tx: Option<flume::Sender<String>>,
    run_log: PathBuf,
) -> Result<WorkerHandle> {
    let pid = child.id();
    let (stop_tx, stop_rx) = flume::bounded(1);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("worker stdout not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("worker stderr not piped"))?;
    let progress = events.clone();
    let stdout_thread = std::thread::spawn(move || -> std::io::Result<WorkerProtocolState> {
        let mut state = WorkerProtocolState::Streaming;
        for line in BufReader::new(stdout).lines() {
            let line = line?;
            if let Ok(update) = serde_json::from_str::<WorkerProgressUpdate>(&line) {
                if matches!(state, WorkerProtocolState::Streaming) {
                    match update {
                        WorkerProgressUpdate::Completed { .. }
                        | WorkerProgressUpdate::Error { .. } => {
                            state = WorkerProtocolState::Terminal(update.with_task_id(task_id))
                        }
                        update => {
                            let _ = progress.send(update.with_task_id(task_id));
                        }
                    }
                }
            }
        }
        Ok(state)
    });
    let stderr_run_log = run_log.clone();
    let stderr_thread = std::thread::spawn(move || {
        let mut tail = VecDeque::with_capacity(50);
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            append_worker_log(&format!("task {task_id} stderr: {line}"));
            append_run_log(&stderr_run_log, &format!("stderr: {line}"));
            if tail.len() == 50 {
                tail.pop_front();
            }
            tail.push_back(line.clone());
            if let Some(tx) = &log_tx {
                let _ = tx.send(line);
            }
        }
        tail
    });
    std::thread::spawn(move || {
        let exit = wait_for_child(&mut child, stop_rx, None);
        let protocol = stdout_thread
            .join()
            .map_err(|_| "worker stdout reader panicked".to_string())
            .and_then(|result| {
                result.map_err(|error| format!("worker stdout read failed: {error}"))
            });
        let tail = stderr_thread
            .join()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>()
            .join("\n");
        let outcome = match (exit, protocol) {
            (_, Ok(WorkerProtocolState::Terminal(error @ WorkerProgressUpdate::Error { .. }))) => {
                error
            }
            (Ok(status), Ok(WorkerProtocolState::Terminal(completed))) if status.success() => {
                completed
            }
            (exit, protocol) => {
                let mut error = match (exit, protocol) {
                    (Err(error), _) => format!("Failed to wait for worker process: {error}"),
                    (_, Err(error)) => error,
                    (Ok(status), _) => {
                        format!("Worker exited before successful completion with status: {status}")
                    }
                };
                if !tail.trim().is_empty() {
                    error.push_str("\n\nLast worker stderr:\n");
                    error.push_str(tail.trim());
                }
                WorkerProgressUpdate::Error {
                    task_id,
                    error,
                    metrics: None,
                }
            }
        };
        if let WorkerProgressUpdate::Error { error, .. } = &outcome {
            append_worker_log(&format!("task {task_id}: {error}"));
            append_run_log(&run_log, error);
        }
        let _ = events.send(outcome);
    });
    Ok(WorkerHandle::Process(WorkerStop {
        pid,
        sender: stop_tx,
    }))
}

/// The async future owns the stop sender. Cancelling that future disconnects
/// the process owner's receiver, including while the child is starting.
async fn probe_output(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (owner, stop) = flume::bounded(1);
    let result = tokio::task::spawn_blocking(move || {
        let mut child = command.spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("probe stdout not piped"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("probe stderr not piped"))?;
        let read_stdout = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let read_stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let status = wait_for_child(&mut child, stop, Some(std::time::Instant::now() + timeout));
        // Join both readers even on timeout or cancellation.
        let stdout = read_stdout
            .join()
            .map_err(|_| std::io::Error::other("probe stdout reader panicked"));
        let stderr = read_stderr
            .join()
            .map_err(|_| std::io::Error::other("probe stderr reader panicked"));
        Ok(std::process::Output {
            status: status?,
            stdout: stdout??,
            stderr: stderr??,
        })
    })
    .await
    .map_err(|error| std::io::Error::other(format!("probe worker panicked: {error}")))?;
    drop(owner);
    result
}

/// Probe a file using `lege --probe-json` and return the parsed JSON value.
/// Runs the subprocess on a blocking thread so the async executor is not touched.
pub async fn probe_file_json(path: &PathBuf) -> Result<serde_json::Value> {
    let cli_path = resolve_cli_path()?;
    let path = path.clone();
    let path_display = path.display().to_string();

    let mut cmd = std::process::Command::new(&cli_path);
    configure_cli_command(&mut cmd, &cli_path);
    cmd.arg("--probe-json").arg(&path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = probe_output(cmd, std::time::Duration::from_secs(30))
        .await
        .map_err(|error| {
            append_worker_log(&format!("probe failed for '{}': {error}", path.display()));
            error
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        // Probe failures are swallowed by callers (a file just shows no page
        // count), so record them here — a broken worker binary first shows up
        // as failing probes, well before Process is ever clicked.
        append_worker_log(&format!(
            "probe FAILED for '{}': status {} {}",
            path_display,
            output.status,
            stderr.trim()
        ));
        anyhow::bail!(
            "Probe failed for '{}': status {}{}{}",
            path_display,
            output.status,
            if stderr.trim().is_empty() { "" } else { "\n" },
            stderr.trim()
        );
    }
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        anyhow::anyhow!(
            "Probe returned invalid JSON for '{}': {}{}{}",
            path_display,
            e,
            if stderr.trim().is_empty() { "" } else { "\n" },
            stderr.trim()
        )
    })?;
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_probe_timeout_reaps_the_child() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exec sleep 60"]);
        let started = std::time::Instant::now();
        let error = probe_output(command, std::time::Duration::from_millis(40))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_dropping_a_probe_future_stops_its_child() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let stopped = directory.path().join("stopped");
        let mut command = std::process::Command::new("sh");
        command.args(["-c", r#"trap 'printf stopped > "$2"; exit 0' TERM; printf ready > "$1"; while :; do sleep 0.01; done"#, "probe-fixture"]);
        command.arg(&ready).arg(&stopped);
        let waiter = tokio::spawn(probe_output(command, std::time::Duration::from_secs(30)));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !stopped.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("dropping probe ownership must terminate the child");
    }

    #[cfg(unix)]
    fn monitor_fixture(
        script: &str,
        gate: Option<&Path>,
    ) -> (
        WorkerHandle,
        flume::Receiver<WorkerProgressUpdate>,
        tempfile::TempDir,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new("sh");
        command.args(["-c", script, "worker-fixture"]);
        if let Some(gate) = gate {
            command.arg(gate);
        }
        let child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let (sender, events) = flume::unbounded();
        let handle =
            monitor_worker(child, 41, sender, None, directory.path().join("run.log")).unwrap();
        (handle, events, directory)
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_completion_waits_for_process_exit_and_stream_drainage() {
        let directory = tempfile::tempdir().unwrap();
        let gate = directory.path().join("exit");
        let (mut handle, events, _logs) = monitor_fixture(
            r#"
            printf '%s\n' '{"type":"status","task_id":0,"status":{"kind":"initializing"},"metrics":null}'
            printf '%s\n' '{"type":"completed","task_id":0,"message":"done","metrics":null}'
            while [ ! -f "$1" ]; do sleep 0.01; done
        "#,
            Some(&gate),
        );
        assert!(matches!(
            events
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            WorkerProgressUpdate::Status { task_id: 41, .. }
        ));
        assert!(matches!(
            events.recv_timeout(std::time::Duration::from_millis(50)),
            Err(flume::RecvTimeoutError::Timeout)
        ));
        std::fs::write(&gate, []).unwrap();
        assert!(matches!(
            events
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            WorkerProgressUpdate::Completed { task_id: 41, .. }
        ));
        // Late cancellation has no OS target once the owner has reaped it.
        handle.kill();
        assert!(
            events
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_failed_exit_overrides_protocol_success_and_includes_final_stderr() {
        let (_handle, events, _logs) = monitor_fixture(
            r#"
            printf '%s\n' '{"type":"completed","task_id":0,"message":"done","metrics":null}'
            printf '%s\n' 'final diagnostic' >&2
            exit 7
        "#,
            None,
        );
        match events
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
        {
            WorkerProgressUpdate::Error { error, .. } => {
                assert!(error.contains('7'));
                assert!(error.contains("final diagnostic"));
            }
            update => panic!("unexpected outcome: {update:?}"),
        }
        assert!(
            events
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_exit_without_completion_and_cancelled_process_each_report_one_error() {
        let (_handle, events, _logs) = monitor_fixture("exit 0", None);
        assert!(matches!(
            events
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            WorkerProgressUpdate::Error { .. }
        ));
        let (mut handle, events, _logs) = monitor_fixture("exec sleep 60", None);
        handle.kill();
        assert!(matches!(
            events
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            WorkerProgressUpdate::Error { .. }
        ));
        assert!(
            events
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_err()
        );
    }
    use std::ffi::OsStr;

    #[test]
    fn preserve_normalizes_stale_preparation_flags_for_pdf_and_djvu() {
        for (format, name, expected) in [
            (OutputFormat::Pdf, "output.pdf", "jbig2"),
            (OutputFormat::Djvu, "output.djvu", "djvu"),
        ] {
            let mut options = ProcessingOptions::new();
            options.output_format = format;
            options.preserve_mode = true;
            options.grayscale_mode = true;
            options.image_processing_type = ImageProcessingType::Dithered;
            options.use_jbig2_halftone = true;
            options.jpeg_compat = true;
            options.symbol_mode = true;
            options.reflow = true;
            options.use_fixed_threshold = true;
            options.use_heavy_binarization = true;
            let args = gui_options_to_cli_args(
                &PathBuf::from("input.pdf"),
                &PathBuf::from(name),
                &options,
                true,
            );
            assert!(args.iter().any(|arg| arg == "--preserve"));
            assert_eq!(
                cli_arg_after(&args, "--text-format").as_deref(),
                Some(expected)
            );
            for flag in [
                "--grayscale",
                "--dither",
                "--halftone",
                "--jpeg-compat",
                "--sym-unify",
                "--reflow",
                "--binarization",
                "--threshold",
                "--sauvola-k",
            ] {
                assert!(
                    !args.iter().any(|arg| arg == flag),
                    "unexpected {flag}: {args:?}"
                );
            }
        }
    }

    #[test]
    fn preserve_keeps_explicit_truetyping_without_grayscale() {
        let mut options = ProcessingOptions::new();
        options.set_preserve_mode(true);
        options.set_text_encoder(CompressionType::Truetyping);
        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );
        assert_eq!(
            cli_arg_after(&args, "--text-format").as_deref(),
            Some("truetyping")
        );
        assert!(args.iter().any(|arg| arg == "--preserve"));
        assert!(!args.iter().any(|arg| arg == "--grayscale"));
    }

    fn cli_arg_after(args: &[OsString], flag: &str) -> Option<String> {
        args.windows(2)
            .find(|window| window[0] == OsStr::new(flag))
            .map(|window| window[1].to_string_lossy().to_string())
    }

    #[test]
    fn detects_macos_bundle_contents_from_worker_path() {
        let path = PathBuf::from("/Applications/Lege.app/Contents/MacOS/lege");
        assert_eq!(
            macos_bundle_contents_dir(&path),
            Some(PathBuf::from("/Applications/Lege.app/Contents"))
        );
        assert_eq!(macos_bundle_contents_dir(Path::new("/usr/bin/lege")), None);
    }

    #[test]
    fn new_options_show_adaptive_as_the_selected_default() {
        let options = ProcessingOptions::new();
        assert!(options.use_custom_adaptive);
        assert!(!options.use_fixed_threshold);
        assert!(!options.use_heavy_binarization);
        assert_eq!(options.k_factor, lege_ipc::DEFAULT_K_FACTOR);
    }

    #[test]
    fn both_ocr_modes_enable_automatic_toc_and_disabled_ocr_does_not() {
        for mode in [OcrMode::Fast, OcrMode::Thorough] {
            let mut options = ProcessingOptions::new();
            options.set_use_ocr(true);
            options.ocr_mode = mode;
            let args = gui_options_to_cli_args(
                &PathBuf::from("input.pdf"),
                &PathBuf::from("output.pdf"),
                &options,
                true,
            );

            assert!(args.iter().any(|arg| arg == OsStr::new("--auto-toc")));
            assert!(args.iter().any(|arg| arg == OsStr::new("--ocr-mode")));
            assert!(!args.iter().any(|arg| arg == OsStr::new("--no-ocr")));
        }

        let options = ProcessingOptions::new();
        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );
        assert!(!args.iter().any(|arg| arg == OsStr::new("--auto-toc")));
        assert!(args.iter().any(|arg| arg == OsStr::new("--no-ocr")));
    }

    #[test]
    fn no_layout_cli_ignores_hidden_image_output_format() {
        let mut options = ProcessingOptions::new();
        options.layout_analysis = false;
        options.compression_type = CompressionType::Ccitt4;
        options.image_processing_type = ImageProcessingType::Dithered;

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );

        assert_eq!(
            cli_arg_after(&args, "--text-format").as_deref(),
            Some("ccitt4")
        );
        assert!(args.iter().any(|arg| arg == OsStr::new("--no-layout")));
        assert!(!args.iter().any(|arg| arg == OsStr::new("--dither")));
    }

    #[test]
    fn dithered_images_no_longer_change_the_text_encoder() {
        let mut options = ProcessingOptions::new();
        options.layout_analysis = true;
        options.image_processing_type = ImageProcessingType::Dithered;

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );

        assert_eq!(
            cli_arg_after(&args, "--text-format").as_deref(),
            Some("jbig2")
        );
        assert!(!args.iter().any(|arg| arg == OsStr::new("--no-layout")));
        assert!(args.iter().any(|arg| arg == OsStr::new("--dither")));
    }

    #[test]
    fn the_default_pdf_run_asks_for_jbig2() {
        let options = ProcessingOptions::new();

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );

        assert_eq!(
            cli_arg_after(&args, "--text-format").as_deref(),
            Some("jbig2")
        );
        assert!(!args.iter().any(|arg| arg == OsStr::new("--jpeg-compat")));
        assert!(!args.iter().any(|arg| arg == OsStr::new("--no-symbol")));
    }

    #[test]
    fn reflow_cli_matrix_keeps_layout_enabled_for_pdf_and_djvu() {
        for (output_format, output_name, text_format) in [
            (OutputFormat::Pdf, "output.pdf", "ccitt4"),
            (OutputFormat::Djvu, "output.djvu", "djvu"),
        ] {
            let mut options = ProcessingOptions::new();
            options.output_format = output_format;
            options.set_reflow(true);

            let args = gui_options_to_cli_args(
                &PathBuf::from("input.pdf"),
                &PathBuf::from(output_name),
                &options,
                true,
            );

            assert_eq!(
                cli_arg_after(&args, "--text-format").as_deref(),
                Some(text_format)
            );
            assert!(args.iter().any(|arg| arg == OsStr::new("--reflow")));
            assert!(!args.iter().any(|arg| arg == OsStr::new("--no-layout")));
            assert!(!args.iter().any(|arg| arg == OsStr::new("--invert")));
            assert!(!args.iter().any(|arg| arg == OsStr::new("--center-margins")));
            assert!(!args.iter().any(|arg| arg == OsStr::new("--crop-margins")));
        }
    }

    #[test]
    fn worker_args_repair_stale_reflow_dependencies() {
        let mut options = ProcessingOptions::new();
        options.reflow = true;
        options.layout_analysis = false;
        options.invert_input = true;
        options.center_margins = true;

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );

        assert!(args.iter().any(|arg| arg == OsStr::new("--reflow")));
        assert!(!args.iter().any(|arg| arg == OsStr::new("--no-layout")));
        assert!(!args.iter().any(|arg| arg == OsStr::new("--invert")));
        assert!(!args.iter().any(|arg| arg == OsStr::new("--center-margins")));
    }

    #[test]
    fn layout_cli_uses_halftone_with_truetyping_or_jbig2_text() {
        for (encoder, text_format) in [
            (CompressionType::Truetyping, "truetyping"),
            (CompressionType::Jbig2, "jbig2"),
        ] {
            let mut options = ProcessingOptions::new();
            options.layout_analysis = true;
            options.set_text_encoder(encoder);
            options.set_jbig2_halftone_images();

            let args = gui_options_to_cli_args(
                &PathBuf::from("input.pdf"),
                &PathBuf::from("output.pdf"),
                &options,
                true,
            );

            assert_eq!(
                cli_arg_after(&args, "--text-format").as_deref(),
                Some(text_format)
            );
            assert!(args.iter().any(|arg| arg == OsStr::new("--halftone")));
            assert!(!args.iter().any(|arg| arg == OsStr::new("--dither")));
        }
    }

    #[test]
    fn hidden_halftone_setting_is_ignored_for_djvu() {
        let mut options = ProcessingOptions::new();
        options.output_format = OutputFormat::Djvu;
        options.layout_analysis = true;
        options.image_processing_type = ImageProcessingType::Dithered;
        options.use_jbig2_halftone = true;

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.djvu"),
            &options,
            true,
        );

        assert_eq!(
            cli_arg_after(&args, "--text-format").as_deref(),
            Some("djvu")
        );
        assert!(!args.iter().any(|arg| arg == OsStr::new("--halftone")));
        assert!(args.iter().any(|arg| arg == OsStr::new("--dither")));
    }

    #[test]
    fn djvu_forwards_enabled_cover_selection() {
        let mut options = ProcessingOptions::new();
        options.output_format = OutputFormat::Djvu;

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.djvu"),
            &options,
            true,
        );

        assert_eq!(
            cli_arg_after(&args, "--cover-format").as_deref(),
            Some("jpeg")
        );
        assert!(!args.iter().any(|arg| arg == OsStr::new("--no-cover")));
    }

    #[test]
    fn jpeg2000_cover_maps_to_jp2_for_pdf() {
        let mut options = ProcessingOptions::new();
        options.cover_image_type = CoverImageType::Jpeg2000;

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );

        assert_eq!(
            cli_arg_after(&args, "--cover-format").as_deref(),
            Some("jp2")
        );
    }

    #[test]
    fn disabled_cover_is_explicit_for_pdf_and_djvu() {
        for output_format in [OutputFormat::Pdf, OutputFormat::Djvu] {
            let mut options = ProcessingOptions::new();
            options.output_format = output_format;
            options.original_cover = false;

            let args = gui_options_to_cli_args(
                &PathBuf::from("input.pdf"),
                &PathBuf::from("output.bin"),
                &options,
                true,
            );

            assert_eq!(
                cli_arg_after(&args, "--cover-format").as_deref(),
                Some("none")
            );
            assert!(args.iter().any(|arg| arg == OsStr::new("--no-cover")));
        }
    }

    #[test]
    fn crop_margins_cli_uses_free_aspect_crop() {
        let mut options = ProcessingOptions::new();
        options.crop_margins = true;

        let args = gui_options_to_cli_args(
            &PathBuf::from("input.pdf"),
            &PathBuf::from("output.pdf"),
            &options,
            true,
        );

        assert!(args.iter().any(|arg| arg == OsStr::new("--crop-margins")));
        assert!(
            args.iter()
                .any(|arg| arg == OsStr::new("--crop-free-aspect"))
        );
    }

    #[test]
    fn target_resolution_is_forwarded_as_an_explicit_worker_flag() {
        for height in [1200, 4800] {
            let mut options = ProcessingOptions::new();
            options.target_height = Some(height);

            let args = gui_options_to_cli_args(
                &PathBuf::from("input.pdf"),
                &PathBuf::from("output.pdf"),
                &options,
                true,
            );
            let expected = height.to_string();

            assert_eq!(
                cli_arg_after(&args, "--target-height").as_deref(),
                Some(expected.as_str())
            );
        }
    }
}
