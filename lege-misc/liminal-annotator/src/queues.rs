//! The queue picker (Q): which pages to review next. Each queue is a
//! condition on the per-page counts ([`Queue`]); the picker shows how many
//! pages each holds in the current book (or all books) so the work with the
//! most payoff — picture pages — comes first, and the text pages, which
//! almost never need an edit, are only sampled.

use liminalkit_raster::{Painter, Rect};
use liminalkit_shell::{KeyInput, Scale};
use liminalkit_text::{Align, TextCache};
use liminalkit_windowing::KeyEvent;

use crate::canvas::draw::outline;
use crate::dataset::database::Queue;
use crate::ui::{self, ACCENT, HUD_DIM, HUD_EDGE, HUD_TEXT, PANEL_BG, ROW_SELECTED};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueAction {
    None,
    Redraw,
    Pick(Queue),
    Close,
}

pub struct QueuePicker {
    /// Each queue with its page count in the scope.
    pub entries: Vec<(Queue, i64)>,
    pub selected: usize,
    /// The queue showing now (`None`: all pages / the command line's).
    pub current: Option<Queue>,
    /// "Book X" or "All books".
    pub scope: String,
    /// The comparison model's name, for the "only the model sees" row.
    pub model: String,
    /// The share of the page the large-picture queue asks for.
    pub large_share: f64,
    hits: Vec<(Rect, usize)>,
    close: Option<Rect>,
    panel: Option<Rect>,
}

impl QueuePicker {
    pub fn new(
        entries: Vec<(Queue, i64)>,
        current: Option<Queue>,
        scope: String,
        model: String,
    ) -> QueuePicker {
        let selected = current
            .and_then(|q| entries.iter().position(|(e, _)| *e == q))
            .unwrap_or(0);
        QueuePicker {
            entries,
            selected,
            current,
            scope,
            model,
            large_share: crate::dataset::database::DEFAULT_LARGE_FIGURE_SHARE,
            hits: Vec::new(),
            close: None,
            panel: None,
        }
    }

    fn label(&self, q: Queue) -> String {
        if q == Queue::ModelOnlyPictures && !self.model.is_empty() {
            return format!("Pictures only {} sees, to do", self.model);
        }
        q.title(self.large_share)
    }

    /// The key that opens row `i`: 1-9, 0, then A for the eleventh.
    fn row_key(i: usize) -> Option<String> {
        match i {
            0..=8 => Some((i + 1).to_string()),
            9 => Some("0".into()),
            10 => Some("A".into()),
            _ => None,
        }
    }

    pub fn key(&mut self, ev: &KeyEvent) -> QueueAction {
        if !ev.pressed {
            return QueueAction::None;
        }
        let n = self.entries.len();
        if n == 0 {
            return QueueAction::Close;
        }
        // Digits by physical key, so they work on any layout: 1..9, then 0.
        let digit = ev
            .physical
            .strip_prefix("Digit")
            .or_else(|| ev.physical.strip_prefix("Numpad"))
            .and_then(|d| d.parse::<usize>().ok());
        if let Some(d) = digit {
            let i = if d == 0 { 9 } else { d - 1 };
            if i < n {
                return QueueAction::Pick(self.entries[i].0);
            }
            return QueueAction::None;
        }
        if ev.physical == "KeyA" && n > 10 {
            return QueueAction::Pick(self.entries[10].0);
        }
        match ev.key {
            KeyInput::Escape => QueueAction::Close,
            KeyInput::Character(c) if c.eq_ignore_ascii_case(&'q') => QueueAction::Close,
            KeyInput::Up => {
                self.selected = (self.selected + n - 1) % n;
                QueueAction::Redraw
            }
            KeyInput::Down | KeyInput::Tab => {
                self.selected = (self.selected + 1) % n;
                QueueAction::Redraw
            }
            KeyInput::Home => {
                self.selected = 0;
                QueueAction::Redraw
            }
            KeyInput::End => {
                self.selected = n - 1;
                QueueAction::Redraw
            }
            KeyInput::Enter => QueueAction::Pick(self.entries[self.selected].0),
            _ => QueueAction::None,
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.panel.is_some_and(|r| r.contains(x, y))
    }

    pub fn click(&mut self, x: i32, y: i32) -> QueueAction {
        if self.close.is_some_and(|r| r.contains(x, y)) {
            return QueueAction::Close;
        }
        match self.hits.iter().find(|(r, _)| r.contains(x, y)) {
            Some((_, i)) => QueueAction::Pick(self.entries[*i].0),
            None => QueueAction::Redraw,
        }
    }

    pub fn draw(&mut self, p: &mut Painter<'_>, text: &mut TextCache, s: Scale, area: Rect) {
        self.hits.clear();
        let title = ui::style(s, 15.0);
        let body = ui::style(s, 13.0);
        let small = ui::style(s, 11.5);
        let lh = text.line_height(body);
        let sl = text.line_height(small);
        let row_h = lh + sl + s.px(10);
        let pw = s.px(720).min(area.w - s.px(24));
        let ph = text.line_height(title)
            + s.px(40)
            + sl
            + self.entries.len() as i32 * row_h
            + sl
            + s.px(16);
        let panel = Rect::new(
            area.x + (area.w - pw) / 2,
            area.y + s.px(24),
            pw,
            ph.min(area.h - s.px(32)),
        );
        self.panel = Some(panel);
        p.fill_rect(panel, PANEL_BG);
        outline(p, panel, ACCENT, 1);
        p.push_clip(panel);
        let x = panel.x + s.px(16);
        let inner = panel.w - s.px(32);
        let mut y = panel.y + s.px(12);
        text.draw(p, "Review queue", x, y, title, HUD_TEXT);
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
        self.close = Some(cl);
        y += text.line_height(title) + s.px(6);
        let sub = format!(
            "{}. Pages to do with a picture are where the edits are; text pages rarely need one.",
            self.scope
        );
        ui::cell(p, text, &sub, x, y, inner, sl, small, HUD_DIM, Align::Left);
        y += sl + s.px(10);

        let num_w = text.measure("10", body) + s.px(10);
        let count_w = text.measure("000,000", body);
        for (i, (q, n)) in self.entries.clone().into_iter().enumerate() {
            let r = Rect::new(panel.x + s.px(6), y, panel.w - s.px(12), row_h);
            if i == self.selected {
                p.fill_rect(r, ROW_SELECTED);
            }
            if Some(q) == self.current {
                p.fill_rect(Rect::new(r.x, r.y, s.px(3), r.h), ACCENT);
            }
            self.hits.push((r, i));
            let key = Self::row_key(i).unwrap_or_default();
            let ty = y + s.px(4);
            text.draw(p, &key, x, ty, body, HUD_DIM);
            let color = if n == 0 { HUD_DIM } else { HUD_TEXT };
            ui::cell(
                p,
                text,
                &self.label(q),
                x + num_w,
                ty,
                inner - num_w - count_w - s.px(12),
                lh,
                body,
                color,
                Align::Left,
            );
            ui::cell(
                p,
                text,
                &ui::thousands(n),
                x + inner - count_w,
                ty,
                count_w,
                lh,
                body,
                color,
                Align::Right,
            );
            ui::cell(
                p,
                text,
                q.help(),
                x + num_w,
                ty + lh + s.px(1),
                inner - num_w,
                sl,
                small,
                HUD_DIM,
                Align::Left,
            );
            y += row_h;
        }
        y += s.px(6);
        ui::cell(
            p,
            text,
            "1-9, 0, A or click: open that queue · Up/Down, Enter · Q or Esc: close",
            x,
            y,
            inner,
            sl,
            small,
            HUD_DIM,
            Align::Left,
        );
        p.pop_clip();
    }
}
