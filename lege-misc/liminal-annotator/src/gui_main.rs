//! liminal-annotator-gui: the window without a console (Windows GUI
//! subsystem). Takes an optional folder or project to open, as Explorer
//! passes it for "Open with" or a dropped file, and `--grid` for the
//! six-page view; everything else is in the window. Errors that stop it from starting are shown in a message box.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use liminal_annotator::shell::{self, ShellConfig};

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // `--grid` opens the six-page view, for launchers.
    let grid = args.iter().any(|a| a == "--grid");
    let open = args
        .iter()
        .find(|a| a.to_str().is_none_or(|s| !s.starts_with("--")))
        .map(PathBuf::from)
        .filter(|p| p.exists());
    let cfg = ShellConfig {
        open,
        grid,
        ..ShellConfig::default()
    };
    if let Err(e) = shell::run(cfg) {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Liminal Annotator")
            .set_description(e)
            .show();
    }
}
