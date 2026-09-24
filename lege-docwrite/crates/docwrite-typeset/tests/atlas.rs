//! Glyph rasters are sized to the glyph and positioned from the baseline.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use docwrite_typeset::{Face, GlyphAtlas};

fn face() -> Face {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    Face::parse(std::fs::read(path).unwrap()).unwrap()
}

fn bitmap_of(
    atlas: &mut GlyphAtlas,
    face: &Face,
    ch: &str,
    size: f32,
) -> (u32, u32, i32, i32, usize) {
    let glyph = face.shape(ch, size, &[]).unwrap()[0].id;
    atlas
        .with_glyph(face, glyph, size, 0.0, |bitmap| {
            let ink = bitmap.coverage.iter().filter(|value| **value > 0).count();
            (bitmap.width, bitmap.height, bitmap.left, bitmap.top, ink)
        })
        .unwrap()
}

#[test]
fn large_glyphs_are_not_clipped_to_a_fixed_cell() {
    let face = face();
    let mut atlas = GlyphAtlas::new();
    let (_, height, _, top, ink) = bitmap_of(&mut atlas, &face, "H", 200.0);
    assert!(
        height > 120,
        "a 200px H is taller than the old 64px cell: {height}"
    );
    assert!(
        (130..=160).contains(&top),
        "cap height sits about 0.71 em up: {top}"
    );
    assert!(ink > 5_000, "the H is filled: {ink}");
}

#[test]
fn descenders_hang_below_the_baseline() {
    let face = face();
    let mut atlas = GlyphAtlas::new();
    let (_, height, _, top, _) = bitmap_of(&mut atlas, &face, "g", 48.0);
    assert!(
        height as i32 > top,
        "g reaches below the baseline: top {top}, height {height}"
    );
}

#[test]
fn faces_and_subpixel_offsets_get_their_own_rasters() {
    let face = face();
    let twin = face.duplicate().unwrap();
    assert_eq!(
        face.id(),
        twin.id(),
        "the same font bytes are the same face"
    );
    let mut atlas = GlyphAtlas::new();
    let glyph = face.shape("a", 16.0, &[]).unwrap()[0].id;
    atlas.ensure(&face, glyph, 16.0, 0.0).unwrap();
    atlas.ensure(&twin, glyph, 16.0, 0.0).unwrap();
    assert_eq!(atlas.len(), 1);
    atlas.ensure(&face, glyph, 16.0, 0.5).unwrap();
    assert_eq!(atlas.len(), 2);
}
