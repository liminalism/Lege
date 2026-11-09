//! End-to-end interaction through the real `PixelApp` callbacks and render
//! path: drawing, resizing, moving, cancel, undo, capture, zoom during a
//! drag, DPI-scaled hit bands, rapid press/release within one frame.

mod common;

use common::{Screen, fixture, open_app};
use liminal_annotator::annotation::geometry::{Box2, Point};
use liminal_annotator::canvas::shortcuts::Action;
use liminalkit_shell::{KeyInput, Modifiers, MouseButton};
use liminalkit_windowing::{GesturePhase, KeyEvent, PixelApp, ScrollEvent};

fn key(app: &mut liminal_annotator::app::AnnotatorApp, k: KeyInput, physical: &str, m: Modifiers) {
    app.on_key_event(&KeyEvent {
        key: k,
        text: String::new(),
        physical: physical.into(),
        pressed: true,
        repeat: false,
        modifiers: m,
    });
}

fn screen_of(app: &liminal_annotator::app::AnnotatorApp, x: f64, y: f64) -> (f32, f32) {
    let p = app.view.image_to_screen(Point::new(x, y));
    (p.x as f32, p.y as f32)
}

fn boxes(app: &liminal_annotator::app::AnnotatorApp) -> Vec<Box2> {
    app.doc
        .as_ref()
        .unwrap()
        .annotations
        .iter()
        .map(|a| a.bbox)
        .collect()
}

#[test]
fn draw_with_press_and_release_inside_one_frame() {
    let f = fixture(3);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let n0 = boxes(&app).len();
    // Right margin of the page is blank (boxes end at x = 780).
    let (ax, ay) = screen_of(&app, 805.0, 100.0);
    let (bx, by) = screen_of(&app, 840.0, 300.0);
    app.on_cursor(ax, ay);
    app.on_mouse(MouseButton::Left, true);
    app.on_cursor(bx, by);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app); // all four transitions consumed in one frame
    let b = boxes(&app);
    assert_eq!(b.len(), n0 + 1, "box committed on release");
    let nb = b.last().unwrap();
    let tol = 1.0 / app.view.scale;
    assert!(
        (nb.x0 - 805.0).abs() <= tol && (nb.y1 - 300.0).abs() <= tol,
        "{nb:?}"
    );
    // New box is selected and has the active class (1 = text).
    let doc = app.doc.as_ref().unwrap();
    assert_eq!(
        app.editor.selected.as_deref(),
        Some(doc.annotations.last().unwrap().id.as_str())
    );
    assert_eq!(doc.annotations.last().unwrap().class_id, 0);
    // One gesture = one undo entry.
    key(
        &mut app,
        KeyInput::Character('z'),
        "KeyZ",
        Modifiers {
            control: true,
            ..Default::default()
        },
    );
    assert_eq!(boxes(&app).len(), n0);
    key(
        &mut app,
        KeyInput::Character('y'),
        "KeyY",
        Modifiers {
            control: true,
            ..Default::default()
        },
    );
    assert_eq!(boxes(&app).len(), n0 + 1);
}

#[test]
fn a_press_and_release_without_movement_does_not_create_or_change_boxes() {
    let f = fixture(1);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let before = boxes(&app);
    let (x, y) = screen_of(&app, 400.0, 150.0); // inside box 0
    app.on_cursor(x, y);
    app.on_mouse(MouseButton::Left, true);
    app.on_mouse(MouseButton::Left, false);
    let (bx, by) = screen_of(&app, 815.0, 220.0); // blank
    app.on_cursor(bx, by);
    app.on_mouse(MouseButton::Left, true);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    assert_eq!(boxes(&app), before);
    assert!(!app.doc.as_ref().unwrap().history.can_undo());
    assert_eq!(
        app.editor.selected, None,
        "blank click clears the selection"
    );
}

#[test]
fn edge_drag_resizes_the_matching_corner_and_escape_cancels() {
    let f = fixture(1);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    // Box 0: x 70..780, y 90..210. Press on the right edge, lower half.
    let (x, y) = screen_of(&app, 780.0, 180.0);
    app.on_cursor(x + 3.0, y);
    app.on_mouse(MouseButton::Left, true);
    s.frame(&mut app);
    app.on_cursor(x + 30.0, y + 40.0);
    s.frame(&mut app);
    // Escape cancels: geometry untouched, no undo entry.
    key(&mut app, KeyInput::Escape, "Escape", Modifiers::default());
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    assert_eq!(boxes(&app)[0], Box2::new(70.0, 90.0, 780.0, 210.0));
    assert!(!app.doc.as_ref().unwrap().history.can_undo());

    // Now for real: bottom-right moves, top-left stays.
    app.on_cursor(x + 3.0, y);
    app.on_mouse(MouseButton::Left, true);
    app.on_cursor(x + 3.0 + 20.0, y + 30.0);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    let b = boxes(&app)[0];
    let k = 1.0 / app.view.scale;
    assert_eq!((b.x0, b.y0), (70.0, 90.0), "opposite corner fixed");
    assert!(
        (b.x1 - (780.0 + 20.0 * k)).abs() < 1e-6 && (b.y1 - (210.0 + 30.0 * k)).abs() < 1e-6,
        "{b:?}"
    );
    assert_eq!(app.doc.as_ref().unwrap().history.len(), 1);
}

#[test]
fn capture_keeps_the_drag_when_the_pointer_crosses_the_toolbar() {
    let f = fixture(1);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let (x, y) = screen_of(&app, 400.0, 150.0); // inside box 0: move
    app.on_cursor(x, y);
    app.on_mouse(MouseButton::Left, true);
    s.frame(&mut app);
    app.on_cursor(x, 5.0); // over the top bar
    s.frame(&mut app);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    let b = boxes(&app)[0];
    assert_eq!(b.y0, 0.0, "moved up and clamped to the page: {b:?}");
    assert!((b.height() - 120.0).abs() < 1e-9);
    assert_eq!(app.editor.active_class, 0, "no toolbar chip was triggered");
}

#[test]
fn zooming_during_a_drag_keeps_the_grabbed_corner_under_the_pointer() {
    let f = fixture(1);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let (x, y) = screen_of(&app, 780.0, 205.0); // bottom edge, right half
    app.on_cursor(x - 10.0, y + 2.0);
    app.on_mouse(MouseButton::Left, true);
    s.frame(&mut app);
    let press_img = app
        .view
        .screen_to_image(Point::new((x - 10.0) as f64, (y + 2.0) as f64));
    let scale_before = app.view.scale;
    app.on_cursor(x - 4.0, y + 12.0);
    s.frame(&mut app);
    app.on_scroll_event(&ScrollEvent {
        pixel_dx: 0.0,
        pixel_dy: 48.0,
        line_dx: Some(0.0),
        line_dy: Some(2.0),
        phase: GesturePhase::Moved,
    });
    s.frame(&mut app);
    assert!(app.view.scale > scale_before * 1.4, "zoomed in");
    let (px, py) = (x - 2.0, y + 15.0);
    app.on_cursor(px, py);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    // corner = original + (pointer in image space now - press in image space then)
    let b = boxes(&app)[0];
    let now = app.view.screen_to_image(Point::new(px as f64, py as f64));
    let want = (780.0 + now.x - press_img.x, 210.0 + now.y - press_img.y);
    assert!(
        (b.x1 - want.0).abs() < 1e-6 && (b.y1 - want.1).abs() < 1e-6,
        "{b:?} vs {want:?}"
    );
    assert_eq!((b.x0, b.y0), (70.0, 90.0));
    assert_eq!(
        app.doc.as_ref().unwrap().history.len(),
        1,
        "one undo entry for the gesture"
    );
}

#[test]
fn hit_band_scales_with_dpi() {
    let f = fixture(1);
    // 1x: 12 physical px outside the right edge is outside the 8 px band → draws.
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let (x, y) = screen_of(&app, 780.0, 150.0);
    app.on_cursor(x + 12.0, y);
    app.on_mouse(MouseButton::Left, true);
    s.frame(&mut app);
    assert!(matches!(
        app.editor.state,
        liminal_annotator::canvas::interaction::ToolState::Drawing { .. }
    ));
    app.run_action(Action::Cancel);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    drop(app);

    // 2x DPI: the band is 16 physical px, so the same offset grabs the edge.
    let mut app = open_app(&f);
    let mut s = Screen::new(2560, 1800, 2.0);
    s.frame(&mut app);
    let (x, y) = screen_of(&app, 780.0, 150.0);
    app.on_cursor(x + 12.0, y);
    app.on_mouse(MouseButton::Left, true);
    s.frame(&mut app);
    assert!(
        matches!(
            app.editor.state,
            liminal_annotator::canvas::interaction::ToolState::Editing { .. }
        ),
        "{:?}",
        app.editor.state
    );
}

#[test]
fn overlapping_boxes_prefer_the_closest_edge_then_the_inner_box() {
    use liminal_annotator::annotation::commands::{Annotation, Edit, new_annotation_id};
    let f = fixture(1);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    // A small box nested inside box 0.
    let inner = Annotation {
        id: new_annotation_id(),
        class_id: 7,
        bbox: Box2::new(300.0, 120.0, 400.0, 180.0),
        is_ignore: false,
        provenance_json: None,
    };
    {
        let doc = app.doc.as_mut().unwrap();
        let n = doc.annotations.len();
        doc.apply(Edit::Add {
            index: n,
            ann: inner.clone(),
        });
    }
    // Interior of the inner box selects the inner box, not box 0.
    let (x, y) = screen_of(&app, 350.0, 150.0);
    app.on_cursor(x, y);
    app.on_mouse(MouseButton::Left, true);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    assert_eq!(app.editor.selected.as_deref(), Some(inner.id.as_str()));
    // Number key reclassifies the selection (one edit) and sets the active class.
    key(
        &mut app,
        KeyInput::Character('2'),
        "Digit2",
        Modifiers::default(),
    );
    let doc = app.doc.as_ref().unwrap();
    assert_eq!(doc.get(&inner.id).unwrap().class_id, 6);
    assert_eq!(app.editor.active_class, 6);
    // Delete removes it; undo restores it.
    key(&mut app, KeyInput::Delete, "Delete", Modifiers::default());
    assert!(app.doc.as_ref().unwrap().get(&inner.id).is_none());
    key(
        &mut app,
        KeyInput::Character('z'),
        "KeyZ",
        Modifiers {
            control: true,
            ..Default::default()
        },
    );
    assert!(app.doc.as_ref().unwrap().get(&inner.id).is_some());
}

#[test]
fn right_arrow_marks_reviewed_and_left_does_not() {
    use liminal_annotator::annotation::commands::ReviewStatus;
    let f = fixture(3);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let first = app.current_key().unwrap().to_owned();
    key(
        &mut app,
        KeyInput::Right,
        "ArrowRight",
        Modifiers::default(),
    );
    assert_eq!(app.pos, 1);
    app.wait_for_image(std::time::Duration::from_secs(5));
    key(&mut app, KeyInput::Left, "ArrowLeft", Modifiers::default());
    assert_eq!(app.pos, 0);
    assert_eq!(app.current_key(), Some(first.as_str()));
    assert_eq!(app.doc.as_ref().unwrap().status, ReviewStatus::Reviewed);
    app.flush();
    let conn = liminal_annotator::dataset::database::open(&f.project).unwrap();
    let st = |k: &str| {
        liminal_annotator::dataset::database::page_meta(&conn, k)
            .unwrap()
            .unwrap()
            .status
    };
    assert_eq!(st(&first), ReviewStatus::Reviewed);
    let second = app.keys[1].clone();
    assert_eq!(
        st(&second),
        ReviewStatus::Opened,
        "Left leaves the page it left unreviewed"
    );
}

#[test]
fn idle_cursor_motion_skips_drawing_but_hover_changes_redraw() {
    let f = fixture(1);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    // Settle: the writer's acknowledgement of the page-open commit changes
    // the save indicator, which is a legitimate redraw.
    app.flush();
    s.frame(&mut app);
    let before = s.buf.pixels.clone();
    let skipped0 = app.perf.skipped;
    for i in 0..10 {
        app.on_cursor(10.0 + i as f32, 500.0); // blank canvas left of the page
        s.frame(&mut app);
    }
    assert_eq!(app.perf.skipped - skipped0, 10, "nothing visible changed");
    assert!(
        s.buf.pixels == before,
        "skipped frames leave the buffer as it was"
    );
    // Hovering a box edge changes the hover marker: drawn.
    let (x, y) = screen_of(&app, 780.0, 150.0);
    app.on_cursor(x + 2.0, y);
    s.frame(&mut app);
    assert_eq!(app.perf.skipped - skipped0, 10);
    assert!(s.buf.pixels != before, "hover feedback drawn");
}

#[test]
fn shift_drag_draws_inside_an_existing_box() {
    let f = fixture(1);
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let n0 = boxes(&app).len();
    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    app.on_modifiers(shift);
    let (ax, ay) = screen_of(&app, 200.0, 110.0); // inside box 0 (70..780 × 90..210)
    let (bx, by) = screen_of(&app, 400.0, 190.0);
    app.on_cursor(ax, ay);
    app.on_mouse(MouseButton::Left, true);
    app.on_cursor(bx, by);
    app.on_mouse(MouseButton::Left, false);
    app.on_modifiers(Modifiers::default());
    s.frame(&mut app);
    let b = boxes(&app);
    assert_eq!(b.len(), n0 + 1, "a new box, not a move of box 0");
    assert_eq!(b[0], Box2::new(70.0, 90.0, 780.0, 210.0));
}

#[test]
fn ctrl_c_reverts_the_page_then_to_imported_and_is_undoable() {
    use liminal_annotator::annotation::commands::Edit;
    let f = fixture(2);
    // Pretend an earlier session edited the page: the imported layer differs.
    {
        let conn = liminal_annotator::dataset::database::open(&f.project).unwrap();
        conn.execute("UPDATE annotations SET x0 = 60.0 WHERE ord = 0", [])
            .unwrap();
    }
    let mut app = open_app(&f);
    let mut s = Screen::new(1280, 900, 1.0);
    s.frame(&mut app);
    let opened = boxes(&app);
    assert_eq!(opened[0].x0, 60.0);
    // Resize box 0 by dragging its right edge, and delete box 1.
    let (x, y) = screen_of(&app, 780.0, 180.0);
    app.on_cursor(x + 2.0, y);
    app.on_mouse(MouseButton::Left, true);
    app.on_cursor(x + 40.0, y + 20.0);
    app.on_mouse(MouseButton::Left, false);
    s.frame(&mut app);
    {
        let doc = app.doc.as_mut().unwrap();
        let ann = doc.annotations[1].clone();
        doc.apply(Edit::Remove { index: 1, ann });
    }
    assert_ne!(boxes(&app), opened);
    let ctrl = Modifiers {
        control: true,
        ..Default::default()
    };
    // 1st Ctrl+C: back to how the page was opened (keeps the earlier session's x0 = 60).
    key(&mut app, KeyInput::Character('c'), "KeyC", ctrl);
    assert_eq!(boxes(&app), opened);
    // 2nd Ctrl+C: back to the imported boxes.
    key(&mut app, KeyInput::Character('c'), "KeyC", ctrl);
    assert_eq!(boxes(&app)[0].x0, 70.0);
    assert_eq!(boxes(&app).len(), 8);
    // Each revert is one undo step.
    key(&mut app, KeyInput::Character('z'), "KeyZ", ctrl);
    assert_eq!(boxes(&app), opened);
    key(&mut app, KeyInput::Character('z'), "KeyZ", ctrl);
    assert_eq!(boxes(&app).len(), 7, "the edits are back");
    // The bottom-bar button does the same as Ctrl+C, and the result is saved.
    s.frame(&mut app);
    let button_at = (900.0f32..1280.0).step_by_f(2.0).find(|&bx| {
        app.on_cursor(bx, 890.0);
        app.on_mouse(MouseButton::Left, true);
        app.on_mouse(MouseButton::Left, false);
        s.frame(&mut app);
        boxes(&app) == opened
    });
    assert!(button_at.is_some(), "revert button found in the bottom bar");
    app.flush();
    let conn = liminal_annotator::dataset::database::open(&f.project).unwrap();
    let key0 = app.current_key().unwrap().to_owned();
    let saved = liminal_annotator::dataset::database::annotations(&conn, &key0).unwrap();
    assert_eq!(saved.iter().map(|a| a.bbox).collect::<Vec<_>>(), opened);
}

trait StepF {
    fn step_by_f(self, step: f32) -> Box<dyn Iterator<Item = f32>>;
}
impl StepF for std::ops::Range<f32> {
    fn step_by_f(self, step: f32) -> Box<dyn Iterator<Item = f32>> {
        let n = ((self.end - self.start) / step) as usize;
        Box::new((0..n).map(move |i| self.start + i as f32 * step))
    }
}
