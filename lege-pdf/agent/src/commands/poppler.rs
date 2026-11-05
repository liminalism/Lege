//! Poppler's command lines, answered by this engine.
//!
//! `lege-pdf pdftotext …` and `lege-pdf pdftoppm …` take the options of the
//! Poppler tools of those names, write the files those tools write under the
//! names they give them, and return their exit codes — so a script, or a
//! habit, built on Poppler runs unchanged. The same happens when the
//! executable itself is named `pdftotext` or `pdftoppm`.
//!
//! Pages are worked on in parallel and written in order. An option this
//! engine has no equivalent for is refused by name rather than ignored, so a
//! caller never gets output that silently differs from what it asked for;
//! `-h` lists what is taken and what differs.

use std::collections::HashMap;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::Path;

use pdf_document::{DocumentSnapshot, PageIndex};
use pdf_render_cpu::{CpuBackend, CpuBackendOptions};
use pdf_text::TextPageOptions;

use crate::bounds::Bounds;
use crate::commands::render::{self, ImageFormat, RenderSpec};
use crate::commands::text::{self, OcrMode, TextLayout};
use crate::commands::{PageOutcome, for_pages, page_compiler, resolve_snapshot};
use crate::pages::PageZero;

/// A Poppler tool this binary answers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    PdfToText,
    PdfToPpm,
}

/// The tool `name` asks for: a first argument, or the executable's own name.
pub fn tool_named(name: &str) -> Option<Tool> {
    let name = name.to_ascii_lowercase();
    match name.strip_suffix(".exe").unwrap_or(&name) {
        "pdftotext" => Some(Tool::PdfToText),
        "pdftoppm" => Some(Tool::PdfToPpm),
        _ => None,
    }
}

// Poppler's exit codes.
const DONE: i32 = 0;
const CANNOT_OPEN: i32 = 1;
const CANNOT_WRITE: i32 = 2;
const OTHER: i32 = 99;

#[derive(Debug)]
struct Failure {
    code: i32,
    message: String,
}

fn failure(code: i32, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

/// Run `tool` on its arguments (without the program or tool name) and
/// return the process exit code.
pub fn run(tool: Tool, args: &[String]) -> i32 {
    let outcome = match tool {
        Tool::PdfToText => pdftotext(args),
        Tool::PdfToPpm => pdftoppm(args),
    };
    match outcome {
        Ok(code) => code,
        Err(Failure { code, message }) => {
            eprintln!("{message}");
            code
        }
    }
}

// ── arguments ────────────────────────────────────────────────────────────

/// Whether an option is followed by a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Takes {
    Nothing,
    Value,
}
use Takes::{Nothing, Value};

#[derive(Debug, Default)]
struct Parsed {
    options: HashMap<&'static str, String>,
    positional: Vec<String>,
}

impl Parsed {
    fn has(&self, name: &str) -> bool {
        self.options.contains_key(name)
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    fn number<T: std::str::FromStr>(&self, tool: &str, name: &str) -> Result<Option<T>, Failure> {
        self.text(name)
            .map(|raw| {
                raw.parse::<T>().map_err(|_| {
                    failure(OTHER, format!("{tool}: {name} takes a number, not {raw:?}"))
                })
            })
            .transpose()
    }
}

/// Split `args` into the options of `table` and the remaining arguments.
/// Like Poppler, options may come before, between or after the file names,
/// and a lone `-` is a file name (standard output).
fn parse(tool: &str, args: &[String], table: &[(&'static str, Takes)]) -> Result<Parsed, Failure> {
    let mut parsed = Parsed::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "-" || !arg.starts_with('-') {
            parsed.positional.push(arg.clone());
            continue;
        }
        // `--opt` is accepted for `-opt`.
        let single = arg.strip_prefix('-').filter(|name| name.starts_with('-'));
        let found = table
            .iter()
            .find(|(name, _)| *name == arg.as_str() || Some(*name) == single);
        let Some((name, takes)) = found else {
            return Err(failure(
                OTHER,
                format!("{tool}: unknown option {arg}; see {tool} -h"),
            ));
        };
        let value = match takes {
            Nothing => String::new(),
            Value => rest
                .next()
                .ok_or_else(|| failure(OTHER, format!("{tool}: {name} needs a value")))?
                .clone(),
        };
        parsed.options.insert(name, value);
    }
    Ok(parsed)
}

fn wants_help(parsed: &Parsed) -> bool {
    ["-h", "-help", "--help", "-?"]
        .iter()
        .any(|name| parsed.has(name))
}

fn refuse(tool: &str, parsed: &Parsed, unsupported: &[(&str, &str)]) -> Result<(), Failure> {
    for (name, instead) in unsupported {
        if parsed.has(name) {
            return Err(failure(
                OTHER,
                format!("{tool} (lege-pdf): {name} is not supported; {instead}"),
            ));
        }
    }
    Ok(())
}

/// The document and the pages `-f`/`-l` select, the way Poppler reads them.
fn open(
    tool: &str,
    parsed: &Parsed,
) -> Result<(std::sync::Arc<DocumentSnapshot>, u32, u32), Failure> {
    let file = &parsed.positional[0];
    if file == "-" {
        return Err(failure(
            CANNOT_OPEN,
            format!("{tool} (lege-pdf): reading the PDF from standard input is not supported"),
        ));
    }
    let password = parsed.text("-upw").or_else(|| parsed.text("-opw"));
    let (_, snapshot) = resolve_snapshot(Path::new(file), password, None, None).map_err(|err| {
        failure(
            CANNOT_OPEN,
            format!("Error: Couldn't open file '{file}': {err:#}"),
        )
    })?;
    let count = snapshot.page_count();
    let first = parsed.number::<i64>(tool, "-f")?.unwrap_or(1).max(1);
    let last = match parsed.number::<i64>(tool, "-l")? {
        Some(last) if last >= 1 && last <= i64::from(count) => last,
        _ => i64::from(count),
    };
    if last < first {
        return Err(failure(
            OTHER,
            format!(
                "Error: Wrong page range given: the first page ({first}) can not be after the last page ({last})."
            ),
        ));
    }
    Ok((snapshot, first as u32, last as u32))
}

/// No limits: Poppler has none, and the caller chose the pages.
fn unbounded() -> Bounds {
    Bounds {
        max_pages: 0,
        scan_pages: 0,
        max_items: 0,
        max_bytes: 0,
        timeout_secs: 0,
        threads: 0,
    }
}

/// The part of a page under `-x -y -W -H`, in PDF space, when any is given.
/// They count pixels of the page as displayed at `scale` pixels per point,
/// from its top-left corner; a width or height of zero runs to the edge.
fn crop_of(
    snapshot: &DocumentSnapshot,
    page: PageZero,
    scale: f64,
    [x, y, width, height]: [f64; 4],
) -> Option<[f64; 4]> {
    if x <= 0.0 && y <= 0.0 && width <= 0.0 && height <= 0.0 {
        return None;
    }
    let page_ref = snapshot.page(PageIndex(page.0)).ok()?;
    let (page_width, page_height) = display_size(snapshot, page);
    let right = if width > 0.0 {
        x + width
    } else {
        (page_width * scale).ceil()
    };
    let bottom = if height > 0.0 {
        y + height
    } else {
        (page_height * scale).ceil()
    };
    render::pixel_region(
        page_ref.rotate,
        page_ref.crop_box,
        scale,
        [x.max(0.0), y.max(0.0), right, bottom],
    )
}

/// Width and height of a page as displayed, in points.
fn display_size(snapshot: &DocumentSnapshot, page: PageZero) -> (f64, f64) {
    let Ok(page) = snapshot.page(PageIndex(page.0)) else {
        return (0.0, 0.0);
    };
    let [x0, y0, x1, y1] = page.crop_box;
    let (width, height) = ((x1 - x0).abs(), (y1 - y0).abs());
    if page.rotate % 180 == 90 {
        (height, width)
    } else {
        (width, height)
    }
}

// ── pdftotext ────────────────────────────────────────────────────────────

const PDFTOTEXT_OPTIONS: &[(&str, Takes)] = &[
    ("-f", Value),
    ("-l", Value),
    ("-r", Value),
    ("-x", Value),
    ("-y", Value),
    ("-W", Value),
    ("-H", Value),
    ("-layout", Nothing),
    ("-fixed", Value),
    ("-raw", Nothing),
    ("-nodiag", Nothing),
    ("-htmlmeta", Nothing),
    ("-tsv", Nothing),
    ("-enc", Value),
    ("-listenc", Nothing),
    ("-eol", Value),
    ("-nopgbrk", Nothing),
    ("-bbox", Nothing),
    ("-bbox-layout", Nothing),
    ("-cropbox", Nothing),
    ("-colspacing", Value),
    ("-opw", Value),
    ("-upw", Value),
    ("-q", Nothing),
    ("-v", Nothing),
    ("-h", Nothing),
    ("-help", Nothing),
    ("--help", Nothing),
    ("-?", Nothing),
];

const PDFTOTEXT_USAGE: &str = "\
Usage: pdftotext [options] <PDF-file> [<text-file>]
  -f <int>           first page to convert
  -l <int>           last page to convert
  -r <fp>            resolution the crop options count pixels at (default 72)
  -x <int>           x-coordinate of the crop area top left corner
  -y <int>           y-coordinate of the crop area top left corner
  -W <int>           width of crop area in pixels (default is 0)
  -H <int>           height of crop area in pixels (default is 0)
  -layout            maintain original physical layout
  -raw               keep strings in content stream order
  -enc <string>      output text encoding name (UTF-8 only)
  -eol <string>      output end-of-line convention (unix, dos, or mac)
  -nopgbrk           don't insert page breaks between pages
  -opw <string>      owner password (for encrypted files)
  -upw <string>      user password (for encrypted files)
  -q                 don't print any messages or errors
  -v                 print version info
  -h, -help, --help  print usage information

Answered by lege-pdf. <text-file> defaults to the PDF's name with .txt; '-'
writes to standard output. Pages are extracted in parallel.
Differences from Poppler: -raw gives reading order, as the default does;
-nodiag, -cropbox and -colspacing are accepted and have no effect; -bbox,
-bbox-layout, -tsv, -htmlmeta, -fixed and -listenc are refused (for word
boxes use: lege-pdf text FILE --layout words --json).";

fn pdftotext(args: &[String]) -> Result<i32, Failure> {
    const TOOL: &str = "pdftotext";
    let parsed = parse(TOOL, args, PDFTOTEXT_OPTIONS)?;
    if parsed.has("-v") {
        eprintln!("pdftotext (lege-pdf) version {}", env!("CARGO_PKG_VERSION"));
        return Ok(DONE);
    }
    if wants_help(&parsed) {
        eprintln!("{PDFTOTEXT_USAGE}");
        return Ok(DONE);
    }
    if parsed.positional.is_empty() || parsed.positional.len() > 2 {
        return Err(failure(OTHER, PDFTOTEXT_USAGE));
    }
    let words = "for word boxes use `lege-pdf text FILE --layout words --json`";
    refuse(
        TOOL,
        &parsed,
        &[
            ("-bbox", words),
            ("-bbox-layout", words),
            ("-tsv", words),
            ("-htmlmeta", "leave it out for plain text"),
            ("-fixed", "use -layout"),
            ("-listenc", "the output is always UTF-8"),
        ],
    )?;
    if let Some(encoding) = parsed.text("-enc")
        && !encoding.eq_ignore_ascii_case("UTF-8")
    {
        return Err(failure(
            OTHER,
            format!(
                "pdftotext (lege-pdf): the output is always UTF-8; -enc {encoding} is not supported"
            ),
        ));
    }
    let eol = match parsed.text("-eol") {
        Some("unix") => "\n",
        Some("dos") => "\r\n",
        Some("mac") => "\r",
        Some(other) => {
            return Err(failure(
                OTHER,
                format!("Bad '-eol' value {other:?}: use unix, dos or mac"),
            ));
        }
        None if cfg!(windows) => "\r\n",
        None => "\n",
    };
    let quiet = parsed.has("-q");
    let page_breaks = !parsed.has("-nopgbrk");
    let layout = if parsed.has("-layout") {
        TextLayout::Physical
    } else {
        TextLayout::Plain
    };
    let resolution = parsed.number::<f64>(TOOL, "-r")?.unwrap_or(72.0);
    if !(resolution.is_finite() && resolution > 0.0) {
        return Err(failure(OTHER, "pdftotext: -r must be a positive number"));
    }
    let mut area = [0.0f64; 4];
    for (slot, name) in area.iter_mut().zip(["-x", "-y", "-W", "-H"]) {
        *slot = parsed.number::<f64>(TOOL, name)?.unwrap_or(0.0);
    }

    let (snapshot, first, last) = open(TOOL, &parsed)?;
    let file = &parsed.positional[0];
    let target = match parsed.positional.get(1) {
        Some(target) => target.clone(),
        None => text_file_for(file),
    };
    let mut out: Box<dyn Write> = if target == "-" {
        Box::new(std::io::BufWriter::new(std::io::stdout().lock()))
    } else {
        let created = std::fs::File::create(&target).map_err(|err| {
            failure(
                CANNOT_WRITE,
                format!("Error: Couldn't open text file '{target}': {err}"),
            )
        })?;
        Box::new(std::io::BufWriter::new(created))
    };

    let pages: Vec<PageZero> = (first - 1..last).map(PageZero).collect();
    let bounds = unbounded();
    let snapshot = snapshot.as_ref();
    let mut code = DONE;
    let mut write_error: Option<std::io::Error> = None;
    for_pages(
        &pages,
        0,
        || text::PageState::new(false),
        |state, page, warnings| {
            let extracted = text::extract_page(
                snapshot,
                &state.compiler,
                page,
                &TextPageOptions::default(),
                layout,
                crop_of(snapshot, page, resolution / 72.0, area),
                OcrMode::Never,
                "",
                &bounds,
                warnings,
                &mut state.ocr_backend,
                &mut state.glyphs,
            )?;
            Ok(extracted.text.unwrap_or_default())
        },
        |_, page, outcome, warnings| {
            let page_text = match outcome {
                PageOutcome::Done(page_text) => page_text,
                PageOutcome::Failed(error) => {
                    code = OTHER;
                    if !quiet {
                        eprintln!("Error: page {}: {error:#}", page.one_based());
                    }
                    String::new()
                }
                // No budget is armed here.
                PageOutcome::TimedOut => String::new(),
            };
            if !quiet {
                for warning in warnings {
                    eprintln!("Warning: page {}: {warning}", page.one_based());
                }
            }
            match write_page(&mut out, &page_text, eol, page_breaks) {
                Ok(()) => ControlFlow::Continue(()),
                Err(err) => {
                    write_error = Some(err);
                    ControlFlow::Break(())
                }
            }
        },
    );
    if let Some(err) = write_error.or_else(|| out.flush().err()) {
        return Err(failure(
            CANNOT_WRITE,
            format!("Error: writing '{target}': {err}"),
        ));
    }
    Ok(code)
}

/// The text file Poppler writes when none is named: the PDF's name with
/// `.pdf` replaced by `.txt`, or `.txt` added to a name without it.
fn text_file_for(pdf: &str) -> String {
    let stem = match pdf.len().checked_sub(4) {
        Some(at) if pdf.is_char_boundary(at) && pdf[at..].eq_ignore_ascii_case(".pdf") => {
            &pdf[..at]
        }
        _ => pdf,
    };
    format!("{stem}.txt")
}

/// One page of text: its lines under the chosen line ending, then a form
/// feed. A page without text is the form feed alone.
fn write_page(
    out: &mut dyn Write,
    page_text: &str,
    eol: &str,
    page_break: bool,
) -> std::io::Result<()> {
    let unix = page_text.replace("\r\n", "\n");
    let body = unix.trim_end_matches('\n');
    if !body.is_empty() {
        for line in body.split('\n') {
            out.write_all(line.as_bytes())?;
            out.write_all(eol.as_bytes())?;
        }
    }
    if page_break {
        out.write_all(b"\x0c")?;
    }
    Ok(())
}

// ── pdftoppm ─────────────────────────────────────────────────────────────

const PDFTOPPM_OPTIONS: &[(&str, Takes)] = &[
    ("-f", Value),
    ("-l", Value),
    ("-o", Nothing),
    ("-e", Nothing),
    ("-singlefile", Nothing),
    ("-scale-dimension-before-rotation", Nothing),
    ("-r", Value),
    ("-rx", Value),
    ("-ry", Value),
    ("-scale-to", Value),
    ("-scale-to-x", Value),
    ("-scale-to-y", Value),
    ("-x", Value),
    ("-y", Value),
    ("-W", Value),
    ("-H", Value),
    ("-sz", Value),
    ("-cropbox", Nothing),
    ("-hide-annotations", Nothing),
    ("-mono", Nothing),
    ("-gray", Nothing),
    ("-displayprofile", Value),
    ("-defaultgrayprofile", Value),
    ("-defaultrgbprofile", Value),
    ("-defaultcmykprofile", Value),
    ("-sep", Value),
    ("-forcenum", Nothing),
    ("-png", Nothing),
    ("-jpeg", Nothing),
    ("-jpegcmyk", Nothing),
    ("-jpegopt", Value),
    ("-overprint", Nothing),
    ("-tiff", Nothing),
    ("-tiffcompression", Value),
    ("-freetype", Value),
    ("-thinlinemode", Value),
    ("-aa", Value),
    ("-aaVector", Value),
    ("-opw", Value),
    ("-upw", Value),
    ("-q", Nothing),
    ("-progress", Nothing),
    ("-v", Nothing),
    ("-h", Nothing),
    ("-help", Nothing),
    ("--help", Nothing),
    ("-?", Nothing),
];

const PDFTOPPM_USAGE: &str = "\
Usage: pdftoppm [options] <PDF-file> [<PPM-file-prefix>]
  -f <int>            first page to print
  -l <int>            last page to print
  -o                  print only odd pages
  -e                  print only even pages
  -singlefile         write only the first page and do not add digits
  -r <fp>             resolution, in DPI (default is 150)
  -rx <fp>, -ry <fp>  X and Y resolution, in DPI (must be equal)
  -scale-to <int>     scales each page to fit within scale-to*scale-to pixel box
  -scale-to-x <int>   scales each page horizontally to fit in scale-to-x pixels
  -scale-to-y <int>   scales each page vertically to fit in scale-to-y pixels
  -x <int>            x-coordinate of the crop area top left corner
  -y <int>            y-coordinate of the crop area top left corner
  -W <int>            width of crop area in pixels (default is 0)
  -H <int>            height of crop area in pixels (default is 0)
  -sz <int>           size of crop square in pixels (sets W and H)
  -hide-annotations   do not show annotations
  -gray               generate a grayscale PGM file
  -sep <string>       separator between the prefix and the page number (default -)
  -png                generate a PNG file
  -jpeg               generate a JPEG file
  -jpegopt <string>   jpeg options; quality=<1-100> is honoured
  -opw <string>       owner password (for encrypted files)
  -upw <string>       user password (for encrypted files)
  -q                  don't print any messages or errors
  -progress           print progress info
  -v                  print version info
  -h, -help, --help   print usage information

Answered by lege-pdf. Without a prefix the images go to standard output.
Pages are rendered in parallel and written in order.
Differences from Poppler: the page is its CropBox (-cropbox is implied);
-forcenum, -freetype, -thinlinemode, -aa and -aaVector are accepted and
have no effect; -mono, -tiff, -tiffcompression, -jpegcmyk, -overprint,
-scale-dimension-before-rotation, the colour profile options, and X and Y
scales that differ are refused.";

fn pdftoppm(args: &[String]) -> Result<i32, Failure> {
    const TOOL: &str = "pdftoppm";
    let parsed = parse(TOOL, args, PDFTOPPM_OPTIONS)?;
    if parsed.has("-v") {
        eprintln!("pdftoppm (lege-pdf) version {}", env!("CARGO_PKG_VERSION"));
        return Ok(DONE);
    }
    if wants_help(&parsed) {
        eprintln!("{PDFTOPPM_USAGE}");
        return Ok(DONE);
    }
    if parsed.positional.is_empty() || parsed.positional.len() > 2 {
        return Err(failure(OTHER, PDFTOPPM_USAGE));
    }
    let profile = "colour management is not configurable";
    refuse(
        TOOL,
        &parsed,
        &[
            ("-mono", "use -gray"),
            ("-tiff", "use -png"),
            ("-tiffcompression", "use -png"),
            ("-jpegcmyk", "use -jpeg"),
            ("-overprint", "overprint preview is not available"),
            (
                "-scale-dimension-before-rotation",
                "sizes apply to the page as displayed",
            ),
            ("-displayprofile", profile),
            ("-defaultgrayprofile", profile),
            ("-defaultrgbprofile", profile),
            ("-defaultcmykprofile", profile),
        ],
    )?;

    let number = |name: &str| parsed.number::<f64>(TOOL, name);
    let mut resolution = number("-r")?.unwrap_or(150.0);
    match (number("-rx")?, number("-ry")?) {
        (Some(x), Some(y)) if x != y => {
            return Err(failure(
                OTHER,
                "pdftoppm (lege-pdf): -rx and -ry must be equal; pages are scaled uniformly",
            ));
        }
        (Some(both), _) | (None, Some(both)) => resolution = both,
        (None, None) => {}
    }
    if !(resolution.is_finite() && resolution > 0.0) {
        return Err(failure(
            OTHER,
            "pdftoppm: the resolution must be a positive number",
        ));
    }
    let fit = number("-scale-to")?.filter(|edge| *edge > 0.0);
    let fit_x = number("-scale-to-x")?.filter(|edge| *edge > 0.0);
    let fit_y = number("-scale-to-y")?.filter(|edge| *edge > 0.0);
    if fit.is_none() && fit_x.is_some() && fit_y.is_some() {
        return Err(failure(
            OTHER,
            "pdftoppm (lege-pdf): give -scale-to-x or -scale-to-y, not both; pages are scaled uniformly",
        ));
    }
    let mut area = [0.0f64; 4];
    for (slot, name) in area.iter_mut().zip(["-x", "-y", "-W", "-H"]) {
        *slot = number(name)?.unwrap_or(0.0);
    }
    if let Some(square) = number("-sz")?.filter(|size| *size > 0.0) {
        (area[2], area[3]) = (square, square);
    }
    let gray = parsed.has("-gray");
    let (format, extension) = match (parsed.has("-png"), parsed.has("-jpeg"), gray) {
        (true, _, _) => (ImageFormat::Png, "png"),
        (false, true, _) => (ImageFormat::Jpeg, "jpg"),
        (false, false, true) => (ImageFormat::Pgm, "pgm"),
        (false, false, false) => (ImageFormat::Ppm, "ppm"),
    };
    let quality = parsed
        .text("-jpegopt")
        .and_then(|options| {
            options
                .split(',')
                .find_map(|option| option.trim().strip_prefix("quality="))
        })
        .map(|raw| {
            raw.parse::<u8>()
                .ok()
                .filter(|quality| (1..=100).contains(quality))
                .ok_or_else(|| failure(OTHER, format!("pdftoppm: bad jpeg quality {raw:?}")))
        })
        .transpose()?
        .unwrap_or(RenderSpec::default().quality);
    let quiet = parsed.has("-q");
    let progress = parsed.has("-progress");
    let single = parsed.has("-singlefile");
    let separator = parsed.text("-sep").unwrap_or("-");

    let (snapshot, first, mut last) = open(TOOL, &parsed)?;
    if single && first < last {
        if !quiet {
            eprintln!(
                "Warning: Single file will write only the first of the {} pages.",
                last - first + 1
            );
        }
        last = first;
    }
    let (odd, even) = (parsed.has("-o"), parsed.has("-e"));
    let pages: Vec<PageZero> = (first..=last)
        .filter(|page| !(odd && page % 2 == 0) && !(even && page % 2 == 1))
        .map(|page| PageZero(page - 1))
        .collect();
    let digits = snapshot.page_count().to_string().len();
    let prefix = parsed.positional.get(1);

    let snapshot = snapshot.as_ref();
    let spec_for = |page: PageZero| -> RenderSpec {
        let (width, height) = display_size(snapshot, page);
        let scale = match (fit, fit_x, fit_y) {
            (Some(edge), _, _) => edge / width.max(height).max(1.0),
            (None, Some(edge), _) => edge / width.max(1.0),
            (None, None, Some(edge)) => edge / height.max(1.0),
            (None, None, None) => resolution / 72.0,
        };
        RenderSpec {
            dpi: Some(scale * 72.0),
            crop: crop_of(snapshot, page, scale, area),
            gray,
            format,
            quality,
            ..RenderSpec::default()
        }
    };

    let Some(first_page) = pages.first() else {
        return Ok(DONE);
    };
    let workers = crate::parallel::worker_count(
        render::render_workers(snapshot, &pages, &spec_for(*first_page), 0),
        pages.len(),
    );
    let backend = CpuBackend::new(CpuBackendOptions::default());
    let mut code = DONE;
    let mut stopped: Option<Failure> = None;
    crate::parallel::ordered(
        &pages,
        workers,
        || page_compiler(false, !parsed.has("-hide-annotations")),
        |compiler, _, page| {
            let mut warnings = Vec::new();
            let image = render::render_encoded(
                snapshot,
                compiler,
                &backend,
                *page,
                &spec_for(*page),
                &mut warnings,
            );
            (image, warnings)
        },
        |_, page, (image, warnings)| {
            let number = page.one_based();
            if !quiet {
                for warning in warnings {
                    eprintln!("Warning: page {number}: {warning}");
                }
            }
            let image = match image {
                Ok(image) => image,
                Err(error) => {
                    code = OTHER;
                    if !quiet {
                        eprintln!("Error: page {number}: {error:#}");
                    }
                    return ControlFlow::Continue(());
                }
            };
            let target = match prefix {
                None => "-".to_owned(),
                Some(prefix) if single => format!("{prefix}.{extension}"),
                Some(prefix) => format!("{prefix}{separator}{number:0digits$}.{extension}"),
            };
            if let Err(error) = render::write_image(&target, &image.bytes) {
                stopped = Some(failure(CANNOT_WRITE, format!("Error: {error:#}")));
                return ControlFlow::Break(());
            }
            if progress {
                eprintln!("{number} {last} {target}");
            }
            ControlFlow::Continue(())
        },
    );
    match stopped {
        Some(failure) => Err(failure),
        None => Ok(code),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn names_select_a_tool_whatever_their_case_or_suffix() {
        assert_eq!(tool_named("pdftotext"), Some(Tool::PdfToText));
        assert_eq!(tool_named("PDFTOPPM.EXE"), Some(Tool::PdfToPpm));
        assert_eq!(tool_named("lege-pdf"), None);
        assert_eq!(tool_named("text"), None);
    }

    #[test]
    fn options_are_found_wherever_they_stand() {
        let parsed = parse(
            "pdftotext",
            &args(&["-f", "2", "in.pdf", "-layout", "-", "--nopgbrk"]),
            PDFTOTEXT_OPTIONS,
        )
        .unwrap();
        assert_eq!(parsed.positional, ["in.pdf", "-"]);
        assert_eq!(parsed.number::<i64>("pdftotext", "-f").unwrap(), Some(2));
        assert!(parsed.has("-layout") && parsed.has("-nopgbrk"));
        assert!(!parsed.has("-raw"));
    }

    #[test]
    fn an_unknown_option_or_a_missing_value_is_refused() {
        let unknown = parse("pdftoppm", &args(&["-webp", "in.pdf"]), PDFTOPPM_OPTIONS).unwrap_err();
        assert_eq!(unknown.code, OTHER);
        assert!(unknown.message.contains("-webp"), "{}", unknown.message);

        let missing = parse("pdftoppm", &args(&["in.pdf", "-r"]), PDFTOPPM_OPTIONS).unwrap_err();
        assert!(
            missing.message.contains("-r needs a value"),
            "{}",
            missing.message
        );

        let parsed = parse("pdftoppm", &args(&["-r", "x", "in.pdf"]), PDFTOPPM_OPTIONS).unwrap();
        assert!(parsed.number::<f64>("pdftoppm", "-r").is_err());
    }

    #[test]
    fn the_text_file_takes_the_place_of_the_pdf_suffix() {
        assert_eq!(text_file_for("dir/report.pdf"), "dir/report.txt");
        assert_eq!(text_file_for("REPORT.PDF"), "REPORT.txt");
        assert_eq!(text_file_for("report"), "report.txt");
        assert_eq!(text_file_for("a"), "a.txt");
    }

    #[test]
    fn a_page_is_its_lines_and_a_form_feed() {
        let page = |text: &str, eol: &str, page_break: bool| {
            let mut out = Vec::new();
            write_page(&mut out, text, eol, page_break).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(page("one\r\ntwo", "\n", true), "one\ntwo\n\x0c");
        assert_eq!(page("one\ntwo\n", "\r\n", false), "one\r\ntwo\r\n");
        assert_eq!(page("", "\n", true), "\x0c");
    }
}
