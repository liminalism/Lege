//! `lege-titler`: give every PDF in a folder a Title and Author.
//!
//! Page 1 of each file goes through Lege's thorough OCR (the lege-document-ocr
//! pipeline with layout detection); the largest text, preferably in the top
//! half, becomes the title and smaller name-like text, preferably in the bottom
//! half, the author. The values are appended to the file as an incremental
//! update. By default only empty (or placeholder) fields are filled.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod heuristics;
mod ocr;
mod pdf_update;
mod xmp;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use lege_document_pipeline::{BackendChoice, TensorRtPaddleConfig};

use crate::heuristics::Guess;
use crate::ocr::{OcrSettings, PageReader};
use crate::pdf_update::MetadataUpdate;

#[derive(Debug, Parser)]
#[command(
    name = "lege-titler",
    version,
    about = "Guess each PDF's title and author from page 1 (OCR + layout) and write them into its metadata"
)]
struct Cli {
    /// Folder holding the PDFs.
    input: PathBuf,
    /// Also process PDFs in subfolders.
    #[arg(long, short)]
    recursive: bool,
    /// Print the guesses without changing any file.
    #[arg(long, short = 'n')]
    dry_run: bool,
    /// Replace a Title/Author the file already has (default: fill empty fields only).
    #[arg(long)]
    overwrite: bool,
    /// Append to digitally signed files too (the signature stays valid but the
    /// file then shows a post-signing revision).
    #[arg(long)]
    allow_signed: bool,
    /// OCR backend, as for `lege-ocr batch`.
    #[arg(long, value_enum, default_value_t = BackendArg::Auto)]
    backend: BackendArg,
    /// Packaged TensorRT root containing bin, models, and runtime directories.
    #[arg(long)]
    tensorrt_ocr_root: Option<PathBuf>,
    /// Render resolution for OCR.
    #[arg(long, default_value_t = 300)]
    dpi: u32,
    /// Skip PP-DocLayout-M layout detection (loses the doc_title signal).
    #[arg(long)]
    no_layout: bool,
    /// Read a trustworthy embedded text layer instead of running OCR.
    #[arg(long)]
    use_text_layer: bool,
    /// Show every line with its size and position, and why each guess was made.
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendArg {
    Auto,
    #[value(name = "tensorrt-paddle", alias = "tensor-rt-paddle")]
    TensorRtPaddle,
    Paddle,
    WinocrLegacy,
}

impl BackendArg {
    fn choice(self) -> BackendChoice {
        match self {
            Self::Auto => BackendChoice::Auto,
            Self::TensorRtPaddle => BackendChoice::TensorRtPaddle,
            Self::Paddle => BackendChoice::Paddle,
            Self::WinocrLegacy => BackendChoice::WinOcrLegacy,
        }
    }
}

#[derive(Debug, Default)]
struct Tally {
    written: u32,
    dry_run: u32,
    kept: u32,
    no_guess: u32,
    skipped: u32,
    failed: u32,
}

enum Outcome {
    Written,
    DryRun,
    Kept,
    NoGuess,
    Skipped(String),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(tally) if tally.failed == 0 => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(error) => {
            eprintln!("lege-titler: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<Tally, String> {
    let files = discover(&cli.input, cli.recursive)?;
    if files.is_empty() {
        return Err(format!("no PDF files in {}", cli.input.display()));
    }
    let tensorrt_paddle = if matches!(cli.backend, BackendArg::Auto | BackendArg::TensorRtPaddle) {
        match &cli.tensorrt_ocr_root {
            Some(root) => {
                Some(TensorRtPaddleConfig::from_root(root, 8).map_err(|error| error.to_string())?)
            }
            None => TensorRtPaddleConfig::discover_result(8).map_err(|error| error.to_string())?,
        }
    } else {
        None
    };
    let settings = OcrSettings {
        backend: cli.backend.choice(),
        render_dpi: cli.dpi,
        layout: !cli.no_layout,
        use_text_layer: cli.use_text_layer,
        tensorrt_paddle,
    };
    // The reader (and its GPU/OCR runtime) starts only when a file needs OCR.
    let mut reader: Option<PageReader> = None;
    let mut tally = Tally::default();
    for path in &files {
        let name = display_name(&cli.input, path);
        match process(cli, &settings, &mut reader, path) {
            Ok((guess, outcome)) => {
                let summary = describe(&guess);
                match outcome {
                    Outcome::Written => {
                        tally.written += 1;
                        println!("{name}: {summary} [written]");
                    }
                    Outcome::DryRun => {
                        tally.dry_run += 1;
                        println!("{name}: {summary} [dry-run]");
                    }
                    Outcome::Kept => {
                        tally.kept += 1;
                        println!("{name}: {summary} [kept existing]");
                    }
                    Outcome::NoGuess => {
                        tally.no_guess += 1;
                        println!("{name}: [no confident guess]");
                    }
                    Outcome::Skipped(reason) => {
                        tally.skipped += 1;
                        println!("{name}: [skipped: {reason}]");
                    }
                }
                if cli.verbose {
                    for note in &guess.notes {
                        println!("    note: {note}");
                    }
                }
            }
            Err(Fatal(error)) => return Err(error),
            Err(FileError(error)) => {
                tally.failed += 1;
                println!("{name}: [error: {error}]");
            }
        }
    }
    println!(
        "lege-titler: {} file(s): {} written, {} dry-run, {} kept existing, {} no guess, {} skipped, {} failed",
        files.len(),
        tally.written,
        tally.dry_run,
        tally.kept,
        tally.no_guess,
        tally.skipped,
        tally.failed
    );
    Ok(tally)
}

enum Failure {
    /// The OCR runtime itself is unusable: stop the batch.
    Fatal(String),
    /// This file could not be handled: report it and go on.
    FileError(String),
}
use Failure::{Fatal, FileError};

fn process(
    cli: &Cli,
    settings: &OcrSettings,
    reader: &mut Option<PageReader>,
    path: &Path,
) -> Result<(Guess, Outcome), Failure> {
    let bytes: Arc<[u8]> = std::fs::read(path)
        .map_err(|error| FileError(error.to_string()))?
        .into();
    let inspection = pdf_update::inspect(&bytes, cli.allow_signed).map_err(FileError)?;
    if let Some(reason) = inspection.blocker {
        return Ok((Guess::default(), Outcome::Skipped(reason)));
    }
    let existing = inspection.existing;
    if cli.verbose {
        println!(
            "    in file: title={:?} author={:?}",
            existing.title, existing.author
        );
    }
    let want_title = cli.overwrite || existing.title.is_none();
    let want_author = cli.overwrite || existing.author.is_none();
    if !want_title && !want_author {
        let guess = Guess {
            title: existing.title.map(existing_field),
            author: existing.author.map(existing_field),
            notes: Vec::new(),
        };
        return Ok((guess, Outcome::Kept));
    }

    let reader = match reader {
        Some(reader) => reader,
        None => {
            let started = PageReader::new(settings).map_err(Fatal)?;
            let backend = started.start().map_err(Fatal)?;
            eprintln!("lege-titler: OCR backend `{backend}`");
            for warning in started.warnings() {
                eprintln!("lege-titler: {warning}");
            }
            reader.insert(started)
        }
    };
    let page = reader.first_page(&bytes).map_err(FileError)?;
    if cli.verbose {
        dump_lines(&page);
    }
    let mut guess = heuristics::guess(&page);
    if cli.verbose {
        for (label, field) in [("title", &guess.title), ("author", &guess.author)] {
            if let Some(field) = field {
                println!("    {label}: {:?} ({})", field.value, field.reason);
            }
        }
    }

    let update = MetadataUpdate {
        title: guess
            .title
            .as_ref()
            .filter(|_| want_title)
            .map(|field| field.value.clone()),
        author: guess
            .author
            .as_ref()
            .filter(|_| want_author)
            .map(|field| field.value.clone()),
    };
    // Show what the file will hold afterwards, not only what was guessed.
    let had_any = existing.title.is_some() || existing.author.is_some();
    if !want_title && let Some(title) = existing.title {
        guess.title = Some(existing_field(title));
    }
    if !want_author && let Some(author) = existing.author {
        guess.author = Some(existing_field(author));
    }
    if update.is_empty() {
        let outcome = if had_any && !cli.overwrite {
            Outcome::Kept
        } else {
            Outcome::NoGuess
        };
        return Ok((guess, outcome));
    }
    let updated =
        pdf_update::apply(&bytes, &update, std::time::SystemTime::now()).map_err(FileError)?;
    for warning in &updated.warnings {
        eprintln!("lege-titler: {}: {warning}", path.display());
    }
    pdf_update::verify(&updated.bytes, inspection.page_count, &update).map_err(FileError)?;
    if cli.dry_run {
        return Ok((guess, Outcome::DryRun));
    }
    pdf_update::replace_file(path, &updated.bytes).map_err(FileError)?;
    Ok((guess, Outcome::Written))
}

fn existing_field(value: String) -> heuristics::Field {
    heuristics::Field {
        value,
        reason: "already in the file".to_string(),
    }
}

fn describe(guess: &Guess) -> String {
    let show = |field: &Option<heuristics::Field>| match field {
        Some(field) if field.reason == "already in the file" => {
            format!("{:?} (existing)", field.value)
        }
        Some(field) => format!("{:?}", field.value),
        None => "-".to_string(),
    };
    format!(
        "title={} author={}",
        show(&guess.title),
        show(&guess.author)
    )
}

fn dump_lines(page: &heuristics::PageLines) {
    let mut lines: Vec<&heuristics::Line> = page.lines.iter().collect();
    lines.sort_by(|a, b| a.bbox[1].total_cmp(&b.bbox[1]));
    println!(
        "    page {}x{} px, {} line(s):",
        page.width,
        page.height,
        lines.len()
    );
    for line in lines {
        println!(
            "    y={:>5.0} h={:>4.0} x={:>5.0}-{:<5.0} conf={} r={} {:?} {:?}",
            line.bbox[1],
            line.bbox[3] - line.bbox[1],
            line.bbox[0],
            line.bbox[2],
            line.confidence
                .map_or_else(|| "-".to_string(), |value| format!("{value:.2}")),
            line.region
                .map_or_else(|| "-".to_string(), |region| region.to_string()),
            line.kind,
            line.text
        );
    }
}

/// The path relative to the input folder, or the file name when the input was
/// the file itself.
fn display_name(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
        _ => path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

/// PDFs under `folder`, sorted. A single PDF path is accepted too.
fn discover(folder: &Path, recursive: bool) -> Result<Vec<PathBuf>, String> {
    if folder.is_file() {
        return if is_pdf(folder) {
            Ok(vec![folder.to_path_buf()])
        } else {
            Err(format!("{} is not a PDF", folder.display()))
        };
    }
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            let path = entry.path();
            if kind.is_dir() {
                if recursive {
                    pending.push(path);
                }
            } else if kind.is_file() && is_pdf(&path) {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_finds_pdfs_case_insensitively_and_recurses_on_request() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.pdf"), b"%PDF").unwrap();
        std::fs::write(root.path().join("B.PDF"), b"%PDF").unwrap();
        std::fs::write(root.path().join("notes.txt"), b"x").unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        std::fs::write(root.path().join("sub/c.pdf"), b"%PDF").unwrap();
        assert_eq!(discover(root.path(), false).unwrap().len(), 2);
        assert_eq!(discover(root.path(), true).unwrap().len(), 3);
    }
}
