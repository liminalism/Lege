use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::RwLock;

pub type Rgb = (u8, u8, u8);

/// The app's active theme: either the built-in Default (day) palette or one of the
/// curated Sanzo Wada combos in light or dark mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    Default,
    /// `index` is the 0-based position in `sanzowada::SANZO_THEMES`.
    Sanzo {
        index: usize,
        dark: bool,
    },
}

impl Default for ThemeChoice {
    fn default() -> Self {
        ThemeChoice::Default
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppearanceSettings {
    pub theme: ThemeChoice,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Default,
        }
    }
}

/// On-disk form of [`ThemeChoice`]. Sanzo themes are identified by their
/// source color names, which are stable across reorders or removals in
/// `SANZO_THEMES`; `index` is written too so older builds can still read the
/// file, and doubles as a fallback when the names fail to resolve.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredThemeChoice {
    Default,
    Sanzo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        colors: Option<Vec<String>>,
        #[serde(default)]
        index: Option<usize>,
        dark: bool,
    },
}

impl Default for StoredThemeChoice {
    fn default() -> Self {
        StoredThemeChoice::Default
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredAppearanceSettings {
    theme: StoredThemeChoice,
}

fn to_stored(choice: ThemeChoice) -> StoredThemeChoice {
    match choice {
        ThemeChoice::Default => StoredThemeChoice::Default,
        ThemeChoice::Sanzo { index, dark } => StoredThemeChoice::Sanzo {
            colors: crate::sanzowada::theme_at(index)
                .map(|t| t.colors.iter().map(|c| c.to_string()).collect()),
            index: Some(index),
            dark,
        },
    }
}

fn from_stored(stored: StoredThemeChoice) -> ThemeChoice {
    match stored {
        StoredThemeChoice::Default => ThemeChoice::Default,
        StoredThemeChoice::Sanzo {
            colors,
            index,
            dark,
        } => {
            let resolved = colors
                .as_deref()
                .and_then(crate::sanzowada::theme_index_by_colors)
                .or(index)
                .filter(|&i| i < crate::sanzowada::THEME_COUNT);
            match resolved {
                Some(index) => ThemeChoice::Sanzo { index, dark },
                None => ThemeChoice::Default,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub app_bg: Rgb,
    pub panel_bg: Rgb,
    pub card_bg: Rgb,
    pub control_bg: Rgb,
    pub selected_bg: Rgb,
    pub hover_bg: Rgb,
    pub focus_bg: Rgb,
    pub active_bg: Rgb,
    pub info_bg: Rgb,
    pub text: Rgb,
    pub muted: Rgb,
    pub border: Rgb,
    pub border_focus: Rgb,
    pub check_mark: Rgb,
    pub inverse_surface: Rgb,
    pub inverse_surface_secondary: Rgb,
    pub inverse_surface_tertiary: Rgb,
    pub progress_track_bg: Rgb,
}

const BASE_APP_BG: Rgb = (226, 232, 235);

// On Linux the light surfaces use a slightly warmer base than on other platforms.
#[cfg(target_os = "linux")]
const BASE_PANEL_BG: Rgb = (253, 249, 244);
#[cfg(not(target_os = "linux"))]
const BASE_PANEL_BG: Rgb = (255, 250, 245);

#[cfg(target_os = "linux")]
const BASE_SELECTED_BG: Rgb = (255, 241, 241);
#[cfg(not(target_os = "linux"))]
const BASE_SELECTED_BG: Rgb = (255, 243, 243);

#[cfg(target_os = "linux")]
const BASE_CARD_BG: Rgb = (253, 249, 244);
#[cfg(not(target_os = "linux"))]
const BASE_CARD_BG: Rgb = (255, 250, 245);

/// The built-in "Default" theme (the original daytime look).
pub(crate) const BASE_DAY: Palette = Palette {
    app_bg: BASE_APP_BG,
    panel_bg: BASE_PANEL_BG,
    card_bg: BASE_CARD_BG,
    control_bg: (255, 250, 250),
    selected_bg: BASE_SELECTED_BG,
    hover_bg: (236, 236, 236),
    focus_bg: (232, 232, 232),
    active_bg: (210, 210, 210),
    info_bg: (255, 255, 225),
    text: (20, 20, 20),
    muted: (90, 90, 90),
    border: (128, 128, 128),
    border_focus: (64, 64, 64),
    check_mark: (30, 30, 30),
    inverse_surface: (128, 128, 128),
    inverse_surface_secondary: (96, 96, 96),
    inverse_surface_tertiary: (64, 64, 64),
    progress_track_bg: (236, 236, 236),
};

static SETTINGS: LazyLock<RwLock<AppearanceSettings>> =
    LazyLock::new(|| RwLock::new(load_appearance_settings().unwrap_or_default()));
// A non-persisted override used by the theme picker to preview a choice live while
// browsing; committing writes it to SETTINGS and clears this.
static PREVIEW: LazyLock<RwLock<Option<ThemeChoice>>> = LazyLock::new(|| RwLock::new(None));

/// Force the persisted settings to load at startup.
pub fn initialize_runtime_appearance() {
    drop(SETTINGS.read().unwrap_or_else(|e| e.into_inner()));
}

/// The committed (persisted) theme choice.
pub fn committed_theme_choice() -> ThemeChoice {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).theme
}

/// The choice currently driving the palette: the live preview if one is set, else the
/// committed choice.
pub fn active_theme_choice() -> ThemeChoice {
    PREVIEW
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or_else(committed_theme_choice)
}

/// Set a live, non-persisted preview (used while browsing the picker). `None` clears it
/// and falls back to the committed theme.
pub fn set_theme_preview(choice: Option<ThemeChoice>) {
    *PREVIEW.write().unwrap_or_else(|e| e.into_inner()) = choice;
}

/// Commit a theme choice: persist it and clear any active preview.
pub fn commit_theme_choice(choice: ThemeChoice) -> anyhow::Result<()> {
    {
        let mut settings = SETTINGS.write().unwrap_or_else(|e| e.into_inner());
        persist_theme_choice(&mut settings, choice, save_appearance_settings)?;
    }
    *PREVIEW.write().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(())
}

fn persist_theme_choice(
    settings: &mut AppearanceSettings,
    choice: ThemeChoice,
    save: impl FnOnce(&AppearanceSettings) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let candidate = AppearanceSettings { theme: choice };
    save(&candidate)?;
    *settings = candidate;
    Ok(())
}

pub fn active_palette() -> Palette {
    palette_for(active_theme_choice())
}

/// The palette a theme choice draws with.
pub(crate) fn palette_for(choice: ThemeChoice) -> Palette {
    match choice {
        ThemeChoice::Default => BASE_DAY,
        ThemeChoice::Sanzo { index, dark } => match crate::sanzowada::theme_at(index) {
            Some(t) => {
                let mut palette = if dark { t.dark } else { t.light };

                // Notifications use the combo's lightest source color instead of
                // the built-in yellow; the light palette's background is that color.
                palette.info_bg = t.light.app_bg;

                // Text keeps the combo's own color (its darkest in a light theme,
                // its lightest in a dark one), moved only as far toward black or
                // white as legibility needs, so every theme shows all its colors.
                palette.text = legible(palette.text, palette.app_bg, TEXT_CONTRAST);
                palette.muted = legible(palette.muted, palette.app_bg, MUTED_CONTRAST);
                palette
            }
            None => BASE_DAY,
        },
    }
}

/// WCAG contrast for body text, and for the secondary (muted) text.
const TEXT_CONTRAST: f32 = 4.5;
const MUTED_CONTRAST: f32 = 3.0;

/// `color`, or the nearest mix of it away from the background (toward white
/// if it is the lighter of the two, else toward black) with at least
/// `minimum` WCAG contrast. On a mid-tone background that is out of reach,
/// the text goes all the way to white or black, keeping a dark theme's light
/// text light rather than flipping it.
pub(crate) fn legible(color: Rgb, background: Rgb, minimum: f32) -> Rgb {
    let target = if luminance(color) >= luminance(background) {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    };
    (0..=20)
        .map(|step| blend(color, target, step as f32 / 20.0))
        .find(|candidate| contrast(*candidate, background) >= minimum)
        .unwrap_or(target)
}

/// Label color for text drawn on `fill` (a selected button, say): the
/// theme's text color made legible against that fill. A label may flip to
/// the other extreme when only that one is legible on the fill.
pub fn text_on(fill: Rgb) -> Rgb {
    let text = legible(active_palette().text, fill, TEXT_CONTRAST);
    if contrast(text, fill) >= TEXT_CONTRAST {
        return text;
    }
    let (black, white) = ((0, 0, 0), (255, 255, 255));
    if contrast(black, fill) > contrast(white, fill) {
        black
    } else {
        white
    }
}

/// WCAG 2 contrast ratio between two sRGB colors.
pub(crate) fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (lighter, darker) = {
        let (la, lb) = (luminance(a), luminance(b));
        if la > lb { (la, lb) } else { (lb, la) }
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// WCAG 2 relative luminance.
fn luminance(c: Rgb) -> f32 {
    let channel = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.0) + 0.7152 * channel(c.1) + 0.0722 * channel(c.2)
}

fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let mix = |x: u8, y: u8| -> u8 {
        ((x as f32 * (1.0 - t)) + (y as f32 * t))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn settings_directory() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Lege")
}

pub fn get_appearance_settings_file_path() -> PathBuf {
    settings_directory().join("appearance.json")
}

pub fn load_appearance_settings() -> anyhow::Result<AppearanceSettings> {
    let settings_path = get_appearance_settings_file_path();
    if !settings_path.exists() {
        return Ok(AppearanceSettings::default());
    }

    let content = fs::read_to_string(&settings_path)?;
    let stored: StoredAppearanceSettings = serde_json::from_str(&content)?;
    Ok(AppearanceSettings {
        theme: from_stored(stored.theme),
    })
}

pub fn save_appearance_settings(settings: &AppearanceSettings) -> anyhow::Result<()> {
    let settings_path = get_appearance_settings_file_path();
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let stored = StoredAppearanceSettings {
        theme: to_stored(settings.theme),
    };
    // Keep the previous settings file intact if serialization, writing or
    // flushing fails. Commit the complete candidate with one rename.
    let parent = settings_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), &stored)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&settings_path)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_failed_theme_persistence_preserves_the_committed_choice() {
        let mut settings = AppearanceSettings::default();
        let selected = ThemeChoice::Sanzo {
            index: 1,
            dark: true,
        };
        let result = persist_theme_choice(&mut settings, selected, |candidate| {
            assert_eq!(candidate.theme, selected);
            Err(anyhow::anyhow!("disk write failed"))
        });
        assert!(result.is_err());
        assert_eq!(settings.theme, ThemeChoice::Default);
        persist_theme_choice(&mut settings, selected, |_| Ok(())).unwrap();
        assert_eq!(settings.theme, selected);
    }

    #[test]
    fn default_theme_is_the_day_palette() {
        assert_eq!(AppearanceSettings::default().theme, ThemeChoice::Default);
        set_theme_preview(Some(ThemeChoice::Default));
        assert_eq!(active_palette(), BASE_DAY);
        set_theme_preview(None);
    }

    #[test]
    fn preview_drives_active_palette_light_and_dark() {
        let t = crate::sanzowada::theme_at(0).unwrap();

        set_theme_preview(Some(ThemeChoice::Sanzo {
            index: 0,
            dark: false,
        }));
        let light = active_palette();
        assert_eq!(light.app_bg, t.light.app_bg, "the combo's own background");
        assert_eq!(light.info_bg, t.light.app_bg);

        set_theme_preview(Some(ThemeChoice::Sanzo {
            index: 0,
            dark: true,
        }));
        let dark = active_palette();
        assert_eq!(dark.app_bg, t.dark.app_bg);
        assert_eq!(dark.info_bg, t.light.app_bg);
        // Golden Yellow on Slate Color is legible as it is.
        assert_eq!(dark.text, t.dark.text);

        // Out-of-range index falls back to the default palette rather than panicking.
        set_theme_preview(Some(ThemeChoice::Sanzo {
            index: usize::MAX,
            dark: false,
        }));
        assert_eq!(active_palette(), BASE_DAY);

        set_theme_preview(None);
    }

    #[test]
    fn every_theme_keeps_its_colors_and_legible_text() {
        for index in 0..crate::sanzowada::THEME_COUNT {
            let t = crate::sanzowada::theme_at(index).unwrap();
            for dark in [false, true] {
                let p = palette_for(ThemeChoice::Sanzo { index, dark });
                let source = if dark { t.dark } else { t.light };
                assert_eq!(p.app_bg, source.app_bg, "theme {index} dark={dark}");
                for (color, minimum) in [(p.text, TEXT_CONTRAST), (p.muted, MUTED_CONTRAST)] {
                    // Pure white or black is as far as a mid-tone allows.
                    assert!(
                        contrast(color, p.app_bg) >= minimum
                            || [(0, 0, 0), (255, 255, 255)].contains(&color),
                        "{index} {dark}: {color:?} on {:?}",
                        p.app_bg
                    );
                }
                // A dark theme's text stays light, a light theme's dark.
                assert_eq!(
                    luminance(p.text) > luminance(p.app_bg),
                    dark,
                    "{index} {dark}"
                );
                // Text that is already legible keeps the combo's exact color.
                if contrast(source.text, source.app_bg) >= TEXT_CONTRAST {
                    assert_eq!(p.text, source.text, "{index} {dark}");
                }
            }
        }
    }

    #[test]
    fn legible_keeps_a_legible_color_and_mixes_a_faint_one() {
        let bg = (58, 57, 61);
        assert_eq!(legible((255, 168, 63), bg, 4.5), (255, 168, 63));
        let fixed = legible((90, 80, 70), bg, 4.5);
        assert!(contrast(fixed, bg) >= 4.5);
        assert_ne!(fixed, (255, 255, 255));
    }

    #[test]
    fn theme_choice_round_trips_through_stored_form() {
        let choice = ThemeChoice::Sanzo {
            index: 7,
            dark: true,
        };
        let stored = StoredAppearanceSettings {
            theme: to_stored(choice),
        };
        let json = serde_json::to_string(&stored).unwrap();
        assert!(
            json.contains("colors"),
            "stable color names must be persisted: {json}"
        );
        let back: StoredAppearanceSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(from_stored(back.theme), choice);
    }

    #[test]
    fn legacy_index_only_settings_still_load() {
        let json = r#"{"theme":{"kind":"sanzo","index":3,"dark":false}}"#;
        let stored: StoredAppearanceSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            from_stored(stored.theme),
            ThemeChoice::Sanzo {
                index: 3,
                dark: false
            }
        );
    }

    #[test]
    fn color_names_win_over_a_stale_index() {
        // Simulates SANZO_THEMES having been reordered since the file was
        // written: the persisted names identify theme 2, the stale index says 0.
        let names: Vec<String> = crate::sanzowada::theme_at(2)
            .unwrap()
            .colors
            .iter()
            .map(|c| c.to_string())
            .collect();
        let stored = StoredThemeChoice::Sanzo {
            colors: Some(names),
            index: Some(0),
            dark: true,
        };
        assert_eq!(
            from_stored(stored),
            ThemeChoice::Sanzo {
                index: 2,
                dark: true
            }
        );
    }

    #[test]
    fn unresolvable_stored_theme_falls_back_to_default() {
        let stored = StoredThemeChoice::Sanzo {
            colors: Some(vec!["Not A Real Color".to_string()]),
            index: Some(usize::MAX),
            dark: false,
        };
        assert_eq!(from_stored(stored), ThemeChoice::Default);
    }
}
