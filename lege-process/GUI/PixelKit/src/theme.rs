use crate::appearance::{Palette, Rgb};
use pixelkit_shell::Scale;
use pixelkit_text::{Face, FaceId, FontSet, TextStyle};
use pixelkit_ui::Theme;
use std::sync::OnceLock;

pub const FACE: FaceId = FaceId(0);
pub fn fonts() -> &'static FontSet {
    static FONTS: OnceLock<FontSet> = OnceLock::new();
    FONTS.get_or_init(|| {
        let bytes =
            include_bytes!("../../../../freya-main/crates/freya-edit/tests/NotoSans-Regular.ttf");
        let face = Face::from_bytes(bytes).expect("bundled Noto Sans must parse");
        let mut fonts = FontSet::new();
        fonts.add(Box::leak(Box::new(face)));
        fonts
    })
}
pub fn rgb(c: Rgb) -> u32 {
    (u32::from(c.0) << 16) | (u32::from(c.1) << 8) | u32::from(c.2)
}

/// Keep the palette unscaled. These widgets consume surface dimensions raw;
/// convert those to physical pixels here. Fonts and focus_width are the two
/// tokens Ui scales internally, so they remain logical until consumption.
pub fn theme(p: Palette, scale: Scale) -> Theme {
    Theme {
        background: rgb(p.control_bg),
        panel: rgb(p.card_bg),
        panel_alt: rgb(p.control_bg),
        panel_edge: rgb(p.border),
        text: rgb(p.text),
        text_dim: rgb(p.muted),
        accent: rgb(p.selected_bg),
        accent_soft: rgb(p.selected_bg),
        hover: rgb(p.hover_bg),
        selection: rgb(p.selected_bg),
        body: TextStyle::new(FACE, 13.0),
        heading: TextStyle::new(FACE, 15.0),
        row_height: scale.px(27),
        padding: scale.px(8),
        corner_radius: scale.px(5),
        border_width: scale.hairline(),
        focus_width: 1,
        ..Theme::default()
    }
}
