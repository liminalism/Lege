use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};
use pdf_document::{DocumentSnapshot, PageIndex};
use pdf_page_ir::{Point, Rect};
use pdf_render_cpu::{CpuBackend, CpuBackendOptions};
use pdf_text::{CharInfo, CharType, GlyphCache, TextPage, TextPageOptions};

use crate::bounds::Bounds;
use crate::commands::{
    PageRun, default_annotations, page_compiler, parse_context, resolve_snapshot, run_pages,
};
use crate::open::DocumentIdentity;
use crate::pages::{PageZero, parse_bbox, parse_page_range};
use crate::schema::{Envelope, OutputMode, Status};
use crate::views::text::{BlockView, CharView, LineView, TextPageData, TextProvenance, WordView};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TextLayout {
    /// Text in reading order.
    Plain,
    /// Text laid out on a character grid the way it sits on the page, so
    /// columns and tables keep their alignment (like `pdftotext -layout`).
    Physical,
    /// Paragraph-like blocks in reading order, with geometry.
    Blocks,
    Words,
    Chars,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OcrMode {
    /// Read only native PDF text operators.
    Never,
    /// OCR pages whose native text fails conservative trust checks.
    Auto,
    /// Render and OCR every selected page, ignoring native text for output.
    Always,
}

#[derive(Debug)]
pub struct TextArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    pub layout: TextLayout,
    pub bbox: Option<&'a str>,
    pub rtl: bool,
    pub normalize: bool,
    pub include_hidden: bool,
    pub include_annotations: bool,
    pub ocr: OcrMode,
    pub ocr_language: &'a str,
    pub system_fonts: bool,
    pub bounds: Bounds,
    pub fail_fast: bool,
    pub output: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

pub fn run(args: TextArgs<'_>) -> Result<i32> {
    let (identity, snapshot) =
        resolve_snapshot(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    let mut global_warnings = Vec::new();
    let (page_indices, range_warnings) =
        parse_page_range(args.pages, snapshot.page_count(), args.bounds.max_pages)?;
    global_warnings.extend(range_warnings);

    let bbox = args.bbox.map(parse_bbox).transpose()?;
    let options = TextPageOptions {
        rtl: args.rtl,
        normalize: args.normalize,
        include_hidden: args.include_hidden,
        include_annotations: args.include_annotations,
        include_soft_masks: false,
    };

    let multi = page_indices.len() > 1 || matches!(args.output, OutputMode::Jsonl);
    let run = PageRun {
        document: &document,
        mode: args.output,
        multi,
        fail_fast: args.fail_fast,
        // OCR goes through one engine; its pages are not run side by side.
        threads: if args.ocr == OcrMode::Never {
            args.bounds.threads
        } else {
            1
        },
    };
    let system_fonts = args.system_fonts;
    run_pages(
        run,
        &page_indices,
        global_warnings,
        // Per worker: a compiler, an OCR backend built on first need, and a
        // glyph cache. Fonts repeat on every page, so their glyph extents are
        // worked out once per worker rather than once per page.
        || PageState::new(system_fonts),
        |state, pz, warnings| {
            let mut data = extract_page(
                &snapshot,
                &state.compiler,
                pz,
                &options,
                args.layout,
                bbox,
                args.ocr,
                args.ocr_language,
                &args.bounds,
                warnings,
                &mut state.ocr_backend,
                &mut state.glyphs,
            )?;
            // Enforce the byte budget on the record itself, so what is
            // written is what was measured.
            let cut = fit_to_budget(&mut data, args.bounds.max_bytes)?;
            let truncated = cut.is_some();
            warnings.extend(cut);
            Ok((data, truncated))
        },
        |pz, (data, truncated), warnings| {
            let mut env =
                Envelope::page_ok(&document, pz.one_based(), serde_json::to_value(&data)?)
                    .with_warnings(warnings);
            if truncated {
                env = env.set_status(Status::Truncated);
            }
            let pretty = matches!(args.output, OutputMode::Json) && !multi;
            let rendered = render_envelope(&env, pretty)?;
            emit_page(&env, &data, args.output, multi, &rendered)
        },
    )
}

/// What one worker keeps between pages.
pub(crate) struct PageState {
    pub compiler: pdf_content::PageCompiler,
    pub ocr_backend: Option<CpuBackend>,
    pub glyphs: GlyphCache,
}

impl PageState {
    pub fn new(system_fonts: bool) -> Self {
        Self {
            compiler: page_compiler(system_fonts, default_annotations()),
            ocr_backend: None,
            glyphs: GlyphCache::new(),
        }
    }
}

fn render_envelope(env: &Envelope, pretty: bool) -> Result<String> {
    Ok(if pretty {
        serde_json::to_string_pretty(env)?
    } else {
        serde_json::to_string(env)?
    })
}

/// What a service text request asks for.
#[derive(Debug, Clone, Copy)]
pub struct TextRequest<'a> {
    pub layout: TextLayout,
    pub bbox: Option<[f64; 4]>,
    pub options: TextPageOptions,
    pub ocr: OcrMode,
    pub ocr_language: &'a str,
    pub bounds: Bounds,
    /// Budget for the whole response, in characters of page text (for the
    /// geometry layouts, of serialized record). 0 = none.
    pub max_chars: usize,
    /// Characters of the first selected page to skip: where an earlier
    /// response that stopped inside that page left off.
    pub start_char: usize,
}

/// Extract a page selection, in parallel, into one value that says exactly
/// how far it got.
///
/// The result always carries `page_count`, `pages_selected`, `truncated` and
/// `warnings`. When a limit stops the run, `truncated` is true, `next_page`
/// names the first page that was not returned in full (with `next_char` when
/// the stop fell inside it) and a warning says which limit it was — a caller
/// never has to infer coverage from the number of records that came back. A
/// page that fails is reported in place with an `error` and does not end the
/// run.
pub fn text_pages(
    snapshot: &DocumentSnapshot,
    pages: Option<&str>,
    request: &TextRequest<'_>,
) -> Result<serde_json::Value> {
    use std::ops::ControlFlow;

    use crate::commands::{PageOutcome, for_pages};

    let bounds = request.bounds;
    // Limits are applied here, where their effect can be reported, rather
    // than inside the range parser.
    let (selection, mut warnings) = parse_page_range(pages, snapshot.page_count(), 0)?;
    let selected = selection.len();
    let limit = match bounds.max_pages {
        0 => selected,
        max => selected.min(max as usize),
    };
    let text_only = matches!(request.layout, TextLayout::Plain | TextLayout::Physical);
    if request.start_char > 0 && !text_only {
        bail!("start_char applies to the plain and physical layouts only");
    }

    let mut out: Vec<serde_json::Value> = Vec::new();
    let (mut used_bytes, mut used_chars) = (0u64, 0usize);
    let mut stopped: Option<(usize, String)> = None;
    let mut next_char: Option<usize> = None;
    let mut failure: Option<anyhow::Error> = None;

    for_pages(
        &selection[..limit],
        if request.ocr == OcrMode::Never {
            bounds.threads
        } else {
            1
        },
        || PageState::new(false),
        |state, page, warnings| {
            let mut data = extract_page(
                snapshot,
                &state.compiler,
                page,
                &request.options,
                request.layout,
                request.bbox,
                request.ocr,
                request.ocr_language,
                &bounds,
                warnings,
                &mut state.ocr_backend,
                &mut state.glyphs,
            )?;
            warnings.extend(fit_to_budget(&mut data, bounds.max_bytes)?);
            Ok(data)
        },
        |index, page, outcome, page_warnings| {
            let mut entry = serde_json::json!({
                "page": page.one_based(),
                "page_index": page.0,
            });
            let step = || -> Result<ControlFlow<()>> {
                let mut flow = ControlFlow::Continue(());
                match outcome {
                    PageOutcome::TimedOut => {
                        stopped = Some((index, crate::deadline::message(index, selected)));
                        return Ok(ControlFlow::Break(()));
                    }
                    PageOutcome::Failed(error) => {
                        entry["error"] = serde_json::json!(format!("{error:#}"));
                    }
                    PageOutcome::Done(mut data) => {
                        let mut offset = 0;
                        if index == 0 && request.start_char > 0 {
                            offset = request.start_char;
                            if let Some(text) = &mut data.text {
                                *text = text.chars().skip(offset).collect();
                            }
                            entry["start_char"] = serde_json::json!(offset);
                        }
                        let cost = if text_only {
                            data.text.as_ref().map_or(0, |text| text.chars().count())
                        } else {
                            serde_json::to_vec(&data)?.len()
                        };
                        if request.max_chars > 0 && used_chars + cost > request.max_chars {
                            let reason =
                                format!("character budget max_chars={} reached", request.max_chars);
                            if !out.is_empty() {
                                stopped = Some((index, reason));
                                return Ok(ControlFlow::Break(()));
                            }
                            // The first page alone is over budget: return
                            // what fits of it and say where it was cut.
                            if text_only {
                                if let Some(text) = &mut data.text {
                                    let kept = cut_at_line(text, request.max_chars);
                                    next_char = Some(offset + kept);
                                }
                                stopped = Some((index, reason));
                                flow = ControlFlow::Break(());
                            }
                        }
                        used_chars += cost;
                        entry["data"] = serde_json::to_value(&data)?;
                    }
                }
                if !page_warnings.is_empty() {
                    entry["warnings"] = serde_json::json!(page_warnings);
                }
                let size = serde_json::to_vec(&entry)?.len() as u64;
                // The first page is always returned; it was already cut to fit.
                if bounds.max_bytes > 0 && !out.is_empty() && used_bytes + size > bounds.max_bytes {
                    stopped = Some((
                        index,
                        format!("byte budget max-bytes={} reached", bounds.max_bytes),
                    ));
                    return Ok(ControlFlow::Break(()));
                }
                used_bytes += size;
                out.push(std::mem::take(&mut entry));
                Ok(flow)
            };
            match step() {
                Ok(flow) => flow,
                Err(error) => {
                    failure = Some(error);
                    ControlFlow::Break(())
                }
            }
        },
    );
    if let Some(error) = failure {
        return Err(error);
    }
    if stopped.is_none() && limit < selected {
        stopped = Some((
            limit,
            format!("page limit max-pages={} reached", bounds.max_pages),
        ));
    }

    let mut result = serde_json::json!({
        "page_count": snapshot.page_count(),
        "pages_selected": selected,
        "truncated": stopped.is_some(),
    });
    if let Some((index, reason)) = stopped {
        let next = selection[index].one_based();
        result["next_page"] = serde_json::json!(next);
        match next_char {
            Some(next_char) => {
                result["next_char"] = serde_json::json!(next_char);
                warnings.push(format!(
                    "{reason} inside page {next}: continue with pages=\"{next}-\" and start_char={next_char}"
                ));
            }
            None => warnings.push(format!(
                "{reason}: {} of {selected} selected pages returned; pages from {next} on were not processed",
                out.len()
            )),
        }
    }
    result["pages"] = serde_json::Value::Array(out);
    result["warnings"] = serde_json::json!(warnings);
    Ok(result)
}

/// A [`text_pages`] result as text to be read: what it covered and any
/// warnings first, then each page under a `--- page N ---` line.
pub(crate) fn pages_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    for warning in result["warnings"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "warning: {}\n",
            warning.as_str().unwrap_or_default()
        ));
    }
    let pages = result["pages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for entry in pages {
        let mut header = format!("--- page {}", entry["page"]);
        if let Some(start) = entry["start_char"].as_u64() {
            header.push_str(&format!(", from character {start}"));
        }
        let data = &entry["data"];
        let provenance = &data["provenance"];
        let text = data["text"].as_str().unwrap_or_default();
        if provenance["source"] == "ocr" {
            header.push_str(", read by OCR");
        } else if text.trim().is_empty() {
            match provenance["page_content_kind"].as_str() {
                Some("scanned-image") => {
                    header.push_str(", no text layer: a scanned image, pass ocr=\"auto\"");
                }
                Some("blank") => header.push_str(", blank page"),
                Some(_) => header.push_str(", no text: render the page to see it"),
                None => {}
            }
        } else if provenance["native_text_trustworthy"] == false {
            header.push_str(
                ", text layer looks unreliable: compare with a render or pass ocr=\"auto\"",
            );
        }
        out.push_str(&header);
        out.push_str(" ---\n");
        if let Some(error) = entry["error"].as_str() {
            out.push_str(&format!("error: {error}\n"));
        }
        for warning in entry["warnings"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "warning: {}\n",
                warning.as_str().unwrap_or_default()
            ));
        }
        // Page text ends its lines the PDF way; one character is enough here.
        out.push_str(&text.replace("\r\n", "\n"));
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    if pages.is_empty() {
        out.push_str("no pages returned\n");
    }
    out
}

/// Cut `text` to at most `max_chars` characters, at the end of a line when
/// one falls in the last fifth of what is allowed. Returns the number of
/// characters kept.
fn cut_at_line(text: &mut String, max_chars: usize) -> usize {
    let mut kept = 0usize;
    let mut end = text.len();
    let mut line_end: Option<(usize, usize)> = None;
    for (count, (at, character)) in text.char_indices().enumerate() {
        if count == max_chars {
            end = at;
            break;
        }
        kept = count + 1;
        if character == '\n' && count + 1 >= max_chars - max_chars / 5 {
            line_end = Some((at + 1, count + 1));
        }
    }
    if end == text.len() {
        return kept;
    }
    if let Some((at, count)) = line_end {
        (end, kept) = (at, count);
    }
    text.truncate(end);
    kept
}

/// Cut a page record down until it serializes within `max_bytes`.
///
/// Geometry goes first — it is the bulk of a `words`/`chars`/`blocks` record
/// and the part a reader can re-request for a smaller region — then the text
/// itself. Returns the warning to attach, or `None` when the record already
/// fits (or the budget is unlimited).
pub(crate) fn fit_to_budget(data: &mut TextPageData, max_bytes: u64) -> Result<Option<String>> {
    if max_bytes == 0 {
        return Ok(None);
    }
    let size = |data: &TextPageData| -> Result<u64> { Ok(serde_json::to_vec(data)?.len() as u64) };
    let original = size(data)?;
    if original <= max_bytes {
        return Ok(None);
    }
    // Leave room for the envelope around the data and for this warning.
    let budget = max_bytes.saturating_sub(max_bytes.min(1024) / 2).max(1);

    fn shrink<T>(items: &mut Option<Vec<T>>, ratio: f64) -> bool {
        match items {
            Some(items) if !items.is_empty() => {
                let keep = ((items.len() as f64) * ratio).floor() as usize;
                items.truncate(keep.min(items.len() - 1));
                true
            }
            _ => false,
        }
    }
    let counts = |data: &TextPageData| {
        (
            data.words.as_ref().map_or(0, Vec::len),
            data.chars.as_ref().map_or(0, Vec::len),
            data.blocks.as_ref().map_or(0, Vec::len),
            data.text.as_ref().map_or(0, |text| text.chars().count()),
        )
    };
    let before = counts(data);

    loop {
        let now = size(data)?;
        if now <= budget {
            break;
        }
        let ratio = (budget as f64 / now as f64 * 0.9).clamp(0.0, 0.9);
        let geometry = shrink(&mut data.chars, ratio)
            | shrink(&mut data.words, ratio)
            | shrink(&mut data.blocks, ratio);
        if geometry {
            continue;
        }
        match &mut data.text {
            Some(text) if !text.is_empty() => {
                let mut keep = ((text.len() as f64) * ratio).floor() as usize;
                while keep > 0 && !text.is_char_boundary(keep) {
                    keep -= 1;
                }
                text.truncate(keep);
            }
            // Nothing left to cut: the fixed fields alone exceed the budget.
            _ => break,
        }
    }

    let after = counts(data);
    let mut kept = Vec::new();
    for (label, was, now) in [
        ("words", before.0, after.0),
        ("chars", before.1, after.1),
        ("blocks", before.2, after.2),
        ("text characters", before.3, after.3),
    ] {
        if was != now {
            kept.push(format!("{now} of {was} {label}"));
        }
    }
    Ok(Some(format!(
        "record cut from {original} bytes to fit max-bytes={max_bytes}: kept {}",
        if kept.is_empty() {
            "everything that could be cut".to_owned()
        } else {
            kept.join(", ")
        }
    )))
}

/// Extract one page against a caller-owned compiler and OCR backend.
///
/// `search` runs this across many pages and owns all three, so none is
/// rebuilt per page: constructing a `PageCompiler` under `--system-fonts`
/// re-scans every platform font directory, a fresh `CpuBackend` starts with
/// empty glyph and image caches, and a fresh `GlyphCache` re-reads every
/// font's outlines.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_page(
    snapshot: &DocumentSnapshot,
    compiler: &pdf_content::PageCompiler,
    pz: PageZero,
    options: &TextPageOptions,
    layout: TextLayout,
    bbox: Option<[f64; 4]>,
    ocr: OcrMode,
    ocr_language: &str,
    bounds: &Bounds,
    warnings: &mut Vec<String>,
    ocr_backend: &mut Option<CpuBackend>,
    glyphs: &mut GlyphCache,
) -> Result<TextPageData> {
    let mut ctx = parse_context();
    let page = compiler.compile_semantic(snapshot, PageIndex(pz.0), &mut ctx)?;
    let text_page = TextPage::build_with_cache(&page, options, glyphs);
    // Bind the page text once: `all_text()` allocates and walks the whole
    // page, so both counts are derived from this one string in a single
    // pass, and (below) the same string is reused for `data.text` instead of
    // being rebuilt a third time.
    let native_text = text_page.all_text();
    let mut native_char_count = 0usize;
    let mut replacement_count = 0usize;
    for character in native_text.chars() {
        if !character.is_whitespace() {
            native_char_count += 1;
        }
        if character == '\u{fffd}' {
            replacement_count += 1;
        }
    }
    let native_words = text_page.words();
    let valid_word_count = native_words
        .iter()
        .filter(|word| word.bbox.x1 > word.bbox.x0 && word.bbox.y1 > word.bbox.y0)
        .count();
    // Mostly-Latin text whose "words" average over twenty letters has lost its
    // word breaks. Scripts written without spaces are not judged this way.
    let ascii_letters = native_text
        .chars()
        .filter(char::is_ascii_alphabetic)
        .count();
    let spacing_lost = ascii_letters >= 200
        && ascii_letters.saturating_mul(10) >= native_char_count.saturating_mul(8)
        && native_char_count > valid_word_count.max(1).saturating_mul(20);
    if spacing_lost {
        warnings.push(format!(
            "native text has almost no word breaks ({native_char_count} characters in {valid_word_count} words); word spacing was probably lost, so it is reported as untrustworthy"
        ));
    }
    let trustworthy = native_char_count >= 8
        && replacement_count.saturating_mul(100) <= native_char_count
        && valid_word_count >= 2
        && !spacing_lost;
    // Nothing drawn at all: there is nothing to read and nothing to see.
    let draws_nothing = page.text_runs.is_empty()
        && page.images.is_empty()
        && page.paths.is_empty()
        && page.shadings.is_empty();
    let page_content_kind = match (trustworthy, !page.images.is_empty(), native_char_count > 0) {
        (true, true, _) => "hybrid",
        (true, false, _) => "native-text",
        (false, true, false) => "scanned-image",
        _ if draws_nothing => "blank",
        _ => "render-required",
    };

    let rect = bbox.map(|b| Rect {
        x0: b[0].min(b[2]),
        y0: b[1].min(b[3]),
        x1: b[0].max(b[2]),
        y1: b[1].max(b[3]),
    });

    // OCR has nothing to find on a page that draws nothing.
    if ocr == OcrMode::Always || (ocr == OcrMode::Auto && !trustworthy && !draws_nothing) {
        let backend =
            ocr_backend.get_or_insert_with(|| CpuBackend::new(CpuBackendOptions::default()));
        return extract_ocr_page(
            snapshot,
            compiler,
            pz,
            layout,
            rect,
            bounds,
            ocr_language,
            page_content_kind,
            trustworthy,
            native_char_count,
            page.images.len(),
            warnings,
            backend,
        );
    }

    let mut data = TextPageData {
        unit: "pdf_points",
        layout: format!("{:?}", layout).to_ascii_lowercase(),
        text: None,
        words: None,
        chars: None,
        blocks: None,
        char_count: text_page.char_count(),
        provenance: TextProvenance {
            source: "native",
            page_content_kind,
            native_text_trustworthy: trustworthy,
            native_char_count,
            image_count: page.images.len(),
            ocr_engine: None,
            ocr_language: None,
        },
    };

    match layout {
        TextLayout::Plain => {
            data.text = Some(match rect {
                Some(r) => text_page.text_in_rect(r),
                None => native_text,
            });
        }
        TextLayout::Physical => {
            data.text = Some(physical_text(collect_words(&text_page, rect)));
        }
        TextLayout::Words => {
            let mut words = collect_words(&text_page, rect);
            words = bounds.truncate_items(words, warnings, "words");
            data.words = Some(words);
            data.text = Some(match rect {
                Some(r) => text_page.text_in_rect(r),
                None => native_text,
            });
        }
        TextLayout::Chars => {
            let mut chars = collect_chars(&text_page, rect);
            chars = bounds.truncate_items(chars, warnings, "chars");
            data.chars = Some(chars);
            data.text = Some(match rect {
                Some(r) => text_page.text_in_rect(r),
                None => native_text,
            });
        }
        TextLayout::Blocks => {
            let words = collect_words(&text_page, rect);
            let mut blocks = group_blocks(words);
            blocks = bounds.truncate_items(blocks, warnings, "blocks");
            data.text = Some(
                blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
            data.blocks = Some(blocks);
        }
    }

    Ok(data)
}

#[allow(clippy::too_many_arguments)]
fn extract_ocr_page(
    snapshot: &DocumentSnapshot,
    compiler: &pdf_content::PageCompiler,
    pz: PageZero,
    layout: TextLayout,
    rect: Option<Rect>,
    bounds: &Bounds,
    language: &str,
    page_content_kind: &'static str,
    native_text_trustworthy: bool,
    native_char_count: usize,
    image_count: usize,
    warnings: &mut Vec<String>,
    backend: &CpuBackend,
) -> Result<TextPageData> {
    if layout == TextLayout::Chars {
        bail!(
            "OCR character-level geometry is not available in this first draft; use layout=words or blocks"
        );
    }
    const OCR_DPI: f64 = 300.0;
    const OCR_MAX_PIXELS: u64 = 20_000_000;
    let (image, device) = super::render::render_gray_for_ocr(
        snapshot,
        compiler,
        pz,
        OCR_DPI,
        OCR_MAX_PIXELS,
        backend,
    )?;
    let inverse = device
        .invert()
        .ok_or_else(|| anyhow::anyhow!("OCR page transform is not invertible"))?;
    let engine = lege_ocr::engine::default_engine();
    let engine_name = engine.name();
    let lines = engine.ocr_page(&image, language)?;

    let mut words = Vec::new();
    let mut line_text = Vec::new();
    let mut first_char = 0usize;
    for line in lines {
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        line_text.push(trimmed.to_owned());
        let line_start = first_char;
        let mut search_byte = 0usize;
        for word in line.words {
            let [line_x, line_y, _, _] = line.bbox_highres;
            let [x0, y0, x1, y1] = word.bbox_crop_local;
            let bbox = device_box_to_pdf(
                [line_x + x0, line_y + y0, line_x + x1, line_y + y1],
                inverse,
            );
            let char_count = word.text.chars().count();
            let word_first_char = trimmed[search_byte..]
                .find(&word.text)
                .map(|relative_byte| {
                    let absolute_byte = search_byte + relative_byte;
                    search_byte = absolute_byte + word.text.len();
                    line_start + trimmed[..absolute_byte].chars().count()
                })
                .unwrap_or(first_char);
            if rect.is_none_or(|selection| intersects_arr(bbox, selection)) {
                words.push(WordView {
                    text: word.text,
                    bbox,
                    first_char: word_first_char,
                    char_count,
                    continued: false,
                });
            }
        }
        first_char = line_start + trimmed.chars().count() + 1;
    }
    words = bounds.truncate_items(words, warnings, "OCR words");
    let mut text = if rect.is_some() {
        words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        line_text.join("\n")
    };
    let mut blocks = None;
    let mut output_words = None;
    match layout {
        TextLayout::Plain => {}
        TextLayout::Physical => text = physical_text(words),
        TextLayout::Words => output_words = Some(words),
        TextLayout::Blocks => {
            let grouped = group_blocks(words);
            text = grouped
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            blocks = Some(grouped);
        }
        TextLayout::Chars => unreachable!("rejected above"),
    }
    let char_count = text.chars().count();
    Ok(TextPageData {
        unit: "pdf_points",
        layout: format!("{:?}", layout).to_ascii_lowercase(),
        text: Some(text),
        words: output_words,
        chars: None,
        blocks,
        char_count,
        provenance: TextProvenance {
            source: "ocr",
            page_content_kind,
            native_text_trustworthy,
            native_char_count,
            image_count,
            ocr_engine: Some(engine_name),
            ocr_language: Some(language.to_owned()),
        },
    })
}

fn device_box_to_pdf(bbox: [u32; 4], inverse: pdf_page_ir::Matrix) -> [f64; 4] {
    let points = [
        inverse.apply(Point {
            x: f64::from(bbox[0]),
            y: f64::from(bbox[1]),
        }),
        inverse.apply(Point {
            x: f64::from(bbox[2]),
            y: f64::from(bbox[1]),
        }),
        inverse.apply(Point {
            x: f64::from(bbox[0]),
            y: f64::from(bbox[3]),
        }),
        inverse.apply(Point {
            x: f64::from(bbox[2]),
            y: f64::from(bbox[3]),
        }),
    ];
    points.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |mut result, point| {
            result[0] = result[0].min(point.x);
            result[1] = result[1].min(point.y);
            result[2] = result[2].max(point.x);
            result[3] = result[3].max(point.y);
            result
        },
    )
}

fn intersects_arr(a: [f64; 4], b: Rect) -> bool {
    a[0] < b.x1 && a[2] > b.x0 && a[1] < b.y1 && a[3] > b.y0
}

/// The words of a page, with `first_char` and `char_count` counting
/// characters of the page text.
///
/// The page text joins a word broken by a line-end hyphen, and the engine
/// reports it as one word whose box spans both lines and everything between
/// them. Here it comes back as its pieces, one per line, each with its own
/// box: `inter-` (marked `continued`) and `national`.
fn collect_words(text_page: &TextPage, rect: Option<Rect>) -> Vec<WordView> {
    let chars = text_page.chars();
    // Where each character falls in the page text, which leaves out the
    // line-end hyphens and the glyphs that have no Unicode value.
    let mut offsets = Vec::with_capacity(chars.len() + 1);
    let mut at = 0usize;
    for info in chars {
        offsets.push(at);
        let in_text = !matches!(info.char_type, CharType::NotUnicode | CharType::Hyphen);
        // The second half of a surrogate pair is the same character.
        if in_text && !(0xDC00..=0xDFFF).contains(&info.unicode) {
            at += 1;
        }
    }
    offsets.push(at);

    let mut out = Vec::new();
    for word in text_page.words() {
        let end = (word.first_char + word.char_count).min(chars.len());
        if !word.continued {
            if rect.is_none_or(|r| intersects(word.bbox, r)) {
                out.push(WordView {
                    text: word.text,
                    bbox: rect_arr(word.bbox),
                    first_char: offsets[word.first_char],
                    char_count: offsets[end] - offsets[word.first_char],
                    continued: false,
                });
            }
            continue;
        }
        let mut start = word.first_char;
        for index in word.first_char..end {
            let hyphen = chars[index].char_type == CharType::Hyphen;
            if !hyphen && index + 1 < end {
                continue;
            }
            if let Some(piece) = word_piece(chars, start..index + 1, &offsets, hyphen)
                && rect.is_none_or(|r| intersects_arr(piece.bbox, r))
            {
                out.push(piece);
            }
            start = index + 1;
        }
    }
    out
}

/// One line's share of a word broken across lines.
fn word_piece(
    chars: &[CharInfo],
    range: std::ops::Range<usize>,
    offsets: &[usize],
    continued: bool,
) -> Option<WordView> {
    let mut units = Vec::with_capacity(range.len());
    let mut bbox: Option<Rect> = None;
    for info in &chars[range.clone()] {
        if info.char_type == CharType::NotUnicode || info.unicode == 0 {
            continue;
        }
        units.push(if info.char_type == CharType::Hyphen {
            u16::from(b'-')
        } else {
            info.unicode as u16
        });
        bbox = Some(bbox.map_or(info.char_box, |old| old.union(info.char_box)));
    }
    Some(WordView {
        text: String::from_utf16_lossy(&units),
        bbox: rect_arr(bbox?),
        first_char: offsets[range.start],
        char_count: offsets[range.end] - offsets[range.start],
        continued,
    })
}

fn collect_chars(text_page: &TextPage, rect: Option<Rect>) -> Vec<CharView> {
    text_page
        .chars()
        .iter()
        .filter(|c| rect.is_none_or(|r| intersects(c.char_box, r)))
        .map(|info| {
            let printable = char::from_u32(info.unicode)
                .filter(|c| !c.is_control())
                .map(|c| c.to_string())
                .unwrap_or_default();
            CharView {
                unicode: info.unicode,
                text: printable,
                char_code: info.char_code,
                cid: info.cid,
                glyph_id: info.glyph_id,
                text_object: info.text_object.0,
                tight_box: rect_arr(info.char_box),
                loose_box: rect_arr(info.loose_char_box),
                char_type: format!("{:?}", info.char_type),
            }
        })
        .collect()
}

fn rect_arr(r: Rect) -> [f64; 4] {
    [r.x0, r.y0, r.x1, r.y1]
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0
}

/// A run of words on one text row with no column-sized gap inside it.
#[derive(Debug, Clone)]
pub(crate) struct Segment {
    /// Index of the row it was cut from, counted from the top of the page.
    pub row: usize,
    pub bbox: [f64; 4],
    pub words: Vec<WordView>,
}

/// A horizontal gap wider than this many word heights ends a segment.
const SEGMENT_GAP: f64 = 1.2;
/// A column gutter is at least this many segment heights wide.
const GUTTER: f64 = 1.0;
/// How many rows ahead to look for the start of a multi-column region.
const COLUMN_LOOKAHEAD: usize = 4;
/// A column has at least this many rows. Two lines that leave room for the
/// same displayed formula line up as well as that, and are one paragraph.
const COLUMN_ROWS: usize = 3;
/// Recursion bound for nested column structures.
const MAX_REGION_DEPTH: usize = 16;

/// Heuristic physical layout: words → rows → segments → reading order →
/// blocks. Not `pdftotext -layout` parity.
///
/// Rows are cut into segments at column-sized gaps, and the segments are
/// ordered by a recursive cut: where a clear gutter separates two bodies of
/// running text, the left body is read to its end before the right one;
/// otherwise rows are read top to bottom. A row that crosses the gutter — a
/// title over two columns, a full-width caption — ends the columns above it.
/// A gap that separates short cells rather than running text (a glossary, a
/// table, a contents line and its page number) is not a gutter, so those
/// rows stay whole.
fn group_blocks(words: Vec<WordView>) -> Vec<BlockView> {
    if words.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    order_region(row_segments(words), &mut lines, 0);
    build_blocks(lines)
}

pub(crate) fn height(bbox: [f64; 4]) -> f64 {
    (bbox[3] - bbox[1]).abs().max(1.0)
}

pub(crate) fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 1.0;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// How many of the rows above a word are tried before it starts a new one.
const ROW_LOOKBACK: usize = 3;

/// A row's band stops growing at this many typical word heights, so a tall
/// bracket or a drop capital cannot pull the lines it spans into one row.
const ROW_BAND: f64 = 2.0;

/// Cluster words into rows: top of the page first, each row left to right.
///
/// A word belongs to the row whose vertical band covers at least half of its
/// height (or half of the band, for a word taller than the row). Word boxes
/// are tight, so their middles do not line up — "an" is half the height of
/// "They", and a superscript sits above both — but they all overlap the band
/// of the line they are set on.
fn rows_of(mut words: Vec<WordView>) -> Vec<Vec<WordView>> {
    struct Row {
        low: f64,
        high: f64,
        words: Vec<WordView>,
    }
    let typical = median(words.iter().map(|word| height(word.bbox)).collect());
    words.sort_by(|a, b| {
        b.bbox[3]
            .total_cmp(&a.bbox[3])
            .then_with(|| a.bbox[0].total_cmp(&b.bbox[0]))
    });

    let mut rows: Vec<Row> = Vec::new();
    for word in words {
        let (low, high) = (
            word.bbox[1].min(word.bbox[3]),
            word.bbox[1].max(word.bbox[3]),
        );
        let tall = (high - low).max(1.0);
        let first = rows.len().saturating_sub(ROW_LOOKBACK);
        let home = rows[first..]
            .iter()
            .enumerate()
            .filter_map(|(offset, row)| {
                let shared = high.min(row.high) - low.max(row.low);
                let needed = 0.5 * tall.min((row.high - row.low).max(1.0));
                (shared >= needed).then_some((first + offset, shared))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index);
        match home {
            Some(index) => {
                let row = &mut rows[index];
                let limit = (ROW_BAND * typical).max(row.high - row.low);
                if high.max(row.high) - low.min(row.low) <= limit {
                    row.low = row.low.min(low);
                    row.high = row.high.max(high);
                }
                row.words.push(word);
            }
            None => rows.push(Row {
                low,
                high,
                words: vec![word],
            }),
        }
    }
    rows.sort_by(|a, b| (b.low + b.high).total_cmp(&(a.low + a.high)));
    rows.into_iter()
        .map(|mut row| {
            row.words.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
            row.words
        })
        .collect()
}

/// Cut each row of words wherever two neighbours stand further apart than
/// words in running text do.
pub(crate) fn row_segments(words: Vec<WordView>) -> Vec<Segment> {
    let mut segments = Vec::new();
    for (row, words) in rows_of(words).into_iter().enumerate() {
        let limit = SEGMENT_GAP * median(words.iter().map(|word| height(word.bbox)).collect());
        let mut current: Vec<WordView> = Vec::new();
        for word in words {
            if current
                .last()
                .is_some_and(|previous| word.bbox[0] - previous.bbox[2] > limit)
            {
                segments.push(finish_segment(row, std::mem::take(&mut current)));
            }
            current.push(word);
        }
        if !current.is_empty() {
            segments.push(finish_segment(row, current));
        }
    }
    segments
}

/// Blank lines kept between two rows of a physical layout, at most.
const MAX_BLANK_LINES: usize = 2;

/// In a physical layout, a step between two rows of this many line heights
/// leaves a blank line; twice as far, two.
const BLANK_LINE_STEP: f64 = 2.0;

/// In a physical layout, two words closer than this many grid cells are
/// neighbours in running text.
const WORD_GAP: f64 = 1.5;

/// A justified line set loosely has word spaces wider than that. In a row of
/// five words or more, a gap is a word space up to this many times the row's
/// median gap ...
const LOOSE_GAP: f64 = 1.6;

/// ... but never beyond this many grid cells, which is a column gutter in
/// any row.
const GUTTER_CELLS: f64 = 3.0;

/// The words of a page on a character grid: each word starts at the column
/// its left edge maps to, each row of words is one line, and vertical gaps
/// become blank lines. Alignment survives — the columns of a table line up,
/// a second text column stays to the right of the first — at the cost of
/// reading order, which is simply top to bottom.
pub(crate) fn physical_text(words: Vec<WordView>) -> String {
    if words.is_empty() {
        return String::new();
    }
    // One grid cell is a typical character. A little under the median, so a
    // run of narrow letters does not overrun the column that follows it.
    let mut widths: Vec<f64> = words
        .iter()
        .filter(|word| word.char_count >= 2)
        .map(|word| (word.bbox[2] - word.bbox[0]) / word.text.chars().count().max(1) as f64)
        .filter(|width| *width > 0.0)
        .collect();
    widths.sort_by(f64::total_cmp);
    let cell = widths
        .get(widths.len() * 2 / 5)
        .copied()
        .unwrap_or(6.0)
        .max(0.5);
    let left = words
        .iter()
        .map(|word| word.bbox[0])
        .fold(f64::INFINITY, f64::min);

    let rows = rows_of(words);
    let boxes: Vec<[f64; 4]> = rows
        .iter()
        .map(|row| union_boxes(row.iter().map(|word| word.bbox)))
        .collect();
    let middles: Vec<f64> = boxes.iter().map(|bbox| (bbox[1] + bbox[3]) * 0.5).collect();
    // A blank line stands for a step of two lines of type or more. Measured
    // against the height of a line rather than the steps between lines: a
    // table's rows are evenly and loosely spaced, and a page of them is not
    // a page of blank lines.
    let pitch =
        (BLANK_LINE_STEP * median(boxes.iter().map(|bbox| height(*bbox)).collect())).max(1.0);

    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            let blank = ((middles[index - 1] - middles[index]) / pitch)
                .floor()
                .max(0.0) as usize;
            for _ in 0..=blank.min(MAX_BLANK_LINES) {
                out.push('\n');
            }
        }
        let mut gaps: Vec<f64> = row
            .windows(2)
            .map(|pair| pair[1].bbox[0] - pair[0].bbox[2])
            .collect();
        let word_gap = if gaps.len() >= 4 {
            gaps.sort_by(f64::total_cmp);
            (LOOSE_GAP * gaps[gaps.len() / 2]).clamp(WORD_GAP * cell, GUTTER_CELLS * cell)
        } else {
            WORD_GAP * cell
        };
        let mut column = 0usize;
        let mut previous_right: Option<f64> = None;
        for word in row {
            let target = ((word.bbox[0] - left) / cell).round().max(0.0) as usize;
            let pad = match previous_right {
                None => target,
                // An ordinary word space is one space, wherever the grid
                // would have put the word: proportional type does not sit on
                // a grid, and rounding each word onto one doubles spaces.
                Some(right) if word.bbox[0] - right < word_gap => 1,
                // Anything wider is a column boundary, kept apart from a
                // word space even where the text before it overran its cells.
                Some(_) => target.saturating_sub(column).max(2),
            };
            previous_right = Some(word.bbox[2]);
            out.extend(std::iter::repeat_n(' ', pad));
            out.push_str(&word.text);
            column += pad + word.text.chars().count();
        }
    }
    out
}

fn finish_segment(row: usize, words: Vec<WordView>) -> Segment {
    Segment {
        row,
        bbox: union_boxes(words.iter().map(|word| word.bbox)),
        words,
    }
}

/// The x position of a gutter separating two columns of running text in
/// `segments`, if there is one.
pub(crate) fn column_cut(segments: &[&Segment]) -> Option<f64> {
    if segments.len() < 2 {
        return None;
    }
    let gutter = GUTTER
        * median(
            segments
                .iter()
                .map(|segment| height(segment.bbox))
                .collect(),
        );

    let mut by_left: Vec<&Segment> = segments.to_vec();
    by_left.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
    let mut gaps: Vec<(f64, f64)> = Vec::new();
    let mut covered = by_left[0].bbox[2];
    for segment in &by_left[1..] {
        if segment.bbox[0] - covered >= gutter {
            gaps.push((covered, segment.bbox[0]));
        }
        covered = covered.max(segment.bbox[2]);
    }
    gaps.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));

    gaps.into_iter()
        .map(|(left_edge, right_edge)| (left_edge + right_edge) * 0.5)
        .find(|&cut| {
            let side = |left: bool| {
                segments
                    .iter()
                    .copied()
                    .filter(move |segment| (segment.bbox[0] < cut) == left)
            };
            let extent = |left: bool| -> (f64, f64) {
                side(left).fold(
                    (f64::INFINITY, f64::NEG_INFINITY),
                    |(low, high), segment| (low.min(segment.bbox[1]), high.max(segment.bbox[3])),
                )
            };
            let (left_low, left_high) = extent(true);
            let (right_low, right_high) = extent(false);
            // Columns stand side by side; a heading above a body does not.
            let side_by_side = left_high.min(right_high) - left_low.max(right_low) > 0.0;
            let rows = |left: bool| {
                let mut rows: Vec<usize> = side(left).map(|segment| segment.row).collect();
                rows.sort_unstable();
                rows.dedup();
                rows.len()
            };
            side_by_side
                && rows(true) >= COLUMN_ROWS
                && rows(false) >= COLUMN_ROWS
                && is_running_text(side(true))
                && is_running_text(side(false))
        })
}

/// Whether most of the words sit in segments of three or more — sentences
/// rather than cells, labels or page numbers.
fn is_running_text<'a>(segments: impl Iterator<Item = &'a Segment>) -> bool {
    let (mut total, mut in_long) = (0usize, 0usize);
    for segment in segments {
        total += segment.words.len();
        if segment.words.len() >= 3 {
            in_long += segment.words.len();
        }
    }
    total > 0 && in_long * 10 >= total * 6
}

/// Append the lines of `segments` to `out` in reading order.
fn order_region(segments: Vec<Segment>, out: &mut Vec<Vec<Segment>>, depth: usize) {
    if segments.is_empty() {
        return;
    }
    if depth < MAX_REGION_DEPTH {
        let refs: Vec<&Segment> = segments.iter().collect();
        if let Some(cut) = column_cut(&refs) {
            let (left, right): (Vec<Segment>, Vec<Segment>) = segments
                .into_iter()
                .partition(|segment| segment.bbox[0] < cut);
            // An empty line marks a column boundary: text on either side
            // of it is never one block, however close it sits.
            out.push(Vec::new());
            order_region(left, out, depth + 1);
            out.push(Vec::new());
            order_region(right, out, depth + 1);
            out.push(Vec::new());
            return;
        }
    }

    // No gutter runs through the whole region: read it row by row, handing
    // each stretch of rows that does share a gutter back as a region.
    let mut rows: Vec<Vec<Segment>> = Vec::new();
    let mut segments = segments;
    segments.sort_by(|a, b| {
        a.row
            .cmp(&b.row)
            .then_with(|| a.bbox[0].total_cmp(&b.bbox[0]))
    });
    for segment in segments {
        match rows.last_mut() {
            Some(row) if row[0].row == segment.row => row.push(segment),
            _ => rows.push(vec![segment]),
        }
    }
    if rows.len() == 1 || depth >= MAX_REGION_DEPTH {
        out.extend(rows);
        return;
    }

    let splits = |rows: &[Vec<Segment>]| -> bool {
        let refs: Vec<&Segment> = rows.iter().flatten().collect();
        column_cut(&refs).is_some()
    };
    let mut start = 0;
    while start < rows.len() {
        let mut end = None;
        for ahead in 1..=COLUMN_LOOKAHEAD {
            if start + ahead < rows.len() && splits(&rows[start..=start + ahead]) {
                end = Some(start + ahead);
            }
        }
        let Some(mut end) = end else {
            out.push(std::mem::take(&mut rows[start]));
            start += 1;
            continue;
        };
        while end + 1 < rows.len() && splits(&rows[start..=end + 1]) {
            end += 1;
        }
        let region: Vec<Segment> = rows[start..=end]
            .iter_mut()
            .flat_map(std::mem::take)
            .collect();
        order_region(region, out, depth + 1);
        start = end + 1;
    }
}

/// A step between two lines of this many typical line steps is a paragraph
/// break.
const PARAGRAPH_STEP: f64 = 1.3;
/// A line set in or out by this many line heights starts a paragraph, where
/// the lines around it say which of the two edges is the paragraph's own.
const INDENT: f64 = 0.8;

/// What a line opens with, if it opens a list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// A bullet glyph: never the start of a line in running text.
    Bullet,
    /// `1.`, `(a)`, `[12]`: a list number, or a citation in mid-sentence.
    Number,
}

fn list_marker(line: &LineView) -> Option<Marker> {
    let first = line.words.first()?.text.as_str();
    let mut chars = first.chars();
    if let (Some(only), None) = (chars.next(), chars.next())
        && "\u{2022}\u{25e6}\u{25aa}\u{25a0}\u{25cf}\u{25cb}\u{2023}\u{2219}\u{25c6}\u{27a2}\u{f0b7}\u{f0a7}"
            .contains(only)
    {
        return Some(Marker::Bullet);
    }
    let opened = first.strip_prefix(['(', '[']);
    let body = opened.unwrap_or(first);
    let label = body.strip_suffix(['.', ')', ']'])?;
    let numbered = (1..=3).contains(&label.len()) && label.bytes().all(|b| b.is_ascii_digit());
    let lettered = label.len() == 1 && label.bytes().all(|b| b.is_ascii_alphabetic());
    (numbered || lettered).then_some(Marker::Number)
}

/// Turn ordered lines into blocks. A new block starts wherever the reading
/// order jumps to another column, at a step between lines wider than the
/// usual one on the page, after a line that stops short of room its
/// successor's first word would have filled, and at a line indented from (or
/// hanging out of) the left edge of the lines around it.
fn build_blocks(lines: Vec<Vec<Segment>>) -> Vec<BlockView> {
    // `None` is a column boundary. The baseline of a line is the bottom of
    // its typical word: word boxes are tight, and most words have no
    // descender.
    let lines: Vec<Option<(LineView, f64)>> = lines
        .into_iter()
        .map(|segments| {
            let words: Vec<WordView> = segments
                .into_iter()
                .flat_map(|segment| segment.words)
                .collect();
            if words.is_empty() {
                return None;
            }
            let baseline = median(words.iter().map(|word| word.bbox[1]).collect());
            let line = LineView {
                text: words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                bbox: union_boxes(words.iter().map(|word| word.bbox)),
                words,
            };
            Some((line, baseline))
        })
        .collect();

    // One line under another, in the same column.
    let follows = |above: &LineView, below: &LineView| {
        below.bbox[1] <= above.bbox[1]
            && below.bbox[0] < above.bbox[2]
            && above.bbox[0] < below.bbox[2]
    };
    let steps: Vec<f64> = lines
        .windows(2)
        .filter_map(|pair| match pair {
            [Some((above, high)), Some((below, low))] if follows(above, below) => Some(high - low),
            _ => None,
        })
        .filter(|step| *step > 0.0)
        .collect();
    let typical_step = (steps.len() >= 3).then(|| median(steps));

    let mut blocks = Vec::new();
    let mut current: Vec<LineView> = Vec::new();
    let mut previous_baseline = 0.0;
    for (index, entry) in lines.iter().enumerate() {
        let Some((line, baseline)) = entry else {
            if !current.is_empty() {
                blocks.push(finish_block(std::mem::take(&mut current)));
            }
            continue;
        };
        if let Some(previous) = current.last() {
            let block = union_boxes(current.iter().map(|line| line.bbox));
            let paragraph_gap = match typical_step {
                Some(typical) => previous_baseline - baseline > PARAGRAPH_STEP * typical,
                None => previous.bbox[1] - line.bbox[3] > 1.5 * height(previous.bbox),
            };
            let moved_up = line.bbox[1] > previous.bbox[1];
            let beside = line.bbox[0] >= block[2] || line.bbox[2] <= block[0];
            let tall = height(line.bbox);
            let shift = line.bbox[0] - previous.bbox[0];
            // The line after this one is back at the edge of the one before.
            let returns = lines
                .get(index + 1)
                .and_then(Option::as_ref)
                .is_some_and(|(next, _)| {
                    follows(line, next) && (next.bbox[0] - previous.bbox[0]).abs() <= 0.3 * tall
                });
            let marker = list_marker(line);
            // A first line set in. Under a list marker it is the item going
            // on, not a paragraph.
            let set_in = shift > INDENT * tall && returns && list_marker(previous).is_none();
            // An item hanging out to the left of the lines that continue it.
            // Only under a marker: without one, a line set out between two
            // set in is the second line of a two-line paragraph.
            let set_out = shift < -INDENT * tall && marker.is_some();
            // A paragraph's last line: the next word would have fit on it.
            // Centred lines share a middle rather than an edge, and every
            // one of them stops short.
            let centred = shift.abs() > 0.3 * tall
                && ((line.bbox[0] + line.bbox[2]) - (previous.bbox[0] + previous.bbox[2])).abs()
                    <= 0.6 * tall;
            let first_word = line
                .words
                .first()
                .map_or(0.0, |word| word.bbox[2] - word.bbox[0]);
            let stopped_short = !centred
                && shift.abs() <= 3.0 * tall
                && previous.bbox[2] + first_word + 0.8 * tall < block[2].max(line.bbox[2]);
            let new_item = marker == Some(Marker::Bullet);
            if paragraph_gap || moved_up || beside || set_in || set_out || stopped_short || new_item
            {
                blocks.push(finish_block(std::mem::take(&mut current)));
            }
        }
        previous_baseline = *baseline;
        current.push(line.clone());
    }
    if !current.is_empty() {
        blocks.push(finish_block(current));
    }
    blocks
}

fn finish_block(lines: Vec<LineView>) -> BlockView {
    let text = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let bbox = union_boxes(lines.iter().map(|l| l.bbox));
    BlockView { text, bbox, lines }
}

pub(crate) fn union_boxes(iter: impl Iterator<Item = [f64; 4]>) -> [f64; 4] {
    let mut out = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for b in iter {
        out[0] = out[0].min(b[0]);
        out[1] = out[1].min(b[1]);
        out[2] = out[2].max(b[2]);
        out[3] = out[3].max(b[3]);
    }
    if !out[0].is_finite() {
        [0.0, 0.0, 0.0, 0.0]
    } else {
        out
    }
}

fn emit_page(
    env: &Envelope,
    data: &TextPageData,
    mode: OutputMode,
    multi: bool,
    rendered: &str,
) -> Result<()> {
    match mode {
        OutputMode::Human => {
            if multi {
                println!("--- page {} ---", env.page.unwrap_or(0));
            }
            if let Some(text) = &data.text {
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            }
            if let Some(words) = &data.words {
                for (i, w) in words.iter().enumerate() {
                    println!(
                        "word {i}: {:?} box={:?} chars={}..{}",
                        w.text,
                        w.bbox,
                        w.first_char,
                        w.first_char + w.char_count
                    );
                }
            }
            if let Some(chars) = &data.chars {
                for (i, c) in chars.iter().enumerate() {
                    println!(
                        "char {i}: U+{:04X} {:?} box={:?}",
                        c.unicode, c.text, c.tight_box
                    );
                }
            }
            if let Some(blocks) = &data.blocks {
                for (i, b) in blocks.iter().enumerate() {
                    println!("block {i}: box={:?}\n{}", b.bbox, b.text);
                }
            }
            for w in &env.warnings {
                eprintln!("warning: {w}");
            }
            Ok(())
        }
        // `rendered` was already serialized (pretty for single-page Json,
        // compact for Jsonl/multi-page Json) by the caller, so both JSON
        // modes just print it instead of asking `Envelope` to serialize
        // again.
        OutputMode::Json | OutputMode::Jsonl => {
            println!("{rendered}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    /// Words laid left to right from `x`, 8pt tall, 3pt apart.
    fn row(y: f64, x: f64, text: &str) -> Vec<WordView> {
        let mut cursor = x;
        text.split_whitespace()
            .map(|word| {
                let width = 5.0 * word.chars().count() as f64;
                let view = WordView {
                    text: word.to_owned(),
                    bbox: [cursor, y, cursor + width, y + 8.0],
                    first_char: 0,
                    char_count: word.chars().count(),
                    continued: false,
                };
                cursor += width + 3.0;
                view
            })
            .collect()
    }

    fn lines(blocks: &[BlockView]) -> Vec<&str> {
        blocks
            .iter()
            .flat_map(|block| block.lines.iter().map(|line| line.text.as_str()))
            .collect()
    }

    /// The defect this replaces: left and right lines sharing a baseline were
    /// merged into one line, so two columns read as alternating halves.
    #[test]
    fn two_columns_under_a_spanning_title_read_one_column_at_a_time() {
        let mut words = row(
            700.0,
            60.0,
            "A title wide enough to run across both of the columns below it",
        );
        for (index, y) in [680.0, 668.0, 656.0, 644.0].into_iter().enumerate() {
            words.extend(row(y, 40.0, &format!("left column line {index} of text")));
            words.extend(row(y, 320.0, &format!("right column line {index} of text")));
        }
        let blocks = group_blocks(words);
        assert_eq!(
            lines(&blocks),
            vec![
                "A title wide enough to run across both of the columns below it",
                "left column line 0 of text",
                "left column line 1 of text",
                "left column line 2 of text",
                "left column line 3 of text",
                "right column line 0 of text",
                "right column line 1 of text",
                "right column line 2 of text",
                "right column line 3 of text",
            ]
        );
        assert!(
            blocks.len() >= 3,
            "title, left and right are separate blocks"
        );
    }

    /// A glossary has the same geometry as two columns, but its left side is
    /// cells, not running text: each row must stay whole.
    #[test]
    fn a_term_and_definition_table_keeps_its_rows() {
        let mut words = Vec::new();
        for (y, term) in [(680.0, "HCDH"), (668.0, "HDMMR"), (656.0, "HGX")] {
            words.extend(row(y, 40.0, term));
            words.extend(row(y, 150.0, "the value used by the decoding procedure"));
        }
        assert_eq!(
            lines(&group_blocks(words)),
            vec![
                "HCDH the value used by the decoding procedure",
                "HDMMR the value used by the decoding procedure",
                "HGX the value used by the decoding procedure",
            ]
        );
    }

    #[test]
    fn a_single_column_breaks_into_blocks_at_paragraph_gaps() {
        let mut words = Vec::new();
        for y in [700.0, 688.0, 676.0] {
            words.extend(row(y, 40.0, "first paragraph line of running text"));
        }
        for y in [640.0, 628.0] {
            words.extend(row(y, 40.0, "second paragraph line of running text"));
        }
        let blocks = group_blocks(words);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lines.len(), 3);
        assert_eq!(blocks[1].lines.len(), 2);
    }

    /// Paragraphs set without space between them: the step from line to
    /// line never changes, so the breaks are a last line that stops short
    /// and a first line set in.
    #[test]
    fn paragraphs_set_solid_break_at_short_last_lines_and_indents() {
        let full = "full line of running text here now";
        let mut words = row(700.0, 40.0, &format!("one {full}"));
        words.extend(row(688.0, 40.0, &format!("one {full}")));
        words.extend(row(676.0, 40.0, "ends short"));
        words.extend(row(664.0, 50.0, &format!("two {full}")));
        words.extend(row(652.0, 40.0, &format!("two {full}")));
        words.extend(row(640.0, 50.0, "set in after a full line of running tx"));
        words.extend(row(628.0, 40.0, "three full line of running text here"));
        let blocks = group_blocks(words);
        let sizes: Vec<usize> = blocks.iter().map(|block| block.lines.len()).collect();
        assert_eq!(sizes, [3, 2, 2], "{:?}", lines(&blocks));
    }

    /// A list item hangs out to the left of the lines that continue it; the
    /// continuation is set in exactly as a new paragraph would be.
    #[test]
    fn a_list_item_keeps_its_continuation_lines() {
        let mut words = row(700.0, 40.0, "• first item runs to the end of the line");
        words.extend(row(688.0, 52.0, "and goes on here"));
        words.extend(row(676.0, 40.0, "• second item is a single line"));
        words.extend(row(664.0, 40.0, "• third item runs to the end of the line"));
        words.extend(row(652.0, 52.0, "and ends"));
        let blocks = group_blocks(words);
        let sizes: Vec<usize> = blocks.iter().map(|block| block.lines.len()).collect();
        assert_eq!(sizes, [2, 1, 2], "{:?}", lines(&blocks));
    }

    /// Word boxes are tight: a superscript, a word with no ascender and a
    /// comma left behind by a formula are all on the line they are set on.
    #[test]
    fn small_and_raised_words_stay_on_their_row() {
        let word = |text: &str, x: f64, low: f64, high: f64| WordView {
            text: text.to_owned(),
            bbox: [x, low, x + 5.0 * text.chars().count() as f64, high],
            first_char: 0,
            char_count: text.chars().count(),
            continued: false,
        };
        let mut words = vec![
            word("Type", 40.0, 698.0, 708.0),
            word("an", 63.0, 700.0, 704.5),
            word("mc", 76.0, 700.0, 704.5),
            word("2", 86.5, 705.5, 710.0),
            word(",", 120.0, 698.5, 701.0),
        ];
        words.extend(row(688.0, 40.0, "the next line"));
        assert_eq!(physical_text(words), "Type an mc 2    ,\nthe next line");
    }

    /// A justified line set loosely is still one run of words.
    #[test]
    fn physical_layout_reads_a_loose_line_as_running_text() {
        let mut words = row(
            700.0,
            40.0,
            "an ordinary line of running text sets the grid",
        );
        let mut cursor = 40.0;
        for text in ["this", "line", "was", "stretched", "to", "fit"] {
            let width = 5.0 * text.chars().count() as f64;
            words.push(WordView {
                text: text.to_owned(),
                bbox: [cursor, 688.0, cursor + width, 696.0],
                first_char: 0,
                char_count: text.chars().count(),
                continued: false,
            });
            cursor += width + 9.0;
        }
        let text = physical_text(words);
        assert_eq!(
            text.split('\n').nth(1),
            Some("this line was stretched to fit"),
            "{text:?}"
        );
    }

    /// A second column keeps its place, and a table keeps its columns.
    #[test]
    fn physical_layout_keeps_columns_aligned() {
        let mut words = row(700.0, 40.0, "Name");
        words.extend(row(700.0, 140.0, "Value"));
        words.extend(row(688.0, 40.0, "SBW"));
        words.extend(row(688.0, 140.0, "32"));
        // A paragraph gap, then a longer line.
        words.extend(row(652.0, 40.0, "after a gap"));
        let text = physical_text(words);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 5, "{text:?}");
        assert_eq!(lines[0].find("Value"), lines[1].find("32"));
        assert!(lines[0].starts_with("Name") && lines[1].starts_with("SBW"));
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "after a gap");
    }

    #[test]
    fn a_cut_lands_on_a_line_end_and_counts_characters() {
        let mut text = "first line\nsecond liné\nthird line".to_owned();
        let kept = cut_at_line(&mut text, 25);
        assert_eq!(text, "first line\nsecond liné\n");
        assert_eq!(kept, 23);

        let mut text = "no line breaks here at all".to_owned();
        assert_eq!(cut_at_line(&mut text, 7), 7);
        assert_eq!(text, "no line");

        let mut text = "short".to_owned();
        assert_eq!(cut_at_line(&mut text, 50), 5);
        assert_eq!(text, "short");
    }

    #[test]
    fn a_record_over_the_byte_budget_is_cut_and_says_what_was_kept() {
        let words: Vec<WordView> = (0..400)
            .flat_map(|index| row(700.0 - index as f64, 40.0, "word"))
            .collect();
        let mut data = TextPageData {
            unit: "pdf_points",
            layout: "words".into(),
            text: Some("word ".repeat(400)),
            words: Some(words),
            chars: None,
            blocks: None,
            char_count: 2000,
            provenance: TextProvenance {
                source: "native",
                page_content_kind: "native-text",
                native_text_trustworthy: true,
                native_char_count: 1600,
                image_count: 0,
                ocr_engine: None,
                ocr_language: None,
            },
        };
        assert!(fit_to_budget(&mut data.clone(), 0).unwrap().is_none());
        assert!(fit_to_budget(&mut data.clone(), 1 << 20).unwrap().is_none());

        let warning = fit_to_budget(&mut data, 4096)
            .unwrap()
            .expect("over budget");
        assert!(serde_json::to_vec(&data).unwrap().len() <= 4096);
        assert!(data.words.as_ref().unwrap().len() < 400);
        assert!(warning.contains("of 400 words"), "{warning}");
    }
}
