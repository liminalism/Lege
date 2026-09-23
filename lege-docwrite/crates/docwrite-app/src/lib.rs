//! Headless editor behavior the window hosts: paging, snap, and the latency budget.

mod nav;

use std::time::Duration;

pub use nav::{Pager, Phase};

use docwrite_model::Book;

/// The manuscript and the page viewport the window draws.
pub struct Editor {
    book: Book,
    pager: Pager,
}

impl Editor {
    pub fn new() -> Self {
        let mut book = Book::new("Untitled");
        let _ = book.insert("The book opens on a page.");
        Self {
            book,
            pager: Pager::new(8, false),
        }
    }

    pub fn book(&self) -> &Book {
        &self.book
    }

    pub fn pager(&self) -> &Pager {
        &self.pager
    }

    pub fn type_text(&mut self, text: &str) {
        let _ = self.book.insert(text);
    }

    pub fn backspace(&mut self) {
        let _ = self.book.delete_backward();
    }

    pub fn page_down(&mut self) {
        self.pager.page_down();
    }

    pub fn page_up(&mut self) {
        self.pager.page_up();
    }

    pub fn scroll_gesture(&mut self, delta: f64, phase: Phase) {
        self.pager.scroll_gesture(delta, phase);
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

/// Keypress-to-present budget. A few milliseconds, inside one 60 Hz frame.
pub const KEYPRESS_BUDGET: Duration = Duration::from_millis(8);

/// True when opening a window is not expected to succeed.
pub fn display_unavailable() -> bool {
    if std::env::var_os("LEGE_DOCWRITE_HEADLESS").is_some() {
        return true;
    }
    cfg!(unix) && !cfg!(target_os = "macos") && std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()
}
