//! Projects made from a dataset folder, the book manager's counts, and
//! `abandon` conversion: one box, a page, a whole book by region, undo.

mod common;

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use liminal_annotator::annotation::commands::ReviewStatus;
use liminal_annotator::annotation::labels::{ABANDON, class_by_name};
use liminal_annotator::app::{AnnotatorApp, AppConfig};
use liminal_annotator::dataset::database::{
    self, PageFilter, PageSave, Region, book_stats, convert_class, count_by_region, undo_bulk,
};
use liminal_annotator::dataset::export_coco::{ExportOptions, export};
use liminal_annotator::dataset::import_folder::{
    FolderKind, FolderOptions, PADDLE_MODEL, detect, needs_sync, project_for, sync_folder,
};
use liminalkit_shell::{KeyInput, Modifiers};
use liminalkit_windowing::{KeyEvent, PixelApp};
use rusqlite::{Connection, params};

const W: u32 = 200;
const H: u32 = 300;

/// One page's teacher row: text, three YOLO `abandon` boxes (top band,
/// bottom band, large) and PP-DocLayout-L's view of the text.
fn row(id: &str) -> String {
    format!(
        r#"{{"image_id": "{id}", "width": {W}, "height": {H}, "paddle": [["text", 0.85, 10, 50, 190, 120], ["header", 0.2, 20, 128, 180, 140]], "yolo": [["plain text", 0.9, 10, 50, 190, 120], ["abandon", 0.8, 20, 5, 170, 20], ["abandon", 0.7, 90, 285, 110, 298], ["abandon", 0.5, 40, 150, 160, 250], ["figure", 0.1, 0, 0, 10, 10]], "ms": [1, 1]}}"#
    )
}

/// `book-a` (3 pages, train) and `book-b` (2 pages), teacher-pool layout.
fn pool(dir: &Path) {
    for (book, n) in [("book-a", 3), ("book-b", 2)] {
        let d = dir.join("images").join(book);
        std::fs::create_dir_all(&d).unwrap();
        for i in 0..n {
            let img = image::RgbImage::from_pixel(W, H, image::Rgb([240, 236, 228]));
            img.save(d.join(format!("p{i:04}.jpg"))).unwrap();
        }
    }
    let mut f = std::fs::File::create(dir.join("teachers_raw.jsonl")).unwrap();
    for id in [
        "book-a_p0000",
        "book-a_p0001",
        "book-b_p0000",
        "book-a_p0002",
        "book-b_p0001",
    ] {
        writeln!(f, "{}", row(id)).unwrap();
    }
    std::fs::write(
        dir.join("books.jsonl"),
        "{\"path\": \"x.pdf\", \"status\": \"done\", \"book_id\": \"book-a\", \"split\": \"train\"}\n",
    )
    .unwrap();
}

fn create(dir: &Path) -> (std::path::PathBuf, Connection) {
    let project = project_for(dir);
    let mut conn = database::open(&project).unwrap();
    let report = sync_folder(&mut conn, dir, &FolderOptions::default(), &mut |_, _, _| {}).unwrap();
    assert_eq!(report.pages_added, 5, "{report}");
    assert_eq!(report.books_added, 2);
    // Three abandon boxes a page are pre-filled; the 0.1 figure is not.
    assert_eq!(report.boxes, 5 * 4);
    assert_eq!(report.abandon_boxes, 5 * 3);
    (project, conn)
}

fn book_a() -> PageFilter {
    PageFilter {
        book_id: Some("book-a".into()),
        ..PageFilter::default()
    }
}

#[test]
fn a_folder_becomes_a_project_inside_it() {
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    assert!(matches!(
        detect(dir.path()).unwrap(),
        FolderKind::Teachers { .. }
    ));
    let (project, conn) = create(dir.path());
    assert_eq!(project, dir.path().join("annotator.db"));
    assert!(project.is_file());

    // Pages of a book are in page order whatever the file order.
    let keys = database::page_keys(&conn, &book_a()).unwrap();
    assert_eq!(keys, ["book-a_p0000", "book-a_p0001", "book-a_p0002"]);
    let meta = database::page_meta(&conn, "book-a_p0002").unwrap().unwrap();
    assert_eq!(meta.split, "train");
    assert_eq!(meta.page_index, Some(2));
    assert!(
        meta.source_path.ends_with("images/book-a/p0002.jpg")
            || meta.source_path.ends_with("images\\book-a\\p0002.jpg")
    );
    let b = database::page_meta(&conn, "book-b_p0000").unwrap().unwrap();
    assert_eq!(b.split, "new");

    // The other teacher is a prediction layer (its 0.2 header is below
    // 0.3); it covers none of the abandon boxes, so they stay abandon.
    assert_eq!(database::models(&conn).unwrap(), [PADDLE_MODEL]);
    assert_eq!(
        database::predictions(&conn, "book-a_p0000", PADDLE_MODEL)
            .unwrap()
            .len(),
        1
    );
    let anns = database::annotations(&conn, "book-a_p0000").unwrap();
    assert_eq!(anns.iter().filter(|a| a.class_id == ABANDON).count(), 3);
    assert!(
        anns[0]
            .provenance_json
            .as_deref()
            .unwrap()
            .contains("doclayout-yolo")
    );

    // Book manager counts.
    let stats = book_stats(&conn).unwrap();
    assert_eq!(stats.len(), 2);
    let a = &stats[0];
    assert_eq!((a.book_id.as_str(), a.pages, a.done), ("book-a", 3, 0));
    assert_eq!(
        (
            a.abandon_boxes,
            a.abandon_pages,
            a.abandon_large,
            a.abandon_imported
        ),
        (9, 3, 3, 9)
    );
    assert!((a.abandon_per_page() - 3.0).abs() < 1e-9);
    // Every label is counted: one text box a page.
    assert_eq!((a.class_boxes[0], a.class_pages[0]), (3, 3));
    assert_eq!(a.class_boxes[6], 0);
    assert_eq!(a.class_boxes[ABANDON as usize], 9);
}

#[test]
fn a_rescan_imports_only_what_is_new() {
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    let (_, mut conn) = create(dir.path());
    assert!(!needs_sync(&conn));
    // The teacher pass carries on: one more page, and a half-written line.
    let d = dir.path().join("images").join("book-c");
    std::fs::create_dir_all(&d).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("teachers_raw.jsonl"))
        .unwrap();
    writeln!(f, "{}", row("book-c_p0000")).unwrap();
    write!(f, "{{\"image_id\": \"book-c_p0001\", \"wid").unwrap();
    drop(f);
    assert!(needs_sync(&conn));
    let r = sync_folder(
        &mut conn,
        dir.path(),
        &FolderOptions::default(),
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!((r.pages_added, r.pages_present, r.books_added), (1, 5, 1));
    assert_eq!(
        r.warnings.len(),
        1,
        "the partial line is reported, not imported"
    );
    assert!(!needs_sync(&conn));
    // Book c sorts after the books imported before it.
    let stats = book_stats(&conn).unwrap();
    assert_eq!(stats.last().unwrap().book_id, "book-c");
}

#[test]
fn bulk_conversion_by_region_and_its_undo() {
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    let (_, mut conn) = create(dir.path());
    let counts = count_by_region(&conn, &book_a(), ABANDON).unwrap();
    let get = |r: Region| {
        counts
            .iter()
            .find(|c| c.0 == r)
            .map(|c| (c.1, c.2))
            .unwrap()
    };
    assert_eq!(get(Region::All), (9, 3));
    assert_eq!(get(Region::Top), (3, 3));
    assert_eq!(get(Region::Bottom), (3, 3));
    assert_eq!(get(Region::Large), (3, 3));
    assert_eq!(get(Region::Margins), (0, 0));
    assert_eq!(get(Region::Other), (0, 0));
    // The SQL rule and the in-memory rule agree on every box.
    for r in Region::ALL {
        let mut n = 0;
        for k in database::page_keys(&conn, &book_a()).unwrap() {
            n += database::annotations(&conn, &k)
                .unwrap()
                .iter()
                .filter(|a| a.class_id == ABANDON && r.contains(&a.bbox, W as f64, H as f64))
                .count() as i64;
        }
        assert_eq!(n, get(r).0, "{r:?}");
    }

    let header = class_by_name("running_header").unwrap();
    let rev_before = database::page_meta(&conn, "book-a_p0001")
        .unwrap()
        .unwrap()
        .revision;
    let r = convert_class(&mut conn, &book_a(), Region::Top, ABANDON, header).unwrap();
    assert_eq!((r.boxes, r.pages), (3, 3));
    let after = database::page_meta(&conn, "book-a_p0001").unwrap().unwrap();
    assert_eq!(after.revision, rev_before + 1);
    let stats = book_stats(&conn).unwrap();
    assert_eq!(
        stats[0].abandon_boxes, 6,
        "per-page counts follow the conversion"
    );
    assert_eq!(stats[0].class_boxes[header as usize], 3);
    assert_eq!(stats[0].class_pages[header as usize], 3);
    assert_eq!(stats[1].abandon_boxes, 6, "book-b untouched");
    assert_eq!(
        database::annotations(&conn, "book-a_p0000")
            .unwrap()
            .iter()
            .filter(|a| a.class_id == header)
            .count(),
        1
    );
    let footer = class_by_name("page_number").unwrap();
    convert_class(&mut conn, &book_a(), Region::Bottom, ABANDON, footer).unwrap();

    // Undo the most recent one; a box edited after it keeps the edit.
    let (op, desc, boxes) = database::last_bulk_op(&conn).unwrap().unwrap();
    assert!(desc.contains("page_number") && boxes == 3, "{desc}");
    let mut anns = database::annotations(&conn, "book-a_p0002").unwrap();
    let i = anns.iter().position(|a| a.class_id == footer).unwrap();
    anns[i].class_id = 5;
    database::save_page(
        &mut conn,
        &PageSave {
            page_key: "book-a_p0002".into(),
            status: ReviewStatus::Opened,
            annotations: anns,
            revision: 1,
            kind: "edit",
        },
    )
    .unwrap();
    assert_eq!(undo_bulk(&mut conn, op).unwrap(), 2);
    assert_eq!(book_stats(&conn).unwrap()[0].abandon_boxes, 5);
    // The earlier conversion is now the last one.
    let (_, desc, _) = database::last_bulk_op(&conn).unwrap().unwrap();
    assert!(desc.contains("running_header"), "{desc}");
}

#[test]
fn export_leaves_out_pages_with_unconverted_abandon() {
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    let (_, mut conn) = create(dir.path());
    // Resolve every abandon box of one page by hand.
    let mut anns = database::annotations(&conn, "book-b_p0001").unwrap();
    anns.retain(|a| a.class_id != ABANDON);
    database::save_page(
        &mut conn,
        &PageSave {
            page_key: "book-b_p0001".into(),
            status: ReviewStatus::Reviewed,
            annotations: anns,
            revision: 1,
            kind: "edit",
        },
    )
    .unwrap();
    assert_eq!(book_stats(&conn).unwrap()[1].abandon_pages, 1);
    let r = export(
        &conn,
        &ExportOptions {
            out_dir: dir.path().join("exports"),
            name: Some("t".into()),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(r.pages, 1);
    assert_eq!(r.unresolved_abandon_pages, 4);
}

#[test]
fn a_schema_1_project_is_upgraded_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE pages (page_key TEXT PRIMARY KEY, seq INTEGER NOT NULL, book_id TEXT NOT NULL, \
             split TEXT NOT NULL, set_name TEXT NOT NULL, source_path TEXT NOT NULL, file_name TEXT NOT NULL, \
             source_hash TEXT, width INTEGER NOT NULL, height INTEGER NOT NULL, book_type TEXT, page_index INTEGER, \
             queue_reason TEXT, review_status TEXT NOT NULL DEFAULT 'unseen', revision INTEGER NOT NULL DEFAULT 0, \
             priority REAL NOT NULL DEFAULT 0, coco_image_id INTEGER, image_json TEXT, updated_at INTEGER);
             CREATE TABLE annotations (id TEXT PRIMARY KEY, page_key TEXT NOT NULL REFERENCES pages(page_key), \
             ord INTEGER NOT NULL, class_id INTEGER NOT NULL CHECK(class_id BETWEEN 0 AND 13), x0 REAL NOT NULL, \
             y0 REAL NOT NULL, x1 REAL NOT NULL, y1 REAL NOT NULL, is_ignore INTEGER NOT NULL DEFAULT 0, \
             provenance_json TEXT, CHECK(x1 > x0 AND y1 > y0));
             CREATE INDEX annotations_by_page ON annotations(page_key);
             INSERT INTO pages (page_key, seq, book_id, split, set_name, source_path, file_name, width, height) \
             VALUES ('p', 0, 'b', 'train', 's', 'x.jpg', 'x.jpg', 100, 100);
             INSERT INTO annotations (id, page_key, ord, class_id, x0, y0, x1, y1) VALUES ('p#0', 'p', 0, 6, 1, 1, 50, 50);",
        )
        .unwrap();
    }
    let conn = database::open(&path).unwrap();
    assert_eq!(
        database::annotations(&conn, "p").unwrap().len(),
        1,
        "rows kept"
    );
    conn.execute(
        "INSERT INTO annotations (id, page_key, ord, class_id, x0, y0, x1, y1) VALUES ('p#1', 'p', 1, ?1, 1, 1, 9, 9)",
        params![ABANDON as i64],
    )
    .expect("the holding class is now allowed");
    assert_eq!(
        database::meta_get(&conn, "schema_version")
            .unwrap()
            .as_deref(),
        Some("2")
    );
    let stats = book_stats(&conn).unwrap();
    assert_eq!(stats[0].pages, 1);
    assert_eq!(
        stats[0].class_boxes[6], 1,
        "class counts filled in on upgrade"
    );
    // Opening again is a no-op.
    drop(conn);
    database::open(&path).unwrap();
}

fn key(app: &mut AnnotatorApp, k: KeyInput, physical: &str, m: Modifiers) {
    let text = match k {
        KeyInput::Character(c) => c.to_string(),
        _ => String::new(),
    };
    app.on_key_event(&KeyEvent {
        key: k,
        text,
        physical: physical.into(),
        pressed: true,
        repeat: false,
        modifiers: m,
    });
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

fn abandon_on_page(app: &AnnotatorApp) -> usize {
    app.doc
        .as_ref()
        .unwrap()
        .annotations
        .iter()
        .filter(|a| a.class_id == ABANDON)
        .count()
}

#[test]
fn the_editor_converts_abandon_boxes_one_page_and_one_book_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    let (project, conn) = create(dir.path());
    drop(conn);
    let mut app = AnnotatorApp::new(AppConfig {
        project: project.clone(),
        filter: PageFilter::default(),
        start_at: None,
        prediction_threshold: 0.3,
    })
    .unwrap();
    let mut screen = common::Screen::new(1200, 800, 1.0);

    // The book manager lists both books; choosing one makes it the queue.
    app.show_books();
    assert!(app.books_showing());
    screen.frame(&mut app);
    for c in "book-b".chars() {
        key(&mut app, KeyInput::Character(c), "", Modifiers::default());
    }
    key(&mut app, KeyInput::Enter, "Enter", Modifiers::default());
    assert!(!app.books_showing());
    assert_eq!(app.filter.book_id.as_deref(), Some("book-b"));
    assert_eq!(app.keys.len(), 2);
    assert_eq!(app.last_book().as_deref(), Some("book-b"));
    app.open_book(Some("book-a".into()));
    assert_eq!(app.keys.len(), 3);
    assert!(app.wait_for_image(Duration::from_secs(10)));
    screen.frame(&mut app);

    // A selects an abandon box; a class key converts that one box.
    key(
        &mut app,
        KeyInput::Character('a'),
        "KeyA",
        Modifiers::default(),
    );
    let sel = app
        .editor
        .selected
        .clone()
        .expect("an abandon box is selected");
    // The first abandon box in score order: the top band's.
    let b = app.doc.as_ref().unwrap().get(&sel).unwrap().bbox;
    assert!(Region::Top.contains(&b, W as f64, H as f64));
    let text_key = (KeyInput::Character('1'), "Digit1");
    key(&mut app, text_key.0, text_key.1, Modifiers::default());
    let doc = app.doc.as_ref().unwrap();
    assert_eq!(doc.get(&sel).unwrap().class_id, 0);
    assert_eq!(abandon_on_page(&app), 2);

    // Shift+A on a page with abandon boxes starts on this page; digit 7 is
    // running_header in the default hotkeys. One undo step.
    key(&mut app, KeyInput::Character('A'), "KeyA", shift());
    screen.frame(&mut app);
    key(
        &mut app,
        KeyInput::Character('7'),
        "Digit7",
        Modifiers::default(),
    );
    assert_eq!(abandon_on_page(&app), 0);
    key(
        &mut app,
        KeyInput::Character('z'),
        "KeyZ",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(abandon_on_page(&app), 2);

    // Book-wide, top band only: Tab to the book, Down to "top band".
    key(&mut app, KeyInput::Character('A'), "KeyA", shift());
    key(&mut app, KeyInput::Tab, "Tab", Modifiers::default());
    key(&mut app, KeyInput::Down, "ArrowDown", Modifiers::default());
    screen.frame(&mut app);
    key(
        &mut app,
        KeyInput::Character('7'),
        "Digit7",
        Modifiers::default(),
    );
    app.flush();
    let header = class_by_name("running_header").unwrap();
    {
        let c = database::open(&project).unwrap();
        // Page 0's top-band box was converted to text by hand, so the
        // top band held two abandon boxes: pages 1 and 2.
        let s = book_stats(&c).unwrap();
        assert_eq!(s[0].abandon_boxes, 6, "{s:?}");
        for (k, n) in [
            ("book-a_p0000", 0),
            ("book-a_p0001", 1),
            ("book-a_p0002", 1),
        ] {
            let anns = database::annotations(&c, k).unwrap();
            assert_eq!(
                anns.iter().filter(|a| a.class_id == header).count(),
                n,
                "{k}"
            );
        }
        assert_eq!(s[1].abandon_boxes, 6, "book-b untouched");
    }
    // The open page was reloaded from the database, hand edit included.
    let doc = app.doc.as_ref().unwrap();
    assert_eq!(doc.get(&sel).unwrap().class_id, 0);
    assert_eq!(abandon_on_page(&app), 2);

    // Shift+A, U undoes the book-wide conversion.
    key(&mut app, KeyInput::Character('A'), "KeyA", shift());
    key(
        &mut app,
        KeyInput::Character('u'),
        "KeyU",
        Modifiers::default(),
    );
    app.flush();
    let c = database::open(&project).unwrap();
    assert_eq!(book_stats(&c).unwrap()[0].abandon_boxes, 8);
    app.shutdown();
}

/// A pool where PP-DocLayout-L boxes two of YOLO's three abandon regions:
/// a running head (score 0.2, above the 0.15 used for resolving) and a page
/// number. The large one has no PP box.
fn pool_with_pp(dir: &Path) {
    let d = dir.join("images").join("book-p");
    std::fs::create_dir_all(&d).unwrap();
    let mut f = std::fs::File::create(dir.join("teachers_raw.jsonl")).unwrap();
    for i in 0..2 {
        image::RgbImage::from_pixel(W, H, image::Rgb([240, 236, 228]))
            .save(d.join(format!("p{i:04}.jpg")))
            .unwrap();
        writeln!(
            f,
            r#"{{"image_id": "book-p_p{i:04}", "width": {W}, "height": {H}, "paddle": [["text", 0.85, 10, 50, 190, 120], ["header", 0.2, 22, 6, 168, 21], ["number", 0.6, 88, 284, 112, 298]], "yolo": [["plain text", 0.9, 10, 50, 190, 120], ["abandon", 0.8, 20, 5, 170, 20], ["abandon", 0.7, 90, 285, 110, 298], ["abandon", 0.5, 40, 150, 160, 250]]}}"#
        )
        .unwrap();
    }
}

fn classes(conn: &Connection, key: &str) -> Vec<u8> {
    let mut v: Vec<u8> = database::annotations(conn, key)
        .unwrap()
        .iter()
        .map(|a| a.class_id)
        .collect();
    v.sort();
    v
}

#[test]
fn abandon_boxes_take_the_other_teachers_label_at_import() {
    let dir = tempfile::tempdir().unwrap();
    pool_with_pp(dir.path());
    let mut conn = database::open(&project_for(dir.path())).unwrap();
    let r = sync_folder(
        &mut conn,
        dir.path(),
        &FolderOptions::default(),
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(r.abandon_resolved, 4, "{r}");
    let header = class_by_name("running_header").unwrap();
    let number = class_by_name("page_number").unwrap();
    // text, page number, running head, and the large box left as abandon.
    let mut want = vec![0, header, number, ABANDON];
    want.sort();
    assert_eq!(classes(&conn, "book-p_p0000"), want);
    // The imported layer is the resolved one, so nothing reads as an edit.
    let imp: Vec<u8> = database::imported_annotations(&conn, "book-p_p0000")
        .unwrap()
        .iter()
        .map(|a| a.class_id)
        .collect();
    assert_eq!(imp.iter().filter(|c| **c == ABANDON).count(), 1);
    let a = database::annotations(&conn, "book-p_p0000").unwrap();
    let head = a.iter().find(|a| a.class_id == header).unwrap();
    let prov = head.provenance_json.as_deref().unwrap();
    assert!(
        prov.contains("\"label_class\":\"header\"")
            && prov.contains("\"teacher_class\":\"abandon\""),
        "{prov}"
    );
    assert_eq!(book_stats(&conn).unwrap()[0].abandon_boxes, 2);
    assert!(!needs_sync(&conn), "a new project needs no second pass");
}

#[test]
fn an_older_project_gets_the_labels_once_and_can_undo_them() {
    let dir = tempfile::tempdir().unwrap();
    pool_with_pp(dir.path());
    let project = project_for(dir.path());
    let mut conn = database::open(&project).unwrap();
    // An import as the previous version made it: nothing resolved.
    let old = FolderOptions {
        resolve_min_score: 2.0,
        ..FolderOptions::default()
    };
    sync_folder(&mut conn, dir.path(), &old, &mut |_, _, _| {}).unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'abandon_resolved'", [])
        .unwrap();
    // A reviewer already converted one abandon box by hand; it stays as it is.
    let mut anns = database::annotations(&conn, "book-p_p0001").unwrap();
    let i = anns
        .iter()
        .position(|a| a.class_id == ABANDON && a.bbox.y0 < 10.0)
        .unwrap();
    anns[i].class_id = 1;
    database::save_page(
        &mut conn,
        &PageSave {
            page_key: "book-p_p0001".into(),
            status: ReviewStatus::Opened,
            annotations: anns,
            revision: 1,
            kind: "edit",
        },
    )
    .unwrap();
    assert_eq!(book_stats(&conn).unwrap()[0].abandon_boxes, 5);

    assert!(
        needs_sync(&conn),
        "an older project is due the one-time pass"
    );
    let r = sync_folder(
        &mut conn,
        dir.path(),
        &FolderOptions::default(),
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(r.abandon_resolved, 3, "{r}");
    assert!(!needs_sync(&conn));
    let header = class_by_name("running_header").unwrap();
    let number = class_by_name("page_number").unwrap();
    let mut want = vec![0, 1, number, ABANDON];
    want.sort();
    assert_eq!(
        classes(&conn, "book-p_p0001"),
        want,
        "the hand edit is kept"
    );
    let mut want = vec![0, header, number, ABANDON];
    want.sort();
    assert_eq!(classes(&conn, "book-p_p0000"), want);
    let stats = book_stats(&conn).unwrap();
    assert_eq!(stats[0].abandon_boxes, 2);
    assert_eq!(stats[0].class_boxes[number as usize], 2);

    // It is one logged change, and undo restores every box it labelled.
    let (op, desc, boxes) = database::last_bulk_op(&conn).unwrap().unwrap();
    assert!(desc.contains("PP-DocLayout-L") && boxes == 3, "{desc}");
    assert_eq!(undo_bulk(&mut conn, op).unwrap(), 3);
    assert_eq!(book_stats(&conn).unwrap()[0].abandon_boxes, 5);
}

fn set_class(conn: &mut Connection, key: &str, pick: impl Fn(&Box2) -> bool, to: u8) {
    let mut anns = database::annotations(conn, key).unwrap();
    for a in anns.iter_mut().filter(|a| pick(&a.bbox)) {
        a.class_id = to;
    }
    database::save_page(
        conn,
        &PageSave {
            page_key: key.into(),
            status: ReviewStatus::Opened,
            annotations: anns,
            revision: 1,
            kind: "edit",
        },
    )
    .unwrap();
}

use liminal_annotator::annotation::geometry::Box2;
use liminal_annotator::dataset::database::{Tolerance, is_similar, similar_boxes};

#[test]
fn same_place_matching_ignores_side_and_title_length_but_not_shape() {
    let head = Box2::new(20.0, 5.0, 170.0, 20.0);
    // The facing page's running head: other side, longer title.
    assert!(is_similar(
        &head,
        200.0,
        300.0,
        &Box2::new(10.0, 6.0, 190.0, 21.0),
        200.0,
        300.0,
        Tolerance::Normal
    ));
    // The same place on a page rendered at another size.
    assert!(is_similar(
        &head,
        200.0,
        300.0,
        &Box2::new(40.0, 10.0, 340.0, 40.0),
        400.0,
        600.0,
        Tolerance::Tight
    ));
    // A page number beside it: same line, a tenth of the width.
    assert!(!is_similar(
        &head,
        200.0,
        300.0,
        &Box2::new(175.0, 5.0, 190.0, 20.0),
        200.0,
        300.0,
        Tolerance::Normal
    ));
    // The first line of text further down.
    assert!(!is_similar(
        &head,
        200.0,
        300.0,
        &Box2::new(20.0, 50.0, 170.0, 65.0),
        200.0,
        300.0,
        Tolerance::Normal
    ));
}

#[test]
fn a_running_head_labelled_three_ways_becomes_one_class() {
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    let (_, mut conn) = create(dir.path());
    let top = |b: &Box2| b.y0 < 10.0;
    set_class(&mut conn, "book-a_p0000", top, 1); // heading
    set_class(&mut conn, "book-a_p0001", top, 2); // running head
    // page 2 keeps abandon
    let t = Box2::new(20.0, 5.0, 170.0, 20.0);
    let m = similar_boxes(
        &conn,
        "book-a",
        &t,
        W as f64,
        H as f64,
        &database::FURNITURE_FAMILY,
        Tolerance::Normal,
    )
    .unwrap();
    let mut got: Vec<(String, u8)> = m.iter().map(|s| (s.page_key.clone(), s.class_id)).collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("book-a_p0000".into(), 1),
            ("book-a_p0001".into(), 2),
            ("book-a_p0002".into(), ABANDON)
        ],
        "only the top boxes: not the page numbers at the foot, not the large boxes, not book-b"
    );
}

#[test]
fn m_in_the_editor_relabels_the_book_and_u_undoes_it() {
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    let (project, mut conn) = create(dir.path());
    let top = |b: &Box2| b.y0 < 10.0;
    set_class(&mut conn, "book-a_p0000", top, 2);
    set_class(&mut conn, "book-a_p0001", top, 1);
    drop(conn);
    let mut app = AnnotatorApp::new(AppConfig {
        project: project.clone(),
        filter: book_a(),
        start_at: None,
        prediction_threshold: 0.3,
    })
    .unwrap();
    assert!(app.wait_for_image(Duration::from_secs(10)));
    let id = app
        .doc
        .as_ref()
        .unwrap()
        .annotations
        .iter()
        .find(|a| top(&a.bbox))
        .unwrap()
        .id
        .clone();
    app.editor.selected = Some(id);
    key(
        &mut app,
        KeyInput::Character('m'),
        "KeyM",
        Modifiers::default(),
    );
    key(&mut app, KeyInput::Enter, "Enter", Modifiers::default());
    app.flush();
    let header = class_by_name("running_header").unwrap();
    {
        let c = database::open(&project).unwrap();
        for k in ["book-a_p0000", "book-a_p0001", "book-a_p0002"] {
            let a = database::annotations(&c, k).unwrap();
            assert_eq!(
                a.iter().find(|a| top(&a.bbox)).unwrap().class_id,
                header,
                "{k}"
            );
        }
        // Book b is another book: untouched.
        let b = database::annotations(&c, "book-b_p0000").unwrap();
        assert_eq!(b.iter().find(|a| top(&a.bbox)).unwrap().class_id, ABANDON);
        let (_, desc, boxes) = database::last_bulk_op(&c).unwrap().unwrap();
        assert_eq!(boxes, 2, "{desc}");
    }
    // M, then U: undone.
    let id = app
        .doc
        .as_ref()
        .unwrap()
        .annotations
        .iter()
        .find(|a| top(&a.bbox))
        .unwrap()
        .id
        .clone();
    app.editor.selected = Some(id);
    key(
        &mut app,
        KeyInput::Character('m'),
        "KeyM",
        Modifiers::default(),
    );
    key(
        &mut app,
        KeyInput::Character('u'),
        "KeyU",
        Modifiers::default(),
    );
    app.flush();
    let c = database::open(&project).unwrap();
    let a = database::annotations(&c, "book-a_p0001").unwrap();
    assert_eq!(a.iter().find(|a| top(&a.bbox)).unwrap().class_id, 1);
    app.shutdown();
}

#[test]
fn excluded_books_stay_out_of_the_export_and_flags_show_in_the_table() {
    use liminal_annotator::books::{BooksAction, BooksView};
    let dir = tempfile::tempdir().unwrap();
    pool(dir.path());
    let (_, mut conn) = create(dir.path());
    // Resolve every abandon box so only the flag decides.
    for k in database::page_keys(&conn, &PageFilter::default()).unwrap() {
        set_class(&mut conn, &k, |_| true, 0);
    }
    let mut v = BooksView::new(book_stats(&conn).unwrap(), 3.0, 20, None, String::new());
    // F6 on book-a (first row): unticks "train".
    let f6 = KeyEvent {
        key: KeyInput::Function(6),
        text: String::new(),
        physical: "F6".into(),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    };
    let BooksAction::SetFlags(book, flags) = v.key(&f6) else {
        panic!("F6 sets flags");
    };
    assert_eq!(book, "book-a");
    assert!(flags.excluded && !flags.needs_work);
    database::set_book_flags(&conn, &book, flags).unwrap();
    database::set_book_flags(
        &conn,
        "book-b",
        database::BookFlags {
            excluded: false,
            needs_work: true,
        },
    )
    .unwrap();
    let stats = book_stats(&conn).unwrap();
    assert!(stats[0].excluded && !stats[0].needs_work);
    assert!(!stats[1].excluded && stats[1].needs_work);
    let r = export(
        &conn,
        &ExportOptions {
            out_dir: dir.path().join("exports"),
            name: Some("t".into()),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!((r.pages, r.excluded_book_pages), (2, 3));
    assert_eq!(r.excluded_books, ["book-a"]);
}
