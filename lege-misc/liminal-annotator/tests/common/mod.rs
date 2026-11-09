#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use liminal_annotator::app::{AnnotatorApp, AppConfig};
use liminal_annotator::cli::generate_synthetic;
use liminal_annotator::dataset::database::PageFilter;
use liminalkit_raster::WindowBuffer;
use liminalkit_shell::Scale;
use liminalkit_windowing::PixelApp;

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub project: PathBuf,
}

/// A project of `pages` synthetic 850×1280 pages, each with eight boxes at
/// x 70..780, y 90+140k .. 210+140k (box 3 is a figure).
pub fn fixture(pages: usize) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("t.db");
    generate_synthetic(&project, pages, &dir.path().join("img")).expect("synthetic project");
    Fixture { dir, project }
}

pub fn open_app(f: &Fixture) -> AnnotatorApp {
    let mut app = AnnotatorApp::new(AppConfig {
        project: f.project.clone(),
        filter: PageFilter::default(),
        start_at: None,
        prediction_threshold: 0.3,
    })
    .expect("app");
    assert!(app.wait_for_image(Duration::from_secs(10)), "image loads");
    app
}

pub struct Screen {
    pub buf: WindowBuffer,
    pub scale: Scale,
}

impl Screen {
    pub fn new(w: u32, h: u32, scale: f64) -> Screen {
        Screen {
            buf: WindowBuffer::new(w, h),
            scale: Scale::new(scale),
        }
    }
    pub fn frame(&mut self, app: &mut AnnotatorApp) {
        app.render(&mut self.buf, self.scale);
    }
}
