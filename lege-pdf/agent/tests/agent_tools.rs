//! The commands an agent reaches for on a document it has not seen, end to
//! end on one small fixture: a table, a word broken across lines, a link, an
//! outline, printed page numbers and a blank page.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use base64::Engine;
use pdf_test_support::builder::PdfBuilder;
use serde_json::{Value, json};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lege-pdf"))
}

fn text_at(x: u32, y: u32, text: &str) -> String {
    format!("BT /F1 10 Tf {x} {y} Td ({text}) Tj ET\n")
}

/// Four 400 x 300 pages, labelled i, ii, 1, 2:
///
/// 1. a table of four rows and three columns;
/// 2. `inter-` / `national` broken across two lines, the second line under
///    a link to page 3;
/// 3. one line, the target of the outline's only item;
/// 4. nothing.
fn fixture(dir: &Path) -> PathBuf {
    let mut builder = PdfBuilder::new();
    builder.add_object(
        1,
        "<</Type/Catalog/Pages 2 0 R/Outlines 5 0 R\
         /PageLabels<</Nums[0<</S/r>> 2<</S/D/St 1>>]>>>>",
    );
    builder.add_object(
        2,
        "<</Type/Pages/Kids[10 0 R 11 0 R 12 0 R 13 0 R]/Count 4/MediaBox[0 0 400 300]>>",
    );
    builder.add_object(
        3,
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>",
    );
    builder.add_object(4, "<</Title(Agent tools fixture)/Author(Lege tests)>>");
    builder.add_object(5, "<</Type/Outlines/First 6 0 R/Last 6 0 R/Count 1>>");
    builder.add_object(6, "<</Title(Chapter Two)/Parent 5 0 R/Dest[12 0 R/Fit]>>");
    for page in 0..4u32 {
        let annotations = if page == 1 { "/Annots[20 0 R]" } else { "" };
        builder.add_object(
            10 + page,
            &format!(
                "<</Type/Page/Parent 2 0 R/Contents {} 0 R\
                 /Resources<</Font<</F1 3 0 R>>>>{annotations}>>",
                100 + page
            ),
        );
    }
    builder.add_object(
        20,
        "<</Type/Annot/Subtype/Link/Rect[38 182 200 196]/Border[0 0 0]/Dest[12 0 R/Fit]>>",
    );

    let rows = [
        ["Name", "Type", "Size"],
        ["alpha", "integer", "4"],
        ["beta", "float", "8"],
        ["gamma", "string", "16"],
    ];
    let mut table = String::new();
    for (row, cells) in rows.iter().enumerate() {
        for (column, cell) in cells.iter().enumerate() {
            table.push_str(&text_at(
                40 + 120 * column as u32,
                260 - 14 * row as u32,
                cell,
            ));
        }
    }
    builder.add_stream(100, "", table.as_bytes());
    let broken = text_at(40, 200, "The committee adopted the inter-")
        + &text_at(40, 186, "national standard last year.");
    builder.add_stream(101, "", broken.as_bytes());
    builder.add_stream(
        102,
        "",
        text_at(40, 260, "Chapter two begins here.").as_bytes(),
    );
    builder.add_stream(103, "", b"");
    builder.finish_classic_xref("/Root 1 0 R/Info 4 0 R");

    let path = dir.join("tools.pdf");
    std::fs::write(&path, builder.into_bytes()).expect("write fixture");
    path
}

fn run(document: &Path, arguments: &[&str]) -> Output {
    let (command, rest) = arguments.split_first().expect("a command");
    bin()
        .arg(command)
        .arg(document)
        .args(rest)
        .output()
        .expect("run lege-pdf")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

/// The records a command wrote, one JSON value per line.
fn records(output: &Output) -> Vec<Value> {
    stdout(output)
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|err| {
                panic!(
                    "not a JSON record ({err}): {line:?}; stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
        })
        .collect()
}

/// The one pretty-printed record a `--json` command wrote.
fn record(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}): {:?}; stderr={}",
            stdout(output),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn bbox(value: &Value) -> [f64; 4] {
    let numbers: Vec<f64> = value
        .as_array()
        .expect("a box")
        .iter()
        .map(|number| number.as_f64().expect("a number"))
        .collect();
    numbers.try_into().expect("four numbers")
}

/// Call MCP tools in order and return each call's `result`.
fn mcp(calls: &[(&str, Value)]) -> Vec<Value> {
    let mut child = bin()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lege-pdf mcp");
    {
        let mut stdin = child.stdin.take().expect("MCP stdin");
        for (id, (name, arguments)) in calls.iter().enumerate() {
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
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON-RPC response"))
        .map(|response| {
            assert!(response.get("error").is_none(), "{response}");
            response["result"].clone()
        })
        .collect()
}

fn reply_text(result: &Value) -> &str {
    assert_eq!(result["isError"], false, "{result}");
    result["content"][0]["text"].as_str().expect("text block")
}

#[test]
fn search_writes_matching_pages_and_then_what_it_covered() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    // The page text joins the broken word, so the phrase is found; its box
    // covers both lines.
    let output = run(&document, &["search", "international", "--jsonl"]);
    assert!(output.status.success());
    let found = records(&output);
    assert_eq!(found.len(), 2, "one matching page and the summary");
    assert_eq!(found[0]["page"], 2);
    let hit = &found[0]["data"]["matches"][0];
    assert_eq!(hit["text"], "international");
    let [_, y0, _, y1] = bbox(&hit["bbox"]);
    assert!(y0 < 190.0 && y1 > 200.0, "box spans both lines: {hit}");
    let summary = &found[1]["data"]["summary"];
    assert_eq!(summary["match_count"], 1);
    assert_eq!(summary["pages_with_matches"], 1);
    assert_eq!(summary["pages_searched"], 4);
    assert_eq!(summary["truncated"], false);

    // A match after the line-end hyphen is boxed where it is, on the second
    // line and to the right of `national standard`.
    let found = records(&run(&document, &["search", "last year", "--jsonl"]));
    let [x0, _, x1, y1] = bbox(&found[0]["data"]["matches"][0]["bbox"]);
    assert!(x0 > 115.0 && x1 < 165.0 && y1 < 195.0, "{:?}", found[0]);

    // Nothing found is a summary saying so, not silence and not a failure.
    let output = run(
        &document,
        &["search", "ALPHA", "--case-sensitive", "--jsonl"],
    );
    assert!(output.status.success());
    let found = records(&output);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["data"]["summary"]["match_count"], 0);
    assert_eq!(found[0]["data"]["summary"]["pages_searched"], 4);

    let found = records(&run(
        &document,
        &["search", "al(pha|so)", "--regex", "--jsonl"],
    ));
    assert_eq!(found[0]["page"], 1);
    assert_eq!(found[0]["data"]["matches"][0]["text"], "alpha");
}

#[test]
fn a_page_limit_on_a_scan_says_where_to_resume() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    let output = bin()
        .args(["--max-pages", "2", "search"])
        .arg(&document)
        .args(["the", "--jsonl"])
        .output()
        .expect("run lege-pdf search");
    let found = records(&output);
    let last = found.last().expect("a summary record");
    assert_eq!(last["status"], "truncated");
    let summary = &last["data"]["summary"];
    assert_eq!(summary["pages_selected"], 4);
    assert_eq!(summary["pages_searched"], 2);
    assert_eq!(summary["truncated"], true);
    assert_eq!(summary["next_page"], 3);

    let output = bin()
        .args(["--max-pages", "2", "map"])
        .arg(&document)
        .arg("--json")
        .output()
        .expect("run lege-pdf map");
    // A limit the caller set is not a failure.
    assert!(output.status.success());
    let map = record(&output);
    assert_eq!(map["status"], "truncated");
    assert_eq!(map["data"]["pages"].as_array().unwrap().len(), 2);
    assert_eq!(map["data"]["pages_selected"], 4);
    assert_eq!(map["data"]["next_page"], 3);
    assert!(
        map["warnings"][0]
            .as_str()
            .is_some_and(|warning| warning.contains("max-pages=2")),
    );
}

#[test]
fn tables_come_back_as_rows_of_cells() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    let found = records(&run(&document, &["tables", "--jsonl"]));
    assert_eq!(found[0]["page"], 1);
    let table = &found[0]["data"]["tables"][0];
    assert_eq!((&table["rows"], &table["columns"]), (&json!(4), &json!(3)));
    assert_eq!(
        table["cells"],
        json!([
            ["Name", "Type", "Size"],
            ["alpha", "integer", "4"],
            ["beta", "float", "8"],
            ["gamma", "string", "16"]
        ])
    );
    // The prose pages are not mistaken for tables.
    assert_eq!(found.last().unwrap()["data"]["summary"]["table_count"], 1);

    let csv = stdout(&run(&document, &["tables", "--format", "csv"]));
    assert!(csv.contains("Name,Type,Size\nalpha,integer,4\n"), "{csv}");
    let markdown = stdout(&run(&document, &["tables"]));
    assert!(markdown.contains("| beta | float | 8 |"), "{markdown}");
}

#[test]
fn map_describes_every_page_in_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    let map = record(&run(&document, &["map", "--json"]));
    let pages = map["data"]["pages"].as_array().expect("pages");
    assert_eq!(pages.len(), 4);
    let column = |key: &str| -> Vec<&Value> { pages.iter().map(|page| &page[key]).collect() };
    assert_eq!(
        column("label"),
        [&json!("i"), &json!("ii"), &json!("1"), &json!("2")]
    );
    assert_eq!(
        column("kind"),
        ["native-text", "native-text", "native-text", "blank"]
            .map(|kind| json!(kind))
            .iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(pages[2]["first_line"], "Chapter two begins here.");
    assert_eq!(map["data"]["kinds"]["native-text"], 3);
    assert_eq!(map["data"]["truncated"], false);

    let lines = stdout(&run(&document, &["map"]));
    assert!(lines.contains("p.4 (2) 400x300 blank"), "{lines}");
}

#[test]
fn annotations_give_a_link_its_text_and_target() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    let found = record(&run(&document, &["annotations", "--json"]));
    let data = &found["data"];
    assert_eq!(data["link_count"], 1);
    assert_eq!(data["pages"][0]["page"], 2);
    let link = &data["pages"][0]["links"][0];
    assert_eq!(link["target_page"], 3);
    // Only the second line is under the link: the half of the broken word
    // on the first line is not.
    assert_eq!(link["text"], "national standard last year.");
}

#[test]
fn check_reports_findings_and_strict_fails_on_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    let output = run(&document, &["check", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let data = &record(&output)["data"];
    assert_eq!(data["pages_checked"], 4);
    assert_eq!(data["errors"], 0);
    let findings = data["findings"].as_array().expect("findings");
    let finding = |code: &str| {
        findings
            .iter()
            .find(|finding| finding["code"] == code)
            .unwrap_or_else(|| panic!("no {code} finding in {findings:?}"))
    };
    assert_eq!(finding("blank-page")["pages"], "4");
    assert_eq!(finding("font-not-embedded")["severity"], "warning");

    let output = run(&document, &["check", "--strict"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn inspect_summarises_and_details_pages_only_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    let data = &record(&run(&document, &["inspect", "--json"]))["data"];
    assert_eq!(data["page_count"], 4);
    assert_eq!(data["metadata"]["title"], "Agent tools fixture");
    assert_eq!(
        data["outline"],
        json!([{ "depth": 0, "page": 3, "title": "Chapter Two" }])
    );
    assert_eq!(data["page_labels"][0]["first"], "i");
    assert_eq!(data["page_labels"][1]["pages"], "3-4");
    assert_eq!(data["page_sizes"][0]["count"], 4);
    assert_eq!(data["pages"], json!([]));

    let data = &record(&run(&document, &["inspect", "--pages", "2", "--json"]))["data"];
    assert_eq!(data["pages"].as_array().unwrap().len(), 1);
}

#[test]
fn a_word_broken_across_lines_is_two_boxes_and_physical_text_keeps_columns() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());

    let page = record(&run(
        &document,
        &["text", "--pages", "2", "--layout", "words", "--json"],
    ));
    let text = page["data"]["text"].as_str().expect("text");
    let words = page["data"]["words"].as_array().expect("words");
    let word = |text: &str| {
        words
            .iter()
            .find(|word| word["text"] == text)
            .unwrap_or_else(|| panic!("no word {text:?} in {words:?}"))
    };
    let (first, second) = (word("inter-"), word("national"));
    assert_eq!(first["continued"], true);
    assert_eq!(second["continued"], false);
    let (upper, lower) = (bbox(&first["bbox"]), bbox(&second["bbox"]));
    assert!(lower[3] < upper[1], "one box per line: {upper:?} {lower:?}");
    assert!(lower[0] < 45.0 && upper[0] > 150.0);
    // Offsets count characters of the page text, which has no hyphen there.
    let chars: Vec<char> = text.chars().collect();
    let slice = |word: &Value| -> String {
        let start = word["first_char"].as_u64().unwrap() as usize;
        let count = word["char_count"].as_u64().unwrap() as usize;
        chars[start..start + count].iter().collect()
    };
    assert_eq!(slice(first), "inter");
    assert_eq!(slice(second), "national");
    assert_eq!(slice(word("year.")), "year.");

    let physical = stdout(&run(
        &document,
        &["text", "--pages", "1", "--layout", "physical"],
    ));
    let lines: Vec<&str> = physical.lines().collect();
    assert_eq!(lines[0].find("Type"), lines[1].find("integer"));
    assert_eq!(lines[0].find("Size"), lines[3].find("16"));
    assert!(lines[0].find("Type").is_some_and(|column| column > 10));
}

#[test]
fn mcp_answers_in_text_and_in_json_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());
    let path = document.to_str().unwrap();

    let results = mcp(&[
        ("pdf_map", json!({ "path": path })),
        ("pdf_tables", json!({ "path": path })),
        ("pdf_annotations", json!({ "path": path })),
        ("pdf_check", json!({ "path": path })),
        ("pdf_inspect", json!({ "path": path })),
        (
            "pdf_search",
            json!({ "path": path, "query": "a", "max_matches": 2 }),
        ),
        (
            "pdf_search",
            json!({ "path": path, "query": "a", "max_matches": 2, "response_format": "json" }),
        ),
    ]);

    let map = reply_text(&results[0]);
    assert!(results[0].get("structuredContent").is_none());
    assert!(map.starts_with("4 of 4 pages mapped"), "{map}");
    assert!(map.contains("p.2 (ii) 400x300 native-text"), "{map}");

    assert!(reply_text(&results[1]).contains("| alpha | integer | 4 |"));
    assert!(reply_text(&results[2]).contains("\"national standard last year.\" -> p.3"));
    assert!(reply_text(&results[3]).contains("note blank-page p.4"));
    let inspect = reply_text(&results[4]);
    assert!(inspect.contains("title: Agent tools fixture"), "{inspect}");
    assert!(inspect.contains("Chapter Two (p.3)"), "{inspect}");

    // The match limit stops the search and names the page to go on from.
    let search = reply_text(&results[5]);
    assert!(search.contains("max-matches=2"), "{search}");
    assert!(search.contains("resume from page 1"), "{search}");
    assert_eq!(
        search
            .lines()
            .filter(|line| line.starts_with("p.1 ["))
            .count(),
        2
    );

    let structured = &results[6]["structuredContent"];
    assert_eq!(structured["match_count"], 2);
    assert_eq!(structured["truncated"], true);
    assert_eq!(structured["next_page"], 1);
    let mirrored: Value = serde_json::from_str(reply_text(&results[6])).expect("JSON text");
    assert_eq!(&mirrored, structured);
}

#[test]
fn mcp_text_is_cut_at_a_budget_and_resumes_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());
    let path = document.to_str().unwrap();

    let results = mcp(&[
        ("pdf_text", json!({ "path": path })),
        ("pdf_text", json!({ "path": path, "max_chars": 60 })),
        (
            "pdf_text",
            json!({ "path": path, "max_chars": 60, "response_format": "json" }),
        ),
        (
            "pdf_text",
            json!({ "path": path, "pages": "1-", "start_char": 60, "max_chars": 80, "response_format": "json" }),
        ),
    ]);

    // No selection: the document from the start, a header over each page.
    let whole = reply_text(&results[0]);
    assert!(
        whole.starts_with("--- page 1 ---\nName Type Size\n"),
        "{whole}"
    );
    assert!(
        whole.contains("--- page 3 ---\nChapter two begins here."),
        "{whole}"
    );
    assert!(whole.contains("--- page 4, blank page ---"), "{whole}");
    assert!(!whole.contains('\r'));

    let cut = reply_text(&results[1]);
    assert!(
        cut.starts_with("warning: character budget max_chars=60 reached inside page 1"),
        "{cut}"
    );
    assert!(cut.contains("start_char=60"), "{cut}");

    let first = &results[2]["structuredContent"];
    assert_eq!(first["truncated"], true);
    assert_eq!(
        (&first["next_page"], &first["next_char"]),
        (&json!(1), &json!(60))
    );
    let head = first["pages"][0]["data"]["text"].as_str().unwrap();
    assert_eq!(head.chars().count(), 60);

    // The continuation starts with the character the first call stopped at.
    let second = &results[3]["structuredContent"];
    assert_eq!(second["pages"][0]["start_char"], 60);
    let tail = second["pages"][0]["data"]["text"].as_str().unwrap();
    assert_eq!(
        format!("{head}{tail}").replace("\r\n", "\n"),
        "Name Type Size\nalpha integer 4\nbeta float 8\ngamma string 16"
    );
    assert_eq!(second["pages"][1]["page"], 2);
}

#[test]
fn mcp_render_returns_images_unless_given_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let document = fixture(dir.path());
    let path = document.to_str().unwrap();
    let saved = dir.path().join("page-{page}.png");

    let results = mcp(&[
        ("pdf_render", json!({ "path": path, "pages": "2" })),
        (
            "pdf_render",
            json!({ "path": path, "pages": "1-4", "contact_sheet": true }),
        ),
        (
            "pdf_render",
            json!({ "path": path, "pages": "1-2", "output": saved, "max_edge": 200 }),
        ),
    ]);

    let png = |block: &Value| -> Vec<u8> {
        assert_eq!(block["type"], "image", "{block}");
        assert_eq!(block["mimeType"], "image/png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(block["data"].as_str().expect("image data"))
            .expect("base64");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "PNG magic missing");
        bytes
    };
    let size = |bytes: &[u8]| {
        let field = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        (field(16), field(20))
    };

    // One page: a line about it and the page itself, sized for reading.
    assert!(reply_text(&results[0]).starts_with("page 2: 1568x1176 px"));
    let content = results[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(size(&png(&content[1])), (1568, 1176));

    // Four pages on one sheet: one image, not four.
    assert!(reply_text(&results[1]).starts_with("contact sheet of 4 pages"));
    let content = results[1]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    png(&content[1]);

    // With a path the images go to disk and only their description returns.
    let content = results[2]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "{content:?}");
    for page in 1..=2 {
        let file = dir.path().join(format!("page-{page}.png"));
        let bytes = std::fs::read(&file).unwrap_or_else(|err| panic!("{}: {err}", file.display()));
        assert_eq!(size(&bytes), (200, 150));
    }
}
