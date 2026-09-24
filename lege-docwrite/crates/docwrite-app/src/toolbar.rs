//! The toolbar above the pages, and the one-line prompt it shows when a
//! command needs text (a chapter's name, a note).

use docwrite_model::{BlockKind, Mark, NoteKind, Position, Selection};
use docwrite_typeset::Face;

use crate::Editor;

/// Toolbar height in logical pixels.
pub const TOOLBAR_H: f32 = 40.0;

/// Paragraph kinds the style button cycles through, with their labels.
const KINDS: [(&str, BlockKind); 7] = [
    ("Body", BlockKind::Body),
    ("Subhead", BlockKind::Subhead),
    ("Block Quote", BlockKind::BlockQuote),
    ("Epigraph", BlockKind::Epigraph),
    ("Verse", BlockKind::Verse),
    ("Caption", BlockKind::Caption),
    ("Scene Break", BlockKind::SceneBreak),
];

/// Something the toolbar can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Mark(Mark),
    CycleStyle,
    Smaller,
    Larger,
    NewChapter,
    RenameChapter,
    Footnote,
    Endnote,
    OpenSource,
    Spread,
    Contents,
}

/// What a prompt's text is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    NewChapter,
    RenameChapter,
    Footnote,
    Endnote,
    /// The citation text for a source just captured.
    Citation,
    /// The path of a PDF to open beside the manuscript.
    OpenSource,
    Find,
    /// What to replace; the prompt then asks what with.
    Replace,
    ReplaceWith,
}

impl PromptKind {
    fn label(self) -> &'static str {
        match self {
            Self::NewChapter => "New chapter title:",
            Self::RenameChapter => "Rename chapter:",
            Self::Footnote => "Footnote:",
            Self::Endnote => "Endnote:",
            Self::Citation => "Cite as:",
            Self::OpenSource => "Open source PDF:",
            Self::Find => "Find:",
            Self::Replace => "Replace:",
            Self::ReplaceWith => "With:",
        }
    }
}

/// A one-line text entry shown in the toolbar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
}

/// A toolbar button in window pixels.
#[derive(Clone, Debug)]
pub struct Button {
    pub action: Action,
    pub label: String,
    pub active: bool,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Editor {
    /// Toolbar height in window pixels; zero in full screen.
    pub(crate) fn toolbar_height(&self) -> i32 {
        if self.fullscreen {
            0
        } else {
            (TOOLBAR_H * self.ui_scale).round() as i32
        }
    }

    /// The paragraph style name of the caret's block.
    pub fn current_style_label(&self) -> &'static str {
        let focus = self.book.selection().focus;
        let kind = self
            .book
            .block(focus.block)
            .map(|block| block.kind().clone());
        match kind {
            Ok(BlockKind::ChapterTitle) => "Chapter Title",
            Ok(BlockKind::Image { .. }) => "Image",
            Ok(BlockKind::BibliographyEntry) => "Bibliography",
            Ok(kind) => KINDS
                .iter()
                .find(|(_, candidate)| *candidate == kind)
                .map_or("Body", |(label, _)| label),
            Err(_) => "Body",
        }
    }

    /// The toolbar's buttons, laid out for a window `width` pixels wide.
    pub fn toolbar_buttons(&self, width: i32) -> Vec<Button> {
        let s = self.ui_scale;
        let mut x = self.sidebar_width() + (10.0 * s) as i32;
        let y = (6.0 * s) as i32;
        let h = (28.0 * s) as i32;
        let mut out = Vec::new();
        let mut push = |action: Action, label: String, active: bool, w: f32, gap: f32| {
            let w = (w * s) as i32;
            out.push(Button {
                action,
                label,
                active,
                x,
                y,
                w,
                h,
            });
            x += w + (gap * s) as i32;
        };
        let marks = [
            (Mark::Bold, "B"),
            (Mark::Italic, "I"),
            (Mark::SmallCaps, "Sc"),
            (Mark::Superscript, "x\u{b2}"),
            (Mark::Subscript, "x\u{2082}"),
        ];
        for (number, (mark, label)) in marks.into_iter().enumerate() {
            let gap = if number + 1 == marks.len() { 14.0 } else { 4.0 };
            push(
                Action::Mark(mark),
                label.into(),
                self.book.selection_has(mark),
                30.0,
                gap,
            );
        }
        push(
            Action::CycleStyle,
            format!("\u{b6} {}", self.current_style_label()),
            false,
            120.0,
            10.0,
        );
        push(Action::Smaller, "A\u{2212}".into(), false, 34.0, 4.0);
        push(Action::Larger, "A+".into(), false, 34.0, 14.0);
        push(Action::NewChapter, "+ Chapter".into(), false, 84.0, 4.0);
        push(Action::RenameChapter, "Rename".into(), false, 66.0, 14.0);
        push(Action::Footnote, "Footnote".into(), false, 74.0, 4.0);
        push(Action::Endnote, "Endnote".into(), false, 70.0, 14.0);
        push(
            Action::OpenSource,
            "Source\u{2026}".into(),
            false,
            70.0,
            14.0,
        );
        push(Action::Spread, "Spread".into(), self.spread, 62.0, 4.0);
        push(
            Action::Contents,
            "Contents".into(),
            self.book.has_contents(),
            78.0,
            4.0,
        );
        out.retain(|button| button.x + button.w <= width);
        out
    }

    /// Run a toolbar action.
    pub fn run_action(&mut self, action: Action) {
        match action {
            Action::Mark(mark) => self.toggle_mark(mark),
            Action::CycleStyle => self.cycle_block_style(),
            Action::Smaller => self.change_body_size(-0.5),
            Action::Larger => self.change_body_size(0.5),
            Action::NewChapter => self.open_prompt(PromptKind::NewChapter),
            Action::RenameChapter => self.open_prompt(PromptKind::RenameChapter),
            Action::Footnote => self.open_prompt(PromptKind::Footnote),
            Action::Endnote => self.open_prompt(PromptKind::Endnote),
            Action::OpenSource => self.open_prompt(PromptKind::OpenSource),
            Action::Spread => self.set_spread(!self.spread),
            Action::Contents => self.toggle_contents(),
        }
    }

    /// Set or remove the table of contents before the first chapter.
    pub fn toggle_contents(&mut self) {
        let on = !self.book.has_contents();
        self.book.set_contents(on);
        self.edited();
    }

    /// Toggle a character mark on the selection, or for what is typed next.
    pub fn toggle_mark(&mut self, mark: Mark) {
        let selected = !self.book.selection().is_collapsed();
        if self.book.toggle_mark(mark).is_ok() && selected {
            self.edited();
        }
    }

    /// Set every selected block (or the caret's) to paragraph kind `kind`.
    pub fn set_block_kind(&mut self, kind: BlockKind) {
        let selection = self.book.selection();
        let ids = self.book.block_ids();
        let at = |block| ids.iter().position(|id| *id == block);
        let (Some(a), Some(b)) = (at(selection.anchor.block), at(selection.focus.block)) else {
            return;
        };
        let (from, to) = (a.min(b), a.max(b));
        let mut changed = false;
        for id in &ids[from..=to] {
            if self.book.set_kind(*id, kind.clone()).is_ok() {
                changed = true;
            }
        }
        if changed {
            self.edited();
        }
    }

    /// Set the caret's block to the next paragraph kind in the style list.
    pub fn cycle_block_style(&mut self) {
        let current = self.current_style_label();
        let next = KINDS
            .iter()
            .position(|(label, _)| *label == current)
            .map_or(0, |index| (index + 1) % KINDS.len());
        self.set_block_kind(KINDS[next].1.clone());
    }

    /// Paragraph kinds by number, for the keyboard (0 = Body, 1 = Subhead, ...).
    pub fn set_block_kind_number(&mut self, number: usize) {
        if let Some((_, kind)) = KINDS.get(number) {
            self.set_block_kind(kind.clone());
        }
    }

    /// Make the body text `delta` points larger (smaller when negative);
    /// the first paragraph follows, and leading keeps its proportion.
    pub fn change_body_size(&mut self, delta: f32) {
        for name in ["Body", "First Paragraph"] {
            let Some(mut style) = self
                .book
                .paragraph_styles()
                .iter()
                .find(|style| style.name == name)
                .cloned()
            else {
                continue;
            };
            let size = (style.size_pt + delta).clamp(6.0, 36.0);
            style.leading_pt *= size / style.size_pt;
            style.size_pt = size;
            let _ = self.book.set_paragraph_style(style);
        }
        self.edited();
    }

    /// Show a prompt in the toolbar; typing goes to it until Enter or Esc.
    pub fn open_prompt(&mut self, kind: PromptKind) {
        let text = match kind {
            PromptKind::RenameChapter => self.current_chapter_title().unwrap_or_default(),
            PromptKind::Find | PromptKind::Replace => self.last_find.clone().unwrap_or_default(),
            _ => String::new(),
        };
        if self.fullscreen {
            self.set_fullscreen(false);
        }
        self.prompt = Some(Prompt { kind, text });
    }

    /// The prompt being typed into, if any.
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    /// Type into the prompt. Returns false when no prompt is open.
    pub fn prompt_type(&mut self, text: &str) -> bool {
        match self.prompt.as_mut() {
            Some(prompt) => {
                prompt.text.push_str(text);
                true
            }
            None => false,
        }
    }

    /// Delete the prompt's last character. Returns false when no prompt is open.
    pub fn prompt_backspace(&mut self) -> bool {
        match self.prompt.as_mut() {
            Some(prompt) => {
                prompt.text.pop();
                true
            }
            None => false,
        }
    }

    /// Close the prompt without acting.
    pub fn cancel_prompt(&mut self) -> bool {
        self.prompt.take().is_some()
    }

    /// Act on the prompt's text and close it.
    pub fn commit_prompt(&mut self) -> bool {
        let Some(prompt) = self.prompt.take() else {
            return false;
        };
        let text = prompt.text.trim().to_string();
        match prompt.kind {
            PromptKind::NewChapter => {
                let title = if text.is_empty() {
                    "Untitled".to_string()
                } else {
                    text
                };
                self.new_chapter(&title);
            }
            PromptKind::RenameChapter => {
                if let Some(id) = self.current_chapter() {
                    if self.book.rename_chapter(id, text).is_ok() {
                        self.edited();
                    }
                }
            }
            PromptKind::Citation => self.set_last_citation(&text),
            PromptKind::Find => {
                if !text.is_empty() {
                    self.find(&text);
                }
            }
            PromptKind::Replace => {
                if !text.is_empty() {
                    self.last_find = Some(text.clone());
                    self.replacing = Some(text);
                    self.open_prompt(PromptKind::ReplaceWith);
                }
            }
            PromptKind::ReplaceWith => {
                if let Some(query) = self.replacing.take() {
                    // The replacement is taken as typed, spaces included.
                    self.replace_all(&query, &prompt.text);
                }
            }
            PromptKind::OpenSource => {
                let path = text.trim_matches(|ch| ch == '"' || ch == '\'');
                if let Err(err) = self.open_source(path) {
                    eprintln!("lege-docwrite: {err}");
                }
            }
            PromptKind::Footnote | PromptKind::Endnote if !text.is_empty() => {
                let kind = if prompt.kind == PromptKind::Footnote {
                    NoteKind::Footnote
                } else {
                    NoteKind::Endnote
                };
                if self.book.attach_note(kind, &text).is_ok() {
                    self.edited();
                }
            }
            _ => {}
        }
        true
    }

    /// Add a chapter after the caret's chapter's part and put the caret in it.
    pub fn new_chapter(&mut self, title: &str) {
        let part = self
            .current_part()
            .or_else(|| self.book.parts().first().map(|part| part.id()));
        let Some(part) = part else {
            return;
        };
        if self.book.add_chapter(part, title).is_err() {
            return;
        }
        let chapter = self
            .book
            .parts()
            .iter()
            .find(|candidate| candidate.id() == part)
            .and_then(|part| part.chapters().last());
        let first = chapter.and_then(|chapter| {
            chapter
                .sections()
                .iter()
                .flat_map(|section| section.blocks())
                .next()
                .map(|block| block.id())
        });
        if let Some(block) = first {
            let _ = self
                .book
                .set_selection(Selection::collapsed(Position::new(block, 0)));
        }
        self.edited();
    }

    fn current_chapter(&self) -> Option<docwrite_model::ChapterId> {
        let focus = self.book.selection().focus.block;
        self.book.parts().iter().find_map(|part| {
            part.chapters()
                .iter()
                .find(|chapter| {
                    chapter
                        .sections()
                        .iter()
                        .any(|section| section.blocks().iter().any(|block| block.id() == focus))
                })
                .map(|chapter| chapter.id())
        })
    }

    fn current_part(&self) -> Option<docwrite_model::PartId> {
        let chapter = self.current_chapter()?;
        self.book
            .parts()
            .iter()
            .find(|part| {
                part.chapters()
                    .iter()
                    .any(|candidate| candidate.id() == chapter)
            })
            .map(|part| part.id())
    }

    fn current_chapter_title(&self) -> Option<String> {
        let chapter = self.current_chapter()?;
        self.book
            .parts()
            .iter()
            .flat_map(|part| part.chapters())
            .find(|candidate| candidate.id() == chapter)
            .map(|chapter| chapter.title().to_string())
    }

    /// Put the caret on the first line of chapter `chapter` and show its page.
    pub fn go_to_chapter(&mut self, chapter: docwrite_model::ChapterId) {
        let first = self
            .book
            .parts()
            .iter()
            .flat_map(|part| part.chapters())
            .find(|candidate| candidate.id() == chapter)
            .and_then(|chapter| {
                chapter
                    .sections()
                    .iter()
                    .flat_map(|section| section.blocks())
                    .next()
                    .map(|block| block.id())
            });
        if let Some(block) = first {
            let _ = self
                .book
                .set_selection(Selection::collapsed(Position::new(block, 0)));
            self.follow_caret = true;
            self.refresh_layout();
        }
    }

    /// The toolbar button under a window point.
    pub(crate) fn button_at(&self, x: f32, y: f32, width: i32) -> Option<Action> {
        self.toolbar_buttons(width)
            .into_iter()
            .find(|button| {
                x >= button.x as f32
                    && x < (button.x + button.w) as f32
                    && y >= button.y as f32
                    && y < (button.y + button.h) as f32
            })
            .map(|button| button.action)
    }

    /// Draw the toolbar (or the open prompt) across the top of the pages.
    pub(crate) fn paint_toolbar(
        &mut self,
        painter: &mut pixelkit_raster::Painter<'_>,
        face: &Face,
        width: i32,
        pages: u32,
    ) {
        let s = self.ui_scale;
        let left = self.sidebar_width();
        let height = self.toolbar_height();
        painter.fill_rect(
            pixelkit_raster::Rect::new(left, 0, width - left, height),
            0x00EE_E8DE,
        );
        painter.fill_rect(
            pixelkit_raster::Rect::new(
                left,
                height - (1.0 * s).max(1.0) as i32,
                width - left,
                (1.0 * s).max(1.0) as i32,
            ),
            0x00D2_C9BB,
        );
        let size = 14.0 * s;
        let baseline = (26.0 * s) as i32;
        if let Some(prompt) = self.prompt.clone() {
            let label = prompt.kind.label();
            self.paint_label(
                painter,
                face,
                label,
                left + (14.0 * s) as i32,
                baseline,
                size,
            );
            let label_w: f32 = face
                .shape(label, size, &[])
                .map(|glyphs| glyphs.iter().map(|glyph| glyph.x_advance).sum())
                .unwrap_or(0.0);
            let field_x = left + (22.0 * s) as i32 + label_w as i32;
            painter.fill_rect(
                pixelkit_raster::Rect::new(
                    field_x,
                    (6.0 * s) as i32,
                    width - field_x - (14.0 * s) as i32,
                    (28.0 * s) as i32,
                ),
                0x00FF_FFFF,
            );
            let shown = format!("{}\u{2502}", prompt.text);
            self.paint_label(
                painter,
                face,
                &shown,
                field_x + (8.0 * s) as i32,
                baseline,
                size,
            );
            return;
        }
        for button in self.toolbar_buttons(width) {
            let fill = if button.active {
                0x00C9_D8EE
            } else {
                0x00F8_F4EC
            };
            painter.fill_rect(
                pixelkit_raster::Rect::new(button.x, button.y, button.w, button.h),
                fill,
            );
            let label_w: f32 = face
                .shape(&button.label, size, &[])
                .map(|glyphs| glyphs.iter().map(|glyph| glyph.x_advance).sum())
                .unwrap_or(0.0);
            let x = button.x + ((button.w as f32 - label_w) / 2.0) as i32;
            self.paint_label(painter, face, &button.label, x, baseline, size);
        }
        if pages > 0 {
            let top = self.pager.page_top();
            let status = if self.spread {
                // Spread k shows pages 2k and 2k + 1; page 1 stands alone.
                match (top, (top + 1).min(pages)) {
                    (0, _) => format!("1 / {pages}"),
                    (left, right) if right > left => format!("{left}\u{2013}{right} / {pages}"),
                    (left, _) => format!("{left} / {pages}"),
                }
            } else {
                format!("{} / {pages}", top + 1)
            };
            let status_w: f32 = face
                .shape(&status, size, &[])
                .map(|glyphs| glyphs.iter().map(|glyph| glyph.x_advance).sum())
                .unwrap_or(0.0);
            let x = width - status_w as i32 - (14.0 * s) as i32;
            let last_button = self
                .toolbar_buttons(width)
                .last()
                .map_or(left, |button| button.x + button.w);
            if x > last_button + (10.0 * s) as i32 {
                self.paint_label(painter, face, &status, x, baseline, size);
            }
        }
    }
}
