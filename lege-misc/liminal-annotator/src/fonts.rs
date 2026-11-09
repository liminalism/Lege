//! The bundled UI face: Noto Sans Regular (SIL OFL 1.1, `assets/NotoSans-OFL.txt`).

use std::sync::OnceLock;

use liminalkit_text::{Face, FaceId, FontSet};

const NOTO_SANS: &[u8] = include_bytes!("../assets/NotoSans-Regular.ttf");

pub const FACE: FaceId = FaceId(0);

pub fn font_set() -> &'static FontSet {
    static SET: OnceLock<FontSet> = OnceLock::new();
    SET.get_or_init(|| {
        let face: &'static Face = Box::leak(Box::new(
            Face::from_bytes(NOTO_SANS).expect("bundled Noto Sans parses"),
        ));
        let mut set = FontSet::new();
        set.add(face);
        set
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every non-ASCII character the UI draws. Tofu in the UI is a bug.
    pub const UI_GLYPHS: [char; 5] = ['…', '·', '×', '–', '—'];

    #[test]
    fn the_bundled_font_covers_the_ui_glyphs() {
        let face = font_set().face(FACE);
        let missing: Vec<char> = UI_GLYPHS
            .iter()
            .copied()
            .filter(|&g| !face.covers(g))
            .collect();
        assert!(missing.is_empty(), "Noto Sans lacks {missing:?}");
        // Known gaps: never use these in UI strings.
        for g in ['⇧', '−', '→'] {
            assert!(!face.covers(g), "{g:?} is now covered; the UI could use it");
        }
    }
}
