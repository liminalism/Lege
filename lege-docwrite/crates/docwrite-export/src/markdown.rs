//! Markdown that keeps structure and drops page geometry.

use crate::{ExportBlock, blocks_of};
use docwrite_model::Book;

/// Markdown for `book`. Page size, margins and folios are not written.
///
/// ```
/// use docwrite_export::export_markdown;
/// use docwrite_model::Book;
/// let mut book = Book::new("Essay");
/// assert!(book.insert("The river.").is_ok());
/// let markdown = export_markdown(&book);
/// assert!(markdown.contains("The river."));
/// assert!(!markdown.contains("margin_inner"));
/// ```
pub fn export_markdown(book: &Book) -> String {
    let mut out = String::new();
    let mut footnote = 1u32;
    let mut notes = Vec::new();
    for block in blocks_of(book) {
        match block.kind.as_str() {
            "heading" => {
                out.push_str("# ");
                out.push_str(&inline(&block));
                push_note(&block, &mut footnote, &mut notes, &mut out);
                out.push_str("\n\n");
            }
            "quote" => {
                out.push_str("> ");
                out.push_str(&inline(&block));
                push_note(&block, &mut footnote, &mut notes, &mut out);
                out.push_str("\n\n");
            }
            "image" => {
                out.push_str("![");
                out.push_str(&block.text);
                out.push_str("](assets/image)");
                push_note(&block, &mut footnote, &mut notes, &mut out);
                out.push_str("\n\n");
            }
            _ => {
                if block.text.is_empty() && block.note.is_none() {
                    continue;
                }
                out.push_str(&inline(&block));
                push_note(&block, &mut footnote, &mut notes, &mut out);
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

fn push_note(block: &ExportBlock, footnote: &mut u32, notes: &mut Vec<String>, out: &mut String) {
    if let Some(note) = &block.note {
        out.push_str(&format!("[^{footnote}]"));
        notes.push(format!("[^{footnote}]: {note}\n"));
        *footnote += 1;
    }
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

#[cfg(test)]
mod tests {
    use super::export_markdown;
    use docwrite_model::Book;

    #[test]
    fn markdown_keeps_the_sentence_and_drops_page_geometry() {
        let mut book = Book::new("Essay");
        assert!(book.insert("The river.").is_ok());
        let markdown = export_markdown(&book);
        assert!(markdown.contains("The river."), "{markdown}");
        assert!(!markdown.contains("margin_inner"), "{markdown}");
        assert!(!markdown.contains("width_pt"), "{markdown}");
    }
}
