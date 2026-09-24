//! Editing, file and caret commands the window binds to keys and the pointer.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use docwrite_model::{Book, Direction, Motion, Position, Selection};
use docwrite_typeset::{NOTE_MARK_CLUSTER, PaintedLine};

use crate::{Editor, focus_mark};

/// Where the last paint put one page, in window pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PageSlot {
    pub page: u32,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f32,
}

impl Editor {
    /// Open the bundle at `path`, or start a new book that will be saved
    /// there on the first save.
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let (book, fresh) = if Book::is_bundle(&path) {
            (
                Book::load_bundle(&path).map_err(|err| err.to_string())?,
                false,
            )
        } else {
            let title = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into());
            (Book::new(title), true)
        };
        let mut editor = Self::with_book(book);
        editor.open_bundle(path);
        editor.dirty = fresh;
        Ok(editor)
    }

    /// The bundle this editor saves to, if any.
    pub fn bundle(&self) -> Option<&Path> {
        self.bundle.as_deref()
    }

    /// Whether an edit has landed since the last save started.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Save now, after any background save in flight. For Cmd-S and exit.
    pub fn save_now(&mut self) -> Result<(), String> {
        self.wait_for_background_save();
        if let Some(error) = self.take_save_error() {
            self.dirty = true;
            return Err(error);
        }
        self.autosave()
    }

    /// Start a background save when the book has been dirty and untouched
    /// for `idle`. Serialization runs on its own thread against a copy of
    /// the book, so typing never waits on the disk. Returns whether a save
    /// started.
    pub fn autosave_if_idle(&mut self, idle: Duration) -> bool {
        if !self.dirty || self.saving.load(Ordering::Acquire) {
            return false;
        }
        if self.last_edit.is_some_and(|last| last.elapsed() < idle) {
            return false;
        }
        let Some(path) = self.bundle.clone() else {
            return false;
        };
        let mut copy = self.book.clone();
        let saving = self.saving.clone();
        let error = self.save_error.clone();
        saving.store(true, Ordering::Release);
        self.dirty = false;
        std::thread::spawn(move || {
            if let Err(err) = copy.save_bundle(&path)
                && let Ok(mut slot) = error.lock()
            {
                *slot = Some(err.to_string());
            }
            saving.store(false, Ordering::Release);
        });
        true
    }

    /// The error from the last background save, if it failed. Taking it
    /// marks the book dirty again so the next save retries.
    pub fn take_save_error(&mut self) -> Option<String> {
        let error = self.save_error.lock().ok().and_then(|mut slot| slot.take());
        if error.is_some() {
            self.dirty = true;
        }
        error
    }

    pub(crate) fn wait_for_background_save(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.saving.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Write the book as a typeset PDF at `path`.
    pub fn export_pdf_to(&self, path: &Path) -> Result<(), String> {
        let font = self
            .font_bytes
            .as_deref()
            .ok_or_else(|| "no font is loaded".to_string())?;
        let pdf = docwrite_export::export_pdf(&self.book, font)?;
        write_atomically(path, &pdf.bytes)
    }

    /// Write the book as Markdown at `path`.
    pub fn export_markdown_to(&self, path: &Path) -> Result<(), String> {
        write_atomically(
            path,
            docwrite_export::export_markdown(&self.book).as_bytes(),
        )
    }

    pub fn undo(&mut self) {
        if self.book.undo().is_ok() {
            self.edited();
        }
    }

    pub fn redo(&mut self) {
        if self.book.redo().is_ok() {
            self.edited();
        }
    }

    /// The selected text, for the clipboard. `None` when nothing is selected.
    pub fn copy_text(&self) -> Option<String> {
        if self.book.selection().is_collapsed() {
            return None;
        }
        self.book.copy().ok().map(|fragment| fragment.plain_text())
    }

    /// Remove the selection and return its text, for the clipboard.
    pub fn cut_text(&mut self) -> Option<String> {
        if self.book.selection().is_collapsed() {
            return None;
        }
        let fragment = self.book.cut().ok()?;
        self.edited();
        Some(fragment.plain_text())
    }

    /// Replace the selection with `text`. Newlines start new paragraphs.
    pub fn paste_text(&mut self, text: &str) {
        if !text.is_empty() && self.book.paste_text(text).is_ok() {
            self.edited();
        }
    }

    pub fn delete_forward(&mut self) {
        if self.book.delete_forward().is_ok() {
            self.edited();
        }
    }

    /// Move (or with `extend`, select) one word forward or backward.
    pub fn move_word(&mut self, forward: bool, extend: bool) {
        let motion = Motion::Word(if forward {
            Direction::Forward
        } else {
            Direction::Backward
        });
        let _ = if extend {
            self.book.extend_selection(motion)
        } else {
            self.book.move_caret(motion)
        };
        self.follow_caret = true;
    }

    /// Move to the start of the caret's paragraph.
    pub fn move_home(&mut self, extend: bool) {
        let focus = self.book.selection().focus;
        self.set_focus(Position::new(focus.block, 0), extend);
    }

    /// Move to the end of the caret's paragraph.
    pub fn move_end(&mut self, extend: bool) {
        let focus = self.book.selection().focus;
        let len = self.book.block_len(focus.block).unwrap_or(focus.offset);
        self.set_focus(Position::new(focus.block, len), extend);
    }

    /// Move to the typeset line above or below, keeping the caret's
    /// horizontal position. Crosses page boundaries.
    pub fn move_line(&mut self, down: bool, extend: bool) {
        self.refresh_layout();
        let Some(target) = self.line_neighbor(down) else {
            return;
        };
        self.set_focus(target, extend);
    }

    /// Put the caret at a window point on a page, as a click does.
    /// Returns whether the point was on a page.
    pub fn click_at(&mut self, x: f32, y: f32, extend: bool) -> bool {
        let Some(slot) = self
            .slots
            .iter()
            .find(|slot| {
                x >= slot.left as f32
                    && x < (slot.left + slot.width) as f32
                    && y >= slot.top as f32
                    && y < (slot.top + slot.height) as f32
            })
            .copied()
        else {
            return false;
        };
        let Some(document) = self.document.as_ref() else {
            return false;
        };
        let page_x = (x - slot.left as f32) / slot.scale;
        let page_y = (y - slot.top as f32) / slot.scale;
        let lines = document.page_painted_lines(slot.page);
        let Some(line) = lines.iter().min_by(|a, b| {
            let da = (a.baseline - a.em * 0.3 - page_y).abs();
            let db = (b.baseline - b.em * 0.3 - page_y).abs();
            da.total_cmp(&db)
        }) else {
            return true;
        };
        let inset = document.page_content_inset(slot.page);
        let byte = byte_at_x(
            line,
            page_x - inset - line.indent,
            self.paragraph_len(line.paragraph),
        );
        if let Some(position) = self.position_of(line.paragraph, byte) {
            self.set_focus(position, extend);
        }
        true
    }

    fn line_neighbor(&self, down: bool) -> Option<Position> {
        let document = self.document.as_ref()?;
        let (paragraph, byte) = focus_mark(&self.book, document)?;
        let page = document.page_of(paragraph, byte)?;
        let lines = document.page_painted_lines(page);
        let index = lines
            .iter()
            .rposition(|line| line.paragraph == paragraph && line_start(line) <= byte)
            .or_else(|| lines.iter().position(|line| line.paragraph == paragraph))?;
        let current = &lines[index];
        let x = current.indent + x_of(current, byte);
        let target = if down {
            match lines.get(index + 1) {
                Some(line) => line.clone(),
                None => first_line_after(document, page)?,
            }
        } else {
            match index
                .checked_sub(1)
                .and_then(|previous| lines.get(previous))
            {
                Some(line) => line.clone(),
                None => last_line_before(document, page)?,
            }
        };
        let byte = byte_at_x(
            &target,
            x - target.indent,
            self.paragraph_len(target.paragraph),
        );
        self.position_of(target.paragraph, byte)
    }

    fn paragraph_len(&self, paragraph: usize) -> usize {
        self.document
            .as_ref()
            .and_then(|document| document.paragraphs().get(paragraph))
            .map_or(0, |paragraph| paragraph.text.len())
    }

    /// The model position of byte `byte` in layout paragraph `paragraph`.
    /// `None` for a paragraph that is not a block, such as a chapter title
    /// set from the chapter's metadata.
    fn position_of(&self, paragraph: usize, byte: usize) -> Option<Position> {
        let raw = self.document.as_ref()?.paragraphs().get(paragraph)?.id;
        let id = self
            .book
            .block_ids()
            .into_iter()
            .find(|id| id.raw() == raw)?;
        let text = self.book.block(id).ok()?.text();
        let byte = byte.min(text.len());
        let chars = text.get(..byte).map_or(0, |head| head.chars().count());
        Some(Position::new(id, chars))
    }

    fn set_focus(&mut self, position: Position, extend: bool) {
        let selection = if extend {
            Selection {
                anchor: self.book.selection().anchor,
                focus: position,
            }
        } else {
            Selection::collapsed(position)
        };
        let _ = self.book.set_selection(selection);
        self.follow_caret = true;
    }

    /// Record an edit: the layout is stale, the view follows the caret, and
    /// autosave waits for typing to pause.
    pub(crate) fn edited(&mut self) {
        self.dirty = true;
        self.layout_stale = true;
        self.follow_caret = true;
        self.last_edit = Some(Instant::now());
    }
}

fn first_line_after(document: &docwrite_typeset::Document, page: u32) -> Option<PaintedLine> {
    (page + 1..=document.page_count())
        .find_map(|next| document.page_painted_lines(next).into_iter().next())
}

fn last_line_before(document: &docwrite_typeset::Document, page: u32) -> Option<PaintedLine> {
    (1..page)
        .rev()
        .find_map(|previous| document.page_painted_lines(previous).into_iter().last())
}

/// Byte offset where `line` starts in its paragraph.
fn line_start(line: &PaintedLine) -> usize {
    line.glyphs
        .iter()
        .filter(|glyph| glyph.cluster < NOTE_MARK_CLUSTER)
        .map(|glyph| glyph.cluster as usize)
        .min()
        .unwrap_or(0)
}

/// Pen position of byte `byte` along `line`, from the line's start.
fn x_of(line: &PaintedLine, byte: usize) -> f32 {
    line.glyphs
        .iter()
        .filter(|glyph| glyph.cluster < NOTE_MARK_CLUSTER && (glyph.cluster as usize) < byte)
        .map(|glyph| glyph.x_advance)
        .sum()
}

/// The byte nearest pen position `x` on `line`. `len` is the paragraph's
/// length in bytes.
fn byte_at_x(line: &PaintedLine, x: f32, len: usize) -> usize {
    let mut pen = 0.0;
    let mut last_start = line_start(line);
    for glyph in line
        .glyphs
        .iter()
        .filter(|glyph| glyph.cluster < NOTE_MARK_CLUSTER)
    {
        if pen + glyph.x_advance / 2.0 > x {
            return glyph.cluster as usize;
        }
        pen += glyph.x_advance;
        last_start = glyph.cluster as usize;
    }
    // Past the end: a paragraph's last line ends at the paragraph's end;
    // any other line keeps the caret before its last glyph, on this line.
    if line.ends_paragraph { len } else { last_start }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let temporary = path.with_file_name(format!(".{}.partial", name.to_string_lossy()));
    std::fs::write(&temporary, bytes).map_err(|err| err.to_string())?;
    std::fs::rename(&temporary, path).map_err(|err| err.to_string())
}
