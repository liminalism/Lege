use pixelkit_raster::Rect;
use pixelkit_shell::Scale;

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
        let process = Rect::new(p, left.bottom() + gap, w, s.px(196));
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
            checks: Rect::new(a.x, field_y + s.px(40), a.w, s.px(100)),
            ocr: Rect::new(a.x, a.bottom() - s.px(78), a.w, s.px(30)),
            geometry: Rect::new(a.x, a.bottom() - s.px(40), a.w, s.px(40)),
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
            rows: Rect::new(a.x, a.y + s.px(26), a.w, s.px(166)),
            binarization: Rect::new(a.x, a.bottom() - s.px(64), a.w, s.px(64)),
            scale: s,
        }
    }
    pub fn setting(&self, index: i32) -> (Rect, Rect) {
        let y = self.rows.y + self.scale.px(index * 34);
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
        let a = self.binarization.inset(self.scale.px(6));
        let value = Rect::new(
            a.right() - self.scale.px(52),
            a.y + self.scale.px(16),
            self.scale.px(52),
            self.scale.px(30),
        );
        let caption = Rect::new(value.x, a.y, value.w, self.scale.px(14));
        let modes = Rect::new(a.x, value.y, a.w - value.w - self.scale.px(8), value.h);
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
    pub fn new(a: Rect, s: Scale) -> Self {
        let utility_w = s.px(72);
        let group_w = utility_w * 3 + s.px(6) * 2;
        let group_x = a.right() - group_w;
        let primary_w = s.px(320).min(group_x - a.x - s.px(24));
        let primary_x = (a.x + (a.w - primary_w) / 2).min(group_x - s.px(24) - primary_w);
        let primary = Rect::new(primary_x, a.y, primary_w, s.px(44));
        let utilities = cells(
            Rect::new(group_x, a.y + s.px(10), group_w, s.px(24)),
            3,
            s.px(6),
        );
        let queue = Rect::new(a.x, a.y + s.px(58), a.w, (a.h - s.px(58)).max(1));
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
    fn compact_groups_keep_equal_buttons_and_smaller_parameters() {
        for width in [620, 940, 1400] {
            for factor in [1.0, 1.5, 2.0] {
                let s = Scale(factor);
                let bands = Bands::new(s.px(width), s.px(700), s);
                assert_eq!(bands.left.h, s.px(280));
                assert_eq!(bands.process.h, s.px(196));
                let left = LeftCard::new(bands.left.inset(s.px(8)), s);
                assert_eq!(left.resolution.w, s.px(64));
                assert_eq!(left.range.y, left.resolution.y);
                assert!(left.range.right() < left.resolution.x);
                assert!(left.checks.bottom() <= left.ocr.y);
                assert!(left.ocr.bottom() < left.geometry.y);
                let margins = cells(left.geometry.inset(s.px(5)), 3, s.px(4));
                assert!(
                    margins
                        .iter()
                        .all(|rect| rect.w == margins[0].w && rect.h == margins[0].h)
                );
                let right = RightCard::new(bands.right.inset(s.px(8)), s);
                let (modes, _, value) = right.binarization_fields();
                let mode_buttons = cells(modes, 3, s.px(4));
                assert_eq!(value.w, s.px(52));
                assert!(mode_buttons.iter().all(|rect| rect.w == mode_buttons[0].w
                    && rect.h == mode_buttons[0].h
                    && rect.w > value.w));
                assert!(right.setting(4).1.bottom() < right.binarization.y);
                let actions = Actions::new(bands.process.inset(s.px(8)), s);
                assert!(actions.primary.right() + s.px(24) <= actions.utilities[0].x);
                assert!(actions.primary.h > actions.utilities[0].h);
                assert!(actions.utilities.iter().all(
                    |rect| rect.w == actions.utilities[0].w && rect.h == actions.utilities[0].h
                ));
                assert!(actions.queue.h >= s.px(120));
            }
        }
    }
}
