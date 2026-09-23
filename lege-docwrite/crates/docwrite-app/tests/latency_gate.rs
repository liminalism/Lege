//! 500-page open, page, and keypress-to-present budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Instant;

use docwrite_app::{Pager, Phase, KEYPRESS_BUDGET};
use docwrite_typeset::{book_of_repeated_line, Face};

fn noto() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    std::fs::read(&path).expect("noto")
}

#[test]
fn five_hundred_pages_page_and_type_inside_the_budget() {
    let face = Face::parse(noto()).expect("font");
    let opened = Instant::now();
    let mut document = book_of_repeated_line(face, 500, "A sentence of the manuscript.").expect("open");
    let open = opened.elapsed();
    assert_eq!(document.page_count(), 500);
    assert!(
        open <= KEYPRESS_BUDGET,
        "opening 500 cached pages took {open:?}, budget {KEYPRESS_BUDGET:?}"
    );

    let mut pager = Pager::new(document.page_count(), false);
    for _ in 0..40 {
        let frame = Instant::now();
        pager.page_down();
        let elapsed = frame.elapsed();
        assert!(elapsed <= KEYPRESS_BUDGET, "page-down frame {elapsed:?}");
    }
    let frame = Instant::now();
    pager.scroll_gesture(0.0, Phase::Started);
    pager.scroll_gesture(12.4, Phase::Moved);
    pager.scroll_gesture(12.4, Phase::Ended);
    assert!(frame.elapsed() <= KEYPRESS_BUDGET, "scroll snap frame");
    assert_eq!(pager.scroll(), 52.0);

    for page in [1u32, 250, 500] {
        let frame = Instant::now();
        let report = document.edit_page(page, "k").expect("type");
        let elapsed = frame.elapsed();
        assert!(
            report.pages_laid_out.iter().all(|laid| *laid < page + 2),
            "typing on page {page} laid out {:?}",
            report.pages_laid_out
        );
        assert!(
            elapsed <= KEYPRESS_BUDGET,
            "keypress on page {page} took {elapsed:?}, budget {KEYPRESS_BUDGET:?}"
        );
    }
}
