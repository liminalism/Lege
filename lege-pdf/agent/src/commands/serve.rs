//! Stdio JSONL persistent agent service.

use std::borrow::Cow;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::bounds::Bounds;
use crate::cache::SnapshotCache;
use crate::commands::{
    annotations, check, content, images, inspect, map, matching, render, search, tables, text,
};
use crate::pages::{parse_bbox, parse_page_range, parse_scan_range};

#[derive(Debug, Deserialize)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, serde::Serialize)]
struct Response {
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
}

#[derive(Debug, serde::Serialize)]
struct ErrorBody {
    code: i32,
    message: String,
}

#[derive(Debug)]
pub struct ServeArgs {
    pub max_open: usize,
    pub idle_timeout: u64,
    pub bounds: Bounds,
}

pub fn run(args: ServeArgs) -> Result<i32> {
    let mut cache = SnapshotCache::new(args.max_open, args.idle_timeout);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let lines = stdin.lock().lines();

    for line in lines {
        let line = line.context("reading stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(err) => {
                write_response(
                    &mut stdout,
                    Response {
                        id: Value::Null,
                        result: None,
                        error: Some(ErrorBody {
                            code: -32700,
                            message: format!("parse error: {err}"),
                        }),
                    },
                )?;
                continue;
            }
        };

        if req.method == "close" || req.method == "shutdown" {
            write_response(
                &mut stdout,
                Response {
                    id: req.id,
                    result: Some(serde_json::json!({"ok": true})),
                    error: None,
                },
            )?;
            break;
        }

        // This transport carries JSON only: an image goes to a file.
        let response = match call_method(&mut cache, &req.method, &req.params, args.bounds, false) {
            Ok(reply) => Response {
                id: req.id,
                result: Some(reply.value),
                error: None,
            },
            Err(err) => Response {
                id: req.id,
                result: None,
                error: Some(ErrorBody {
                    code: -32000,
                    message: format!("{err:#}"),
                }),
            },
        };
        write_response(&mut stdout, response)?;
    }
    Ok(0)
}

fn write_response(out: &mut impl Write, response: Response) -> Result<()> {
    let mut line = serde_json::to_string(&response)?;
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// What a method returns: the structured result, the same result as text a
/// reader takes in directly, and any images it produced in memory.
#[derive(Debug)]
pub(crate) struct Reply {
    pub value: Value,
    pub text: String,
    pub images: Vec<render::EncodedImage>,
}

impl Reply {
    fn new(value: Value, text: String) -> Self {
        Self {
            value,
            text,
            images: Vec::new(),
        }
    }

    /// A result with no better text form than its own JSON.
    fn json(value: Value) -> Self {
        let text = value.to_string();
        Self::new(value, text)
    }
}

/// Warnings first — they qualify everything below them — then the body.
fn text_of(warnings: &[String], body: impl IntoIterator<Item = String>) -> String {
    let mut lines: Vec<String> = warnings
        .iter()
        .map(|warning| format!("warning: {warning}"))
        .collect();
    lines.extend(body);
    lines.join("\n")
}

/// Outline items one inspect request lists unless it asks for more.
const DEFAULT_OUTLINE: u32 = 200;
/// Characters of page text one text request returns unless it asks for
/// more: about ten thousand tokens, a comfortable single read.
const DEFAULT_MAX_CHARS: u32 = 40_000;
/// Matches returned by one search request unless it asks for more: enough to
/// answer "where is this", small enough to read.
const DEFAULT_MAX_MATCHES: u32 = 50;
/// Page lines one map request writes out as text; the structured result
/// always has every page.
const MAP_LINES: usize = 250;
/// Longest edge of an image returned inline when no size is asked for: what
/// a vision model takes without downscaling.
const INLINE_EDGE: u32 = 1568;
/// Longest edge of any image returned inline.
const INLINE_MAX_EDGE: u32 = 4096;
/// Pages one request returns inline as separate images.
const INLINE_MAX_PAGES: usize = 6;

/// `inline` says whether the transport can carry image bytes beside the
/// result (MCP can; the JSONL service cannot, and writes files).
pub(crate) fn call_method(
    cache: &mut SnapshotCache,
    method: &str,
    params: &Value,
    bounds: Bounds,
    inline: bool,
) -> Result<Reply> {
    // One time budget per request; dropped, and so disarmed, on return.
    let _deadline = crate::deadline::arm(bounds.timeout_secs);
    if method == "ping" {
        return Ok(Reply::json(serde_json::json!({
            "pong": true,
            "cache_open": cache.len(),
            "schema": crate::schema::SCHEMA_ID,
        })));
    }
    if !is_method(method) {
        bail!("unknown method {method:?}");
    }
    let path = param_path(params)?;
    let password = param_str(params, "password");
    let (identity, snapshot) = cache.get_or_open(&path, password)?;
    let snapshot = snapshot.as_ref();
    let pages = param_page_selection(params, "pages")?;
    let pages = pages.as_deref();

    match method {
        "pdf_inspect" => {
            let outline_limit = param_u32(params, "outline_limit").unwrap_or(DEFAULT_OUTLINE);
            let (data, warnings) =
                inspect::inspect_data(snapshot, pages, outline_limit as usize, bounds)?;
            let text = text_of(&warnings, inspect::lines(&data));
            Ok(Reply::new(
                with_warnings(serde_json::to_value(&data)?, warnings),
                text,
            ))
        }
        "pdf_map" => {
            let (data, warnings) = map::map_document(snapshot, pages, false, &bounds)?;
            let listed = data.pages.len().min(MAP_LINES);
            let mut lines = vec![map::summary_line(&data)];
            lines.extend(data.pages[..listed].iter().map(map::page_line));
            if let Some(next) = data.pages.get(listed) {
                lines.push(format!(
                    "… {} more pages not listed: ask again with pages=\"{}-\"",
                    data.pages.len() - listed,
                    next.page
                ));
            }
            let text = text_of(&warnings, lines);
            Ok(Reply::new(
                with_warnings(serde_json::to_value(&data)?, warnings),
                text,
            ))
        }
        "pdf_text" => {
            let request = text::TextRequest {
                layout: match param_str(params, "layout").unwrap_or("plain") {
                    "plain" => text::TextLayout::Plain,
                    "physical" => text::TextLayout::Physical,
                    "blocks" => text::TextLayout::Blocks,
                    "words" => text::TextLayout::Words,
                    "chars" => text::TextLayout::Chars,
                    other => bail!("unknown layout {other:?}"),
                },
                bbox: param_str(params, "bbox").map(parse_bbox).transpose()?,
                options: pdf_text::TextPageOptions::default(),
                ocr: param_ocr(params)?,
                ocr_language: param_str(params, "ocr_language").unwrap_or("eng"),
                bounds,
                max_chars: param_u32(params, "max_chars").unwrap_or(DEFAULT_MAX_CHARS) as usize,
                start_char: param_u32(params, "start_char").unwrap_or(0) as usize,
            };
            // Without a selection: from the first page, as far as the
            // character budget reaches.
            let value = text::text_pages(snapshot, pages, &request)?;
            Ok(
                if matches!(
                    request.layout,
                    text::TextLayout::Words | text::TextLayout::Chars
                ) {
                    // Geometry is the point of these layouts; it has no
                    // prose form.
                    Reply::json(value)
                } else {
                    let text = text::pages_text(&value);
                    Reply::new(value, text)
                },
            )
        }
        "pdf_search" => {
            let query = param_str(params, "query").context("params.query required")?;
            let spec = search::SearchSpec {
                query,
                options: matching::QueryOptions {
                    regex: param_bool(params, "regex").unwrap_or(false),
                    case_insensitive: param_bool(params, "case_insensitive").unwrap_or(true),
                    exact: param_bool(params, "exact").unwrap_or(false),
                },
                context: param_u32(params, "context").unwrap_or(60) as usize,
                max_matches: param_u32(params, "max_matches").unwrap_or(DEFAULT_MAX_MATCHES)
                    as usize,
                ocr: param_ocr(params)?,
                ocr_language: param_str(params, "ocr_language").unwrap_or("eng"),
                system_fonts: false,
                bounds,
            };
            let scan = parse_scan_range(pages, snapshot.page_count(), bounds.scan_pages)?;
            let (selection, mut warnings) = (scan.pages, scan.warnings);
            let mut out: Vec<Value> = Vec::new();
            let mut lines: Vec<String> = Vec::new();
            let mut used = 0u64;
            let mut over_budget = None;
            let mut summary = search::search_pages(snapshot, &selection, &spec, |event| {
                let (entry, entry_lines) = match event {
                    search::SearchEvent::Page(page, data, page_warnings) => {
                        let mut entry_lines: Vec<String> = page_warnings
                            .iter()
                            .map(|warning| format!("warning: p.{}: {warning}", page.one_based()))
                            .collect();
                        entry_lines.extend(
                            data.matches
                                .iter()
                                .map(|found| search::match_line(page.one_based(), found)),
                        );
                        let mut entry = serde_json::json!({
                            "page": page.one_based(),
                            "page_index": page.0,
                            "data": data,
                        });
                        if !page_warnings.is_empty() {
                            entry["warnings"] = serde_json::json!(page_warnings);
                        }
                        (entry, entry_lines)
                    }
                    search::SearchEvent::Failed(page, error, _) => (
                        serde_json::json!({
                            "page": page.one_based(),
                            "page_index": page.0,
                            "error": error,
                        }),
                        vec![format!("p.{}: error: {error}", page.one_based())],
                    ),
                };
                let size = serde_json::to_vec(&entry)?.len() as u64;
                if bounds.max_bytes > 0 && !out.is_empty() && used + size > bounds.max_bytes {
                    over_budget = entry["page"].as_u64();
                    return Ok(std::ops::ControlFlow::Break(()));
                }
                used += size;
                out.push(entry);
                lines.extend(entry_lines);
                Ok(std::ops::ControlFlow::Continue(()))
            })?;
            if let Some(page) = over_budget {
                summary.stop(
                    page as u32,
                    format!("byte budget max-bytes={} reached", bounds.max_bytes),
                );
            }
            summary.limited(scan.selected, scan.next_page);
            warnings.extend(summary.notes());
            lines.insert(
                0,
                format!(
                    "{} match{} for {query:?} on {} page{} ({} of {} pages searched)",
                    summary.match_count,
                    if summary.match_count == 1 { "" } else { "es" },
                    summary.pages_with_matches,
                    if summary.pages_with_matches == 1 {
                        ""
                    } else {
                        "s"
                    },
                    summary.pages_searched,
                    snapshot.page_count(),
                ),
            );
            let mut result = serde_json::to_value(&summary)?;
            result["page_count"] = serde_json::json!(snapshot.page_count());
            result["pages"] = Value::Array(out);
            result["warnings"] = serde_json::json!(warnings);
            let text = text_of(&warnings, lines);
            Ok(Reply::new(result, text))
        }
        "pdf_tables" => {
            let (value, blocks) = tables::tables_value(
                snapshot,
                pages,
                param_ocr(params)?,
                param_str(params, "ocr_language").unwrap_or("eng"),
                &bounds,
            )?;
            let warnings = string_list(&value["warnings"]);
            let mut lines = vec![format!(
                "{} table{} on {} page{} examined",
                value["table_count"],
                if value["table_count"] == 1 { "" } else { "s" },
                value["pages_selected"],
                if value["pages_selected"] == 1 {
                    ""
                } else {
                    "s"
                },
            )];
            lines.extend(blocks.into_iter().map(|block| format!("\n{block}")));
            let text = text_of(&warnings, lines);
            Ok(Reply::new(value, text))
        }
        "pdf_annotations" => {
            let kinds = annotations::Kinds::from_flags(
                param_bool(params, "links").unwrap_or(false),
                param_bool(params, "comments").unwrap_or(false),
                param_bool(params, "forms").unwrap_or(false),
            );
            let (data, warnings) = annotations::collect(snapshot, pages, kinds, &bounds)?;
            let mut lines = vec![annotations::summary_line(&data)];
            lines.extend(annotations::lines(&data));
            let text = text_of(&warnings, lines);
            Ok(Reply::new(
                with_warnings(serde_json::to_value(&data)?, warnings),
                text,
            ))
        }
        "pdf_check" => {
            let min_dpi = param_u32(params, "min_dpi").unwrap_or(100);
            let (data, warnings) = check::check_document(snapshot, pages, min_dpi, &bounds)?;
            let text = text_of(&warnings, check::lines(&data));
            Ok(Reply::new(
                with_warnings(serde_json::to_value(&data)?, warnings),
                text,
            ))
        }
        "pdf_images" => {
            let page = param_u32(params, "page").unwrap_or(1);
            let mode = match param_str(params, "mode").unwrap_or("inventory") {
                "inventory" => images::ImageMode::Inventory,
                "source" => images::ImageMode::Source,
                "decoded" => images::ImageMode::Decoded,
                "rendered" => images::ImageMode::Rendered,
                other => bail!("unknown image mode {other:?}"),
            };
            let extract = param_str(params, "extract").map(PathBuf::from);
            let (data, warnings) =
                images::images_value(snapshot, page, mode, extract.as_deref(), false, bounds)?;
            Ok(Reply::json(with_warnings(data, warnings)))
        }
        "pdf_content" => {
            let page = param_u32(params, "page").unwrap_or(1);
            let ops = param_bool(params, "ops").unwrap_or(true);
            let resources = param_bool(params, "resources").unwrap_or(false);
            let objects = param_bool(params, "objects").unwrap_or(false);
            let (data, warnings) =
                content::content_value(snapshot, page, ops, resources, objects, false, bounds)?;
            Ok(Reply::json(with_warnings(data, warnings)))
        }
        "pdf_render" => {
            let output = param_str(params, "output");
            if output.is_none() && !inline {
                bail!("params.output required: this transport cannot carry image bytes");
            }
            // `page` is the older, single-page spelling of `pages`.
            let single = param_u32(params, "page").map(|page| page.to_string());
            let selected = pages.or(single.as_deref()).unwrap_or("1");
            let (selection, mut warnings) =
                parse_page_range(Some(selected), snapshot.page_count(), 0)?;
            let mut spec = render_spec(params)?;
            if output.is_none() {
                // An inline image is for looking at: sized for a vision
                // model unless a size is asked for, never enormous, and in
                // whichever of PNG and JPEG is smaller unless one is named.
                let sized = spec.dpi.is_some() || spec.scale.is_some() || spec.thumbnail;
                spec.max_edge = Some(match spec.max_edge {
                    Some(edge) => edge.min(INLINE_MAX_EDGE),
                    None if sized => INLINE_MAX_EDGE,
                    None => INLINE_EDGE,
                });
                spec.prefer_small = param_str(params, "format").is_none();
            }
            let stem = identity
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("page");

            if param_bool(params, "contact_sheet").unwrap_or(false) {
                let columns = param_u32(params, "columns").unwrap_or(0);
                let sheet = render::contact_sheet(
                    snapshot,
                    &selection,
                    &spec,
                    columns,
                    false,
                    bounds.threads,
                    &mut warnings,
                )?;
                let mut path = String::new();
                if let (Some(template), Some(first)) = (output, selection.first()) {
                    path = render::expand_template(template, first.one_based(), first.0, stem);
                    render::write_image(&path, &sheet.image.bytes)?;
                }
                let data = sheet.view(path.clone());
                let line = format!(
                    "contact sheet of {} pages ({} columns x {} rows), {}x{} px {} {}; each cell carries its page number{}",
                    data.pages.len(),
                    data.columns,
                    data.rows,
                    data.width,
                    data.height,
                    data.format,
                    data.color,
                    if path.is_empty() {
                        String::new()
                    } else {
                        format!("; wrote {path}")
                    }
                );
                let text = text_of(&warnings, [line]);
                let mut reply =
                    Reply::new(with_warnings(serde_json::to_value(&data)?, warnings), text);
                if output.is_none() {
                    reply.images.push(sheet.image);
                }
                return Ok(reply);
            }

            if output.is_none() && selection.len() > INLINE_MAX_PAGES {
                bail!(
                    "{} pages selected; at most {INLINE_MAX_PAGES} come back inline per request. Use contact_sheet=true to see them all at once, select fewer pages, or give an output path",
                    selection.len()
                );
            }
            let rendered = render::render_pages(
                snapshot,
                &selection,
                &spec,
                output,
                stem,
                false,
                bounds.threads,
            )?;
            let mut out = Vec::new();
            let mut lines = Vec::new();
            let mut reply_images = Vec::new();
            for (page, result) in rendered {
                let mut entry = serde_json::json!({
                    "page": page.one_based(),
                    "page_index": page.0,
                });
                match result {
                    Ok(rendered) => {
                        let data = &rendered.data;
                        let mut line = format!(
                            "page {}: {}x{} px at {:.0} dpi, {} {}, {} bytes",
                            page.one_based(),
                            data.width,
                            data.height,
                            data.dpi,
                            data.format,
                            data.color,
                            data.bytes
                        );
                        if let Some(region) = data.region {
                            line.push_str(&format!(
                                ", region [{}]",
                                region
                                    .map(|edge| crate::views::round2(edge).to_string())
                                    .join(",")
                            ));
                        }
                        if !data.path.is_empty() {
                            line.push_str(&format!(", wrote {}", data.path));
                        }
                        if data.degraded_draws > 0 {
                            line.push_str(&format!(
                                ", {} drawing operations degraded",
                                data.degraded_draws
                            ));
                        }
                        lines.push(line);
                        lines.extend(
                            rendered.warnings.iter().map(|warning| {
                                format!("warning: p.{}: {warning}", page.one_based())
                            }),
                        );
                        entry["data"] = serde_json::to_value(data)?;
                        if !rendered.warnings.is_empty() {
                            entry["warnings"] = serde_json::json!(rendered.warnings);
                        }
                        reply_images.extend(rendered.image);
                    }
                    Err(_) if crate::deadline::expired() => {
                        let message = crate::deadline::message(out.len(), selection.len());
                        lines.push(format!(
                            "page {}: not rendered: {message}",
                            page.one_based()
                        ));
                        entry["error"] = message.into();
                    }
                    Err(error) => {
                        lines.push(format!("page {}: error: {error:#}", page.one_based()));
                        entry["error"] = format!("{error:#}").into();
                    }
                }
                out.push(entry);
            }
            // A single page that failed is the request failing.
            if let [only] = out.as_slice()
                && let Some(error) = only["error"].as_str()
            {
                bail!("{error}");
            }
            let text = text_of(&warnings, lines);
            let mut reply = Reply::new(
                serde_json::json!({ "pages": out, "warnings": warnings }),
                text,
            );
            reply.images = reply_images;
            Ok(reply)
        }
        other => bail!("unknown method {other:?}"),
    }
}

pub(crate) fn is_method(name: &str) -> bool {
    matches!(
        name,
        "pdf_inspect"
            | "pdf_map"
            | "pdf_text"
            | "pdf_search"
            | "pdf_render"
            | "pdf_tables"
            | "pdf_annotations"
            | "pdf_check"
            | "pdf_images"
            | "pdf_content"
    )
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn param_ocr(params: &Value) -> Result<text::OcrMode> {
    Ok(match param_str(params, "ocr").unwrap_or("never") {
        "never" => text::OcrMode::Never,
        "auto" => text::OcrMode::Auto,
        "always" => text::OcrMode::Always,
        other => bail!("unknown OCR mode {other:?}"),
    })
}

/// The size, region and encoding a render request asks for.
pub(crate) fn render_spec(params: &Value) -> Result<render::RenderSpec> {
    let format = match param_str(params, "format").unwrap_or("png") {
        "png" => render::ImageFormat::Png,
        "ppm" => render::ImageFormat::Ppm,
        "pgm" => render::ImageFormat::Pgm,
        "jpeg" | "jpg" => render::ImageFormat::Jpeg,
        other => bail!("unknown format {other:?}"),
    };
    let defaults = render::RenderSpec::default();
    Ok(render::RenderSpec {
        dpi: param_f64(params, "dpi"),
        scale: param_f64(params, "scale"),
        max_edge: param_u32(params, "max_edge"),
        crop: param_str(params, "crop").map(parse_bbox).transpose()?,
        thumbnail: param_bool(params, "thumbnail").unwrap_or(false),
        gray: param_bool(params, "gray").unwrap_or(false) || format == render::ImageFormat::Pgm,
        format,
        quality: param_u32(params, "quality")
            .map_or(defaults.quality, |quality| quality.min(255) as u8),
        prefer_small: false,
    })
}

/// Attach a result's warnings to it. The key is always present, so a reader
/// does not have to tell "no warnings" from "not reported".
fn with_warnings(mut data: Value, warnings: Vec<String>) -> Value {
    if let Value::Object(fields) = &mut data {
        fields.insert("warnings".into(), serde_json::json!(warnings));
    }
    data
}

fn param_path(params: &Value) -> Result<PathBuf> {
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .context("params.path required")?;
    Ok(PathBuf::from(path))
}

fn param_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str())
}

fn param_page_selection<'a>(params: &'a Value, key: &str) -> Result<Option<Cow<'a, str>>> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    if let Some(selection) = value.as_str() {
        return Ok(Some(Cow::Borrowed(selection)));
    }
    if let Some(page) = value.as_u64() {
        if page == 0 || page > u64::from(u32::MAX) {
            bail!(
                "params.{key} page number must be between 1 and {}",
                u32::MAX
            );
        }
        return Ok(Some(Cow::Owned(page.to_string())));
    }
    bail!("params.{key} must be a page-range string or one-based page number")
}

fn param_u32(params: &Value, key: &str) -> Option<u32> {
    params
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

fn param_f64(params: &Value, key: &str) -> Option<f64> {
    params.get(key).and_then(|v| v.as_f64())
}

fn param_bool(params: &Value, key: &str) -> Option<bool> {
    params.get(key).and_then(|v| v.as_bool())
}
