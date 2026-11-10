//! The book manager: every book (image folder) of the project as a table,
//! one row per book, with review progress, the boxes of every label, the
//! paper tone and YOLO `abandon` counts. Click a column header (or Tab) to
//! sort by it, again (or F4) to reverse; type to filter by name. Choosing a
//! book makes it the editor's queue.

use std::time::{Duration, Instant};

use liminalkit_raster::{Painter, Rect};
use liminalkit_shell::{KeyInput, Scale};
use liminalkit_text::{Align, TextCache, TextStyle};
use liminalkit_windowing::KeyEvent;

use crate::annotation::labels::{ABANDON, ABANDON_CLASS, class};
use crate::canvas::draw::outline;
use crate::dataset::database::{BookStats, Tone};
use crate::ui::{self, ACCENT, HUD_BG, HUD_DIM, HUD_EDGE, HUD_TEXT, ROW_ALT, ROW_SELECTED, WARN};

/// A table column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    /// Import order.
    Order,
    Name,
    /// Ticked: the book goes into training exports.
    Train,
    /// Marked as needing significant work.
    NeedsWork,
    Split,
    Pages,
    Reviewed,
    /// Pages not yet reviewed.
    ToDo,
    /// Pages with a picture (a figure box or a model figure).
    Pictures,
    /// Of those, still to do.
    PicturesToDo,
    Attention,
    /// Paper yellowness (CIE b*).
    Paper,
    /// Boxes of one class (`ABANDON` included), shown per [`Measure`].
    Class(u8),
    /// `abandon` boxes covering 4% of the page or more.
    LargeAbandon,
}

/// Label columns in display order: pictures first, then text, furniture,
/// the rare classes, and `abandon`.
const CLASS_ORDER: [u8; 15] = [6, 7, 8, 13, 0, 1, 2, 3, 4, 5, 9, 10, 11, 12, ABANDON];

/// Every column, left to right. The first two stay put when the table
/// scrolls sideways.
fn columns() -> Vec<Col> {
    let mut v = vec![
        Col::Order,
        Col::Name,
        Col::Train,
        Col::NeedsWork,
        Col::Split,
        Col::Pages,
        Col::Reviewed,
        Col::ToDo,
        Col::Pictures,
        Col::PicturesToDo,
        Col::Attention,
        Col::Paper,
    ];
    v.extend(CLASS_ORDER.iter().map(|&c| Col::Class(c)));
    v.push(Col::LargeAbandon);
    v
}

const FROZEN: usize = 2;

/// Which books the table lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    All,
    /// Not excluded from training.
    Training,
    Excluded,
    NeedsWork,
    /// Many unconverted `abandon` boxes per page.
    AbandonFlagged,
}

impl Show {
    const ALL: [Show; 5] = [
        Show::All,
        Show::Training,
        Show::Excluded,
        Show::NeedsWork,
        Show::AbandonFlagged,
    ];

    fn label(self) -> &'static str {
        match self {
            Show::All => "All",
            Show::Training => "For training",
            Show::Excluded => "Excluded",
            Show::NeedsWork => "Needs work",
            Show::AbandonFlagged => "Many abandon",
        }
    }
}

/// A per-book flag the table can toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    Train,
    NeedsWork,
}

/// What a label column counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    Boxes,
    PerPage,
    /// Share of the book's pages with at least one.
    PagesWith,
}

impl Measure {
    const ALL: [Measure; 3] = [Measure::Boxes, Measure::PerPage, Measure::PagesWith];

    fn label(self) -> &'static str {
        match self {
            Measure::Boxes => "Boxes",
            Measure::PerPage => "Per page",
            Measure::PagesWith => "% of pages with",
        }
    }
}

impl Col {
    fn header(self) -> String {
        match self {
            Col::Order => "#".into(),
            Col::Name => "Book".into(),
            Col::Train => "Train".into(),
            Col::NeedsWork => "Needs work".into(),
            Col::Split => "Split".into(),
            Col::Pages => "Pages".into(),
            Col::Reviewed => "Reviewed".into(),
            Col::ToDo => "To do".into(),
            Col::Pictures => "Picture pp.".into(),
            Col::PicturesToDo => "Pict. to do".into(),
            Col::Attention => "Attention".into(),
            Col::Paper => "Paper b*".into(),
            Col::Class(c) => class(c).short.into(),
            Col::LargeAbandon => "Large aband.".into(),
        }
    }

    /// Text columns sort A to Z first; numbers largest first.
    fn descending_first(self) -> bool {
        !matches!(self, Col::Order | Col::Name | Col::Split)
    }

    fn numeric(self) -> bool {
        !matches!(self, Col::Name | Col::Split)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BooksAction {
    None,
    Redraw,
    /// Make this book the queue (`None`: every book).
    Open(Option<String>),
    /// Store a book's flags.
    SetFlags(String, crate::dataset::database::BookFlags),
    /// Move a book to another split.
    SetSplit(String, String),
    /// Give every book of split `new` a split (asked twice).
    AssignSplits,
    OpenFolder,
    Rescan,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Row(usize),
    Header(Col),
    Measure(Measure),
    Show(Show),
    Toggle(usize, Flag),
    AllBooks,
    OpenSelected,
    OpenFolder,
    Rescan,
    Close,
    ScrollLeft,
    ScrollRight,
}

pub struct BooksView {
    pub stats: Vec<BookStats>,
    pub sort: Col,
    pub descending: bool,
    pub measure: Measure,
    pub query: String,
    pub show: Show,
    /// Index into `visible`.
    pub selected: usize,
    scroll: usize,
    /// First scrolled (non-frozen) column shown.
    hscroll: usize,
    /// Scrolled columns that fit at the current width.
    hfit: usize,
    rows_on_screen: usize,
    visible: Vec<usize>,
    hits: Vec<(Rect, Hit)>,
    flag_per_page: f64,
    flag_min: i64,
    /// Whether the editor has a page to go back to.
    pub can_close: bool,
    pub project_label: String,
    pub current_book: Option<String>,
    /// Paper measurement in progress: (done, total).
    pub paper_progress: Option<(usize, usize)>,
    last_click: Option<(usize, Instant)>,
    /// Split names offered by F9, in cycle order.
    pub split_names: Vec<String>,
    /// Books of split `new`, and when Shift+F9 was first pressed (a second
    /// press within a few seconds assigns them).
    assign_armed: Option<Instant>,
    /// A message for the footer, in place of the selected book's line.
    pub notice: Option<(String, Instant)>,
    /// The vertical scroll bar: its hit zone, track and thumb (set by draw).
    vbar: Option<(Rect, Rect, Rect)>,
    /// The pointer is over the scroll bar: draw it wide.
    vbar_hover: bool,
    /// Dragging the thumb: where in the thumb it was grabbed.
    vbar_drag: Option<i32>,
}

impl BooksView {
    pub fn new(
        stats: Vec<BookStats>,
        flag_per_page: f64,
        flag_min: usize,
        current_book: Option<String>,
        project_label: String,
    ) -> BooksView {
        let mut v = BooksView {
            stats,
            sort: Col::Order,
            descending: false,
            measure: Measure::Boxes,
            query: String::new(),
            show: Show::All,
            selected: 0,
            scroll: 0,
            hscroll: 0,
            hfit: usize::MAX,
            rows_on_screen: 20,
            visible: Vec::new(),
            hits: Vec::new(),
            flag_per_page,
            flag_min: flag_min as i64,
            can_close: true,
            project_label,
            current_book,
            paper_progress: None,
            last_click: None,
            split_names: Vec::new(),
            assign_armed: None,
            notice: None,
            vbar: None,
            vbar_hover: false,
            vbar_drag: None,
        };
        v.refilter();
        if let Some(cur) = &v.current_book {
            if let Some(i) = v.visible.iter().position(|&i| &v.stats[i].book_id == cur) {
                v.selected = i;
            }
        }
        v
    }

    /// Replace the counts (after an edit or a bulk conversion), keeping the
    /// selected book selected.
    pub fn update_stats(&mut self, stats: Vec<BookStats>) {
        let keep = self.selected_book().map(str::to_owned);
        self.stats = stats;
        self.refilter();
        self.select_book(keep.as_deref());
    }

    /// A paper tone measured in the background.
    pub fn set_tone(&mut self, book: &str, tone: Tone) {
        if let Some(b) = self.stats.iter_mut().find(|b| b.book_id == book) {
            b.tone = Some(tone);
        }
        if self.sort == Col::Paper {
            let keep = self.selected_book().map(str::to_owned);
            self.refilter();
            self.select_book(keep.as_deref());
        }
    }

    fn select_book(&mut self, book: Option<&str>) {
        if let Some(k) = book {
            if let Some(i) = self
                .visible
                .iter()
                .position(|&i| self.stats[i].book_id == k)
            {
                self.selected = i;
                self.keep_selection_visible();
            }
        }
    }

    /// Many `abandon` boxes left: a book worth a bulk conversion.
    pub fn is_flagged(&self, b: &BookStats) -> bool {
        b.abandon_boxes >= self.flag_min.max(1) && b.abandon_per_page() >= self.flag_per_page
    }

    pub fn flagged_count(&self) -> usize {
        self.stats.iter().filter(|b| self.is_flagged(b)).count()
    }

    pub fn selected_book(&self) -> Option<&str> {
        self.visible
            .get(self.selected)
            .map(|&i| self.stats[i].book_id.as_str())
    }

    /// Book ids in display order (tests).
    pub fn visible_books(&self) -> Vec<&str> {
        self.visible
            .iter()
            .map(|&i| self.stats[i].book_id.as_str())
            .collect()
    }

    fn class_value(&self, b: &BookStats, c: u8) -> f64 {
        let c = c as usize;
        match self.measure {
            Measure::Boxes => b.class_boxes[c] as f64,
            Measure::PerPage => b.per_page(c),
            Measure::PagesWith => b.page_share(c),
        }
    }

    /// The number a numeric column sorts by; `None` sorts last.
    fn value(&self, i: usize, col: Col) -> Option<f64> {
        let b = &self.stats[i];
        Some(match col {
            Col::Order => i as f64,
            Col::Pages => b.pages as f64,
            Col::Reviewed => b.done as f64 / b.pages.max(1) as f64,
            Col::ToDo => b.todo as f64,
            Col::Pictures => b.picture_pages as f64,
            Col::PicturesToDo => b.picture_todo as f64,
            Col::Attention => b.needs_attention as f64,
            Col::Paper => b.tone?.yellowness,
            Col::Class(c) => self.class_value(b, c),
            Col::LargeAbandon => match self.measure {
                Measure::Boxes => b.abandon_large as f64,
                _ => b.large_per_page(),
            },
            Col::Train => !b.excluded as i64 as f64,
            Col::NeedsWork => b.needs_work as i64 as f64,
            Col::Name | Col::Split => return None,
        })
    }

    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        let mut v: Vec<usize> = (0..self.stats.len())
            .filter(|&i| {
                let b = &self.stats[i];
                (q.is_empty() || b.book_id.to_lowercase().contains(&q))
                    && match self.show {
                        Show::All => true,
                        Show::Training => !b.excluded,
                        Show::Excluded => b.excluded,
                        Show::NeedsWork => b.needs_work,
                        Show::AbandonFlagged => self.is_flagged(b),
                    }
            })
            .collect();
        let (col, desc) = (self.sort, self.descending);
        let s = &self.stats;
        v.sort_by(|&a, &b| {
            let ord = match col {
                Col::Name => s[a].book_id.cmp(&s[b].book_id),
                Col::Split => s[a].split.cmp(&s[b].split),
                _ => match (self.value(a, col), self.value(b, col)) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    // Unmeasured values go last whichever way the sort runs.
                    (Some(_), None) => return std::cmp::Ordering::Less,
                    (None, Some(_)) => return std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
            };
            let ord = if desc { ord.reverse() } else { ord };
            ord.then(a.cmp(&b))
        });
        self.visible = v;
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.keep_selection_visible();
    }

    /// Sort by `col`; the same column again reverses the order.
    pub fn sort_by(&mut self, col: Col) {
        if self.sort == col {
            self.descending = !self.descending;
        } else {
            self.sort = col;
            self.descending = col.descending_first();
        }
        let keep = self.selected_book().map(str::to_owned);
        self.refilter();
        // The top of the new order is what was asked for.
        self.selected = 0;
        self.scroll = 0;
        if keep.is_none() {
            self.keep_selection_visible();
        }
        // Bring the sorted column into view.
        let cols = columns();
        if let Some(k) = cols.iter().position(|c| *c == col) {
            if k >= FROZEN {
                let k = k - FROZEN;
                if k < self.hscroll {
                    self.hscroll = k;
                } else if self.hfit != usize::MAX && k >= self.hscroll + self.hfit.max(1) {
                    self.hscroll = k + 1 - self.hfit.max(1);
                }
            }
        }
    }

    fn keep_selection_visible(&mut self) {
        let n = self.rows_on_screen.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + n {
            self.scroll = self.selected + 1 - n;
        }
        let max_scroll = self.visible.len().saturating_sub(n);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn move_to(&mut self, i: isize) -> BooksAction {
        if self.visible.is_empty() {
            return BooksAction::None;
        }
        let last = self.visible.len() as isize - 1;
        self.selected = i.clamp(0, last) as usize;
        self.keep_selection_visible();
        BooksAction::Redraw
    }

    fn move_by(&mut self, d: isize) -> BooksAction {
        self.move_to(self.selected as isize + d)
    }

    fn open_selected(&self) -> BooksAction {
        match self.selected_book() {
            Some(b) => BooksAction::Open(Some(b.to_owned())),
            None => BooksAction::None,
        }
    }

    pub fn hscroll_by(&mut self, d: isize) {
        let n = columns().len() - FROZEN;
        let max = n.saturating_sub(self.hfit.min(n));
        self.hscroll = (self.hscroll as isize + d).clamp(0, max as isize) as usize;
    }

    pub fn key(&mut self, ev: &KeyEvent) -> BooksAction {
        if !ev.pressed {
            return BooksAction::None;
        }
        let page = self.rows_on_screen.max(1) as isize;
        let ctrl = ev.modifiers.control || ev.modifiers.logo;
        match ev.key {
            KeyInput::Up => return self.move_by(-1),
            KeyInput::Down => return self.move_by(1),
            KeyInput::PageUp => return self.move_by(-page),
            KeyInput::PageDown => return self.move_by(page),
            KeyInput::Home => return self.move_to(0),
            KeyInput::End => return self.move_to(isize::MAX),
            KeyInput::Left => {
                self.hscroll_by(-1);
                return BooksAction::Redraw;
            }
            KeyInput::Right => {
                self.hscroll_by(1);
                return BooksAction::Redraw;
            }
            KeyInput::Enter => return self.open_selected(),
            KeyInput::Tab => {
                let cols = columns();
                let i = cols.iter().position(|k| *k == self.sort).unwrap_or(0);
                let n = cols.len();
                let next = if ev.modifiers.shift {
                    (i + n - 1) % n
                } else {
                    (i + 1) % n
                };
                self.sort_by(cols[next]);
                return BooksAction::Redraw;
            }
            KeyInput::Escape => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.refilter();
                    return BooksAction::Redraw;
                }
                return if self.can_close {
                    BooksAction::Close
                } else {
                    BooksAction::None
                };
            }
            KeyInput::Backspace => {
                self.query.pop();
                self.refilter();
                return BooksAction::Redraw;
            }
            KeyInput::Function(2) => {
                let i = Measure::ALL
                    .iter()
                    .position(|m| *m == self.measure)
                    .unwrap_or(0);
                self.measure = Measure::ALL[(i + 1) % Measure::ALL.len()];
                self.refilter();
                return BooksAction::Redraw;
            }
            KeyInput::Function(3) => {
                self.show = if self.show == Show::AbandonFlagged {
                    Show::All
                } else {
                    Show::AbandonFlagged
                };
                self.refilter();
                return BooksAction::Redraw;
            }
            KeyInput::Function(9) if ev.modifiers.shift => {
                let n = self.stats.iter().filter(|b| b.split == "new").count();
                if n == 0 {
                    self.say("No book is in split \"new\"".into());
                    return BooksAction::Redraw;
                }
                if self
                    .assign_armed
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(6))
                {
                    self.assign_armed = None;
                    return BooksAction::AssignSplits;
                }
                self.assign_armed = Some(Instant::now());
                self.say(format!(
                    "Shift+F9 again: give the {n} books in split \"new\" a split by a hash of their name \
                     (about 10% dev, 10% final_test, the rest train). F9 moves one book."
                ));
                return BooksAction::Redraw;
            }
            KeyInput::Function(9) => return self.cycle_split(self.selected),
            KeyInput::Function(6) => return self.toggle(self.selected, Flag::Train),
            KeyInput::Function(7) => return self.toggle(self.selected, Flag::NeedsWork),
            KeyInput::Function(8) => {
                self.cycle_show();
                return BooksAction::Redraw;
            }
            KeyInput::Function(4) => {
                self.descending = !self.descending;
                self.refilter();
                return BooksAction::Redraw;
            }
            KeyInput::Function(5) => return BooksAction::Rescan,
            _ => {}
        }
        if ctrl {
            return match ev.key {
                KeyInput::Character(c) if c.eq_ignore_ascii_case(&'o') => BooksAction::OpenFolder,
                _ => BooksAction::None,
            };
        }
        let typed: String = ev.text.chars().filter(|c| !c.is_control()).collect();
        if typed.is_empty() {
            return BooksAction::None;
        }
        self.query.push_str(&typed);
        self.selected = 0;
        self.refilter();
        BooksAction::Redraw
    }

    /// Flip a flag of the book in row `vi`; the caller stores it.
    pub fn toggle(&mut self, vi: usize, flag: Flag) -> BooksAction {
        let Some(&bi) = self.visible.get(vi) else {
            return BooksAction::None;
        };
        let b = &mut self.stats[bi];
        match flag {
            Flag::Train => b.excluded = !b.excluded,
            Flag::NeedsWork => b.needs_work = !b.needs_work,
        }
        let (book, flags) = (
            b.book_id.clone(),
            crate::dataset::database::BookFlags {
                excluded: b.excluded,
                needs_work: b.needs_work,
            },
        );
        // Stay on the book, even if the filter or sort now puts it elsewhere.
        self.selected = vi;
        self.refilter();
        self.select_book(Some(&book));
        BooksAction::SetFlags(book, flags)
    }

    /// A footer message for a few seconds.
    pub fn say(&mut self, msg: String) {
        self.notice = Some((msg, Instant::now()));
    }

    /// F9: the next split for the book in row `vi`; the caller stores it.
    pub fn cycle_split(&mut self, vi: usize) -> BooksAction {
        let Some(&bi) = self.visible.get(vi) else {
            return BooksAction::None;
        };
        let mut names: Vec<String> = ["train", "dev", "final_test", "regression", "new"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        for n in &self.split_names {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        let b = &mut self.stats[bi];
        let i = names
            .iter()
            .position(|n| *n == b.split)
            .unwrap_or(names.len() - 1);
        b.split = names[(i + 1) % names.len()].clone();
        let (book, split) = (b.book_id.clone(), b.split.clone());
        self.refilter();
        self.select_book(Some(&book));
        BooksAction::SetSplit(book, split)
    }

    fn cycle_show(&mut self) {
        let i = Show::ALL.iter().position(|m| *m == self.show).unwrap_or(0);
        self.show = Show::ALL[(i + 1) % Show::ALL.len()];
        self.selected = 0;
        self.refilter();
    }

    pub fn scroll_by(&mut self, notches: f64) {
        let rows = (-notches * 3.0).round() as isize;
        self.scroll_to(self.scroll as isize + rows);
    }

    /// Show row `first` at the top, keeping the selection on screen.
    fn scroll_to(&mut self, first: isize) {
        let max_scroll = self
            .visible
            .len()
            .saturating_sub(self.rows_on_screen.max(1));
        self.scroll = first.clamp(0, max_scroll as isize) as usize;
        let n = self.rows_on_screen.max(1);
        if self.selected < self.scroll {
            self.selected = self.scroll;
        } else if self.selected >= self.scroll + n {
            self.selected = self.scroll + n - 1;
        }
    }

    /// Scroll so the thumb's top sits at `thumb_y`.
    fn drag_thumb_to(&mut self, thumb_y: i32) {
        let Some((_, track, thumb)) = self.vbar else {
            return;
        };
        let room = (track.h - thumb.h).max(1) as f64;
        let max_scroll = self
            .visible
            .len()
            .saturating_sub(self.rows_on_screen.max(1)) as f64;
        let f = ((thumb_y - track.y) as f64 / room).clamp(0.0, 1.0);
        self.scroll_to((f * max_scroll).round() as isize);
    }

    /// Pointer motion: hover widens the scroll bar; a drag moves the thumb.
    /// Returns whether to redraw.
    pub fn pointer_move(&mut self, x: i32, y: i32) -> bool {
        if let Some(grab) = self.vbar_drag {
            let before = self.scroll;
            self.drag_thumb_to(y - grab);
            return self.scroll != before;
        }
        let hover = self.vbar.is_some_and(|(zone, _, _)| zone.contains(x, y));
        let changed = hover != self.vbar_hover;
        self.vbar_hover = hover;
        changed
    }

    /// The button is up: a thumb drag ends. Returns whether to redraw.
    pub fn pointer_up(&mut self, x: i32, y: i32) -> bool {
        if self.vbar_drag.take().is_some() {
            self.vbar_hover = self.vbar.is_some_and(|(zone, _, _)| zone.contains(x, y));
            return true;
        }
        false
    }

    pub fn dragging_scrollbar(&self) -> bool {
        self.vbar_drag.is_some()
    }

    pub fn click(&mut self, x: i32, y: i32) -> BooksAction {
        // The scroll bar: grab the thumb, or jump to where the track was hit.
        if let Some((zone, _, thumb)) = self.vbar {
            if zone.contains(x, y) {
                let grab = if y >= thumb.y && y < thumb.bottom() {
                    y - thumb.y
                } else {
                    thumb.h / 2
                };
                self.vbar_drag = Some(grab);
                self.vbar_hover = true;
                self.drag_thumb_to(y - grab);
                return BooksAction::Redraw;
            }
        }
        // A flag cell lies inside its row: it wins.
        let hit = self
            .hits
            .iter()
            .filter(|(r, _)| r.contains(x, y))
            .map(|(_, h)| *h)
            .min_by_key(|h| !matches!(h, Hit::Toggle(..)));
        let Some(hit) = hit else {
            return BooksAction::None;
        };
        match hit {
            Hit::Row(i) => {
                // A second click on the same row (or a double click) opens it.
                let again = self
                    .last_click
                    .is_some_and(|(r, t)| r == i && t.elapsed() < Duration::from_millis(600));
                self.last_click = Some((i, Instant::now()));
                if again || self.selected == i {
                    self.selected = i;
                    return self.open_selected();
                }
                self.selected = i;
                BooksAction::Redraw
            }
            Hit::Header(c) => {
                self.sort_by(c);
                BooksAction::Redraw
            }
            Hit::Measure(m) => {
                self.measure = m;
                self.refilter();
                BooksAction::Redraw
            }
            Hit::Show(m) => {
                self.show = m;
                self.selected = 0;
                self.refilter();
                BooksAction::Redraw
            }
            Hit::Toggle(vi, flag) => self.toggle(vi, flag),
            Hit::ScrollLeft => {
                self.hscroll_by(-3);
                BooksAction::Redraw
            }
            Hit::ScrollRight => {
                self.hscroll_by(3);
                BooksAction::Redraw
            }
            Hit::AllBooks => BooksAction::Open(None),
            Hit::OpenSelected => self.open_selected(),
            Hit::OpenFolder => BooksAction::OpenFolder,
            Hit::Rescan => BooksAction::Rescan,
            Hit::Close => BooksAction::Close,
        }
    }

    /// The text of a cell, and its colour.
    fn cell_text(&self, i: usize, col: Col) -> (String, u32) {
        let b = &self.stats[i];
        let dim_if_zero = |n: i64, s: String| {
            if n == 0 {
                ("–".to_owned(), HUD_DIM)
            } else {
                (s, HUD_TEXT)
            }
        };
        match col {
            Col::Order => ((i + 1).to_string(), HUD_DIM),
            Col::Name => (
                b.book_id.clone(),
                if b.excluded { HUD_DIM } else { HUD_TEXT },
            ),
            Col::Train => (String::new(), HUD_TEXT),
            Col::NeedsWork => {
                if b.needs_work {
                    ("needs work".into(), WARN)
                } else {
                    ("–".into(), HUD_DIM)
                }
            }
            Col::Split => (b.split.clone(), HUD_DIM),
            Col::Pages => (ui::thousands(b.pages), HUD_TEXT),
            Col::Reviewed => {
                let pct = (b.done * 100).checked_div(b.pages).unwrap_or(0);
                let c = if b.done == b.pages && b.pages > 0 {
                    ui::GOOD
                } else if b.done > 0 {
                    HUD_TEXT
                } else {
                    HUD_DIM
                };
                (format!("{pct}%"), c)
            }
            Col::ToDo => {
                let (s, c) = dim_if_zero(b.todo, ui::thousands(b.todo));
                (
                    if b.todo == 0 { "done".into() } else { s },
                    if b.todo == 0 { ui::GOOD } else { c },
                )
            }
            Col::Pictures => dim_if_zero(b.picture_pages, ui::thousands(b.picture_pages)),
            Col::PicturesToDo => dim_if_zero(b.picture_todo, ui::thousands(b.picture_todo)),
            Col::Attention => {
                let (s, c) = dim_if_zero(b.needs_attention, ui::thousands(b.needs_attention));
                (s, if c == HUD_TEXT { WARN } else { c })
            }
            Col::Paper => match &b.tone {
                Some(t) => (format!("{:.1}", t.yellowness), HUD_TEXT),
                None => (
                    if self.paper_progress.is_some() {
                        "…".into()
                    } else {
                        "–".into()
                    },
                    HUD_DIM,
                ),
            },
            Col::Class(c) => {
                let n = b.class_boxes[c as usize];
                let s = match self.measure {
                    Measure::Boxes => ui::thousands(n),
                    Measure::PerPage => format!("{:.2}", b.per_page(c as usize)),
                    Measure::PagesWith => format!("{:.0}%", b.page_share(c as usize) * 100.0),
                };
                let (s, col) = dim_if_zero(n, s);
                let col = if c == ABANDON && self.is_flagged(b) {
                    ABANDON_CLASS.color
                } else {
                    col
                };
                (s, col)
            }
            Col::LargeAbandon => {
                let s = match self.measure {
                    Measure::Boxes => ui::thousands(b.abandon_large),
                    _ => format!("{:.2}", b.large_per_page()),
                };
                let (s, c) = dim_if_zero(b.abandon_large, s);
                (
                    s,
                    if c == HUD_TEXT && b.large_per_page() >= 0.25 {
                        WARN
                    } else {
                        c
                    },
                )
            }
        }
    }

    fn col_width(
        &self,
        text: &mut TextCache,
        s: Scale,
        col: Col,
        st: TextStyle,
        hst: TextStyle,
    ) -> i32 {
        let sample = match col {
            Col::Order => "0000".to_owned(),
            Col::Name | Col::Train => String::new(),
            Col::NeedsWork => "needs work".to_owned(),
            Col::Split => "final_test".to_owned(),
            Col::Pages => "00,000".to_owned(),
            Col::Reviewed => "100%".to_owned(),
            Col::ToDo | Col::Pictures | Col::PicturesToDo => "00,000".to_owned(),
            Col::Attention => "0,000".to_owned(),
            Col::Paper => "00.0".to_owned(),
            Col::Class(_) | Col::LargeAbandon => match self.measure {
                Measure::Boxes => "000,000".to_owned(),
                Measure::PerPage => "00.00".to_owned(),
                Measure::PagesWith => "100%".to_owned(),
            },
        };
        let extra = if col == Col::Paper { s.px(16) } else { 0 };
        text.measure(&sample, st)
            .max(text.measure(&col.header(), hst) + s.px(10))
            + s.px(14)
            + extra
    }

    pub fn draw(&mut self, p: &mut Painter<'_>, text: &mut TextCache, s: Scale, w: i32, h: i32) {
        self.hits.clear();
        p.fill_rect(Rect::new(0, 0, w, h), HUD_BG);
        let title = ui::style(s, 17.0);
        let body = ui::style(s, 13.0);
        let small = ui::style(s, 11.5);
        let lh = text.line_height(body);
        let sl = text.line_height(small);
        let pad = s.px(16);
        let mut y = s.px(12);

        // Title and project.
        let tw = text.draw(p, "Books", pad, y, title, HUD_TEXT);
        let tl = text.line_height(title);
        ui::cell(
            p,
            text,
            &self.project_label,
            pad + tw + s.px(14),
            y + (tl - sl) / 2,
            w - 2 * pad - tw - s.px(14),
            tl,
            small,
            HUD_DIM,
            Align::Left,
        );
        y += tl + s.px(8);

        // Actions.
        let mut x = pad;
        let button = |p: &mut Painter<'_>,
                      text: &mut TextCache,
                      x: &mut i32,
                      label: &str,
                      hit: Hit,
                      hits: &mut Vec<(Rect, Hit)>| {
            let r = ui::button(p, text, s, *x, y, label, body, HUD_EDGE, None);
            hits.push((r, hit));
            *x = r.right() + s.px(8);
            r
        };
        let r = button(
            p,
            text,
            &mut x,
            "Review selected book  Enter",
            Hit::OpenSelected,
            &mut self.hits,
        );
        button(
            p,
            text,
            &mut x,
            "Review all books",
            Hit::AllBooks,
            &mut self.hits,
        );
        button(
            p,
            text,
            &mut x,
            "Open folder…  Ctrl+O",
            Hit::OpenFolder,
            &mut self.hits,
        );
        button(
            p,
            text,
            &mut x,
            "Rescan folder  F5",
            Hit::Rescan,
            &mut self.hits,
        );
        if self.can_close {
            button(
                p,
                text,
                &mut x,
                "Back to page  Esc",
                Hit::Close,
                &mut self.hits,
            );
        }
        let q = if self.query.is_empty() {
            "Type to filter by name".to_owned()
        } else {
            format!("Filter: {}|", self.query)
        };
        ui::cell(
            p,
            text,
            &q,
            x + s.px(10),
            y + s.px(5),
            w - pad - x - s.px(10),
            lh,
            small,
            if self.query.is_empty() {
                HUD_DIM
            } else {
                HUD_TEXT
            },
            Align::Left,
        );
        y = r.bottom() + s.px(10);

        // Measure, flag filter, paper progress, search box.
        let mut x = pad;
        x += text.draw(p, "Label columns show:", x, y + s.px(5), small, HUD_DIM) + s.px(8);
        for m in Measure::ALL {
            let active = self.measure == m;
            let r = ui::button(
                p,
                text,
                s,
                x,
                y,
                m.label(),
                small,
                if active { ACCENT } else { HUD_EDGE },
                active.then_some(ROW_SELECTED),
            );
            self.hits.push((r, Hit::Measure(m)));
            x = r.right() + s.px(4);
        }
        x += text.draw(p, "F2", x + s.px(4), y + s.px(5), small, HUD_DIM) + s.px(18);
        x += text.draw(p, "Show:", x, y + s.px(5), small, HUD_DIM) + s.px(8);
        let mut r = Rect::new(x, y, 0, lh + s.px(10));
        for m in Show::ALL {
            let active = self.show == m;
            let n = match m {
                Show::All => self.stats.len(),
                Show::Training => self.stats.iter().filter(|b| !b.excluded).count(),
                Show::Excluded => self.stats.iter().filter(|b| b.excluded).count(),
                Show::NeedsWork => self.stats.iter().filter(|b| b.needs_work).count(),
                Show::AbandonFlagged => self.flagged_count(),
            };
            let label = format!("{} {n}", m.label());
            r = ui::button(
                p,
                text,
                s,
                x,
                y,
                &label,
                small,
                if active { ACCENT } else { HUD_EDGE },
                active.then_some(ROW_SELECTED),
            );
            self.hits.push((r, Hit::Show(m)));
            x = r.right() + s.px(4);
        }
        x += text.draw(p, "F8", x + s.px(4), y + s.px(5), small, HUD_DIM) + s.px(18);
        if let Some((done, total)) = self.paper_progress {
            let m = format!("Measuring paper tone: {done} of {total} books");
            x += text.draw(p, &m, x, y + s.px(5), small, ACCENT) + s.px(14);
        }
        let _ = x;
        y = r.bottom() + s.px(10);

        // Totals.
        let pages: i64 = self.stats.iter().map(|b| b.pages).sum();
        let done: i64 = self.stats.iter().map(|b| b.done).sum();
        let figures: i64 = self.stats.iter().map(|b| b.class_boxes[6]).sum();
        let abandon: i64 = self.stats.iter().map(|b| b.abandon_boxes).sum();
        let totals = format!(
            "{} books ({} shown, {} excluded from training, {} need work) · {} pages · {}% reviewed · {} figures · {} abandon boxes left",
            ui::thousands(self.stats.len() as i64),
            ui::thousands(self.visible.len() as i64),
            self.stats.iter().filter(|b| b.excluded).count(),
            self.stats.iter().filter(|b| b.needs_work).count(),
            ui::thousands(pages),
            (done * 100).checked_div(pages).unwrap_or(0),
            ui::thousands(figures),
            ui::thousands(abandon),
        );
        ui::cell(
            p,
            text,
            &totals,
            pad,
            y,
            w - 2 * pad,
            lh,
            small,
            HUD_DIM,
            Align::Left,
        );
        y += sl + s.px(6);

        // Table geometry: frozen #/Book, then as many columns as fit.
        let footer_h = 2 * sl + s.px(22);
        let table = Rect::new(pad, y, w - 2 * pad, (h - y - footer_h).max(0));
        let row_h = lh + s.px(8);
        let cols = columns();
        let widths: Vec<i32> = cols
            .iter()
            .map(|&c| self.col_width(text, s, c, body, small))
            .collect();
        let swatch = s.px(22);
        let name_min = s.px(220);
        let arrow_w = s.px(22);
        let avail = table.w - swatch - widths[0] - name_min - arrow_w;
        let scrolled = cols.len() - FROZEN;
        self.hscroll = self.hscroll.min(scrolled.saturating_sub(1));
        let mut fit = 0;
        let mut used = 0;
        for k in self.hscroll..scrolled {
            if used + widths[FROZEN + k] > avail {
                break;
            }
            used += widths[FROZEN + k];
            fit += 1;
        }
        // Everything fits from the start: no sideways scroll.
        let all_fit = self.hscroll == 0 && fit == scrolled;
        self.hfit = fit.max(1);
        let name_w =
            (table.w - swatch - widths[0] - used - if all_fit { 0 } else { arrow_w }).max(name_min);
        let shown: Vec<usize> = (0..FROZEN)
            .chain((FROZEN + self.hscroll)..(FROZEN + self.hscroll + fit))
            .collect();
        let mut xs: Vec<(usize, i32, i32)> = Vec::new(); // (col index, x, width)
        let mut cx = table.x + swatch;
        for &k in &shown {
            let cw = if cols[k] == Col::Name {
                name_w
            } else {
                widths[k]
            };
            xs.push((k, cx, cw));
            cx += cw;
        }

        // Header.
        let hy = table.y;
        p.fill_rect(Rect::new(table.x, hy, table.w, row_h), ROW_ALT);
        for &(k, x, cw) in &xs {
            let col = cols[k];
            let r = Rect::new(x, hy, cw, row_h);
            let sorted = self.sort == col;
            if sorted {
                p.fill_rect(r, ROW_SELECTED);
            }
            if let Col::Class(c) = col {
                // The label's colour as a bar under its name.
                p.fill_rect(
                    Rect::new(x + s.px(6), hy + row_h - s.px(3), cw - s.px(10), s.px(2)),
                    class(c).color,
                );
            }
            let align = if col.numeric() {
                Align::Right
            } else {
                Align::Left
            };
            let tx = if col.numeric() { x } else { x + s.px(6) };
            let tw = cw - s.px(8) - if sorted { s.px(10) } else { 0 };
            ui::cell(
                p,
                text,
                &col.header(),
                tx,
                hy + (row_h - sl) / 2,
                tw,
                row_h,
                small,
                if sorted { HUD_TEXT } else { HUD_DIM },
                align,
            );
            if sorted {
                ui::triangle(
                    p,
                    x + cw - s.px(12),
                    hy + row_h / 2,
                    s.px(4),
                    self.descending,
                    ACCENT,
                );
            }
            self.hits.push((r, Hit::Header(col)));
        }
        if !all_fit {
            let left = self.hscroll > 0;
            let right = self.hscroll + fit < scrolled;
            let rr = Rect::new(table.right() - arrow_w, hy, arrow_w, row_h);
            if left {
                let r = Rect::new(table.x, hy, swatch, row_h);
                text.draw(p, "«", r.x + s.px(5), hy + (row_h - sl) / 2, small, ACCENT);
                self.hits.push((r, Hit::ScrollLeft));
            }
            if right {
                text.draw(p, "»", rr.x + s.px(6), hy + (row_h - sl) / 2, small, ACCENT);
                self.hits.push((rr, Hit::ScrollRight));
            }
        }
        p.fill_rect(Rect::new(table.x, hy + row_h - 1, table.w, 1), HUD_EDGE);

        // Rows.
        let body_top = hy + row_h;
        self.rows_on_screen = ((table.bottom() - body_top) / row_h).max(1) as usize;
        self.keep_selection_visible();
        if self.visible.is_empty() {
            let msg = if self.stats.is_empty() {
                "This project has no pages yet. Open a dataset folder (Ctrl+O) or drop one onto the window."
            } else {
                "No book matches the filter."
            };
            ui::cell(
                p,
                text,
                msg,
                table.x,
                body_top + s.px(12),
                table.w,
                row_h,
                body,
                HUD_DIM,
                Align::Left,
            );
        }
        let end = (self.scroll + self.rows_on_screen).min(self.visible.len());
        for (row, vi) in (self.scroll..end).enumerate() {
            let bi = self.visible[vi];
            let ry = body_top + row as i32 * row_h;
            let r = Rect::new(table.x, ry, table.w, row_h);
            if vi == self.selected {
                p.fill_rect(r, ROW_SELECTED);
                outline(p, r, ACCENT, 1);
            } else if row % 2 == 1 {
                p.fill_rect(r, ROW_ALT);
            }
            self.hits.push((r, Hit::Row(vi)));
            let ty = ry + (row_h - lh) / 2;
            let b = &self.stats[bi];
            if self.is_flagged(b) {
                let k = s.px(8);
                p.fill_rect(
                    Rect::new(table.x + s.px(7), ry + (row_h - k) / 2, k, k),
                    ABANDON_CLASS.color,
                );
            }
            for &(k, x, cw) in &xs {
                let col = cols[k];
                let (mut v, c) = self.cell_text(bi, col);
                if col == Col::Name && self.current_book.as_deref() == Some(b.book_id.as_str()) {
                    v.push_str("  (current)");
                }
                if col == Col::Paper {
                    if let Some(t) = &self.stats[bi].tone {
                        let k = s.px(11);
                        let sw = Rect::new(x + s.px(6), ry + (row_h - k) / 2, k, k);
                        let [r8, g8, b8] = t.rgb;
                        p.fill_rect(sw, (r8 as u32) << 16 | (g8 as u32) << 8 | b8 as u32);
                        outline(p, sw, HUD_EDGE, 1);
                    }
                }
                if col == Col::Train {
                    // A checkbox: ticked = goes into training.
                    let k = s.px(13);
                    let bx = Rect::new(x + (cw - k) / 2, ry + (row_h - k) / 2, k, k);
                    outline(p, bx, if b.excluded { WARN } else { HUD_DIM }, 1);
                    if !b.excluded {
                        p.fill_rect(bx.inset(s.px(3)), ui::GOOD);
                    }
                    self.hits
                        .push((Rect::new(x, ry, cw, row_h), Hit::Toggle(vi, Flag::Train)));
                    continue;
                }
                if col == Col::NeedsWork {
                    self.hits.push((
                        Rect::new(x, ry, cw, row_h),
                        Hit::Toggle(vi, Flag::NeedsWork),
                    ));
                }
                let align = if col.numeric() {
                    Align::Right
                } else {
                    Align::Left
                };
                let tx = if col.numeric() { x } else { x + s.px(6) };
                ui::cell(p, text, &v, tx, ty, cw - s.px(8), row_h, body, c, align);
            }
        }
        // Vertical scroll bar: a slim visible thumb at rest; wide, with a
        // track, under the pointer or while dragged. The hit zone is wider
        // than either, so it is easy to grab.
        self.vbar = None;
        if self.visible.len() > self.rows_on_screen {
            let active = self.vbar_hover || self.vbar_drag.is_some();
            let bw = if active { s.px(12) } else { s.px(5) };
            let zone_w = s.px(18);
            let full = Rect::new(
                table.right() - bw - s.px(2),
                body_top,
                bw,
                table.bottom() - body_top,
            );
            let n = self.visible.len() as f64;
            let max_scroll = self
                .visible
                .len()
                .saturating_sub(self.rows_on_screen.max(1)) as f64;
            let th = ((self.rows_on_screen as f64 / n) * full.h as f64)
                .max(s.px(28) as f64)
                .min(full.h as f64) as i32;
            let room = (full.h - th).max(0) as f64;
            let ty = full.y + (self.scroll as f64 / max_scroll.max(1.0) * room).round() as i32;
            let thumb = Rect::new(full.x, ty, full.w, th);
            if active {
                p.fill_rect(full, 0x0022_262b);
                outline(p, full, HUD_EDGE, 1);
            }
            let color = if self.vbar_drag.is_some() {
                ACCENT
            } else if active {
                0x00b4_bac2
            } else {
                0x0062_6a74
            };
            p.fill_rect(thumb, color);
            let zone = Rect::new(table.right() - zone_w, body_top, zone_w, full.h);
            self.vbar = Some((zone, full, thumb));
        }

        // Footer: the selected book, then the keys.
        let fy = h - footer_h + s.px(6);
        p.fill_rect(Rect::new(0, h - footer_h, w, 1), HUD_EDGE);
        let notice = self
            .notice
            .as_ref()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(6))
            .map(|(m, _)| m.clone());
        if let Some(msg) = notice {
            ui::cell(
                p,
                text,
                &msg,
                pad,
                fy,
                w - 2 * pad,
                lh,
                small,
                WARN,
                Align::Left,
            );
        } else if let Some(&bi) = self.visible.get(self.selected) {
            let detail = self.detail(bi);
            ui::cell(
                p,
                text,
                &detail,
                pad,
                fy,
                w - 2 * pad,
                lh,
                small,
                HUD_TEXT,
                Align::Left,
            );
        }
        let legend = format!(
            "Type to filter · Up/Down PgUp PgDn Home End · Enter or a second click: review · click a header or Tab: sort, again or F4: reverse · \
             Left/Right: more columns · F6: train on/off · F7: needs work · F8: show · F9: split (Shift+F9: split the new books) · \
             F2: boxes, per page, % of pages · \
             F3: many abandon ({:.1}+ per page) · paper b*: 0 white, 15+ yellowed",
            self.flag_per_page
        );
        ui::cell(
            p,
            text,
            &legend,
            pad,
            fy + sl + s.px(6),
            w - 2 * pad,
            lh,
            small,
            HUD_DIM,
            Align::Left,
        );
    }

    /// One line about a book: the classes it has most of, and its paper.
    fn detail(&self, i: usize) -> String {
        let b = &self.stats[i];
        let mut parts = vec![b.book_id.clone()];
        if b.excluded {
            parts.push("EXCLUDED from training".into());
        }
        if b.needs_work {
            parts.push("needs work".into());
        }
        parts.extend([format!(
            "{} pages, {}% reviewed, {} to do",
            ui::thousands(b.pages),
            (b.done * 100).checked_div(b.pages).unwrap_or(0),
            ui::thousands(b.todo)
        )]);
        if b.picture_pages > 0 {
            let mut s = format!(
                "{} picture pages, {} to do",
                ui::thousands(b.picture_pages),
                ui::thousands(b.picture_todo)
            );
            if b.model_only_todo > 0 {
                s.push_str(&format!(
                    " ({} with a picture only the model sees)",
                    ui::thousands(b.model_only_todo)
                ));
            }
            parts.push(s);
        }
        for c in [6u8, 7, 8, 13] {
            let n = b.class_boxes[c as usize];
            if n > 0 {
                parts.push(format!(
                    "{} {} on {} pages",
                    ui::thousands(n),
                    class(c).name,
                    ui::thousands(b.class_pages[c as usize])
                ));
            }
        }
        if b.abandon_boxes > 0 {
            parts.push(format!(
                "{} abandon ({} large)",
                ui::thousands(b.abandon_boxes),
                ui::thousands(b.abandon_large)
            ));
        }
        if let Some(t) = &b.tone {
            parts.push(format!(
                "paper #{:02x}{:02x}{:02x}, L* {:.0}, b* {:.1}",
                t.rgb[0], t.rgb[1], t.rgb[2], t.lightness, t.yellowness
            ));
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(id: &str, pages: i64, abandon: i64, figures: i64) -> BookStats {
        let mut b = BookStats {
            book_id: id.into(),
            split: "new".into(),
            pages,
            abandon_boxes: abandon,
            abandon_pages: abandon.min(pages),
            abandon_imported: abandon,
            ..Default::default()
        };
        b.class_boxes[ABANDON as usize] = abandon;
        b.class_boxes[6] = figures;
        b.class_pages[6] = figures.min(pages);
        b
    }

    fn view(books: Vec<BookStats>) -> BooksView {
        BooksView::new(books, 3.0, 20, None, String::new())
    }

    #[test]
    fn flags_books_with_many_abandon_boxes() {
        let v = view(vec![
            book("a", 100, 150, 0),
            book("b", 100, 400, 0),
            book("c", 5, 19, 0),
        ]);
        let flagged: Vec<&str> = v
            .stats
            .iter()
            .filter(|b| v.is_flagged(b))
            .map(|b| b.book_id.as_str())
            .collect();
        // a: 1.5 per page (running heads and page numbers); c: 3.8 per page
        // but under 20 in all.
        assert_eq!(flagged, ["b"]);
    }

    #[test]
    fn any_column_sorts_both_ways() {
        let mut v = view(vec![
            book("alpha", 10, 1, 40),
            book("beta", 100, 50, 60),
            book("gamma", 10, 7, 5),
        ]);
        v.sort_by(Col::Class(6));
        assert!(v.descending);
        assert_eq!(v.visible_books(), ["beta", "alpha", "gamma"]);
        v.sort_by(Col::Class(6));
        assert_eq!(v.visible_books(), ["gamma", "alpha", "beta"]);
        // Per page, alpha's 4 figures a page beat beta's 0.6.
        v.measure = Measure::PerPage;
        v.descending = true;
        v.refilter();
        assert_eq!(v.visible_books(), ["alpha", "beta", "gamma"]);
        v.sort_by(Col::Name);
        assert!(!v.descending);
        assert_eq!(v.visible_books(), ["alpha", "beta", "gamma"]);
        v.query = "ta".into();
        v.refilter();
        assert_eq!(v.visible_books(), ["beta"]);
        v.query.clear();
        v.show = Show::AbandonFlagged;
        v.refilter();
        assert!(v.visible_books().is_empty());
    }

    #[test]
    fn unmeasured_paper_sorts_last_either_way() {
        let mut books = vec![
            book("a", 10, 0, 0),
            book("b", 10, 0, 0),
            book("c", 10, 0, 0),
        ];
        books[0].tone = Some(Tone {
            yellowness: 4.0,
            ..Tone::default()
        });
        books[2].tone = Some(Tone {
            yellowness: 18.0,
            ..Tone::default()
        });
        let mut v = view(books);
        v.sort_by(Col::Paper);
        assert_eq!(v.visible_books(), ["c", "a", "b"]);
        v.sort_by(Col::Paper);
        assert_eq!(v.visible_books(), ["a", "c", "b"]);
        v.set_tone(
            "b",
            Tone {
                yellowness: 9.0,
                ..Tone::default()
            },
        );
        assert_eq!(v.visible_books(), ["a", "b", "c"]);
    }

    #[test]
    fn home_and_end_work_while_filtering() {
        let mut v = view(
            (0..50)
                .map(|i| book(&format!("book{i:02}"), 10, 0, 0))
                .collect(),
        );
        v.query = "book".into();
        v.refilter();
        let key = |k| KeyEvent {
            key: k,
            text: String::new(),
            physical: String::new(),
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        v.key(&key(KeyInput::End));
        assert_eq!(v.selected_book(), Some("book49"));
        v.key(&key(KeyInput::Home));
        assert_eq!(v.selected_book(), Some("book00"));
    }

    fn press(v: &mut BooksView, k: KeyInput, shift: bool) -> BooksAction {
        v.key(&KeyEvent {
            key: k,
            text: String::new(),
            physical: String::new(),
            pressed: true,
            repeat: false,
            modifiers: liminalkit_shell::Modifiers {
                shift,
                ..Default::default()
            },
        })
    }

    #[test]
    fn f9_moves_a_book_to_the_next_split_and_shift_f9_asks_twice() {
        let mut v = view(vec![book("a", 10, 0, 0), book("b", 10, 0, 0)]);
        // new → train → dev → final_test → regression → new.
        let mut seen = Vec::new();
        for _ in 0..5 {
            match press(&mut v, KeyInput::Function(9), false) {
                BooksAction::SetSplit(b, s) => {
                    assert_eq!(b, "a");
                    seen.push(s);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(seen, ["train", "dev", "final_test", "regression", "new"]);
        assert_eq!(
            press(&mut v, KeyInput::Function(9), true),
            BooksAction::Redraw
        );
        assert!(v.notice.as_ref().unwrap().0.contains("2 books"));
        assert_eq!(
            press(&mut v, KeyInput::Function(9), true),
            BooksAction::AssignSplits
        );
    }

    #[test]
    fn the_scroll_bar_widens_under_the_pointer_and_drags() {
        let books: Vec<BookStats> = (0..100)
            .map(|i| book(&format!("b{i:03}"), 10, 0, 0))
            .collect();
        let mut v = view(books);
        v.rows_on_screen = 10;
        // As drawn: a zone 18 px wide at the right, track 0..400, thumb 40 px.
        let zone = Rect::new(982, 0, 18, 400);
        let track = Rect::new(986, 0, 12, 400);
        v.vbar = Some((zone, track, Rect::new(986, 0, 12, 40)));
        assert!(v.pointer_move(990, 100), "hover widens it");
        assert!(v.vbar_hover);
        assert!(!v.pointer_move(500, 100) || !v.vbar_hover);
        // A click low on the track jumps there and keeps dragging.
        assert_eq!(v.click(990, 380), BooksAction::Redraw);
        assert!(v.dragging_scrollbar());
        assert_eq!(v.scroll, 90, "thumb centred on 380 hits the bottom");
        assert!(v.selected >= v.scroll && v.selected < v.scroll + 10);
        v.vbar = Some((zone, track, Rect::new(986, 360, 12, 40)));
        // Drag to the top, even past the track.
        assert!(v.pointer_move(700, -50));
        assert_eq!(v.scroll, 0);
        assert!(v.pointer_up(700, -50));
        assert!(!v.dragging_scrollbar());
        assert!(!v.vbar_hover, "released away from the bar: narrow again");
        // Grabbing the thumb itself keeps the grab offset: no jump.
        v.vbar = Some((zone, track, Rect::new(986, 0, 12, 40)));
        v.click(990, 30);
        assert_eq!(v.scroll, 0);
        v.pointer_move(990, 210);
        assert_eq!(v.scroll, 45, "thumb top at 180 of 360");
    }
}
