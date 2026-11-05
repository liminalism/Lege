//! Tables recovered from where the words sit.
//!
//! A PDF has no tables, only text placed at coordinates. What reads as a
//! table is a run of rows whose words fall into the same few columns, with
//! clear space between the columns on every row. That is what is looked for
//! here. Ruling lines are not consulted, so a ruled table and one laid out
//! with whitespace alone are found the same way, and a table whose cells
//! touch (no whitespace between columns) is not found at all.
//!
//! A wrapped cell comes back as a row of its own with the other cells empty:
//! merging it into the row above would be a guess, and a reader can see the
//! continuation for what it is.

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use pdf_document::DocumentSnapshot;
use pdf_text::TextPageOptions;
use serde_json::Value;

use crate::bounds::Bounds;
use crate::commands::text::{self, OcrMode, Segment, TextLayout};
use crate::commands::{PageOutcome, emit_failed, for_pages, resolve_snapshot};
use crate::open::DocumentIdentity;
use crate::pages::{PageZero, parse_scan_range};
use crate::schema::{Envelope, OutputMode};
use crate::views::tables::{TableView, TablesPageData};
use crate::views::text::WordView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TableFormat {
    Markdown,
    Csv,
}

#[derive(Debug)]
pub struct TablesArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    pub format: TableFormat,
    pub ocr: OcrMode,
    pub ocr_language: &'a str,
    pub system_fonts: bool,
    pub bounds: Bounds,
    pub fail_fast: bool,
    pub output: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

pub fn run(args: TablesArgs<'_>) -> Result<i32> {
    let (identity, snapshot) =
        resolve_snapshot(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    let scan_range = parse_scan_range(args.pages, snapshot.page_count(), args.bounds.scan_pages)?;
    let (pages, mut selection_warnings) = (scan_range.pages, scan_range.warnings);
    let mode = args.output;
    let mut exit = 0;
    let mut found = 0usize;
    let mut failure: Option<anyhow::Error> = None;

    scan(
        &snapshot,
        &pages,
        args.ocr,
        args.ocr_language,
        args.system_fonts,
        &args.bounds,
        |index, page, outcome, mut warnings| {
            let step = || -> Result<ControlFlow<()>> {
                warnings.splice(0..0, std::mem::take(&mut selection_warnings));
                match outcome {
                    PageOutcome::TimedOut => {
                        exit = 1;
                        crate::commands::timed_out(&document, index, pages.len(), mode, true)?;
                        return Ok(ControlFlow::Break(()));
                    }
                    PageOutcome::Failed(error) => {
                        exit = 1;
                        let env = Envelope::page_failed(
                            &document,
                            page.one_based(),
                            format!("{error:#}"),
                        )
                        .with_warnings(std::mem::take(&mut warnings));
                        emit_failed(&env, mode, true)?;
                        if args.fail_fast {
                            return Ok(ControlFlow::Break(()));
                        }
                    }
                    PageOutcome::Done(data) => {
                        found += data.tables.len();
                        match mode {
                            OutputMode::Human => {
                                for warning in &warnings {
                                    eprintln!("warning: {warning}");
                                }
                                for (number, table) in data.tables.iter().enumerate() {
                                    println!(
                                        "{}\n",
                                        table_text(
                                            page.one_based(),
                                            number + 1,
                                            table,
                                            args.format
                                        )
                                    );
                                }
                            }
                            // A page without tables is only worth a record
                            // when it has something to say.
                            OutputMode::Json | OutputMode::Jsonl => {
                                if !data.tables.is_empty() || !warnings.is_empty() {
                                    Envelope::page_ok(
                                        &document,
                                        page.one_based(),
                                        serde_json::to_value(&data)?,
                                    )
                                    .with_warnings(std::mem::take(&mut warnings))
                                    .write_jsonl()?;
                                }
                            }
                        }
                    }
                }
                Ok(ControlFlow::Continue(()))
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
    match mode {
        OutputMode::Human => eprintln!(
            "{found} table{} on {} page{}",
            if found == 1 { "" } else { "s" },
            pages.len(),
            if pages.len() == 1 { "" } else { "s" }
        ),
        OutputMode::Json | OutputMode::Jsonl => Envelope::ok(
            &document,
            serde_json::json!({ "summary": {
                "pages_selected": pages.len(),
                "table_count": found,
            }}),
        )
        .with_warnings(selection_warnings)
        .write_jsonl()?,
    }
    Ok(exit)
}

/// The tables of a page selection as one value, for the services, and
/// each table written out as Markdown under a line saying where it is.
pub fn tables_value(
    snapshot: &DocumentSnapshot,
    pages: Option<&str>,
    ocr: OcrMode,
    ocr_language: &str,
    bounds: &Bounds,
) -> Result<(Value, Vec<String>)> {
    let scan_range = parse_scan_range(pages, snapshot.page_count(), bounds.scan_pages)?;
    let (selection, mut warnings) = (scan_range.pages, scan_range.warnings);
    let mut out = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut found = 0usize;
    let mut stopped = None;
    let mut used = 0u64;
    scan(
        snapshot,
        &selection,
        ocr,
        ocr_language,
        false,
        bounds,
        |index, page, outcome, page_warnings| {
            let mut entry = serde_json::json!({ "page": page.one_based(), "page_index": page.0 });
            let mut page_blocks = Vec::new();
            match outcome {
                PageOutcome::TimedOut => {
                    stopped = Some((index, crate::deadline::message(index, selection.len())));
                    return ControlFlow::Break(());
                }
                PageOutcome::Failed(error) => entry["error"] = format!("{error:#}").into(),
                PageOutcome::Done(data) => {
                    if data.tables.is_empty() && page_warnings.is_empty() {
                        return ControlFlow::Continue(());
                    }
                    found += data.tables.len();
                    page_blocks.extend(data.tables.iter().enumerate().map(|(number, table)| {
                        table_text(page.one_based(), number + 1, table, TableFormat::Markdown)
                    }));
                    entry["tables"] = serde_json::json!(data.tables);
                    entry["text_source"] = data.provenance.source.into();
                }
            }
            if !page_warnings.is_empty() {
                entry["warnings"] = serde_json::json!(page_warnings);
            }
            let size = entry.to_string().len() as u64;
            if bounds.max_bytes > 0 && !out.is_empty() && used + size > bounds.max_bytes {
                stopped = Some((
                    index,
                    format!("byte budget max-bytes={} reached", bounds.max_bytes),
                ));
                return ControlFlow::Break(());
            }
            used += size;
            out.push(entry);
            blocks.extend(page_blocks);
            ControlFlow::Continue(())
        },
    );
    let mut result = serde_json::json!({
        "page_count": snapshot.page_count(),
        "pages_selected": scan_range.selected,
        "table_count": found,
        "truncated": stopped.is_some() || scan_range.next_page.is_some(),
    });
    if let Some((index, reason)) = stopped {
        let next = selection[index].one_based();
        result["next_page"] = next.into();
        warnings.push(format!("{reason}: pages from {next} on were not examined"));
    } else if let Some(next) = scan_range.next_page {
        result["next_page"] = next.into();
    }
    result["pages"] = Value::Array(out);
    result["warnings"] = serde_json::json!(warnings);
    Ok((result, blocks))
}

fn scan(
    snapshot: &DocumentSnapshot,
    pages: &[PageZero],
    ocr: OcrMode,
    ocr_language: &str,
    system_fonts: bool,
    bounds: &Bounds,
    deliver: impl FnMut(usize, PageZero, PageOutcome<TablesPageData>, Vec<String>) -> ControlFlow<()>,
) {
    for_pages(
        pages,
        if ocr == OcrMode::Never {
            bounds.threads
        } else {
            1
        },
        || text::PageState::new(system_fonts),
        |state, page, warnings| {
            let extracted = text::extract_page(
                snapshot,
                &state.compiler,
                page,
                &TextPageOptions::default(),
                TextLayout::Words,
                None,
                ocr,
                ocr_language,
                bounds,
                warnings,
                &mut state.ocr_backend,
                &mut state.glyphs,
            )?;
            Ok(TablesPageData {
                unit: "pdf_points",
                tables: detect_tables(extracted.words.unwrap_or_default()),
                provenance: extracted.provenance,
            })
        },
        deliver,
    );
}

/// A table needs this many rows that have more than one cell.
const MIN_TABLE_ROWS: usize = 2;
/// Rows further apart than this many row heights belong to different tables.
const MAX_ROW_GAP: f64 = 2.5;
/// Space between two columns, in row heights, on every row of the table.
/// Less than the gap that cuts a row into segments: the spaces between the
/// words of a cell do not line up from row to row, so a narrow strip that
/// stays clear on every row is a column boundary all the same.
const MIN_COLUMN_GAP: f64 = 0.45;

/// One row in this many may run across a column boundary without hiding it.
const SPANNING_ROWS: usize = 8;

/// Find the tables among the words of one page.
pub(crate) fn detect_tables(words: Vec<WordView>) -> Vec<TableView> {
    let mut rows: Vec<Vec<Segment>> = Vec::new();
    for segment in text::row_segments(words) {
        match rows.last_mut() {
            Some(row) if row[0].row == segment.row => row.push(segment),
            _ => rows.push(vec![segment]),
        }
    }

    let mut tables = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        if rows[start].len() < 2 {
            start += 1;
            continue;
        }
        // Grow the run while the next row is close and the rows still share
        // at least one column gap.
        let mut end = start;
        while end + 1 < rows.len() {
            let height = text::height(row_box(&rows[end]));
            let gap = row_box(&rows[end])[1] - row_box(&rows[end + 1])[3];
            if gap > MAX_ROW_GAP * height || columns_of(&rows[start..=end + 1], 0).len() < 2 {
                break;
            }
            end += 1;
        }
        // A short line under a table is not part of it.
        while end > start && rows[end].len() < 2 {
            end -= 1;
        }
        let run = &rows[start..=end];
        // A long table may have a few rows that run across its columns — a
        // footnote, a spanning header. They do not erase the boundaries.
        let columns = settle_columns(run, columns_of(run, run.len() / SPANNING_ROWS));
        let cell_rows = run.iter().filter(|row| row.len() >= 2).count();
        let segments: Vec<&Segment> = run.iter().flatten().collect();
        if cell_rows >= MIN_TABLE_ROWS
            && cell_rows * 10 >= run.len() * 6
            && columns.len() >= 2
            // Two columns of running text share a gap on every row too.
            && text::column_cut(&segments).is_none()
        {
            tables.push(build_table(run, &columns));
        }
        start = end + 1;
    }
    tables
}

fn row_box(row: &[Segment]) -> [f64; 4] {
    text::union_boxes(row.iter().map(|segment| segment.bbox))
}

/// The x extents of the columns of `rows`: what is left when every stretch
/// of horizontal space that at most `spanning` rows put text in is taken
/// out.
fn columns_of(rows: &[Vec<Segment>], spanning: usize) -> Vec<(f64, f64)> {
    let gap = MIN_COLUMN_GAP
        * text::median(
            rows.iter()
                .flatten()
                .map(|segment| text::height(segment.bbox))
                .collect(),
        );
    // Sweep the word edges left to right, counting how many words lie over
    // each stretch. Words of one row do not overlap, so that is rows.
    let mut edges: Vec<(f64, i32)> = rows
        .iter()
        .flatten()
        .flat_map(|segment| &segment.words)
        .flat_map(|word| [(word.bbox[0], 1), (word.bbox[2], -1)])
        .collect();
    // At one x, a word ending comes before a word starting.
    edges.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut columns: Vec<(f64, f64)> = Vec::new();
    let mut covering = 0i32;
    // Where the current clear stretch began, if the sweep is in one.
    let mut clear_from: Option<f64> = None;
    let mut column_from: Option<f64> = None;
    for (x, step) in edges {
        let was_clear = covering as usize <= spanning;
        covering += step;
        let is_clear = covering as usize <= spanning;
        match (was_clear, is_clear) {
            // Text begins: the clear stretch before it either was a real
            // boundary, ending the column to its left, or was too narrow.
            (true, false) => {
                if let (Some(from), Some(start)) = (clear_from, column_from)
                    && x - from >= gap
                {
                    columns.push((start, from));
                    column_from = Some(x);
                }
                column_from.get_or_insert(x);
                clear_from = None;
            }
            (false, true) => clear_from = Some(x),
            _ => {}
        }
    }
    if let Some(start) = column_from {
        let end = rows
            .iter()
            .flatten()
            .map(|segment| segment.bbox[2])
            .fold(start, f64::max);
        columns.push((start, clear_from.map_or(end, |from| from.min(end))));
    }
    columns
}

/// The column a word belongs to: the one it sits in, else the nearest.
fn column_at(columns: &[(f64, f64)], word: &WordView) -> usize {
    let centre = (word.bbox[0] + word.bbox[2]) * 0.5;
    let distance = |(left, right): &(f64, f64)| {
        if centre < *left {
            left - centre
        } else if centre > *right {
            centre - right
        } else {
            0.0
        }
    };
    (0..columns.len())
        .min_by(|a, b| distance(&columns[*a]).total_cmp(&distance(&columns[*b])))
        .unwrap_or(0)
}

/// Fold a column that few rows use into its neighbour. Where spanning rows
/// are tolerated, the ragged end of a column of sentences can look like a
/// narrow column of its own; a real column has something on many rows.
fn settle_columns(rows: &[Vec<Segment>], mut columns: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    let needed = (rows.len() / 5).max(2);
    loop {
        if columns.len() < 2 {
            return columns;
        }
        let mut used = vec![0usize; columns.len()];
        for row in rows {
            let mut seen = vec![false; columns.len()];
            for word in row.iter().flat_map(|segment| &segment.words) {
                seen[column_at(&columns, word)] = true;
            }
            for (count, seen) in used.iter_mut().zip(seen) {
                *count += usize::from(seen);
            }
        }
        let Some(thin) = (0..columns.len()).find(|column| used[*column] < needed) else {
            return columns;
        };
        // Into the left neighbour: text runs on from it.
        let into = if thin == 0 { 1 } else { thin - 1 };
        columns[into] = (
            columns[into].0.min(columns[thin].0),
            columns[into].1.max(columns[thin].1),
        );
        columns.remove(thin);
    }
}

fn build_table(rows: &[Vec<Segment>], columns: &[(f64, f64)]) -> TableView {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut cells = vec![String::new(); columns.len()];
            for word in row.iter().flat_map(|segment| &segment.words) {
                let cell = &mut cells[column_at(columns, word)];
                if !cell.is_empty() {
                    cell.push(' ');
                }
                cell.push_str(&word.text);
            }
            cells
        })
        .collect();
    TableView {
        bbox: text::union_boxes(rows.iter().flatten().map(|segment| segment.bbox)),
        rows: cells.len(),
        columns: columns.len(),
        cells,
    }
}

/// A table as text, under a line saying where it is.
pub(crate) fn table_text(
    page: u32,
    number: usize,
    table: &TableView,
    format: TableFormat,
) -> String {
    let mut out = format!(
        "p.{page} table {number} ({} rows x {} columns) [{}]",
        table.rows,
        table.columns,
        table
            .bbox
            .map(|edge| crate::views::round2(edge).to_string())
            .join(",")
    );
    for (index, row) in table.cells.iter().enumerate() {
        out.push('\n');
        match format {
            TableFormat::Markdown => {
                out.push('|');
                for cell in row {
                    out.push(' ');
                    out.push_str(&cell.replace('|', "\\|"));
                    out.push_str(" |");
                }
                if index == 0 {
                    out.push('\n');
                    out.push('|');
                    out.push_str(&"---|".repeat(row.len()));
                }
            }
            TableFormat::Csv => {
                let line: Vec<String> = row
                    .iter()
                    .map(|cell| {
                        if cell.contains([',', '"', '\n']) {
                            format!("\"{}\"", cell.replace('"', "\"\""))
                        } else {
                            cell.clone()
                        }
                    })
                    .collect();
                out.push_str(&line.join(","));
            }
        }
    }
    out
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

    fn three_column_table() -> Vec<WordView> {
        let mut words = Vec::new();
        for (y, cells) in [
            (680.0, ["Name", "Width", "Meaning"]),
            (668.0, ["SBW", "32", "region width"]),
            (656.0, ["SBH", "32", "region height"]),
            (644.0, ["SBDSOFFSET", "5", "strip offset"]),
        ] {
            for (x, cell) in [100.0, 200.0, 280.0].into_iter().zip(cells) {
                words.extend(row(y, x, cell));
            }
        }
        words
    }

    #[test]
    fn rows_of_aligned_cells_are_a_table() {
        let mut words = row(
            720.0,
            100.0,
            "A paragraph of running text that stands above the table",
        );
        words.extend(three_column_table());
        words.extend(row(
            600.0,
            100.0,
            "Another paragraph of running text that stands below it",
        ));

        let tables = detect_tables(words);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!((table.rows, table.columns), (4, 3));
        assert_eq!(table.cells[0], ["Name", "Width", "Meaning"]);
        assert_eq!(table.cells[3], ["SBDSOFFSET", "5", "strip offset"]);
        assert_eq!(table.bbox[1], 644.0);
    }

    #[test]
    fn an_empty_cell_stays_in_its_column() {
        let mut words = three_column_table();
        // A row with nothing in the middle column.
        words.extend(row(632.0, 100.0, "SBRTEMPLATE"));
        words.extend(row(632.0, 280.0, "refinement template"));
        let tables = detect_tables(words);
        assert_eq!(
            tables[0].cells[4],
            ["SBRTEMPLATE", "", "refinement template"]
        );
    }

    /// A footnote under a long table runs across its first columns; they
    /// are still separate columns.
    #[test]
    fn a_few_spanning_rows_do_not_merge_columns() {
        let mut words = Vec::new();
        for index in 0..16 {
            let y = 700.0 - 12.0 * index as f64;
            words.extend(row(y, 100.0, "NAME"));
            words.extend(row(y, 140.0, "8"));
            words.extend(row(y, 260.0, "what it means"));
        }
        // Reaches from the first column into the second, not the third.
        words.extend(row(508.0, 100.0, "a) unused if"));
        words.extend(row(508.0, 260.0, "see above"));
        let tables = detect_tables(words);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns, 3);
        assert_eq!(tables[0].cells[0], ["NAME", "8", "what it means"]);
    }

    #[test]
    fn two_columns_of_prose_are_not_a_table() {
        let mut words = Vec::new();
        for y in [680.0, 668.0, 656.0, 644.0] {
            words.extend(row(y, 40.0, "left column line of running text"));
            words.extend(row(y, 320.0, "right column line of running text"));
        }
        assert!(detect_tables(words).is_empty());
    }

    #[test]
    fn plain_paragraphs_are_not_a_table() {
        let mut words = Vec::new();
        for y in [680.0, 668.0, 656.0] {
            words.extend(row(y, 40.0, "a single column of ordinary running text"));
        }
        assert!(detect_tables(words).is_empty());
    }

    #[test]
    fn tables_are_written_as_markdown_or_csv() {
        let table = TableView {
            bbox: [100.0, 644.0, 345.0, 688.0],
            rows: 2,
            columns: 2,
            cells: vec![
                vec!["Name".into(), "Note".into()],
                vec!["a|b".into(), "x, \"y\"".into()],
            ],
        };
        assert_eq!(
            table_text(3, 1, &table, TableFormat::Markdown),
            "p.3 table 1 (2 rows x 2 columns) [100,644,345,688]\n| Name | Note |\n|---|---|\n| a\\|b | x, \"y\" |"
        );
        assert_eq!(
            table_text(3, 1, &table, TableFormat::Csv),
            "p.3 table 1 (2 rows x 2 columns) [100,644,345,688]\nName,Note\na|b,\"x, \"\"y\"\"\""
        );
    }
}
