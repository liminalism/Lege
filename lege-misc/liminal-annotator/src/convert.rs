//! The bulk-convert dialog (Shift+A): turn `abandon` boxes into a real class
//! on this page, or across the whole book (the current queue's scope),
//! optionally only those in one region of the page (top band, bottom band,
//! side margins, large boxes, the rest). A class key does the conversion;
//! book-wide conversions are logged and `U` undoes the last one.

use liminalkit_raster::{Painter, Rect};
use liminalkit_shell::{KeyInput, Scale};
use liminalkit_text::{Align, TextCache};
use liminalkit_windowing::KeyEvent;

use crate::annotation::labels::{ABANDON_CLASS, class};
use crate::canvas::draw::outline;
use crate::dataset::database::Region;
use crate::ui::{self, HUD_DIM, HUD_EDGE, HUD_TEXT, PANEL_BG, ROW_SELECTED, WARN};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Page,
    /// Every page in the queue's scope (its book, set or split), whatever
    /// the page's review status.
    Queue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConvertAction {
    None,
    Redraw,
    Undo(i64),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Scope(Scope),
    Region(usize),
    Undo,
    Close,
}

pub struct ConvertDialog {
    pub scope: Scope,
    /// Index into [`Region::ALL`].
    pub region: usize,
    /// Per region: boxes on this page.
    pub page_counts: Vec<i64>,
    /// Per region: (boxes, pages) in the queue's scope.
    pub queue_counts: Vec<(i64, i64)>,
    /// What the queue scope is, for the label ("book=…").
    pub scope_label: String,
    /// The last book-wide conversion not undone: (id, description, boxes).
    pub last_op: Option<(i64, String, i64)>,
    hits: Vec<(Rect, Hit)>,
    panel: Option<Rect>,
}

impl ConvertDialog {
    pub fn new(
        page_counts: Vec<i64>,
        queue_counts: Vec<(i64, i64)>,
        scope_label: String,
        last_op: Option<(i64, String, i64)>,
    ) -> ConvertDialog {
        // Start where the work is: the page if it has abandon boxes.
        let scope = if page_counts.first().copied().unwrap_or(0) > 0 {
            Scope::Page
        } else {
            Scope::Queue
        };
        ConvertDialog {
            scope,
            region: 0,
            page_counts,
            queue_counts,
            scope_label,
            last_op,
            hits: Vec::new(),
            panel: None,
        }
    }

    pub fn region(&self) -> Region {
        Region::ALL[self.region.min(Region::ALL.len() - 1)]
    }

    /// Boxes the conversion would change now.
    pub fn selected_boxes(&self) -> i64 {
        match self.scope {
            Scope::Page => self.page_counts.get(self.region).copied().unwrap_or(0),
            Scope::Queue => self.queue_counts.get(self.region).map_or(0, |c| c.0),
        }
    }

    /// Navigation keys. Class keys are mapped by the caller (the editor's
    /// hotkey table) and come back as a conversion.
    pub fn key(&mut self, ev: &KeyEvent) -> ConvertAction {
        if !ev.pressed {
            return ConvertAction::None;
        }
        let n = Region::ALL.len();
        match ev.key {
            KeyInput::Up => {
                self.region = (self.region + n - 1) % n;
                ConvertAction::Redraw
            }
            KeyInput::Down => {
                self.region = (self.region + 1) % n;
                ConvertAction::Redraw
            }
            KeyInput::Tab | KeyInput::Left | KeyInput::Right => {
                self.scope = match self.scope {
                    Scope::Page => Scope::Queue,
                    Scope::Queue => Scope::Page,
                };
                ConvertAction::Redraw
            }
            KeyInput::Escape => ConvertAction::Close,
            KeyInput::Character(c) if c.eq_ignore_ascii_case(&'u') => match &self.last_op {
                Some((id, _, _)) => ConvertAction::Undo(*id),
                None => ConvertAction::None,
            },
            _ => ConvertAction::None,
        }
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<ConvertAction> {
        let hit = self
            .hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| *h)?;
        Some(match hit {
            Hit::Scope(s) => {
                self.scope = s;
                ConvertAction::Redraw
            }
            Hit::Region(i) => {
                self.region = i;
                ConvertAction::Redraw
            }
            Hit::Undo => match &self.last_op {
                Some((id, _, _)) => ConvertAction::Undo(*id),
                None => ConvertAction::None,
            },
            Hit::Close => ConvertAction::Close,
        })
    }

    /// Whether a point is on the dialog (clicks elsewhere go to the page).
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.panel.is_some_and(|r| r.contains(x, y))
    }

    pub fn draw(&mut self, p: &mut Painter<'_>, text: &mut TextCache, s: Scale, area: Rect) {
        self.hits.clear();
        let title = ui::style(s, 15.0);
        let body = ui::style(s, 13.0);
        let small = ui::style(s, 11.5);
        let lh = text.line_height(body);
        let row_h = lh + s.px(10);
        let pw = s.px(620).min(area.w - s.px(24));
        let ph = s.px(126) + row_h * Region::ALL.len() as i32 + 3 * text.line_height(small);
        let panel = Rect::new(
            area.x + (area.w - pw) / 2,
            area.y + s.px(24),
            pw,
            ph.min(area.h - s.px(32)),
        );
        self.panel = Some(panel);
        p.fill_rect(panel, PANEL_BG);
        outline(p, panel, ABANDON_CLASS.color, 1);
        p.push_clip(panel);
        let x = panel.x + s.px(16);
        let inner = panel.w - s.px(32);
        let mut y = panel.y + s.px(12);
        text.draw(p, "Convert abandon boxes", x, y, title, HUD_TEXT);
        // Close button.
        let esc_w = text.measure("Esc", small);
        let cl = ui::button(
            p,
            text,
            s,
            panel.right() - s.px(16) - esc_w - s.px(20),
            y - s.px(2),
            "Esc",
            small,
            HUD_EDGE,
            None,
        );
        self.hits.push((cl, Hit::Close));
        y += text.line_height(title) + s.px(10);

        // Scope tabs.
        let page_all = self.page_counts.first().copied().unwrap_or(0);
        let queue_all = self.queue_counts.first().map_or(0, |c| c.0);
        let tabs = [
            (Scope::Page, format!("This page ({page_all})")),
            (
                Scope::Queue,
                format!("{} ({})", self.scope_label, ui::thousands(queue_all)),
            ),
        ];
        let mut tx = x;
        for (sc, label) in tabs {
            let active = self.scope == sc;
            let r = ui::button(
                p,
                text,
                s,
                tx,
                y,
                &label,
                body,
                if active {
                    ABANDON_CLASS.color
                } else {
                    HUD_EDGE
                },
                active.then_some(ROW_SELECTED),
            );
            self.hits.push((r, Hit::Scope(sc)));
            tx = r.right() + s.px(8);
        }
        y += lh + s.px(20);

        // Regions.
        for (i, region) in Region::ALL.iter().enumerate() {
            let r = Rect::new(x, y, inner, row_h);
            if i == self.region {
                p.fill_rect(r, ROW_SELECTED);
                outline(p, r, ABANDON_CLASS.color, 1);
            }
            self.hits.push((r, Hit::Region(i)));
            let ty = y + (row_h - lh) / 2;
            text.draw(p, region.label(), x + s.px(10), ty, body, HUD_TEXT);
            let count = match self.scope {
                Scope::Page => {
                    let n = self.page_counts.get(i).copied().unwrap_or(0);
                    ui::count(n, "box", "boxes")
                }
                Scope::Queue => {
                    let (n, pages) = self.queue_counts.get(i).copied().unwrap_or((0, 0));
                    format!(
                        "{} on {}",
                        ui::count(n, "box", "boxes"),
                        ui::count(pages, "page", "pages")
                    )
                }
            };
            ui::cell(
                p,
                text,
                &count,
                x + inner / 2,
                ty,
                inner / 2 - s.px(10),
                row_h,
                body,
                HUD_DIM,
                Align::Right,
            );
            y += row_h;
        }
        y += s.px(12);
        let n = self.selected_boxes();
        let what = if n == 0 {
            "Nothing to convert here.".to_owned()
        } else {
            format!(
                "A class key (1–9, Shift+1–5) or class chip converts {} · Up/Down: region · Tab: scope",
                ui::count(n, "box", "boxes")
            )
        };
        ui::cell(
            p,
            text,
            &what,
            x,
            y,
            inner,
            lh,
            small,
            if n == 0 { HUD_DIM } else { HUD_TEXT },
            Align::Left,
        );
        y += text.line_height(small) + s.px(6);
        let note = match self.scope {
            Scope::Page => "One undo step (Ctrl+Z).".to_owned(),
            Scope::Queue => {
                "Every page in scope, any review status; logged, U undoes it.".to_owned()
            }
        };
        ui::cell(p, text, &note, x, y, inner, lh, small, HUD_DIM, Align::Left);
        y += text.line_height(small) + s.px(6);
        if let Some((_, desc, boxes)) = &self.last_op {
            let label = format!(
                "U: undo the last bulk conversion ({} boxes: {desc})",
                ui::thousands(*boxes)
            );
            let r = Rect::new(x, y, inner, text.line_height(small));
            ui::cell(p, text, &label, x, y, inner, lh, small, WARN, Align::Left);
            self.hits.push((r, Hit::Undo));
        }
        p.pop_clip();
    }
}

/// `abandon → caption` style summary of a conversion.
pub fn describe(from: u8, to: u8) -> String {
    format!("{} to {}", class(from).name, class(to).name)
}
