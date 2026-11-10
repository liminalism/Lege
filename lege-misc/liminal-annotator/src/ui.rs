//! Colours and small drawing helpers shared by the editor, the book manager,
//! the convert dialog and the start screen.

use liminalkit_raster::{Painter, Rect};
use liminalkit_shell::Scale;
use liminalkit_text::{Align, TextCache, TextStyle};

use crate::canvas::draw::outline;
use crate::fonts;

pub const HUD_BG: u32 = 0x0017_191c;
pub const PANEL_BG: u32 = 0x001d_2025;
pub const ROW_ALT: u32 = 0x001a_1d21;
pub const ROW_SELECTED: u32 = 0x0029_3340;
pub const HUD_EDGE: u32 = 0x002e_3238;
pub const HUD_TEXT: u32 = 0x00e6_e8eb;
pub const HUD_DIM: u32 = 0x0098_9ea6;
pub const ACCENT: u32 = 0x004d_9bff;
pub const WARN: u32 = 0x00ff_a040;
pub const BAD: u32 = 0x00ff_4a4a;
pub const GOOD: u32 = 0x0040_e070;

pub fn style(scale: Scale, logical: f32) -> TextStyle {
    TextStyle::new(fonts::FACE, logical * scale.0)
}

/// An outlined button with a centred label. Returns its rectangle.
#[allow(clippy::too_many_arguments)]
pub fn button(
    p: &mut Painter<'_>,
    text: &mut TextCache,
    scale: Scale,
    x: i32,
    y: i32,
    label: &str,
    st: TextStyle,
    edge: u32,
    fill: Option<u32>,
) -> Rect {
    let lh = text.line_height(st);
    let w = text.measure(label, st) + scale.px(20);
    let r = Rect::new(x, y, w, lh + scale.px(10));
    if let Some(f) = fill {
        p.fill_rect(r, f);
    }
    outline(p, r, edge, 1);
    text.draw(p, label, x + scale.px(10), y + scale.px(5), st, HUD_TEXT);
    r
}

/// Text cut to `w` pixels with an ellipsis, left-aligned at `(x, y)`.
#[allow(clippy::too_many_arguments)]
pub fn cell(
    p: &mut Painter<'_>,
    text: &mut TextCache,
    s: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    st: TextStyle,
    color: u32,
    align: Align,
) {
    if w <= 0 {
        return;
    }
    text.draw_fitted(p, s, Rect::new(x, y, w, h), y, st, color, align);
}

/// `12345` → `12,345`.
pub fn thousands(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A small filled triangle centred on `(cx, cy)`, pointing down or up (the
/// bundled font has no arrow glyphs).
pub fn triangle(p: &mut Painter<'_>, cx: i32, cy: i32, half: i32, down: bool, color: u32) {
    let h = half.max(2);
    for i in 0..h {
        // Row i from the wide end.
        let w = 2 * (h - i) - 1;
        let y = if down { cy - h / 2 + i } else { cy + h / 2 - i };
        p.fill_rect(Rect::new(cx - (h - i) + 1, y, w, 1), color);
    }
}

/// `1 box`, `1,954 boxes`.
pub fn count(n: i64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    #[test]
    fn thousands_groups_digits() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(1000), "1,000");
        assert_eq!(super::thousands(207432), "207,432");
        assert_eq!(super::thousands(-1234567), "-1,234,567");
    }
}
