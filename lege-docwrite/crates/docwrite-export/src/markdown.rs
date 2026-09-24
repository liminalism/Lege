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

/// A block's text with each run's emphasis and link, run by run.
fn inline(block: &ExportBlock) -> String {
    if block.runs.is_empty() {
        return block.text.clone();
    }
    let chars: Vec<char> = block.text.chars().collect();
    let mut out = String::new();
    for run in &block.runs {
        let piece: String = chars
            .get(run.start..run.end.min(chars.len()))
            .unwrap_or(&[])
            .iter()
            .collect();
        if piece.trim().is_empty() {
            out.push_str(&piece);
            continue;
        }
        // Markers hug the words: spaces at a run's edges stay outside them.
        let lead = piece.len() - piece.trim_start().len();
        let trail = piece.len() - piece.trim_end().len();
        let core = piece.trim();
        let mut text = core.to_string();
        if run.marks.italic {
            text = format!("*{text}*");
        }
        if run.marks.bold {
            text = format!("**{text}**");
        }
        if let Some(link) = &run.marks.link {
            text = format!("[{text}]({link})");
        }
        out.push_str(&piece[..lead]);
        out.push_str(&text);
        out.push_str(&piece[piece.len() - trail..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::export_markdown;
    use docwrite_model::Book;

    #[test]
    fn emphasis_and_links_cover_only_their_own_words() {
        use docwrite_model::{Mark, Position, RunMarks, Selection};
        let mut book = Book::new("Marks");
        assert!(book.insert("A plain sentence with one bold word.").is_ok());
        let block = book.block_ids()[0];
        assert!(
            book.set_selection(Selection {
                anchor: Position::new(block, 26),
                focus: Position::new(block, 30),
            })
            .is_ok()
        );
        assert!(book.toggle_mark(Mark::Bold).is_ok());
        assert!(
            book.set_selection(Selection::collapsed(Position::new(block, 36)))
                .is_ok()
        );
        let link = RunMarks {
            link: Some("https://example.test".into()),
            ..RunMarks::default()
        };
        assert!(book.insert_with_marks(" See this.", link).is_ok());
        let markdown = export_markdown(&book);
        assert!(
            markdown.contains(
                "A plain sentence with one **bold** word. [See this.](https://example.test)"
            ),
            "{markdown}"
        );
    }

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
