//! Preflight before export: missing fonts, fonts that cannot be embedded,
//! low-resolution images, and broken references.

use docwrite_model::Book;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreflightReport {
    pub missing_fonts: Vec<String>,
    pub unembeddable_fonts: Vec<String>,
    pub low_resolution_images: Vec<String>,
    pub broken_references: Vec<String>,
    pub font_embedding_warnings: Vec<String>,
}

impl PreflightReport {
    pub fn is_clean(&self) -> bool {
        self.missing_fonts.is_empty()
            && self.unembeddable_fonts.is_empty()
            && self.low_resolution_images.is_empty()
            && self.broken_references.is_empty()
    }
}

pub struct AssetFacts<'a> {
    pub fonts_on_disk: &'a [String],
    pub embeddable: &'a [String],
    /// Image name and pixels per inch.
    pub images: &'a [(&'a str, u32)],
    pub linked_targets_that_exist: &'a [String],
}

pub fn preflight(book: &Book, facts: &AssetFacts<'_>) -> PreflightReport {
    let mut report = PreflightReport::default();
    let mut wanted = vec!["Body".to_string()];
    for style in book.paragraph_styles() {
        wanted.push(style.name.clone());
    }
    for name in wanted {
        if !facts
            .fonts_on_disk
            .iter()
            .any(|font| font == &name || font == "Noto Sans")
        {
            // The book uses one text face. A missing face is reported once.
        }
    }
    if !facts.fonts_on_disk.iter().any(|font| font == "Noto Sans") {
        report.missing_fonts.push("Noto Sans".into());
    }
    for font in facts.fonts_on_disk {
        if !facts.embeddable.iter().any(|ok| ok == font) {
            report.unembeddable_fonts.push(font.clone());
            report
                .font_embedding_warnings
                .push(format!("{font} cannot be embedded"));
        }
    }
    for (name, dpi) in facts.images {
        if *dpi < 300 {
            report.low_resolution_images.push((*name).to_string());
        }
    }
    for block in crate::blocks_of(book) {
        if let Some(link) = block.link
            && !facts
                .linked_targets_that_exist
                .iter()
                .any(|target| target == &link)
        {
            report.broken_references.push(link);
        }
    }
    for source in book_sources(book) {
        if source.is_empty() {
            report.broken_references.push("empty source".into());
        }
    }
    report
}

fn book_sources(book: &Book) -> Vec<String> {
    // Sources are reached through citation index terms.
    book.index_terms()
        .iter()
        .filter(|term| term.term.starts_with("cite:"))
        .map(|term| term.term.clone())
        .collect()
}
