//! Model Context Protocol adapter over the persistent PDF service.
//!
//! A tool result here is written for the model that reads it. By default it
//! is plain text — page text as text, matches as one line each, a page as an
//! image — because that is what the caller wants to read and JSON around it
//! is paid for in tokens twice over. `response_format: "json"` returns the
//! structured result instead, with the geometry, for a caller that will
//! compute on it.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::bounds::Bounds;
use crate::cache::SnapshotCache;

use super::serve;

const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";
const SERVER_INSTRUCTIONS: &str = r#"Lege PDF reads local PDF files. Paths are resolved on this host; page numbers are one-based.

Usual order: pdf_inspect (pages, outline, metadata, whether there is a text layer) -> pdf_search or pdf_map to find the pages that matter -> pdf_text to read them -> pdf_render to look at a page or a region of one (figures, tables, formulas, scans, layout). pdf_tables, pdf_annotations (links, comments, form fields) and pdf_check (is this PDF sound) answer narrower questions.

Results are plain text; pass response_format="json" for structured data with geometry. Every result says what it covered: when a limit cuts it short, it names the limit and the pages (and start_char) to continue from, so read that line rather than assuming the whole document was returned. Boxes are [x0,y0,x1,y1] in PDF points with the origin at the bottom left; a box from pdf_search or pdf_text can be passed as crop to pdf_render or bbox to pdf_text. Pages reported as scanned-image have no text layer: pass ocr="auto".

If a result is wrong or unusable (a tool error, a degraded render, text that is visibly on the page but missing, an unreadable reading order), use another PDF tool for that file, page and operation only, and report it: when the current repository has a .akr directory, run `akr papercut -m <your agent name> --about lege-pdf "tool=<tool>; file=<basename>; page=<n>; observed=<what happened>; fallback=<what you used>"`; otherwise tell the user. Leave document contents, passwords and private paths out of the report. Do not re-check a result that looks right with another tool."#;

#[derive(Debug)]
pub struct McpArgs {
    pub max_open: usize,
    pub idle_timeout: u64,
    pub bounds: Bounds,
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Run the newline-delimited MCP stdio transport until the client closes stdin.
pub fn run(args: McpArgs) -> Result<i32> {
    let mut cache = SnapshotCache::new(args.max_open, args.idle_timeout);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line.context("reading MCP stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(request) => request,
            Err(err) => {
                write_message(
                    &mut stdout,
                    rpc_error(Value::Null, -32700, format!("parse error: {err}")),
                )?;
                continue;
            }
        };

        if request.jsonrpc.as_deref() != Some("2.0") {
            if let Some(id) = request.id {
                write_message(
                    &mut stdout,
                    rpc_error(id, -32600, "jsonrpc must be \"2.0\""),
                )?;
            }
            continue;
        }

        // JSON-RPC notifications never receive responses. MCP initialization and
        // cancellation notifications do not require state in this synchronous server.
        let Some(id) = request.id else {
            continue;
        };

        let response = match request.method.as_str() {
            "initialize" => rpc_result(id, initialize_result(&request.params)),
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, json!({ "tools": tools() })),
            "tools/call" => match call_tool(&mut cache, &request.params, args.bounds) {
                Ok(value) => rpc_result(id, value),
                Err((code, message)) => rpc_error(id, code, message),
            },
            _ => rpc_error(id, -32601, format!("method not found: {}", request.method)),
        };
        write_message(&mut stdout, response)?;
    }

    Ok(0)
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let protocol_version = requested
        .filter(|version| {
            matches!(
                *version,
                "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
            )
        })
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);

    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "lege-pdf",
            "title": "Lege PDF",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": SERVER_INSTRUCTIONS
    })
}

fn call_tool(
    cache: &mut SnapshotCache,
    params: &Value,
    bounds: Bounds,
) -> std::result::Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "params.name must be a string".to_owned()))?;
    if !serve::is_method(name) {
        return Err((-32602, format!("unknown tool {name:?}")));
    }

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let structured = validate_arguments(name, &arguments)?;

    match serve::call_method(cache, name, &arguments, bounds, true) {
        Ok(reply) => {
            let mut content = Vec::with_capacity(1 + reply.images.len());
            let mut result = json!({ "isError": false });
            if structured {
                content.push(json!({ "type": "text", "text": reply.value.to_string() }));
                result["structuredContent"] = reply.value;
            } else {
                content.push(json!({ "type": "text", "text": reply.text }));
            }
            for image in reply.images {
                content.push(json!({
                    "type": "image",
                    "data": base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                    "mimeType": image.format.mime(),
                }));
            }
            result["content"] = Value::Array(content);
            Ok(result)
        }
        Err(err) => Ok(json!({
            "content": [{ "type": "text", "text": format!("{err:#}") }],
            "isError": true
        })),
    }
}

/// Check what can be checked without opening the document, and return
/// whether the caller asked for the structured form of the result.
fn validate_arguments(name: &str, arguments: &Value) -> std::result::Result<bool, (i32, String)> {
    let object = arguments
        .as_object()
        .ok_or_else(|| (-32602, "params.arguments must be an object".to_owned()))?;
    if object.get("path").and_then(Value::as_str).is_none() {
        return Err((-32602, "arguments.path must be a string".to_owned()));
    }
    if name == "pdf_search" && object.get("query").and_then(Value::as_str).is_none() {
        return Err((-32602, "arguments.query must be a string".to_owned()));
    }
    match object.get("response_format").map(Value::as_str) {
        None | Some(Some("text")) => Ok(false),
        Some(Some("json")) => Ok(true),
        Some(_) => Err((
            -32602,
            "arguments.response_format must be \"text\" or \"json\"".to_owned(),
        )),
    }
}

fn tools() -> Vec<Value> {
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    let writes = json!({ "readOnlyHint": false, "openWorldHint": false });
    vec![
        json!({
            "name": "pdf_inspect",
            "title": "Inspect PDF",
            "description": "What a PDF is, in one call: page count, metadata, page sizes, printed page numbering, outline (bookmarks) with page numbers, whether pages have a text layer or are scans, encryption, forms, and file damage. Start here. Pass pages to also get boxes and render status for specific pages.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "pages": pages_schema("Pages to report in detail (boxes, rotation, whether each draws cleanly). Omit for the document summary alone."),
                    "outline_limit": { "type": "integer", "minimum": 0, "default": 200, "description": "Outline items listed; 0 lists all." },
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path"]
            ),
            "annotations": read_only
        }),
        json!({
            "name": "pdf_map",
            "title": "Map PDF pages",
            "description": "One line per page: size, printed page number, kind (native-text, hybrid, scanned-image needing OCR, render-required, blank), how much text, how many images, and the first line of text. Use it to find chapters, figures-heavy or scanned pages before reading. Covers every selected page.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "pages": pages_schema("Pages to map. Default: all."),
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path"]
            ),
            "annotations": read_only
        }),
        json!({
            "name": "pdf_text",
            "title": "Read PDF text",
            "description": "The text of one or more pages, in reading order, each page under a `--- page N ---` line. Returns up to max_chars characters; if cut, the first line says which pages (and start_char) to ask for next. layout=physical keeps columns and tables aligned on a character grid; layout=blocks groups paragraphs; words/chars return JSON with a box per word or character. Scanned pages need ocr=\"auto\".",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "pages": pages_schema("Pages to read. Default: from page 1 on, as far as max_chars reaches."),
                    "layout": { "type": "string", "enum": ["plain", "physical", "blocks", "words", "chars"], "default": "plain" },
                    "bbox": { "type": "string", "description": "Only text inside this PDF-space rectangle X0,Y0,X1,Y1." },
                    "max_chars": { "type": "integer", "minimum": 0, "default": 40000, "description": "Budget for the whole response in characters of text; 0 for none." },
                    "start_char": { "type": "integer", "minimum": 0, "default": 0, "description": "Skip this many characters of the first selected page: the continuation point a cut response reports." },
                    "ocr": ocr_schema(),
                    "ocr_language": ocr_language_schema(),
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path"]
            ),
            "annotations": read_only
        }),
        json!({
            "name": "pdf_search",
            "title": "Search PDF text",
            "description": "Find a phrase or regular expression in the whole document (or pages). One line per match: page, box, and the text around it; pages without matches are only counted. A phrase matches across line breaks, hyphenation, ligatures and typographic quotes unless exact=true. The box can be passed to pdf_render as crop to see the match in place.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "query": { "type": "string", "minLength": 1 },
                    "regex": { "type": "boolean", "default": false, "description": "Treat query as a regular expression (Rust regex syntax). Each page is one line of text with single spaces." },
                    "exact": { "type": "boolean", "default": false, "description": "Match character for character, without folding whitespace, hyphens, ligatures or quotes." },
                    "case_insensitive": { "type": "boolean", "default": true },
                    "pages": pages_schema("Pages to search. Default: all."),
                    "context": { "type": "integer", "minimum": 0, "default": 60, "description": "Characters of text shown on each side of a match." },
                    "max_matches": { "type": "integer", "minimum": 0, "default": 50, "description": "Stop after this many matches; 0 for no limit. A stopped search says where to resume." },
                    "ocr": ocr_schema(),
                    "ocr_language": ocr_language_schema(),
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path", "query"]
            ),
            "annotations": read_only
        }),
        json!({
            "name": "pdf_render",
            "title": "Look at PDF pages",
            "description": "Render pages as images. Without output the images come back inline, sized for viewing (longest edge 1568 px); with output they are written to that path. Up to 6 pages inline per call; contact_sheet=true tiles many pages into one numbered overview image. crop renders just a region, magnified to max_edge: use it to read a figure, table, formula or small print.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "pages": pages_schema("Pages to render. Default: 1."),
                    "crop": { "type": "string", "description": "PDF-space rectangle X0,Y0,X1,Y1 to render instead of the whole page." },
                    "max_edge": { "type": "integer", "minimum": 1, "description": "Longest edge in pixels. Alone it sets the size; with dpi or scale it caps it. For a contact sheet it sizes the whole sheet." },
                    "dpi": { "type": "number", "exclusiveMinimum": 0 },
                    "scale": { "type": "number", "exclusiveMinimum": 0 },
                    "contact_sheet": { "type": "boolean", "default": false, "description": "One image of all selected pages (up to 64), each under its page number." },
                    "columns": { "type": "integer", "minimum": 0, "description": "Columns of a contact sheet; default a near-square grid." },
                    "gray": { "type": "boolean", "default": false },
                    "format": { "type": "string", "enum": ["png", "jpeg", "ppm"], "description": "Image format. Default: inline, PNG or JPEG, whichever is smaller for the page; to a file, PNG." },
                    "quality": { "type": "integer", "minimum": 1, "maximum": 100, "default": 85, "description": "JPEG quality." },
                    "thumbnail": { "type": "boolean", "default": false, "description": "Draft at 72 dpi, longest edge at most 512 px." },
                    "output": { "type": "string", "description": "Write to this path instead of returning the image; prefer an absolute path. Supports {page}, {page_index} and {stem}, needed when several pages are written." },
                    "page": { "type": "integer", "minimum": 1, "description": "Deprecated: one page, same as pages." },
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path"]
            ),
            "annotations": writes
        }),
        json!({
            "name": "pdf_tables",
            "title": "Extract PDF tables",
            "description": "Tables found from how text is aligned, as Markdown, each with its page and box. Works for tables laid out with clear space between columns, ruled or not; a wrapped cell comes back as a row of its own with the other cells empty. When a table looks wrong, pdf_text layout=physical on its box or pdf_render with crop shows what is really there.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "pages": pages_schema("Pages to look at. Default: all."),
                    "ocr": ocr_schema(),
                    "ocr_language": ocr_language_schema(),
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path"]
            ),
            "annotations": read_only
        }),
        json!({
            "name": "pdf_annotations",
            "title": "Links, comments and form fields",
            "description": "What is in a PDF besides its page content: links with the text they sit on and the page or URL they go to, comments and highlights with author and text, and form fields with their names and current values. With none of links/comments/forms set, all three.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "pages": pages_schema("Pages to look at. Default: all."),
                    "links": { "type": "boolean", "default": false },
                    "comments": { "type": "boolean", "default": false },
                    "forms": { "type": "boolean", "default": false },
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path"]
            ),
            "annotations": read_only
        }),
        json!({
            "name": "pdf_check",
            "title": "Check a PDF is sound",
            "description": "Preflight, for a PDF you just generated or one that misbehaves: pages that fail or only partly draw, text cut off at the page edge, text that cannot be searched or copied, fonts not embedded, low-resolution images, blank pages, pages with no text layer, file damage. Each finding names its pages; render those to see the problem.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "pages": pages_schema("Pages to check. Default: all."),
                    "min_dpi": { "type": "integer", "minimum": 1, "default": 100, "description": "Report images below this resolution on the page." },
                    "password": password_schema(),
                    "response_format": response_format_schema()
                }),
                &["path"]
            ),
            "annotations": read_only
        }),
        json!({
            "name": "pdf_images",
            "title": "Inventory or extract embedded images",
            "description": "The images embedded in one page (size, color space, codec, where each is drawn), as JSON; or, with a mode other than inventory and an extract directory, the image files themselves. To look at a page, use pdf_render instead.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "page": page_schema(),
                    "mode": { "type": "string", "enum": ["inventory", "source", "decoded", "rendered"], "default": "inventory" },
                    "extract": { "type": "string", "description": "Output directory required by the extraction modes; prefer an absolute path." },
                    "password": password_schema()
                }),
                &["path"]
            ),
            "annotations": writes
        }),
        json!({
            "name": "pdf_content",
            "title": "Inspect PDF page internals",
            "description": "The drawing operations, resources and objects of one page, as JSON. For debugging how a page is built, not for reading it.",
            "inputSchema": object_schema(
                json!({
                    "path": path_schema(),
                    "page": page_schema(),
                    "ops": { "type": "boolean", "default": true },
                    "resources": { "type": "boolean", "default": false },
                    "objects": { "type": "boolean", "default": false },
                    "password": password_schema()
                }),
                &["path"]
            ),
            "annotations": read_only
        }),
    ]
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn path_schema() -> Value {
    json!({
        "type": "string",
        "description": "PDF path visible to the local server process; prefer an absolute path."
    })
}

fn pages_schema(what: &str) -> Value {
    json!({
        "description": format!("{what} One-based: an integer, or a string such as \"3\", \"1-5\", \"1,4-6\", \"10-\" or \"all\"."),
        "oneOf": [
            { "type": "integer", "minimum": 1 },
            { "type": "string" }
        ]
    })
}

fn page_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "default": 1,
        "description": "One-based page number."
    })
}

fn response_format_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["text", "json"],
        "default": "text",
        "description": "text: written to be read. json: the structured result, with geometry."
    })
}

fn ocr_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["never", "auto", "always"],
        "default": "never",
        "description": "auto runs OCR on pages whose own text is missing or untrustworthy (scans); always ignores the page's own text."
    })
}

fn ocr_language_schema() -> Value {
    json!({
        "type": "string",
        "default": "eng",
        "description": "Language code for OCR, for example eng."
    })
}

fn password_schema() -> Value {
    json!({
        "type": "string",
        "description": "Document password, if it has one. Never returned."
    })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn write_message(out: &mut impl Write, value: Value) -> Result<()> {
    serde_json::to_writer(&mut *out, &value)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
