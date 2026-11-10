//! Command line: every non-interactive operation, plus `open` for the window.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::params;
use serde_json::Value;

use crate::annotation::commands::{Annotation, ReviewStatus};
use crate::annotation::labels::{Group, class};
use crate::app::{AnnotatorApp, AppConfig, percentile};
use crate::dataset::database::{self, PageFilter, Queue};
use crate::dataset::export_coco::{ExportOptions, export};
use crate::dataset::import_coco::{ImportOptions, import_coco, import_predictions};
use crate::dataset::import_folder::{self, FolderOptions, Prefill};
use crate::review::comparison::{Det, match_boxes};
use crate::review::metrics::Accumulator;
use crate::review::queue::Signals;
use crate::shell::{self, ShellConfig};

pub const USAGE: &str = "\
Liminal Annotator — fast box review for page-layout datasets

USAGE
  liminal-annotator                                       reopen the last project, or the start screen
  liminal-annotator <folder> [--prefill yolo|paddle|none]  open a dataset folder: creates
        <folder>/annotator.db on first use, imports new detections later, opens the book manager
  liminal-annotator <project.db> [queue options] [--grid]  open a project on its first page to do
        (--grid: in the six-page view, six pages of the queue at once; G toggles it)
  liminal-annotator open [folder|project.db] [queue options] [--grid]
  liminal-annotator create <folder> [--prefill yolo|paddle|none] [--min-score 0.3] [--layer-min-score 0.3]
        create or update <folder>/annotator.db without opening the window
  liminal-annotator books <project.db|folder> [--sort CLASS|paper|pictures|abandon|large|todo|pages|name]
        [--per-page] [--flagged] [--show excluded|training|needs-work] [--limit N]
        [--paper (measure paper tone first)]
  liminal-annotator split <project.db|folder> --book ID --to SPLIT
  liminal-annotator split <project.db|folder> --assign-new [--dev 0.1] [--test 0.1]
        [--test-name final_test] [--from new] [--dry-run]
        whole books of split `new` to train/dev/test by a stable hash of the book id
  liminal-annotator queues <project.db|folder> [--book ID]   pages in each review queue
  liminal-annotator flag <project.db|folder> --book ID [--exclude|--include] [--needs-work|--done]
        a book's flags: excluded books are left out of every export
        per-book progress and YOLO abandon counts
  liminal-annotator convert <project.db|folder> --to CLASS [--book ID] [--region top|bottom|margins|large|other]
        convert abandon boxes in bulk (logged; `convert --undo-last` reverts the last one)
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
        [--keys Tab,2,c] [--start PAGE_KEY] [--books] [--grid] [queue options]   render one frame
        headlessly (--books: the book manager; keys then go to it; --grid: the six-page view.
        Key steps also take hover:X:Y and click:X:Y in image pixels, cell:N to click and
        hovercell:N to point at page N of the six-page view). Marks the pages it shows as opened

Commands that take a project also accept its folder, and use the last opened project
when it is left out.

QUEUE OPTIONS
  --set NAME  --split NAME  --book ID  --reason TEXT  --status unseen,opened,...
  --with abandon (only pages that still have a box of that class)
  --figure-min PCT (only pages with a figure box covering at least PCT% of the page, e.g. 40)
  --queue todo|pictures|large-pictures|model-only|abandon|empty|text-sample|attention|excluded|reviewed|all
        (the review queues of the Q picker: pictures first, text pages last; large-pictures:
        a figure box of large_figure_percent of the page or more, 40 unless the project's
        settings file says otherwise)
  --priority (order by review priority, diversified by book)  --start PAGE_KEY
  --pred-threshold 0.3 (prediction overlay score)
";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const BOOL_FLAGS: [&str; 18] = [
    "--grid",
    "--assign-new",
    "--dry-run",
    "--exclude",
    "--include",
    "--needs-work",
    "--done",
    "--paper",
    "--per-page",
    "--books",
    "--images",
    "--reviewed-only",
    "--allow-split-leak",
    "--priority",
    "--help",
    "-h",
    "--flagged",
    "--undo-last",
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
            has_class: match self.get("--with") {
                Some(name) => Some(
                    crate::annotation::labels::class_by_name(name)
                        .ok_or_else(|| format!("--with: unknown class {name:?}"))?,
                ),
                None => None,
            },
            queue: match self.get("--queue") {
                Some(q) => Some(Queue::from_slug(q).ok_or_else(|| {
                    format!(
                        "--queue: {q:?} is not one of {}",
                        Queue::ALL.map(|q| q.slug()).join(", ")
                    )
                })?),
                None => None,
            },
            figure_min: match self.get("--figure-min") {
                Some(v) => {
                    let pct: f64 = v
                        .trim_end_matches('%')
                        .parse()
                        .map_err(|_| format!("--figure-min: not a number: {v}"))?;
                    if !(0.0..=100.0).contains(&pct) {
                        return Err(format!("--figure-min: {v} is not a percentage (0 to 100)"));
                    }
                    Some(pct / 100.0)
                }
                None => None,
            },
            large_figure_share: None,
        })
    }

    /// [`Args::filter`] with the large-picture share from the project's
    /// settings file (read, never created).
    fn filter_for(&self, project: &Path) -> Result<PageFilter, String> {
        let mut f = self.filter()?;
        f.large_figure_share = Some(
            crate::settings::Settings::load(&crate::settings::Settings::path_for(project))
                .large_figure_share(),
        );
        Ok(f)
    }
}

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push_ancestors = |start: PathBuf| {
        let mut cur: Option<&Path> = Some(start.as_path());
        for _ in 0..4 {
            let Some(dir) = cur else { break };
            let owned = dir.to_path_buf();
            if !roots.contains(&owned) {
                roots.push(owned);
            }
            cur = dir.parent();
        }
    };
    if let Ok(cwd) = std::env::current_dir() {
        push_ancestors(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            push_ancestors(dir.to_path_buf());
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if !roots.contains(&manifest) {
        roots.push(manifest);
    }
    roots
}

fn resolve_existing_path(p: &Path) -> Option<PathBuf> {
    if p.exists() {
        return Some(p.to_path_buf());
    }
    if p.is_relative() {
        for root in search_roots() {
            let cand = root.join(p);
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

fn project_arg(a: &Args, i: usize) -> Result<PathBuf, String> {
    let p = a
        .positional
        .get(i)
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing project path\n\n{USAGE}"))?;
    let p = resolve_existing_path(&p).unwrap_or(p);
    Ok(import_folder::project_for(&p))
}

/// The most recently opened project, for commands given none.
fn last_project() -> Result<PathBuf, String> {
    crate::recent::load()
        .into_iter()
        .next()
        .ok_or_else(|| format!("no project given and none opened before\n\n{USAGE}"))
}

fn folder_options(a: &Args) -> Result<FolderOptions, String> {
    let mut o = FolderOptions::default();
    if let Some(p) = a.get("--prefill") {
        o.prefill = Prefill::parse(p)
            .ok_or_else(|| format!("--prefill: {p:?} is not yolo, paddle or none"))?;
    }
    o.prefill_min_score = a.num("--min-score", o.prefill_min_score)?;
    o.layer_min_score = a.num("--layer-min-score", o.layer_min_score)?;
    Ok(o)
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
    if a.has("--help") || a.has("-h") {
        print!("{USAGE}");
        return Ok(());
    }
    if a.positional.is_empty() {
        return open(&a);
    }
    let cmd = a.positional[0].as_str();
    let rest = Args {
        positional: a.positional[1..].to_vec(),
        flags: a.flags.clone(),
    };
    match cmd {
        "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "open" => open(&rest),
        "create" => cmd_create(&rest),
        "books" => cmd_books(&rest),
        "convert" => cmd_convert(&rest),
        "flag" => cmd_flag(&rest),
        "split" => cmd_split(&rest),
        "queues" => cmd_queues(&rest),
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
        _ if cmd.ends_with(".db") || resolve_existing_path(Path::new(cmd)).is_some() => open(&a),
        _ => Err(format!("unknown command {cmd:?}\n\n{USAGE}")),
    }
}

/// The project a command works on: its first argument (a `.db` or a
/// dataset folder), else the last project opened.
fn project_or_default(a: &Args) -> Result<PathBuf, String> {
    if a.positional.is_empty() {
        last_project()
    } else {
        let p = project_arg(a, 0)?;
        if !p.exists() {
            return Err(format!(
                "{} does not exist; open its folder, or use `create` or `import`",
                p.display()
            ));
        }
        Ok(p)
    }
}

/// The window: a folder or project from the command line, or the last one.
fn open(a: &Args) -> Result<(), String> {
    let path = match a.positional.first() {
        Some(p) => {
            let p = PathBuf::from(p);
            Some(
                resolve_existing_path(&p)
                    .ok_or_else(|| format!("{} does not exist", p.display()))?,
            )
        }
        None => None,
    };
    shell::run(ShellConfig {
        open: path,
        filter: a.filter()?,
        start_at: a.get("--start").map(str::to_owned),
        prediction_threshold: a.num("--pred-threshold", 0.3)?,
        folder: folder_options(a)?,
        grid: a.has("--grid"),
    })
}

fn cmd_create(a: &Args) -> Result<(), String> {
    let root = a
        .positional
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing folder\n\n{USAGE}"))?;
    let project = match a.get("--project") {
        Some(p) => PathBuf::from(p),
        None => import_folder::project_for(&root),
    };
    let kind = import_folder::detect(&root)?;
    println!("{}: {}", root.display(), kind.describe());
    let creating = !project.exists();
    let mut conn = database::open(&project)?;
    let t = Instant::now();
    let mut last = Instant::now();
    let mut shown = String::new();
    let result = import_folder::sync_folder(
        &mut conn,
        &root,
        &folder_options(a)?,
        &mut |stage, done, total| {
            if last.elapsed() >= Duration::from_secs(2) || stage != shown {
                last = Instant::now();
                shown = stage.to_owned();
                match (done * 100).checked_div(total) {
                    Some(pct) => eprintln!("  {stage}: {pct}%"),
                    None => eprintln!("  {stage}"),
                }
            }
        },
    );
    if result.is_err() && creating {
        drop(conn);
        import_folder::remove_project(&project);
    }
    let report = result?;
    println!("{report}  ({:.1}s)", t.elapsed().as_secs_f64());
    println!("project: {}", project.display());
    crate::recent::touch(&project);
    Ok(())
}

fn cmd_books(a: &Args) -> Result<(), String> {
    use crate::annotation::labels::{ABANDON, class_by_name};
    let project = project_or_default(a)?;
    if a.has("--paper") {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            let _ = tx.send(());
        });
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        if let Some(job) = crate::dataset::tone::ToneJob::start(&project, threads, wake)? {
            let t = Instant::now();
            let total = job.total;
            while !job.finished() {
                let _ = rx.recv_timeout(Duration::from_millis(500));
            }
            job.wait();
            eprintln!(
                "paper tone measured for {} books in {:.1}s",
                total,
                t.elapsed().as_secs_f64()
            );
        }
    }
    let conn = database::open(&project)?;
    let settings =
        crate::settings::Settings::load_or_create(&crate::settings::Settings::path_for(&project)).0;
    let t = Instant::now();
    let mut books = database::book_stats(&conn)?;
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let flagged = |b: &database::BookStats| {
        b.abandon_boxes >= settings.abandon_flag_min_boxes.max(1) as i64
            && b.abandon_per_page() >= settings.abandon_flag_per_page
    };
    let per_page = a.has("--per-page");
    let key = |b: &database::BookStats, c: usize| {
        if per_page {
            b.per_page(c)
        } else {
            b.class_boxes[c] as f64
        }
    };
    match a.get("--sort") {
        Some("abandon") => {
            books.sort_by(|x, y| y.abandon_per_page().total_cmp(&x.abandon_per_page()))
        }
        Some("large") => books.sort_by(|x, y| y.large_per_page().total_cmp(&x.large_per_page())),
        Some("todo") => books.sort_by_key(|b| std::cmp::Reverse(b.todo)),
        Some("pictures") => books.sort_by_key(|b| std::cmp::Reverse(b.picture_todo)),
        Some("name") => books.sort_by(|x, y| x.book_id.cmp(&y.book_id)),
        Some("pages") => books.sort_by_key(|b| std::cmp::Reverse(b.pages)),
        Some("paper") => books.sort_by(|x, y| {
            let v = |b: &database::BookStats| b.tone.map_or(f64::NEG_INFINITY, |t| t.yellowness);
            v(y).total_cmp(&v(x))
        }),
        Some(other) => {
            let c = class_by_name(other).ok_or_else(|| {
                format!(
                    "--sort: {other:?} is not abandon, large, todo, pictures, name, pages, paper or a class name"
                )
            })? as usize;
            books.sort_by(|x, y| key(y, c).total_cmp(&key(x, c)));
        }
        None => {}
    }
    if a.has("--flagged") {
        books.retain(|b| flagged(b));
    }
    match a.get("--show") {
        Some("excluded") => books.retain(|b| b.excluded),
        Some("training") => books.retain(|b| !b.excluded),
        Some("needs-work") => books.retain(|b| b.needs_work),
        Some(other) => {
            return Err(format!(
                "--show: {other:?} is not excluded, training or needs-work"
            ));
        }
        None => {}
    }
    let n: usize = a.num("--limit", usize::MAX)?;
    println!(
        "{:<40} {:>10} {:>6} {:>5} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8} {:>6}  flag",
        "book",
        "split",
        "pages",
        "done",
        "to do",
        "pic.pp",
        "pic.todo",
        "paper",
        "figure",
        "table",
        "abandon",
        "large"
    );
    let fmt = |b: &database::BookStats, c: usize| {
        if per_page {
            format!("{:.2}", b.per_page(c))
        } else {
            b.class_boxes[c].to_string()
        }
    };
    for b in books.iter().take(n) {
        println!(
            "{:<40} {:>10} {:>6} {:>4}% {:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8} {:>6}  {}",
            b.book_id.chars().take(40).collect::<String>(),
            b.split,
            b.pages,
            (b.done * 100).checked_div(b.pages).unwrap_or(0),
            b.todo,
            b.picture_pages,
            b.picture_todo,
            b.tone
                .map_or("-".to_owned(), |t| format!("{:.1}", t.yellowness)),
            fmt(b, 6),
            fmt(b, 8),
            fmt(b, ABANDON as usize),
            b.abandon_large,
            [
                if flagged(b) { "ABANDON" } else { "" },
                if b.excluded { "EXCLUDED" } else { "" },
                if b.needs_work { "NEEDS-WORK" } else { "" },
            ]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
        );
    }
    println!(
        "{} books, {} flagged ({}+ abandon boxes left per page and {}+ in all); counted in {ms:.0} ms{}",
        books.len(),
        books.iter().filter(|b| flagged(b)).count(),
        settings.abandon_flag_per_page,
        settings.abandon_flag_min_boxes,
        if per_page {
            "; label counts per page"
        } else {
            ""
        }
    );
    println!(
        "paper = CIE b* of the paper colour: about 0 white or grey, 15+ yellowed (--paper measures missing books)"
    );
    Ok(())
}

fn cmd_split(a: &Args) -> Result<(), String> {
    let project = project_or_default(a)?;
    let mut conn = database::open(&project)?;
    if let Some(book) = a.get("--book") {
        let to = a.get("--to").ok_or("--book needs --to SPLIT")?;
        let n = database::set_book_split(&conn, book, to)?;
        println!("{book}: {n} pages moved to split {to:?}");
        return Ok(());
    }
    if !a.has("--assign-new") {
        return Err("split: give --book ID --to SPLIT, or --assign-new".into());
    }
    let mut plan = database::SplitPlan::default();
    if let Some(f) = a.get("--from") {
        plan.from = f.to_owned();
    }
    plan.dev.1 = a.num("--dev", plan.dev.1)?;
    plan.test.1 = a.num("--test", plan.test.1)?;
    if let Some(n) = a.get("--test-name") {
        plan.test.0 = n.to_owned();
    }
    if !(0.0..=1.0).contains(&(plan.dev.1 + plan.test.1)) {
        return Err("--dev plus --test must be between 0 and 1".into());
    }
    let dry = a.has("--dry-run");
    let moved = database::assign_splits(&mut conn, &plan, dry)?;
    let count = |s: &str| moved.iter().filter(|(_, x)| x == s).count();
    println!(
        "{} {} books of split {:?}: {} {}, {} {}, {} {}",
        if dry { "would assign" } else { "assigned" },
        moved.len(),
        plan.from,
        count(&plan.train),
        plan.train,
        count(&plan.dev.0),
        plan.dev.0,
        count(&plan.test.0),
        plan.test.0
    );
    Ok(())
}

fn cmd_queues(a: &Args) -> Result<(), String> {
    let project = project_or_default(a)?;
    let conn = database::open(&project)?;
    let f = a.filter_for(&project)?;
    let counts = database::queue_counts(&conn, &f)?;
    for (q, n) in Queue::ALL.iter().zip(counts) {
        println!(
            "{:<14} {:>8}  {} — {}",
            q.slug(),
            n,
            q.title(f.large_share()),
            q.help()
        );
    }
    Ok(())
}

fn cmd_flag(a: &Args) -> Result<(), String> {
    let project = project_or_default(a)?;
    let conn = database::open(&project)?;
    let book = a.get("--book").ok_or("--book ID is required")?;
    let mut f = database::get_book_flags(&conn, book)?;
    if a.has("--exclude") {
        f.excluded = true;
    }
    if a.has("--include") {
        f.excluded = false;
    }
    if a.has("--needs-work") {
        f.needs_work = true;
    }
    if a.has("--done") {
        f.needs_work = false;
    }
    database::set_book_flags(&conn, book, f)?;
    println!(
        "{book}: {} · {}",
        if f.excluded {
            "excluded from training"
        } else {
            "for training"
        },
        if f.needs_work {
            "needs work"
        } else {
            "no work mark"
        }
    );
    Ok(())
}

fn cmd_convert(a: &Args) -> Result<(), String> {
    use crate::annotation::labels::{ABANDON, class_by_name, is_taxonomy};
    use crate::dataset::database::Region;
    let project = project_or_default(a)?;
    let mut conn = database::open(&project)?;
    if a.has("--undo-last") {
        let (id, desc, _) = database::last_bulk_op(&conn)?.ok_or("no bulk conversion to undo")?;
        let n = database::undo_bulk(&mut conn, id)?;
        println!("undone: {desc} ({n} boxes restored)");
        return Ok(());
    }
    let to_name = a.get("--to").ok_or("--to CLASS is required")?;
    let to = class_by_name(to_name)
        .filter(|c| is_taxonomy(*c))
        .ok_or_else(|| format!("--to: {to_name:?} is not a taxonomy class"))?;
    let region = match a.get("--region").unwrap_or("all") {
        "all" => Region::All,
        "top" => Region::Top,
        "bottom" => Region::Bottom,
        "margins" => Region::Margins,
        "large" => Region::Large,
        "other" => Region::Other,
        r => {
            return Err(format!(
                "--region: {r:?} is not all, top, bottom, margins, large or other"
            ));
        }
    };
    let filter = a.filter_for(&project)?;
    let r = database::convert_class(&mut conn, &filter, region, ABANDON, to)?;
    println!(
        "converted {} abandon boxes ({}) on {} pages to {to_name}; `convert --undo-last` reverts it",
        r.boxes,
        region.label(),
        r.pages
    );
    Ok(())
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
    let project = project_or_default(a)?;
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
        filter: a.filter_for(&project)?,
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
    let project = project_or_default(a)?;
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
    let project = project_or_default(a)?;
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
    let project = project_or_default(a)?;
    let conn = database::open(&project)?;
    let model = a.get("--model").ok_or("--model NAME is required")?;
    let thr: f64 = a.num("--threshold", 0.35)?;
    let mut filter = a.filter_for(&project)?;
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
    tx.commit().map_err(|e| e.to_string())?;
    database::recount_pages(&conn, "seq >= ?1", &[&base])
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
    println!(
        "(ms per frame incl. HUD. drag = continuous corner drag; zoom = wheel, page resampled every frame;"
    );
    println!(
        " pan = middle-drag, raster shifted + exposed strips; switch = navigation to a prefetched page until shown;"
    );
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
    if a.has("--books") {
        app.show_books();
    }
    if a.has("--grid") {
        app.show_grid();
        app.wait_for_image(Duration::from_secs(20));
    }
    let mut buf = WindowBuffer::new(w, h);
    app.render(&mut buf, scale);
    for step in a
        .get("--keys")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Page N (1-6) of the six-page view: point at it, or click it.
        let cell = step
            .strip_prefix("hovercell:")
            .map(|n| (n, false))
            .or_else(|| step.strip_prefix("cell:").map(|n| (n, true)));
        if let Some((n, click)) = cell {
            use liminalkit_shell::MouseButton;
            let i: usize = n
                .parse()
                .map_err(|_| format!("{step}: not a page number"))?;
            let r = app
                .grid_layout()
                .and_then(|l| l.cells.get(i.wrapping_sub(1)).copied())
                .ok_or_else(|| format!("{step}: the six-page view is not showing"))?;
            app.on_cursor((r.x + r.w / 2) as f32, (r.y + r.h / 2) as f32);
            app.render(&mut buf, scale);
            if click {
                app.on_mouse(MouseButton::Left, true);
                app.on_mouse(MouseButton::Left, false);
                app.render(&mut buf, scale);
                app.wait_for_image(Duration::from_secs(10));
                app.render(&mut buf, scale);
            }
            continue;
        }
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
            "Up" => (KeyInput::Up, "ArrowUp".to_owned()),
            "Down" => (KeyInput::Down, "ArrowDown".to_owned()),
            "Enter" => (KeyInput::Enter, "Enter".to_owned()),
            "Home" => (KeyInput::Home, "Home".to_owned()),
            "End" => (KeyInput::End, "End".to_owned()),
            "PgUp" => (KeyInput::PageUp, "PageUp".to_owned()),
            "PgDn" => (KeyInput::PageDown, "PageDown".to_owned()),
            f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok() => (
                KeyInput::Function(f[1..].parse().unwrap_or(1)),
                f.to_owned(),
            ),
            "Escape" => (KeyInput::Escape, "Escape".to_owned()),
            "Delete" => (KeyInput::Delete, "Delete".to_owned()),
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
