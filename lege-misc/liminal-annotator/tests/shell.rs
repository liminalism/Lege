//! The window shell, headless: opening a dataset folder creates its project
//! on a background thread and lands on the book manager; opening it again
//! goes straight there; with nothing to open, the start screen.

use std::io::Write;
use std::time::{Duration, Instant};

use liminal_annotator::recent;
use liminal_annotator::shell::{Shell, ShellConfig};
use liminalkit_raster::WindowBuffer;
use liminalkit_shell::Scale;
use liminalkit_windowing::PixelApp;

fn pool(dir: &std::path::Path) {
    let d = dir.join("images").join("only-book");
    std::fs::create_dir_all(&d).unwrap();
    image::RgbImage::from_pixel(120, 180, image::Rgb([250, 250, 250]))
        .save(d.join("p0000.jpg"))
        .unwrap();
    let mut f = std::fs::File::create(dir.join("teachers_raw.jsonl")).unwrap();
    writeln!(
        f,
        r#"{{"image_id": "only-book_p0000", "width": 120, "height": 180, "paddle": [], "yolo": [["abandon", 0.9, 10, 2, 60, 12]]}}"#
    )
    .unwrap();
}

fn settle(shell: &mut Shell, buf: &mut WindowBuffer) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while shell.importing() {
        assert!(Instant::now() < deadline, "import did not finish");
        std::thread::sleep(Duration::from_millis(10));
        shell.render(buf, Scale::ONE);
    }
    shell.render(buf, Scale::ONE);
}

#[test]
fn a_folder_opens_on_the_book_manager_and_is_remembered() {
    let cfg_dir = tempfile::tempdir().unwrap();
    // This test binary is the only user of the variable.
    std::env::set_var("LIMINAL_ANNOTATOR_CONFIG_DIR", cfg_dir.path());
    let mut buf = WindowBuffer::new(1000, 700);

    // Nothing to open: the start screen.
    let mut shell = Shell::new(ShellConfig::default());
    assert!(shell.app().is_none());
    shell.render(&mut buf, Scale::ONE);

    let data = tempfile::tempdir().unwrap();
    pool(data.path());
    let mut shell = Shell::new(ShellConfig {
        open: Some(data.path().to_path_buf()),
        ..ShellConfig::default()
    });
    assert!(shell.importing(), "a new folder is imported first");
    settle(&mut shell, &mut buf);
    assert!(shell.error().is_none(), "{:?}", shell.error());
    let project = data.path().join("annotator.db");
    assert!(project.is_file());
    let app = shell.app().expect("project open");
    assert!(app.books_showing());
    assert_eq!(
        app.project_root().as_deref(),
        Some(&*std::path::absolute(data.path()).unwrap())
    );
    assert!(recent::same(
        &recent::load()[0],
        &std::path::absolute(&project).unwrap()
    ));

    // Dropping the same folder again: nothing new, so no import.
    shell.on_file_drop(data.path().to_path_buf());
    assert!(!shell.importing());
    assert!(shell.app().is_some_and(|a| a.books_showing()));

    // A plain start reopens the last project.
    drop(shell);
    let mut shell = Shell::new(ShellConfig::default());
    assert!(shell.app().is_some_and(|a| a.books_showing()));
    shell.render(&mut buf, Scale::ONE);

    // A folder with nothing usable is an error on the start screen, not a crash.
    let empty = tempfile::tempdir().unwrap();
    shell.on_file_drop(empty.path().to_path_buf());
    settle(&mut shell, &mut buf);
    assert!(
        shell
            .error()
            .is_some_and(|e| e.contains("no teachers_raw.jsonl")),
        "{:?}",
        shell.error()
    );
    assert!(shell.app().is_none());
    assert!(
        !empty.path().join("annotator.db").exists(),
        "no stray project left behind"
    );
}
