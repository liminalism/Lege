//! The M1 convergence gate: an edit on page 147 of 400 does not lay out
//! pages 149–400.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use docwrite_typeset::{Face, GlyphAtlas, book_of_pages};

fn noto() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

#[test]
fn edit_on_page_147_of_400_skips_pages_149_through_400() {
    let face = Face::parse(noto()).expect("noto");
    let mut document = book_of_pages(face, 400, "Line").expect("book");
    assert_eq!(document.page_count(), 400, "the fixture is 400 pages");

    let before = document.page_texts(400);
    let report = document.edit_page(147, "x").expect("edit");
    assert!(
        report.pages_laid_out.contains(&147),
        "page 147 is rebuilt, laid out {:?}",
        report.pages_laid_out
    );
    assert!(
        report.pages_laid_out.iter().all(|page| *page < 149),
        "pages 149-400 received layout work: {:?}",
        report.pages_laid_out
    );
    assert_eq!(document.page_count(), 400);
    assert_eq!(
        document.page_texts(400),
        before,
        "the last page is the cached one"
    );
    assert!(document.page_texts(147)[0].starts_with('x'));
    println!(
        "page 147 of 400 laid out {:?}; every rebuilt page is < 149; page 400 unchanged",
        report.pages_laid_out
    );
}

#[test]
fn shaping_distinguishes_small_caps_and_the_atlas_caches_a_glyph() {
    let bytes = noto();
    let face = Face::parse(bytes).expect("noto");
    let plain = face.shape("Hello 123", 16.0, &[]).expect("shape");
    let featured = face
        .shape(
            "Hello 123",
            16.0,
            &docwrite_typeset::features_for(true, true),
        )
        .expect("features");
    assert_ne!(
        plain.iter().map(|glyph| glyph.id).collect::<Vec<_>>(),
        featured.iter().map(|glyph| glyph.id).collect::<Vec<_>>()
    );
    let mut atlas = GlyphAtlas::new();
    let glyph = plain[0].id;
    assert!(atlas.ensure(&face, glyph, 32.0, 0.0).expect("raster"));
    let first = atlas.len();
    assert!(atlas.ensure(&face, glyph, 32.0, 0.0).expect("cache"));
    assert_eq!(
        atlas.len(),
        first,
        "a second lookup does not rasterize again"
    );
}
