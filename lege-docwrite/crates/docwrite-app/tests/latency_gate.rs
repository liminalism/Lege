//! 500-page open, page, and keypress-to-present budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Instant;

use docwrite_app::{InputTrace, KEYPRESS_BUDGET, Pager, Phase, TraceCommand};
use docwrite_typeset::{Face, book_of_repeated_line};

fn noto() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    std::fs::read(&path).expect("noto")
}

#[test]
fn five_hundred_pages_page_and_type_inside_the_budget() {
    let face = Face::parse(noto()).expect("font");
    let opened = Instant::now();
    let mut document =
        book_of_repeated_line(face, 500, "A sentence of the manuscript.").expect("open");
    let open = opened.elapsed();
    assert_eq!(document.page_count(), 500);
    assert!(
        open <= KEYPRESS_BUDGET,
        "opening 500 cached pages took {open:?}, budget {KEYPRESS_BUDGET:?}"
    );

    let mut pager = Pager::new(document.page_count(), false);
    let mut trace = InputTrace::default();
    for _ in 0..40 {
        trace.push(TraceCommand::PageDown);
    }
    trace.push(TraceCommand::Scroll {
        delta: 0.0,
        phase: Phase::Started,
    });
    trace.push(TraceCommand::Scroll {
        delta: 12.4,
        phase: Phase::Moved,
    });
    trace.push(TraceCommand::Scroll {
        delta: 12.4,
        phase: Phase::Ended,
    });
    for page in [1u32, 250, 500] {
        trace.push(TraceCommand::Type {
            page,
            text: "k".to_string(),
        });
    }
    let steps = trace.replay(&mut document, &mut pager).expect("replay");
    assert_eq!(steps.len(), 46);
    assert_eq!(pager.scroll(), 52.0);
    for step in &steps {
        let latency = step.frame.input_to_present().expect("frame stamps");
        assert!(
            latency <= KEYPRESS_BUDGET,
            "replayed frame took {latency:?}, budget {KEYPRESS_BUDGET:?}"
        );
    }
    let typed = &steps[steps.len() - 3..];
    for (page, step) in [1u32, 250, 500].into_iter().zip(typed) {
        assert!(
            step.pages_laid_out.iter().all(|laid| *laid < page + 2),
            "typing on page {page} laid out {:?}",
            step.pages_laid_out
        );
        assert!(
            document.page_texts(page)[0].starts_with('k'),
            "page {page} text {:?}",
            document.page_texts(page)
        );
    }
    println!(
        "typing trace {} frames inside {:?}",
        steps.len(),
        KEYPRESS_BUDGET
    );
}
