//! File tiles beside the process button: every input as a tile at the
//! bottom-left, every finished output as a tile at the bottom-right.
//!
//! A tile is the file's own shell icon (the icon Explorer shows for a `.pdf`,
//! a `.djvu` or a folder), rendered at the tile's physical pixel size, with the
//! file name under it in at most two lines. Press a tile and move the pointer
//! to drag the file out of the window — to Explorer, the desktop or any
//! application that takes files; double-click to open it. The tiles have the
//! status card's bottom row to themselves (`layout::Actions::inputs` and
//! `outputs`), so a tile never covers text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use liminalkit_raster::{Fit, Path as Outline, PreparedImage, Rect};
use liminalkit_shell::Scale;
use liminalkit_text::{Align, TextStyle};
use liminalkit_ui::Ui;

/// Logical size of a tile's icon.
pub const ICON: i32 = 56;
/// Logical width of a tile.
pub const TILE_W: i32 = 132;
/// Logical height of a tile: icon, then two lines of name.
pub const TILE_H: i32 = 104;
/// Logical gap between tiles.
const TILE_GAP: i32 = 6;
/// Pointer travel, in logical pixels, that turns a press on a tile into a drag.
const DRAG_THRESHOLD: i32 = 4;
/// Two presses on one tile within this make a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// Where the tiles of one frame go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfLayout {
    /// The input area (left of the process button) and the output area (right).
    pub left: Rect,
    pub right: Rect,
    /// One rectangle per shown input, from the left edge rightwards.
    pub inputs: Vec<Rect>,
    /// The "+N" tile when the inputs do not all fit, and how many it stands for.
    pub inputs_more: Option<(Rect, usize)>,
    /// One rectangle per shown output, from the right edge leftwards.
    pub outputs: Vec<Rect>,
    pub outputs_more: Option<(Rect, usize)>,
}

/// Lay out `inputs` tiles in `left`, from its left edge, and `outputs` in
/// `right`, from its right edge.
pub fn layout(left: Rect, right: Rect, s: Scale, inputs: usize, outputs: usize) -> ShelfLayout {
    let gap = s.px(TILE_GAP);
    let place = |area: Rect, count: usize, from_left: bool| {
        let tile_w = s.px(TILE_W).min(area.w.max(1));
        let capacity = (((area.w + gap) / (tile_w + gap)).max(1)) as usize;
        let rect = |i: usize| {
            let offset = i as i32 * (tile_w + gap);
            if from_left {
                Rect::new(area.x + offset, area.y, tile_w, area.h)
            } else {
                Rect::new(area.right() - offset - tile_w, area.y, tile_w, area.h)
            }
        };
        if count <= capacity {
            ((0..count).map(rect).collect::<Vec<_>>(), None)
        } else {
            let shown = capacity - 1;
            (
                (0..shown).map(rect).collect(),
                Some((rect(shown), count - shown)),
            )
        }
    };
    let (inputs, inputs_more) = place(left, inputs, true);
    let (outputs, outputs_more) = place(right, outputs, false);
    ShelfLayout {
        left,
        right,
        inputs,
        inputs_more,
        outputs,
        outputs_more,
    }
}

/// A file's name in at most two lines of `width` pixels: the first as wrapped
/// (at a space when there is one), the second shortened with an ellipsis but
/// keeping the extension, which is the part that says what the file is.
pub fn name_lines(
    text: &mut liminalkit_text::TextCache,
    style: TextStyle,
    width: i32,
    name: &str,
) -> Vec<String> {
    if name.is_empty() || width <= 0 {
        return Vec::new();
    }
    if text.measure(name, style) <= width {
        return vec![name.to_owned()];
    }
    let lines = text.wrap(name, width, style);
    let Some(first) = lines.first().cloned() else {
        return vec![text.truncate(name, width, style)];
    };
    let head = name[first.clone()].trim_end().to_owned();
    let rest = name[first.end..].trim_start();
    if rest.is_empty() {
        return vec![head];
    }
    if text.measure(rest, style) <= width {
        return vec![head, rest.to_owned()];
    }
    // Keep ".pdf" whole; shorten what comes before it.
    let extension = Path::new(rest)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 8)
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let stem = &rest[..rest.len() - extension.len()];
    let room = width - text.measure(&extension, style);
    let tail = if room > 0 {
        format!("{}{extension}", text.truncate(stem, room, style))
    } else {
        text.truncate(rest, width, style)
    };
    vec![head, tail]
}

/// What a click on the shelf asks of the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShelfAction {
    /// Open the file with its default application.
    Open(PathBuf),
}

struct Press {
    path: PathBuf,
    x: i32,
    y: i32,
}

/// The shelf's state between frames.
#[derive(Default)]
pub struct Shelf {
    /// Shell icons by path and physical size. `None` when the shell has none,
    /// so it is not asked again every frame.
    icons: HashMap<(PathBuf, u32), Option<PreparedImage>>,
    press: Option<Press>,
    last_click: Option<(PathBuf, Instant)>,
    /// A drag the window should start once this frame is done: the shell's
    /// drag loop is modal, and it must not run inside a paint.
    pub pending_drag: Option<PathBuf>,
    /// The last file dragged out, and when: a drop of that file back onto
    /// this window is the drag ending where it began, not a new input.
    pub recent_drag: Option<(PathBuf, Instant)>,
}

impl Shelf {
    /// Whether a dropped `path` is the file this window just dragged out.
    pub fn is_own_drop(&self, path: &Path) -> bool {
        self.recent_drag
            .as_ref()
            .is_some_and(|(dragged, at)| dragged == path && at.elapsed() < Duration::from_secs(3))
    }

    /// Draw the input tiles in `left` and the output tiles in `right`. An
    /// empty side draws nothing at all.
    pub fn draw(
        &mut self,
        ui: &mut Ui<'_>,
        left: Rect,
        right: Rect,
        inputs: &[PathBuf],
        outputs: &[PathBuf],
    ) -> Option<ShelfAction> {
        let plan = layout(left, right, ui.scale, inputs.len(), outputs.len());
        let mut action = None;
        for (rect, path) in plan
            .inputs
            .iter()
            .zip(inputs)
            .chain(plan.outputs.iter().zip(outputs))
        {
            if let Some(next) = self.tile(ui, *rect, path) {
                action = Some(next);
            }
        }
        for (rect, more) in [plan.inputs_more, plan.outputs_more].into_iter().flatten() {
            more_tile(ui, rect, more);
        }
        self.follow_press(ui);
        action
    }

    fn tile(&mut self, ui: &mut Ui<'_>, rect: Rect, path: &Path) -> Option<ShelfAction> {
        let pressed = self.press.as_ref().is_some_and(|p| p.path == path);
        let radius = ui.px(ui.theme.corner_radius);
        if pressed {
            let fill = ui.theme.selection;
            ui.fill_rounded(rect, radius, fill);
        } else if ui.input.hovering(rect) {
            let fill = ui.theme.hover;
            ui.fill_rounded(rect, radius, fill);
        }
        let side = ui.px(ICON);
        let icon_rect = Rect::new(rect.x + (rect.w - side) / 2, rect.y + ui.px(6), side, side);
        self.icon(ui, icon_rect, path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let style = caption_style(ui);
        let line_h = ui.text.line_height(style).max(1);
        let text_w = rect.w - ui.px(8);
        let lines = name_lines(ui.text, style, text_w, &name);
        let mut y = icon_rect.bottom() + ui.px(4);
        let colour = ui.theme.text;
        for line in &lines {
            let line_rect = Rect::new(rect.x + ui.px(4), y, text_w, line_h);
            ui.label_styled(line_rect, line, style, colour, Align::Centre);
            y += line_h;
        }
        if ui.input.take_click(rect) {
            let (x, y) = ui.input.cursor().unwrap_or((rect.x, rect.y));
            let double = self
                .last_click
                .as_ref()
                .is_some_and(|(last, at)| last == path && at.elapsed() < DOUBLE_CLICK);
            if double {
                self.last_click = None;
                self.press = None;
                return Some(ShelfAction::Open(path.to_owned()));
            }
            self.last_click = Some((path.to_owned(), Instant::now()));
            self.press = Some(Press {
                path: path.to_owned(),
                x,
                y,
            });
        }
        None
    }

    /// A press on a tile becomes a drag once the pointer has moved, and is
    /// forgotten when the button comes up first.
    fn follow_press(&mut self, ui: &mut Ui<'_>) {
        let Some(press) = &self.press else {
            return;
        };
        if !ui.input.is_held() {
            self.press = None;
            return;
        }
        let threshold = ui.px(DRAG_THRESHOLD).max(2);
        if let Some((x, y)) = ui.input.cursor()
            && ((x - press.x).abs() > threshold || (y - press.y).abs() > threshold)
        {
            self.pending_drag = self.press.take().map(|p| p.path);
            self.last_click = None;
        }
    }

    fn icon(&mut self, ui: &mut Ui<'_>, area: Rect, path: &Path) {
        let size = area.w.max(1) as u32;
        let key = (path.to_owned(), size);
        let image = self.icons.entry(key).or_insert_with(|| {
            crate::shell_files::file_icon(path, size)
                .map(|bitmap| PreparedImage::from_bitmap(&bitmap, size, size, Fit::Contain, 0.0))
        });
        match image {
            Some(image) => ui.painter.draw_image(image, area.x, area.y),
            None => generic_icon(ui, area, path.is_dir()),
        }
    }

    /// Drop cached icons for files no longer on the shelf (and every size but
    /// the current one), so the cache stays the size of the shelf.
    pub fn retain_icons(&mut self, shown: &[PathBuf], size: u32) {
        self.icons
            .retain(|(path, s), _| *s == size && shown.iter().any(|p| p == path));
    }
}

fn caption_style(ui: &Ui<'_>) -> TextStyle {
    let mut style = ui.theme.body;
    style.size = crate::theme::crisp(style.size * 0.93);
    style
}

fn more_tile(ui: &mut Ui<'_>, rect: Rect, count: usize) {
    let radius = ui.px(ui.theme.corner_radius);
    let (fill, edge) = (ui.theme.panel_alt, ui.theme.panel_edge);
    let border = ui.px(ui.theme.border_width);
    let side = ui.px(ICON);
    let badge = Rect::new(rect.x + (rect.w - side) / 2, rect.y + ui.px(6), side, side);
    ui.rounded(badge, radius, border, fill, edge);
    let mut style = ui.theme.heading;
    style.size = crate::theme::crisp(style.size * 1.2);
    let colour = ui.theme.text;
    ui.label_styled(badge, &format!("+{count}"), style, colour, Align::Centre);
}

/// A page with a folded corner, or a folder, for a file the shell has no icon
/// for (and on platforms without shell icons).
fn generic_icon(ui: &mut Ui<'_>, area: Rect, folder: bool) {
    let (x, y, s) = (area.x as f32, area.y as f32, area.w as f32);
    let line = ui.scale.f(1.5);
    let edge = ui.theme.panel_edge;
    let fill = ui.theme.panel_alt;
    let mut shape = Outline::new();
    if folder {
        let (top, tab_w, tab_h) = (y + s * 0.2, s * 0.4, s * 0.1);
        shape
            .move_to(x + s * 0.06, top)
            .line_to(x + s * 0.06, top - tab_h)
            .line_to(x + s * 0.06 + tab_w, top - tab_h)
            .line_to(x + s * 0.06 + tab_w + tab_h, top)
            .line_to(x + s * 0.94, top)
            .line_to(x + s * 0.94, y + s * 0.86)
            .line_to(x + s * 0.06, y + s * 0.86)
            .close();
    } else {
        let (left, right, top, bottom, fold) = (
            x + s * 0.2,
            x + s * 0.8,
            y + s * 0.06,
            y + s * 0.94,
            s * 0.2,
        );
        shape
            .move_to(left, top)
            .line_to(right - fold, top)
            .line_to(right, top + fold)
            .line_to(right, bottom)
            .line_to(left, bottom)
            .close();
    }
    ui.painter
        .fill_path(ui.kernel, &mut shape.clone(), edge, 255);
    // The same outline, shrunk by the line width, in the fill colour.
    let (cx, cy) = (x + s / 2.0, y + s / 2.0);
    let k = (s / 2.0 - line) / (s / 2.0);
    let mut inner = Outline::new();
    let (points, _) = shape.geometry();
    for (i, [px, py]) in points.iter().enumerate() {
        let (ix, iy) = (cx + (px - cx) * k, cy + (py - cy) * k);
        if i == 0 {
            inner.move_to(ix, iy);
        } else {
            inner.line_to(ix, iy);
        }
    }
    inner.close();
    ui.painter.fill_path(ui.kernel, &mut inner, fill, 255);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
    }

    fn inside(inner: Rect, outer: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn inputs_fill_from_the_left_and_outputs_from_the_right_without_overlap() {
        for factor in [1.0, 1.5, 2.0] {
            let s = Scale(factor);
            for width in [704, 1024, 1600] {
                let side = s.px((width - 344) / 2);
                let left = Rect::new(s.px(16), s.px(600), side, s.px(TILE_H));
                let right = Rect::new(left.right() + s.px(344), s.px(600), side, s.px(TILE_H));
                for (inputs, outputs) in [(0, 0), (1, 1), (3, 2), (40, 40)] {
                    let plan = layout(left, right, s, inputs, outputs);
                    assert!(plan.left.right() < plan.right.x, "the sides are apart");
                    let shown_in: Vec<Rect> = plan
                        .inputs
                        .iter()
                        .copied()
                        .chain(plan.inputs_more.map(|m| m.0))
                        .collect();
                    let shown_out: Vec<Rect> = plan
                        .outputs
                        .iter()
                        .copied()
                        .chain(plan.outputs_more.map(|m| m.0))
                        .collect();
                    for r in &shown_in {
                        assert!(inside(*r, plan.left), "{r:?} in the input half");
                    }
                    for r in &shown_out {
                        assert!(inside(*r, plan.right), "{r:?} in the output half");
                    }
                    let all: Vec<Rect> = shown_in.iter().chain(&shown_out).copied().collect();
                    for (i, a) in all.iter().enumerate() {
                        for b in &all[i + 1..] {
                            assert!(!overlaps(*a, *b), "{a:?} and {b:?}");
                        }
                    }
                    let counted = plan.inputs.len() + plan.inputs_more.map_or(0, |m| m.1);
                    assert_eq!(counted, inputs, "every input is shown or counted");
                    let counted = plan.outputs.len() + plan.outputs_more.map_or(0, |m| m.1);
                    assert_eq!(counted, outputs, "every output is shown or counted");
                    if let Some(first) = plan.inputs.first() {
                        assert_eq!(first.x, plan.left.x, "inputs start at the left edge");
                    }
                    if let Some(first) = plan.outputs.first() {
                        assert_eq!(
                            first.right(),
                            plan.right.right(),
                            "outputs start at the right edge"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_long_name_takes_two_lines_and_keeps_its_extension() {
        let mut text = liminalkit_text::TextCache::new(crate::theme::fonts());
        let style = TextStyle::new(crate::theme::FACE, 13.0);
        let width = 120;
        assert_eq!(
            name_lines(&mut text, style, width, "book.pdf"),
            vec!["book.pdf".to_owned()]
        );
        let lines = name_lines(
            &mut text,
            style,
            width,
            "The Buddha and the Sahibs - The Men Who Discovered India's Lost Religion.pdf",
        );
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with(".pdf"), "{lines:?}");
        for line in &lines {
            assert!(text.measure(line, style) <= width, "{line:?} fits");
        }
        let lines = name_lines(
            &mut text,
            style,
            width,
            "risefallofconfed01daviuoft_processed_2026-11-08.pdf",
        );
        assert!(lines.len() <= 2);
        for line in &lines {
            assert!(text.measure(line, style) <= width, "{line:?} fits");
        }
    }
}
