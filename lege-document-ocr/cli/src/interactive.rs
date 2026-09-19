//! OpenArc-style guided wizard: drag-and-drop paths, a few questions, then batch.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use crate::paths::{expand_inputs, parse_path_input};
use crate::{BackendArg, BatchArgs, OnErrorArg, PdfModeArg, ProfileArg, TextViewArg, run_batch};

struct Colors {
    prompt: &'static str,
    info: &'static str,
    highlight: &'static str,
    success: &'static str,
    warning: &'static str,
    reset: &'static str,
}

const COLORS: Colors = Colors {
    prompt: "\x1b[97m",
    info: "\x1b[36m",
    highlight: "\x1b[35m",
    success: "\x1b[92m",
    warning: "\x1b[93m",
    reset: "\x1b[0m",
};

pub fn run_interactive() -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "interactive mode needs a terminal. Example:\n  lege-ocr batch list.txt --output ~/Downloads/lege-ocr --format text,markdown --backend auto --resume"
                .to_string(),
        );
    }

    println!(
        "{}╔══════════════════════════════════════════════╗{}",
        COLORS.info, COLORS.reset
    );
    println!(
        "{}║   Lege OCR  —  PDF to text / markdown        ║{}",
        COLORS.info, COLORS.reset
    );
    println!(
        "{}╚══════════════════════════════════════════════╝{}",
        COLORS.info, COLORS.reset
    );
    println!(
        "{}Drag-and-drop PDFs, a folder, or a list.txt of file:// paths.{}",
        COLORS.info, COLORS.reset
    );

    println!(
        "\n{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        COLORS.highlight, COLORS.reset
    );
    println!("{}Step 1/3: Input files{}", COLORS.highlight, COLORS.reset);
    println!(
        "{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        COLORS.highlight, COLORS.reset
    );
    let inputs = collect_input_paths()?;
    if inputs.is_empty() {
        println!(
            "{}No files selected. Exiting.{}",
            COLORS.warning, COLORS.reset
        );
        return Ok(());
    }
    let sources = expand_inputs(&inputs, true)?;
    if sources.is_empty() {
        return Err("no PDF files were found in the paths you gave".to_string());
    }
    println!(
        "\n{}✓ {} PDF{} ready{}",
        COLORS.success,
        sources.len(),
        if sources.len() == 1 { "" } else { "s" },
        COLORS.reset
    );
    for source in sources.iter().take(12) {
        println!("  {}", source.display());
    }
    if sources.len() > 12 {
        println!("  … {} more", sources.len() - 12);
    }

    println!(
        "\n{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        COLORS.highlight, COLORS.reset
    );
    println!(
        "{}Step 2/3: Output folder{}",
        COLORS.highlight, COLORS.reset
    );
    println!(
        "{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        COLORS.highlight, COLORS.reset
    );
    let default_output = default_output_dir();
    print!(
        "{}Folder [{}]:{} ",
        COLORS.prompt,
        default_output.display(),
        COLORS.reset
    );
    io::stdout().flush().map_err(|error| error.to_string())?;
    let output = match read_line()?.as_str() {
        "" => default_output,
        typed => crate::paths::resolve_user_path_str(typed),
    };

    println!(
        "\n{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        COLORS.highlight, COLORS.reset
    );
    println!("{}Step 3/3: Confirm{}", COLORS.highlight, COLORS.reset);
    println!(
        "{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        COLORS.highlight, COLORS.reset
    );
    println!(
        "{}Outputs:{}     text + markdown",
        COLORS.info, COLORS.reset
    );
    println!(
        "{}Backend:{}     auto (TensorRT on NVIDIA, otherwise Paddle)",
        COLORS.info, COLORS.reset
    );
    println!(
        "{}Native text:{} keep it; OCR only pages that need it",
        COLORS.info, COLORS.reset
    );
    println!("{}Resume:{}      yes", COLORS.info, COLORS.reset);
    print!(
        "{}OCR every page anyway, ignoring embedded text? (y/N):{} ",
        COLORS.prompt, COLORS.reset
    );
    io::stdout().flush().map_err(|error| error.to_string())?;
    let force_ocr = read_yes_no(false)?;
    print!("{}Start now? (Y/n):{} ", COLORS.prompt, COLORS.reset);
    io::stdout().flush().map_err(|error| error.to_string())?;
    if !read_yes_no(true)? {
        println!("{}Cancelled.{}", COLORS.warning, COLORS.reset);
        return Ok(());
    }

    let args = BatchArgs {
        inputs,
        output: output.clone(),
        recursive: true,
        profile: ProfileArg::Search,
        backend: BackendArg::Auto,
        language: "eng".into(),
        formats: vec!["text".into(), "markdown".into()],
        text_view: TextViewArg::Corrected,
        resume: true,
        force: false,
        on_error: OnErrorArg::Continue,
        json_progress: false,
        dictionary: None,
        no_spellcheck: false,
        apply_spelling_edits: false,
        force_ocr,
        render_dpi: 300,
        max_page_pixels: 40_000_000,
        model_pack: None,
        tensorrt_ocr_root: None,
        tensorrt_dll_dir: Vec::new(),
        tensorrt_rec_batch: 8,
        broker_bridge: None,
        broker_endpoint: "evidence-trt".into(),
        broker_model: "turbo-ocr".into(),
        broker_revision: None,
        pdf_mode: PdfModeArg::Preserve,
        workers: 0,
        gpu_batch_lines: 64,
        gpu_batch_pixels: 12_000_000,
        gpu_batch_wait_ms: 3,
        gpu_queue_capacity: 32,
    };
    println!(
        "\n{}Writing to {}{}",
        COLORS.success,
        output.display(),
        COLORS.reset
    );
    run_batch(args)
}

fn default_output_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Downloads")
        .join("lege-ocr")
}

fn collect_input_paths() -> Result<Vec<PathBuf>, String> {
    println!(
        "{}One path per line, or several separated by spaces.{}",
        COLORS.info, COLORS.reset
    );
    println!(
        "{}A .txt list of file:// URLs is fine. Enter twice when done.{}",
        COLORS.info, COLORS.reset
    );
    print!("{}> {}", COLORS.prompt, COLORS.reset);
    io::stdout().flush().map_err(|error| error.to_string())?;

    let mut paths = Vec::new();
    let mut empty_lines = 0;
    loop {
        let line = read_line()?;
        if line.is_empty() {
            empty_lines += 1;
            if empty_lines >= 2 || !paths.is_empty() {
                break;
            }
            continue;
        }
        empty_lines = 0;
        paths.extend(parse_path_input(&line));
        print!("{}> {}", COLORS.prompt, COLORS.reset);
        io::stdout().flush().map_err(|error| error.to_string())?;
    }
    Ok(paths)
}

fn read_line() -> Result<String, String> {
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| error.to_string())?;
    Ok(input.trim().to_string())
}

fn read_yes_no(default_yes: bool) -> Result<bool, String> {
    let line = read_line()?;
    if line.is_empty() {
        return Ok(default_yes);
    }
    match line.to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Ok(default_yes),
    }
}
