//! Typeset PDF through `lege-pdf-write`.
//!
//! The book is laid out with the same engine the editor uses, and every
//! page is written as it paginates: body lines, running head, folio and
//! footnotes as visible glyph runs in one embedded font. The font is the
//! subset of `font` holding the glyphs the book uses, with advance widths and
//! a ToUnicode map built from the shaping clusters, so the text extracts as
//! written. Chapters become bookmarks on the page each one opens.

use std::collections::BTreeMap;
use std::sync::Arc;

use docwrite_model::Book;
use docwrite_typeset::{Document, Face, Glyph, HYPHEN_CLUSTER, features_for, from_book};
use lege_pdf_write::artifact::{
    GlyphItem, GlyphLine, PageRotation, PdfPageArtifact, PreparedGlyphLayer,
};
use lege_pdf_write::font::{EmbeddedFont, ToUnicode, to_unicode_cmap};
use lege_pdf_write::outline::OutlineItem;
use lege_pdf_write::types::{Affine, PdfRect};
use lege_pdf_write::writer::DocumentWriter;
use subsetter::GlyphRemapper;

/// A finished PDF and the size of the font program embedded in it.
#[derive(Debug)]
pub struct PdfExport {
    /// The PDF file.
    pub bytes: Vec<u8>,
    /// Bytes of the embedded (subset) font program.
    pub font_bytes: usize,
}

/// Running heads and folios are set this size, footnotes smaller.
const APPARATUS_PT: f32 = 9.0;
const FOOTNOTE_PT: f32 = 8.0;

/// A glyph run placed on a page, before glyph ids are remapped into the subset.
struct PlacedLine {
    /// Left edge of the run, from the page's left edge.
    x: f32,
    /// Baseline, from the page's top edge.
    baseline: f32,
    em: f32,
    glyphs: Vec<PlacedGlyph>,
}

struct PlacedGlyph {
    gid: u16,
    advance: f32,
    rise: f32,
    /// Text this glyph stands for; empty when an earlier glyph of the same
    /// cluster already carries it.
    text: String,
}

/// Lay out `book` with `font` and write it as a PDF.
///
/// `font` must be a TrueType-outline font; CFF-flavoured OpenType is refused
/// until lege-pdf-write can embed FontFile3 programs.
pub fn export_pdf(book: &Book, font: &[u8]) -> Result<PdfExport, String> {
    if font.starts_with(b"OTTO") {
        return Err("CFF-outline fonts cannot be embedded yet; use a TrueType font".into());
    }
    let face = Face::parse(font.to_vec()).map_err(|err| err.to_string())?;
    let document = from_book(book, face).map_err(|err| err.to_string())?;
    let geometry = document.geometry();
    let pages: Vec<Vec<PlacedLine>> = (1..=document.page_count())
        .map(|page| place_page(&document, page))
        .collect::<Result<_, _>>()?;

    let mut used: Vec<u16> = vec![0];
    for line in pages.iter().flatten() {
        used.extend(line.glyphs.iter().map(|glyph| glyph.gid));
    }
    used.sort_unstable();
    used.dedup();
    let mapper = GlyphRemapper::new_from_glyphs_sorted(&used);
    let program = subsetter::subset(font, 0, &mapper).map_err(|err| err.to_string())?;
    let font_bytes = program.len();

    // Widths and text per subset glyph, from the first place each is used.
    let mut widths: BTreeMap<u16, u16> = BTreeMap::new();
    let mut unicode: BTreeMap<u16, String> = BTreeMap::new();
    for line in pages.iter().flatten() {
        for glyph in &line.glyphs {
            let Some(gid) = mapper.get(glyph.gid) else {
                continue;
            };
            widths
                .entry(gid)
                .or_insert_with(|| thousandths(glyph.advance, line.em).clamp(0, 65_535) as u16);
            if !glyph.text.is_empty() {
                unicode.entry(gid).or_insert_with(|| glyph.text.clone());
            }
        }
    }
    let width_count = widths.keys().last().map_or(1, |gid| usize::from(*gid) + 1);
    let mut cid_widths = vec![0u16; width_count];
    for (gid, width) in &widths {
        cid_widths[usize::from(*gid)] = *width;
    }
    let entries: Vec<(u16, String)> = unicode.into_iter().collect();

    let mut writer =
        DocumentWriter::new(Vec::new(), pages.len().max(1)).map_err(|err| err.to_string())?;
    writer.set_language("en-US");
    let page_height = f64::from(geometry.page_height);
    for (index, lines) in pages.iter().enumerate() {
        let glyph_lines: Vec<GlyphLine> = lines
            .iter()
            .filter(|line| !line.glyphs.is_empty())
            .map(|line| glyph_line(line, &mapper, &widths, page_height))
            .collect();
        writer
            .add_page(&PdfPageArtifact {
                index: index as u32,
                media_box: PdfRect::from_size(
                    f64::from(geometry.page_width),
                    f64::from(geometry.page_height),
                ),
                elements: Box::new([]),
                text_layer: None,
                glyph_layer: Some(PreparedGlyphLayer {
                    lines: glyph_lines.into_boxed_slice(),
                    font: 0,
                }),
                rotation: PageRotation::Upright,
            })
            .map_err(|err| err.to_string())?;
    }
    writer.set_glyph_font(EmbeddedFont {
        data: Arc::from(program),
        post_script_name: "DocwriteSubset".into(),
        ascent: 800,
        descent: -200,
        cap_height: 700,
        italic_angle: 0.0,
        bbox: [0, -250, 1000, 900],
        symbolic: true,
        to_unicode: ToUnicode::Custom(Arc::from(to_unicode_cmap(&entries))),
        cid_widths: Some(Arc::from(cid_widths)),
        compress_program: true,
    });
    writer.set_bookmarks(bookmarks(book, &document));
    writer.set_structure(structure(&document));
    writer.set_page_labels(b"<< /Nums [ 0 << /S /D >> ] >>".to_vec());
    let bytes = writer.finalize().map_err(|err| err.to_string())?;
    Ok(PdfExport { bytes, font_bytes })
}

/// Everything drawn on 1-based `page`, in page coordinates from the top left.
fn place_page(document: &Document, page: u32) -> Result<Vec<PlacedLine>, String> {
    let geometry = document.geometry();
    let inset = document.page_content_inset(page);
    let mut placed = Vec::new();
    for line in document.page_painted_lines(page) {
        let text = document
            .paragraphs()
            .get(line.paragraph)
            .map(|paragraph| paragraph.text.as_str())
            .unwrap_or("");
        placed.push(PlacedLine {
            x: inset + line.indent,
            baseline: line.baseline,
            em: line.em,
            glyphs: placed_glyphs(text, &line.glyphs),
        });
    }
    if let Some(head) = document.page_running_head(page) {
        placed.push(set_line(
            document,
            &head,
            APPARATUS_PT,
            inset,
            geometry.margin_top * 0.5,
        )?);
    }
    if let Some(folio) = document.page_folio(page) {
        let mut line = set_line(
            document,
            &folio,
            APPARATUS_PT,
            0.0,
            geometry.page_height - geometry.margin_bottom * 0.4,
        )?;
        let width: f32 = line.glyphs.iter().map(|glyph| glyph.advance).sum();
        line.x = (geometry.page_width - width) / 2.0;
        placed.push(line);
    }
    let notes = document.page_footnotes(page);
    for (index, note) in notes.iter().enumerate() {
        let baseline = geometry.page_height - geometry.margin_bottom
            + FOOTNOTE_PT * 1.4 * (index as f32 + 1.0);
        placed.push(set_line(document, note, FOOTNOTE_PT, inset, baseline)?);
    }
    Ok(placed)
}

/// Shape `text` as one line at `size`.
fn set_line(
    document: &Document,
    text: &str,
    size: f32,
    x: f32,
    baseline: f32,
) -> Result<PlacedLine, String> {
    let glyphs = document
        .face()
        .shape(text, size, &features_for(false, false))
        .map_err(|err| err.to_string())?;
    Ok(PlacedLine {
        x,
        baseline,
        em: size,
        glyphs: placed_glyphs(text, &glyphs),
    })
}

/// Pair each glyph with the text of its cluster. Clusters are byte offsets
/// into `text`; a cluster spans to the next larger cluster in the line.
fn placed_glyphs(text: &str, glyphs: &[Glyph]) -> Vec<PlacedGlyph> {
    let mut starts: Vec<usize> = glyphs
        .iter()
        .map(|glyph| glyph.cluster)
        .filter(|cluster| *cluster != HYPHEN_CLUSTER)
        .map(|cluster| cluster as usize)
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let mut claimed = std::collections::HashSet::new();
    glyphs
        .iter()
        .map(|glyph| {
            let text = if glyph.cluster == HYPHEN_CLUSTER {
                "-".to_string()
            } else {
                let start = glyph.cluster as usize;
                if claimed.insert(start) {
                    let end = starts
                        .iter()
                        .find(|next| **next > start)
                        .copied()
                        .unwrap_or_else(|| char_end(text, start));
                    text.get(start..end).unwrap_or("").to_string()
                } else {
                    String::new()
                }
            };
            PlacedGlyph {
                gid: glyph.id,
                advance: glyph.x_advance,
                rise: glyph.y_offset,
                text,
            }
        })
        .collect()
}

/// End of the character starting at `start`, or `start` when out of range.
fn char_end(text: &str, start: usize) -> usize {
    text.get(start..)
        .and_then(|rest| rest.chars().next())
        .map_or(start, |ch| start + ch.len_utf8())
}

/// A PDF text line: the matrix scales the size-1 font to the line's em and
/// puts the pen on the baseline; `TJ` adjustments turn each subset glyph's
/// nominal width into its shaped advance, kerning included.
fn glyph_line(
    line: &PlacedLine,
    mapper: &GlyphRemapper,
    widths: &BTreeMap<u16, u16>,
    page_height: f64,
) -> GlyphLine {
    let em = f64::from(line.em.max(0.1));
    let mut items = Vec::with_capacity(line.glyphs.len());
    let mut correction = 0;
    for glyph in &line.glyphs {
        let gid = mapper.get(glyph.gid).unwrap_or(0);
        items.push(GlyphItem {
            gid,
            adjust: correction,
            rise: thousandths(glyph.rise, line.em),
        });
        let nominal = widths.get(&gid).copied().map_or(0, i32::from);
        correction = nominal - thousandths(glyph.advance, line.em);
    }
    GlyphLine {
        matrix: Affine::scale_translate(
            em,
            em,
            f64::from(line.x),
            page_height - f64::from(line.baseline),
        ),
        items: items.into_boxed_slice(),
    }
}

fn thousandths(length: f32, em: f32) -> i32 {
    if em <= 0.0 {
        return 0;
    }
    (length / em * 1000.0).round() as i32
}

/// One bookmark per chapter, on the page its first block lands on.
fn bookmarks(book: &Book, document: &Document) -> Vec<OutlineItem> {
    let ids = book.block_ids();
    let top = document.geometry().page_height - document.geometry().margin_top;
    let mut items = Vec::new();
    for part in book.parts() {
        for chapter in part.chapters() {
            let Some(first) = chapter
                .sections()
                .iter()
                .flat_map(|section| section.blocks())
                .next()
            else {
                continue;
            };
            let Some(index) = ids.iter().position(|id| *id == first.id()) else {
                continue;
            };
            let Some(page) = document.page_of(index, 0) else {
                continue;
            };
            items.push(OutlineItem {
                title: chapter.title().to_string(),
                page_index: page - 1,
                top: Some(top),
                children: Vec::new(),
            });
        }
    }
    items
}

/// Structure element names in reading order, one per paragraph.
fn structure(document: &Document) -> Vec<String> {
    document
        .paragraphs()
        .iter()
        .map(|paragraph| {
            match paragraph.style.name.as_str() {
                "Chapter Title" => "H1",
                "Image" => "Figure",
                "Caption" => "Caption",
                _ if paragraph.note.is_some() => "P",
                _ => "P",
            }
            .to_string()
        })
        .collect()
}
