//! The limits and failure paths an agent leans on: a crop that crops, output
//! to stdout, failures as records, and coverage that is stated, not implied.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pdf_test_support::builder::PdfBuilder;
use serde_json::{Value, json};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../render/crates/pdf-chaos-tests/tests/fixtures/hello_world.pdf")
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lege-pdf"))
}

fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}): {:?}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "PNG magic missing");
    let field = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (field(16), field(20))
}

/// A document of `pages` pages, each reading `page N of the limits fixture`.
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
            "BT /F1 12 Tf 20 100 Td (page {} of the limits fixture) Tj ET",
            page + 1
        );
        builder.add_stream(100 + page, "", content.as_bytes());
    }
    builder.finish_classic_xref("/Root 1 0 R");
    let path = dir.join("numbered.pdf");
    std::fs::write(&path, builder.into_bytes()).expect("write fixture");
    path
}

#[test]
fn render_crop_produces_only_the_requested_region() {
    let dir = tempfile::tempdir().unwrap();
    let render = |name: &str, crop: Option<&str>| -> (Value, (u32, u32)) {
        let out = dir.path().join(name);
        let mut command = bin();
        command.arg("render").arg(fixture()).args([
            "--pages",
            "1",
            "--dpi",
            "72",
            "--output",
            out.to_str().unwrap(),
            "--json",
        ]);
        if let Some(crop) = crop {
            command.args(["--crop", crop]);
        }
        let output = command.output().expect("run lege-pdf render");
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        let size = png_size(&std::fs::read(&out).expect("png exists"));
        (stdout_json(&output), size)
    };

    let (full, (full_width, full_height)) = render("full.png", None);
    assert!(full["data"].get("region").is_none());
    assert!(full_width > 100 && full_height > 100);

    // The lower-left 100 x 50 points of the page, at one pixel per point.
    let (cropped, size) = render("crop.png", Some("0,0,100,50"));
    assert_eq!(size, (100, 50));
    assert_eq!(cropped["data"]["width"], 100);
    assert_eq!(cropped["data"]["height"], 50);
    assert_eq!(cropped["data"]["region"], json!([0.0, 0.0, 100.0, 50.0]));

    // A crop that misses the page is an error, not a blank image.
    let output = bin()
        .arg("render")
        .arg(fixture())
        .args(["--pages", "1", "--crop", "5000,5000,5100,5100", "--json"])
        .args(["--output", dir.path().join("miss.png").to_str().unwrap()])
        .output()
        .expect("run lege-pdf render");
    assert!(!output.status.success());
    assert!(
        stdout_json(&output)["data"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("does not intersect")),
    );
}

#[test]
fn render_output_dash_streams_one_png_and_creates_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let output = bin()
        .current_dir(dir.path())
        .arg("render")
        .arg(fixture())
        .args(["--pages", "1", "--thumbnail", "--output", "-"])
        .output()
        .expect("run lege-pdf render");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (width, height) = png_size(&output.stdout);
    assert!(width > 0 && height > 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("wrote -"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    // Records and an image cannot share stdout.
    let output = bin()
        .current_dir(dir.path())
        .arg("render")
        .arg(fixture())
        .args(["--pages", "1", "--output", "-", "--json"])
        .output()
        .expect("run lege-pdf render");
    assert!(!output.status.success());
    assert_eq!(stdout_json(&output)["status"], "failed");
}

#[test]
fn a_failure_before_any_page_is_still_a_json_record() {
    let output = bin()
        .args(["text", "definitely-missing.pdf", "--json"])
        .output()
        .expect("run lege-pdf text");
    assert_eq!(output.status.code(), Some(1));
    let value = stdout_json(&output);
    assert_eq!(value["schema"], "lege-pdf.agent/v1");
    assert_eq!(value["status"], "failed");
    let error = value["data"]["error"].as_str().expect("error text");
    assert!(error.contains("definitely-missing.pdf"), "{error}");
    // The operating system's reason survives, not just the path.
    assert!(
        error.len() > "cannot read definitely-missing.pdf".len(),
        "{error}"
    );
}

#[test]
fn page_ranges_clamp_past_the_end_and_accept_open_ends() {
    let dir = tempfile::tempdir().unwrap();
    let document = numbered_pages(dir.path(), 3);

    let output = bin()
        .arg("text")
        .arg(&document)
        .args(["--pages", "2-9", "--jsonl"])
        .output()
        .expect("run lege-pdf text");
    assert!(output.status.success());
    let records: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("jsonl record"))
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|r| r["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(
        records[0]["warnings"][0]
            .as_str()
            .is_some_and(|warning| warning.contains("clamped to 2-3")),
    );
    assert_eq!(records[1]["warnings"], json!([]));

    let output = bin()
        .arg("text")
        .arg(&document)
        .args(["--pages", "3-"])
        .output()
        .expect("run lege-pdf text");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("page 3 of the limits fixture"));
}

#[test]
fn mcp_search_states_what_a_page_limit_left_unsearched() {
    let dir = tempfile::tempdir().unwrap();
    let document = numbered_pages(dir.path(), 3);

    let mut child = bin()
        .args(["--max-pages", "2", "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lege-pdf mcp");
    {
        let mut stdin = child.stdin.take().expect("MCP stdin");
        for (id, name, arguments) in [
            (
                1,
                "pdf_search",
                json!({ "path": document, "query": "limits fixture", "response_format": "json" }),
            ),
            (
                2,
                "pdf_text",
                json!({ "path": document, "pages": "3-", "response_format": "json" }),
            ),
            (
                3,
                "pdf_text",
                json!({ "path": document, "pages": "all", "response_format": "json" }),
            ),
        ] {
            let request = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            });
            serde_json::to_writer(&mut stdin, &request).expect("serialize request");
            stdin.write_all(b"\n").expect("write request delimiter");
        }
    }
    let output = child.wait_with_output().expect("wait for MCP server");
    assert!(output.status.success());
    let results: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON-RPC response"))
        .map(|response| response["result"]["structuredContent"].clone())
        .collect();

    // Two of three pages searched: the result must say so and say where to
    // resume, rather than look like a complete search with two hits.
    let search = &results[0];
    assert_eq!(search["page_count"], 3);
    assert_eq!(search["pages_selected"], 3);
    assert_eq!(search["truncated"], true);
    assert_eq!(search["next_page"], 3);
    assert_eq!(search["pages"].as_array().unwrap().len(), 2);
    assert!(
        search["warnings"][0]
            .as_str()
            .is_some_and(|warning| warning.contains("max-pages=2")),
    );

    // The resumed call covers the rest and reports itself complete.
    let rest = &results[1];
    assert_eq!(rest["truncated"], false);
    assert!(rest.get("next_page").is_none());
    assert_eq!(rest["warnings"], json!([]));
    assert_eq!(rest["pages"][0]["page"], 3);
    assert!(
        rest["pages"][0]["data"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("page 3 of the limits fixture")),
    );

    assert_eq!(results[2]["truncated"], true);
    assert_eq!(results[2]["next_page"], 3);
}
