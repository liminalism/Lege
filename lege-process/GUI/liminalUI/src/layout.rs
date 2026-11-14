use liminalkit_raster::Rect;
use liminalkit_shell::Scale;

#[derive(Debug)]
pub struct Bands {
    pub files: Rect,
    pub left: Rect,
    pub right: Rect,
    pub status: Rect,
}

impl Bands {
    pub fn new(width: i32, height: i32, s: Scale) -> Self {
        let p = s.px(8);
        let gap = s.px(8);
        // Between the rows of cards; the columns keep the narrower `gap`.
        let row_gap = s.px(10);
        let w = (width - p * 2).max(1);
        let files = Rect::new(p, p, w, s.px(48));
        // Two equal settings columns expand with the window. No aspect-ratio
        // lock or maximum size; surplus height goes to status/queue information.
        let y = files.bottom() + row_gap;
        let settings_h = s.px(252);
        let col = (w - gap) / 2;
        let left = Rect::new(p, y, col, settings_h);
        let right = Rect::new(p + col + gap, y, w - col - gap, settings_h);
        // The process button sits inside the status card's bottom edge.
        let status_y = left.bottom() + row_gap;
        let status = Rect::new(p, status_y, w, (height - status_y - p).max(1));
        Self {
            files,
            left,
            right,
            status,
        }
    }
}

/// The page card, on the right band: page range, resolution and the
/// Flatten page toggle in one row, the margin buttons under them, then the
/// checkbox grid and the OCR slot.
pub struct PageCard {
    pub range_label: Rect,
    pub range: Rect,
    pub resolution_label: Rect,
    pub resolution: Rect,
    /// Beside the resolution field, as tall as it.
    pub flatten: Rect,
    /// Center margins / Crop margins / Reflow.
    pub geometry: Rect,
    pub checks: Rect,
    /// Height of one checkbox row in `checks`; two rows fit.
    pub check_row: i32,
    pub ocr: Rect,
}

impl PageCard {
    pub fn new(a: Rect, s: Scale) -> Self {
        // Wide enough for a range like "1-12, 40-9999", and no wider.
        let range_w = s.px(140).min(a.w / 2);
        let resolution_w = s.px(80);
        let resolution_x = a.x + range_w + s.px(12);
        let flatten_x = resolution_x + resolution_w + s.px(12);
        let label_h = s.px(15);
        let field_y = a.y + s.px(16);
        let field_h = s.px(28);
        let geometry = Rect::new(a.x, field_y + s.px(36), a.w, s.px(30));
        let check_row = s.px(24);
        let checks = Rect::new(a.x, geometry.bottom() + s.px(8), a.w, check_row * 2);
        Self {
            range_label: Rect::new(a.x, a.y, range_w, label_h),
            range: Rect::new(a.x, field_y, range_w, field_h),
            resolution_label: Rect::new(resolution_x, a.y, resolution_w, label_h),
            resolution: Rect::new(resolution_x, field_y, resolution_w, field_h),
            flatten: Rect::new(flatten_x, field_y, (a.right() - flatten_x).max(1), field_h),
            geometry,
            checks,
            check_row,
            ocr: Rect::new(a.x, checks.bottom() + s.px(6), a.w, s.px(30)),
        }
    }
}

/// The processing card's caption column: fits "Kodierung" and "Modus" at
/// the body size.
const LABEL_W: i32 = 84;

/// Setting rows of the processing card: mode, format, text, layout, images
/// and text encoding. Below the last, in the control column, a JBIG2
/// sub-option (symbol mode) of `SUB_OPTION_H`.
pub const SETTING_ROWS: i32 = 6;
const SUB_OPTION_H: i32 = 24;

/// The processing card, on the left band.
pub struct ProcessingCard {
    pub rows: Rect,
    scale: Scale,
}

impl ProcessingCard {
    pub fn new(a: Rect, s: Scale) -> Self {
        Self {
            rows: Rect::new(a.x, a.y, a.w, s.px(SETTING_ROWS * 32 + SUB_OPTION_H)),
            scale: s,
        }
    }
    pub fn setting(&self, index: i32) -> (Rect, Rect) {
        let y = self.rows.y + self.scale.px(index * 32);
        let label_w = self.scale.px(LABEL_W);
        (
            Rect::new(self.rows.x, y, label_w, self.scale.px(30)),
            Rect::new(
                self.rows.x + label_w + self.scale.px(8),
                y,
                self.rows.w - label_w - self.scale.px(8),
                self.scale.px(30),
            ),
        )
    }
    /// The symbol-mode checkbox: under the text-encoding row, in its control
    /// column.
    pub fn sub_option(&self) -> Rect {
        let (_, control) = self.setting(SETTING_ROWS - 1);
        Rect::new(
            control.x,
            self.rows.y + self.scale.px(SETTING_ROWS * 32),
            control.w,
            self.scale.px(SUB_OPTION_H),
        )
    }
}

pub fn cells(area: Rect, count: usize, gap: i32) -> Vec<Rect> {
    let width = (area.w - gap * (count as i32 - 1)) / count as i32;
    (0..count)
        .map(|i| Rect::new(area.x + i as i32 * (width + gap), area.y, width, area.h))
        .collect()
}

pub struct Actions {
    pub primary: Rect,
    pub utilities: Vec<Rect>,
    pub queue: Rect,
    /// Left of the process button: the input files' tiles.
    pub inputs: Rect,
    /// Right of the process button: the output files' tiles.
    pub outputs: Rect,
    /// The row above the tiles: the one progress bar while running, with
    /// the progress mode and the percentage drawn on it.
    pub meter: Rect,
    /// Between the header row and the queue: a control's tooltip while idle.
    pub tooltip: Rect,
}
impl Actions {
    /// `status` is the status section's content area.
    pub fn new(status: Rect, s: Scale) -> Self {
        let primary_w = s.px(320).min(status.w);
        let primary_h = s.px(44);
        // The bottom row is as tall as a file tile: inputs at its left, the
        // process button centred in it, outputs at its right.
        let tiles_h = s.px(crate::file_tiles::TILE_H).max(primary_h);
        let tiles_y = status.bottom() - tiles_h;
        let primary = Rect::new(
            status.x + (status.w - primary_w) / 2,
            tiles_y + (tiles_h - primary_h) / 2,
            primary_w,
            primary_h,
        );
        let side_gap = s.px(12);
        let inputs = Rect::new(
            status.x,
            tiles_y,
            (primary.x - side_gap - status.x).max(0),
            tiles_h,
        );
        let outputs_x = primary.right() + side_gap;
        let outputs = Rect::new(
            outputs_x,
            tiles_y,
            (status.right() - outputs_x).max(0),
            tiles_h,
        );
        let meter = Rect::new(status.x, tiles_y - s.px(30), status.w, s.px(22));
        let group_w = s.px(228).min(status.w);
        let utilities = cells(
            Rect::new(status.right() - group_w, status.y, group_w, s.px(24)),
            3,
            s.px(6),
        );
        // Above the queue: the header row and the tooltip band. Below it:
        // the progress bar, then the tiles and the button.
        let queue_y = status.y + s.px(78);
        let queue = Rect::new(
            status.x,
            queue_y,
            status.w,
            (meter.y - s.px(6) - queue_y).max(1),
        );
        let tooltip_y = status.y + s.px(24);
        let tooltip = Rect::new(status.x, tooltip_y, status.w, (queue_y - tooltip_y).max(1));
        Self {
            primary,
            utilities,
            queue,
            inputs,
            outputs,
            meter,
            tooltip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bands_fit_the_minimum_and_grow_without_an_aspect_lock() {
        for (w, h) in [(620, 620), (940, 700), (1400, 700), (800, 1200)] {
            for factor in [1.0, 1.5, 2.0] {
                let s = Scale(factor);
                let b = Bands::new(s.px(w), s.px(h), s);
                assert!(b.left.right() < b.right.x);
                assert_eq!(b.left.y - b.files.bottom(), s.px(10));
                assert_eq!(b.status.y - b.left.bottom(), s.px(10));
                assert_eq!(b.left.bottom(), b.right.bottom());
                assert!(b.status.h >= s.px(40));
                assert!(b.status.bottom() <= s.px(h));
            }
        }
    }
    #[test]
    fn controls_fit_their_panels() {
        for width in [720, 940, 1400] {
            let s = Scale(1.0);
            let bands = Bands::new(width, 700, s);
            let page = PageCard::new(bands.right.inset(8), s);
            // Range, resolution and Flatten page share one row.
            assert!(page.range.right() < page.resolution.x);
            assert!(page.resolution.right() < page.flatten.x);
            assert_eq!(page.flatten.y, page.resolution.y);
            assert!(page.flatten.right() <= page.checks.right());
            assert!(
                page.flatten.w >= 72,
                "flatten {} at {width}",
                page.flatten.w
            );
            // Then the margin buttons, the checkboxes and the OCR slot.
            assert!(page.resolution.bottom() <= page.geometry.y);
            assert!(page.geometry.bottom() <= page.checks.y);
            assert_eq!(page.checks.h, page.check_row * 2);
            assert!(page.checks.bottom() <= page.ocr.y);
            assert!(page.ocr.bottom() <= bands.right.inset(8).bottom());
            let processing = ProcessingCard::new(bands.left.inset(8), s);
            let last = processing.setting(SETTING_ROWS - 1).1;
            assert!(last.bottom() <= processing.sub_option().y);
            assert_eq!(processing.sub_option().x, last.x);
            assert!(processing.sub_option().bottom() <= processing.rows.bottom());
            assert!(processing.rows.bottom() <= bands.left.inset(8).bottom());
            let actions = Actions::new(bands.status.inset(8), s);
            assert!(actions.queue.bottom() <= actions.meter.y);
            assert!(actions.meter.bottom() <= actions.primary.y);
            assert!(actions.primary.bottom() <= bands.status.bottom());
        }
    }
    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
    }
    #[test]
    fn the_file_tiles_flank_the_process_button_clear_of_every_text_row() {
        for factor in [1.0, 1.5, 2.0] {
            let s = Scale(factor);
            for (w, h) in [(720, 660), (1040, 760), (1600, 1000)] {
                let bands = Bands::new(s.px(w), s.px(h), s);
                let status = bands.status.inset(s.px(8));
                let a = Actions::new(status, s);
                // Inputs left of the button, outputs right of it, on its row.
                assert!(a.inputs.right() < a.primary.x && a.primary.right() < a.outputs.x);
                assert_eq!(a.inputs.x, status.x);
                assert_eq!(a.outputs.right(), status.right());
                assert_eq!(a.inputs.bottom(), status.bottom());
                assert_eq!(a.outputs.bottom(), status.bottom());
                assert!(a.primary.y >= a.inputs.y && a.primary.bottom() <= a.inputs.bottom());
                // Room for at least one tile on each side.
                assert!(
                    a.inputs.w >= s.px(crate::file_tiles::TILE_W),
                    "{w}x{h}@{factor}"
                );
                assert!(
                    a.outputs.w >= s.px(crate::file_tiles::TILE_W),
                    "{w}x{h}@{factor}"
                );
                // No text row reaches the tiles.
                for text in [a.queue, a.meter, a.utilities[0], a.utilities[2]] {
                    assert!(
                        !overlaps(text, a.inputs) && !overlaps(text, a.outputs),
                        "{text:?}"
                    );
                }
                assert!(a.queue.bottom() <= a.meter.y && a.meter.bottom() <= a.inputs.y);
                // The queue keeps at least the four rows it had at the old minimum.
                if (w, h) == (720, 660) {
                    assert!(a.queue.h >= s.px(20) * 4, "queue {} at minimum", a.queue.h);
                }
            }
        }
    }
}
