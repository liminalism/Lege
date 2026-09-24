//! `.legebook` directory bundle.
//!
//! Format 2 is lossless: `book.json` holds the title, id allocator, styles,
//! page masters, chapter templates, research records, notes, selection and
//! the parts with their chapters in order; `chapters/<id>.json` holds each
//! chapter's sections and blocks (kind, text, runs with every mark, note
//! link). Ids survive a round trip, and reordering chapters rewrites only
//! `book.json`. Format 1 bundles (`manifest.txt` plus text chapters) still
//! open. A save writes a whole new directory beside the old one and swaps it
//! in, so a crash leaves either the old bundle or the new one.

use std::fs;
use std::io;
use std::path::Path;

use std::collections::HashMap;

use crate::error::ModelError;
use crate::publish::{BibliographyEntry, SourceNote};
use crate::tree::{Book, BookManifest, Chapter};

/// Failure while saving or opening a bundle.
#[derive(Debug)]
pub struct BundleError {
    message: String,
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BundleError {}

impl From<io::Error> for BundleError {
    fn from(err: io::Error) -> Self {
        Self {
            message: err.to_string(),
        }
    }
}

impl From<ModelError> for BundleError {
    fn from(err: ModelError) -> Self {
        Self {
            message: err.to_string(),
        }
    }
}

impl Book {
    /// Write the manuscript, styles, and selection to `directory` atomically.
    ///
    /// A `snapshots/` directory already inside the bundle is kept.
    pub fn save_bundle(&mut self, directory: &Path) -> Result<(), BundleError> {
        let parent = bundle_parent(directory);
        let file_name = directory.file_name().ok_or_else(|| BundleError {
            message: "bundle path has no name".into(),
        })?;
        let hold = parent.join(format!(".{}.snapshots", file_name.to_string_lossy()));
        let snapshots = directory.join("snapshots");
        let parked = snapshots.is_dir();
        if parked {
            if hold.exists() {
                fs::remove_dir_all(&hold)?;
            }
            fs::rename(&snapshots, &hold)?;
        }
        let written = self.write_tree(directory);
        if parked && directory.is_dir() {
            let back = directory.join("snapshots");
            if back.exists() {
                fs::remove_dir_all(&back)?;
            }
            fs::rename(&hold, &back)?;
        }
        written
    }

    /// Write a named snapshot of the current manuscript under `directory/snapshots`.
    ///
    /// The snapshot is its own bundle. A later autosave of the live book does not
    /// replace it. `name` is one path segment.
    pub fn save_snapshot(&mut self, directory: &Path, name: &str) -> Result<(), BundleError> {
        let name = snapshot_name(name)?;
        if !Self::is_bundle(directory) {
            self.save_bundle(directory)?;
        }
        let dest = directory.join("snapshots").join(name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        self.write_tree(&dest)
    }

    /// Open a snapshot written by [`Self::save_snapshot`].
    pub fn load_snapshot(directory: &Path, name: &str) -> Result<Self, BundleError> {
        let name = snapshot_name(name)?;
        Self::load_bundle(&directory.join("snapshots").join(name))
    }

    fn write_tree(&mut self, directory: &Path) -> Result<(), BundleError> {
        self.remember_position();
        let parent = bundle_parent(directory);
        fs::create_dir_all(parent)?;
        let file_name = directory.file_name().ok_or_else(|| BundleError {
            message: "bundle path has no name".into(),
        })?;
        let temp = parent.join(format!(".{}.tmp", file_name.to_string_lossy()));
        let old = parent.join(format!(".{}.old", file_name.to_string_lossy()));
        if temp.exists() {
            fs::remove_dir_all(&temp)?;
        }
        fs::create_dir_all(temp.join("chapters"))?;
        let (manifest, chapters) = self.to_manifest();
        for chapter in chapters {
            let json = serde_json::to_string_pretty(chapter).map_err(json_error)?;
            fs::write(temp.join("chapters").join(chapter_file(chapter)), json)?;
        }
        // The manifest goes last: a bundle without one is not a bundle.
        let json = serde_json::to_string_pretty(&manifest).map_err(json_error)?;
        fs::write(temp.join("book.json"), json)?;
        // Swap: the old bundle steps aside before the new one takes its
        // name, and is only deleted once the new one is in place.
        if old.exists() {
            fs::remove_dir_all(&old)?;
        }
        if directory.exists() {
            fs::rename(directory, &old)?;
        }
        fs::rename(&temp, directory)?;
        if old.exists() {
            fs::remove_dir_all(&old)?;
        }
        Ok(())
    }

    /// Whether `directory` holds a bundle of either format.
    pub fn is_bundle(directory: &Path) -> bool {
        directory.join("book.json").is_file() || directory.join("manifest.txt").is_file()
    }

    /// Open a bundle written by [`Self::save_bundle`]. If a save was
    /// interrupted after the old bundle stepped aside, the old one is opened.
    pub fn load_bundle(directory: &Path) -> Result<Self, BundleError> {
        if !Self::is_bundle(directory) {
            let old = bundle_parent(directory).join(format!(
                ".{}.old",
                directory
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ));
            if Self::is_bundle(&old) {
                return Self::load_bundle(&old);
            }
        }
        if !directory.join("book.json").is_file() {
            return Self::load_format_1(directory);
        }
        let manifest: BookManifest =
            serde_json::from_str(&fs::read_to_string(directory.join("book.json"))?)
                .map_err(json_error)?;
        if manifest.format != 2 {
            return Err(BundleError {
                message: format!(
                    "bundle format {} is newer than this editor",
                    manifest.format
                ),
            });
        }
        let mut chapters = HashMap::new();
        for entry in fs::read_dir(directory.join("chapters"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let chapter: Chapter =
                    serde_json::from_str(&fs::read_to_string(&path)?).map_err(json_error)?;
                chapters.insert(chapter.id(), chapter);
            }
        }
        let mut book = Book::from_manifest(manifest, chapters)?;
        book.remember_position();
        Ok(book)
    }

    /// Open a format-1 bundle: `manifest.txt` and text chapters. Block
    /// kinds, marks and notes were not stored in that format.
    fn load_format_1(directory: &Path) -> Result<Self, BundleError> {
        let manifest = fs::read_to_string(directory.join("manifest.txt"))?;
        let mut book = Book::new(field(&manifest, "title").unwrap_or_else(|| "Untitled".into()));
        if let Some(size) = field(&manifest, "body-size")
            && let Ok(size_pt) = size.parse::<f32>()
        {
            let mut style = book
                .paragraph_styles()
                .iter()
                .find(|style| style.name == "Body")
                .cloned()
                .ok_or_else(|| BundleError {
                    message: "missing Body".into(),
                })?;
            style.size_pt = size_pt;
            book.set_paragraph_style(style)?;
        }
        if let Some(opener) = field(&manifest, "template-opener") {
            let mut template = book
                .chapter_templates()
                .iter()
                .find(|template| template.name == "Chapter")
                .cloned()
                .ok_or_else(|| BundleError {
                    message: "missing template".into(),
                })?;
            template.opener = opener;
            book.set_chapter_template(template)?;
        }
        let mut chapters = Vec::new();
        let mut index = 0usize;
        loop {
            let path = directory.join("chapters").join(format!("{index:04}.txt"));
            if !path.exists() {
                break;
            }
            chapters.push(fs::read_to_string(path)?);
            index += 1;
        }
        if chapters.is_empty() {
            return Err(BundleError {
                message: "bundle has no chapters".into(),
            });
        }
        book.load_chapters(&chapters)?;
        if let Some(pos) = field(&manifest, "caret")
            && let Some(focus) = position_at(&book, &pos)
        {
            let _ = book.set_selection(crate::Selection::collapsed(focus));
            book.remember_position();
        }
        if let Some(spec) = field(&manifest, "selection") {
            let mut ends = spec.split_whitespace();
            let anchor = ends.next().and_then(|end| position_at(&book, end));
            let focus = ends.next().and_then(|end| position_at(&book, end));
            if let (Some(anchor), Some(focus)) = (anchor, focus) {
                let _ = book.set_selection(crate::Selection { anchor, focus });
                book.remember_position();
            }
        }
        for line in manifest.lines() {
            if let Some(rest) = line.strip_prefix("source ") {
                let mut bits = rest.split('\t');
                let id = bits.next().unwrap_or("").to_string();
                let document = bits.next().unwrap_or("").to_string();
                let page = bits.next().unwrap_or("0").parse().unwrap_or(0);
                let passage = bits.next().unwrap_or("").to_string();
                if !id.is_empty() {
                    book.add_source(SourceNote {
                        id,
                        document,
                        page,
                        rects: Vec::new(),
                        passage,
                        citation: String::new(),
                        annotation: String::new(),
                    });
                }
            }
            if let Some(rest) = line.strip_prefix("bib ") {
                let mut bits = rest.split('\t');
                book.add_bibliography(BibliographyEntry {
                    key: bits.next().unwrap_or("").into(),
                    kind: bits.next().unwrap_or("book").into(),
                    title: bits.next().unwrap_or("").into(),
                    author: bits.next().unwrap_or("").into(),
                    issued: bits.next().unwrap_or("").into(),
                });
            }
        }
        Ok(book)
    }

    fn load_chapters(&mut self, chapters: &[String]) -> Result<(), BundleError> {
        // Rebuild by typing into the first chapter and adding the rest.
        let first = parse_chapter(&chapters[0]);
        self.set_chapter_title(self.parts()[0].chapters()[0].id(), first.title)?;
        // Clear the initial empty block by replacing the book text.
        let ids = self.block_ids();
        if let Some(id) = ids.first().copied() {
            let len = self.block_len(id)?;
            if len > 0 {
                self.set_selection(crate::Selection {
                    anchor: crate::Position::new(id, 0),
                    focus: crate::Position::new(id, len),
                })?;
                self.insert("")?;
            }
            if !first.blocks.is_empty() {
                self.set_selection(crate::Selection::collapsed(crate::Position::new(id, 0)))?;
                self.insert(&first.blocks.join("\n"))?;
            }
        }
        let part = self.parts()[0].id();
        for chapter in chapters.iter().skip(1) {
            let parsed = parse_chapter(chapter);
            self.add_chapter(part, parsed.title)?;
            let Some(id) = self.block_ids().last().copied() else {
                return Err(BundleError {
                    message: "new chapter has no block".into(),
                });
            };
            // add_chapter clears undo; the new block is empty.
            self.set_selection(crate::Selection::collapsed(crate::Position::new(id, 0)))?;
            if !parsed.blocks.is_empty() {
                self.insert(&parsed.blocks.join("\n"))?;
            }
        }
        Ok(())
    }

    fn set_chapter_title(&mut self, id: crate::ChapterId, title: String) -> Result<(), ModelError> {
        for part in self.parts_mut() {
            for chapter in part.chapters_mut() {
                if chapter.id() == id {
                    chapter.set_title(title);
                    return Ok(());
                }
            }
        }
        Err(ModelError::UnknownChapter(id))
    }
}

struct ParsedChapter {
    title: String,
    blocks: Vec<String>,
}

fn parse_chapter(text: &str) -> ParsedChapter {
    let mut title = String::new();
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            title = rest.to_string();
        } else if line.starts_with("@template ") || line.starts_with("@block ") {
            if in_block {
                blocks.push(std::mem::take(&mut current));
            }
            in_block = line.starts_with("@block ");
        } else if in_block {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if in_block {
        blocks.push(current);
    }
    ParsedChapter { title, blocks }
}

fn bundle_parent(directory: &Path) -> &Path {
    directory
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn snapshot_name(name: &str) -> Result<String, BundleError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.starts_with('.')
    {
        return Err(BundleError {
            message: "snapshot name is not a single path segment".into(),
        });
    }
    Ok(trimmed.to_string())
}

fn encode_position(book: &Book, pos: crate::Position) -> String {
    let index = book
        .block_ids()
        .iter()
        .position(|id| *id == pos.block)
        .unwrap_or(0);
    format!("{index}:{}", pos.offset)
}

fn position_at(book: &Book, spec: &str) -> Option<crate::Position> {
    let mut parts = spec.split(':');
    let block_index: usize = parts.next()?.parse().ok()?;
    let offset: usize = parts.next()?.parse().ok()?;
    let id = book.block_ids().get(block_index).copied()?;
    let len = book.block_len(id).ok()?;
    Some(crate::Position::new(id, offset.min(len)))
}

fn field(manifest: &str, name: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let mut parts = line.splitn(2, ' ');
        let key = parts.next()?;
        let value = parts.next().unwrap_or("");
        (key == name).then(|| value.to_string())
    })
}

fn chapter_file(chapter: &Chapter) -> String {
    format!("{}.json", chapter.id().raw())
}

fn json_error(err: serde_json::Error) -> BundleError {
    BundleError {
        message: format!("bundle JSON: {err}"),
    }
}
