//! DocumentWriter: write-on-arrival PDF assembly.
//!
//! `add_page` writes each artifact's objects to the sink the instant it is
//! submitted (any order), recording the page object id by logical index;
//! `finalize` writes the flat page tree, catalog, outline, metadata, xref, and
//! trailer. Object numbering is arrival-order (varies run-to-run); page order
//! and pixels do not. The async bounded-channel actor that drives this
//! (replacing the `spawn_pdf_writer_actor` body) arrives in M3.

use std::io::Write;
use std::sync::Arc;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::artifact::{PdfPageArtifact, TextFont};
use crate::content::ContentWriter;
use crate::font::{EmbeddedFont, write_embedded_font, write_embedded_font_at, write_helvetica};
use crate::images::write_image_xobject;
use crate::meta::{DocumentMeta, PdfProfile, write_info, write_output_intent};
use crate::outline::{OutlineItem, write_outline};
use crate::pages::{
    CatalogExtras, WrittenPageSlots, write_catalog, write_length_dict, write_page_dict,
    write_pages_root,
};
use crate::resources::{ResourceRegistry, SharedResourceId};
use crate::sink::{PdfSink, StreamBody};
use crate::text::{emit_glyph_layer, emit_text_layer};
use crate::types::{ObjectId, ResourceName, Result, WriteError};
use crate::xref::write_xref_and_trailer;

/// Write-on-arrival PDF document assembler.
pub struct DocumentWriter<W: Write> {
    sink: PdfSink<W>,
    pages_root: ObjectId,
    slots: WrittenPageSlots,
    registry: ResourceRegistry,
    saw_text: bool,
    profile: PdfProfile,
    meta: DocumentMeta,
    metadata_explicit: bool,
    embedded_font: Option<EmbeddedFont>,
    /// Memoized Type0 font id (written on the first text-bearing page).
    font_id: Option<ObjectId>,
    /// Memoized Helvetica fallback id.
    helvetica_id: Option<ObjectId>,
    /// The glyph font programs, supplied before `finalize`. A document with
    /// more shapes than one font's 16-bit id space holds carries several.
    glyph_fonts: Vec<EmbeddedFont>,
    /// Reserved Type0 id per glyph font bank: allocated by the first page
    /// that draws from that bank, written at finalization.
    glyph_font_ids: Vec<Option<ObjectId>>,
    bookmarks: Vec<OutlineItem>,
    lang: Option<String>,
    /// Structure element names (`H1`, `P`, `Note`, `Figure`) in reading order.
    structure: Vec<String>,
    page_labels: Option<Vec<u8>>,
}

impl<W: Write> std::fmt::Debug for DocumentWriter<W> {
    // Manual: `#[derive(Debug)]` would add a `where W: Debug` bound (from the
    // `sink: PdfSink<W>` field) that constrains every call site, even though
    // `PdfSink`'s own manual `Debug` impl doesn't actually need it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentWriter")
            .field("sink", &self.sink)
            .field("pages_root", &self.pages_root)
            .field("slots", &self.slots)
            .field("registry", &self.registry)
            .field("saw_text", &self.saw_text)
            .field("profile", &self.profile)
            .field("meta", &self.meta)
            .field("metadata_explicit", &self.metadata_explicit)
            .field("embedded_font", &self.embedded_font)
            .field("font_id", &self.font_id)
            .field("helvetica_id", &self.helvetica_id)
            .field("glyph_fonts", &self.glyph_fonts)
            .field("glyph_font_ids", &self.glyph_font_ids)
            .field("bookmarks", &self.bookmarks)
            .finish()
    }
}

impl<W: Write> DocumentWriter<W> {
    /// Create a writer using the default (PDF 1.7 image-only) profile.
    pub fn new(writer: W, total_pages: usize) -> Result<Self> {
        Self::with_profile(writer, total_pages, PdfProfile::default())
    }

    /// Create a writer with an explicit conformance profile. The profile fixes
    /// the file version, so it is chosen up front (the caller knows whether OCR
    /// is enabled for the job).
    pub fn with_profile(writer: W, total_pages: usize, profile: PdfProfile) -> Result<Self> {
        let sink = PdfSink::new(writer, profile.version())?;
        let mut w = Self {
            sink,
            pages_root: ObjectId::new(0),
            slots: WrittenPageSlots::new(total_pages),
            registry: ResourceRegistry::new(),
            saw_text: false,
            profile,
            meta: DocumentMeta {
                profile,
                ..Default::default()
            },
            metadata_explicit: false,
            embedded_font: None,
            font_id: None,
            helvetica_id: None,
            glyph_fonts: Vec::new(),
            glyph_font_ids: Vec::new(),
            bookmarks: Vec::new(),
            lang: None,
            structure: Vec::new(),
            page_labels: None,
        };
        // Reserve the /Pages root so pages written on arrival can carry a valid
        // /Parent.
        w.pages_root = w.sink.alloc_id();
        Ok(w)
    }

    /// Provide the embedded OCR font (Lege's glyphless program + metrics).
    pub fn set_embedded_font(&mut self, font: EmbeddedFont) {
        self.embedded_font = Some(font);
    }

    /// Provide the document-wide glyph font (visible raster-text glyphs). It
    /// may arrive at any time before `finalize`, typically after the last page,
    /// because the glyph set is only complete once every page has been seen.
    /// Pages carrying a glyph layer reference its reserved object id; the
    /// program itself is written at finalization.
    pub fn set_glyph_font(&mut self, font: EmbeddedFont) {
        self.set_glyph_fonts(vec![font]);
    }

    /// Provide the glyph fonts, bank by bank (see
    /// [`PreparedGlyphLayer::font`](crate::artifact::PreparedGlyphLayer)).
    pub fn set_glyph_fonts(&mut self, fonts: Vec<EmbeddedFont>) {
        self.glyph_fonts = fonts;
    }

    /// Provide document metadata (dates, title, …). The profile is taken from
    /// the constructor; this preserves it.
    pub fn set_metadata(&mut self, mut meta: DocumentMeta) {
        meta.profile = self.profile;
        self.meta = meta;
        self.metadata_explicit = true;
    }

    /// Provide the outline (bookmark) tree, keyed by output page index.
    pub fn set_bookmarks(&mut self, bookmarks: Vec<OutlineItem>) {
        self.bookmarks = bookmarks;
    }

    /// `/Lang` on the catalog.
    pub fn set_language(&mut self, lang: impl Into<String>) {
        self.lang = Some(lang.into());
    }

    /// Structure element types written as a `/StructTreeRoot`.
    pub fn set_structure(&mut self, elements: Vec<String>) {
        self.structure = elements;
    }

    /// Raw `/PageLabels` number-tree dictionary, including the outer `<< >>`.
    pub fn set_page_labels(&mut self, labels: Vec<u8>) {
        self.page_labels = Some(labels);
    }

    /// Register a shared resource's bytes (e.g. JBIG2 globals) before any
    /// artifact references it.
    pub fn register_shared(&mut self, id: SharedResourceId, bytes: Arc<[u8]>) {
        self.registry.register(id, bytes);
    }

    /// Write one page's objects to the sink immediately, in arrival order.
    pub fn add_page(&mut self, artifact: &PdfPageArtifact) -> Result<()> {
        let mut xobjects = Vec::with_capacity(artifact.elements.len());
        let mut content = ContentWriter::new();

        for (i, element) in artifact.elements.iter().enumerate() {
            let img_id = write_image_xobject(&mut self.sink, &mut self.registry, &element.image)?;
            let name = ResourceName::Image((i + 1) as u32);
            xobjects.push((name, img_id));

            content.save();
            if element.image.is_image_mask() {
                content.set_gray_fill(0.0);
            }
            content.concat_matrix(element.transform);
            content.draw_xobject(name);
            content.restore();
        }

        for fill in artifact.fills.iter() {
            let rect = fill.rect;
            content.save();
            content.set_gray_fill(fill.gray);
            content.fill_rect(rect.x0, rect.y0, rect.x1 - rect.x0, rect.y1 - rect.y0);
            content.restore();
        }

        // Text layer: emit into the same content stream and select fonts.
        let mut fonts: Vec<(ResourceName, ObjectId)> = Vec::new();
        let has_text = artifact
            .text_layer
            .as_ref()
            .is_some_and(|tl| !tl.runs.is_empty());
        if has_text {
            let layer = artifact.text_layer.as_ref().unwrap();
            match layer.font {
                TextFont::Embedded => {
                    let fid = self.ensure_embedded_font()?;
                    fonts.push((ResourceName::Font(0), fid));
                }
                TextFont::HelveticaFallback => {
                    let hid = self.ensure_helvetica()?;
                    fonts.push((ResourceName::Font(1), hid));
                }
            }
            emit_text_layer(&mut content, layer);
            self.saw_text = true;
        }

        // Visible glyph text: reserve the document font id on first use; the
        // program is written at finalize.
        let has_glyphs = artifact
            .glyph_layer
            .as_ref()
            .is_some_and(|gl| gl.lines.iter().any(|l| !l.items.is_empty()));
        if has_glyphs {
            let layer = artifact.glyph_layer.as_ref().unwrap();
            // Every bank the page draws from: the layer's and any line's own.
            let mut banks = vec![layer.font];
            for line in layer.lines.iter() {
                if let Some(bank) = line.font
                    && !banks.contains(&bank)
                {
                    banks.push(bank);
                }
            }
            for bank in banks {
                if self.glyph_font_ids.len() <= usize::from(bank) {
                    self.glyph_font_ids.resize(usize::from(bank) + 1, None);
                }
                let gid = match self.glyph_font_ids[usize::from(bank)] {
                    Some(id) => id,
                    None => {
                        let id = self.sink.alloc_id();
                        self.glyph_font_ids[usize::from(bank)] = Some(id);
                        id
                    }
                };
                fonts.push((crate::artifact::glyph_font_resource(bank), gid));
            }
            emit_glyph_layer(&mut content, layer);
        }

        // Content stream: compress when it carries text (parity with the
        // current writer), uncompressed for pure image pages.
        let content_bytes = content.into_bytes();
        let contents_id = self.sink.alloc_id();
        if has_text || has_glyphs {
            let compressed = deflate(&content_bytes)?;
            let mut cdict = Vec::new();
            cdict.extend_from_slice(b"<</Filter /FlateDecode /Length ");
            crate::serialize::write_u64(&mut cdict, compressed.len() as u64);
            cdict.extend_from_slice(b">>");
            self.sink
                .write_stream(contents_id, &cdict, &StreamBody::Owned(compressed))?;
        } else {
            let mut cdict = Vec::new();
            write_length_dict(&mut cdict, content_bytes.len());
            self.sink
                .write_stream(contents_id, &cdict, &StreamBody::Owned(content_bytes))?;
        }

        let page_id = self.sink.alloc_id();
        write_page_dict(
            &mut self.sink,
            page_id,
            self.pages_root,
            artifact.media_box,
            &xobjects,
            &fonts,
            contents_id,
            artifact.rotation,
        )?;

        self.slots.set(artifact.index, page_id)?;
        Ok(())
    }

    pub fn saw_text(&self) -> bool {
        self.saw_text
    }

    /// Finish the document: page tree, outline, metadata, catalog, xref,
    /// trailer. Errors (listing gaps) if any logical page never arrived.
    pub fn finalize(mut self) -> Result<W> {
        for (bank, id) in self.glyph_font_ids.clone().into_iter().enumerate() {
            let Some(id) = id else {
                continue;
            };
            let font = self.glyph_fonts.get(bank).ok_or_else(|| {
                WriteError::InvalidArtifact(format!(
                    "pages draw from glyph font {bank} but only {} were set before finalize",
                    self.glyph_fonts.len()
                ))
            })?;
            write_embedded_font_at(&mut self.sink, font, id)?;
        }

        write_pages_root(&mut self.sink, self.pages_root, &self.slots)?;

        let outlines = if self.bookmarks.is_empty() {
            None
        } else {
            let items = std::mem::take(&mut self.bookmarks);
            write_outline(&mut self.sink, &self.slots, &items)?
        };

        let output_intent = if self.profile.wants_pdfa_metadata() {
            Some(write_output_intent(&mut self.sink)?)
        } else {
            None
        };
        let info = if self.profile.wants_pdfa_metadata() || self.metadata_explicit {
            Some(write_info(&mut self.sink, &self.meta)?)
        } else {
            None
        };

        let struct_tree = if self.structure.is_empty() {
            None
        } else {
            Some(write_structure_tree(
                &mut self.sink,
                &self.slots,
                &self.structure,
            )?)
        };
        let catalog = write_catalog(
            &mut self.sink,
            self.pages_root,
            CatalogExtras {
                outlines,
                output_intent,
                mark_info: self.profile.wants_pdfa_metadata() || struct_tree.is_some(),
                lang: self.lang.clone(),
                struct_tree,
                page_labels: self.page_labels.clone(),
            },
        )?;

        write_xref_and_trailer(&mut self.sink, catalog, info)?;
        self.sink.finish()
    }

    // --- internals ------------------------------------------------------

    fn ensure_embedded_font(&mut self) -> Result<ObjectId> {
        if let Some(id) = self.font_id {
            return Ok(id);
        }
        let font = self.embedded_font.as_ref().ok_or_else(|| {
            WriteError::InvalidArtifact(
                "text layer uses the embedded font but none was set".to_string(),
            )
        })?;
        let id = write_embedded_font(&mut self.sink, font)?;
        self.font_id = Some(id);
        Ok(id)
    }

    fn ensure_helvetica(&mut self) -> Result<ObjectId> {
        if let Some(id) = self.helvetica_id {
            return Ok(id);
        }
        let id = write_helvetica(&mut self.sink)?;
        self.helvetica_id = Some(id);
        Ok(id)
    }
}

/// Deflate (zlib) a content stream.
fn write_structure_tree<W: Write>(
    sink: &mut PdfSink<W>,
    slots: &crate::pages::WrittenPageSlots,
    elements: &[String],
) -> Result<ObjectId> {
    let mut kids = Vec::new();
    for (index, name) in elements.iter().enumerate() {
        let id = sink.alloc_id();
        let Some(page) = slots.page_id(index as u32).or_else(|| slots.page_id(0)) else {
            continue;
        };
        let mut d = Vec::new();
        d.extend_from_slice(b"<<");
        d.extend_from_slice(b"/Type /StructElem");
        d.extend_from_slice(b" /S /");
        d.extend_from_slice(name.as_bytes());
        d.extend_from_slice(b" /Pg ");
        crate::serialize::write_ref(&mut d, page);
        d.extend_from_slice(b" /K (");
        d.extend_from_slice(name.as_bytes());
        d.extend_from_slice(b")>>");
        sink.write_indirect(id, &d)?;
        kids.push(id);
    }
    let root = sink.alloc_id();
    let mut d = Vec::new();
    d.extend_from_slice(b"<< /Type /StructTreeRoot /K [");
    for (index, id) in kids.iter().enumerate() {
        if index > 0 {
            d.push(b' ');
        }
        crate::serialize::write_ref(&mut d, *id);
    }
    d.extend_from_slice(b"]>>");
    sink.write_indirect(root, &d)?;
    Ok(root)
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).map_err(WriteError::from)?;
    enc.finish().map_err(WriteError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{
        ColorModel, GlyphItem, GlyphLine, PageRotation, PdfImageElement, PdfImageResource,
        PreparedGlyphLayer, PreparedTextLayer, TextRun,
    };
    use crate::font::ToUnicode;
    use crate::types::{Affine, PdfRect};

    fn jpeg_page(index: u32) -> PdfPageArtifact {
        PdfPageArtifact {
            index,
            media_box: PdfRect::from_size(612.0, 792.0),
            elements: Box::new([PdfImageElement {
                transform: Affine::scale_translate(612.0, 792.0, 0.0, 0.0),
                image: PdfImageResource::Jpeg {
                    data: Arc::from(&[0xFFu8, 0xD8, 0xFF, 0xD9][..]),
                    width: 100,
                    height: 129,
                    color: ColorModel::Rgb,
                },
            }]),
            fills: Box::new([]),
            text_layer: None,
            glyph_layer: None,
            rotation: PageRotation::Upright,
        }
    }

    fn sample_font() -> EmbeddedFont {
        EmbeddedFont::glyphless(
            Arc::from(&[0u8; 32][..]),
            "Glyphless".to_string(),
            1000,
            -200,
            700,
            0.0,
            [-100, -200, 1000, 900],
        )
    }

    fn glyph_page(index: u32) -> PdfPageArtifact {
        PdfPageArtifact {
            index,
            media_box: PdfRect::from_size(612.0, 792.0),
            elements: Box::new([]),
            fills: Box::new([]),
            text_layer: None,
            glyph_layer: Some(PreparedGlyphLayer {
                lines: Box::new([GlyphLine {
                    matrix: Affine::scale_translate(100.0, 100.0, 72.0, 700.0),
                    items: Box::new([GlyphItem {
                        gid: 1,
                        adjust: 0,
                        rise: 0,
                    }]),
                    font: None,
                }]),
                font: 0,
            }),
            rotation: PageRotation::Upright,
        }
    }

    #[test]
    fn a_second_glyph_bank_gets_its_own_font_and_resource() {
        let mut w = DocumentWriter::new(Vec::new(), 2).unwrap();
        let mut second = glyph_page(1);
        second.glyph_layer.as_mut().unwrap().font = 1;
        w.add_page(&glyph_page(0)).unwrap();
        w.add_page(&second).unwrap();
        let mut font = sample_font();
        font.symbolic = true;
        font.to_unicode = ToUnicode::None;
        font.cid_widths = Some(Arc::from(&[0u16, 500][..]));
        w.set_glyph_fonts(vec![font.clone(), font]);
        let bytes = w.finalize().unwrap();
        let t = String::from_utf8_lossy(&bytes).into_owned();
        assert_eq!(
            t.matches("/Subtype /Type0").count(),
            2,
            "one font per bank: {t}"
        );
        assert!(t.contains("/F2"), "{t}");
        assert!(t.contains("/F3"), "{t}");
    }

    #[test]
    fn one_page_can_draw_lines_from_several_banks() {
        let mut w = DocumentWriter::new(Vec::new(), 1).unwrap();
        let mut page = glyph_page(0);
        let layer = page.glyph_layer.as_mut().unwrap();
        let mut lines = std::mem::take(&mut layer.lines).into_vec();
        lines.push(GlyphLine {
            matrix: Affine::scale_translate(100.0, 100.0, 72.0, 600.0),
            items: Box::new([GlyphItem {
                gid: 1,
                adjust: 0,
                rise: 0,
            }]),
            font: Some(1),
        });
        layer.lines = lines.into_boxed_slice();
        w.add_page(&page).unwrap();
        let mut font = sample_font();
        font.symbolic = true;
        font.to_unicode = ToUnicode::None;
        font.cid_widths = Some(Arc::from(&[0u16, 500][..]));
        w.set_glyph_fonts(vec![font.clone(), font]);
        let bytes = w.finalize().unwrap();
        let t = String::from_utf8_lossy(&bytes).into_owned();
        assert_eq!(t.matches("/Subtype /Type0").count(), 2, "{t}");
        assert!(
            t.contains("/F2") && t.contains("/F3"),
            "both banks are page resources"
        );
    }

    #[test]
    fn fills_are_drawn_as_gray_rectangles() {
        let mut w = DocumentWriter::new(Vec::new(), 1).unwrap();
        let mut page = glyph_page(0);
        page.fills = Box::new([crate::artifact::PdfFill {
            rect: PdfRect::new(72.0, 100.0, 172.0, 100.5),
            gray: 0.0,
        }]);
        w.add_page(&page).unwrap();
        let mut font = sample_font();
        font.symbolic = true;
        font.to_unicode = ToUnicode::None;
        font.cid_widths = Some(Arc::from(&[0u16, 500][..]));
        w.set_glyph_fonts(vec![font]);
        let bytes = w.finalize().unwrap();
        // The content stream is compressed when it has text: inflate them all.
        let mut content = Vec::new();
        for (index, _) in bytes
            .windows(6)
            .enumerate()
            .filter(|(_, window)| *window == b"stream")
        {
            let start = index
                + 6
                + if bytes.get(index + 6) == Some(&b'\r') {
                    2
                } else {
                    1
                };
            let mut data = Vec::new();
            let mut decoder = flate2::read::ZlibDecoder::new(&bytes[start..]);
            if std::io::Read::read_to_end(&mut decoder, &mut data).is_ok() {
                content.extend(data);
            }
        }
        let text = String::from_utf8_lossy(&content);
        assert!(text.contains("72 100 100 0.5 re f"), "{text}");
    }

    #[test]
    fn a_bank_without_a_font_fails_finalize() {
        let mut w = DocumentWriter::new(Vec::new(), 1).unwrap();
        let mut page = glyph_page(0);
        page.glyph_layer.as_mut().unwrap().font = 1;
        w.add_page(&page).unwrap();
        let mut font = sample_font();
        font.symbolic = true;
        font.to_unicode = ToUnicode::None;
        w.set_glyph_fonts(vec![font]);
        assert!(w.finalize().is_err());
    }

    #[test]
    fn glyph_font_is_written_at_finalize_under_the_reserved_id() {
        let mut w = DocumentWriter::new(Vec::new(), 2).unwrap();
        w.add_page(&glyph_page(1)).unwrap();
        w.add_page(&glyph_page(0)).unwrap();
        let reserved = w.glyph_font_ids[0].expect("first glyph page reserves the id");
        let mut font = sample_font();
        font.symbolic = true;
        font.to_unicode = ToUnicode::None;
        font.cid_widths = Some(Arc::from(&[0u16, 500][..]));
        w.set_glyph_font(font);
        let bytes = w.finalize().unwrap();
        let t = String::from_utf8_lossy(&bytes).into_owned();
        let needle = format!("{} 0 obj\n<</Type /Font/Subtype /Type0", reserved.num);
        assert!(t.contains(&needle), "{t}");
        assert_eq!(
            t.matches("/Subtype /Type0").count(),
            1,
            "one font for the whole document"
        );
        assert!(t.contains(&format!("/F2 {} 0 R", reserved.num)), "{t}");
        assert!(
            t.contains("/FlateDecode"),
            "glyph text content is compressed: {t}"
        );
        assert!(t.contains("/W [0 [0 500]]"), "{t}");
    }

    #[test]
    fn glyph_pages_without_a_font_fail_finalize() {
        let mut w = DocumentWriter::new(Vec::new(), 1).unwrap();
        w.add_page(&glyph_page(0)).unwrap();
        let err = w.finalize().unwrap_err();
        assert!(matches!(err, WriteError::InvalidArtifact(_)), "{err:?}");
    }

    #[test]
    fn out_of_order_pages_finalize_in_order() {
        let mut w = DocumentWriter::new(Vec::new(), 3).unwrap();
        w.add_page(&jpeg_page(2)).unwrap();
        w.add_page(&jpeg_page(0)).unwrap();
        w.add_page(&jpeg_page(1)).unwrap();
        let bytes = w.finalize().unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(text.starts_with("%PDF-1.7"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Count 3"));
        assert!(text.trim_end().ends_with("%%EOF"));
    }

    #[test]
    fn missing_page_is_a_finalize_error() {
        let mut w = DocumentWriter::new(Vec::new(), 2).unwrap();
        w.add_page(&jpeg_page(0)).unwrap();
        let err = w.finalize().unwrap_err();
        assert!(matches!(
            err,
            WriteError::IncompletePages {
                missing: 1,
                total: 2
            }
        ));
    }

    #[test]
    fn pdf17_writes_info_only_when_identity_was_explicitly_set() {
        let mut plain = DocumentWriter::new(Vec::new(), 1).unwrap();
        plain.add_page(&jpeg_page(0)).unwrap();
        let plain = String::from_utf8_lossy(&plain.finalize().unwrap()).into_owned();
        assert!(!plain.contains("/Info "));

        let mut identified = DocumentWriter::new(Vec::new(), 1).unwrap();
        identified.set_metadata(DocumentMeta {
            title: "The Book".to_string(),
            author: "Ada Lovelace".to_string(),
            subject: String::new(),
            keywords: String::new(),
            ..Default::default()
        });
        identified.add_page(&jpeg_page(0)).unwrap();
        let identified = String::from_utf8_lossy(&identified.finalize().unwrap()).into_owned();
        assert!(identified.contains("/Info "));
        assert!(identified.contains("/Title (The Book)"));
        assert!(identified.contains("/Author (Ada Lovelace)"));
        assert!(!identified.contains("/Subject "));
    }

    #[test]
    fn duplicate_index_rejected() {
        let mut w = DocumentWriter::new(Vec::new(), 2).unwrap();
        w.add_page(&jpeg_page(0)).unwrap();
        let err = w.add_page(&jpeg_page(0)).unwrap_err();
        assert!(matches!(err, WriteError::DuplicatePage(0)));
    }

    #[test]
    fn text_page_embeds_font_and_pdfa_metadata() {
        let mut w = DocumentWriter::with_profile(Vec::new(), 1, PdfProfile::PdfA1b).unwrap();
        w.set_embedded_font(sample_font());
        let art = PdfPageArtifact {
            index: 0,
            media_box: PdfRect::from_size(612.0, 792.0),
            elements: Box::new([]),
            fills: Box::new([]),
            text_layer: Some(PreparedTextLayer {
                runs: Box::new([TextRun {
                    text: "hello".to_string(),
                    x: 72.0,
                    y: 700.0,
                    size: 10.0,
                }]),
                font: TextFont::Embedded,
            }),
            glyph_layer: None,
            rotation: PageRotation::Upright,
        };
        w.add_page(&art).unwrap();
        assert!(w.saw_text());
        let bytes = w.finalize().unwrap();
        let t = String::from_utf8_lossy(&bytes).into_owned();

        assert!(t.starts_with("%PDF-1.4"), "PDF/A pins 1.4");
        assert!(t.contains("/Subtype /Type0"));
        assert!(t.contains("/F0 "), "page references the embedded font");
        assert!(t.contains("/FlateDecode"), "text content compressed");
        assert!(t.contains("/Type /OutputIntent"));
        assert!(t.contains("/MarkInfo <</Marked true>>"));
        assert!(t.contains("/Producer (Lege PDF Generator)"));
        assert!(t.contains("/Info "), "trailer references Info");
    }

    #[test]
    fn bookmarks_produce_outline() {
        let mut w = DocumentWriter::new(Vec::new(), 2).unwrap();
        w.set_bookmarks(vec![OutlineItem {
            title: "Cover".to_string(),
            page_index: 0,
            top: None,
            children: vec![],
        }]);
        w.add_page(&jpeg_page(0)).unwrap();
        w.add_page(&jpeg_page(1)).unwrap();
        let bytes = w.finalize().unwrap();
        let t = String::from_utf8_lossy(&bytes).into_owned();
        assert!(t.contains("/Type /Outlines"));
        assert!(t.contains("(Cover)"));
        assert!(t.contains("/Outlines "), "catalog links the outline");
    }
}
