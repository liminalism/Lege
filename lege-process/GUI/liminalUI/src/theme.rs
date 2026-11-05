use crate::appearance::{Palette, Rgb};
use liminalkit_text::{Face, FaceId, FontSet, TextStyle};
use liminalkit_ui::Theme;
use std::sync::OnceLock;

pub const FACE: FaceId = FaceId(0);
pub fn fonts() -> &'static FontSet {
    static FONTS: OnceLock<FontSet> = OnceLock::new();
    FONTS.get_or_init(|| {
        let bytes = include_bytes!("../fonts/Inter-Regular.ttf");
        let face = Face::from_bytes(bytes).expect("bundled Inter must parse");
        let mut fonts = FontSet::new();
        fonts.add(Box::leak(Box::new(face)));
        fonts
    })
}

/// Inter's x-height as a fraction of its em (1536 of 2816 units).
const X_HEIGHT: f32 = 1536.0 / 2816.0;

/// The physical pixel size nearest `px` whose x-height is a whole number of
/// pixels. Every baseline is on a pixel row, so at such a size the tops of
/// lowercase letters land on a row too instead of smearing across two. The
/// text cache's vertical hinting would snap the x-height anyway; at a size
/// where it is already whole, hinting only moves caps and ascenders and
/// lowercase keeps its designed proportions.
pub fn crisp(px: f32) -> f32 {
    (px * X_HEIGHT).round().max(1.0) / X_HEIGHT
}

/// [`crisp`], never larger than `px`: for text shrunk to fit a width.
pub fn crisp_down(px: f32) -> f32 {
    (px * X_HEIGHT).floor().max(1.0) / X_HEIGHT
}

/// Snap a `Ui`'s theme fonts once it has scaled them to physical pixels.
pub fn snap_fonts(theme: &mut Theme) {
    theme.body.size = crisp(theme.body.size);
    theme.heading.size = crisp(theme.heading.size);
}

pub fn rgb(c: Rgb) -> u32 {
    (u32::from(c.0) << 16) | (u32::from(c.1) << 8) | u32::from(c.2)
}

/// Every size here is logical. The controls scale `padding`, `row_height`,
/// `corner_radius`, `border_width` and `focus_width` themselves, and
/// `Ui::new` converts the two font styles to physical pixels once, after
/// which [`snap_fonts`] snaps them; code that reads a size token directly
/// goes through `Ui::px`.
pub fn theme(p: Palette) -> Theme {
    Theme {
        background: rgb(p.control_bg),
        panel: rgb(p.card_bg),
        panel_alt: rgb(p.control_bg),
        panel_edge: rgb(p.border),
        text: rgb(p.text),
        text_dim: rgb(p.muted),
        // Controls draw a focused edge in `accent`. The selected fill would
        // erase the edge of a selected button; checkboxes, which also fill
        // with `accent`, swap the selected fill back in (`app::checkbox`).
        accent: rgb(p.border_focus),
        accent_soft: rgb(p.selected_bg),
        hover: rgb(p.hover_bg),
        selection: rgb(p.selected_bg),
        // 7px and 8px x-heights at 1x; `snap_fonts` keeps them whole at
        // other scales.
        body: TextStyle::new(FACE, 12.83),
        heading: TextStyle::new(FACE, 14.67),
        row_height: 27,
        padding: 8,
        corner_radius: 5,
        border_width: 1,
        focus_width: 1,
        ..Theme::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crisp_sizes_have_whole_pixel_x_heights() {
        let face = fonts().face(FACE);
        let ratio = 1536.0 / face.units_per_em();
        assert!((ratio - X_HEIGHT).abs() < 1e-6, "bundled face changed");
        for px in [11.0, 12.83, 14.67, 16.5, 19.25, 22.0] {
            let x = crisp(px) * X_HEIGHT;
            assert!((x - x.round()).abs() < 1e-3, "{px}: {x}");
            assert!(crisp_down(px) <= px + 1e-3);
        }
    }
}
