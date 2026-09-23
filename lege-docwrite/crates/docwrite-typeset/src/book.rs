//! Layout a [`Book`](docwrite_model::Book) with its masters, styles, and notes.

use docwrite_model::{BlockKind, Book, ChapterStart, NoteKind};

use crate::engine::{Document, Face, Geometry, LayoutHints, Paragraph, ParagraphStyle};
use crate::error::TypesetError;

/// Paginate `book` with the chapter template's page master and paragraph styles.
pub fn from_book(book: &Book, face: Face) -> Result<Document, TypesetError> {
    let template = book
        .chapter_templates()
        .iter()
        .find(|template| template.name == "Chapter")
        .or_else(|| book.chapter_templates().first());
    let master_name = template
        .map(|template| template.master.as_str())
        .unwrap_or("Right Body");
    let master = book
        .page_masters()
        .iter()
        .find(|master| master.name == master_name)
        .or_else(|| book.page_masters().first());
    let body = named_style(
        book,
        template
            .map(|template| template.body_style.as_str())
            .unwrap_or("Body"),
    );
    let geometry = match master {
        Some(master) => Geometry {
            page_width: master.width_pt,
            page_height: master.height_pt,
            margin_top: master.margin_top,
            margin_bottom: master.margin_bottom,
            margin_inner: master.margin_inner,
            margin_outer: master.margin_outer,
            font_size: body.size_pt,
            leading: body.leading_pt,
        },
        None => Geometry::one_line_pages(),
    };
    let mut paragraphs = Vec::new();
    let mut recto_at = Vec::new();
    let mut heads = Vec::new();
    for part in book.parts() {
        for chapter in part.chapters() {
            let template = book
                .chapter_templates()
                .iter()
                .find(|template| template.name == chapter.template());
            let recto = template.is_some_and(|template| template.start == ChapterStart::NextRecto);
            let head = chapter.title().to_string();
            let chapter_start = paragraphs.len();
            let mut first_body = true;
            for section in chapter.sections() {
                for block in section.blocks() {
                    let style_name = match block.kind() {
                        BlockKind::ChapterTitle => template
                            .map(|template| template.title_style.as_str())
                            .unwrap_or("Chapter Title"),
                        BlockKind::Image { .. } => "Image",
                        BlockKind::Caption => "Caption",
                        _ if first_body => {
                            first_body = false;
                            template
                                .map(|template| template.first_paragraph_style.as_str())
                                .unwrap_or("First Paragraph")
                        }
                        _ => template
                            .map(|template| template.body_style.as_str())
                            .unwrap_or("Body"),
                    };
                    let mut style = map_style(&named_style(book, style_name));
                    if matches!(block.kind(), BlockKind::Image { .. }) {
                        style.name = "Image".into();
                    }
                    if matches!(block.kind(), BlockKind::Caption) {
                        style.name = "Caption".into();
                    }
                    let mut note = None;
                    let mut note_is_endnote = false;
                    if let Some(id) = block.note() {
                        if let Ok(body) = book.note(id) {
                            note_is_endnote = body.kind() == NoteKind::Endnote;
                            note = Some(body.text());
                        }
                    }
                    paragraphs.push(Paragraph {
                        id: paragraphs.len() as u64 + 1,
                        text: block.text(),
                        style,
                        note,
                        note_is_endnote,
                    });
                    recto_at.push(false);
                    heads.push(head.clone());
                }
            }
            if recto {
                if let Some(flag) = recto_at.get_mut(chapter_start) {
                    *flag = true;
                }
            }
        }
    }
    if paragraphs.is_empty() {
        paragraphs.push(Paragraph {
            id: 1,
            text: String::new(),
            style: map_style(&body),
            note: None,
            note_is_endnote: false,
        });
        recto_at.push(false);
        heads.push(String::new());
    }
    let hints = LayoutHints {
        recto_at,
        heads,
        folio: master.is_some_and(|master| master.folio),
        hide_opener_folio: template.is_some_and(|template| !template.show_opener_folio),
        running_head: master.is_some_and(|master| master.running_head),
    };
    Document::new_with(face, geometry, paragraphs, hints)
}

fn named_style(book: &Book, name: &str) -> docwrite_model::ParagraphStyle {
    book.paragraph_styles()
        .iter()
        .find(|style| style.name == name)
        .cloned()
        .unwrap_or_else(|| docwrite_model::ParagraphStyle {
            name: name.into(),
            size_pt: 12.0,
            leading_pt: 16.0,
            first_indent_pt: 0.0,
            hyphenate: false,
            small_caps: false,
            oldstyle_figures: false,
            drop_cap_lines: 0,
        })
}

fn map_style(style: &docwrite_model::ParagraphStyle) -> ParagraphStyle {
    ParagraphStyle {
        hyphenate: style.hyphenate,
        drop_cap_lines: style.drop_cap_lines,
        small_caps: style.small_caps,
        oldstyle_figures: style.oldstyle_figures,
        font_size: style.size_pt,
        leading: style.leading_pt,
        first_indent: style.first_indent_pt,
        name: style.name.clone(),
        widow_orphan: true,
        ..ParagraphStyle::default()
    }
}
