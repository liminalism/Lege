//! Exact page and spread navigation.
//!
//! `PageDown` lands on the next page top from any scroll position. In spread
//! mode it lands on the next spread. A scroll gesture stays continuous while
//! it moves and settles on the nearer page, or spread, when it ends.

/// Gesture phase, matching the pixelkit scroll seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// A gesture began.
    Started,
    /// It moved.
    Moved,
    /// It ended: the view settles on a page.
    Ended,
    /// The platform cancelled it: the view settles too.
    Cancelled,
}

/// Viewport over a sequence of equal pages.
#[derive(Clone, Debug)]
pub struct Pager {
    pages: u32,
    spread: bool,
    /// Scroll position in pages. `2.0` is the top of page 3 (0-based index 2).
    scroll: f64,
    origin: Option<f64>,
}

impl Pager {
    /// A pager over `pages` pages, by page or (with `spread`) by spread.
    pub fn new(pages: u32, spread: bool) -> Self {
        Self {
            pages: pages.max(1),
            spread,
            scroll: 0.0,
            origin: None,
        }
    }

    /// Follow a document whose page count changed. The scroll position is
    /// kept, clamped to the new last page.
    pub fn set_pages(&mut self, pages: u32) {
        self.pages = pages.max(1);
        self.scroll = self.scroll.clamp(0.0, self.last());
    }

    /// Land on the top of 0-based page `index`, or in spread mode on the
    /// spread that holds it (spread `k` holds pages `2k` and `2k + 1`).
    pub fn jump_to(&mut self, index: u32) {
        let target = if self.spread {
            f64::from(index.div_ceil(2) * 2)
        } else {
            f64::from(index)
        };
        self.scroll = target.clamp(0.0, self.last());
        self.origin = None;
    }

    /// Whether the pager moves by spreads.
    pub fn is_spread(&self) -> bool {
        self.spread
    }

    /// Pages the pager navigates.
    pub fn pages(&self) -> u32 {
        self.pages
    }

    /// The scroll position in pages from the top of the first page.
    pub fn scroll(&self) -> f64 {
        self.scroll
    }

    /// The page (0-based) at the top of the view.
    pub fn page_top(&self) -> u32 {
        self.scroll.round().clamp(0.0, self.last()) as u32
    }

    /// Go to the next page top, or next spread.
    pub fn page_down(&mut self) {
        self.scroll = self.forward_target();
        self.origin = None;
    }

    /// Go to the previous page top, or previous spread.
    pub fn page_up(&mut self) {
        self.scroll = self.backward_target();
        self.origin = None;
    }

    /// `delta` is in pages. Positive moves toward later pages.
    pub fn scroll_gesture(&mut self, delta: f64, phase: Phase) {
        match phase {
            Phase::Started => {
                self.origin = Some(self.scroll);
                self.scroll = (self.scroll + delta).clamp(0.0, self.last());
            }
            Phase::Moved => {
                let base = self.origin.unwrap_or(self.scroll);
                // `delta` is the gesture total from the start, not a step.
                self.scroll = (base + delta).clamp(0.0, self.last());
            }
            Phase::Ended | Phase::Cancelled => {
                let base = self.origin.take().unwrap_or(self.scroll);
                self.scroll = (base + delta).clamp(0.0, self.last());
                self.snap();
            }
        }
    }

    fn forward_target(&self) -> f64 {
        if self.spread {
            let spread = (self.scroll / 2.0).floor();
            ((spread + 1.0) * 2.0).min(self.last_spread())
        } else {
            (self.scroll.floor() + 1.0).min(self.last())
        }
    }

    fn backward_target(&self) -> f64 {
        if self.spread {
            let aligned = (self.scroll / 2.0).floor() * 2.0;
            let previous = if (self.scroll - aligned).abs() < 1e-9 {
                aligned - 2.0
            } else {
                aligned
            };
            previous.max(0.0)
        } else if self.scroll.fract().abs() < 1e-9 {
            (self.scroll - 1.0).max(0.0)
        } else {
            self.scroll.floor()
        }
    }

    fn snap(&mut self) {
        if self.spread {
            self.scroll = (self.scroll / 2.0).round() * 2.0;
            self.scroll = self.scroll.clamp(0.0, self.last_spread());
        } else {
            self.scroll = self.scroll.round().clamp(0.0, self.last());
        }
    }

    fn last(&self) -> f64 {
        if self.spread {
            // Spread k holds pages 2k and 2k + 1; the last one holds the last page.
            f64::from(self.pages / 2 * 2)
        } else {
            self.pages.saturating_sub(1) as f64
        }
    }

    fn last_spread(&self) -> f64 {
        let last = self.last();
        (last / 2.0).floor() * 2.0
    }
}
