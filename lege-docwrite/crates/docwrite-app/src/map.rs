//! Book Map: front matter, parts, chapters, sections, and back matter.
//!
//! Dragging one chapter row onto another calls [`Book::reorder_chapter`].
//! A click on a part or chapter collapses the rows under it.

use docwrite_model::{Book, ChapterId, ModelError, PartId};

/// Width of the sidebar the window paints and hit-tests.
pub const SIDEBAR_W: i32 = 220;
const ROW_H: i32 = 28;
const ROW_TOP: i32 = 16;

/// What a sidebar row represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    /// A part titled "Front matter".
    FrontMatter,
    /// A body part.
    Part,
    /// A part titled "Back matter".
    BackMatter,
    /// A chapter that can be dragged.
    Chapter,
    /// A section under a chapter.
    Section,
}

/// One visible row. Hidden children of a collapsed part or chapter are absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapRow {
    pub kind: MapKind,
    pub title: String,
    /// Chapter index in reading order, when this row is a chapter.
    pub chapter_index: Option<usize>,
    pub part_id: Option<PartId>,
    pub chapter_id: Option<ChapterId>,
}

/// Collapse state for the sidebar. Rows are derived from the book on each read.
#[derive(Clone, Debug, Default)]
pub struct BookMap {
    collapsed_parts: Vec<PartId>,
    collapsed_chapters: Vec<ChapterId>,
}

impl BookMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// The center of row `index`, in window pixels. Hit testing uses this grid.
    pub fn row_center(index: usize) -> (f32, f32) {
        (
            SIDEBAR_W as f32 / 2.0,
            ROW_TOP as f32 + index as f32 * ROW_H as f32 + ROW_H as f32 / 2.0,
        )
    }

    pub fn row_at(&self, x: f32, y: f32, nrows: usize) -> Option<usize> {
        // The caller decides whether the point is inside the (resizable)
        // sidebar; this only maps a height to a row.
        if x < 0.0 || nrows == 0 {
            return None;
        }
        let index = (y as i32 - ROW_TOP) / ROW_H;
        if index < 0 || index as usize >= nrows {
            None
        } else {
            Some(index as usize)
        }
    }

    pub fn rows(&self, book: &Book) -> Vec<MapRow> {
        let mut rows = Vec::new();
        let mut chapter_index = 0usize;
        for part in book.parts() {
            let kind = match part.title() {
                "Front matter" => MapKind::FrontMatter,
                "Back matter" => MapKind::BackMatter,
                _ => MapKind::Part,
            };
            rows.push(MapRow {
                kind,
                title: part.title().to_string(),
                chapter_index: None,
                part_id: Some(part.id()),
                chapter_id: None,
            });
            if self.collapsed_parts.contains(&part.id()) {
                chapter_index += part.chapters().len();
                continue;
            }
            for chapter in part.chapters() {
                let index = chapter_index;
                chapter_index += 1;
                rows.push(MapRow {
                    kind: MapKind::Chapter,
                    title: chapter.title().to_string(),
                    chapter_index: Some(index),
                    part_id: Some(part.id()),
                    chapter_id: Some(chapter.id()),
                });
                if self.collapsed_chapters.contains(&chapter.id()) {
                    continue;
                }
                for section in chapter.sections() {
                    let title = if section.heading().is_empty() {
                        "Section".to_string()
                    } else {
                        section.heading().to_string()
                    };
                    rows.push(MapRow {
                        kind: MapKind::Section,
                        title,
                        chapter_index: None,
                        part_id: Some(part.id()),
                        chapter_id: Some(chapter.id()),
                    });
                }
            }
        }
        rows
    }

    /// Collapse or expand the part or chapter on `row`.
    pub fn activate(&mut self, book: &Book, row: usize) {
        let Some(entry) = self.rows(book).get(row).cloned() else {
            return;
        };
        match entry.kind {
            MapKind::FrontMatter | MapKind::Part | MapKind::BackMatter => {
                if let Some(id) = entry.part_id {
                    toggle(&mut self.collapsed_parts, id);
                }
            }
            MapKind::Chapter => {
                if let Some(id) = entry.chapter_id {
                    toggle(&mut self.collapsed_chapters, id);
                }
            }
            MapKind::Section => {}
        }
    }

    /// Move the chapter on `from_row` to the chapter on `to_row`.
    pub fn drag_chapter(
        &self,
        book: &mut Book,
        from_row: usize,
        to_row: usize,
    ) -> Result<(), ModelError> {
        let rows = self.rows(book);
        let from = rows.get(from_row).and_then(|row| {
            (row.kind == MapKind::Chapter)
                .then_some(row.chapter_index)
                .flatten()
        });
        let to = rows.get(to_row).and_then(|row| {
            (row.kind == MapKind::Chapter)
                .then_some(row.chapter_index)
                .flatten()
        });
        let (Some(from), Some(to)) = (from, to) else {
            return Err(ModelError::Inconsistent("drag a chapter onto a chapter"));
        };
        book.reorder_chapter(from, to)
    }
}

fn toggle<T: Copy + PartialEq>(items: &mut Vec<T>, id: T) {
    if let Some(index) = items.iter().position(|item| *item == id) {
        items.remove(index);
    } else {
        items.push(id);
    }
}
