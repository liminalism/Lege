//! File tiles beside the process button: the inputs at the bottom-left, the
//! finished outputs at the bottom-right, one tile per kind of file.
//!
//! Files are grouped by kind: PDFs, then ZIP archives, then image folders
//! (then any other kind, such as a run's DjVu or EPUB outputs). A group's tile
//! is the shell icon of its first file (the icon Explorer shows for a `.pdf`,
//! a `.zip` or a folder), rendered at the tile's physical pixel size, with a
//! count badge on the icon once the group holds more than one file. Under the
//! icon is the file's name in at most two lines, or the group's size ("12
//! PDFs"). Press a tile and move the pointer to drag its files out of the
//! window, all together — to Explorer, the desktop or any application that
//! takes files; double-click to open a single file, or the folder holding a
//! group's first file. The tiles have the status card's bottom row to
//! themselves (`layout::Actions::inputs` and `outputs`), so a tile never covers
//! text.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use liminalkit_raster::{Fit, Path as Outline, PreparedImage, Rect};
use liminalkit_shell::Scale;
use liminalkit_text::{Align, TextStyle};
use liminalkit_ui::Ui;

use crate::gui_text::tr;

/// Logical size of a tile's icon, when the tile is wide enough for it.
pub const ICON: i32 = 56;
/// Logical width of a tile.
pub const TILE_W: i32 = 132;
/// Logical height of a tile: icon, then two lines of name.
pub const TILE_H: i32 = 104;
/// Narrowest logical tile width: below it, the last tile becomes a "+N".
/// Narrow enough that PDFs, ZIPs and folders all fit beside the process
/// button at the window's minimum width, with a smaller icon.
const MIN_TILE_W: i32 = 48;
/// Logical gap between tiles.
const TILE_GAP: i32 = 6;
/// Pointer travel, in logical pixels, that turns a press on a tile into a drag.
const DRAG_THRESHOLD: i32 = 4;
/// Two presses on one tile within this make a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// What files are grouped by, in the order their tiles appear.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Pdf,
    Zip,
    Folder,
    /// Any other file, by lower-case extension: a run's `.djvu` or `.epub`.
    Other(String),
}

impl Kind {
    pub fn of(path: &Path) -> Self {
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        // The extension first: it settles every queued file without asking
        // the file system, which this does for each file every frame.
        match extension.as_str() {
            "pdf" => Self::Pdf,
            "zip" => Self::Zip,
            _ if path.is_dir() => Self::Folder,
            _ => Self::Other(extension),
        }
    }
}

/// The files of one kind, in the order they were added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub kind: Kind,
    pub paths: Vec<PathBuf>,
}

/// `paths` grouped by kind, groups in [`Kind`] order.
pub fn group(paths: &[PathBuf]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for path in paths {
        let kind = Kind::of(path);
        match groups.iter_mut().find(|g| g.kind == kind) {
            Some(group) => group.paths.push(path.clone()),
            None => groups.push(Group {
                kind,
                paths: vec![path.clone()],
            }),
        }
    }
    groups.sort_by(|a, b| a.kind.cmp(&b.kind));
    groups
}

/// The caption of a tile holding more than one file: "12 PDFs".
pub fn group_label(kind: &Kind, count: usize) -> String {
    let (template, extension) = match kind {
        Kind::Pdf => (tr("tile_pdfs"), String::new()),
        Kind::Zip => (tr("tile_zips"), String::new()),
        Kind::Folder => (tr("tile_folders"), String::new()),
        Kind::Other(extension) => (tr("tile_files"), extension.to_uppercase()),
    };
    template
        .replace("{n}", &count.to_string())
        .replace("{ext}", &extension)
}

/// Where the tiles of one frame go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfLayout {
    /// The input area (left of the process button) and the output area (right).
    pub left: Rect,
    pub right: Rect,
    /// One rectangle per shown input tile, from the left edge rightwards.
    pub inputs: Vec<Rect>,
    /// The "+N" tile when the input tiles do not all fit, and how many tiles
    /// it stands for (the last ones).
    pub inputs_more: Option<(Rect, usize)>,
    /// One rectangle per shown output tile, from the right edge leftwards.
    pub outputs: Vec<Rect>,
    pub outputs_more: Option<(Rect, usize)>,
}

/// Lay out `inputs` tiles in `left`, from its left edge, and `outputs` in
/// `right`, from its right edge. Tiles narrow, down to [`MIN_TILE_W`], before
/// any is folded into a "+N" tile.
pub fn layout(left: Rect, right: Rect, s: Scale, inputs: usize, outputs: usize) -> ShelfLayout {
    let gap = s.px(TILE_GAP);
    let place = |area: Rect, count: usize, from_left: bool| {
        if count == 0 {
            return (Vec::new(), None);
        }
        let room = area.w.max(1);
        let fit = |n: usize| (room - gap * (n as i32 - 1)) / n as i32;
        let min_w = s.px(MIN_TILE_W).min(room);
        let slots = if fit(count) >= min_w {
            count
        } else {
            (((room + gap) / (min_w + gap)).max(1)) as usize
        };
        let tile_w = fit(slots).min(s.px(TILE_W)).max(1);
        let rect = |i: usize| {
            let offset = i as i32 * (tile_w + gap);
            if from_left {
                Rect::new(area.x + offset, area.y, tile_w, area.h)
            } else {
                Rect::new(area.right() - offset - tile_w, area.y, tile_w, area.h)
            }
        };
        if count <= slots {
            ((0..count).map(rect).collect::<Vec<_>>(), None)
        } else {
            let shown = slots - 1;
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

/// The icon's square in a tile: [`ICON`], or less in a narrow tile.
fn icon_rect(ui: &Ui<'_>, tile: Rect) -> Rect {
    let side = ui.px(ICON).min(tile.w - ui.px(4)).max(1);
    Rect::new(tile.x + (tile.w - side) / 2, tile.y + ui.px(6), side, side)
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
    /// Open the file (or folder) with its default application.
    Open(PathBuf),
}

/// What the pointer is doing over the shelf after a frame, for the window's
/// cursor: an open hand over a tile says it can be picked up and dragged out,
/// a closed hand while a press on one has not yet become the shell's drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShelfPointer {
    #[default]
    Elsewhere,
    OverTile,
    HoldingTile,
}

struct Press {
    paths: Vec<PathBuf>,
    x: i32,
    y: i32,
}

/// The shelf's state between frames.
#[derive(Default)]
pub struct Shelf {
    /// Shell icons by path and physical size. `None` when the shell has none,
    /// so it is not asked again every frame.
    icons: HashMap<(PathBuf, u32), Option<PreparedImage>>,
    /// The icons drawn this frame; the rest leave the cache after it.
    drawn: HashSet<(PathBuf, u32)>,
    press: Option<Press>,
    last_click: Option<(Vec<PathBuf>, Instant)>,
    /// A drag the window should start once this frame is done: the shell's
    /// drag loop is modal, and it must not run inside a paint.
    pub pending_drag: Option<Vec<PathBuf>>,
    /// The last files dragged out, and when: a drop of one of them back onto
    /// this window is the drag ending where it began, not a new input.
    pub recent_drag: Option<(Vec<PathBuf>, Instant)>,
    /// The pointer over the shelf as of the last frame drawn.
    pub pointer: ShelfPointer,
}

impl Shelf {
    /// Whether a dropped `path` is one this window just dragged out.
    pub fn is_own_drop(&self, path: &Path) -> bool {
        self.recent_drag.as_ref().is_some_and(|(dragged, at)| {
            dragged.iter().any(|p| p == path) && at.elapsed() < Duration::from_secs(3)
        })
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
        let inputs = group(inputs);
        let outputs = group(outputs);
        let plan = layout(left, right, ui.scale, inputs.len(), outputs.len());
        self.drawn.clear();
        self.pointer = ShelfPointer::Elsewhere;
        let mut action = None;
        for (rect, group) in plan
            .inputs
            .iter()
            .zip(&inputs)
            .chain(plan.outputs.iter().zip(&outputs))
        {
            if let Some(next) = self.tile(ui, *rect, group) {
                action = Some(next);
            }
        }
        for (more, groups) in [(plan.inputs_more, &inputs), (plan.outputs_more, &outputs)] {
            if let Some((rect, hidden)) = more {
                let files = groups[groups.len() - hidden..]
                    .iter()
                    .map(|g| g.paths.len())
                    .sum();
                more_tile(ui, rect, files);
            }
        }
        self.follow_press(ui);
        if self.press.is_some() {
            self.pointer = ShelfPointer::HoldingTile;
        }
        let drawn = &self.drawn;
        self.icons.retain(|key, _| drawn.contains(key));
        action
    }

    fn tile(&mut self, ui: &mut Ui<'_>, rect: Rect, group: &Group) -> Option<ShelfAction> {
        let pressed = self.press.as_ref().is_some_and(|p| p.paths == group.paths);
        let radius = ui.px(ui.theme.corner_radius);
        if pressed {
            let fill = ui.theme.selection;
            ui.fill_rounded(rect, radius, fill);
        } else if ui.input.hovering(rect) {
            let fill = ui.theme.hover;
            ui.fill_rounded(rect, radius, fill);
        }
        // Recorded whatever the press state: on the frame a press ends, the
        // tile still draws as pressed, and the hand must not drop to an arrow.
        if ui.input.hovering(rect) {
            self.pointer = ShelfPointer::OverTile;
        }
        let first = &group.paths[0];
        let icon_rect = icon_rect(ui, rect);
        self.icon(ui, icon_rect, first);
        let count = group.paths.len();
        if count > 1 {
            count_badge(ui, rect, icon_rect, count);
        }
        let style = caption_style(ui);
        let line_h = ui.text.line_height(style).max(1);
        let text_w = rect.w - ui.px(8);
        let lines = if count > 1 {
            let label = group_label(&group.kind, count);
            vec![ui.text.truncate(&label, text_w, style)]
        } else {
            let name = first
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| first.to_string_lossy().into_owned());
            name_lines(ui.text, style, text_w, &name)
        };
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
                .is_some_and(|(last, at)| *last == group.paths && at.elapsed() < DOUBLE_CLICK);
            if double {
                self.last_click = None;
                self.press = None;
                // One file opens; a group opens the folder its first file is in.
                let target = if count == 1 {
                    first.clone()
                } else {
                    first.parent().map_or_else(|| first.clone(), Path::to_owned)
                };
                return Some(ShelfAction::Open(target));
            }
            self.last_click = Some((group.paths.clone(), Instant::now()));
            self.press = Some(Press {
                paths: group.paths.clone(),
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
            self.pending_drag = self.press.take().map(|p| p.paths);
            self.last_click = None;
        }
    }

    fn icon(&mut self, ui: &mut Ui<'_>, area: Rect, path: &Path) {
        let size = area.w.max(1) as u32;
        let key = (path.to_owned(), size);
        self.drawn.insert(key.clone());
        let image = self.icons.entry(key).or_insert_with(|| {
            crate::shell_files::file_icon(path, size)
                .map(|bitmap| PreparedImage::from_bitmap(&bitmap, size, size, Fit::Contain, 0.0))
        });
        match image {
            Some(image) => ui.painter.draw_image(image, area.x, area.y),
            None => generic_icon(ui, area, path.is_dir()),
        }
    }
}

/// How many files a tile holds, as a pill on its icon's top-right corner:
/// text-coloured with the card's colour for the digits, so it reads on any
/// icon in either theme, and ringed in the card's colour to stand off it.
fn count_badge(ui: &mut Ui<'_>, tile: Rect, icon: Rect, count: usize) {
    let mut style = ui.theme.body;
    style.size = crate::theme::crisp_down(style.size * 0.85);
    let text = count.to_string();
    let h = ui.px(18);
    let w = (ui.text.measure(&text, style) + ui.px(10)).max(h);
    let x = (icon.right() + ui.px(6) - w)
        .min(tile.right() - w)
        .max(tile.x);
    let y = (icon.y - ui.px(3)).max(tile.y);
    let badge = Rect::new(x, y, w, h);
    let ring = ui.px(2).max(1);
    let (card, ink) = (ui.theme.panel, ui.theme.text);
    ui.fill_rounded(
        Rect::new(x - ring, y - ring, w + 2 * ring, h + 2 * ring),
        h / 2 + ring,
        card,
    );
    ui.fill_rounded(badge, h / 2, ink);
    ui.label_styled(badge, &text, style, card, Align::Centre);
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
    let badge = icon_rect(ui, rect);
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
    fn files_group_by_kind_pdfs_then_zips_then_folders() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("scans");
        std::fs::create_dir(&folder).unwrap();
        let p = |name: &str| dir.path().join(name);
        let added = vec![
            folder.clone(),
            p("b.zip"),
            p("one.pdf"),
            p("out.djvu"),
            p("C.ZIP"),
            p("two.PDF"),
        ];
        let groups = group(&added);
        let kinds: Vec<_> = groups.iter().map(|g| g.kind.clone()).collect();
        assert_eq!(
            kinds,
            [
                Kind::Pdf,
                Kind::Zip,
                Kind::Folder,
                Kind::Other("djvu".into())
            ]
        );
        // Within a group, the order the files were added in.
        assert_eq!(groups[0].paths, [p("one.pdf"), p("two.PDF")]);
        assert_eq!(groups[1].paths, [p("b.zip"), p("C.ZIP")]);
        assert_eq!(groups[2].paths, [folder]);
        assert!(group(&[]).is_empty());
    }

    #[test]
    fn a_group_is_captioned_by_its_size() {
        assert_eq!(group_label(&Kind::Pdf, 12), "12 PDFs");
        assert_eq!(group_label(&Kind::Zip, 3), "3 ZIP files");
        assert_eq!(group_label(&Kind::Folder, 2), "2 folders");
        assert_eq!(group_label(&Kind::Other("djvu".into()), 4), "4 DJVU files");
    }

    #[test]
    fn pdfs_zips_and_folders_all_fit_beside_the_button_at_every_window_size() {
        for factor in [1.0, 1.5, 2.0] {
            let s = Scale(factor);
            for (w, h) in [(720, 660), (1040, 760), (1600, 1000)] {
                let bands = crate::layout::Bands::new(s.px(w), s.px(h), s);
                let a = crate::layout::Actions::new(bands.status.inset(s.px(8)), s);
                let plan = layout(a.inputs, a.outputs, s, 3, 1);
                assert_eq!(plan.inputs.len(), 3, "{w}x{h}@{factor}: no +N");
                assert!(plan.inputs_more.is_none());
                for r in &plan.inputs {
                    assert!(r.w >= s.px(MIN_TILE_W) && r.w <= s.px(TILE_W));
                }
                if w >= 1040 {
                    // At the default size and up, three full-size icons.
                    assert!(plan.inputs[0].w >= s.px(ICON) + s.px(4), "{w}@{factor}");
                }
            }
        }
    }

    #[test]
    fn the_pointer_is_a_hand_over_a_tile_and_a_closed_hand_while_holding_it() {
        let mut buffer = liminalkit_raster::WindowBuffer::new(800, 300);
        let mut text = liminalkit_text::TextCache::new(crate::theme::fonts());
        let mut kernel = liminalkit_raster::RasterKernel::new();
        let mut input = liminalkit_shell::Input::new();
        let left = Rect::new(0, 100, 300, 110);
        let right = Rect::new(500, 100, 300, 110);
        let outputs = vec![PathBuf::from("Z:/no/such/book_out.pdf")];
        let mut shelf = Shelf::default();
        let mut frame = |shelf: &mut Shelf, input: &mut liminalkit_shell::Input| {
            let mut ui = Ui::new(
                liminalkit_raster::Painter::new(&mut buffer),
                &mut text,
                input,
                liminalkit_ui::Theme::default(),
                Scale(1.0),
                &mut kernel,
            );
            shelf.draw(&mut ui, left, right, &[], &outputs);
            ui.input.end_frame();
        };
        let tile = layout(left, right, Scale(1.0), 0, 1).outputs[0];
        let (cx, cy) = (tile.x + tile.w / 2, tile.y + tile.h / 2);
        // Away from the tiles: no hand.
        input.cursor_moved(10.0, 10.0);
        frame(&mut shelf, &mut input);
        assert_eq!(shelf.pointer, ShelfPointer::Elsewhere);
        // Over the finished output: an open hand.
        input.cursor_moved(cx as f32, cy as f32);
        frame(&mut shelf, &mut input);
        assert_eq!(shelf.pointer, ShelfPointer::OverTile);
        // Pressed on it: a closed hand until the drag begins or the button lifts.
        input.mouse(liminalkit_shell::MouseButton::Left, true);
        frame(&mut shelf, &mut input);
        frame(&mut shelf, &mut input);
        assert_eq!(shelf.pointer, ShelfPointer::HoldingTile);
        input.mouse(liminalkit_shell::MouseButton::Left, false);
        frame(&mut shelf, &mut input);
        assert_eq!(shelf.pointer, ShelfPointer::OverTile);
        input.cursor_left();
        frame(&mut shelf, &mut input);
        assert_eq!(shelf.pointer, ShelfPointer::Elsewhere);
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
