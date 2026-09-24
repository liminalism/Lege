//! Layout a [`Book`](docwrite_model::Book) with its masters, styles, and notes.

use docwrite_model::{BlockKind, Book, ChapterStart, NoteKind};

use crate::engine::{
    Alignment, Document, EditReport, Face, Geometry, LayoutHints, PageRules, Paragraph,
    ParagraphStyle, StyledRun,
};
use crate::error::TypesetError;

/// The Notes section's paragraphs: its heading is this id, and each endnote
/// is this bit with the note's id. Block ids never set it.
pub const ENDNOTES_ID: u64 = 1 << 62;

/// Paragraph ids with this bit set are chapter titles set from chapter
/// metadata; the rest of the id is the chapter's id. Block ids never set it.
pub const CHAPTER_TITLE_ID: u64 = 1 << 63;

/// Paginate `book` with the chapter template's page master and paragraph styles.
pub fn from_book(book: &Book, face: Face) -> Result<Document, TypesetError> {
    from_book_with(book, vec![face])
}

/// Paginate `book` with a font family: regular, bold, italic and bold
/// italic, in that order. Styles the family lacks are set in regular.
pub fn from_book_with(book: &Book, faces: Vec<Face>) -> Result<Document, TypesetError> {
    let (geometry, paragraphs, hints) = layout_inputs(book);
    Document::new_with(faces, geometry, paragraphs, hints)
}

/// Bring `document` up to date with `book` after an edit.
///
/// Only paragraphs whose text, style or note changed are shaped again, and
/// pagination stops once the page breaks line up with the previous layout.
/// A change of page master or body size lays out the whole book.
pub fn update_from_book(document: &mut Document, book: &Book) -> Result<EditReport, TypesetError> {
    let (geometry, paragraphs, hints) = layout_inputs(book);
    document.apply(geometry, paragraphs, hints)
}

fn layout_inputs(book: &Book) -> (Geometry, Vec<Paragraph>, LayoutHints) {
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
    // One set of page rules per chapter template: its master's margins and
    // furniture inside the book's trim, and its opener-folio choice.
    let masters: Vec<PageRules> = book
        .chapter_templates()
        .iter()
        .map(|template| {
            let own = book
                .page_masters()
                .iter()
                .find(|master| master.name == template.master)
                .or(master);
            PageRules {
                margin_top: own.map_or(geometry.margin_top, |m| m.margin_top),
                margin_bottom: own.map_or(geometry.margin_bottom, |m| m.margin_bottom),
                margin_inner: own.map_or(geometry.margin_inner, |m| m.margin_inner),
                margin_outer: own.map_or(geometry.margin_outer, |m| m.margin_outer),
                folio: own.is_some_and(|m| m.folio),
                running_head: own.is_some_and(|m| m.running_head),
                hide_opener_folio: !template.show_opener_folio,
            }
        })
        .collect();
    let default_master = book
        .chapter_templates()
        .iter()
        .position(|candidate| template.is_some_and(|template| template.name == candidate.name))
        .unwrap_or(0);
    let mut paragraphs = Vec::new();
    let mut recto_at = Vec::new();
    let mut break_at = Vec::new();
    let mut master_at = Vec::new();
    let mut heads = Vec::new();
    let mut footnote_count = 0usize;
    let mut endnotes: Vec<(u64, String)> = Vec::new();
    for part in book.parts() {
        for chapter in part.chapters() {
            let template = book
                .chapter_templates()
                .iter()
                .find(|template| template.name == chapter.template());
            let recto = template.is_some_and(|template| template.start == ChapterStart::NextRecto);
            let master_index = book
                .chapter_templates()
                .iter()
                .position(|candidate| candidate.name == chapter.template())
                .unwrap_or(default_master);
            let head = chapter.title().to_string();
            let chapter_start = paragraphs.len();
            let mut first_body = true;
            let titled = chapter
                .sections()
                .iter()
                .flat_map(|section| section.blocks())
                .next()
                .is_some_and(|block| matches!(block.kind(), BlockKind::ChapterTitle));
            if !titled && !head.is_empty() {
                // The chapter's title is metadata, not a block: set it as the
                // opener's first paragraph in the template's title style.
                let title_style = template
                    .map(|template| template.title_style.as_str())
                    .unwrap_or("Chapter Title");
                let mut style = map_style(&named_style(book, title_style));
                style.name = "Chapter Title".into();
                style.keep_with_next = true;
                style.space_after = style.space_after.max(style.leading);
                paragraphs.push(Paragraph {
                    id: CHAPTER_TITLE_ID | chapter.id().raw(),
                    text: head.clone(),
                    style,
                    note: None,
                    note_is_endnote: false,
                    note_mark: String::new(),
                    runs: Vec::new(),
                });
                recto_at.push(false);
                break_at.push(false);
                master_at.push(master_index);
                heads.push(head.clone());
            }
            for section in chapter.sections() {
                for block in section.blocks() {
                    let style_name = match block.kind() {
                        BlockKind::ChapterTitle => template
                            .map(|template| template.title_style.as_str())
                            .unwrap_or("Chapter Title"),
                        BlockKind::Image { .. } => "Image",
                        BlockKind::Caption => "Caption",
                        BlockKind::Subhead => "Subhead",
                        BlockKind::BlockQuote => "Block Quote",
                        BlockKind::Epigraph => "Epigraph",
                        BlockKind::Verse => "Verse",
                        BlockKind::SceneBreak => "Scene Break",
                        BlockKind::BibliographyEntry => "Bibliography Entry",
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
                    // Footnotes are numbered through the book and set at the
                    // foot of their page; endnotes are numbered separately and
                    // set in the Notes section after the last chapter.
                    let mut note = None;
                    let mut note_mark = String::new();
                    if let Some(id) = block.note()
                        && let Ok(body) = book.note(id)
                    {
                        if body.kind() == NoteKind::Endnote {
                            endnotes.push((id.raw(), body.text()));
                            note_mark = endnotes.len().to_string();
                        } else {
                            footnote_count += 1;
                            note_mark = footnote_count.to_string();
                            note = Some(format!("{footnote_count}\u{2002}{}", body.text()));
                        }
                    }
                    let mut text = block.text();
                    if matches!(block.kind(), BlockKind::SceneBreak) && text.trim().is_empty() {
                        // An empty scene break shows as a centered ornament.
                        text = "*\u{2003}*\u{2003}*".into();
                    }
                    let model_style = named_style(book, style_name);
                    let runs =
                        styled_runs(&text, block.runs(), model_style.bold, model_style.italic);
                    paragraphs.push(Paragraph {
                        id: block.id().raw(),
                        text,
                        style,
                        note,
                        note_is_endnote: false,
                        note_mark,
                        runs,
                    });
                    recto_at.push(false);
                    break_at.push(false);
                    master_at.push(master_index);
                    heads.push(head.clone());
                }
            }
            // Every chapter opens a page; NextRecto also skips to a recto.
            if chapter_start > 0
                && let Some(flag) = break_at.get_mut(chapter_start)
            {
                *flag = true;
            }
            if recto && let Some(flag) = recto_at.get_mut(chapter_start) {
                *flag = true;
            }
        }
    }
    if !endnotes.is_empty() {
        // The Notes section: a heading that opens a page, then each endnote
        // as a paragraph, numbered as its reference mark is.
        let mut title = map_style(&named_style(book, "Chapter Title"));
        title.name = "Chapter Title".into();
        title.keep_with_next = true;
        title.space_after = title.space_after.max(title.leading);
        let notes_master = master_at.last().copied().unwrap_or(default_master);
        paragraphs.push(Paragraph {
            id: ENDNOTES_ID,
            text: "Notes".into(),
            style: title,
            ..Paragraph::default()
        });
        recto_at.push(false);
        break_at.push(true);
        master_at.push(notes_master);
        heads.push("Notes".into());
        let mut entry = map_style(&body);
        entry.first_indent = 0.0;
        entry.hyphenate = false;
        for (number, (id, text)) in endnotes.iter().enumerate() {
            paragraphs.push(Paragraph {
                id: ENDNOTES_ID | id,
                text: format!("{}. {text}", number + 1),
                style: entry.clone(),
                ..Paragraph::default()
            });
            recto_at.push(false);
            break_at.push(false);
            master_at.push(notes_master);
            heads.push("Notes".into());
        }
    }
    if paragraphs.is_empty() {
        paragraphs.push(Paragraph {
            id: u64::MAX,
            text: String::new(),
            style: map_style(&body),
            ..Paragraph::default()
        });
        recto_at.push(false);
        break_at.push(false);
        master_at.push(default_master);
        heads.push(String::new());
    }
    let hints = LayoutHints {
        recto_at,
        break_at,
        masters,
        master_at,
        heads,
        folio: master.is_some_and(|master| master.folio),
        hide_opener_folio: template.is_some_and(|template| !template.show_opener_folio),
        running_head: master.is_some_and(|master| master.running_head),
        facing: master.is_some_and(|master| master.facing),
    };
    (geometry, paragraphs, hints)
}

/// The model's runs (character ranges) as byte ranges over `text`, keeping
/// only runs that change how text is set.
fn styled_runs(
    text: &str,
    runs: &[docwrite_model::Run],
    style_bold: bool,
    style_italic: bool,
) -> Vec<StyledRun> {
    if style_bold || style_italic {
        // A bold or italic style sets every run so; italic runs inside an
        // italic style turn roman, for emphasis within an epigraph.
        let mut spans = styled_runs(text, runs, false, false);
        if spans.is_empty() && !text.is_empty() {
            spans.push(StyledRun {
                start: 0,
                end: text.len(),
                ..StyledRun::default()
            });
        }
        let mut covered = 0;
        let mut out = Vec::new();
        for span in spans {
            if span.start > covered {
                out.push(StyledRun {
                    start: covered,
                    end: span.start,
                    ..StyledRun::default()
                });
            }
            covered = covered.max(span.end);
            out.push(span);
        }
        if covered < text.len() {
            out.push(StyledRun {
                start: covered,
                end: text.len(),
                ..StyledRun::default()
            });
        }
        for span in &mut out {
            span.bold |= style_bold;
            span.italic ^= style_italic;
        }
        return out;
    }
    if runs.iter().all(|run| {
        !run.marks.bold
            && !run.marks.italic
            && !run.marks.small_caps
            && run.marks.script == docwrite_model::Script::Normal
    }) {
        return Vec::new();
    }
    let mut bytes: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
    bytes.push(text.len());
    let at = |chars: usize| bytes.get(chars).copied().unwrap_or(text.len());
    runs.iter()
        .filter(|run| run.start < run.end)
        .map(|run| StyledRun {
            start: at(run.start),
            end: at(run.end),
            bold: run.marks.bold,
            italic: run.marks.italic,
            small_caps: run.marks.small_caps,
            script: match run.marks.script {
                docwrite_model::Script::Super => 1,
                docwrite_model::Script::Sub => -1,
                docwrite_model::Script::Normal => 0,
            },
        })
        .collect()
}

fn named_style(book: &Book, name: &str) -> docwrite_model::ParagraphStyle {
    book.paragraph_styles()
        .iter()
        .find(|style| style.name == name)
        .cloned()
        .unwrap_or_else(|| docwrite_model::ParagraphStyle {
            name: name.into(),
            first_indent_pt: 0.0,
            hyphenate: false,
            ..docwrite_model::ParagraphStyle::default()
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
        align: match style.align {
            docwrite_model::Align::Left => Alignment::Left,
            docwrite_model::Align::Justify => Alignment::Justify,
            docwrite_model::Align::Center => Alignment::Center,
            docwrite_model::Align::Right => Alignment::Right,
        },
        left_indent: style.left_indent_pt,
        right_indent: style.right_indent_pt,
        space_before: style.space_before_pt,
        space_after: style.space_after_pt,
        ..ParagraphStyle::default()
    }
}
