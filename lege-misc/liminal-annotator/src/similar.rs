//! "Same on every page" (M): the teachers label one running head as a
//! heading on one page, a running head on the next and `abandon` on a third,
//! although it has the same size and position on every page. Select one such
//! box, press M, and every box of the book in the same place (heading,
//! running head, running foot, page number or `abandon`) is listed by its
//! current label; Enter makes them all the selected box's class, a class key
//! makes them that class. One logged change: U undoes it.

use liminalkit_raster::{Painter, Rect};
use liminalkit_shell::{KeyInput, Scale};
use liminalkit_text::{Align, TextCache};
use liminalkit_windowing::KeyEvent;

use crate::annotation::geometry::Box2;
use crate::annotation::labels::{NUM_LABELS, class};
use crate::canvas::draw::outline;
use crate::dataset::database::{SimilarBox, Tolerance};
use crate::ui::{self, ACCENT, HUD_DIM, HUD_EDGE, HUD_TEXT, PANEL_BG, ROW_SELECTED, WARN};

#[derive(Clone, Debug, PartialEq)]
pub enum SimilarAction {
    None,
    Redraw,
    /// Look again with another tolerance.
    Retolerance(Tolerance),
    /// Make every match the selected box's class.
    ApplyTemplateClass,
    Undo(i64),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Tol(Tolerance),
    Apply,
    Undo,
    Close,
}

pub struct SimilarDialog {
    pub book: String,
    pub template: Box2,
    pub template_class: u8,
    pub page_w: f64,
    pub page_h: f64,
    pub tolerance: Tolerance,
    pub matches: Vec<SimilarBox>,
    pub book_pages: i64,
    pub last_op: Option<(i64, String, i64)>,
    hits: Vec<(Rect, Hit)>,
    panel: Option<Rect>,
}

impl SimilarDialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        book: String,
        template: Box2,
        template_class: u8,
        page_w: f64,
        page_h: f64,
        matches: Vec<SimilarBox>,
        book_pages: i64,
        last_op: Option<(i64, String, i64)>,
    ) -> SimilarDialog {
        SimilarDialog {
            book,
            template,
            template_class,
            page_w,
            page_h,
            tolerance: Tolerance::Normal,
            matches,
            book_pages,
            last_op,
            hits: Vec::new(),
            panel: None,
        }
    }

    /// Matches by their current class, most first.
    pub fn by_class(&self) -> Vec<(u8, usize)> {
        let mut n = [0usize; NUM_LABELS];
        for m in &self.matches {
            n[(m.class_id as usize).min(NUM_LABELS - 1)] += 1;
        }
        let mut v: Vec<(u8, usize)> = n
            .iter()
            .enumerate()
            .filter(|(_, k)| **k > 0)
            .map(|(c, k)| (c as u8, *k))
            .collect();
        v.sort_by_key(|(_, k)| std::cmp::Reverse(*k));
        v
    }

    pub fn pages(&self) -> usize {
        let mut p: Vec<&str> = self.matches.iter().map(|m| m.page_key.as_str()).collect();
        p.sort_unstable();
        p.dedup();
        p.len()
    }

    /// Boxes that converting to `to` would change.
    pub fn changes_for(&self, to: u8) -> usize {
        self.matches.iter().filter(|m| m.class_id != to).count()
    }

    pub fn key(&mut self, ev: &KeyEvent) -> SimilarAction {
        if !ev.pressed {
            return SimilarAction::None;
        }
        let order = [Tolerance::Tight, Tolerance::Normal, Tolerance::Loose];
        let i = order.iter().position(|t| *t == self.tolerance).unwrap_or(1);
        match ev.key {
            KeyInput::Escape => SimilarAction::Close,
            KeyInput::Enter => SimilarAction::ApplyTemplateClass,
            KeyInput::Up | KeyInput::Left if i > 0 => SimilarAction::Retolerance(order[i - 1]),
            KeyInput::Down | KeyInput::Right if i < 2 => SimilarAction::Retolerance(order[i + 1]),
            KeyInput::Character(c) if c.eq_ignore_ascii_case(&'u') => match &self.last_op {
                Some((id, _, _)) => SimilarAction::Undo(*id),
                None => SimilarAction::None,
            },
            _ => SimilarAction::None,
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.panel.is_some_and(|r| r.contains(x, y))
    }

    pub fn click(&mut self, x: i32, y: i32) -> SimilarAction {
        match self
            .hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| *h)
        {
            Some(Hit::Tol(t)) => SimilarAction::Retolerance(t),
            Some(Hit::Apply) => SimilarAction::ApplyTemplateClass,
            Some(Hit::Undo) => match &self.last_op {
                Some((id, _, _)) => SimilarAction::Undo(*id),
                None => SimilarAction::None,
            },
            Some(Hit::Close) => SimilarAction::Close,
            None => SimilarAction::Redraw,
        }
    }

    pub fn draw(&mut self, p: &mut Painter<'_>, text: &mut TextCache, s: Scale, area: Rect) {
        self.hits.clear();
        let title = ui::style(s, 15.0);
        let body = ui::style(s, 13.0);
        let small = ui::style(s, 11.5);
        let lh = text.line_height(body);
        let sl = text.line_height(small);
        let pw = s.px(640).min(area.w - s.px(24));
        let ph = s.px(250) + 3 * sl;
        let panel = Rect::new(
            area.x + (area.w - pw) / 2,
            area.y + s.px(24),
            pw,
            ph.min(area.h - s.px(32)),
        );
        self.panel = Some(panel);
        let tc = class(self.template_class);
        p.fill_rect(panel, PANEL_BG);
        outline(p, panel, tc.color, 1);
        p.push_clip(panel);
        let x = panel.x + s.px(16);
        let inner = panel.w - s.px(32);
        let mut y = panel.y + s.px(12);
        text.draw(p, "Same on every page", x, y, title, HUD_TEXT);
        let esc_w = text.measure("Esc", small);
        let cl = ui::button(
            p,
            text,
            s,
            panel.right() - s.px(36) - esc_w,
            y - s.px(2),
            "Esc",
            small,
            HUD_EDGE,
            None,
        );
        self.hits.push((cl, Hit::Close));
        y += text.line_height(title) + s.px(8);

        let where_ = format!(
            "Like the selected {}: {:.0}% down the page, {:.0}×{:.0} px. Book {} ({} pages).",
            tc.name,
            (self.template.y0 + self.template.y1) / 2.0 / self.page_h * 100.0,
            self.template.width(),
            self.template.height(),
            self.book,
            ui::thousands(self.book_pages)
        );
        ui::cell(
            p,
            text,
            &where_,
            x,
            y,
            inner,
            lh,
            small,
            HUD_DIM,
            Align::Left,
        );
        y += sl + s.px(10);

        // Tolerance.
        let mut tx = x;
        tx += text.draw(p, "Match:", tx, y + s.px(5), small, HUD_DIM) + s.px(8);
        for t in [Tolerance::Tight, Tolerance::Normal, Tolerance::Loose] {
            let active = self.tolerance == t;
            let r = ui::button(
                p,
                text,
                s,
                tx,
                y,
                t.label(),
                small,
                if active { ACCENT } else { HUD_EDGE },
                active.then_some(ROW_SELECTED),
            );
            self.hits.push((r, Hit::Tol(t)));
            tx = r.right() + s.px(4);
        }
        text.draw(p, "Up/Down", tx + s.px(6), y + s.px(5), small, HUD_DIM);
        y += lh + s.px(18);

        // What was found.
        let found = format!(
            "{} on {} of the book's pages",
            ui::count(self.matches.len() as i64, "box", "boxes"),
            ui::thousands(self.pages() as i64)
        );
        text.draw(p, &found, x, y, body, HUD_TEXT);
        y += lh + s.px(6);
        let mut cx = x;
        for (c, n) in self.by_class() {
            let info = class(c);
            let label = format!("{} {}", info.short, ui::thousands(n as i64));
            let k = s.px(9);
            p.fill_rect(Rect::new(cx, y + (lh - k) / 2, k, k), info.color);
            cx += k + s.px(5);
            cx += text.draw(p, &label, cx, y, body, HUD_TEXT) + s.px(16);
            if cx > panel.right() - s.px(80) {
                break;
            }
        }
        y += lh + s.px(16);

        let n = self.changes_for(self.template_class);
        let apply = format!(
            "Make all {} (Enter): {} change",
            tc.short,
            ui::count(n as i64, "box", "boxes")
        );
        let r = ui::button(p, text, s, x, y, &apply, body, tc.color, Some(ROW_SELECTED));
        self.hits.push((r, Hit::Apply));
        y = r.bottom() + s.px(10);
        ui::cell(
            p,
            text,
            "Or press another class key (1–9, Shift+1–5) or click a class chip to make them all that class. Running heads that \
             alternate sides on facing pages match; a page number beside a running head does not (its width differs).",
            x,
            y,
            inner,
            lh,
            small,
            HUD_DIM,
            Align::Left,
        );
        y += sl + s.px(6);
        if let Some((_, desc, boxes)) = &self.last_op {
            let label = format!(
                "U: undo the last bulk change ({}: {desc})",
                ui::count(*boxes, "box", "boxes")
            );
            ui::cell(p, text, &label, x, y, inner, lh, small, WARN, Align::Left);
            self.hits.push((Rect::new(x, y, inner, sl), Hit::Undo));
        }
        p.pop_clip();
    }
}
