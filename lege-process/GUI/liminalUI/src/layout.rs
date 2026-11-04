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

pub struct LeftCard {
    pub range_label: Rect,
    pub range: Rect,
    pub resolution_label: Rect,
    pub resolution: Rect,
    pub checks: Rect,
    /// Height of one checkbox row in `checks`; four rows fit.
    pub check_row: i32,
    pub ocr: Rect,
    pub geometry: Rect,
}

impl LeftCard {
    pub fn new(a: Rect, s: Scale) -> Self {
        // Wide enough for a range like "1-12, 40-9999", and no wider.
        let range_w = s.px(140).min(a.w / 2);
        let resolution_w = s.px(80);
        let resolution_x = a.x + range_w + s.px(12);
        let label_h = s.px(15);
        let field_y = a.y + s.px(16);
        Self {
            range_label: Rect::new(a.x, a.y, range_w, label_h),
            range: Rect::new(a.x, field_y, range_w, s.px(28)),
            resolution_label: Rect::new(resolution_x, a.y, resolution_w, label_h),
            resolution: Rect::new(resolution_x, field_y, resolution_w, s.px(28)),
            checks: Rect::new(a.x, field_y + s.px(34), a.w, s.px(96)),
            check_row: s.px(24),
            ocr: Rect::new(a.x, a.bottom() - s.px(89), a.w, s.px(30)),
            geometry: Rect::new(a.x, a.bottom() - s.px(49), a.w, s.px(30)),
        }
    }
}

/// The Output card's caption column: fits "Binarisierung" at the small size.
const LABEL_W: i32 = 84;

pub struct RightCard {
    pub rows: Rect,
    pub binarization: Rect,
    scale: Scale,
}

impl RightCard {
    pub fn new(a: Rect, s: Scale) -> Self {
        Self {
            rows: Rect::new(a.x, a.y, a.w, s.px(160)),
            binarization: Rect::new(a.x, a.bottom() - s.px(68), a.w, s.px(68)),
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
    pub fn binarization_fields(&self) -> (Rect, Rect, Rect) {
        let a = self.binarization;
        let label_w = self.scale.px(LABEL_W);
        let modes = Rect::new(
            a.x + label_w + self.scale.px(8),
            a.y,
            a.w - label_w - self.scale.px(8),
            self.scale.px(30),
        );
        let caption = Rect::new(
            modes.x,
            a.y + self.scale.px(34),
            self.scale.px(48),
            self.scale.px(28),
        );
        let value = Rect::new(
            caption.right() + self.scale.px(4),
            caption.y,
            self.scale.px(64),
            caption.h,
        );
        (modes, caption, value)
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
}
impl Actions {
    /// `status` is the status section's content area.
    pub fn new(status: Rect, s: Scale) -> Self {
        let primary_w = s.px(320).min(status.w);
        let primary_h = s.px(44);
        let primary = Rect::new(
            status.x + (status.w - primary_w) / 2,
            status.bottom() - primary_h,
            primary_w,
            primary_h,
        );
        let group_w = s.px(228).min(status.w);
        let utilities = cells(
            Rect::new(status.right() - group_w, status.y, group_w, s.px(24)),
            3,
            s.px(6),
        );
        // Above the queue: up to three running-status lines. Below it: the
        // progress meter, then the button row.
        let queue_y = status.y + s.px(78);
        let queue = Rect::new(
            status.x,
            queue_y,
            status.w,
            (primary.y - s.px(30) - queue_y).max(1),
        );
        Self {
            primary,
            utilities,
            queue,
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
            let options = LeftCard::new(bands.right.inset(8), s);
            assert!(options.checks.bottom() <= options.ocr.y);
            assert_eq!(options.checks.h, options.check_row * 4);
            assert!(options.resolution.right() <= options.checks.right());
            assert!(options.ocr.bottom() <= options.geometry.y);
            let output = RightCard::new(bands.left.inset(8), s);
            let (modes, caption, value) = output.binarization_fields();
            assert!(output.setting(4).1.bottom() <= modes.y);
            assert!(modes.bottom() <= caption.y && value.right() <= output.binarization.right());
            let actions = Actions::new(bands.status.inset(8), s);
            assert!(actions.queue.bottom() <= actions.primary.y);
            assert!(actions.primary.bottom() <= bands.status.bottom());
            assert!(options.geometry.bottom() <= bands.right.bottom());
        }
    }
}
