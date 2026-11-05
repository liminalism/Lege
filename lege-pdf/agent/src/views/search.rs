use serde::Serialize;

use super::text::TextProvenance;

#[derive(Debug, Clone, Serialize)]
pub struct SearchPageData {
    pub unit: &'static str,
    pub query: String,
    pub match_count: usize,
    pub matches: Vec<MatchView>,
    pub provenance: TextProvenance,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchView {
    pub text: String,
    pub context: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::rounded_box_opt"
    )]
    pub bbox: Option<[f64; 4]>,
    pub first_char: usize,
    pub char_count: usize,
}

/// What a search run covered. Pages without a match are counted here rather
/// than reported one by one.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchSummary {
    pub query: String,
    pub pages_selected: usize,
    pub pages_searched: usize,
    pub pages_with_matches: usize,
    pub match_count: usize,
    /// Searched pages with no text layer at all — scans, typically. They can
    /// only be searched with OCR.
    pub pages_without_text: usize,
    pub pages_failed: usize,
    /// The run stopped before the last selected page.
    pub truncated: bool,
    /// Where to resume a run that stopped early.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<u32>,
    #[serde(skip)]
    pub timed_out: bool,
    #[serde(skip)]
    pub stop_reason: Option<String>,
    #[serde(skip)]
    pub page_notes: Vec<String>,
}

impl SearchSummary {
    /// Warnings from pages that are not reported themselves, kept few.
    const MAX_PAGE_NOTES: usize = 12;

    pub fn stop(&mut self, next_page: u32, reason: String) {
        self.truncated = true;
        self.next_page = Some(next_page);
        self.stop_reason = Some(reason);
    }

    /// Account for the selected pages a page limit kept the run from reading.
    pub fn limited(&mut self, selected: usize, next_page: Option<u32>) {
        self.pages_selected = selected;
        if let (false, Some(next)) = (self.truncated, next_page) {
            self.truncated = true;
            self.next_page = Some(next);
        }
    }

    pub fn keep_notes(&mut self, page: crate::pages::PageZero, warnings: Vec<String>) {
        for warning in warnings {
            if self.page_notes.len() < Self::MAX_PAGE_NOTES {
                self.page_notes
                    .push(format!("page {}: {warning}", page.one_based()));
            }
        }
    }

    /// The run-level warnings: why it stopped, what it could not read.
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if let (Some(reason), Some(next)) = (&self.stop_reason, self.next_page) {
            notes.push(format!(
                "{reason}: {} of {} selected pages searched; resume from page {next}",
                self.pages_searched, self.pages_selected
            ));
        }
        if self.pages_without_text > 0 {
            notes.push(format!(
                "{} of {} searched pages have no text layer and were not searchable; search them with OCR (ocr=auto)",
                self.pages_without_text, self.pages_searched
            ));
        }
        notes.extend(self.page_notes.iter().cloned());
        notes
    }
}
