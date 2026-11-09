//! Durability: every acknowledged edit survives an abrupt process
//! termination; navigation never waits for or loses a save; the writer
//! coalesces and acknowledges in order.

mod common;

use std::process::Command;
use std::time::Duration;

use common::{Screen, fixture, open_app};
use liminal_annotator::annotation::commands::{Edit, ReviewStatus};
use liminal_annotator::dataset::database::{self, PageSave, WriteAck, Writer};
use liminalkit_shell::{KeyInput, Modifiers, MouseButton};
use liminalkit_windowing::{KeyEvent, PixelApp};

fn right(app: &mut liminal_annotator::app::AnnotatorApp) {
    app.on_key_event(&KeyEvent {
        key: KeyInput::Right,
        text: String::new(),
        physical: "ArrowRight".into(),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    });
}

#[test]
fn acknowledged_edits_survive_abort() {
    let f = fixture(12);
    let exe = env!("CARGO_BIN_EXE_liminal-annotator");
    let out = Command::new(exe)
        .args([
            "__crash-test",
            f.project.to_str().unwrap(),
            "--edits",
            "10",
            "--wait",
            "1",
        ])
        .output()
        .expect("run crash test");
    assert!(!out.status.success(), "the child aborts");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("acked"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let conn = database::open(&f.project).unwrap();
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let keys = database::page_keys(&conn, &Default::default()).unwrap();
    for (i, k) in keys.iter().take(10).enumerate() {
        let a = database::annotations(&conn, k).unwrap();
        assert_eq!(a[0].bbox.x0, 71.0, "page {i} {k}: edit recovered");
        let m = database::page_meta(&conn, k).unwrap().unwrap();
        let want = if i < 9 {
            ReviewStatus::Reviewed
        } else {
            ReviewStatus::Opened
        };
        assert_eq!(m.status, want, "page {i}");
    }
    let untouched = database::annotations(&conn, &keys[10]).unwrap();
    assert_eq!(untouched[0].bbox.x0, 70.0);
}

#[test]
fn abort_without_waiting_leaves_a_consistent_database() {
    let f = fixture(30);
    let exe = env!("CARGO_BIN_EXE_liminal-annotator");
    let _ = Command::new(exe)
        .args([
            "__crash-test",
            f.project.to_str().unwrap(),
            "--edits",
            "25",
            "--wait",
            "0",
        ])
        .output()
        .expect("run crash test");
    let conn = database::open(&f.project).unwrap();
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    // Every page is either fully old or fully new: a page transaction is atomic.
    for k in database::page_keys(&conn, &Default::default()).unwrap() {
        let a = database::annotations(&conn, &k).unwrap();
        assert_eq!(a.len(), 8, "{k}: no partial page");
        assert!(a[0].bbox.x0 == 70.0 || a[0].bbox.x0 == 71.0);
        assert!(a[1..].iter().all(|b| b.bbox.x0 == 70.0));
    }
}

#[test]
fn navigation_while_saving_keeps_the_edit() {
    let f = fixture(4);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    // Draw a box and immediately navigate away and back, before any ack.
    let a = app
        .view
        .image_to_screen(liminal_annotator::annotation::geometry::Point::new(
            805.0, 100.0,
        ));
    let b = app
        .view
        .image_to_screen(liminal_annotator::annotation::geometry::Point::new(
            840.0, 300.0,
        ));
    app.on_cursor(a.x as f32, a.y as f32);
    app.on_mouse(MouseButton::Left, true);
    app.on_cursor(b.x as f32, b.y as f32);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    let first = app.current_key().unwrap().to_owned();
    right(&mut app);
    app.navigate(0, false);
    assert_eq!(app.current_key(), Some(first.as_str()));
    assert_eq!(
        app.doc.as_ref().unwrap().annotations.len(),
        9,
        "edit visible after returning"
    );
    // Undo history survives the round trip too.
    assert!(app.doc.as_ref().unwrap().history.can_undo());
    app.flush();
    assert_eq!(app.unsaved(), 0);
    let conn = database::open(&f.project).unwrap();
    assert_eq!(database::annotations(&conn, &first).unwrap().len(), 9);
    // Reopen at that page (by default the app opens the first unreviewed page).
    drop(app);
    let app = liminal_annotator::app::AnnotatorApp::new(liminal_annotator::app::AppConfig {
        project: f.project.clone(),
        filter: Default::default(),
        start_at: Some(first.clone()),
        prediction_threshold: 0.3,
    })
    .unwrap();
    assert_eq!(app.current_key(), Some(first.as_str()));
    assert_eq!(app.doc.as_ref().unwrap().annotations.len(), 9);
}

#[test]
fn writer_coalesces_and_acknowledges_the_newest_revision() {
    let f = fixture(2);
    let conn = database::open(&f.project).unwrap();
    let key = database::page_keys(&conn, &Default::default()).unwrap()[0].clone();
    let base = database::annotations(&conn, &key).unwrap();
    let mut w = Writer::spawn(&f.project, || {}).unwrap();
    for rev in 1..=50u64 {
        let mut anns = base.clone();
        anns[0].bbox.x0 = 70.0 - rev as f64 * 0.5;
        w.save(PageSave {
            page_key: key.clone(),
            status: ReviewStatus::Opened,
            annotations: anns,
            revision: rev,
            kind: "t",
        });
    }
    w.flush();
    let acks: Vec<WriteAck> = w.acks.try_iter().collect();
    assert!(!acks.is_empty() && acks.len() <= 50);
    let mut last = 0;
    for a in &acks {
        match a {
            WriteAck::Saved { revision, .. } => {
                assert!(*revision > last, "in order");
                last = *revision;
            }
            WriteAck::Failed { error, .. } => panic!("{error}"),
        }
    }
    assert_eq!(last, 50);
    w.shutdown();
    let a = database::annotations(&conn, &key).unwrap();
    assert_eq!(a[0].bbox.x0, 70.0 - 25.0);
}

#[test]
fn a_failed_image_is_never_marked_reviewed() {
    let f = fixture(3);
    let conn = database::open(&f.project).unwrap();
    let keys = database::page_keys(&conn, &Default::default()).unwrap();
    conn.execute(
        "UPDATE pages SET source_path = 'Z:/does/not/exist.jpg' WHERE page_key = ?1",
        [&keys[0]],
    )
    .unwrap();
    drop(conn);
    let mut app = liminal_annotator::app::AnnotatorApp::new(liminal_annotator::app::AppConfig {
        project: f.project.clone(),
        filter: Default::default(),
        start_at: None,
        prediction_threshold: 0.3,
    })
    .unwrap();
    assert!(!app.wait_for_image(Duration::from_secs(5)), "load fails");
    right(&mut app);
    app.flush();
    let conn = database::open(&f.project).unwrap();
    let m = database::page_meta(&conn, &keys[0]).unwrap().unwrap();
    assert_ne!(m.status, ReviewStatus::Reviewed);
    assert_eq!(app.pos, 1, "navigation still advances");
}

#[test]
fn status_toggles_are_undoable_edits() {
    let f = fixture(1);
    let mut app = open_app(&f);
    let doc = app.doc.as_mut().unwrap();
    let before = doc.status;
    doc.apply(Edit::Status {
        before,
        after: ReviewStatus::Excluded,
    });
    assert_eq!(doc.status, ReviewStatus::Excluded);
    doc.undo();
    assert_eq!(doc.status, before);
}
