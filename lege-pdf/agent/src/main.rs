//! `lege-pdf` — agent-facing structured PDF tool over the native Lege engine.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lege_pdf_agent::bounds::Bounds;
use lege_pdf_agent::commands::{
    annotations, check, content, images, inspect, map, matching, mcp, poppler, print, render,
    search, serve, tables, text,
};
use lege_pdf_agent::deadline;
use lege_pdf_agent::schema::{Envelope, OutputMode, Status};

#[derive(Debug, Parser)]
#[command(
    name = "lege-pdf",
    version,
    about = "Agent-facing structured PDF inspection, text, images, content, render, search, and serve",
    after_help = "Poppler's command lines are answered too:
  lege-pdf pdftotext [options] <PDF-file> [<text-file>]
  lege-pdf pdftoppm [options] <PDF-file> [<PPM-file-prefix>]
See `lege-pdf pdftotext -h` and `lege-pdf pdftoppm -h`."
)]
struct Cli {
    /// User/owner password for encrypted documents (never printed).
    #[arg(long, global = true)]
    password: Option<String>,

    /// Emit a single pretty-printed JSON envelope on stdout.
    #[arg(long, global = true, conflicts_with = "jsonl")]
    json: bool,

    /// Emit JSONL records on stdout (one per page for multi-page ops).
    #[arg(long, global = true, conflicts_with = "json")]
    jsonl: bool,

    /// Fail the process on the first page-local error.
    #[arg(long, global = true)]
    fail_fast: bool,

    /// Maximum pages processed per invocation (0 = unlimited). Defaults to
    /// 50 for commands whose output grows with every page (text, images,
    /// render, inspect --pages); commands that scan and summarise (search,
    /// map, check, annotations, tables) cover every selected page unless
    /// this is given.
    #[arg(long, global = true)]
    max_pages: Option<u32>,

    /// Maximum items (words/ops/images/matches) emitted per page.
    #[arg(long, global = true, default_value_t = Bounds::default().max_items)]
    max_items: u32,

    /// Size cap in bytes for one text record, and for one `serve`/`mcp`
    /// response; what does not fit is cut and reported (0 = none).
    #[arg(long, global = true, default_value_t = Bounds::default().max_bytes)]
    max_bytes: u64,

    /// Wall-clock budget in seconds for the command, or for each request to
    /// `serve`/`mcp` (0 = none). Pages finished before it runs out are kept.
    #[arg(long, global = true, default_value_t = Bounds::default().timeout_secs)]
    timeout: u64,

    /// Worker threads for multi-page work (0 = one per available core).
    #[arg(long, global = true, default_value_t = 0)]
    threads: u32,

    /// Resolve non-embedded fonts against system fonts (non-deterministic).
    #[arg(long, global = true)]
    system_fonts: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Document health, boxes, encryption, features, and page compile status.
    Inspect {
        file: PathBuf,
        /// Also report boxes and compile status for these pages (e.g.
        /// 1,3-5,all). Without it only the document summary is printed.
        #[arg(long)]
        pages: Option<String>,
        /// Outline items listed (0 = all).
        #[arg(long, default_value_t = 200)]
        outline_limit: usize,
    },
    /// One line per page: size, printed page number, whether it has a text
    /// layer or is a scan, how much text, how many images, its first line.
    Map {
        file: PathBuf,
        #[arg(long)]
        pages: Option<String>,
    },
    /// Extract page text with optional geometry.
    Text {
        file: PathBuf,
        #[arg(long)]
        pages: Option<String>,
        #[arg(long, value_enum, default_value_t = text::TextLayout::Plain)]
        layout: text::TextLayout,
        /// PDF-space region X0,Y0,X1,Y1.
        #[arg(long)]
        bbox: Option<String>,
        #[arg(long)]
        rtl: bool,
        #[arg(long)]
        no_normalize: bool,
        #[arg(long)]
        no_include_hidden: bool,
        #[arg(long)]
        include_annotations: bool,
        /// OCR policy. `auto` OCRs only pages whose native text is not trustworthy.
        #[arg(long, value_enum, default_value_t = text::OcrMode::Never)]
        ocr: text::OcrMode,
        /// OCR language code understood by the compiled Lege OCR backend.
        #[arg(long, default_value = "eng")]
        ocr_language: String,
    },
    /// Inventory or extract page images.
    Images {
        file: PathBuf,
        #[arg(long)]
        pages: Option<String>,
        #[arg(long, value_enum, default_value_t = images::ImageMode::Inventory)]
        mode: images::ImageMode,
        /// Directory for source/decoded/rendered extraction.
        #[arg(long)]
        extract: Option<PathBuf>,
    },
    /// Semantic content dump / typed ops / resources for one page.
    Content {
        file: PathBuf,
        /// One-based page number.
        #[arg(long)]
        page: u32,
        #[arg(long)]
        ops: bool,
        #[arg(long)]
        resources: bool,
        #[arg(long)]
        objects: bool,
    },
    /// Rasterize pages to PNG or PPM.
    Render {
        file: PathBuf,
        #[arg(long)]
        pages: Option<String>,
        /// Output path template: supports {page}, {page_index}, {stem}.
        #[arg(long)]
        output: String,
        #[arg(long)]
        dpi: Option<f64>,
        #[arg(long)]
        scale: Option<f64>,
        #[arg(long, value_enum, default_value_t = render::ImageFormat::Png)]
        format: render::ImageFormat,
        /// PDF-space region X0,Y0,X1,Y1 to render instead of the whole page.
        #[arg(long)]
        crop: Option<String>,
        /// Fit the longest edge to this many pixels. On its own it sets the
        /// size; with --dpi or --scale it only caps it.
        #[arg(long)]
        max_edge: Option<u32>,
        /// Draft quality at 72 dpi, longest edge capped at 512 pixels.
        #[arg(long)]
        thumbnail: bool,
        /// Render in grayscale.
        #[arg(long)]
        gray: bool,
        /// JPEG quality, 1-100.
        #[arg(long, default_value_t = 85)]
        quality: u8,
        /// Tile the selected pages into one image, each under its page
        /// number. --max-edge then sizes the whole sheet.
        #[arg(long)]
        contact_sheet: bool,
        /// Columns of a contact sheet (default: a near-square grid).
        #[arg(long, default_value_t = 0, requires = "contact_sheet")]
        columns: u32,
    },
    /// Search text across pages.
    Search {
        file: PathBuf,
        query: String,
        #[arg(long)]
        pages: Option<String>,
        /// Characters of surrounding text shown with each match.
        #[arg(long, default_value_t = 60)]
        context: usize,
        /// Match case exactly.
        #[arg(long, alias = "case-sensitive")]
        no_case_insensitive: bool,
        /// Treat the query as a regular expression (Rust `regex` syntax).
        /// It sees each page as one line, with runs of whitespace as single
        /// spaces and ligatures as their letters.
        #[arg(long, short = 'e', conflicts_with = "exact")]
        regex: bool,
        /// Match character for character. By default a phrase also matches
        /// across line breaks, hyphenation, ligatures and typographic quotes.
        #[arg(long)]
        exact: bool,
        /// Stop after this many matches in total (0 = no limit).
        #[arg(long, default_value_t = 0)]
        max_matches: usize,
        /// OCR policy for pages without trustworthy native text.
        #[arg(long, value_enum, default_value_t = text::OcrMode::Never)]
        ocr: text::OcrMode,
        #[arg(long, default_value = "eng")]
        ocr_language: String,
    },
    /// Links (with the text they sit on and where they go), comments and
    /// form fields. With none of --links/--comments/--forms, all three.
    Annotations {
        file: PathBuf,
        #[arg(long)]
        pages: Option<String>,
        #[arg(long)]
        links: bool,
        #[arg(long)]
        comments: bool,
        #[arg(long)]
        forms: bool,
    },
    /// Preflight a PDF: failed or half-drawn pages, text that is cut off or
    /// cannot be searched, fonts not embedded, coarse images, blank pages.
    /// Exits 1 when there are errors.
    Check {
        file: PathBuf,
        #[arg(long)]
        pages: Option<String>,
        /// Report images below this many pixels per inch on the page.
        #[arg(long, default_value_t = 100)]
        min_dpi: u32,
        /// Exit 1 on warnings too.
        #[arg(long)]
        strict: bool,
    },
    /// Find tables from the alignment of text and print their cells.
    Tables {
        file: PathBuf,
        #[arg(long)]
        pages: Option<String>,
        /// How tables are written without --json/--jsonl.
        #[arg(long, value_enum, default_value_t = tables::TableFormat::Markdown)]
        format: tables::TableFormat,
        /// OCR policy for pages without trustworthy native text.
        #[arg(long, value_enum, default_value_t = text::OcrMode::Never)]
        ocr: text::OcrMode,
        #[arg(long, default_value = "eng")]
        ocr_language: String,
    },
    /// Print a document, or plan the job without spooling it.
    Print {
        /// The PDF to print. Optional only with `--list-printers`.
        #[arg(required_unless_present = "list_printers")]
        file: Option<PathBuf>,
        /// Target queue. Defaults to the system default printer.
        #[arg(long)]
        printer: Option<String>,
        /// Enumerate print queues and exit.
        #[arg(long)]
        list_printers: bool,
        /// One-based page selection: 1,3-5 / odd / even / all.
        #[arg(long)]
        pages: Option<String>,
        /// Paper: a4, letter, … or an explicit 210x297mm / 8.5x11in / 612x792pt.
        #[arg(long)]
        paper: Option<String>,
        #[arg(long, value_enum, default_value_t = print::OrientationArg::Auto)]
        orientation: print::OrientationArg,
        /// Uniform user margin in points.
        #[arg(long, conflicts_with_all = ["margin_mm", "margin_in"])]
        margin: Option<f64>,
        /// Uniform user margin in millimetres.
        #[arg(long, conflicts_with = "margin_in")]
        margin_mm: Option<f64>,
        /// Uniform user margin in inches.
        #[arg(long)]
        margin_in: Option<f64>,
        /// Left-edge margin in points, overriding the uniform margin.
        #[arg(long)]
        margin_left: Option<f64>,
        /// Right-edge margin in points, overriding the uniform margin.
        #[arg(long)]
        margin_right: Option<f64>,
        /// Top-edge margin in points, overriding the uniform margin.
        #[arg(long)]
        margin_top: Option<f64>,
        /// Bottom-edge margin in points, overriding the uniform margin.
        #[arg(long)]
        margin_bottom: Option<f64>,
        /// actual | fit | shrink | fill | NN%
        #[arg(long)]
        scaling: Option<String>,
        /// 1 | 2 | 4 | 6 | 9 | 16 | booklet
        #[arg(long)]
        n_up: Option<String>,
        #[arg(long, value_enum, default_value_t = print::NUpOrderArg::RightDown)]
        n_up_order: print::NUpOrderArg,
        /// Draw a hairline around each N-up cell.
        #[arg(long)]
        n_up_border: bool,
        #[arg(long, value_enum, default_value_t = print::DuplexArg::None)]
        duplex: print::DuplexArg,
        #[arg(long, default_value_t = 1)]
        copies: u16,
        #[arg(long)]
        no_collate: bool,
        /// Print the selected pages last-to-first.
        #[arg(long)]
        reverse: bool,
        #[arg(long, value_enum, default_value_t = print::SourceBoxArg::Crop)]
        source_box: print::SourceBoxArg,
        /// Compose in grayscale, or ask the driver for mono on pass-through.
        #[arg(long)]
        gray: bool,
        /// Composition resolution. Ignored on the pass-through route.
        #[arg(long)]
        dpi: Option<f64>,
        /// Spool with the file backend into this directory; no real printer.
        #[arg(long)]
        to_file: Option<PathBuf>,
        /// Report the plan as JSON and spool nothing.
        #[arg(long)]
        dry_run: bool,
        /// In a dry run, ask the real queue for its capabilities instead of
        /// assuming a conservative device.
        #[arg(long, requires = "dry_run")]
        query_device: bool,
    },
    /// Persistent stdio JSONL service with snapshot cache.
    Serve {
        #[arg(long, default_value_t = true)]
        stdio: bool,
        #[arg(long, default_value_t = 4)]
        max_open: usize,
        #[arg(long, default_value_t = 300)]
        idle_timeout: u64,
    },
    /// Run a Model Context Protocol server over stdio.
    Mcp {
        #[arg(long, default_value_t = 4)]
        max_open: usize,
        #[arg(long, default_value_t = 300)]
        idle_timeout: u64,
    },
}

/// `lege-pdf pdftotext …`, `lege-pdf pdftoppm …`, or this executable under
/// one of those names: Poppler's command line, not ours.
fn poppler_request() -> Option<(poppler::Tool, Vec<String>)> {
    // Not `args()`: an argument that is not Unicode must not abort the run.
    let mut args = std::env::args_os().map(|arg| arg.to_string_lossy().into_owned());
    let program = args.next()?;
    let name = std::path::Path::new(&program).file_name()?.to_str()?;
    if let Some(tool) = poppler::tool_named(name) {
        return Some((tool, args.collect()));
    }
    let tool = poppler::tool_named(&args.next()?)?;
    Some((tool, args.collect()))
}

fn main() -> ExitCode {
    if let Some((tool, args)) = poppler_request() {
        return ExitCode::from(poppler::run(tool, &args) as u8);
    }
    let cli = Cli::parse();
    let bounds = Bounds {
        max_pages: cli.max_pages.unwrap_or(Bounds::default().max_pages),
        scan_pages: cli.max_pages.unwrap_or(0),
        max_items: cli.max_items,
        max_bytes: cli.max_bytes,
        timeout_secs: cli.timeout,
        threads: cli.threads,
    };
    let output = if cli.jsonl {
        OutputMode::Jsonl
    } else if cli.json {
        OutputMode::Json
    } else {
        OutputMode::Human
    };
    let password = cli.password.as_deref();

    // The document the command is about, for an error record. `print` plans
    // and spools on its own terms and the services arm a budget per request.
    let (document, timed) = match &cli.command {
        Command::Inspect { file, .. }
        | Command::Map { file, .. }
        | Command::Text { file, .. }
        | Command::Images { file, .. }
        | Command::Content { file, .. }
        | Command::Render { file, .. }
        | Command::Search { file, .. }
        | Command::Tables { file, .. }
        | Command::Annotations { file, .. }
        | Command::Check { file, .. } => (Some(file.display().to_string()), true),
        Command::Print { file, .. } => {
            (file.as_ref().map(|file| file.display().to_string()), false)
        }
        Command::Serve { .. } | Command::Mcp { .. } => (None, false),
    };
    let _deadline = timed.then(|| deadline::arm(cli.timeout));

    let result = match cli.command {
        Command::Map { file, pages } => map::run(map::MapArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            system_fonts: cli.system_fonts,
            bounds,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Inspect {
            file,
            pages,
            outline_limit,
        } => inspect::run(inspect::InspectArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            outline_limit,
            bounds,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Text {
            file,
            pages,
            layout,
            bbox,
            rtl,
            no_normalize,
            no_include_hidden,
            include_annotations,
            ocr,
            ocr_language,
        } => text::run(text::TextArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            layout,
            bbox: bbox.as_deref(),
            rtl,
            normalize: !no_normalize,
            include_hidden: !no_include_hidden,
            include_annotations,
            ocr,
            ocr_language: &ocr_language,
            system_fonts: cli.system_fonts,
            bounds,
            fail_fast: cli.fail_fast,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Images {
            file,
            pages,
            mode,
            extract,
        } => images::run(images::ImagesArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            mode,
            extract_dir: extract.as_deref(),
            system_fonts: cli.system_fonts,
            bounds,
            fail_fast: cli.fail_fast,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Content {
            file,
            page,
            ops,
            resources,
            objects,
        } => content::run(content::ContentArgs {
            path: &file,
            password,
            page,
            ops,
            resources,
            objects,
            system_fonts: cli.system_fonts,
            bounds,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Render {
            file,
            pages,
            output: out_template,
            dpi,
            scale,
            format,
            crop,
            max_edge,
            thumbnail,
            gray,
            quality,
            contact_sheet,
            columns,
        } => crop
            .as_deref()
            .map(lege_pdf_agent::pages::parse_bbox)
            .transpose()
            .and_then(|crop| {
                render::run(render::RenderArgs {
                    path: &file,
                    password,
                    pages: pages.as_deref(),
                    output: &out_template,
                    spec: render::RenderSpec {
                        dpi,
                        scale,
                        max_edge,
                        crop,
                        thumbnail,
                        gray: gray || format == render::ImageFormat::Pgm,
                        format,
                        quality,
                        prefer_small: false,
                    },
                    contact_sheet,
                    columns,
                    system_fonts: cli.system_fonts,
                    bounds,
                    fail_fast: cli.fail_fast,
                    output_mode: output,
                    snapshot: None,
                    identity: None,
                })
            }),
        Command::Search {
            file,
            query,
            pages,
            context,
            no_case_insensitive,
            regex,
            exact,
            max_matches,
            ocr,
            ocr_language,
        } => search::run(search::SearchArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            spec: search::SearchSpec {
                query: &query,
                options: matching::QueryOptions {
                    regex,
                    case_insensitive: !no_case_insensitive,
                    exact,
                },
                context,
                max_matches,
                ocr,
                ocr_language: &ocr_language,
                system_fonts: cli.system_fonts,
                bounds,
            },
            fail_fast: cli.fail_fast,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Annotations {
            file,
            pages,
            links,
            comments,
            forms,
        } => annotations::run(annotations::AnnotationsArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            kinds: annotations::Kinds::from_flags(links, comments, forms),
            bounds,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Check {
            file,
            pages,
            min_dpi,
            strict,
        } => check::run(check::CheckArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            min_dpi,
            strict,
            bounds,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Tables {
            file,
            pages,
            format,
            ocr,
            ocr_language,
        } => tables::run(tables::TablesArgs {
            path: &file,
            password,
            pages: pages.as_deref(),
            format,
            ocr,
            ocr_language: &ocr_language,
            system_fonts: cli.system_fonts,
            bounds,
            fail_fast: cli.fail_fast,
            output,
            snapshot: None,
            identity: None,
        }),
        Command::Print {
            file,
            printer,
            list_printers,
            pages,
            paper,
            orientation,
            margin,
            margin_mm,
            margin_in,
            margin_left,
            margin_right,
            margin_top,
            margin_bottom,
            scaling,
            n_up,
            n_up_order,
            n_up_border,
            duplex,
            copies,
            no_collate,
            reverse,
            source_box,
            gray,
            dpi,
            to_file,
            dry_run,
            query_device,
        } => print::run(print::PrintArgs {
            path: file.as_deref(),
            password,
            printer: printer.as_deref(),
            list_printers,
            pages: pages.as_deref(),
            paper: paper.as_deref(),
            orientation,
            margin,
            margin_mm,
            margin_in,
            margin_left,
            margin_right,
            margin_top,
            margin_bottom,
            scaling: scaling.as_deref(),
            n_up: n_up.as_deref(),
            n_up_order,
            n_up_border,
            duplex,
            copies,
            no_collate,
            reverse,
            source_box,
            gray,
            dpi,
            to_file: to_file.as_deref(),
            dry_run,
            query_device,
            bounds,
            output,
        }),
        Command::Serve {
            stdio,
            max_open,
            idle_timeout,
        } => {
            if !stdio {
                eprintln!("only --stdio is supported in this release");
                return ExitCode::from(2);
            }
            serve::run(serve::ServeArgs {
                max_open,
                idle_timeout,
                bounds,
            })
        }
        Command::Mcp {
            max_open,
            idle_timeout,
        } => mcp::run(mcp::McpArgs {
            max_open,
            idle_timeout,
            bounds,
        }),
    };

    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            let message = if deadline::expired() {
                format!("timeout of {}s reached: {err:#}", cli.timeout)
            } else {
                format!("{err:#}")
            };
            eprintln!("error: {message}");
            // A caller that asked for JSON parses stdout: give it a record
            // for a failure that happened before any page was reached.
            if let (Some(document), false) = (document, matches!(output, OutputMode::Human)) {
                let env = Envelope::ok(document, serde_json::json!({ "error": message }))
                    .set_status(Status::Failed);
                let written = match output {
                    OutputMode::Jsonl => env.write_jsonl(),
                    _ => env.write_json(),
                };
                if let Err(err) = written {
                    eprintln!("error: writing the failure record: {err:#}");
                }
            }
            ExitCode::from(1)
        }
    }
}
