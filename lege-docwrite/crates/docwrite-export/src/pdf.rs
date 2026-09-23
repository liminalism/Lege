//! Searchable PDF through `lege-pdf-write`.
//!
//! The embedded program is the subset of `font` that contains `used_glyphs`
//! when a subset is supplied; otherwise the whole face is embedded. Chapter
//! titles become bookmarks. `/Lang` and a structure tree are always written.

use std::sync::Arc;

use docwrite_model::Book;
use docwrite_typeset::Face;
use lege_pdf_write::artifact::{
    PageRotation, PdfPageArtifact, PreparedTextLayer, TextFont, TextRun,
};
use lege_pdf_write::font::{EmbeddedFont, ToUnicode};
use lege_pdf_write::outline::OutlineItem;
use lege_pdf_write::types::PdfRect;
use lege_pdf_write::writer::DocumentWriter;
use subsetter::GlyphRemapper;

use crate::blocks_of;

pub struct PdfExport {
    pub bytes: Vec<u8>,
    pub font_bytes: usize,
}

/// Embed a subset of `font` containing the glyphs the manuscript shapes.
pub fn export_pdf(book: &Book, font: &[u8]) -> Result<PdfExport, String> {
    let blocks = blocks_of(book);
    let program = subset_face(font, &blocks)?;
    let font_bytes = program.len();
    let pages = blocks.chunks(8).map(|c| c.to_vec()).collect::<Vec<_>>();
    let page_count = pages.len().max(1);
    let mut writer = DocumentWriter::new(Vec::new(), page_count).map_err(|err| err.to_string())?;
    writer.set_embedded_font(EmbeddedFont::glyphless(
        Arc::from(program),
        "DocwriteSubset".into(),
        800,
        -200,
        700,
        0.0,
        [0, -200, 1000, 800],
    ));
    // Identity ToUnicode is what glyphless uses, so the text layer extracts.
    let _ = ToUnicode::Identity;
    writer.set_language("en-US");
    let mut structure = Vec::new();
    let mut bookmarks = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let mut runs = Vec::new();
        let mut y = 720.0;
        for block in page {
            let structure_name = if block.kind == "heading" {
                "H1"
            } else if block.note.is_some() {
                "Note"
            } else if block.kind == "image" {
                "Figure"
            } else {
                "P"
            };
            structure.push(structure_name.to_string());
            if block.kind == "heading" {
                bookmarks.push(OutlineItem {
                    title: block.text.clone(),
                    page_index: index as u32,
                    top: Some(y as f32),
                    children: Vec::new(),
                });
            }
            runs.push(TextRun {
                text: block.text.clone(),
                x: 72.0,
                y,
                size: if block.kind == "heading" { 18.0 } else { 12.0 },
            });
            y -= 24.0;
        }
        if runs.is_empty() {
            runs.push(TextRun {
                text: book.title().to_string(),
                x: 72.0,
                y: 720.0,
                size: 12.0,
            });
            structure.push("P".into());
        }
        writer
            .add_page(&PdfPageArtifact {
                index: index as u32,
                media_box: PdfRect::from_size(432.0, 648.0),
                elements: Box::new([]),
                text_layer: Some(PreparedTextLayer {
                    runs: runs.into_boxed_slice(),
                    font: TextFont::Embedded,
                }),
                glyph_layer: None,
                rotation: PageRotation::Upright,
            })
            .map_err(|err| err.to_string())?;
    }
    writer.set_bookmarks(bookmarks);
    writer.set_structure(structure);
    writer.set_page_labels(b"<< /Nums [ 0 << /S /D >> ] >>".to_vec());
    let bytes = writer.finalize().map_err(|err| err.to_string())?;
    Ok(PdfExport {
        bytes,
        font_bytes,
    })
}

fn subset_face(font: &[u8], blocks: &[crate::ExportBlock]) -> Result<Vec<u8>, String> {
    let face = Face::parse(font.to_vec()).map_err(|err| err.to_string())?;
    let mut glyphs = vec![0u16];
    for block in blocks {
        for glyph in face.shape(&block.text, 12.0, &[]).map_err(|err| err.to_string())? {
            glyphs.push(glyph.id);
        }
    }
    glyphs.sort_unstable();
    glyphs.dedup();
    let mapper = GlyphRemapper::new_from_glyphs(&glyphs);
    subsetter::subset(font, 0, &mapper).map_err(|err| err.to_string())
}
