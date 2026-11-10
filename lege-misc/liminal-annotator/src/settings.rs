//! User settings: hotkey order, colours, cache budget, interaction tuning.
//!
//! Stored as JSON next to the project (`<project>.settings.json`) and created
//! with defaults on first run, so the hotkey order can be edited without
//! rebuilding. Unknown or missing fields fall back to defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::annotation::labels::NUM_CLASSES;
use crate::annotation::labels::{ABANDON_CLASS, CLASSES, NUM_LABELS, class_by_name, is_taxonomy};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Class names on the top-row digits 1..9 (physical `Digit1`..`Digit9`).
    pub digit_keys: Vec<String>,
    /// Class names on Shift+1..Shift+5.
    pub shift_digit_keys: Vec<String>,
    /// Use the physical key position (default). When false, the character the
    /// layout produces is used instead (for layouts where the top row does
    /// not produce digits unshifted, e.g. AZERTY, set this to true — the
    /// default — and the physical row is used regardless).
    pub physical_digits: bool,
    /// Per-class colour overrides, class name → "#rrggbb".
    pub colors: std::collections::BTreeMap<String, String>,
    /// Decoded-image cache budget in MiB.
    pub cache_mb: usize,
    pub prefetch_ahead: usize,
    pub prefetch_behind: usize,
    /// Half-width of the invisible edge hit band, logical pixels.
    pub hit_slop: f64,
    /// Zoom factor per wheel notch.
    pub wheel_zoom_step: f64,
    /// Draw the class name tag on each box.
    pub show_tags: bool,
    /// Tint the selected box's interior.
    pub selection_fill_alpha: u8,
    /// Image decode worker threads.
    pub decode_threads: usize,
    /// Book manager: flag a book when it has at least this many YOLO
    /// `abandon` boxes left per page (a running head and a page number on
    /// every page make about 1.5)...
    pub abandon_flag_per_page: f64,
    /// ...and at least this many in all.
    pub abandon_flag_min_boxes: usize,
    /// The "large pictures" review queue: pages with a figure box covering
    /// at least this percentage of the page.
    pub large_figure_percent: f64,
}

impl Default for Settings {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect();
        Settings {
            digit_keys: s(&[
                "text",
                "figure",
                "caption",
                "heading",
                "table",
                "page_number",
                "running_header",
                "footnote",
                "reference",
            ]),
            shift_digit_keys: s(&["running_footer", "marginalia", "toc", "formula", "ornament"]),
            physical_digits: true,
            colors: Default::default(),
            cache_mb: 256,
            prefetch_ahead: 2,
            prefetch_behind: 1,
            hit_slop: 8.0,
            wheel_zoom_step: 1.2,
            show_tags: true,
            selection_fill_alpha: 28,
            decode_threads: 2,
            abandon_flag_per_page: 3.0,
            abandon_flag_min_boxes: 20,
            large_figure_percent: 40.0,
        }
    }
}

/// Resolved, validated settings for fast lookup.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub digit: [Option<u8>; 9],
    pub shift_digit: [Option<u8>; 9],
    pub colors: [u32; NUM_LABELS],
    pub raw: Settings,
}

impl Settings {
    pub fn path_for(project: &Path) -> PathBuf {
        let mut name = project.file_name().unwrap_or_default().to_os_string();
        name.push(".settings.json");
        project.with_file_name(name)
    }

    /// Load without creating anything (command-line tools): defaults when
    /// the file is missing or malformed.
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// The large-picture queue's share of the page (0..1).
    pub fn large_figure_share(&self) -> f64 {
        let p = self.large_figure_percent;
        if p.is_finite() && p > 0.0 && p <= 100.0 {
            p / 100.0
        } else {
            crate::dataset::database::DEFAULT_LARGE_FIGURE_SHARE
        }
    }

    /// Load, or write defaults if the file does not exist. A malformed file
    /// is reported and defaults are used (the file is left alone).
    pub fn load_or_create(path: &Path) -> (Settings, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(s) => (s, None),
                Err(e) => (
                    Settings::default(),
                    Some(format!("settings {}: {e}; using defaults", path.display())),
                ),
            },
            Err(_) => {
                let s = Settings::default();
                let _ = std::fs::write(path, serde_json::to_string_pretty(&s).unwrap_or_default());
                (s, None)
            }
        }
    }

    pub fn resolve(self) -> (Resolved, Vec<String>) {
        let mut warnings = Vec::new();
        let mut lookup = |names: &[String], what: &str| {
            let mut out = [None; 9];
            for (i, n) in names.iter().take(9).enumerate() {
                match class_by_name(n) {
                    Some(id) if is_taxonomy(id) => out[i] = Some(id),
                    Some(_) => warnings.push(format!(
                        "{what} key {}: {n:?} is a holding class and cannot be drawn",
                        i + 1
                    )),
                    None => warnings.push(format!("{what} key {}: unknown class {n:?}", i + 1)),
                }
            }
            out
        };
        let digit = lookup(&self.digit_keys, "digit");
        let shift_digit = lookup(&self.shift_digit_keys, "shift+digit");
        let p = self.large_figure_percent;
        if !(p.is_finite() && p > 0.0 && p <= 100.0) {
            warnings.push(format!(
                "large_figure_percent {p} is not between 0 and 100; using 40"
            ));
        }
        let mut colors = [0u32; NUM_LABELS];
        for c in CLASSES.iter().chain(std::iter::once(&ABANDON_CLASS)) {
            colors[c.id as usize] = c.color;
        }
        for (name, hex) in &self.colors {
            match (class_by_name(name), parse_hex(hex)) {
                (Some(id), Some(rgb)) => colors[id as usize] = rgb,
                _ => warnings.push(format!("colour {name:?} = {hex:?} ignored")),
            }
        }
        (
            Resolved {
                digit,
                shift_digit,
                colors,
                raw: self,
            },
            warnings,
        )
    }
}

fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_put_text_and_figure_on_1_and_2() {
        let (r, w) = Settings::default().resolve();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(r.digit[0], Some(0));
        assert_eq!(r.digit[1], Some(6));
        assert_eq!(r.shift_digit[4], Some(13));
        // Every class reachable exactly once.
        let mut seen = [0; NUM_CLASSES];
        for id in r.digit.iter().chain(r.shift_digit.iter()).flatten() {
            seen[*id as usize] += 1;
        }
        assert!(seen.iter().all(|&n| n == 1), "{seen:?}");
    }
}
