//! M3 determinism: the same fixture rendered twice yields byte-identical PNGs,
//! and the render diff names exactly the connected changed regions.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{
    ChangedRegion, EvidenceOptions, EvidenceRender, diff_renders, encode_png, render_bytes_evidence,
};
use pdf_test_support::builder;

#[test]
fn same_fixture_renders_byte_identical_pngs() {
    let bytes = builder::multipage_classic(1);
    let options = EvidenceOptions::default();

    let first = render_bytes_evidence(&bytes, 0, None, &options).unwrap();
    let second = render_bytes_evidence(&bytes, 0, None, &options).unwrap();

    assert_eq!(first, second);
    assert_eq!(encode_png(&first).unwrap(), encode_png(&second).unwrap());

    let diff = diff_renders(&first, &second).unwrap();
    assert_eq!(diff.changed_pixels, 0);
    assert!(diff.regions.is_empty());
}

/// Opaque-black canvas with the listed pixels flipped to opaque red.
fn canvas(width: u32, height: u32, changed: &[(u32, u32)]) -> EvidenceRender {
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    for px in pixels.chunks_exact_mut(4) {
        px[3] = 255;
    }
    for &(x, y) in changed {
        let at = (y as usize * width as usize + x as usize) * 4;
        pixels[at] = 255;
    }
    EvidenceRender::from_rgba(width, height, pixels).unwrap()
}

#[test]
fn diff_extracts_connected_changed_regions() {
    let before = canvas(6, 3, &[]);
    // Two separated 2-pixel blocks: one on the top row, one on the bottom.
    let after = canvas(6, 3, &[(0, 0), (1, 0), (4, 2), (5, 2)]);

    let diff = diff_renders(&before, &after).unwrap();

    assert_eq!(diff.changed_pixels, 4);
    assert_eq!(
        diff.regions,
        vec![
            ChangedRegion {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
                pixels: 2,
            },
            ChangedRegion {
                x: 4,
                y: 2,
                width: 2,
                height: 1,
                pixels: 2,
            },
        ]
    );
}
