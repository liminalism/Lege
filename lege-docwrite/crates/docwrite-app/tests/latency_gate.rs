//! Keypress-to-present budget.
//!
//! The editor test types through [`Editor`], the same path the window drives:
//! edit the book, refresh the layout incrementally, paint the frame. Work is
//! asserted in every build; wall-clock time only in optimized builds, since a
//! debug build is not what a writer types into:
//! `cargo test --release -p docwrite-app --test latency_gate -- --nocapture`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Instant;

use docwrite_app::{Editor, InputTrace, KEYPRESS_BUDGET, Pager, Phase, TraceCommand};
use docwrite_model::{Book, ChapterStart, Position, Selection};
use docwrite_typeset::{Face, book_of_repeated_line};

fn noto() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf");
    std::fs::read(&path).expect("noto")
}

#[test]
fn a_replayed_trace_pages_and_types_inside_the_budget() {
    let face = Face::parse(noto()).expect("font");
    let mut document =
        book_of_repeated_line(face, 500, "A sentence of the manuscript.").expect("open");
    assert_eq!(document.page_count(), 500);

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

const PROSE: &str = "The archive kept its letters in bundles tied with string, \
and each bundle carried a date in a hand that changed over the years from \
careful to hurried. Reading them in order was like watching a clerk grow old.";

/// About 500 pages: 50 chapters of 80 four-line paragraphs.
fn long_book() -> Book {
    let mut book = Book::new("Archive");
    let mut template = book.chapter_templates()[0].clone();
    template.start = ChapterStart::NextPage;
    book.set_chapter_template(template).expect("template");
    let body = |chapter: usize| -> String {
        (0..80)
            .map(|n| format!("{chapter}.{n}. {PROSE}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    book.insert(&body(0)).expect("insert");
    let part = book.parts()[0].id();
    for chapter in 1..50 {
        book.add_chapter(part, format!("Chapter {}", chapter + 1))
            .expect("chapter");
        let fresh = *book.block_ids().last().expect("block");
        book.set_selection(Selection::collapsed(Position::new(fresh, 0)))
            .expect("select");
        book.insert(&body(chapter)).expect("insert");
    }
    book
}

#[test]
fn typing_into_the_editor_stays_inside_the_budget() {
    let mut editor = Editor::with_book(long_book());
    let mut frame = pixelkit_raster::WindowBuffer::new(1100, 800);
    let opened = Instant::now();
    editor.paint(&mut frame);
    let open = opened.elapsed();
    let pages = editor.pages_painted();
    assert!(pages >= 450, "the long book has {pages} pages");

    let blocks = editor.book().block_ids();
    let mut latencies = Vec::new();
    for fraction in [0.0, 0.5, 0.999] {
        let block = blocks[((blocks.len() - 1) as f64 * fraction) as usize];
        editor
            .book_mut()
            .set_selection(Selection::collapsed(Position::new(block, 5)))
            .expect("caret");
        editor.type_text(" ");
        editor.paint(&mut frame); // settle the view on the caret's page
        for ch in ["k", "e", "y", "s"] {
            let started = Instant::now();
            editor.type_text(ch);
            editor.paint(&mut frame);
            latencies.push(started.elapsed());
            let report = editor.last_layout().expect("layout ran");
            assert_eq!(
                report.paragraphs_shaped, 1,
                "at {fraction}: one paragraph shaped"
            );
            assert!(
                report.pages_laid_out.len() <= 3,
                "at {fraction}: typing laid out {:?} of {pages}",
                report.pages_laid_out
            );
            assert!(
                editor.caret_area().is_some(),
                "at {fraction}: the caret is in view"
            );
        }
    }
    latencies.sort();
    let p50 = latencies[latencies.len() / 2];
    let max = latencies[latencies.len() - 1];
    println!("{pages} pages: open {open:?}; keypress-to-present p50 {p50:?}, max {max:?}");
    if !cfg!(debug_assertions) {
        assert!(
            max <= KEYPRESS_BUDGET,
            "keypress-to-present max {max:?}, budget {KEYPRESS_BUDGET:?}"
        );
    }
}
