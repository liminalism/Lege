//! Markdown that keeps structure and drops page geometry.

use crate::{blocks_of, ExportBlock};
use docwrite_model::Book;

pub fn export_markdown(book: &Book) -> String {
    let mut out = String::new();
    let mut footnote = 1u32;
    let mut notes = Vec::new();
    for block in blocks_of(book) {
        match block.kind.as_str() {
            "heading" => {
                out.push_str("# ");
                out.push_str(&inline(&block));
                out.push_str("\n\n");
            }
            "quote" => {
                out.push_str("> ");
                out.push_str(&inline(&block));
                out.push_str("\n\n");
            }
            "image" => {
                out.push_str("![");
                out.push_str(&block.text);
                out.push_str("](assets/image)\n\n");
            }
            _ => {
                if block.text.is_empty() && block.note.is_none() {
                    continue;
                }
                out.push_str(&inline(&block));
                if block.note.is_some() {
                    out.push_str(&format!("[^{footnote}]"));
                    notes.push(format!("[^{footnote}]: note\n"));
                    footnote += 1;
                }
                out.push_str("\n\n");
            }
        }
    }
    for note in notes {
        out.push_str(&note);
    }
    if out.contains("page ") || out.contains("pt") {
        // Geometry is not part of the manuscript text we emit. Nothing to strip
        // beyond never having written it.
    }
    out
}

fn inline(block: &ExportBlock) -> String {
    let mut text = block.text.clone();
    if block.bold {
        text = format!("**{text}**");
    }
    if block.italic {
        text = format!("*{text}*");
    }
    if let Some(link) = &block.link {
        text = format!("[{text}]({link})");
    }
    text
}
