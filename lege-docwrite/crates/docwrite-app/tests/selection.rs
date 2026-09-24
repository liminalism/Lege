//! A shifted caret paints a highlight. The same motion the window's key handler calls.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use docwrite_app::{Editor, SELECTION};

#[test]
fn extending_the_caret_paints_a_highlight_and_a_collapsed_caret_does_not() {
    let mut editor = Editor::new();
    let chars = editor.book().plain_text().chars().count();
    for _ in 0..chars {
        editor.move_left();
    }
    assert!(editor.book().selection().is_collapsed());
    assert_eq!(editor.pixels_of(900, 700, SELECTION), 0);
    for _ in 0..4 {
        editor.extend_right();
    }
    assert!(!editor.book().selection().is_collapsed());
    let highlighted = editor.pixels_of(900, 700, SELECTION);
    assert!(
        highlighted > 0,
        "selected glyphs leave highlight pixels, got {highlighted}"
    );
    for _ in 0..4 {
        editor.move_right();
    }
    assert!(editor.book().selection().is_collapsed());
    assert_eq!(editor.pixels_of(900, 700, SELECTION), 0);
}
