//! Find and replace across the book.

use crate::error::ModelError;
use crate::tree::{Book, Position, Selection};

impl Book {
    /// The next occurrence of `query` at or after `from`, searching block by
    /// block and wrapping round to the start of the book. Case is ignored.
    /// Returns the match's start and end.
    pub fn find(&self, query: &str, from: Position) -> Option<(Position, Position)> {
        let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        if needle.is_empty() {
            return None;
        }
        let ids = self.block_ids();
        let start = ids.iter().position(|id| *id == from.block).unwrap_or(0);
        let order = (start..ids.len()).chain(0..=start);
        for (pass, index) in order.enumerate() {
            let id = ids[index];
            let Ok(block) = self.block(id) else {
                continue;
            };
            let chars: Vec<char> = block.text().chars().collect();
            // First pass over the start block begins at `from`; the wrapped
            // pass over it covers what lies before.
            let (low, high) = if pass == 0 {
                (from.offset, chars.len())
            } else if index == start {
                (0, from.offset + needle.len())
            } else {
                (0, chars.len())
            };
            if let Some(offset) = find_in(&chars, &needle, low, high.min(chars.len())) {
                return Some((
                    Position::new(id, offset),
                    Position::new(id, offset + needle.len()),
                ));
            }
        }
        None
    }

    /// Select the next occurrence of `query` after the selection. Returns
    /// whether one was found.
    pub fn find_next(&mut self, query: &str) -> bool {
        let selection = self.selection();
        // Search from the end of the selection, so a found match is skipped.
        let from = if self.position_before(selection.anchor, selection.focus) {
            selection.focus
        } else {
            selection.anchor
        };
        match self.find(query, from) {
            Some((start, end)) => self
                .set_selection(Selection {
                    anchor: start,
                    focus: end,
                })
                .is_ok(),
            None => false,
        }
    }

    /// Replace every occurrence of `query` with `with`, as one undo step.
    /// Returns how many were replaced.
    pub fn replace_all(&mut self, query: &str, with: &str) -> Result<usize, ModelError> {
        let before = self.selection();
        let undo_depth = self.undo.len();
        let first_block = *self
            .block_ids()
            .first()
            .ok_or(ModelError::Inconsistent("book has no blocks"))?;
        let mut from = Position::new(first_block, 0);
        let mut count = 0;
        let limit = self.plain_text().chars().count() + 1;
        while let Some((start, end)) = self.find(query, from) {
            // `find` wraps; stop once it comes back round.
            if count > 0 && !self.position_before(from, start) && from != start {
                break;
            }
            self.set_selection(Selection {
                anchor: start,
                focus: end,
            })?;
            self.insert(with)?;
            count += 1;
            from = self.selection().focus;
            if count > limit {
                break;
            }
        }
        self.merge_undo_since(undo_depth, before);
        if count == 0 {
            let _ = self.set_selection(before);
        }
        Ok(count)
    }

    /// Whether `a` comes before `b` in reading order.
    fn position_before(&self, a: Position, b: Position) -> bool {
        let ids = self.block_ids();
        let index = |position: Position| ids.iter().position(|id| *id == position.block);
        match (index(a), index(b)) {
            (Some(ia), Some(ib)) => (ia, a.offset) < (ib, b.offset),
            _ => false,
        }
    }
}

fn find_in(chars: &[char], needle: &[char], low: usize, high: usize) -> Option<usize> {
    if needle.len() > chars.len() || low >= high {
        return None;
    }
    let lower = |ch: &char| ch.to_lowercase().next().unwrap_or(*ch);
    (low..=high
        .saturating_sub(needle.len())
        .min(chars.len() - needle.len()))
        .find(|at| {
            chars[*at..*at + needle.len()]
                .iter()
                .zip(needle)
                .all(|(have, want)| lower(have) == *want)
        })
}
