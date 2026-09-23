//! Keypress timing and a replayable typing trace.
//!
//! Adapted from lege-viewer's frame metrics and input trace. A frame is
//! input, then compose (the edit or the page motion), then present (the
//! page state the window would draw). Replay drives the shipped paginator.

use std::time::{Duration, Instant};

use docwrite_typeset::{Document, TypesetError};

use crate::nav::{Pager, Phase};

/// One frame from an input event through present.
#[derive(Clone, Debug, Default)]
pub struct FrameMetrics {
    input_received: Option<Instant>,
    frame_started: Option<Instant>,
    compose_finished: Option<Instant>,
    present_finished: Option<Instant>,
    /// Time spent in the edit or page motion.
    pub compose_time: Duration,
    /// Time spent publishing the resulting page state.
    pub present_time: Duration,
}

impl FrameMetrics {
    /// Stamp the moment the input arrived.
    pub fn begin_input(&mut self) {
        self.input_received = Some(Instant::now());
    }

    /// Stamp the start of the work that input caused.
    pub fn begin_frame(&mut self) {
        self.frame_started = Some(Instant::now());
    }

    /// The edit or motion has finished.
    pub fn finish_compose(&mut self) {
        let now = Instant::now();
        self.compose_finished = Some(now);
        self.compose_time = self
            .frame_started
            .map_or(Duration::ZERO, |start| now.saturating_duration_since(start));
    }

    /// The resulting page state is what the window would present.
    pub fn finish_present(&mut self) {
        let now = Instant::now();
        self.present_finished = Some(now);
        self.present_time = self.compose_finished.map_or(Duration::ZERO, |compose| {
            now.saturating_duration_since(compose)
        });
    }

    /// Input event to present. `None` until both stamps exist.
    pub fn input_to_present(&self) -> Option<Duration> {
        Some(
            self.present_finished?
                .saturating_duration_since(self.input_received?),
        )
    }
}

/// One command in a typing trace.
#[derive(Clone, Debug, PartialEq)]
pub enum TraceCommand {
    /// Insert `text` at the paragraph that opens 1-based `page`.
    Type { page: u32, text: String },
    /// Move to the next page top.
    PageDown,
    /// A scroll-gesture sample. `delta` is in pages.
    Scroll { delta: f64, phase: Phase },
}

/// A sequence of editor commands, replayed against a document and pager.
#[derive(Clone, Debug, Default)]
pub struct InputTrace {
    events: Vec<TraceCommand>,
}

/// What one replayed command did.
#[derive(Clone, Debug)]
pub struct ReplayStep {
    /// Timing for this command.
    pub frame: FrameMetrics,
    /// Pages the edit rebuilt. Empty when the command did not edit text.
    pub pages_laid_out: Vec<u32>,
}

impl InputTrace {
    /// Append a command.
    pub fn push(&mut self, command: TraceCommand) {
        self.events.push(command);
    }

    /// Run every command on the shipped paginator and pager.
    pub fn replay(
        &self,
        document: &mut Document,
        pager: &mut Pager,
    ) -> Result<Vec<ReplayStep>, TypesetError> {
        let mut steps = Vec::with_capacity(self.events.len());
        for command in &self.events {
            let mut frame = FrameMetrics::default();
            frame.begin_input();
            frame.begin_frame();
            let pages_laid_out = match command {
                TraceCommand::Type { page, text } => {
                    document.edit_page(*page, text)?.pages_laid_out
                }
                TraceCommand::PageDown => {
                    pager.page_down();
                    Vec::new()
                }
                TraceCommand::Scroll { delta, phase } => {
                    pager.scroll_gesture(*delta, *phase);
                    Vec::new()
                }
            };
            frame.finish_compose();
            let _presented = (document.page_count(), pager.page_top());
            frame.finish_present();
            steps.push(ReplayStep {
                frame,
                pages_laid_out,
            });
        }
        Ok(steps)
    }
}
