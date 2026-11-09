//! Command line: every non-interactive operation, plus `open` for the window.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::params;
use serde_json::Value;

use crate::annotation::commands::{Annotation, ReviewStatus};
use crate::annotation::labels::{class, Group};
use crate::app::{self, percentile, AnnotatorApp, AppConfig};
use crate::dataset::database::{self, PageFilter};
use crate::dataset::export_coco::{export, ExportOptions};
use crate::dataset::import_coco::{import_coco, import_predictions, ImportOptions};
use crate::review::comparison::{match_boxes, Det};
use crate::review::metrics::Accumulator;
use crate::review::queue::Signals;

pub const USAGE: &str = "\
Liminal Annotator — fast box review for page-layout datasets

USAGE
  liminal-annotator <project.db> [queue options]          open the editor
  liminal-annotator open <project.db> [queue options]
  liminal-annotator import <project.db> <coco.json> [--set NAME] [--images-root DIR]
  liminal-annotator import-dataset <project.db> <data-dir>
        imports regression, dev_gold_prefill, final_test_prefill, manual_queue_prefill
  liminal-annotator import-preds <project.db> <results.json> --model NAME
        [--set SET] [--scale-from 640] [--min-score 0.05]
  liminal-annotator export <project.db> [--out DIR] [--name NAME] [--size 640]...
        [--images] [--reviewed-only] [--allow-split-leak] [queue options]
  liminal-annotator stats <project.db>
  liminal-annotator priority <project.db> [--model NAME]
  liminal-annotator compare <project.db> --model NAME [--threshold 0.35] [--json FILE] [queue options]
  liminal-annotator bench [--image FILE.jpg] [--frames N]
  liminal-annotator gen-synthetic <project.db> --pages N [--images-dir DIR]
  liminal-annotator screenshot <project.db> <out.png> [--size 1600x1000] [--scale 1.25]
        [--keys Tab,2,c] [--start PAGE_KEY] [queue options]   render one frame headlessly

QUEUE OPTIONS
  --set NAME  --split NAME  --book ID  --reason TEXT  --status unseen,opened,...
  --priority (order by review priority, diversified by book)  --start PAGE_KEY
  --pred-threshold 0.3 (prediction overlay score)
";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const BOOL_FLAGS: [&str; 6] = [
    "--images",
    "--reviewed-only",
    "--allow-split-leak",
    "--priority",
    "--help",
    "-h",
];

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let a = &raw[i];
            if a.starts_with('-') {
                if let Some((k, v)) = a.split_once('=') {
                    flags.push((k.to_owned(), Some(v.to_owned())));
                } else if BOOL_FLAGS.contains(&a.as_str()) || i + 1 >= raw.len() {
                    flags.push((a.clone(), None));
                } else {
                    flags.push((a.clone(), Some(raw[i + 1].clone())));
                    i += 1;
                }
            } else {
                positional.push(a.clone());
            }
            i += 1;
        }
        Args { positional, flags }
    }

    fn get(&self, k: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(f, _)| f == k)
            .and_then(|(_, v)| v.as_deref())
    }

    fn all(&self, k: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(f, _)| f == k)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == k)
    }

    fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> Result<T, String> {
        match self.get(k) {
            Some(v) => v.parse().map_err(|_| format!("{k}: not a number: {v}")),
            None => Ok(default),
        }
    }

    fn filter(&self) -> Result<PageFilter, String> {
        let mut statuses = Vec::new();
        if let Some(s) = self.get("--status") {
            for part in s.split(',').filter(|p| !p.is_empty()) {
                statuses.push(
                    ReviewStatus::parse(part).ok_or_else(|| format!("unknown status {part:?}"))?,
                );
            }
        }
        Ok(PageFilter {
            set_name: self.get("--set").map(str::to_owned),
            split: self.get("--split").map(str::to_owned),
            book_id: self.get("--book").map(str::to_owned),
            statuses,
            reason: self.get("--reason").map(str::to_owned),
            by_priority: self.has("--priority"),
        })
    }
}

fn project_arg(a: &Args, i: usize) -> Result<PathBuf, String> {
    a.positional
        .get(i)
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing project path\n\n{USAGE}"))
}

/// `data/annotations/x.json` → `data` (COCO `file_name`s are relative to it).
fn default_images_root(coco: &Path) -> PathBuf {
    let parent = coco.parent().unwrap_or(Path::new("."));
    if parent.file_name().is_some_and(|n| n == "annotations") {
        parent.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        parent.to_path_buf()
    }
}

pub fn main_with(raw: Vec<String>) -> Result<(), String> {
    let a = Args::parse(&raw);
    if a.has("--help")
        || a.has("-h")
        || a.positional.is_empty() && !raw.iter().any(|r| r == "bench")
    {
        print!("{USAGE}");
        return Ok(());
    }
    let cmd = a.positional[0].as_str();
    let rest = Args {
        positional: a.positional[1..].to_vec(),
        flags: a.flags.clone(),
    };
    match cmd {
        "open" => open(&rest),
        "import" => cmd_import(&rest),
        "import-dataset" => cmd_import_dataset(&rest),
        "import-preds" => cmd_import_preds(&rest),
        "export" => cmd_export(&rest),
        "stats" => cmd_stats(&rest),
        "priority" => cmd_priority(&rest),
        "compare" => cmd_compare(&rest),
        "bench" => cmd_bench(&rest),
        "gen-synthetic" => cmd_gen_synthetic(&rest),
        "__crash-test" => cmd_crash_test(&rest),
        "screenshot" => cmd_screenshot(&rest),
        _ if cmd.ends_with(".db") || Path::new(cmd).exists() => open(&a),
        _ => Err(format!("unknown command {cmd:?}\n\n{USAGE}")),
    }
}

fn open(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    if !project.exists() {
        return Err(format!(
            "{} does not exist; create it with `import`",
            project.display()
        ));
    }
    app::run(AppConfig {
        project,
        filter: a.filter()?,
        start_at: a.get("--start").map(str::to_owned),
        prediction_threshold: a.num("--pred-threshold", 0.3)?,
    })
}

fn cmd_import(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let coco = a
        .positional
        .get(1)
        .map(PathBuf::from)
        .ok_or("missing COCO file")?;
    let set_name = a.get("--set").map(str::to_owned).unwrap_or_else(|| {
        coco.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "set".into())
    });
    let images_root = a
        .get("--images-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_images_root(&coco));
    let mut conn = database::open(&project)?;
    let t = Instant::now();
    let report = import_coco(
        &mut conn,
        &coco,
        ImportOptions {
            set_name,
            images_root,
        },
    )?;
    println!("{report}  ({:.1}s)", t.elapsed().as_secs_f64());
    Ok(())
}

fn cmd_import_dataset(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let data = a
        .positional
        .get(1)
        .map(PathBuf::from)
        .ok_or("missing data directory")?;
    let mut conn = database::open(&project)?;
    // Priority order of the review plan: regression, gold dev, final test, hard cases.
    for set in [
        "regression",
        "dev_gold_prefill",
        "final_test_prefill",
        "manual_queue_prefill",
    ] {
        let coco = data.join("annotations").join(format!("{set}.json"));
        if !coco.exists() {
            println!("{set}: {} not found, skipped", coco.display());
            continue;
        }
        let report = import_coco(
            &mut conn,
            &coco,
            ImportOptions {
                set_name: set.into(),
                images_root: data.clone(),
            },
        )?;
        println!("{report}");
    }
    Ok(())
}

fn cmd_import_preds(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let file = a
        .positional
        .get(1)
        .map(PathBuf::from)
        .ok_or("missing results file")?;
    let model = a.get("--model").ok_or("--model NAME is required")?;
    let scale_from = a
        .get("--scale-from")
        .map(|s| s.parse::<u32>().map_err(|_| "--scale-from: not a number"))
        .transpose()?;
    let mut conn = database::open(&project)?;
    let (n, skipped) = import_predictions(
        &mut conn,
        &file,
        model,
        a.get("--set"),
        scale_from,
        a.num("--min-score", 0.0)?,
    )?;
    println!(
        "{model}: {n} predictions imported, {skipped} skipped (unknown page/category or malformed)"
    );
    Ok(())
}

fn cmd_export(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let conn = database::open(&project)?;
    let mut sizes: Vec<u32> = Vec::new();
    for s in a.all("--size") {
        sizes.push(
            s.parse()
                .map_err(|_| format!("--size: not a number: {s}"))?,
        );
    }
    if sizes.is_empty() {
        sizes.push(640);
    }
    let opts = ExportOptions {
        out_dir: a
            .get("--out")
            .map(PathBuf::from)
            .unwrap_or_else(|| project.with_file_name("exports")),
        name: a.get("--name").map(str::to_owned),
        filter: a.filter()?,
        sizes,
        write_images: a.has("--images"),
        reviewed_only: a.has("--reviewed-only"),
        strict_splits: !a.has("--allow-split-leak"),
        ..ExportOptions::default()
    };
    let t = Instant::now();
    let r = export(&conn, &opts)?;
    println!(
        "exported {} pages, {} boxes to {} ({:.1}s)\n  excluded {} · unreviewed skipped {} · clamped {} · dropped {} · images {}",
        r.pages,
        r.boxes,
        r.dir.display(),
        t.elapsed().as_secs_f64(),
        r.excluded_pages,
        r.skipped_unreviewed,
        r.clamped_boxes,
        r.dropped_boxes,
        r.images_written
    );
    for leak in &r.split_leaks {
        println!("  warning: split leak {leak}");
    }
    Ok(())
}

fn cmd_stats(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let conn = database::open(&project)?;
    let mut st = conn
        .prepare("SELECT set_name, review_status, COUNT(*) FROM pages GROUP BY set_name, review_status ORDER BY MIN(seq)")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut sets: Vec<String> = Vec::new();
    for (s, _, _) in &rows {
        if !sets.contains(s) {
            sets.push(s.clone());
        }
    }
    println!(
        "{:<24} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "set", "pages", "unseen", "opened", "reviewed", "excluded", "attention"
    );
    for s in &sets {
        let get = |status: &str| {
            rows.iter()
                .filter(|r| &r.0 == s && r.1 == status)
                .map(|r| r.2)
                .sum::<i64>()
        };
        let total: i64 = rows.iter().filter(|r| &r.0 == s).map(|r| r.2).sum();
        println!(
            "{:<24} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9}",
            s,
            total,
            get("unseen"),
            get("opened"),
            get("reviewed"),
            get("excluded"),
            get("needs_attention")
        );
    }
    let q = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    println!(
        "boxes: {} editable, {} imported, {} predictions ({} models); {} committed page revisions",
        q("SELECT COUNT(*) FROM annotations"),
        q("SELECT COUNT(*) FROM imported_annotations"),
        q("SELECT COUNT(*) FROM predictions"),
        q("SELECT COUNT(DISTINCT model) FROM predictions"),
        q("SELECT COUNT(*) FROM events")
    );
    Ok(())
}

fn provenance(a: &Annotation) -> Option<Value> {
    a.provenance_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
}

fn cmd_priority(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let mut conn = database::open(&project)?;
    let model = a.get("--model");
    let keys = database::page_keys(&conn, &PageFilter::default())?;
    let mut scores = Vec::with_capacity(keys.len());
    for k in &keys {
        let imported = database::imported_annotations(&conn, k)?;
        let (book_type, image_json): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT book_type, image_json FROM pages WHERE page_key = ?1",
                params![k],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        let mut sig = Signals::default();
        for ann in &imported {
            if ann.is_ignore {
                sig.ignore_regions += 1.0;
            }
            if provenance(ann)
                .and_then(|p| p.get("class_conflict").and_then(Value::as_bool))
                .unwrap_or(false)
            {
                sig.class_conflicts += 1.0;
            }
            if matches!(
                class(ann.class_id).name,
                "marginalia" | "toc" | "formula" | "ornament" | "table"
            ) {
                sig.rare_classes += 1.0;
            }
        }
        let figures: Vec<_> = imported
            .iter()
            .filter(|x| !x.is_ignore && class(x.class_id).group == Group::Figure)
            .collect();
        for t in imported
            .iter()
            .filter(|x| class(x.class_id).group == Group::TextLike)
        {
            if figures
                .iter()
                .any(|f| t.bbox.intersection(&f.bbox).area() >= 0.6 * t.bbox.area())
            {
                sig.text_in_figure += 0.5;
            }
        }
        let bt = book_type.unwrap_or_default();
        if bt.contains("aged") || bt.contains("dark") || bt.contains("grey") {
            sig.hard_layout += 0.5;
        }
        if let Some(meta) = image_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
        {
            let lm = meta.get("label_meta");
            if lm
                .and_then(|m| m.get("gutters"))
                .and_then(Value::as_array)
                .is_some_and(|g| !g.is_empty())
            {
                sig.hard_layout += 0.5;
            }
            if lm
                .and_then(|m| m.get("dark_page"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                sig.hard_layout += 0.5;
            }
        }
        if let Some(model) = model {
            let preds = database::predictions(&conn, k, model)?;
            let p: Vec<Det> = preds
                .iter()
                .filter(|p| p.score >= 0.3)
                .map(|p| Det {
                    class_id: p.class_id,
                    bbox: p.bbox,
                    score: p.score,
                })
                .collect();
            let t: Vec<Det> = imported
                .iter()
                .filter(|x| !x.is_ignore)
                .map(|x| Det {
                    class_id: x.class_id,
                    bbox: x.bbox,
                    score: 1.0,
                })
                .collect();
            let m = match_boxes(&p, &t, 0.5, |_, _| true);
            let mut disagree = 0.0;
            for &(pi, ti, _) in &m.matched {
                let (pg, tg) = (class(p[pi].class_id).group, class(t[ti].class_id).group);
                if (pg == Group::Figure) != (tg == Group::Figure) {
                    disagree += 1.0;
                }
            }
            sig.text_image_disagreement = disagree / 2.0;
            sig.unmatched = (m.unmatched_pred.len() + m.unmatched_truth.len()) as f64;
        }
        scores.push((k.clone(), sig.score()));
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut up = tx
            .prepare("UPDATE pages SET priority = ?2 WHERE page_key = ?1")
            .map_err(|e| e.to_string())?;
        for (k, s) in &scores {
            up.execute(params![k, s]).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    let mut sorted: Vec<_> = scores.iter().collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    println!(
        "priority computed for {} pages{}; top 10:",
        scores.len(),
        model.map(|m| format!(" (model {m})")).unwrap_or_default()
    );
    for (k, s) in sorted.iter().take(10) {
        println!("  {s:6.2}  {k}");
    }
    println!("open with --priority to review in this order (diversified by book)");
    Ok(())
}

fn cmd_compare(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let conn = database::open(&project)?;
    let model = a.get("--model").ok_or("--model NAME is required")?;
    let thr: f64 = a.num("--threshold", 0.35)?;
    let mut filter = a.filter()?;
    if filter.statuses.is_empty() {
        filter.statuses = vec![ReviewStatus::Reviewed];
    }
    let keys = database::page_keys(&conn, &filter)?;
    let mut acc = Accumulator::default();
    for k in &keys {
        // Human ground truth only: the editable layer of reviewed pages. The
        // imported and prediction layers are read, never written.
        let truth = database::annotations(&conn, k)?;
        let preds = database::predictions(&conn, k, model)?;
        acc.add_page(&truth, &preds, thr);
    }
    let report = acc.finish(model, thr, &filter.describe());
    println!("{report}");
    if let Some(path) = a.get("--json") {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Synthetic projects for scale tests: `pages` rows over a handful of real
/// JPEGs, no bitmap allocated per page.
pub fn generate_synthetic(project: &Path, pages: usize, images_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(images_dir).map_err(|e| e.to_string())?;
    let mut jpgs = Vec::new();
    for i in 0..4u64 {
        let path = images_dir.join(format!("synthetic_{i}.jpg"));
        if !path.exists() {
            let img = crate::image_io::decode::synthetic_page(850, 1280, i + 7);
            let lvl = &img.levels[0];
            let mut rgb = image::RgbImage::new(lvl.w, lvl.h);
            for (o, p) in rgb.pixels_mut().zip(&lvl.px) {
                *o = image::Rgb([(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
            }
            rgb.save(&path).map_err(|e| e.to_string())?;
        }
        jpgs.push(path);
    }
    let mut conn = database::open(project)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let base: i64 = tx
        .query_row("SELECT COALESCE(MAX(seq), -1) + 1 FROM pages", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    {
        let mut ins_page = tx
            .prepare(
                "INSERT INTO pages (page_key, seq, book_id, split, set_name, source_path, file_name, width, height, book_type, page_index) \
                 VALUES (?1, ?2, ?3, 'train', 'synthetic', ?4, ?5, 850, 1280, 'digital', ?6)",
            )
            .map_err(|e| e.to_string())?;
        let mut ins_ann = tx
            .prepare(
                "INSERT INTO annotations (id, page_key, ord, class_id, x0, y0, x1, y1) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(|e| e.to_string())?;
        let mut ins_imp = tx
            .prepare(
                "INSERT INTO imported_annotations (id, page_key, ord, class_id, x0, y0, x1, y1) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(|e| e.to_string())?;
        for i in 0..pages {
            let seq = base + i as i64;
            let key = format!("synth{:03}_p{:06}", seq / 500, seq);
            let book = format!("synth{:03}", seq / 500);
            let jpg = &jpgs[i % jpgs.len()];
            ins_page
                .execute(params![
                    key,
                    seq,
                    book,
                    jpg.to_string_lossy(),
                    format!("images/train/{book}/p{seq:06}.jpg"),
                    seq % 500
                ])
                .map_err(|e| e.to_string())?;
            for b in 0..8i64 {
                let y = 90.0 + b as f64 * 140.0;
                let id = format!("{key}#{b}");
                let args = params![
                    id,
                    key,
                    b,
                    if b == 3 { 6 } else { 0 },
                    70.0,
                    y,
                    780.0,
                    y + 120.0
                ];
                ins_ann.execute(args).map_err(|e| e.to_string())?;
                ins_imp
                    .execute(params![
                        id,
                        key,
                        b,
                        if b == 3 { 6 } else { 0 },
                        70.0,
                        y,
                        780.0,
                        y + 120.0
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

fn cmd_gen_synthetic(a: &Args) -> Result<(), String> {
    let project = project_arg(a, 0)?;
    let pages: usize = a.num("--pages", 1000)?;
    let dir = a
        .get("--images-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| project.with_file_name("synthetic-images"));
    let t = Instant::now();
    generate_synthetic(&project, pages, &dir)?;
    println!(
        "{pages} synthetic pages added in {:.1}s",
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Headless frame timings at 1080p, 1440p and 4K through the real render
/// path: page raster renders (zoom/pan), overlay-only frames during a
/// continuous box drag, and page switches to a prefetched page.
fn cmd_bench(a: &Args) -> Result<(), String> {
    use liminalkit_raster::WindowBuffer;
    use liminalkit_shell::{MouseButton, Scale};
    use liminalkit_windowing::PixelApp;

    let frames: usize = a.num("--frames", 240)?;
    let dir = std::env::temp_dir().join(format!("liminal-annotator-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let project = dir.join("bench.db");
    match a.get("--image") {
        Some(img) => {
            let img = PathBuf::from(img);
            let (w, h) =
                image::image_dimensions(&img).map_err(|e| format!("{}: {e}", img.display()))?;
            let conn = database::open(&project)?;
            for i in 0..6 {
                conn.execute(
                    "INSERT INTO pages (page_key, seq, book_id, split, set_name, source_path, file_name, width, height) VALUES (?1, ?2, 'b', 'train', 'bench', ?3, 'x.jpg', ?4, ?5)",
                    params![format!("p{i}"), i, img.to_string_lossy(), w, h],
                )
                .map_err(|e| e.to_string())?;
                for b in 0..12 {
                    let y = h as f64 * (0.05 + b as f64 * 0.075);
                    conn.execute(
                        "INSERT INTO annotations (id, page_key, ord, class_id, x0, y0, x1, y1) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7)",
                        params![format!("p{i}#{b}"), format!("p{i}"), b, w as f64 * 0.08, y, w as f64 * 0.92, y + h as f64 * 0.06],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
        None => generate_synthetic(&project, 6, &dir)?,
    }
    let mut app = AnnotatorApp::new(AppConfig {
        project: project.clone(),
        filter: PageFilter::default(),
        start_at: None,
        prediction_threshold: 0.3,
    })?;
    if !app.wait_for_image(Duration::from_secs(10)) {
        return Err("bench image did not load".into());
    }
    println!(
        "{:<10} {:>14} {:>14} {:>14} {:>14} {:>14} {:>8}",
        "size",
        "raster p50/95",
        "drag p50/95",
        "zoom p50/95",
        "pan p50/95",
        "switch p50/95",
        "idle"
    );
    for (w, h) in [(1920u32, 1080u32), (2560, 1440), (3840, 2160)] {
        let mut buf = WindowBuffer::new(w, h);
        let scale = Scale::new(if w >= 3840 {
            2.0
        } else if w >= 2560 {
            1.25
        } else {
            1.0
        });
        app.render(&mut buf, scale);
        app.act_fit_for_bench();
        app.render(&mut buf, scale);
        app.perf = Default::default();
        // Continuous drag of the first box's bottom-right corner.
        let first = app
            .doc
            .as_ref()
            .and_then(|d| d.annotations.first().cloned())
            .ok_or("no box")?;
        let br = app
            .view
            .image_to_screen(crate::annotation::geometry::Point::new(
                first.bbox.x1,
                first.bbox.y1,
            ));
        app.on_cursor(br.x as f32 - 1.0, br.y as f32 - 1.0);
        app.on_mouse(MouseButton::Left, true);
        app.render(&mut buf, scale);
        app.perf = Default::default();
        for i in 0..frames {
            let t = i as f32 / frames as f32 * std::f32::consts::TAU;
            app.on_cursor(br.x as f32 + 40.0 * t.sin(), br.y as f32 + 25.0 * t.cos());
            app.render(&mut buf, scale);
        }
        app.on_mouse(MouseButton::Left, false);
        app.render(&mut buf, scale);
        let drag = (
            percentile(&app.perf.frame_ms, 0.5),
            percentile(&app.perf.frame_ms, 0.95),
        );
        // Zoom: every frame re-renders the page raster.
        app.perf = Default::default();
        for i in 0..frames / 4 {
            let notch = if i % 2 == 0 { 1.0 } else { -1.0 };
            app.on_cursor(w as f32 * 0.4, h as f32 * 0.5);
            app.on_scroll_event(&liminalkit_windowing::ScrollEvent {
                pixel_dx: 0.0,
                pixel_dy: 24.0 * notch,
                line_dx: Some(0.0),
                line_dy: Some(notch),
                phase: liminalkit_windowing::GesturePhase::Moved,
            });
            app.render(&mut buf, scale);
        }
        let zoom = (
            percentile(&app.perf.frame_ms, 0.5),
            percentile(&app.perf.frame_ms, 0.95),
        );
        let raster = (
            percentile(&app.perf.raster_ms, 0.5),
            percentile(&app.perf.raster_ms, 0.95),
        );
        // Pan: middle-drag; every frame moves the page by whole pixels.
        app.perf = Default::default();
        let (px, py) = (w as f32 * 0.5, h as f32 * 0.5);
        app.on_cursor(px, py);
        app.on_mouse(MouseButton::Middle, true);
        for i in 0..frames {
            let t = i as f32 / frames as f32 * std::f32::consts::TAU;
            app.on_cursor(
                px + (60.0 * t.sin()).round(),
                py + (45.0 * (2.0 * t).sin()).round(),
            );
            app.render(&mut buf, scale);
        }
        app.on_mouse(MouseButton::Middle, false);
        app.render(&mut buf, scale);
        let pan = (
            percentile(&app.perf.frame_ms, 0.5),
            percentile(&app.perf.frame_ms, 0.95),
        );
        let pan_scrolled = (app.perf.raster_scrolls, app.perf.raster_renders);
        eprintln!(
            "  {w}x{h}: pan frames served by shifting the raster: {}/{}",
            pan_scrolled.0, pan_scrolled.1
        );
        // Idle hover: cursor moves over blank canvas change nothing visible.
        app.perf = Default::default();
        for i in 0..frames {
            app.on_cursor(5.0 + (i % 3) as f32, h as f32 * 0.5);
            app.render(&mut buf, scale);
        }
        let idle_skipped = app.perf.skipped;
        // Undo the drag so the project stays clean between sizes.
        app.on_key_event(&liminalkit_windowing::KeyEvent {
            key: liminalkit_shell::KeyInput::Character('z'),
            text: String::new(),
            physical: "KeyZ".into(),
            pressed: true,
            repeat: false,
            modifiers: liminalkit_shell::Modifiers {
                control: true,
                ..Default::default()
            },
        });
        // Page switches: next page is prefetched; wait for it, then switch.
        app.perf = Default::default();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(150));
            app.poll_background();
            let next = (app.pos + 1) % app.keys.len();
            app.navigate(next, false);
            app.wait_for_image(Duration::from_secs(5));
            app.render(&mut buf, scale);
        }
        let switch = (
            percentile(&app.perf.switch_ms, 0.5),
            percentile(&app.perf.switch_ms, 0.95),
        );
        println!(
            "{:<10} {:>6.2}/{:<7.2} {:>6.2}/{:<7.2} {:>6.2}/{:<7.2} {:>6.2}/{:<7.2} {:>6.1}/{:<7.1} {:>4}/{:<3}",
            format!("{w}x{h}"),
            raster.0,
            raster.1,
            drag.0,
            drag.1,
            zoom.0,
            zoom.1,
            pan.0,
            pan.1,
            switch.0,
            switch.1,
            idle_skipped,
            frames
        );
    }
    app.flush();
    app.shutdown();
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
    println!("(ms per frame incl. HUD. drag = continuous corner drag; zoom = wheel, page resampled every frame;");
    println!(" pan = middle-drag, raster shifted + exposed strips; switch = navigation to a prefetched page until shown;");
    println!(" idle = cursor moves over blank canvas that were skipped as unchanged / total)");
    Ok(())
}

/// Test hook: open the project headlessly, make `--edits N` edits across
/// pages (moving one box per page), optionally wait for the writer to
/// acknowledge them (`--wait`), then abort the process without any shutdown.
fn cmd_crash_test(a: &Args) -> Result<(), String> {
    use crate::annotation::commands::Edit;
    let project = project_arg(a, 0)?;
    let edits: usize = a.num("--edits", 10)?;
    let mut app = AnnotatorApp::new(AppConfig {
        project,
        filter: PageFilter::default(),
        start_at: None,
        prediction_threshold: 0.3,
    })?;
    for i in 0..edits {
        // A page is only accepted once its image has loaded.
        app.wait_for_image(Duration::from_secs(10));
        let doc = app.doc.as_mut().ok_or("no page")?;
        let before = doc.annotations[0].clone();
        let mut after = before.clone();
        after.bbox = after.bbox.translate(1.0, 0.0);
        doc.apply(Edit::Modify { before, after });
        app.commit_now();
        if i + 1 < edits {
            let next = (app.pos + 1) % app.keys.len();
            app.navigate(next, true);
        }
    }
    if a.get("--wait").is_some_and(|v| v == "1") {
        app.flush();
        println!("acked");
    }
    std::process::abort();
}

/// Render one frame through the real window path into a PNG, after an
/// optional comma-separated key script (`Tab`, `Right`, `c`, `2`, `F1`,
/// `Ctrl+1`, `Shift+3`, `+`, `-`, `hover:X,Y` image-space pointer position).
fn cmd_screenshot(a: &Args) -> Result<(), String> {
    use liminalkit_raster::WindowBuffer;
    use liminalkit_shell::{KeyInput, Modifiers, Scale};
    use liminalkit_windowing::{KeyEvent, PixelApp};
    let project = project_arg(a, 0)?;
    let out = a
        .positional
        .get(1)
        .map(PathBuf::from)
        .ok_or("missing output PNG")?;
    let (w, h) = a
        .get("--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        .unwrap_or((1600, 1000));
    let scale = Scale::new(a.num("--scale", 1.0)?);
    let mut app = AnnotatorApp::new(AppConfig {
        project,
        filter: a.filter()?,
        start_at: a.get("--start").map(str::to_owned),
        prediction_threshold: a.num("--pred-threshold", 0.3)?,
    })?;
    app.wait_for_image(Duration::from_secs(10));
    let mut buf = WindowBuffer::new(w, h);
    app.render(&mut buf, scale);
    for step in a
        .get("--keys")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(spec) = step
            .strip_prefix("drag:")
            .or_else(|| step.strip_prefix("click:"))
        {
            use liminalkit_shell::MouseButton;
            let v: Vec<f64> = spec.split(':').filter_map(|x| x.parse().ok()).collect();
            let pt = |x: f64, y: f64| {
                app.view
                    .image_to_screen(crate::annotation::geometry::Point::new(x, y))
            };
            let a0 = pt(v[0], v[1]);
            let a1 = if v.len() >= 4 { pt(v[2], v[3]) } else { a0 };
            app.on_cursor(a0.x as f32, a0.y as f32);
            app.on_mouse(MouseButton::Left, true);
            app.render(&mut buf, scale);
            for i in 1..=8 {
                let t = i as f64 / 8.0;
                app.on_cursor(
                    (a0.x + (a1.x - a0.x) * t) as f32,
                    (a0.y + (a1.y - a0.y) * t) as f32,
                );
                app.render(&mut buf, scale);
            }
            app.on_mouse(MouseButton::Left, false);
            app.render(&mut buf, scale);
            continue;
        }
        if let Some(xy) = step.strip_prefix("hover:") {
            let (x, y) = xy.split_once(':').ok_or("hover:X:Y")?;
            let p = app
                .view
                .image_to_screen(crate::annotation::geometry::Point::new(
                    x.parse().map_err(|_| "hover x")?,
                    y.parse().map_err(|_| "hover y")?,
                ));
            app.on_cursor(p.x as f32, p.y as f32);
            app.render(&mut buf, scale);
            continue;
        }
        let mut m = Modifiers::default();
        let mut k = step;
        if let Some(r) = k.strip_prefix("Ctrl+") {
            m.control = true;
            k = r;
        }
        if let Some(r) = k.strip_prefix("Shift+") {
            m.shift = true;
            k = r;
        }
        let (key, physical) = match k {
            "Tab" => (KeyInput::Tab, "Tab".to_owned()),
            "Right" => (KeyInput::Right, "ArrowRight".to_owned()),
            "Left" => (KeyInput::Left, "ArrowLeft".to_owned()),
            "Escape" => (KeyInput::Escape, "Escape".to_owned()),
            "Delete" => (KeyInput::Delete, "Delete".to_owned()),
            "F1" => (KeyInput::Function(1), "F1".to_owned()),
            "F12" => (KeyInput::Function(12), "F12".to_owned()),
            "+" => (KeyInput::Character('+'), "Equal".to_owned()),
            "-" => (KeyInput::Character('-'), "Minus".to_owned()),
            d if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) => (
                KeyInput::Character(d.chars().next().unwrap_or('1')),
                format!("Digit{d}"),
            ),
            c if c.len() == 1 => {
                let ch = c.chars().next().unwrap_or('x');
                (
                    KeyInput::Character(ch),
                    format!("Key{}", ch.to_ascii_uppercase()),
                )
            }
            other => return Err(format!("unknown key {other:?}")),
        };
        app.on_key_event(&KeyEvent {
            key,
            text: String::new(),
            physical,
            pressed: true,
            repeat: false,
            modifiers: m,
        });
        app.wait_for_image(Duration::from_secs(10));
        app.render(&mut buf, scale);
    }
    app.render(&mut buf, scale);
    liminalkit_raster::png::write(&out, &buf).map_err(|e| format!("{}: {e}", out.display()))?;
    app.shutdown();
    println!("wrote {}", out.display());
    Ok(())
}
