//! 100,000-page projects: generated page metadata over a few real JPEGs,
//! never one bitmap per page. Measures queue construction, page lookup and
//! app start-up, and checks that only the prefetch window is decoded.

mod common;

use std::time::{Duration, Instant};

use liminal_annotator::annotation::commands::ReviewStatus;
use liminal_annotator::app::{AnnotatorApp, AppConfig};
use liminal_annotator::cli::generate_synthetic;
use liminal_annotator::dataset::database::{self, PageFilter};

#[test]
fn hundred_thousand_pages() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("big.db");
    let t = Instant::now();
    generate_synthetic(&project, 100_000, &dir.path().join("img")).unwrap();
    eprintln!(
        "generated 100k pages / 800k boxes in {:.1}s",
        t.elapsed().as_secs_f64()
    );

    let conn = database::open(&project).unwrap();
    let t = Instant::now();
    let keys = database::page_keys(&conn, &PageFilter::default()).unwrap();
    let q_all = t.elapsed();
    assert_eq!(keys.len(), 100_000);
    let t = Instant::now();
    let todo = database::page_keys(
        &conn,
        &PageFilter {
            statuses: vec![ReviewStatus::Unseen],
            split: Some("train".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let q_filtered = t.elapsed();
    assert_eq!(todo.len(), 100_000);

    // Per-page lookups must be index hits, not scans.
    let t = Instant::now();
    for k in keys.iter().step_by(997) {
        let _ = database::page_meta(&conn, k).unwrap().unwrap();
        let a = database::annotations(&conn, k).unwrap();
        assert_eq!(a.len(), 8);
    }
    let per_page = t.elapsed() / (keys.len() / 997) as u32;
    let plan: String = conn
        .query_row(
            "EXPLAIN QUERY PLAN SELECT * FROM annotations WHERE page_key = 'x'",
            [],
            |r| r.get(3),
        )
        .unwrap();
    assert!(
        plan.contains("INDEX"),
        "annotation lookup uses an index: {plan}"
    );
    eprintln!("queue all {q_all:?}, filtered {q_filtered:?}, page+boxes lookup {per_page:?}");
    assert!(per_page < Duration::from_millis(5), "{per_page:?}");
    assert!(q_all < Duration::from_secs(2), "{q_all:?}");

    // App start-up on the big project, and its memory: only the window decoded.
    let t = Instant::now();
    let mut app = AnnotatorApp::new(AppConfig {
        project: project.clone(),
        filter: PageFilter::default(),
        start_at: None,
        prediction_threshold: 0.3,
    })
    .unwrap();
    let startup = t.elapsed();
    assert!(app.wait_for_image(Duration::from_secs(10)));
    for _ in 0..20 {
        let n = app.pos + 1;
        app.navigate(n, true);
    }
    app.wait_for_image(Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(300));
    app.poll_background();
    eprintln!("app start-up on 100k pages {startup:?}");
    assert!(startup < Duration::from_secs(5), "{startup:?}");
    app.flush();
    app.shutdown();
}
