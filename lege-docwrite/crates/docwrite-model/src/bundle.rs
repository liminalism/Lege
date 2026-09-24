//! `.legebook` directory bundle. Writes are temp-file then rename.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::ModelError;
use crate::publish::{BibliographyEntry, SourceNote};
use crate::tree::{BlockKind, Book};

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
        if !directory.join("manifest.txt").is_file() {
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
        if temp.exists() {
            fs::remove_dir_all(&temp)?;
        }
        fs::create_dir_all(temp.join("chapters"))?;
        fs::write(temp.join("manifest.txt"), self.manifest_text())?;
        for (index, chapter) in self.chapter_records().into_iter().enumerate() {
            fs::write(
                temp.join("chapters").join(format!("{index:04}.txt")),
                chapter,
            )?;
        }
        if directory.exists() {
            fs::remove_dir_all(directory)?;
        }
        fs::rename(&temp, directory)?;
        Ok(())
    }

    /// Open a bundle written by [`Self::save_bundle`].
    pub fn load_bundle(directory: &Path) -> Result<Self, BundleError> {
        let manifest = fs::read_to_string(directory.join("manifest.txt"))?;
        let mut book = Book::new(field(&manifest, "title").unwrap_or_else(|| "Untitled".into()));
        if let Some(size) = field(&manifest, "body-size") {
            if let Ok(size_pt) = size.parse::<f32>() {
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
        if let Some(pos) = field(&manifest, "caret") {
            if let Some(focus) = position_at(&book, &pos) {
                let _ = book.set_selection(crate::Selection::collapsed(focus));
                book.remember_position();
            }
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

    fn manifest_text(&self) -> String {
        let body = self
            .paragraph_styles()
            .iter()
            .find(|style| style.name == "Body")
            .map(|style| style.size_pt)
            .unwrap_or(12.0);
        let opener = self
            .chapter_templates()
            .iter()
            .find(|template| template.name == "Chapter")
            .map(|template| template.opener.as_str())
            .unwrap_or("Chapter");
        let caret = self.saved_position().map(|pos| encode_position(self, pos));
        let selection = self.selection();
        let selection = format!(
            "{} {}",
            encode_position(self, selection.anchor),
            encode_position(self, selection.focus)
        );
        let mut text = format!(
            "title {}\nbody-size {body}\ntemplate-opener {opener}\ncaret {}\nselection {selection}\n",
            self.title().replace('\n', " "),
            caret.unwrap_or_else(|| "0:0".into())
        );
        for source in self.stylesheet().sources.iter() {
            text.push_str(&format!(
                "source {}\t{}\t{}\t{}\n",
                source.id,
                source.document,
                source.page,
                source.passage.replace('\n', " ")
            ));
        }
        for entry in self.bibliography() {
            text.push_str(&format!(
                "bib {}\t{}\t{}\t{}\t{}\n",
                entry.key, entry.kind, entry.title, entry.author, entry.issued
            ));
        }
        text
    }

    fn chapter_records(&self) -> Vec<String> {
        let mut records = Vec::new();
        for part in self.parts() {
            for chapter in part.chapters() {
                let mut body = format!("# {}\n@template {}\n", chapter.title(), chapter.template());
                for section in chapter.sections() {
                    for block in section.blocks() {
                        body.push_str(&format!("@block {:?}\n{}\n", block.kind(), block.text()));
                    }
                }
                records.push(body);
            }
        }
        records
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

/// Path helper for tests.
pub fn bundle_path(root: &Path, name: &str) -> PathBuf {
    root.join(format!("{name}.legebook"))
}

impl BlockKind {
    /// Stable label stored in a chapter file.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::ChapterTitle => "chapter-title",
            Self::Subhead => "subhead",
            Self::BlockQuote => "quote",
            Self::Epigraph => "epigraph",
            Self::Image { .. } => "image",
            Self::Caption => "caption",
            Self::Verse => "verse",
            Self::BibliographyEntry => "bibliography",
            Self::SceneBreak => "break",
        }
    }
}
