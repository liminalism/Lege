//! PDF, Markdown and IDML export, plus preflight.

mod idml;
mod markdown;
mod pdf;
mod preflight;

pub use idml::export_idml;
pub use markdown::export_markdown;
pub use pdf::export_pdf;
pub use preflight::{preflight, AssetFacts, PreflightReport};

use docwrite_model::{BlockKind, Book};

/// One exportable block, flattened from the book in reading order.
#[derive(Clone, Debug)]
pub struct ExportBlock {
    pub kind: String,
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub link: Option<String>,
    pub note: Option<String>,
    pub chapter: String,
}

pub fn blocks_of(book: &Book) -> Vec<ExportBlock> {
    let mut out = Vec::new();
    for part in book.parts() {
        for chapter in part.chapters() {
            for section in chapter.sections() {
                for block in section.blocks() {
                    let bold = block.runs().iter().any(|run| run.marks.bold);
                    let italic = block.runs().iter().any(|run| run.marks.italic);
                    let link = block
                        .runs()
                        .iter()
                        .find_map(|run| run.marks.link.clone());
                    out.push(ExportBlock {
                        kind: match block.kind() {
                            BlockKind::ChapterTitle => "heading".into(),
                            BlockKind::Subhead => "heading".into(),
                            BlockKind::BlockQuote => "quote".into(),
                            BlockKind::Image { .. } => "image".into(),
                            BlockKind::Caption => "caption".into(),
                            _ => "paragraph".into(),
                        },
                        text: block.text(),
                        bold,
                        italic,
                        link,
                        note: block.note().map(|id| format!("{id}")),
                        chapter: chapter.title().to_string(),
                    });
                }
            }
        }
    }
    out
}
