//! `lege-pdf pdftotext` and `lege-pdf pdftoppm`: Poppler's options, file
//! names and exit codes.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pdf_test_support::builder::PdfBuilder;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lege-pdf"))
}

/// A document of `pages` 300 x 200 pages, each reading
/// `page N of the alias fixture`.
fn numbered_pages(dir: &Path, pages: u32) -> PathBuf {
    let mut builder = PdfBuilder::new();
    let kids: Vec<String> = (0..pages)
        .map(|page| format!("{} 0 R", 10 + page))
        .collect();
    builder.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    builder.add_object(
        2,
        &format!(
            "<</Type/Pages/Kids[{}]/Count {pages}/MediaBox[0 0 300 200]>>",
            kids.join(" ")
        ),
    );
    builder.add_object(
        3,
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>",
    );
    for page in 0..pages {
        builder.add_object(
            10 + page,
            &format!(
                "<</Type/Page/Parent 2 0 R/Contents {} 0 R/Resources<</Font<</F1 3 0 R>>>>>>",
                100 + page
            ),
        );
        let content = format!(
            "BT /F1 12 Tf 20 100 Td (page {} of the alias fixture) Tj ET",
            page + 1
        );
        builder.add_stream(100 + page, "", content.as_bytes());
    }
    builder.finish_classic_xref("/Root 1 0 R");
    let path = dir.join("numbered.pdf");
    std::fs::write(&path, builder.into_bytes()).expect("write fixture");
    path
}

fn run(dir: &Path, tool: &str, arguments: &[&str]) -> Output {
    bin()
        .current_dir(dir)
        .arg(tool)
        .args(arguments)
        .output()
        .expect("run lege-pdf")
}

fn ok(output: Output) -> Output {
    assert!(
        output.status.success(),
        "exit {:?}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "PNG magic missing");
    let field = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (field(16), field(20))
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "numbered.pdf")
        .collect();
    names.sort();
    names
}

#[test]
fn pdftotext_writes_each_page_and_a_form_feed() {
    let dir = tempfile::tempdir().unwrap();
    numbered_pages(dir.path(), 3);

    let output = ok(run(
        dir.path(),
        "pdftotext",
        &["-eol", "unix", "numbered.pdf", "-"],
    ));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "page 1 of the alias fixture\n\x0c\
         page 2 of the alias fixture\n\x0c\
         page 3 of the alias fixture\n\x0c"
    );

    // Options may follow the file names; a last page past the end is the end.
    let output = ok(run(
        dir.path(),
        "pdftotext",
        &[
            "numbered.pdf",
            "-",
            "-f",
            "2",
            "-l",
            "9",
            "-nopgbrk",
            "-eol",
            "dos",
        ],
    ));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "page 2 of the alias fixture\r\npage 3 of the alias fixture\r\n"
    );

    // No text file named: the PDF's name with .txt, beside it.
    ok(run(
        dir.path(),
        "pdftotext",
        &["-layout", "-l", "1", "numbered.pdf"],
    ));
    assert_eq!(files(dir.path()), ["numbered.txt"]);
    let written = std::fs::read_to_string(dir.path().join("numbered.txt")).unwrap();
    assert!(
        written.contains("page 1 of the alias fixture"),
        "{written:?}"
    );
    assert!(written.ends_with('\x0c'));

    // A crop in pixels from the top-left corner: the line sits 100 points
    // above the bottom of a 200-point page, so the top quarter has no text.
    let output = ok(run(
        dir.path(),
        "pdftotext",
        &[
            "-x",
            "0",
            "-y",
            "0",
            "-W",
            "300",
            "-H",
            "50",
            "-l",
            "1",
            "numbered.pdf",
            "-",
        ],
    ));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "\x0c");
    let output = ok(run(
        dir.path(),
        "pdftotext",
        &[
            "-x",
            "0",
            "-y",
            "80",
            "-W",
            "300",
            "-H",
            "40",
            "-l",
            "1",
            "-eol",
            "unix",
            "numbered.pdf",
            "-",
        ],
    ));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "page 1 of the alias fixture\n\x0c"
    );
}

#[test]
fn pdftoppm_names_and_sizes_files_as_poppler_does() {
    let dir = tempfile::tempdir().unwrap();
    numbered_pages(dir.path(), 12);
    let at = |name: &str| dir.path().join(name);

    // Twelve pages: two digits in every name. 300 x 200 points at 36 dpi.
    ok(run(
        dir.path(),
        "pdftoppm",
        &["-png", "-r", "36", "numbered.pdf", "out"],
    ));
    let expected: Vec<String> = (1..=12).map(|page| format!("out-{page:02}.png")).collect();
    assert_eq!(files(dir.path()), expected);
    assert_eq!(png_size(&at("out-07.png")), (150, 100));
    for name in expected {
        std::fs::remove_file(at(&name)).unwrap();
    }

    // -singlefile: the first page of the range, under the bare prefix.
    let output = ok(run(
        dir.path(),
        "pdftoppm",
        &[
            "-png",
            "-singlefile",
            "-f",
            "3",
            "-scale-to",
            "60",
            "numbered.pdf",
            "one",
        ],
    ));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Single file"));
    assert_eq!(files(dir.path()), ["one.png"]);
    assert_eq!(png_size(&at("one.png")), (60, 40));
    std::fs::remove_file(at("one.png")).unwrap();

    // Odd pages, a separator of the caller's, and a crop in pixels.
    ok(run(
        dir.path(),
        "pdftoppm",
        &[
            "-png",
            "-o",
            "-l",
            "4",
            "-sep",
            "_",
            "-r",
            "72",
            "-x",
            "10",
            "-y",
            "20",
            "-W",
            "50",
            "-H",
            "30",
            "numbered.pdf",
            "crop",
        ],
    ));
    assert_eq!(files(dir.path()), ["crop_01.png", "crop_03.png"]);
    assert_eq!(png_size(&at("crop_03.png")), (50, 30));
    for name in files(dir.path()) {
        std::fs::remove_file(at(&name)).unwrap();
    }

    // No format named: PPM, or PGM with -gray; JPEG takes .jpg.
    ok(run(
        dir.path(),
        "pdftoppm",
        &[
            "-gray",
            "-r",
            "36",
            "-f",
            "1",
            "-l",
            "1",
            "numbered.pdf",
            "g",
        ],
    ));
    ok(run(
        dir.path(),
        "pdftoppm",
        &["-r", "36", "-f", "2", "-l", "2", "numbered.pdf", "c"],
    ));
    ok(run(
        dir.path(),
        "pdftoppm",
        &[
            "-jpeg",
            "-jpegopt",
            "quality=60",
            "-r",
            "36",
            "-f",
            "2",
            "-l",
            "2",
            "numbered.pdf",
            "j",
        ],
    ));
    assert_eq!(files(dir.path()), ["c-02.ppm", "g-01.pgm", "j-02.jpg"]);
    let gray = std::fs::read(at("g-01.pgm")).unwrap();
    assert!(gray.starts_with(b"P5\n150 100\n255\n"));
    assert_eq!(gray.len(), b"P5\n150 100\n255\n".len() + 150 * 100);
    assert!(
        std::fs::read(at("c-02.ppm"))
            .unwrap()
            .starts_with(b"P6\n150 100\n255\n")
    );
    assert!(
        std::fs::read(at("j-02.jpg"))
            .unwrap()
            .starts_with(b"\xff\xd8")
    );

    // No prefix: the images one after another on standard output.
    let output = ok(run(
        dir.path(),
        "pdftoppm",
        &["-r", "18", "-f", "1", "-l", "2", "numbered.pdf"],
    ));
    let header = b"P6\n75 50\n255\n";
    assert!(output.stdout.starts_with(header));
    assert_eq!(output.stdout.len(), 2 * (header.len() + 75 * 50 * 3));
}

#[test]
fn failures_use_popplers_exit_codes_and_name_what_was_refused() {
    let dir = tempfile::tempdir().unwrap();
    numbered_pages(dir.path(), 2);
    let stderr = |output: &Output| String::from_utf8_lossy(&output.stderr).into_owned();

    let output = run(dir.path(), "pdftotext", &["missing.pdf", "-"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("Couldn't open file 'missing.pdf'"));

    let output = run(
        dir.path(),
        "pdftotext",
        &["numbered.pdf", "no/such/dir/out.txt"],
    );
    assert_eq!(output.status.code(), Some(2));

    let output = run(
        dir.path(),
        "pdftotext",
        &["-f", "2", "-l", "1", "numbered.pdf", "-"],
    );
    assert_eq!(output.status.code(), Some(99));
    assert!(stderr(&output).contains("Wrong page range"));

    // Refused by name, never ignored: the caller would get the wrong thing.
    let output = run(dir.path(), "pdftotext", &["-bbox", "numbered.pdf", "-"]);
    assert_eq!(output.status.code(), Some(99));
    assert!(stderr(&output).contains("-bbox is not supported"));
    assert!(output.stdout.is_empty());

    let output = run(dir.path(), "pdftoppm", &["-mono", "numbered.pdf", "out"]);
    assert_eq!(output.status.code(), Some(99));
    assert!(stderr(&output).contains("-mono is not supported; use -gray"));

    let output = run(dir.path(), "pdftoppm", &["-webp", "numbered.pdf", "out"]);
    assert_eq!(output.status.code(), Some(99));
    assert!(stderr(&output).contains("unknown option -webp"));
    assert_eq!(files(dir.path()), Vec::<String>::new());

    let output = run(dir.path(), "pdftoppm", &["-h"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).contains("Usage: pdftoppm"));
}

#[test]
fn the_executable_answers_to_popplers_names() {
    let dir = tempfile::tempdir().unwrap();
    numbered_pages(dir.path(), 1);
    let name = format!("pdftotext{}", std::env::consts::EXE_SUFFIX);
    let renamed = dir.path().join(name);
    std::fs::copy(env!("CARGO_BIN_EXE_lege-pdf"), &renamed).expect("copy the executable");

    let output = ok(Command::new(&renamed)
        .current_dir(dir.path())
        .args(["-eol", "unix", "numbered.pdf", "-"])
        .output()
        .expect("run the renamed executable"));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "page 1 of the alias fixture\n\x0c"
    );
}
