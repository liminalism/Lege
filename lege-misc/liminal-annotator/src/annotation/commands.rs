//! The page document and its undo history. One gesture is one [`Edit`];
//! undo restores the state before the gesture began.

use super::geometry::Box2;

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    /// Stable id. Imported boxes keep their import id, so the comparison
    /// with the immutable imported layer is a join on id.
    pub id: String,
    pub class_id: u8,
    pub bbox: Box2,
    pub is_ignore: bool,
    /// Imported per-box extras (`source`, `score`, `teachers`, ...) as JSON,
    /// carried through to export untouched.
    pub provenance_json: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReviewStatus {
    Unseen,
    Opened,
    Reviewed,
    Excluded,
    NeedsAttention,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewStatus::Unseen => "unseen",
            ReviewStatus::Opened => "opened",
            ReviewStatus::Reviewed => "reviewed",
            ReviewStatus::Excluded => "excluded",
            ReviewStatus::NeedsAttention => "needs_attention",
        }
    }

    pub fn parse(s: &str) -> Option<ReviewStatus> {
        Some(match s {
            "unseen" => ReviewStatus::Unseen,
            "opened" => ReviewStatus::Opened,
            "reviewed" => ReviewStatus::Reviewed,
            "excluded" => ReviewStatus::Excluded,
            "needs_attention" => ReviewStatus::NeedsAttention,
            _ => return None,
        })
    }

    pub const ALL: [ReviewStatus; 5] = [
        ReviewStatus::Unseen,
        ReviewStatus::Opened,
        ReviewStatus::Reviewed,
        ReviewStatus::Excluded,
        ReviewStatus::NeedsAttention,
    ];
}

#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Add {
        index: usize,
        ann: Annotation,
    },
    Remove {
        index: usize,
        ann: Annotation,
    },
    Modify {
        before: Annotation,
        after: Annotation,
    },
    Status {
        before: ReviewStatus,
        after: ReviewStatus,
    },
    /// The whole box set at once (page revert): one undo step.
    Replace {
        before: Vec<Annotation>,
        after: Vec<Annotation>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct UndoStack {
    done: Vec<Edit>,
    undone: Vec<Edit>,
}

const UNDO_LIMIT: usize = 1000;

impl UndoStack {
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }
    pub fn len(&self) -> usize {
        self.done.len()
    }
    pub fn is_empty(&self) -> bool {
        self.done.is_empty()
    }
}

/// One page's editable state.
#[derive(Clone, Debug)]
pub struct PageDoc {
    pub page_key: String,
    pub width: f64,
    pub height: f64,
    pub annotations: Vec<Annotation>,
    pub status: ReviewStatus,
    pub history: UndoStack,
    /// Bumped on every change; the writer acknowledges revisions.
    pub revision: u64,
}

impl PageDoc {
    pub fn new(
        page_key: String,
        width: f64,
        height: f64,
        annotations: Vec<Annotation>,
        status: ReviewStatus,
    ) -> PageDoc {
        PageDoc {
            page_key,
            width,
            height,
            annotations,
            status,
            history: UndoStack::default(),
            revision: 0,
        }
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.annotations.iter().position(|a| a.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    /// Apply a new edit and record it. Returns false (and records nothing)
    /// when the edit is a no-op.
    pub fn apply(&mut self, edit: Edit) -> bool {
        if let Edit::Modify { before, after } = &edit {
            if before == after {
                return false;
            }
        }
        if let Edit::Status { before, after } = &edit {
            if before == after {
                return false;
            }
        }
        if let Edit::Replace { before, after } = &edit {
            if before == after {
                return false;
            }
        }
        if !self.forward(&edit) {
            return false;
        }
        self.history.done.push(edit);
        if self.history.done.len() > UNDO_LIMIT {
            self.history.done.remove(0);
        }
        self.history.undone.clear();
        self.revision += 1;
        true
    }

    pub fn undo(&mut self) -> Option<Edit> {
        let edit = self.history.done.pop()?;
        self.backward(&edit);
        self.history.undone.push(edit.clone());
        self.revision += 1;
        Some(edit)
    }

    pub fn redo(&mut self) -> Option<Edit> {
        let edit = self.history.undone.pop()?;
        self.forward(&edit);
        self.history.done.push(edit.clone());
        self.revision += 1;
        Some(edit)
    }

    fn forward(&mut self, edit: &Edit) -> bool {
        match edit {
            Edit::Add { index, ann } => {
                let i = (*index).min(self.annotations.len());
                self.annotations.insert(i, ann.clone());
                true
            }
            Edit::Remove { ann, .. } => match self.index_of(&ann.id) {
                Some(i) => {
                    self.annotations.remove(i);
                    true
                }
                None => false,
            },
            Edit::Modify { after, .. } => match self.index_of(&after.id) {
                Some(i) => {
                    self.annotations[i] = after.clone();
                    true
                }
                None => false,
            },
            Edit::Status { after, .. } => {
                self.status = *after;
                true
            }
            Edit::Replace { after, .. } => {
                self.annotations = after.clone();
                true
            }
        }
    }

    fn backward(&mut self, edit: &Edit) {
        match edit {
            Edit::Add { ann, .. } => {
                if let Some(i) = self.index_of(&ann.id) {
                    self.annotations.remove(i);
                }
            }
            Edit::Remove { index, ann } => {
                let i = (*index).min(self.annotations.len());
                self.annotations.insert(i, ann.clone());
            }
            Edit::Modify { before, .. } => {
                if let Some(i) = self.index_of(&before.id) {
                    self.annotations[i] = before.clone();
                }
            }
            Edit::Status { before, .. } => self.status = *before,
            Edit::Replace { before, .. } => self.annotations = before.clone(),
        }
    }

    /// Set the status outside the undo history (navigation bookkeeping:
    /// opened / reviewed are not edits the user would want to undo).
    pub fn set_status_untracked(&mut self, status: ReviewStatus) -> bool {
        if self.status == status {
            return false;
        }
        self.status = status;
        self.revision += 1;
        true
    }
}

/// A fresh, unique annotation id for a box drawn in this session.
pub fn new_annotation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("h{t:x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(id: &str, x: f64) -> Annotation {
        Annotation {
            id: id.into(),
            class_id: 0,
            bbox: Box2::new(x, 0.0, x + 10.0, 10.0),
            is_ignore: false,
            provenance_json: None,
        }
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut d = PageDoc::new(
            "p".into(),
            100.0,
            100.0,
            vec![ann("a", 0.0)],
            ReviewStatus::Unseen,
        );
        d.apply(Edit::Add {
            index: 1,
            ann: ann("b", 20.0),
        });
        let before = d.annotations[0].clone();
        let mut after = before.clone();
        after.bbox.x1 = 50.0;
        d.apply(Edit::Modify { before, after });
        d.apply(Edit::Remove {
            index: 1,
            ann: ann("b", 20.0),
        });
        assert_eq!(d.annotations.len(), 1);
        d.undo();
        d.undo();
        assert_eq!(d.annotations[0].bbox.x1, 10.0);
        assert_eq!(d.annotations.len(), 2);
        d.redo();
        assert_eq!(d.annotations[0].bbox.x1, 50.0);
        d.undo();
        d.undo();
        assert_eq!(d.annotations.len(), 1);
        assert!(!d.history.can_undo());
    }

    #[test]
    fn noop_modify_is_not_recorded() {
        let mut d = PageDoc::new(
            "p".into(),
            100.0,
            100.0,
            vec![ann("a", 0.0)],
            ReviewStatus::Unseen,
        );
        let a = d.annotations[0].clone();
        assert!(!d.apply(Edit::Modify {
            before: a.clone(),
            after: a
        }));
        assert!(!d.history.can_undo());
    }
}
