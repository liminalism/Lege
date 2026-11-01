use liminalkit_raster::Rect;
use liminalkit_shell::Scale;

#[derive(Debug)]
pub struct Bands {
    pub files: Rect,
    pub left: Rect,
    pub right: Rect,
    pub process: Rect,
    pub status: Rect,
}

impl Bands {
    pub fn new(width: i32, height: i32, s: Scale) -> Self {
        let p = s.px(8);
        let gap = s.px(8);
        let w = (width - p * 2).max(1);
        let files = Rect::new(p, p, w, s.px(48));
        // Two equal settings columns expand with the window. No aspect-ratio
        // lock or maximum size; surplus height goes to status/queue information.
        let y = files.bottom() + gap;
        let settings_h = s.px(280);
        let col = (w - gap) / 2;
        let left = Rect::new(p, y, col, settings_h);
        let right = Rect::new(p + col + gap, y, w - col - gap, settings_h);
        // Give all 110px recovered from settings to the queue beneath actions.
        let process = Rect::new(p, left.bottom() + gap, w, s.px(44));
        let status = Rect::new(
            p,
            process.bottom() + gap,
            w,
            (height - process.bottom() - gap - p).max(1),
        );
        Self {
            files,
            left,
            right,
            process,
            status,
        }
    }
}

pub struct LeftCard {
    pub heading: Rect,
    pub range_label: Rect,
    pub range: Rect,
    pub resolution_label: Rect,
    pub resolution: Rect,
    pub checks: Rect,
    pub ocr: Rect,
    pub geometry: Rect,
}

impl LeftCard {
    pub fn new(a: Rect, s: Scale) -> Self {
        let resolution_w = s.px(64);
        let field_y = a.y + s.px(40);
        let range_w = a.w - resolution_w - s.px(10);
        Self {
            heading: Rect::new(a.x, a.y, a.w, s.px(24)),
            range_label: Rect::new(a.x, a.y + s.px(25), range_w, s.px(15)),
            range: Rect::new(a.x, field_y, range_w, s.px(28)),
            resolution_label: Rect::new(
                a.right() - resolution_w,
                a.y + s.px(25),
                resolution_w,
                s.px(15),
            ),
            resolution: Rect::new(a.right() - resolution_w, field_y, resolution_w, s.px(28)),
            checks: Rect::new(a.x, field_y + s.px(34), a.w, s.px(75)),
            ocr: Rect::new(a.x, a.bottom() - s.px(74), a.w, s.px(30)),
            geometry: Rect::new(a.x, a.bottom() - s.px(34), a.w, s.px(30)),
        }
    }
}

pub struct RightCard {
    pub heading: Rect,
    pub rows: Rect,
    pub binarization: Rect,
    scale: Scale,
}

impl RightCard {
    pub fn new(a: Rect, s: Scale) -> Self {
        Self {
            heading: Rect::new(a.x, a.y, a.w, s.px(24)),
            rows: Rect::new(a.x, a.y + s.px(26), a.w, s.px(160)),
            binarization: Rect::new(a.x, a.bottom() - s.px(68), a.w, s.px(68)),
            scale: s,
        }
    }
    pub fn setting(&self, index: i32) -> (Rect, Rect) {
        let y = self.rows.y + self.scale.px(index * 32);
        let label_w = self.scale.px(64);
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
        let label_w = self.scale.px(64);
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
    pub fn new(process: Rect, status: Rect, s: Scale) -> Self {
        let primary_w = s.px(320).min(process.w);
        let primary = Rect::new(
            process.x + (process.w - primary_w) / 2,
            process.y,
            primary_w,
            process.h,
        );
        let group_w = s.px(228).min(status.w);
        let utilities = cells(
            Rect::new(status.right() - group_w, status.y, group_w, s.px(24)),
            3,
            s.px(6),
        );
        let queue = Rect::new(
            status.x,
            status.y + s.px(78),
            status.w,
            (status.h - s.px(108)).max(1),
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
            assert!(options.ocr.bottom() <= options.geometry.y);
            let output = RightCard::new(bands.left.inset(8), s);
            let (modes, caption, value) = output.binarization_fields();
            assert!(output.setting(4).1.bottom() <= modes.y);
            assert!(modes.bottom() <= caption.y && value.right() <= output.binarization.right());
            let actions = Actions::new(bands.process, bands.status.inset(8), s);
            assert!(actions.queue.bottom() <= bands.status.bottom());
        }
    }
}
