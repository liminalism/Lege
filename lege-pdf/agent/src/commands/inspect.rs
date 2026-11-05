use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_read::{CompileStatus, DocumentReport, OpenOutcome};
use pdf_source::PdfSource;

use crate::bounds::Bounds;
use crate::open::{self, DocumentIdentity};
use crate::pages::{PageZero, parse_page_range};
use crate::schema::{Envelope, OutputMode, Status};
use crate::views::report::{
    AnnotationView, CompileView, EncryptionView, FeaturesView, InspectData, MetadataView, OpenView,
    OutlineItemView, PageLabelView, PageSizeView, PageView, TextSampleView, XrefView,
};

#[derive(Debug)]
pub struct InspectArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    /// Outline items listed (0 = all).
    pub outline_limit: usize,
    pub bounds: Bounds,
    pub output: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

pub fn run(args: InspectArgs<'_>) -> Result<i32> {
    let (identity, source, snapshot) =
        prepare(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    let mut warnings = Vec::new();

    // One parse: the snapshot opened here is the one examined below.
    let opened = match snapshot {
        Some(snapshot) => Ok(snapshot),
        None => open::open_from_source(&identity, Arc::clone(&source), args.password).map(Arc::new),
    };
    let snapshot = match opened {
        Ok(snapshot) => snapshot,
        Err(err) => {
            // A document that will not open still has a story to tell: the
            // structural layer alone yields xref health and the declared
            // encryption, which is what says "this needs a password".
            let report = pdf_read::examine_with_password(
                Arc::clone(&source),
                DocumentLimits::default(),
                args.password,
            );
            if !matches!(report.open, OpenOutcome::Failed { .. }) {
                return Err(err);
            }
            let data = build_data(None, &report, &[], 0, &args.bounds)?;
            let env = Envelope::ok(&document, serde_json::to_value(&data)?)
                .set_status(Status::Failed)
                .with_warnings(warnings);
            emit(&env, &data, args.output)?;
            return Ok(1);
        }
    };

    let page_indices = detail_pages(args.pages, &snapshot, &args.bounds, &mut warnings)?;
    let report = examine_selected(&snapshot, &page_indices);
    let data = build_data(
        Some(&snapshot),
        &report,
        &page_indices,
        args.outline_limit,
        &args.bounds,
    )?;
    let env = Envelope::ok(&document, serde_json::to_value(&data)?).with_warnings(warnings.clone());
    emit(&env, &data, args.output)?;
    Ok(0)
}

/// The report for an already-open document, for the services.
pub fn inspect_data(
    snapshot: &DocumentSnapshot,
    pages: Option<&str>,
    outline_limit: usize,
    bounds: Bounds,
) -> Result<(InspectData, Vec<String>)> {
    // The cache handed us an already-open snapshot: examine that, without
    // reading or parsing the file again on every `pdf_inspect` call.
    let mut warnings = Vec::new();
    let page_indices = detail_pages(pages, snapshot, &bounds, &mut warnings)?;
    let report = examine_selected(snapshot, &page_indices);
    let data = build_data(
        Some(snapshot),
        &report,
        &page_indices,
        outline_limit,
        &bounds,
    )?;
    Ok((data, warnings))
}

/// The pages to report in detail: the ones asked for, and none otherwise.
///
/// A page's boxes and compile status are a question about that page. The
/// document summary — what `inspect` is usually run for — should not wait
/// for fifty pages to compile first.
fn detail_pages(
    pages: Option<&str>,
    snapshot: &DocumentSnapshot,
    bounds: &Bounds,
    warnings: &mut Vec<String>,
) -> Result<Vec<PageZero>> {
    if pages.is_none() {
        return Ok(Vec::new());
    }
    let (page_indices, range_warnings) =
        parse_page_range(pages, snapshot.page_count(), bounds.max_pages)?;
    warnings.extend(range_warnings);
    Ok(page_indices)
}

/// The report for the pages `inspect` is about to show, and no others.
///
/// A page's compile status means compiling the page, which dominates the cost
/// of a report: examining all 650 pages of a book to describe its first one
/// took twenty times as long as the rest of the command.
fn examine_selected(snapshot: &DocumentSnapshot, pages: &[PageZero]) -> DocumentReport {
    let indices: Vec<u32> = pages.iter().map(|page| page.0).collect();
    pdf_read::examine_snapshot(snapshot, Some(&indices))
}

/// A document ready to be examined: what it is, its bytes, and its snapshot
/// when the caller already had one open.
type Prepared = (
    DocumentIdentity,
    Arc<dyn PdfSource>,
    Option<Arc<DocumentSnapshot>>,
);

fn prepare(
    path: &Path,
    password: Option<&str>,
    snapshot: Option<Arc<DocumentSnapshot>>,
    identity: Option<DocumentIdentity>,
) -> Result<Prepared> {
    if let Some(snapshot) = snapshot {
        let identity = identity.unwrap_or_else(|| DocumentIdentity {
            path: path.to_path_buf(),
            len: 0,
            modified: None,
        });
        // Reuse the already-open snapshot's source instead of re-reading the
        // file from disk (`source()` is infallible, so no fallback is
        // needed).
        let source: Arc<dyn PdfSource> = Arc::clone(snapshot.source());
        return Ok((identity, source, Some(snapshot)));
    }
    let (identity, source) = open::load_source(path)?;
    let _ = password;
    Ok((identity, source, None))
}

fn build_data(
    snapshot: Option<&DocumentSnapshot>,
    report: &DocumentReport,
    page_indices: &[PageZero],
    outline_limit: usize,
    bounds: &Bounds,
) -> Result<InspectData> {
    let open = match &report.open {
        OpenOutcome::Ok => OpenView::Ok,
        OpenOutcome::Recovered { how } => OpenView::Recovered {
            repairs: how.clone(),
        },
        OpenOutcome::Failed { error } => OpenView::Failed {
            error: error.clone(),
        },
    };

    let info = |key: &str| report.identity.info.get(key).cloned();
    let metadata = MetadataView {
        title: info("Title"),
        author: info("Author"),
        subject: info("Subject"),
        keywords: info("Keywords"),
        creator: info("Creator"),
        producer: info("Producer"),
        creation_date: info("CreationDate"),
        modification_date: info("ModDate"),
    };
    let mut summary = Summary::default();
    if let Some(snapshot) = snapshot {
        summary = summarize(snapshot, outline_limit, bounds);
    }

    let (page_count, version, pages) = if let Some(snapshot) = snapshot {
        let version = {
            let v = snapshot.structure().version;
            Some(format!("{}.{}", v.major, v.minor))
        };
        let status_by: std::collections::HashMap<u32, &pdf_read::PageStatus> =
            report.pages.iter().map(|p| (p.index, p)).collect();
        let mut pages = Vec::new();
        for pz in page_indices {
            let pref = snapshot.page(PageIndex(pz.0))?;
            let status = status_by.get(&pz.0).copied();
            let compile = match status.map(|s| &s.compile) {
                Some(CompileStatus::Ok { op_count }) => CompileView::Ok {
                    op_count: *op_count,
                },
                Some(CompileStatus::Degraded { op_count, detail }) => CompileView::Degraded {
                    op_count: *op_count,
                    detail: detail.clone(),
                },
                Some(CompileStatus::Failed { error }) => CompileView::Failed {
                    error: error.clone(),
                },
                None => CompileView::Unknown,
            };
            pages.push(PageView {
                page: pz.one_based(),
                page_index: pz.0,
                media_box: pref.media_box,
                crop_box: pref.crop_box,
                bleed_box: pref.bleed_box,
                trim_box: pref.trim_box,
                art_box: pref.art_box,
                rotate: pref.rotate,
                media_box_present: status.map(|s| s.media_box_present).unwrap_or(true),
                compile,
            });
        }
        (snapshot.page_count(), version, pages)
    } else {
        (0, None, Vec::new())
    };

    Ok(InspectData {
        open,
        page_count,
        version,
        encryption: report.encryption.as_ref().map(|e| EncryptionView {
            version: e.version,
            revision: e.revision,
            method: e.method.clone(),
            user_password_empty: e.user_password_empty,
        }),
        xref: XrefView {
            recovery_used: report.xref.recovery_used,
            rebuilt: report.xref.rebuilt,
            revision_count: report.xref.revision_count,
        },
        features: FeaturesView {
            has_acroform: report.features.has_acroform,
            has_xfa: report.features.has_xfa,
            has_javascript: report.features.has_javascript,
            has_outlines: report.features.has_outlines,
            has_optional_content: report.features.has_optional_content,
            uses_object_streams: report.features.uses_object_streams,
        },
        annotations: AnnotationView {
            total: report.annotations.total(),
            by_subtype: report
                .annotations
                .count_per_subtype
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        },
        outline_item_count: summary.outline_item_count,
        outline: summary.outline,
        outline_truncated: summary.outline_truncated,
        metadata,
        page_sizes: summary.page_sizes,
        page_labels: summary.page_labels,
        text_sample: summary.text_sample,
        pages,
    })
}

/// The parts of the report that describe the whole document and need more
/// than the examiner's facts: outline, page sizes, page labels and a look at
/// what a few pages contain.
#[derive(Default)]
struct Summary {
    outline_item_count: usize,
    outline: Vec<OutlineItemView>,
    outline_truncated: bool,
    page_sizes: Vec<PageSizeView>,
    page_labels: Vec<PageLabelView>,
    text_sample: Option<TextSampleView>,
}

/// Distinct page sizes listed.
const MAX_PAGE_SIZES: usize = 8;
/// Label runs listed.
const MAX_LABEL_RUNS: usize = 24;
/// Pages looked at for the text sample.
const SAMPLE_PAGES: u32 = 5;

fn summarize(snapshot: &DocumentSnapshot, outline_limit: usize, bounds: &Bounds) -> Summary {
    let page_count = snapshot.page_count();
    let mut ctx = ParseContext::new();
    let outline = snapshot.outline(&mut ctx);
    let shown = match outline_limit {
        0 => outline.items.len(),
        limit => outline.items.len().min(limit),
    };

    // Page sizes, most common first.
    let mut sizes: Vec<((u64, u64), Vec<u32>)> = Vec::new();
    for index in 0..page_count {
        let (width, height) = crate::commands::map::page_size(snapshot, PageZero(index));
        let key = (width.round() as u64, height.round() as u64);
        match sizes.iter_mut().find(|(size, _)| *size == key) {
            Some((_, pages)) => pages.push(index + 1),
            None => sizes.push((key, vec![index + 1])),
        }
    }
    sizes.sort_by_key(|entry| std::cmp::Reverse(entry.1.len()));
    let more_sizes = sizes.len().saturating_sub(MAX_PAGE_SIZES);
    let mut page_sizes: Vec<PageSizeView> = sizes
        .iter()
        .take(MAX_PAGE_SIZES)
        .map(|((width, height), pages)| PageSizeView {
            width: *width as f64,
            height: *height as f64,
            count: pages.len() as u32,
            pages: page_ranges(pages),
        })
        .collect();
    if more_sizes > 0 {
        let rest: Vec<u32> = {
            let mut rest: Vec<u32> = sizes[MAX_PAGE_SIZES..]
                .iter()
                .flat_map(|(_, pages)| pages.iter().copied())
                .collect();
            rest.sort_unstable();
            rest
        };
        // A scan with every page cropped a little differently: say so in
        // one entry rather than hundreds.
        page_sizes.push(PageSizeView {
            width: 0.0,
            height: 0.0,
            count: rest.len() as u32,
            pages: format!("{more_sizes} other sizes: {}", page_ranges(&rest)),
        });
    }

    // Label runs, where the printed number is not the position.
    let labels = crate::objects::page_labels(snapshot);
    let mut page_labels = Vec::new();
    for (at, range) in labels.iter().enumerate() {
        let end = labels
            .get(at + 1)
            .map_or(page_count, |next| next.start.min(page_count));
        if range.start >= end {
            continue;
        }
        let label = |index: u32| crate::objects::label_for(&labels, index).unwrap_or_default();
        let (first, last) = (label(range.start), label(end - 1));
        let unnumbered = first.is_empty() && last.is_empty();
        if unnumbered || (first == (range.start + 1).to_string() && last == end.to_string()) {
            continue;
        }
        if page_labels.len() == MAX_LABEL_RUNS {
            break;
        }
        page_labels.push(PageLabelView {
            pages: if end - range.start == 1 {
                (range.start + 1).to_string()
            } else {
                format!("{}-{end}", range.start + 1)
            },
            first,
            last,
        });
    }

    Summary {
        outline_item_count: outline.items.len(),
        outline: outline.items[..shown]
            .iter()
            .map(|item| OutlineItemView {
                title: item.title.to_string(),
                page: item
                    .destination
                    .map(|destination| destination.page.0 + 1)
                    .filter(|page| *page <= page_count),
                depth: item.depth,
            })
            .collect(),
        outline_truncated: shown < outline.items.len(),
        page_sizes,
        page_labels,
        text_sample: text_sample(snapshot, bounds),
    }
}

/// Look at up to [`SAMPLE_PAGES`] pages spread evenly through the document.
fn text_sample(snapshot: &DocumentSnapshot, bounds: &Bounds) -> Option<TextSampleView> {
    use crate::commands::text::{self, OcrMode, TextLayout};

    let page_count = snapshot.page_count();
    if page_count == 0 {
        return None;
    }
    let wanted = SAMPLE_PAGES.min(page_count);
    let mut picks: Vec<PageZero> = (0..wanted)
        .map(|step| {
            if wanted == 1 {
                PageZero(0)
            } else {
                PageZero(
                    (u64::from(step) * u64::from(page_count - 1) / u64::from(wanted - 1)) as u32,
                )
            }
        })
        .collect();
    picks.dedup();

    let mut sample = TextSampleView {
        pages: Vec::new(),
        kinds: std::collections::BTreeMap::new(),
    };
    crate::commands::for_pages(
        &picks,
        bounds.threads,
        || text::PageState::new(false),
        |state, page, warnings| {
            let data = text::extract_page(
                snapshot,
                &state.compiler,
                page,
                &pdf_text::TextPageOptions::default(),
                TextLayout::Plain,
                None,
                OcrMode::Never,
                "",
                bounds,
                warnings,
                &mut state.ocr_backend,
                &mut state.glyphs,
            )?;
            Ok(data.provenance.page_content_kind)
        },
        |_, page, outcome, _| {
            if let crate::commands::PageOutcome::Done(kind) = outcome {
                sample.pages.push(page.one_based());
                *sample.kinds.entry(kind).or_default() += 1;
            }
            std::ops::ControlFlow::Continue(())
        },
    );
    (!sample.pages.is_empty()).then_some(sample)
}

/// One-based pages as a page-range string: `1-12,15,20-22`.
fn page_ranges(pages: &[u32]) -> String {
    /// Runs written out before the rest is summarised.
    const MAX_RUNS: usize = 12;
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &page in pages {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == page => *end = page,
            _ => runs.push((page, page)),
        }
    }
    let mut out: Vec<String> = runs
        .iter()
        .take(MAX_RUNS)
        .map(|(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}-{end}")
            }
        })
        .collect();
    if runs.len() > MAX_RUNS {
        out.push(format!("… ({} more runs)", runs.len() - MAX_RUNS));
    }
    out.join(",")
}

fn emit(env: &Envelope, data: &InspectData, mode: OutputMode) -> Result<()> {
    match mode {
        OutputMode::Human => {
            print_human(env, data);
            Ok(())
        }
        OutputMode::Json => env.write_json(),
        OutputMode::Jsonl => env.write_jsonl(),
    }
}

fn print_human(env: &Envelope, data: &InspectData) {
    println!("document: {}", env.document);
    for line in lines(data) {
        println!("{line}");
    }
    for w in &env.warnings {
        eprintln!("warning: {w}");
    }
}

/// The report as the lines a person or a model reads top to bottom.
pub(crate) fn lines(data: &InspectData) -> Vec<String> {
    let mut out = Vec::new();
    match &data.open {
        OpenView::Ok => out.push("open: ok".to_string()),
        OpenView::Recovered { repairs } => {
            out.push(format!("open: recovered ({} repair(s))", repairs.len()));
            for r in repairs {
                out.push(format!("  repair: {r}"));
            }
        }
        OpenView::Failed { error } => out.push(format!("open: failed: {error}")),
    }
    out.push(format!("pages: {}", data.page_count));
    if let Some(v) = &data.version {
        out.push(format!("version: {v}"));
    }
    for (label, value) in [
        ("title", &data.metadata.title),
        ("author", &data.metadata.author),
        ("subject", &data.metadata.subject),
        ("keywords", &data.metadata.keywords),
        ("creator", &data.metadata.creator),
        ("producer", &data.metadata.producer),
        ("created", &data.metadata.creation_date),
        ("modified", &data.metadata.modification_date),
    ] {
        if let Some(value) = value {
            out.push(format!("{label}: {value}"));
        }
    }
    for size in &data.page_sizes {
        if size.width == 0.0 {
            out.push(format!("page size: {}", size.pages));
        } else {
            out.push(format!(
                "page size: {}x{} pt ({} page{}: {})",
                size.width,
                size.height,
                size.count,
                if size.count == 1 { "" } else { "s" },
                size.pages
            ));
        }
    }
    for run in &data.page_labels {
        out.push(format!(
            "page labels: pages {} are numbered {}..{}",
            run.pages, run.first, run.last
        ));
    }
    if let Some(sample) = &data.text_sample {
        let kinds: Vec<String> = sample
            .kinds
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        out.push(format!(
            "text sample: pages {:?}: {}",
            sample.pages,
            kinds.join(", ")
        ));
    }
    if let Some(enc) = &data.encryption {
        out.push(format!(
            "encryption: {} (V={} R={}) empty_user={}",
            enc.method, enc.version, enc.revision, enc.user_password_empty
        ));
    }
    out.push(format!(
        "features: acroform={} xfa={} js={} outlines={} ocg={} objstm={}",
        data.features.has_acroform,
        data.features.has_xfa,
        data.features.has_javascript,
        data.features.has_outlines,
        data.features.has_optional_content,
        data.features.uses_object_streams
    ));
    out.push(format!("outline_items: {}", data.outline_item_count));
    for item in &data.outline {
        let page = item
            .page
            .map_or_else(String::new, |page| format!(" (p.{page})"));
        out.push(format!(
            "  {}{}{page}",
            "  ".repeat(usize::from(item.depth)),
            item.title
        ));
    }
    if data.outline_truncated {
        out.push(format!(
            "  … {} more (--outline-limit 0 for all)",
            data.outline_item_count - data.outline.len()
        ));
    }
    out.push(format!("annotations: {}", data.annotations.total));
    out.push(format!(
        "xref: recovery={} rebuilt={} revisions={}",
        data.xref.recovery_used, data.xref.rebuilt, data.xref.revision_count
    ));
    for page in &data.pages {
        let compile = match &page.compile {
            CompileView::Ok { op_count } => format!("ok ops={op_count}"),
            CompileView::Degraded { op_count, detail } => {
                format!("degraded ops={op_count} ({detail})")
            }
            CompileView::Failed { error } => format!("failed ({error})"),
            CompileView::Unknown => "unknown".into(),
        };
        // Bleed/Trim/Art default to the CropBox, so only mention the ones a
        // document actually declares — otherwise every line grows by three
        // identical rectangles.
        let mut boxes = String::new();
        for (label, rect) in [
            ("bleed", page.bleed_box),
            ("trim", page.trim_box),
            ("art", page.art_box),
        ] {
            if rect != page.crop_box {
                boxes.push_str(&format!(" {label}={rect:?}"));
            }
        }
        out.push(format!(
            "page {} index {} rotate {} crop={:?}{boxes} compile={compile}",
            page.page, page.page_index, page.rotate, page.crop_box
        ));
    }
    out
}
