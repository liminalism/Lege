//! Caret steps inside a single block's text.
//!
//! Character motion walks extended grapheme clusters. Word motion walks
//! Unicode word boundaries and stops on words that contain a letter or number,
//! so a combining mark stays with its base character.

use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn byte_index(text: &str, char_offset: usize) -> usize {
    text.chars().take(char_offset).map(char::len_utf8).sum()
}

fn char_index_at_byte(text: &str, byte: usize) -> Option<usize> {
    text.get(..byte).map(|prefix| prefix.chars().count())
}

/// Next grapheme boundary strictly after `char_offset`, or `char_offset` at the end.
pub(crate) fn next_grapheme(text: &str, char_offset: usize) -> usize {
    let byte = byte_index(text, char_offset);
    let Some(rest) = text.get(byte..) else {
        return char_offset;
    };
    match rest.graphemes(true).next() {
        Some(cluster) => char_offset + cluster.chars().count(),
        None => char_offset,
    }
}

/// Previous grapheme boundary strictly before `char_offset`, or 0.
pub(crate) fn prev_grapheme(text: &str, char_offset: usize) -> usize {
    if char_offset == 0 {
        return 0;
    }
    let byte = byte_index(text, char_offset);
    let Some(head) = text.get(..byte) else {
        return char_offset;
    };
    match head.graphemes(true).next_back() {
        Some(cluster) => char_offset - cluster.chars().count(),
        None => char_offset,
    }
}

fn is_word(segment: &str) -> bool {
    segment.chars().any(|ch| ch.is_alphanumeric())
}

/// Character offset at the end of the next word after `char_offset`.
///
/// `None` means the offset is already at the end of the block, so the caller
/// crosses into the next block. Punctuation at the end of the block with no
/// following word lands on the block end first.
pub(crate) fn word_forward(text: &str, char_offset: usize) -> Option<usize> {
    let len = text.chars().count();
    if char_offset >= len {
        return None;
    }
    let byte = byte_index(text, char_offset);
    for (start, word) in text.split_word_bound_indices() {
        let end = start + word.len();
        if end <= byte || !is_word(word) {
            continue;
        }
        let off = char_index_at_byte(text, end)?;
        if off > char_offset {
            return Some(off);
        }
    }
    if char_offset < len { Some(len) } else { None }
}

/// Character offset at the start of the word before `char_offset`.
///
/// `None` means the offset is already 0, so the caller crosses into the
/// previous block.
pub(crate) fn word_backward(text: &str, char_offset: usize) -> Option<usize> {
    if char_offset == 0 {
        return None;
    }
    let byte = byte_index(text, char_offset);
    let mut start_of_last = None;
    for (start, word) in text.split_word_bound_indices() {
        if start >= byte {
            break;
        }
        if is_word(word) {
            start_of_last = char_index_at_byte(text, start);
        }
    }
    match start_of_last {
        Some(offset) if offset < char_offset => Some(offset),
        _ => Some(0),
    }
}
