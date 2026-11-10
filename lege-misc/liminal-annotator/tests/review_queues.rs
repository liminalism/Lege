//! The review queues (Q), the status rules around them (Right refuses a
//! page with abandon boxes left; X/N toggled off restore the earlier
//! status), adopting a comparison box with a click, and split assignment.

mod common;

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use liminal_annotator::annotation::commands::{Annotation, ReviewStatus};
use liminal_annotator::annotation::geometry::{Box2, Point};
use liminal_annotator::annotation::labels::ABANDON;
use liminal_annotator::app::{AnnotatorApp, AppConfig, Pace};
use liminal_annotator::dataset::database::{
    self, PageFilter, PageSave, Queue, SplitPlan, assign_splits, book_fraction, book_stats,
    queue_counts, set_book_split,
};
use liminal_annotator::dataset::import_folder::{FolderOptions, project_for, sync_folder};
use liminalkit_shell::{KeyInput, Modifiers, MouseButton};
use liminalkit_windowing::{KeyEvent, PixelApp};
use rusqlite::Connection;

const W: u32 = 200;
const H: u32 = 300;

/// `book-q`, five pages: plain text; a picture both teachers see; a
/// picture only PP sees; nothing at all; and a YOLO abandon box PP has
/// nothing for.
fn pool(dir: &Path) {
    let d = dir.join("images").join("book-q");
    std::fs::create_dir_all(&d).unwrap();
    let text = r#"["plain text", 0.9, 10, 50, 190, 120]"#;
    let pp_text = r#"["text", 0.85, 10, 50, 190, 120]"#;
    let rows = [
        (format!("[{pp_text}]"), format!("[{text}]")),
        (
            r#"[["image", 0.9, 22, 132, 178, 248]]"#.to_owned(),
            r#"[["figure", 0.8, 20, 130, 180, 250]]"#.to_owned(),
        ),
        (
            format!(r#"[{pp_text}, ["image", 0.7, 30, 150, 170, 260]]"#),
            format!("[{text}]"),
        ),
        ("[]".to_owned(), "[]".to_owned()),
        (
            format!("[{pp_text}]"),
            format!(r#"[{text}, ["abandon", 0.6, 40, 150, 160, 250]]"#),
        ),
    ];
    let mut f = std::fs::File::create(dir.join("teachers_raw.jsonl")).unwrap();
    for (i, (paddle, yolo)) in rows.iter().enumerate() {
        image::RgbImage::from_pixel(W, H, image::Rgb([240, 236, 228]))
            .save(d.join(format!("p{i:04}.jpg")))
            .unwrap();
        writeln!(
            f,
            r#"{{"image_id": "book-q_p{i:04}", "width": {W}, "height": {H}, "paddle": {paddle}, "yolo": {yolo}}}"#
        )
        .unwrap();
    }
}

fn create(dir: &Path) -> (std::path::PathBuf, Connection) {
    pool(dir);
    let project = project_for(dir);
    let mut conn = database::open(&project).unwrap();
    let r = sync_folder(&mut conn, dir, &FolderOptions::default(), &mut |_, _, _| {}).unwrap();
    assert_eq!(r.pages_added, 5, "{r}");
    (project, conn)
}

fn counts(conn: &Connection) -> Vec<(Queue, i64)> {
    Queue::ALL
        .into_iter()
        .zip(queue_counts(conn, &PageFilter::default()).unwrap())
        .collect()
}

fn count(conn: &Connection, q: Queue) -> i64 {
    counts(conn).into_iter().find(|(x, _)| *x == q).unwrap().1
}

fn keys(conn: &Connection, q: Queue) -> Vec<String> {
    database::page_keys(
        conn,
        &PageFilter {
            queue: Some(q),
            ..PageFilter::default()
        },
    )
    .unwrap()
}

#[test]
fn queues_put_picture_pages_first_and_sample_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut conn) = create(dir.path());
    assert_eq!(count(&conn, Queue::ToDo), 5);
    assert_eq!(
        keys(&conn, Queue::Pictures),
        ["book-q_p0001", "book-q_p0002"]
    );
    assert_eq!(keys(&conn, Queue::ModelOnlyPictures), ["book-q_p0002"]);
    assert_eq!(keys(&conn, Queue::Abandon), ["book-q_p0004"]);
    assert_eq!(keys(&conn, Queue::Empty), ["book-q_p0003"]);
    assert_eq!(keys(&conn, Queue::TextSample), ["book-q_p0000"]);
    assert_eq!(count(&conn, Queue::All), 5);

    let b = &book_stats(&conn).unwrap()[0];
    assert_eq!(
        (b.todo, b.picture_pages, b.picture_todo, b.model_only_todo),
        (5, 2, 2, 1)
    );

    // Drawing the figure PP saw takes the page out of "only the model sees".
    let mut anns = database::annotations(&conn, "book-q_p0002").unwrap();
    anns.push(Annotation {
        id: "drawn".into(),
        class_id: 6,
        bbox: Box2::new(31.0, 151.0, 169.0, 259.0),
        is_ignore: false,
        provenance_json: None,
    });
    database::save_page(
        &mut conn,
        &PageSave {
            page_key: "book-q_p0002".into(),
            status: ReviewStatus::Reviewed,
            annotations: anns,
            revision: 1,
            kind: "edit",
        },
    )
    .unwrap();
    assert!(keys(&conn, Queue::ModelOnlyPictures).is_empty());
    assert_eq!(
        keys(&conn, Queue::Pictures),
        ["book-q_p0001"],
        "reviewed: no longer to do"
    );
    assert_eq!(count(&conn, Queue::Reviewed), 1);
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

fn plain(app: &mut AnnotatorApp, c: char, physical: &str) {
    key(app, KeyInput::Character(c), physical, Modifiers::default());
}

fn status(project: &Path, page: &str) -> ReviewStatus {
    let conn = database::open(project).unwrap();
    database::page_meta(&conn, page).unwrap().unwrap().status
}

fn open(project: &Path) -> AnnotatorApp {
    AnnotatorApp::new(AppConfig {
        project: project.to_path_buf(),
        filter: PageFilter::default(),
        start_at: None,
        prediction_threshold: 0.3,
    })
    .unwrap()
}

#[test]
fn the_q_picker_opens_a_queue_and_a_click_adopts_the_models_picture() {
    let dir = tempfile::tempdir().unwrap();
    let (project, conn) = create(dir.path());
    drop(conn);
    let mut app = open(&project);
    let mut screen = common::Screen::new(1200, 800, 1.0);
    screen.frame(&mut app);

    plain(&mut app, 'q', "KeyQ");
    screen.frame(&mut app); // the picker draws
    plain(&mut app, '2', "Digit2");
    assert_eq!(app.filter.queue, Some(Queue::Pictures));
    assert_eq!(app.keys, ["book-q_p0001", "book-q_p0002"]);

    // "Only the model sees" shows the model's boxes, ready to click.
    plain(&mut app, 'q', "KeyQ");
    plain(&mut app, '4', "Digit4");
    assert_eq!(app.keys, ["book-q_p0002"]);
    assert_eq!(app.current_key(), Some("book-q_p0002"));
    assert!(app.wait_for_image(Duration::from_secs(10)));
    screen.frame(&mut app);
    let n = app.doc.as_ref().unwrap().annotations.len();

    let at = app.view.image_to_screen(Point::new(100.0, 205.0));
    app.on_cursor(at.x as f32, at.y as f32);
    app.on_mouse(MouseButton::Left, true);
    app.on_mouse(MouseButton::Left, false);
    screen.frame(&mut app);
    let doc = app.doc.as_ref().unwrap();
    assert_eq!(doc.annotations.len(), n + 1, "the PP picture was adopted");
    let added = doc.annotations.last().unwrap();
    assert_eq!(added.class_id, 6);
    assert!(added.bbox.iou(&Box2::new(30.0, 150.0, 170.0, 260.0)) > 0.99);
    assert_eq!(app.editor.selected.as_deref(), Some(added.id.as_str()));
    assert!(
        added
            .provenance_json
            .as_deref()
            .unwrap()
            .contains("adopted")
    );

    // A second click on it selects it: nothing more is added.
    app.on_mouse(MouseButton::Left, true);
    app.on_mouse(MouseButton::Left, false);
    screen.frame(&mut app);
    assert_eq!(app.doc.as_ref().unwrap().annotations.len(), n + 1);

    // One undo step.
    key(
        &mut app,
        KeyInput::Character('z'),
        "KeyZ",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(app.doc.as_ref().unwrap().annotations.len(), n);

    // A drag inside a model box still draws a box of the active class.
    let a = app.view.image_to_screen(Point::new(60.0, 170.0));
    let b = app.view.image_to_screen(Point::new(120.0, 230.0));
    app.on_cursor(a.x as f32, a.y as f32);
    app.on_mouse(MouseButton::Left, true);
    app.on_cursor(b.x as f32, b.y as f32);
    app.on_mouse(MouseButton::Left, false);
    screen.frame(&mut app);
    let doc = app.doc.as_ref().unwrap();
    assert_eq!(doc.annotations.len(), n + 1);
    assert!(
        doc.annotations
            .last()
            .unwrap()
            .bbox
            .iou(&Box2::new(60.0, 170.0, 120.0, 230.0))
            > 0.95
    );
    app.shutdown();
}

#[test]
fn right_keeps_a_page_with_abandon_left_to_do_and_x_restores_reviewed() {
    let dir = tempfile::tempdir().unwrap();
    let (project, conn) = create(dir.path());
    drop(conn);
    let mut app = open(&project);
    let mut screen = common::Screen::new(1200, 800, 1.0);
    assert!(app.wait_for_image(Duration::from_secs(10)));
    screen.frame(&mut app);
    assert_eq!(app.current_key(), Some("book-q_p0000"));

    // A plain page: Right accepts it.
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    assert_eq!(app.current_key(), Some("book-q_p0001"));
    // Back on it, X excludes and X again restores *reviewed*, not "opened".
    key(&mut app, KeyInput::Left, "ArrowLeft", Modifiers::default());
    assert!(app.wait_for_image(Duration::from_secs(10)));
    plain(&mut app, 'x', "KeyX");
    assert_eq!(app.doc.as_ref().unwrap().status, ReviewStatus::Excluded);
    plain(&mut app, 'x', "KeyX");
    assert_eq!(app.doc.as_ref().unwrap().status, ReviewStatus::Reviewed);
    // N on a page never reviewed restores "opened".
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    plain(&mut app, 'n', "KeyN");
    plain(&mut app, 'n', "KeyN");
    assert_eq!(app.doc.as_ref().unwrap().status, ReviewStatus::Opened);

    // The page with an abandon box: the first Right stays and says why.
    let pos = app.keys.iter().position(|k| k == "book-q_p0004").unwrap();
    app.navigate(pos, false);
    assert!(app.wait_for_image(Duration::from_secs(10)));
    assert!(
        app.doc
            .as_ref()
            .unwrap()
            .annotations
            .iter()
            .any(|a| a.class_id == ABANDON)
    );
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    assert_eq!(app.current_key(), Some("book-q_p0004"), "Right refused");
    assert_eq!(app.doc.as_ref().unwrap().status, ReviewStatus::Opened);
    // Going back and coming again, Right still refuses once more.
    key(&mut app, KeyInput::Left, "ArrowLeft", Modifiers::default());
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    assert!(app.wait_for_image(Duration::from_secs(10)));
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    assert_eq!(app.current_key(), Some("book-q_p0004"));
    // Labelling the box (A selects it, 1 makes it text) lets Right accept.
    plain(&mut app, 'a', "KeyA");
    plain(&mut app, '1', "Digit1");
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    app.flush();
    assert_eq!(status(&project, "book-q_p0004"), ReviewStatus::Reviewed);
    assert_eq!(status(&project, "book-q_p0000"), ReviewStatus::Reviewed);
    app.shutdown();
}

#[test]
fn a_second_right_moves_on_without_accepting() {
    let dir = tempfile::tempdir().unwrap();
    let (project, conn) = create(dir.path());
    drop(conn);
    let mut app = open(&project);
    let pos = app.keys.iter().position(|k| k == "book-q_p0004").unwrap();
    app.navigate(pos - 1, false);
    app.navigate(pos, false);
    assert!(app.wait_for_image(Duration::from_secs(10)));
    // The last page: a refused Right stays, a second one does not accept.
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    app.flush();
    assert_eq!(status(&project, "book-q_p0004"), ReviewStatus::Opened);
    app.shutdown();
}

#[test]
fn new_books_get_whole_book_splits_by_a_stable_hash() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut conn) = create(dir.path());
    // Stable across runs and machines.
    assert_eq!(book_fraction("book-q"), book_fraction("book-q"));
    assert!((0.0..1.0).contains(&book_fraction("x")));
    let spread: Vec<f64> = (0..1000)
        .map(|i| book_fraction(&format!("book-{i}")))
        .collect();
    let dev = spread.iter().filter(|f| **f < 0.1).count();
    assert!((60..140).contains(&dev), "about a tenth: {dev}");

    assert_eq!(book_stats(&conn).unwrap()[0].split, "new");
    let dry = assign_splits(&mut conn, &SplitPlan::default(), true).unwrap();
    assert_eq!(dry.len(), 1);
    assert_eq!(
        book_stats(&conn).unwrap()[0].split,
        "new",
        "dry run writes nothing"
    );
    let done = assign_splits(&mut conn, &SplitPlan::default(), false).unwrap();
    assert_eq!(dry, done);
    let split = &book_stats(&conn).unwrap()[0].split;
    assert_eq!(split, &done[0].1);
    assert!(
        assign_splits(&mut conn, &SplitPlan::default(), false)
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        set_book_split(&conn, "book-q", "dev").unwrap(),
        if split == "dev" { 0 } else { 5 }
    );
    assert_eq!(book_stats(&conn).unwrap()[0].split, "dev");
}

#[test]
fn pace_counts_work_not_breaks() {
    let mut p = Pace::default();
    let t = Instant::now();
    p.accepted(t);
    assert_eq!(p.per_hour(), None);
    for i in 1..=3 {
        p.accepted(t + Duration::from_secs(30 * i));
    }
    // A long break, then two more pages 30 s apart.
    p.accepted(t + Duration::from_secs(3600));
    p.accepted(t + Duration::from_secs(3630));
    // Four 30 s steps: 120 pages an hour.
    assert_eq!(p.per_hour().map(|r| r.round()), Some(120.0));
}
