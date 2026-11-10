//! Recently opened projects, so a plain start (no arguments, or a
//! double-click in Explorer) reopens the last one and the start screen can
//! list the others.
//!
//! Stored per user: `%APPDATA%\liminal-annotator\recent.json` on Windows,
//! `$XDG_CONFIG_HOME/liminal-annotator/recent.json` (or `~/.config/…`)
//! elsewhere. `LIMINAL_ANNOTATOR_CONFIG_DIR` overrides the folder (tests).

use std::path::{Path, PathBuf};

const MAX: usize = 12;

pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("LIMINAL_ANNOTATOR_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("liminal-annotator"))
}

fn file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("recent.json"))
}

/// Recent project files, most recent first, that still exist.
pub fn load() -> Vec<PathBuf> {
    let Some(f) = file() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(f) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(&text)
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .collect()
}

/// Put `project` first. Failures are ignored: the list is a convenience.
pub fn touch(project: &Path) {
    let Some(f) = file() else { return };
    let abs = std::path::absolute(project).unwrap_or_else(|_| project.to_path_buf());
    let mut list = load();
    list.retain(|p| !same(p, &abs));
    list.insert(0, abs);
    list.truncate(MAX);
    let strings: Vec<String> = list
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&strings) {
        let _ = std::fs::write(f, json);
    }
}

/// Path equality as the file system sees it (case-insensitive on Windows).
pub fn same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}
