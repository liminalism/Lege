pub mod annotations;
pub mod check;
pub mod content;
pub mod images;
pub mod inspect;
pub mod map;
pub mod matching;
pub mod mcp;
pub mod poppler;
pub mod print;
pub mod render;
pub mod search;
pub mod serve;
pub mod tables;
pub mod text;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use pdf_content::PageCompiler;
use pdf_document::{DocumentSnapshot, ParseContext};
use pdf_render_api::AnnotationMode;

use crate::open::{self, DocumentIdentity};
use crate::schema::{Envelope, OutputMode};

/// Shared compile helper: annotations on by default (viewer parity).
///
/// The system font provider scans every platform font directory when it is
/// built, so one is kept for the process: each worker of a parallel run
/// builds its own compiler, and none of them should repeat that scan.
pub fn page_compiler(system_fonts: bool, annotations: bool) -> PageCompiler {
    static SYSTEM_FONTS: std::sync::OnceLock<Arc<pdf_font::FolderFontProvider>> =
        std::sync::OnceLock::new();
    let c = PageCompiler::new().with_annotations(annotations);
    if system_fonts {
        let provider =
            SYSTEM_FONTS.get_or_init(|| Arc::new(pdf_font::FolderFontProvider::system()));
        c.with_system_fonts(Arc::clone(provider) as Arc<dyn pdf_font::SystemFontProvider>)
    } else {
        c
    }
}

pub fn default_annotations() -> bool {
    matches!(AnnotationMode::default(), AnnotationMode::StaticAppearances)
}

/// Fresh parse context for a page job, bound to the armed time budget so a
/// page that is still compiling when the budget runs out stops there.
pub fn parse_context() -> ParseContext {
    let mut ctx = ParseContext::new();
    ctx.set_cancellation_flag(Some(crate::deadline::flag()));
    ctx
}

/// Render limits bound to the armed time budget.
pub fn render_limits() -> pdf_render_api::RenderLimits {
    pdf_render_api::RenderLimits {
        cancellation: Some(crate::deadline::render_token()),
        ..pdf_render_api::RenderLimits::default()
    }
}

/// If the time budget has run out, say so on the record stream and return
/// `true`; the caller stops and exits non-zero.
///
/// The report is a record of its own with status `truncated`, so a consumer
/// reading JSONL sees every finished page and then, explicitly, that the run
/// was cut short — never a stream that just ends.
pub fn timed_out(
    document: &str,
    done: usize,
    selected: usize,
    mode: OutputMode,
    multi: bool,
) -> Result<bool> {
    if !crate::deadline::expired() {
        return Ok(false);
    }
    let message = crate::deadline::message(done, selected);
    match mode {
        OutputMode::Human => eprintln!("error: {message}"),
        OutputMode::Json | OutputMode::Jsonl => {
            let env = Envelope::ok(
                document,
                serde_json::json!({
                    "error": message,
                    "pages_done": done,
                    "pages_selected": selected,
                }),
            )
            .set_status(crate::schema::Status::Truncated);
            if multi || matches!(mode, OutputMode::Jsonl) {
                env.write_jsonl()?;
            } else {
                env.write_json()?;
            }
        }
    }
    Ok(true)
}

/// How a multi-page command reports: where records go and what stops it.
#[derive(Debug, Clone, Copy)]
pub struct PageRun<'a> {
    pub document: &'a str,
    pub mode: OutputMode,
    /// More than one record may be written, so each must be one line.
    pub multi: bool,
    pub fail_fast: bool,
    /// Worker threads (0 = one per core). Pass 1 for work that must not
    /// overlap, such as OCR on a single shared engine.
    pub threads: u32,
}

/// Run `work` on every page of `pages`, in parallel, and `emit` each result
/// in page order.
///
/// This is the loop every multi-page command shares: per-worker state from
/// `init`, the time budget checked between pages, a failed page reported as a
/// record of its own without ending the run (unless `fail_fast`), and the
/// warnings about the page selection attached to the first record only.
/// Returns the process exit code.
pub fn run_pages<S, D: Send>(
    run: PageRun<'_>,
    pages: &[crate::pages::PageZero],
    selection_warnings: Vec<String>,
    init: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, crate::pages::PageZero, &mut Vec<String>) -> Result<D> + Sync,
    mut emit: impl FnMut(crate::pages::PageZero, D, Vec<String>) -> Result<()>,
) -> Result<i32> {
    use std::ops::ControlFlow;

    let selected = pages.len();
    let workers = crate::parallel::worker_count(run.threads, selected);
    let mut exit = 0i32;
    let mut failure: Option<anyhow::Error> = None;
    let mut selection_warnings = Some(selection_warnings);

    crate::parallel::ordered(
        pages,
        workers,
        init,
        |state, _, page| {
            let mut warnings = Vec::new();
            if crate::deadline::expired() {
                return (Err(anyhow::anyhow!("time budget exhausted")), warnings);
            }
            let result = work(state, *page, &mut warnings);
            (result, warnings)
        },
        |done, page, (result, page_warnings)| {
            let step = || -> Result<ControlFlow<()>> {
                // A page that came back after the budget ran out is
                // unfinished, not broken, whatever error it carries.
                let expired = crate::deadline::expired();
                if (expired && result.is_err())
                    && timed_out(run.document, done, selected, run.mode, run.multi)?
                {
                    exit = 1;
                    return Ok(ControlFlow::Break(()));
                }
                let mut warnings = selection_warnings.take().unwrap_or_default();
                warnings.extend(page_warnings);
                match result {
                    Ok(data) => emit(*page, data, warnings)?,
                    Err(err) => {
                        exit = 1;
                        if run.mode == OutputMode::Human {
                            for warning in &warnings {
                                eprintln!("warning: {warning}");
                            }
                        }
                        let env = Envelope::page_failed(
                            run.document,
                            page.one_based(),
                            format!("{err:#}"),
                        )
                        .with_warnings(warnings);
                        emit_failed(&env, run.mode, run.multi)?;
                        if run.fail_fast {
                            return Ok(ControlFlow::Break(()));
                        }
                    }
                }
                Ok(ControlFlow::Continue(()))
            };
            match step() {
                Ok(flow) => flow,
                Err(err) => {
                    failure = Some(err);
                    ControlFlow::Break(())
                }
            }
        },
    );

    match failure {
        Some(err) => Err(err),
        None => Ok(exit),
    }
}

/// How one page of a parallel run ended.
#[derive(Debug)]
pub enum PageOutcome<D> {
    Done(D),
    Failed(anyhow::Error),
    /// The time budget ran out before or while this page was worked on.
    TimedOut,
}

/// Run `work` on every page of `pages`, in parallel, and hand each outcome
/// to `deliver` in page order, with the page's warnings. `deliver` returns
/// `Break` to stop the run.
///
/// The loop under [`run_pages`], for callers that build their own result:
/// the services, and commands that report only some pages.
pub fn for_pages<S, D: Send>(
    pages: &[crate::pages::PageZero],
    threads: u32,
    init: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, crate::pages::PageZero, &mut Vec<String>) -> Result<D> + Sync,
    mut deliver: impl FnMut(
        usize,
        crate::pages::PageZero,
        PageOutcome<D>,
        Vec<String>,
    ) -> std::ops::ControlFlow<()>,
) {
    let workers = crate::parallel::worker_count(threads, pages.len());
    crate::parallel::ordered(
        pages,
        workers,
        init,
        |state, _, page| {
            let mut warnings = Vec::new();
            if crate::deadline::expired() {
                return (PageOutcome::TimedOut, warnings);
            }
            let outcome = match work(state, *page, &mut warnings) {
                Ok(data) => PageOutcome::Done(data),
                // A page cut off by the time budget is unfinished, not
                // broken, whatever error it carries.
                Err(_) if crate::deadline::expired() => PageOutcome::TimedOut,
                Err(error) => PageOutcome::Failed(error),
            };
            (outcome, warnings)
        },
        |index, page, (outcome, warnings)| deliver(index, *page, outcome, warnings),
    );
}

pub fn snapshot_arc(snapshot: DocumentSnapshot) -> Arc<DocumentSnapshot> {
    Arc::new(snapshot)
}

/// Take the caller-supplied snapshot when there is one (the `serve`/`mcp`
/// cache hands one over), else open `path` from disk.
///
/// A cached snapshot arrives without a stat, so the placeholder identity
/// carries only the path: `len`/`modified` exist to invalidate the cache, and
/// the cache has already made that decision by the time we are called.
pub fn resolve_snapshot(
    path: &Path,
    password: Option<&str>,
    snapshot: Option<Arc<DocumentSnapshot>>,
    identity: Option<DocumentIdentity>,
) -> Result<(DocumentIdentity, Arc<DocumentSnapshot>)> {
    if let Some(snapshot) = snapshot {
        let identity = identity.unwrap_or_else(|| DocumentIdentity {
            path: path.to_path_buf(),
            len: 0,
            modified: None,
        });
        return Ok((identity, snapshot));
    }
    let (identity, snap) = open::open_document(path, password)?;
    Ok((identity, Arc::new(snap)))
}

/// Emit one page-local failure record.
///
/// `multi` selects the pretty single-envelope form over JSONL, matching
/// `emit`: a run that will produce more than one record must stay
/// newline-delimited so a partial stream is still parseable.
pub fn emit_failed(env: &Envelope, mode: OutputMode, multi: bool) -> Result<()> {
    match mode {
        OutputMode::Human => {
            eprintln!(
                "error: page {}: {}",
                env.page.unwrap_or(0),
                env.data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("failed")
            );
            Ok(())
        }
        OutputMode::Json if !multi => env.write_json(),
        OutputMode::Json | OutputMode::Jsonl => env.write_jsonl(),
    }
}
