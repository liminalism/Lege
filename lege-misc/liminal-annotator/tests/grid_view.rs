//! The large-picture filter and queue (`fig_area`, `--figure-min`), and the
//! six-page view: which pages its Right accepts, and how a click or a digit
//! maps to a page and back.

mod common;

use std::path::Path;
use std::time::Duration;

use liminal_annotator::annotation::commands::{Annotation, ReviewStatus};
use liminal_annotator::annotation::geometry::Box2;
use liminal_annotator::annotation::labels::{ABANDON, FIGURE};
use liminal_annotator::app::AnnotatorApp;
use liminal_annotator::dataset::database::{self, PageFilter, PageSave, Queue, queue_counts};
use liminal_annotator::grid::{self, PER_VIEW};
use liminalkit_shell::{KeyInput, Modifiers, MouseButton, Scale};
use liminalkit_windowing::{KeyEvent, PixelApp};
use rusqlite::{Connection, params};

const W: u32 = 200;
const H: u32 = 300;

fn ann(id: &str, class_id: u8, b: (f64, f64, f64, f64)) -> Annotation {
    Annotation {
        id: id.into(),
        class_id,
        bbox: Box2::new(b.0, b.1, b.2, b.3),
        is_ignore: false,
        provenance_json: None,
    }
}

fn save(conn: &mut Connection, key: &str, status: ReviewStatus, anns: Vec<Annotation>) {
    database::save_page(
        conn,
        &PageSave {
            page_key: key.into(),
            status,
            annotations: anns,
            revision: 1,
            kind: "edit",
        },
    )
    .unwrap();
}

/// Six 200×300 pages: the page is 60,000 px², so 40% is 24,000.
fn figure_project(path: &Path) -> Connection {
    let mut conn = database::open(path).unwrap();
    for i in 0..6 {
        conn.execute(
            "INSERT INTO pages (page_key, seq, book_id, split, set_name, source_path, file_name, width, height) \
             VALUES (?1, ?2, 'b', 'train', 's', 'x.jpg', 'x.jpg', ?3, ?4)",
            params![format!("p{i}"), i, W, H],
        )
        .unwrap();
    }
    let todo = ReviewStatus::Opened;
    // Exactly 40%: 120 × 200.
    save(
        &mut conn,
        "p0",
        todo,
        vec![ann("a", FIGURE, (40.0, 50.0, 160.0, 250.0))],
    );
    // Just under: 119 × 200; a large text box does not count.
    save(
        &mut conn,
        "p1",
        todo,
        vec![
            ann("b", FIGURE, (40.0, 50.0, 159.0, 250.0)),
            ann("c", 0, (0.0, 0.0, 200.0, 300.0)),
        ],
    );
    // A small figure and a large one: the largest decides.
    save(
        &mut conn,
        "p2",
        todo,
        vec![
            ann("d", FIGURE, (10.0, 10.0, 30.0, 30.0)),
            ann("e", FIGURE, (0.0, 0.0, 200.0, 290.0)),
        ],
    );
    // The whole page.
    save(
        &mut conn,
        "p3",
        todo,
        vec![ann("f", FIGURE, (0.0, 0.0, 200.0, 300.0))],
    );
    // No boxes at all.
    save(&mut conn, "p4", todo, vec![]);
    // Large, but already reviewed.
    save(
        &mut conn,
        "p5",
        ReviewStatus::Reviewed,
        vec![ann("g", FIGURE, (0.0, 0.0, 200.0, 250.0))],
    );
    conn
}

fn keys(conn: &Connection, f: &PageFilter) -> Vec<String> {
    database::page_keys(conn, f).unwrap()
}

fn large(conn: &Connection, share: f64) -> (Vec<String>, i64) {
    let f = PageFilter {
        queue: Some(Queue::LargePictures),
        large_figure_share: Some(share),
        ..PageFilter::default()
    };
    let i = Queue::ALL
        .iter()
        .position(|q| *q == Queue::LargePictures)
        .unwrap();
    let scope = PageFilter {
        large_figure_share: Some(share),
        ..PageFilter::default()
    };
    (keys(conn, &f), queue_counts(conn, &scope).unwrap()[i])
}

#[test]
fn large_figure_filter_takes_pages_at_the_threshold_and_leaves_those_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.db");
    let mut conn = figure_project(&path);

    let at = |share: f64| PageFilter {
        figure_min: Some(share),
        ..PageFilter::default()
    };
    assert_eq!(keys(&conn, &at(0.4)), ["p0", "p2", "p3", "p5"]);
    assert_eq!(keys(&conn, &at(0.39)), ["p0", "p1", "p2", "p3", "p5"]);
    assert_eq!(keys(&conn, &at(1.0)), ["p3"]);
    assert_eq!(keys(&conn, &at(0.0)), ["p0", "p1", "p2", "p3", "p5"]);
    // Combined with the other options.
    let todo_only = PageFilter {
        figure_min: Some(0.4),
        statuses: vec![ReviewStatus::Unseen, ReviewStatus::Opened],
        ..PageFilter::default()
    };
    assert_eq!(keys(&conn, &todo_only), ["p0", "p2", "p3"]);

    // The queue is the same rule on pages to do, and its count agrees.
    assert_eq!(
        large(&conn, 0.4),
        (vec!["p0".into(), "p2".into(), "p3".into()], 3)
    );
    assert_eq!(large(&conn, 0.9), (vec!["p2".into(), "p3".into()], 2));
    assert_eq!(
        large(&conn, database::DEFAULT_LARGE_FIGURE_SHARE).0,
        ["p0", "p2", "p3"]
    );
    assert!(at(0.4).describe().contains("figure 40%+"));

    // Saving the page keeps the area: a figure cut down leaves the queue,
    // one drawn bigger joins it.
    save(
        &mut conn,
        "p0",
        ReviewStatus::Opened,
        vec![ann("a", FIGURE, (40.0, 50.0, 160.0, 150.0))],
    );
    save(
        &mut conn,
        "p1",
        ReviewStatus::Opened,
        vec![ann("b", FIGURE, (0.0, 0.0, 200.0, 300.0))],
    );
    assert_eq!(large(&conn, 0.4).0, ["p1", "p2", "p3"]);

    // A project from before the column gets it on open, and a page saved by
    // an older build (count and area out of step) is measured again.
    conn.execute_batch(
        "ALTER TABLE pages DROP COLUMN fig_area; DELETE FROM meta WHERE key = 'fig_area_checked_at';",
    )
    .unwrap();
    drop(conn);
    let conn = database::open(&path).unwrap();
    assert_eq!(large(&conn, 0.4).0, ["p1", "p2", "p3"]);
    conn.execute("UPDATE pages SET fig_area = 0 WHERE page_key = 'p3'", [])
        .unwrap();
    drop(conn);
    let conn = database::open(&path).unwrap();
    assert_eq!(large(&conn, 0.4).0, ["p1", "p2", "p3"]);
}

// --- the six-page view -------------------------------------------------------

fn key(app: &mut AnnotatorApp, k: KeyInput, physical: &str, m: Modifiers) {
    app.on_key_event(&KeyEvent {
        key: k,
        text: String::new(),
        physical: physical.into(),
        pressed: true,
        repeat: false,
        modifiers: m,
    });
}

fn press(app: &mut AnnotatorApp, k: KeyInput, physical: &str) {
    key(app, k, physical, Modifiers::default());
}

fn letter(app: &mut AnnotatorApp, c: char) {
    press(
        app,
        KeyInput::Character(c),
        &format!("Key{}", c.to_ascii_uppercase()),
    );
}

fn status(project: &Path, page: &str) -> ReviewStatus {
    let conn = database::open(project).unwrap();
    database::page_meta(&conn, page).unwrap().unwrap().status
}

fn page(i: usize) -> String {
    format!("synth000_p{i:06}")
}

#[test]
fn right_in_the_view_accepts_the_pages_to_do_and_leaves_the_rest() {
    let f = common::fixture(14);
    {
        let mut conn = database::open(&f.project).unwrap();
        let anns = database::annotations(&conn, &page(2)).unwrap();
        save(&mut conn, &page(2), ReviewStatus::Reviewed, anns);
        let anns = database::annotations(&conn, &page(3)).unwrap();
        save(&mut conn, &page(3), ReviewStatus::NeedsAttention, anns);
        let mut anns = database::annotations(&conn, &page(4)).unwrap();
        anns.push(ann("left", ABANDON, (100.0, 1200.0, 300.0, 1250.0)));
        save(&mut conn, &page(4), ReviewStatus::Unseen, anns);
        // Page 1's image is gone.
        conn.execute(
            "UPDATE pages SET source_path = 'missing.jpg' WHERE page_key = ?1",
            [page(1)],
        )
        .unwrap();
    }
    let mut app = common::open_app(&f);
    let mut screen = common::Screen::new(1500, 950, 1.0);
    letter(&mut app, 'g');
    assert!(app.grid_showing());
    app.wait_for_image(Duration::from_secs(10));
    screen.frame(&mut app);
    assert_eq!(app.grid_start(), Some(0));

    // Page 6 opened from the view and left without accepting.
    press(&mut app, KeyInput::Character('6'), "Digit6");
    assert!(!app.grid_showing());
    assert_eq!(app.current_key(), Some(page(5).as_str()));
    app.wait_for_image(Duration::from_secs(10));
    screen.frame(&mut app);
    press(&mut app, KeyInput::Escape, "Escape");
    assert!(app.grid_showing(), "Escape goes back to the six");
    assert_eq!(app.grid_start(), Some(0));
    let held: Vec<bool> = app.grid_pages().iter().map(|p| p.2).collect();
    assert_eq!(held, [false, false, false, false, false, true]);

    // The first Right warns about page 5's abandon box and changes nothing.
    app.wait_for_image(Duration::from_secs(10));
    press(&mut app, KeyInput::Right, "ArrowRight");
    assert_eq!(app.grid_start(), Some(0));
    app.flush();
    assert_eq!(status(&f.project, &page(0)), ReviewStatus::Opened);

    // The second accepts what is left and shows the next six.
    press(&mut app, KeyInput::Right, "ArrowRight");
    assert_eq!(app.grid_start(), Some(PER_VIEW));
    app.flush();
    let expected = [
        ReviewStatus::Reviewed,       // opened when the app started: accepted
        ReviewStatus::Unseen,         // image did not load: never reviewed
        ReviewStatus::Reviewed,       // already reviewed
        ReviewStatus::NeedsAttention, // flagged pages keep their flag
        ReviewStatus::Unseen,         // abandon box left
        ReviewStatus::Opened,         // opened from the view, not accepted
    ];
    for (i, want) in expected.iter().enumerate() {
        assert_eq!(status(&f.project, &page(i)), *want, "page {i}");
    }
    // Nothing beyond the six was touched.
    assert_eq!(status(&f.project, &page(6)), ReviewStatus::Unseen);

    // Shift+Right moves on without accepting; Left comes back.
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(app.grid_start(), Some(12));
    press(&mut app, KeyInput::Left, "ArrowLeft");
    assert_eq!(app.grid_start(), Some(6));
    app.flush();
    assert_eq!(status(&f.project, &page(12)), ReviewStatus::Unseen);
    assert_eq!(status(&f.project, &page(6)), ReviewStatus::Unseen);

    // X marks the page under the pointer, and again gives back its status.
    app.wait_for_image(Duration::from_secs(10));
    screen.frame(&mut app);
    let c = app.grid_layout().unwrap().cells[1];
    app.on_cursor((c.x + c.w / 2) as f32, (c.y + c.h / 2) as f32);
    screen.frame(&mut app);
    letter(&mut app, 'x');
    app.flush();
    assert_eq!(status(&f.project, &page(7)), ReviewStatus::Excluded);
    // An excluded page is not accepted by Right either.
    press(&mut app, KeyInput::End, "End");
    assert_eq!(app.grid_start(), Some(12));
    press(&mut app, KeyInput::Left, "ArrowLeft");
    app.wait_for_image(Duration::from_secs(10));
    press(&mut app, KeyInput::Right, "ArrowRight");
    assert_eq!(app.grid_start(), Some(12));
    app.flush();
    assert_eq!(status(&f.project, &page(6)), ReviewStatus::Reviewed);
    assert_eq!(status(&f.project, &page(7)), ReviewStatus::Excluded);
    assert_eq!(status(&f.project, &page(8)), ReviewStatus::Reviewed);
    app.shutdown();
}

#[test]
fn a_click_opens_the_page_under_it_and_escape_returns_to_its_six() {
    // Cells by position: three across, two down, none in the margins.
    let l = grid::layout(1500, 950, Scale::ONE, 22);
    let centre = |i: usize| {
        let r = l.cells[i];
        (r.x + r.w / 2, r.y + r.h / 2)
    };
    for i in 0..PER_VIEW {
        let (x, y) = centre(i);
        assert_eq!(l.cell_at(x, y), Some(i));
        let r = l.cells[i];
        assert_eq!(l.cell_at(r.x, r.y), Some(i), "top-left pixel");
        assert_eq!(l.cell_at(r.right() - 1, r.bottom() - 1), Some(i));
    }
    assert_eq!(l.cell_at(2, 500), None, "left margin");
    assert_eq!(l.cell_at(l.cells[0].right() + 2, centre(0).1), None, "gap");
    assert_eq!(l.cell_at(750, 5), None, "header");
    assert_eq!(l.cell_at(750, 948), None, "bottom margin");
    assert_eq!(l.cell_at(centre(4).0, centre(4).1 - 1), Some(4));

    // In the app: the second group, a click on its fifth page.
    let f = common::fixture(14);
    let mut app = common::open_app(&f);
    let mut screen = common::Screen::new(1500, 950, 1.0);
    letter(&mut app, 'g');
    press(&mut app, KeyInput::PageDown, "PageDown");
    assert_eq!(
        app.grid_start(),
        Some(12),
        "sixty pages on, clamped to the last six"
    );
    press(&mut app, KeyInput::Home, "Home");
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(app.grid_start(), Some(6));
    app.wait_for_image(Duration::from_secs(10));
    screen.frame(&mut app);
    let layout = app.grid_layout().unwrap();
    assert_eq!(layout, grid::layout(1500, 950, Scale::ONE, layout.label_h));
    // A click in the gap does nothing.
    let gap = (layout.cells[0].right() + 3, layout.cells[0].y + 40);
    app.on_cursor(gap.0 as f32, gap.1 as f32);
    app.on_mouse(MouseButton::Left, true);
    app.on_mouse(MouseButton::Left, false);
    screen.frame(&mut app);
    assert!(app.grid_showing());
    let c = layout.cells[4];
    app.on_cursor((c.x + 30) as f32, (c.bottom() - 30) as f32);
    app.on_mouse(MouseButton::Left, true);
    app.on_mouse(MouseButton::Left, false);
    screen.frame(&mut app);
    assert!(!app.grid_showing());
    assert_eq!(app.current_key(), Some(page(10).as_str()));
    app.wait_for_image(Duration::from_secs(10));
    screen.frame(&mut app);

    // Escape clears a selection first; with none, it goes back.
    press(&mut app, KeyInput::Tab, "Tab");
    assert!(app.editor.selected.is_some());
    press(&mut app, KeyInput::Escape, "Escape");
    assert!(!app.grid_showing());
    assert!(app.editor.selected.is_none());
    // An edit in the editor is saved as ever, and the view shows it.
    let n = app.doc.as_ref().unwrap().annotations.len();
    press(&mut app, KeyInput::Tab, "Tab");
    press(&mut app, KeyInput::Delete, "Delete");
    assert_eq!(app.doc.as_ref().unwrap().annotations.len(), n - 1);
    press(&mut app, KeyInput::Escape, "Escape");
    assert!(app.grid_showing());
    let shown = app.grid_pages();
    assert_eq!(shown[4].0, page(10));
    assert_eq!(
        app.grid_start(),
        Some(6),
        "the six the page was opened from"
    );
    screen.frame(&mut app);
    app.flush();
    let conn = database::open(&f.project).unwrap();
    assert_eq!(
        database::annotations(&conn, &page(10)).unwrap().len(),
        n - 1
    );
    drop(conn);
    // G there opens the page pointed at; G again comes back.
    let c = app.grid_layout().unwrap().cells[2];
    app.on_cursor((c.x + c.w / 2) as f32, (c.y + c.h / 2) as f32);
    screen.frame(&mut app);
    letter(&mut app, 'g');
    assert_eq!(app.current_key(), Some(page(8).as_str()));
    letter(&mut app, 'g');
    assert!(app.grid_showing());
    assert_eq!(app.grid_start(), Some(6));
    app.shutdown();
}
