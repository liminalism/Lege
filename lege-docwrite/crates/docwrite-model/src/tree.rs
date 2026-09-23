//! The semantic book: parts, chapters, sections, blocks and notes.
//!
//! Pages are not stored. [`Book::plain_text`] joins blocks with newlines only
//! so callers can see paragraph boundaries; a page boundary is wherever
//! typesetting later cuts this same sequence.

use std::collections::{BTreeMap, HashMap};

use ropey::Rope;

use crate::error::ModelError;
use crate::ids::{BlockId, ChapterId, IdGen, NoteId, PartId, SectionId};
use crate::runs::{self, Run, RunMarks};

/// Where a block sits. Indices are valid only until the next structural edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockLoc {
    pub part: usize,
    pub chapter: usize,
    pub section: usize,
    pub block: usize,
}

/// Kind of block. Footnote and endnote *references* use [`Block::note`];
/// the body of the note is a [`Note`] on the book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BlockKind {
    /// A body paragraph.
    #[default]
    Body,
    /// The chapter title.
    ChapterTitle,
    /// A subhead.
    Subhead,
    /// A block quote.
    BlockQuote,
    /// An epigraph.
    Epigraph,
    /// An image. `asset` is a path inside the book bundle, when one is known.
    Image {
        /// Bundle-relative asset path.
        asset: Option<String>,
    },
    /// A caption.
    Caption,
    /// Verse.
    Verse,
    /// One bibliography entry.
    BibliographyEntry,
    /// A section or scene break. The text is usually empty.
    SceneBreak,
}

/// Footnote or endnote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    /// Placed on the page that references it, and continued when it does not fit.
    Footnote,
    /// Placed on the page that references it, and continued when it does not fit.
    Endnote,
}

/// A note body. Pagination reads it through `from_book`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    id: NoteId,
    kind: NoteKind,
    text: Rope,
}

impl Note {
    /// Stable id.
    pub fn id(&self) -> NoteId {
        self.id
    }

    /// Footnote or endnote.
    pub fn kind(&self) -> NoteKind {
        self.kind
    }

    /// Note body as a string.
    pub fn text(&self) -> String {
        self.text.to_string()
    }

    pub(crate) fn from_text(id: NoteId, kind: NoteKind, text: &str) -> Self {
        Self {
            id,
            kind,
            text: Rope::from_str(text),
        }
    }
}

/// One block: a rope of text, the runs over it, and an optional note slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    id: BlockId,
    kind: BlockKind,
    text: Rope,
    runs: Vec<Run>,
    note: Option<NoteId>,
}

impl Block {
    /// Stable id. Never reused.
    pub fn id(&self) -> BlockId {
        self.id
    }

    /// Structural kind.
    pub fn kind(&self) -> &BlockKind {
        &self.kind
    }

    /// Character length.
    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    /// Block text.
    pub fn text(&self) -> String {
        self.text.to_string()
    }

    /// Styled spans covering [`Self::text`].
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// Footnote or endnote anchored to this block, if any.
    pub fn note(&self) -> Option<NoteId> {
        self.note
    }

    pub(crate) fn text_rope(&self) -> &Rope {
        &self.text
    }

    pub(crate) fn text_rope_mut(&mut self) -> &mut Rope {
        &mut self.text
    }

    pub(crate) fn runs_mut(&mut self) -> &mut Vec<Run> {
        &mut self.runs
    }

    pub(crate) fn set_kind(&mut self, kind: BlockKind) {
        self.kind = kind;
    }

    pub(crate) fn set_note(&mut self, note: Option<NoteId>) {
        self.note = note;
    }

    pub(crate) fn new(id: BlockId, kind: BlockKind, text: &str, runs: Vec<Run>) -> Self {
        Self {
            id,
            kind,
            text: Rope::from_str(text),
            runs,
            note: None,
        }
    }

    pub(crate) fn empty(id: BlockId, kind: BlockKind) -> Self {
        Self::new(id, kind, "", runs::covering(0, RunMarks::default()))
    }
}

/// A section: an optional heading and the blocks under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    id: SectionId,
    heading: String,
    blocks: Vec<Block>,
}

impl Section {
    /// Stable id.
    pub fn id(&self) -> SectionId {
        self.id
    }

    /// Heading text. Empty when the section has no heading of its own.
    pub fn heading(&self) -> &str {
        &self.heading
    }

    /// Blocks in reading order.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub(crate) fn blocks_mut(&mut self) -> &mut Vec<Block> {
        &mut self.blocks
    }
}

/// A chapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chapter {
    id: ChapterId,
    title: String,
    /// Chapter template name. Changing that template changes every chapter that uses it.
    template: String,
    sections: Vec<Section>,
}

impl Chapter {
    /// Stable id.
    pub fn id(&self) -> ChapterId {
        self.id
    }

    /// Chapter title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Template this chapter was opened from.
    pub fn template(&self) -> &str {
        &self.template
    }

    /// Sections in reading order.
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    pub(crate) fn set_title(&mut self, title: String) {
        self.title = title;
    }
}

/// A part. Front matter, body and back matter are parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    id: PartId,
    title: String,
    chapters: Vec<Chapter>,
}

impl Part {
    /// Stable id.
    pub fn id(&self) -> PartId {
        self.id
    }

    /// Part title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Chapters in reading order.
    pub fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    pub(crate) fn chapters_mut(&mut self) -> &mut Vec<Chapter> {
        &mut self.chapters
    }
}

/// A caret position. `offset` is a character index into the block, and may
/// equal the block length (the caret sits after the last character).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    /// Block that contains the caret.
    pub block: BlockId,
    /// Character offset inside the block.
    pub offset: usize,
}

impl Position {
    /// A position in `block`. The book validates the offset when it is used.
    pub fn new(block: BlockId, offset: usize) -> Self {
        Self { block, offset }
    }
}

/// An anchor and a focus. The focus is the end that motions move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The end that stays put while the selection is extended.
    pub anchor: Position,
    /// The end that [`crate::Motion`] moves.
    pub focus: Position,
}

impl Selection {
    /// A caret with no selected characters.
    pub fn collapsed(position: Position) -> Self {
        Self {
            anchor: position,
            focus: position,
        }
    }

    /// True when anchor and focus are the same position.
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.focus
    }
}

/// The manuscript.
#[derive(Clone, Debug)]
pub struct Book {
    title: String,
    pub(crate) ids: IdGen,
    parts: Vec<Part>,
    pub(crate) stylesheet: crate::publish::Stylesheet,
    notes: BTreeMap<NoteId, Note>,
    pub(crate) index: HashMap<BlockId, BlockLoc>,
    pub(crate) selection: Selection,
    pub(crate) undo: Vec<crate::edit::Transaction>,
    pub(crate) redo: Vec<crate::edit::Transaction>,
}

impl Book {
    /// A book with one part, one chapter, one section and one empty body block.
    /// The caret is at the start of that block.
    pub fn new(title: impl Into<String>) -> Self {
        let mut ids = IdGen::new();
        let block_id = ids.block();
        let block = Block::empty(block_id, BlockKind::Body);
        let section = Section {
            id: ids.section(),
            heading: String::new(),
            blocks: vec![block],
        };
        let chapter = Chapter {
            id: ids.chapter(),
            title: "Chapter 1".to_string(),
            template: "Chapter".to_string(),
            sections: vec![section],
        };
        let part = Part {
            id: ids.part(),
            title: "Body".to_string(),
            chapters: vec![chapter],
        };
        let selection = Selection::collapsed(Position::new(block_id, 0));
        let mut book = Self {
            title: title.into(),
            ids,
            parts: vec![part],
            stylesheet: crate::publish::Stylesheet::standard(),
            notes: BTreeMap::new(),
            index: HashMap::new(),
            selection,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        book.reindex();
        book
    }

    /// Book title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Parts in reading order.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    pub(crate) fn parts_mut(&mut self) -> &mut [Part] {
        &mut self.parts
    }

    /// Current selection.
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// True when undo would restore an earlier manuscript.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True when redo would reapply an undone edit.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Block ids in reading order.
    pub fn block_ids(&self) -> Vec<BlockId> {
        self.blocks().map(Block::id).collect()
    }

    /// Every block in reading order.
    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.parts
            .iter()
            .flat_map(|part| part.chapters.iter())
            .flat_map(|chapter| chapter.sections.iter())
            .flat_map(|section| section.blocks.iter())
    }

    /// The block, if the id is still in the book.
    pub fn block(&self, id: BlockId) -> Result<&Block, ModelError> {
        self.block_ref(id)
    }

    /// Character length of a block.
    pub fn block_len(&self, id: BlockId) -> Result<usize, ModelError> {
        Ok(self.block_ref(id)?.len_chars())
    }

    /// Note body.
    pub fn note(&self, id: NoteId) -> Result<&Note, ModelError> {
        self.notes.get(&id).ok_or(ModelError::UnknownNote(id))
    }

    /// Blocks joined by newlines. This is the paragraph sequence, not pages.
    pub fn plain_text(&self) -> String {
        self.blocks()
            .map(Block::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Append a part with no chapters.
    ///
    /// Structural builders are not edit transactions. They clear undo and redo
    /// so a later undo cannot revive a block index from before the builder ran.
    pub fn add_part(&mut self, title: impl Into<String>) -> PartId {
        let id = self.ids.part();
        self.parts.push(Part {
            id,
            title: title.into(),
            chapters: Vec::new(),
        });
        self.forget_history();
        id
    }

    /// Append a chapter with one empty section and one empty body block.
    ///
    /// See [`Self::add_part`] for the undo consequence.
    pub fn add_chapter(
        &mut self,
        part: PartId,
        title: impl Into<String>,
    ) -> Result<ChapterId, ModelError> {
        let chapter_id = self.ids.chapter();
        let block_id = self.ids.block();
        let section = Section {
            id: self.ids.section(),
            heading: String::new(),
            blocks: vec![Block::empty(block_id, BlockKind::Body)],
        };
        let chapter = Chapter {
            id: chapter_id,
            title: title.into(),
            template: "Chapter".to_string(),
            sections: vec![section],
        };
        let part_index = self.part_index(part)?;
        self.parts[part_index].chapters.push(chapter);
        self.reindex();
        self.forget_history();
        Ok(chapter_id)
    }

    /// Append a section with one empty body block.
    ///
    /// See [`Self::add_part`] for the undo consequence.
    pub fn add_section(
        &mut self,
        chapter: ChapterId,
        heading: impl Into<String>,
    ) -> Result<SectionId, ModelError> {
        let section_id = self.ids.section();
        let block_id = self.ids.block();
        let section = Section {
            id: section_id,
            heading: heading.into(),
            blocks: vec![Block::empty(block_id, BlockKind::Body)],
        };
        let (part_index, chapter_index) = self.chapter_index(chapter)?;
        self.parts[part_index].chapters[chapter_index]
            .sections
            .push(section);
        self.reindex();
        self.forget_history();
        Ok(section_id)
    }

    /// Ids, runs, the selection and note links agree with the tree.
    pub fn check_consistency(&self) -> Result<(), ModelError> {
        let mut seen = HashMap::<BlockId, BlockLoc>::new();
        for (part_index, part) in self.parts.iter().enumerate() {
            for (chapter_index, chapter) in part.chapters.iter().enumerate() {
                for (section_index, section) in chapter.sections.iter().enumerate() {
                    for (block_index, block) in section.blocks.iter().enumerate() {
                        let loc = BlockLoc {
                            part: part_index,
                            chapter: chapter_index,
                            section: section_index,
                            block: block_index,
                        };
                        if seen.insert(block.id, loc).is_some() {
                            return Err(ModelError::Inconsistent("duplicate block id"));
                        }
                        if !runs::runs_cover(&block.runs, block.len_chars()) {
                            return Err(ModelError::Inconsistent("runs do not cover text"));
                        }
                        if let Some(note) = block.note
                            && !self.notes.contains_key(&note)
                        {
                            return Err(ModelError::Inconsistent("block note is missing"));
                        }
                    }
                }
            }
        }
        if seen.len() != self.index.len() {
            return Err(ModelError::Inconsistent("index length"));
        }
        for (id, loc) in &seen {
            match self.index.get(id) {
                Some(indexed) if indexed == loc => {}
                _ => return Err(ModelError::Inconsistent("index location")),
            }
        }
        self.ensure_position(self.selection.anchor)?;
        self.ensure_position(self.selection.focus)?;
        Ok(())
    }

    pub(crate) fn block_ref(&self, id: BlockId) -> Result<&Block, ModelError> {
        let loc = self.loc(id)?;
        self.parts
            .get(loc.part)
            .and_then(|part| part.chapters.get(loc.chapter))
            .and_then(|chapter| chapter.sections.get(loc.section))
            .and_then(|section| section.blocks.get(loc.block))
            .ok_or(ModelError::UnknownBlock(id))
    }

    pub(crate) fn block_mut(&mut self, id: BlockId) -> Result<&mut Block, ModelError> {
        let loc = self.loc(id)?;
        self.parts
            .get_mut(loc.part)
            .and_then(|part| part.chapters.get_mut(loc.chapter))
            .and_then(|chapter| chapter.sections.get_mut(loc.section))
            .and_then(|section| section.blocks.get_mut(loc.block))
            .ok_or(ModelError::UnknownBlock(id))
    }

    pub(crate) fn section_mut(&mut self, loc: BlockLoc) -> Result<&mut Section, ModelError> {
        self.parts
            .get_mut(loc.part)
            .and_then(|part| part.chapters.get_mut(loc.chapter))
            .and_then(|chapter| chapter.sections.get_mut(loc.section))
            .ok_or(ModelError::EditConflict)
    }

    pub(crate) fn loc(&self, id: BlockId) -> Result<BlockLoc, ModelError> {
        self.index
            .get(&id)
            .copied()
            .ok_or(ModelError::UnknownBlock(id))
    }

    pub(crate) fn reindex(&mut self) {
        self.index.clear();
        for (part_index, part) in self.parts.iter().enumerate() {
            for (chapter_index, chapter) in part.chapters.iter().enumerate() {
                for (section_index, section) in chapter.sections.iter().enumerate() {
                    for (block_index, block) in section.blocks.iter().enumerate() {
                        self.index.insert(
                            block.id,
                            BlockLoc {
                                part: part_index,
                                chapter: chapter_index,
                                section: section_index,
                                block: block_index,
                            },
                        );
                    }
                }
            }
        }
    }

    pub(crate) fn ensure_position(&self, position: Position) -> Result<(), ModelError> {
        let len = self.block_ref(position.block)?.len_chars();
        if position.offset > len {
            Err(ModelError::OffsetOutOfRange {
                block: position.block,
                offset: position.offset,
                len,
            })
        } else {
            Ok(())
        }
    }

    pub(crate) fn cmp_pos(
        &self,
        left: Position,
        right: Position,
    ) -> Result<std::cmp::Ordering, ModelError> {
        if left.block == right.block {
            return Ok(left.offset.cmp(&right.offset));
        }
        let left_loc = self.loc(left.block)?;
        let right_loc = self.loc(right.block)?;
        Ok((
            left_loc.part,
            left_loc.chapter,
            left_loc.section,
            left_loc.block,
            left.offset,
        )
            .cmp(&(
                right_loc.part,
                right_loc.chapter,
                right_loc.section,
                right_loc.block,
                right.offset,
            )))
    }

    pub(crate) fn neighbor(
        &self,
        id: BlockId,
        direction: crate::edit::Direction,
    ) -> Result<Option<BlockId>, ModelError> {
        let ids = self.block_ids();
        let index = ids
            .iter()
            .position(|block| *block == id)
            .ok_or(ModelError::UnknownBlock(id))?;
        let next = match direction {
            crate::edit::Direction::Forward => index.checked_add(1).and_then(|next| ids.get(next)),
            crate::edit::Direction::Backward => index.checked_sub(1).and_then(|prev| ids.get(prev)),
        };
        Ok(next.copied())
    }

    pub(crate) fn notes_mut(&mut self) -> &mut BTreeMap<NoteId, Note> {
        &mut self.notes
    }

    pub(crate) fn notes(&self) -> &BTreeMap<NoteId, Note> {
        &self.notes
    }

    fn forget_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    fn part_index(&self, id: PartId) -> Result<usize, ModelError> {
        self.parts
            .iter()
            .position(|part| part.id == id)
            .ok_or(ModelError::UnknownPart(id))
    }

    fn chapter_index(&self, id: ChapterId) -> Result<(usize, usize), ModelError> {
        for (part_index, part) in self.parts.iter().enumerate() {
            if let Some(chapter_index) = part.chapters.iter().position(|chapter| chapter.id == id) {
                return Ok((part_index, chapter_index));
            }
        }
        Err(ModelError::UnknownChapter(id))
    }
}
