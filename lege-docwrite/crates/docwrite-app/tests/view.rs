//! The resizable Book Map, its tab, and the full-screen writing mode.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_app::{Editor, PAPER, PointerShape, WORDPERFECT};

fn painted(editor: &mut Editor) -> pixelkit_raster::WindowBuffer {
    let mut frame = pixelkit_raster::WindowBuffer::new(1100, 800);
    editor.paint(&mut frame);
    frame
}

fn drag(editor: &mut Editor, from: f32, to: f32) {
    editor.hover(from, 300.0);
    editor.pointer(true);
    editor.hover(to, 300.0);
    editor.pointer(false);
}

#[test]
fn the_book_map_resizes_folds_into_a_tab_and_comes_back() {
    let mut editor = Editor::new();
    painted(&mut editor);
    let start = editor.sidebar_logical_width();
    editor.hover(start, 300.0);
    assert_eq!(editor.pointer_shape(), PointerShape::ResizeColumn);

    drag(&mut editor, start, 360.0);
    assert_eq!(editor.sidebar_logical_width(), 360.0, "dragged wider");
    let frame = painted(&mut editor);
    let row = frame.pixels[(40 * 1100 + 340) as usize];
    assert_ne!(row, PAPER.desk, "the map paints out to its new edge");

    drag(&mut editor, 360.0, 30.0);
    assert!(editor.sidebar_collapsed(), "dropped narrow, it folds away");
    assert_eq!(editor.sidebar_logical_width(), 0.0);
    let frame = painted(&mut editor);
    let page_edge = frame.pixels[(400 * 1100 + 60) as usize];
    assert_eq!(page_edge, PAPER.desk, "the pages take the width back");

    // The tab brings it back at the width it had.
    editor.hover(8.0, 50.0);
    assert_eq!(editor.pointer_shape(), PointerShape::Hand);
    editor.pointer(true);
    editor.pointer(false);
    assert!(!editor.sidebar_collapsed());
    assert_eq!(editor.sidebar_logical_width(), 360.0);

    editor.toggle_sidebar();
    assert!(editor.sidebar_collapsed());
    editor.toggle_sidebar();
    assert_eq!(editor.sidebar_logical_width(), 360.0);
}

#[test]
fn full_screen_is_edge_to_edge_wordperfect_with_a_status_line() {
    let mut editor = Editor::new();
    editor.type_text(" More words for the page.");
    assert_eq!(editor.poll_fullscreen(), None);
    editor.toggle_fullscreen();
    assert!(editor.is_fullscreen());
    assert_eq!(
        editor.poll_fullscreen(),
        Some(true),
        "the window is asked once"
    );
    assert_eq!(editor.poll_fullscreen(), None);

    let frame = painted(&mut editor);
    assert_eq!(editor.theme(), WORDPERFECT);
    assert!(
        frame
            .pixels
            .iter()
            .all(|pixel| *pixel != PAPER.desk && *pixel != PAPER.page),
        "no desk, no paper, no Book Map"
    );
    let ink = frame
        .pixels
        .iter()
        .filter(|pixel| **pixel != WORDPERFECT.desk && **pixel != WORDPERFECT.rule)
        .count();
    assert!(ink > 500, "text is drawn: {ink}");
    // The page spans the whole width: text starts inside the left margin
    // band scaled up, far from the old centered column.
    assert!(editor.caret_area().is_some());
    // The status line's text sits in the bottom band.
    let bottom_ink = frame.pixels[(780 * 1100) as usize..]
        .iter()
        .filter(|pixel| **pixel != WORDPERFECT.desk)
        .count();
    assert!(bottom_ink > 50, "status line text: {bottom_ink}");

    editor.set_fullscreen(false);
    assert_eq!(editor.poll_fullscreen(), Some(false));
    painted(&mut editor);
    assert_eq!(editor.theme(), PAPER);
}
