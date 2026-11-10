//! Title and author from the layout of one page's OCR lines.
//!
//! The rule of thumb: a cover or title page sets the title in the largest type,
//! usually in the top half, and the author's name in smaller type, usually in
//! the bottom half (or right under the title on a paper's first page). Layout
//! detection's `doc_title` label, when it fires, is a strong extra vote.
//!
//! Everything here is pure: coordinates are raster pixels with the origin at the
//! top left and y growing downwards, exactly as the OCR pipeline reports them.
//!
//! The text filters (`normalize_text`, `author_name_like`, `strip_author_cue`,
//! the chapter-word list) are ported from `lege-process/core/toc.rs`, which
//! infers the same metadata for converted books; they are private there and the
//! `lege` crate is far too heavy a dependency for this tool.

use std::fmt::Write as _;

/// Layout role of the region a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Title,
    Heading,
    Header,
    Footer,
    Other,
}

/// One recognised text line.
#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    /// `[left, top, right, bottom]` in page pixels.
    pub bbox: [f32; 4],
    pub confidence: Option<f32>,
    pub kind: LineKind,
    /// Layout detector score of the region this line belongs to.
    pub layout_confidence: Option<f32>,
    /// Index of the layout region this line was grouped into, if any. Lines
    /// of one `doc_title` region are one title however far apart they sit.
    pub region: Option<usize>,
}

impl Line {
    fn height(&self) -> f32 {
        (self.bbox[3] - self.bbox[1]).max(0.0)
    }
}

/// The lines of one page plus its size in the same pixel space.
#[derive(Debug, Clone)]
pub struct PageLines {
    pub width: f32,
    pub height: f32,
    pub lines: Vec<Line>,
}

/// A guessed value and why it was chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub value: String,
    pub reason: String,
}

/// What the heuristics concluded about one page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Guess {
    pub title: Option<Field>,
    pub author: Option<Field>,
    /// Why a field is missing, for `--verbose`.
    pub notes: Vec<String>,
}

/// Lines below this recognition confidence are noise.
const MIN_LINE_CONFIDENCE: f32 = 0.5;
/// A title must be set at least this much larger than the page's typical line.
const MIN_TITLE_SIZE_RATIO: f32 = 1.35;
/// An author line may be at most this tall relative to the title.
const MAX_AUTHOR_TO_TITLE_RATIO: f32 = 0.9;
/// Two candidates scoring closer than this are a tie.
const TIE_MARGIN: f32 = 0.25;
const MAX_TITLE_CHARS: usize = 200;
/// A page with at most this many lines is a sparse cover.
const SPARSE_PAGE_LINES: usize = 4;
/// Body text height as a share of the page height, for sparse covers.
const SPARSE_BODY_FRACTION: f32 = 0.012;
/// For the person-name swap, the other title must be at least this large
/// relative to the name.
const MIN_SWAP_SIZE_RATIO: f32 = 0.75;
const MAX_TITLE_WORDS: usize = 30;

pub fn guess(page: &PageLines) -> Guess {
    let mut guess = Guess::default();
    let lines = clean_lines(&page.lines);
    if lines.is_empty() {
        guess
            .notes
            .push("no usable text lines on page 1".to_string());
        return guess;
    }
    let page_height = page.height.max(1.0);
    let mut body_height = lower_quartile(lines.iter().map(Line::height).collect());
    // A sparse cover has no body text to measure against; there "large" means
    // large for the page: body type is roughly 1.2% of the page height.
    if lines.len() <= SPARSE_PAGE_LINES {
        body_height = body_height.min(page_height * SPARSE_BODY_FRACTION);
    }
    if body_height <= 0.0 {
        guess.notes.push("text lines have no height".to_string());
        return guess;
    }
    let blocks = group_blocks(&lines, body_height);

    let title = pick_title(&blocks, body_height, page_height, &mut guess.notes);
    let (title_index, swapped_author) = match title {
        Some(choice) => (Some(choice.index), choice.swapped_author),
        None => (None, None),
    };
    if let Some(index) = title_index {
        let block = &blocks[index];
        guess.title = Some(Field {
            value: truncate(&block.text, MAX_TITLE_CHARS),
            reason: title_reason(block, body_height, page_height),
        });
    }

    guess.author = explicit_author(&lines, &mut guess.notes)
        .or(swapped_author)
        .or_else(|| {
            title_index.and_then(|index| {
                implicit_author(&blocks, index, body_height, page_height, &mut guess.notes)
            })
        });
    if title_index.is_none() && guess.author.is_none() {
        guess
            .notes
            .push("no title to anchor an implicit author search".to_string());
    }
    guess
}

// ── cleaning and grouping ─────────────────────────────────────────────────────

fn clean_lines(lines: &[Line]) -> Vec<Line> {
    lines
        .iter()
        .filter(|line| line.height() > 0.0 && line.bbox[2] > line.bbox[0])
        .filter(|line| {
            line.confidence
                .is_none_or(|confidence| confidence >= MIN_LINE_CONFIDENCE)
        })
        .filter_map(|line| {
            let text = normalize_text(&line.text);
            let alphanumeric = text.chars().filter(|ch| ch.is_alphanumeric()).count();
            // A lone glyph, punctuation or a roman folio is never identity. A
            // bare number stays: "1984" can be a title, and a small folio
            // never qualifies as one.
            if alphanumeric < 2 || is_roman_numeral(&text) {
                return None;
            }
            Some(Line {
                text,
                ..line.clone()
            })
        })
        .collect()
}

/// A run of lines set in the same size that reads as one unit (a title broken
/// over two lines, a pair of author names).
#[derive(Debug, Clone)]
struct Block {
    lines: Vec<Line>,
    bbox: [f32; 4],
    /// Mean line height.
    height: f32,
    text: String,
    confidence: Option<f32>,
    kind: LineKind,
    layout_confidence: Option<f32>,
}

impl Block {
    fn new(line: Line) -> Self {
        let mut block = Self {
            bbox: line.bbox,
            height: line.height(),
            text: String::new(),
            confidence: line.confidence,
            kind: line.kind,
            layout_confidence: line.layout_confidence,
            lines: vec![line],
        };
        block.refresh();
        block
    }

    fn tallest(&self) -> f32 {
        self.lines.iter().map(Line::height).fold(0.0, f32::max)
    }

    fn center_y(&self) -> f32 {
        (self.bbox[1] + self.bbox[3]) * 0.5
    }

    fn shares_region(&self, line: &Line) -> bool {
        line.region.is_some() && self.lines.iter().any(|other| other.region == line.region)
    }

    fn accepts(&self, line: &Line, body_height: f32) -> bool {
        // The layout model already said these lines are one title.
        if line.kind == LineKind::Title && self.shares_region(line) {
            return true;
        }
        let height = line.height();
        let ratio = height / self.height.max(1.0);
        // Display type is often set in mixed sizes ("1933: / ROOSEVELT'S /
        // the / GOLD / Standard"), and a title the detector boxed line by
        // line arrives as several doc_title regions; both tolerate more size
        // variation than running text.
        let both_titles = line.kind == LineKind::Title && self.kind == LineKind::Title;
        let display = self.tallest() >= body_height * MIN_TITLE_SIZE_RATIO;
        let sizes = if display || both_titles {
            0.65..=1.54
        } else {
            0.75..=1.33
        };
        if !sizes.contains(&ratio) {
            return false;
        }
        let reference = height.max(self.height);
        let last = self
            .lines
            .iter()
            .map(|line| line.bbox[3])
            .fold(0.0, f32::max);
        let horizontal_overlap = line.bbox[2].min(self.bbox[2]) - line.bbox[0].max(self.bbox[0]);
        let gap = line.bbox[1] - last;
        // The next line of the same paragraph: just below, sharing columns.
        // Within one layout region looser leading still reads as one unit.
        let max_gap = if self.shares_region(line) || both_titles {
            1.5
        } else {
            0.8
        };
        let stacked =
            gap <= reference * max_gap && gap >= -reference * 0.5 && horizontal_overlap > 0.0;
        // Text the recogniser split on one row.
        let vertical_overlap = line.bbox[3].min(self.bbox[3]) - line.bbox[1].max(self.bbox[1]);
        let horizontal_gap = (line.bbox[0] - self.bbox[2]).max(self.bbox[0] - line.bbox[2]);
        let same_row =
            vertical_overlap >= height.min(self.height) * 0.5 && horizontal_gap <= reference * 1.5;
        stacked || same_row
    }

    fn push(&mut self, line: Line) {
        self.lines.push(line);
        self.refresh();
    }

    fn refresh(&mut self) {
        let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for line in &self.lines {
            bbox[0] = bbox[0].min(line.bbox[0]);
            bbox[1] = bbox[1].min(line.bbox[1]);
            bbox[2] = bbox[2].max(line.bbox[2]);
            bbox[3] = bbox[3].max(line.bbox[3]);
        }
        self.bbox = bbox;
        self.height =
            self.lines.iter().map(Line::height).sum::<f32>() / self.lines.len().max(1) as f32;
        self.confidence = self
            .lines
            .iter()
            .filter_map(|line| line.confidence)
            .reduce(f32::min);
        self.kind = self
            .lines
            .iter()
            .map(|line| line.kind)
            .min_by_key(|kind| kind_rank(*kind))
            .unwrap_or(LineKind::Other);
        self.layout_confidence = self
            .lines
            .iter()
            .filter(|line| line.kind == self.kind)
            .filter_map(|line| line.layout_confidence)
            .reduce(f32::max);
        self.text = join_rows(&self.rows());
    }

    /// Lines in reading order, grouped into rows.
    fn rows(&self) -> Vec<Vec<&Line>> {
        let mut ordered: Vec<&Line> = self.lines.iter().collect();
        ordered.sort_by(|a, b| a.bbox[1].total_cmp(&b.bbox[1]));
        let mut rows: Vec<Vec<&Line>> = Vec::new();
        for line in ordered {
            let center = (line.bbox[1] + line.bbox[3]) * 0.5;
            match rows.last_mut() {
                Some(row)
                    if row.first().is_some_and(|first| {
                        center <= first.bbox[3] && center >= first.bbox[1]
                    }) =>
                {
                    row.push(line);
                }
                _ => rows.push(vec![line]),
            }
        }
        for row in &mut rows {
            row.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
        }
        rows
    }

    /// Each row as its own string, for author blocks that list one name per row.
    fn row_texts(&self) -> Vec<String> {
        self.rows()
            .iter()
            .map(|row| {
                row.iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }
}

fn kind_rank(kind: LineKind) -> u8 {
    match kind {
        LineKind::Title => 0,
        LineKind::Heading => 1,
        LineKind::Other => 2,
        LineKind::Header => 3,
        LineKind::Footer => 4,
    }
}

/// Join rows with spaces, mending a word hyphenated across a line break.
fn join_rows(rows: &[Vec<&Line>]) -> String {
    let mut text = String::new();
    for row in rows {
        let row_text = row
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            text = row_text;
            continue;
        }
        let continues_word = text.ends_with('-')
            && row_text.chars().next().is_some_and(char::is_lowercase)
            && text.chars().rev().nth(1).is_some_and(char::is_alphabetic);
        if continues_word {
            text.pop();
        } else {
            text.push(' ');
        }
        text.push_str(&row_text);
    }
    text
}

fn group_blocks(lines: &[Line], body_height: f32) -> Vec<Block> {
    let mut ordered: Vec<&Line> = lines.iter().collect();
    ordered.sort_by(|a, b| {
        a.bbox[1]
            .total_cmp(&b.bbox[1])
            .then(a.bbox[0].total_cmp(&b.bbox[0]))
    });
    let mut blocks: Vec<Block> = Vec::new();
    for line in ordered {
        // Newest first: a cover's blocks rarely interleave, but a two-column
        // first page does.
        match blocks
            .iter_mut()
            .rev()
            .find(|block| block.accepts(line, body_height))
        {
            Some(block) => block.push(line.clone()),
            None => blocks.push(Block::new(line.clone())),
        }
    }
    blocks
}

// ── title ─────────────────────────────────────────────────────────────────────

struct TitleChoice {
    index: usize,
    /// When the largest text was a person's name, that name is the author.
    swapped_author: Option<Field>,
}

fn title_score(block: &Block, body_height: f32, page_height: f32) -> f32 {
    let size = (block.height / body_height).min(4.0);
    // "Large text in the top half" is the rule; a big logo or a stamp at the
    // bottom must out-size a top-half title clearly to beat it.
    let top_half = if block.center_y() <= page_height * 0.5 {
        1.5
    } else {
        0.0
    };
    let layout = match block.kind {
        LineKind::Title => block.layout_confidence.unwrap_or(0.8),
        LineKind::Heading => 0.3,
        // Running heads, form numbers, printer's lines.
        LineKind::Header | LineKind::Footer => -0.5,
        LineKind::Other => 0.0,
    };
    // A three-letter logo is less of a title than a phrase.
    let words = block
        .text
        .split_whitespace()
        .count()
        .saturating_sub(1)
        .min(4) as f32
        * 0.1;
    size + top_half + layout + words
}

fn title_qualifies(block: &Block, body_height: f32) -> bool {
    (block.tallest() >= body_height * MIN_TITLE_SIZE_RATIO || block.kind == LineKind::Title)
        // A bare number ("20663", a stock code) is a title only if the layout
        // model says so ("1984").
        && (block.kind == LineKind::Title || block.text.chars().any(char::is_alphabetic))
        && strip_author_cue(&block.text).is_none()
        && credible_title(&block.text)
        && wordlike(&block.text)
        && !imprint_like(&block.text)
}

/// Most tokens are words or plain numbers, not shelf marks or stamps such as
/// "LIBRAR349694", "qW2f" or "10.21-100" that OCR reads off a library copy.
fn wordlike(text: &str) -> bool {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    let good = tokens
        .iter()
        .filter(|token| {
            let core = token.trim_matches(|ch: char| !ch.is_alphanumeric());
            let letters = core.chars().filter(|ch| ch.is_alphabetic()).count();
            let digits = core.chars().filter(|ch| ch.is_ascii_digit()).count();
            // A word ("ROOSEVELT'S", "f/2.3" counts as a mixed token but is
            // rare), or a plain number ("1933", "101").
            (letters > 0 && digits == 0) || (digits > 0 && letters == 0 && core.len() <= 4)
        })
        .count();
    good * 10 >= tokens.len() * 6
}

fn pick_title(
    blocks: &[Block],
    body_height: f32,
    page_height: f32,
    notes: &mut Vec<String>,
) -> Option<TitleChoice> {
    let mut ranked: Vec<(f32, usize)> = blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| title_qualifies(block, body_height))
        .map(|(index, block)| (title_score(block, body_height, page_height), index))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let Some(&(best_score, best)) = ranked.first() else {
        notes.push(format!(
            "no title: nothing set at least {MIN_TITLE_SIZE_RATIO}x the typical line height \
             ({body_height:.0}px) and no doc_title region"
        ));
        return None;
    };

    // The author's name is sometimes the largest thing on a cover. If the
    // winner reads as a person's name and another large, non-name block exists,
    // that one is the title and the name is the author.
    if person_name_like(&blocks[best].text) {
        let alternative = ranked
            .iter()
            .skip(1)
            .find(|(_, index)| {
                let block = &blocks[*index];
                block.height >= blocks[best].height * MIN_SWAP_SIZE_RATIO
                    && !person_name_like(&block.text)
            })
            .map(|(_, index)| *index);
        if let Some(index) = alternative {
            return Some(TitleChoice {
                index,
                swapped_author: Some(Field {
                    value: author_text(&blocks[best]),
                    reason: "the largest text reads as a person's name".to_string(),
                }),
            });
        }
    }

    if let Some(&(runner_score, runner)) = ranked.get(1)
        && best_score - runner_score < TIE_MARGIN
        && blocks[best].text.to_lowercase() != blocks[runner].text.to_lowercase()
    {
        let (a, b) = (blocks[best].height, blocks[runner].height);
        if (a - b).abs() <= a.max(b) * 0.1 {
            notes.push(format!(
                "no title: \"{}\" and \"{}\" are equally plausible",
                blocks[best].text, blocks[runner].text
            ));
            return None;
        }
        // Close call: the top-half block wins, then the larger one.
        let in_top = |index: usize| blocks[index].center_y() <= page_height * 0.5;
        let index = match (in_top(best), in_top(runner)) {
            (true, false) => best,
            (false, true) => runner,
            _ if a >= b => best,
            _ => runner,
        };
        return Some(TitleChoice {
            index,
            swapped_author: None,
        });
    }
    Some(TitleChoice {
        index: best,
        swapped_author: None,
    })
}

fn title_reason(block: &Block, body_height: f32, page_height: f32) -> String {
    let mut reason = format!(
        "{:.1}x typical line height, {} of page",
        block.height / body_height,
        if block.center_y() <= page_height * 0.5 {
            "top half"
        } else {
            "bottom half"
        }
    );
    if block.kind == LineKind::Title {
        let _ = write!(
            reason,
            ", layout doc_title ({:.2})",
            block.layout_confidence.unwrap_or(0.0)
        );
    }
    reason
}

fn credible_title(text: &str) -> bool {
    let lower = text.to_lowercase();
    let alphanumeric = text.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric >= 2
        && text.chars().count() <= 400
        && text.split_whitespace().count() <= MAX_TITLE_WORDS
        && !GENERIC_HEADINGS.contains(&lower.trim_end_matches([':', '.']))
        && !has_chapter_pattern(text)
}

/// Section headings that are set large on many first pages but never name the
/// document.
const GENERIC_HEADINGS: &[&str] = &[
    "document",
    "untitled",
    "unknown",
    "contents",
    "table of contents",
    "abstract",
    "summary",
    "introduction",
    "preface",
    "foreword",
    "acknowledgements",
    "acknowledgments",
    "index",
    "references",
    "bibliography",
    "appendix",
    "ingredients",
    "directions",
    "instructions",
    "preparation",
    "notes",
    "glossary",
    "dedication",
    "prologue",
    "epilogue",
];

// ── author ────────────────────────────────────────────────────────────────────

/// "by Jane Doe", or "by" alone above the name.
fn explicit_author(lines: &[Line], notes: &mut Vec<String>) -> Option<Field> {
    let mut ordered: Vec<&Line> = lines.iter().collect();
    ordered.sort_by(|a, b| a.bbox[1].total_cmp(&b.bbox[1]));
    let mut found: Vec<String> = Vec::new();
    for (index, line) in ordered.iter().enumerate() {
        let Some(after) = strip_author_cue(&line.text) else {
            continue;
        };
        if !after.is_empty() {
            if author_name_like(after) {
                found.push(normalize_text(after));
            }
        } else if let Some(next) = ordered.get(index + 1)
            && author_name_like(&next.text)
            && !publisher_like(&next.text)
        {
            found.push(normalize_text(&next.text));
        }
    }
    found.sort();
    found.dedup();
    match found.len() {
        0 => None,
        1 => found.pop().map(|value| Field {
            value,
            reason: "follows an explicit \"by\"".to_string(),
        }),
        _ => {
            notes.push(format!(
                "several \"by\" lines disagree: {}",
                found.join(" / ")
            ));
            None
        }
    }
}

fn implicit_author(
    blocks: &[Block],
    title_index: usize,
    body_height: f32,
    page_height: f32,
    notes: &mut Vec<String>,
) -> Option<Field> {
    let title = &blocks[title_index];
    let mut ranked: Vec<(f32, usize, &'static str)> = blocks
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != title_index)
        .filter(|(_, block)| block.height <= title.height * MAX_AUTHOR_TO_TITLE_RATIO)
        .filter(|(_, block)| block.confidence.is_none_or(|confidence| confidence >= 0.6))
        .filter(|(_, block)| !publisher_like(&block.text))
        // Without a "by" cue, only text that reads as people's names: a
        // subtitle or a slogan is name-like too, but not capitalised word by
        // word without a single "of" or "the".
        .filter(|(_, block)| block.row_texts().iter().all(|row| person_list_like(row)))
        .map(|(index, block)| {
            let bottom_half = block.center_y() > page_height * 0.5;
            let below_title = block.bbox[1] >= title.bbox[3]
                && block.bbox[1] - title.bbox[3] <= page_height * 0.18;
            let mut score = (block.height / body_height).min(2.0) * 0.5;
            if bottom_half {
                score += 1.0;
            }
            if below_title {
                score += 0.5;
            }
            if matches!(block.kind, LineKind::Header | LineKind::Footer) {
                score -= 0.5;
            }
            let place = if bottom_half {
                "bottom half"
            } else if below_title {
                "just below the title"
            } else {
                "top half"
            };
            (score, index, place)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let &(best_score, best, place) = ranked.first()?;
    if let Some(&(runner_score, runner, _)) = ranked.get(1)
        && best_score - runner_score < TIE_MARGIN * 0.8
    {
        notes.push(format!(
            "no author: \"{}\" and \"{}\" are equally plausible",
            blocks[best].text, blocks[runner].text
        ));
        return None;
    }
    let block = &blocks[best];
    Some(Field {
        value: author_text(block),
        reason: format!(
            "name-like, {:.1}x typical line height, {place}",
            block.height / body_height
        ),
    })
}

/// One name per row reads as a list: "Jane Doe, John Roe".
fn author_text(block: &Block) -> String {
    let rows = block.row_texts();
    if rows.len() > 1 && rows.iter().all(|row| author_name_like(row)) {
        rows.join(", ")
    } else {
        block.text.clone()
    }
}

// ── text filters (ported from lege-process/core/toc.rs) ──────────────────────

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            token
                .chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>()
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The typical (body) line height. The lower quartile rather than the median:
/// a sparse cover has as many display lines as body lines, and there the small
/// lines are the reference, not the large ones. On a page of running text the
/// two agree.
fn lower_quartile(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    values[(values.len() - 1) / 4]
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    match cut.rfind(' ') {
        Some(space) if space > max_chars / 2 => cut[..space].to_string(),
        _ => cut,
    }
}

const CHAPTER_WORDS: &[&str] = &[
    "chapter",
    "part",
    "section",
    "kapitel",
    "teil",
    "abschnitt",
    "chapitre",
    "partie",
    "capitolo",
    "capitulo",
    "capítulo",
    "parte",
    "hoofdstuk",
    "rozdzial",
    "rozdział",
    "глава",
    "часть",
];

/// "Chapter 4", "Part II", "§ 3". Unlike `toc.rs`, a bare leading number is
/// allowed: on a cover, "1984" or "2001: A Space Odyssey" is the title.
fn has_chapter_pattern(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut tokens = lower
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '§'));
    let Some(first) = tokens.next() else {
        return false;
    };
    if first.starts_with('§') {
        return true;
    }
    CHAPTER_WORDS.contains(&first)
        && tokens.next().is_some_and(|token| {
            token.parse::<u32>().is_ok()
                || is_roman_numeral(token)
                || matches!(
                    token,
                    "one" | "two" | "three" | "four" | "five" | "six" | "seven" | "eight"
                )
        })
}

fn is_roman_numeral(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 8
        && token
            .chars()
            .all(|ch| matches!(ch.to_ascii_lowercase(), 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'))
        // "mix", "dim" and friends are words, not numerals.
        && !matches!(token.to_ascii_lowercase().as_str(), "mix" | "dim" | "mid" | "lid" | "did" | "dll" | "civil" | "vivid" | "mild")
}

fn strip_author_cue(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    for cue in ["written by", "authored by", "edited by", "by"] {
        if lower == cue {
            return Some("");
        }
        if lower.starts_with(cue)
            && lower
                .as_bytes()
                .get(cue.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b':' | b'-'))
        {
            return Some(trimmed[cue.len()..].trim_matches([' ', ':', '-', '—']));
        }
    }
    None
}

/// Whole words that mark a publisher, imprint or rights line.
const PUBLISHER_WORDS: &[&str] = &[
    "press",
    "publisher",
    "publishers",
    "publishing",
    "publications",
    "edition",
    "editions",
    "volume",
    "vol",
    "university",
    "universität",
    "copyright",
    "isbn",
    "library",
    "books",
    "house",
    "verlag",
    "media",
    "inc",
    "ltd",
    "llc",
    "gmbh",
];

/// Fragments that mark the same anywhere in a line.
const PUBLISHER_FRAGMENTS: &[&str] = &[
    "www.",
    "http",
    ".com",
    ".org",
    "©",
    "all rights reserved",
    "printed in",
];

fn publisher_like(text: &str) -> bool {
    let lower = text.to_lowercase();
    PUBLISHER_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
        || lower
            .split_whitespace()
            .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
            .any(|token| PUBLISHER_WORDS.contains(&token))
}

/// Narrower than [`publisher_like`], for titles: "Medical Symbolism in Books"
/// or "University Physics" are titles, a rights line or a URL never is.
fn imprint_like(text: &str) -> bool {
    let lower = text.to_lowercase();
    PUBLISHER_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
        || lower
            .split_whitespace()
            .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
            .any(|token| {
                matches!(
                    token,
                    "copyright"
                        | "isbn"
                        | "publishers"
                        | "publishing"
                        | "publication"
                        | "publications"
                )
            })
}

/// One or more people: "Jane Doe", "Jane Doe and John Roe", "Doe, Jane",
/// "Ann Lee, Bo Kim & Cy Young".
fn person_list_like(text: &str) -> bool {
    let text = normalize_text(text);
    if !author_name_like(&text) {
        return false;
    }
    let parts: Vec<&str> = text
        .split([',', '&'])
        .flat_map(|part| part.split(" and "))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    // "Doe, Jane": surname first.
    if parts.len() == 2 && parts.iter().all(|part| !part.contains(' ')) {
        return person_name_like(&format!("{} {}", parts[1], parts[0]));
    }
    !parts.is_empty() && parts.iter().all(|part| person_name_like(part))
}

fn author_name_like(text: &str) -> bool {
    let text = normalize_text(text);
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() || words.len() > 8 {
        return false;
    }
    if publisher_like(&text) || text.chars().filter(|ch| ch.is_ascii_digit()).count() >= 3 {
        return false;
    }
    let alphabetic_words = words
        .iter()
        .filter(|word| word.chars().any(char::is_alphabetic))
        .count();
    alphabetic_words >= 2
        && text.chars().all(|ch| {
            ch.is_alphabetic()
                || ch.is_whitespace()
                || matches!(ch, '.' | ',' | '-' | '\'' | '’' | '&')
        })
}

/// Words that make a capitalised phrase an organisation, not a person.
const ORGANISATION_WORDS: &[&str] = &[
    "department",
    "office",
    "bureau",
    "ministry",
    "government",
    "company",
    "corporation",
    "society",
    "association",
    "institute",
    "council",
    "committee",
    "commission",
    "board",
    "agency",
    "service",
    "services",
    "power",
    "utilities",
    "plant",
    "bank",
    "museum",
    "school",
    "college",
    "church",
    "club",
    "group",
    "foundation",
    "center",
    "centre",
    "laboratory",
    "times",
    "news",
    "magazine",
    "journal",
];

const NAME_STOPWORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "of",
    "and",
    "in",
    "on",
    "to",
    "for",
    "with",
    "from",
    "at",
    "into",
    "is",
    "my",
    "our",
    "your",
    "how",
    "what",
    "why",
    "when",
    "guide",
    "introduction",
    "handbook",
    "manual",
    "story",
    "history",
    "book",
    "life",
    "new",
    "world",
];

/// Stricter than [`author_name_like`]: two to five capitalised words or
/// initials and none of the little words a title is made of. "J. R. R.
/// Tolkien" and "MARY SHELLEY" pass; "The Great Gatsby" does not.
fn person_name_like(text: &str) -> bool {
    let text = normalize_text(text);
    let words: Vec<&str> = text
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '&')
        .filter(|word| !word.is_empty())
        .collect();
    let named = words
        .iter()
        .filter(|word| word.chars().filter(|ch| ch.is_alphabetic()).count() >= 2)
        .count();
    (2..=6).contains(&words.len())
        && (1..=4).contains(&named)
        && author_name_like(&text)
        && words.iter().all(|word| {
            let bare = word.trim_matches(|ch: char| !ch.is_alphabetic());
            !bare.is_empty()
                && bare.chars().next().is_some_and(char::is_uppercase)
                && !NAME_STOPWORDS.contains(&bare.to_lowercase().as_str())
                && !ORGANISATION_WORDS.contains(&bare.to_lowercase().as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 2480.0;
    const H: f32 = 3508.0;

    fn line(text: &str, left: f32, top: f32, right: f32, height: f32) -> Line {
        Line {
            text: text.to_string(),
            bbox: [left, top, right, top + height],
            confidence: Some(0.95),
            kind: LineKind::Other,
            layout_confidence: None,
            region: None,
        }
    }

    fn page(lines: Vec<Line>) -> PageLines {
        PageLines {
            width: W,
            height: H,
            lines,
        }
    }

    fn values(guess: &Guess) -> (Option<&str>, Option<&str>) {
        (
            guess.title.as_ref().map(|field| field.value.as_str()),
            guess.author.as_ref().map(|field| field.value.as_str()),
        )
    }

    #[test]
    fn big_title_on_top_and_small_author_at_the_bottom() {
        let guess = guess(&page(vec![
            line("The Silent Orchard", 400.0, 700.0, 2080.0, 180.0),
            line("A Novel", 1000.0, 950.0, 1480.0, 70.0),
            line("Margaret Ellison", 800.0, 2900.0, 1680.0, 90.0),
            line("Harbor House", 1000.0, 3200.0, 1480.0, 50.0),
        ]));
        assert_eq!(
            values(&guess),
            (Some("The Silent Orchard"), Some("Margaret Ellison")),
            "{guess:?}"
        );
    }

    #[test]
    fn a_title_broken_over_two_lines_is_one_title() {
        let guess = guess(&page(vec![
            line("Principles of Quantum", 300.0, 600.0, 2180.0, 160.0),
            line("Navigation", 800.0, 800.0, 1680.0, 160.0),
            line("Arthur K. Bellamy", 900.0, 2800.0, 1580.0, 80.0),
            line("Second printing", 1000.0, 3300.0, 1480.0, 40.0),
        ]));
        assert_eq!(
            values(&guess),
            (
                Some("Principles of Quantum Navigation"),
                Some("Arthur K. Bellamy")
            ),
            "{guess:?}"
        );
    }

    #[test]
    fn an_explicit_by_line_wins() {
        let guess = guess(&page(vec![
            line("Winter Harvest", 400.0, 500.0, 2080.0, 170.0),
            line("by Clara Mendes", 900.0, 750.0, 1580.0, 60.0),
            line("Illustrated Edition", 900.0, 2900.0, 1580.0, 60.0),
        ]));
        assert_eq!(
            values(&guess),
            (Some("Winter Harvest"), Some("Clara Mendes")),
            "{guess:?}"
        );
    }

    #[test]
    fn a_lone_by_takes_the_next_line() {
        let guess = guess(&page(vec![
            line("Winter Harvest", 400.0, 500.0, 2080.0, 170.0),
            line("by", 1200.0, 2600.0, 1280.0, 50.0),
            line("Clara Mendes", 900.0, 2700.0, 1580.0, 70.0),
        ]));
        assert_eq!(values(&guess).1, Some("Clara Mendes"), "{guess:?}");
    }

    #[test]
    fn an_author_printed_larger_than_the_title_is_swapped() {
        let guess = guess(&page(vec![
            line("STEPHEN MARLOWE", 300.0, 400.0, 2180.0, 220.0),
            line("The Quiet Hours", 500.0, 2500.0, 1980.0, 190.0),
            line("a thriller", 1000.0, 2700.0, 1480.0, 50.0),
        ]));
        assert_eq!(
            values(&guess),
            (Some("The Quiet Hours"), Some("STEPHEN MARLOWE")),
            "{guess:?}"
        );
    }

    #[test]
    fn a_uniform_text_page_gives_no_guess() {
        let lines = (0..30)
            .map(|row| {
                line(
                    "Lorem ipsum dolor sit amet consectetur adipiscing",
                    200.0,
                    200.0 + row as f32 * 100.0,
                    2280.0,
                    50.0,
                )
            })
            .collect();
        let guess = guess(&page(lines));
        assert_eq!(values(&guess), (None, None), "{guess:?}");
    }

    #[test]
    fn publisher_and_copyright_lines_are_never_author_or_title() {
        let guess = guess(&page(vec![
            line("Coastal Birds", 400.0, 600.0, 2080.0, 180.0),
            line("Oxford University Press", 800.0, 3000.0, 1680.0, 70.0),
            line("Copyright Jane Smith", 800.0, 3150.0, 1680.0, 40.0),
        ]));
        assert_eq!(values(&guess), (Some("Coastal Birds"), None), "{guess:?}");
    }

    #[test]
    fn a_doc_title_region_counts_even_at_body_size() {
        let mut title = line("Sparse Attention Revisited", 300.0, 300.0, 2180.0, 60.0);
        title.kind = LineKind::Title;
        title.layout_confidence = Some(0.9);
        let mut lines = vec![
            title,
            line("Jane Q. Researcher", 800.0, 420.0, 1680.0, 45.0),
        ];
        for row in 0..20 {
            lines.push(line(
                "We study the problem of efficient attention in long",
                200.0,
                800.0 + row as f32 * 90.0,
                2280.0,
                45.0,
            ));
        }
        let guess = guess(&page(lines));
        assert_eq!(
            values(&guess),
            (
                Some("Sparse Attention Revisited"),
                Some("Jane Q. Researcher")
            ),
            "{guess:?}"
        );
    }

    #[test]
    fn two_names_on_two_rows_become_a_list() {
        let guess = guess(&page(vec![
            line("Field Notes on Rivers", 300.0, 600.0, 2180.0, 170.0),
            line("Anna Berg", 1000.0, 2800.0, 1480.0, 70.0),
            line("Tomas Lind", 1000.0, 2890.0, 1480.0, 70.0),
        ]));
        assert_eq!(
            values(&guess),
            (Some("Field Notes on Rivers"), Some("Anna Berg, Tomas Lind")),
            "{guess:?}"
        );
    }

    #[test]
    fn a_number_title_is_allowed_but_a_chapter_heading_is_not() {
        assert!(credible_title("1984"));
        assert!(credible_title("2001: A Space Odyssey"));
        assert!(!credible_title("Chapter 4"));
        assert!(!credible_title("Part II The Return"));
    }

    #[test]
    fn person_names_are_told_apart_from_titles() {
        assert!(person_name_like("J. R. R. Tolkien"));
        assert!(person_name_like("MARY SHELLEY"));
        assert!(!person_name_like("The Great Gatsby"));
        assert!(!person_name_like("A History of Rome"));
    }

    #[test]
    fn hyphenated_line_breaks_are_mended() {
        let guess = guess(&page(vec![
            line("Thermo-", 300.0, 600.0, 2180.0, 170.0),
            line("dynamics Today", 300.0, 800.0, 2180.0, 170.0),
            line("Lena Fischer", 900.0, 3000.0, 1580.0, 60.0),
        ]));
        assert_eq!(values(&guess).0, Some("Thermodynamics Today"), "{guess:?}");
    }

    fn titled(mut line: Line, region: usize, confidence: f32) -> Line {
        line.kind = LineKind::Title;
        line.region = Some(region);
        line.layout_confidence = Some(confidence);
        line
    }

    // Shapes below are taken from real page-1 OCR dumps.

    #[test]
    fn widely_spaced_lines_of_one_doc_title_region_are_one_title() {
        let guess = guess(&page(vec![
            titled(
                line("PROCEEDINGS OF THE CONFERENCE", 268.0, 749.0, 1299.0, 55.0),
                0,
                0.44,
            ),
            titled(line("OF", 750.0, 875.0, 817.0, 52.0), 0, 0.44),
            titled(
                line("FEDERAL AND PROVINCIAL", 304.0, 997.0, 1263.0, 64.0),
                0,
                0.44,
            ),
            titled(line("GOVERNMENTS", 514.0, 1112.0, 1048.0, 66.0), 0, 0.44),
            line("Ottawa, December 4-7, 1950", 416.0, 1494.0, 1149.0, 57.0),
            line("Price $1.50", 654.0, 1880.0, 897.0, 51.0),
            line(
                "EDMOND CLOUTIER, C.M.G., O.A., D.S.P.",
                506.0,
                2035.0,
                1046.0,
                32.0,
            ),
            line(
                "QUEEN'S PRINTER AND CONTROLLER OF STATIONERY",
                396.0,
                2064.0,
                1159.0,
                33.0,
            ),
            line("OTTAWA,1953", 688.0, 2094.0, 868.0, 31.0),
        ]));
        assert_eq!(
            values(&guess),
            (
                Some("PROCEEDINGS OF THE CONFERENCE OF FEDERAL AND PROVINCIAL GOVERNMENTS"),
                None
            ),
            "{guess:?}"
        );
    }

    #[test]
    fn a_title_may_mention_books_and_sit_at_the_bottom() {
        let guess = guess(&page(vec![
            titled(
                line("MEDICAL SYMBOLISM", 75.0, 1984.0, 1566.0, 118.0),
                0,
                0.8,
            ),
            titled(
                line("IN BOOKS OF THE", 335.0, 2126.0, 1265.0, 100.0),
                0,
                0.8,
            ),
            titled(
                line("RENAISSANCE AND BAROQUE", 57.0, 2254.0, 1576.0, 101.0),
                0,
                0.8,
            ),
        ]));
        assert_eq!(
            values(&guess).0,
            Some("MEDICAL SYMBOLISM IN BOOKS OF THE RENAISSANCE AND BAROQUE"),
            "{guess:?}"
        );
    }

    #[test]
    fn a_section_heading_is_not_a_title_and_a_photo_credit_is_not_an_author() {
        let mut heading = line("Ingredients", 170.0, 2522.0, 588.0, 71.0);
        heading.kind = LineKind::Heading;
        let mut lines = vec![
            line(
                "Craig Lee for The New York Times",
                173.0,
                1692.0,
                935.0,
                55.0,
            ),
            heading,
        ];
        for row in 0..12 {
            lines.push(line(
                "Every Monday, you can find a pot of red beans and rice cooking",
                170.0,
                1794.0 + row as f32 * 70.0,
                2300.0,
                53.0,
            ));
        }
        let guess = guess(&page(lines));
        assert_eq!(values(&guess), (None, None), "{guess:?}");
    }

    #[test]
    fn a_much_smaller_block_does_not_steal_the_title_from_a_name_like_phrase() {
        let guess = guess(&page(vec![
            line("ALVIN W. VOGTLE", 153.0, 1990.0, 835.0, 72.0),
            line("Electric Generating Plant", 143.0, 2092.0, 1485.0, 133.0),
            line("MEAGPOWER", 1003.0, 2317.0, 1244.0, 78.0),
            line("Dalton", 1357.0, 2324.0, 1510.0, 56.0),
            line("UTILITIES", 1365.0, 2370.0, 1502.0, 26.0),
        ]));
        assert_eq!(
            values(&guess).0,
            Some("Electric Generating Plant"),
            "{guess:?}"
        );
    }

    #[test]
    fn a_single_large_line_on_an_empty_page_is_the_title() {
        let guess = guess(&page(vec![line("FRAGMENTS", 1085.0, 2272.0, 1445.0, 71.0)]));
        assert_eq!(values(&guess).0, Some("FRAGMENTS"), "{guess:?}");
    }

    #[test]
    fn a_title_boxed_line_by_line_in_separate_regions_is_still_one_title() {
        let guess = guess(&page(vec![
            titled(
                line("PROCEEDINGS OF THE CONFERENCE", 268.0, 749.0, 1299.0, 55.0),
                0,
                0.44,
            ),
            titled(line("OF", 750.0, 875.0, 817.0, 52.0), 1, 0.44),
            titled(
                line("FEDERAL AND PROVINCIAL", 304.0, 997.0, 1263.0, 64.0),
                2,
                0.44,
            ),
            titled(line("GOVERNMENTS", 514.0, 1112.0, 1048.0, 66.0), 3, 0.44),
            line("Ottawa, December 4-7, 1950", 416.0, 1494.0, 1149.0, 57.0),
            line(
                "EDMOND CLOUTIER, C.M.G., O.A., D.S.P.",
                506.0,
                2035.0,
                1046.0,
                32.0,
            ),
        ]));
        assert_eq!(
            values(&guess).0,
            Some("PROCEEDINGS OF THE CONFERENCE OF FEDERAL AND PROVINCIAL GOVERNMENTS"),
            "{guess:?}"
        );
    }

    #[test]
    fn a_cover_title_set_in_mixed_sizes_stays_together() {
        let guess = guess(&PageLines {
            width: 4857.0,
            height: 8236.0,
            lines: vec![
                line("1933:", 831.0, 357.0, 1566.0, 310.0),
                line("ROOSEVELT'S", 129.0, 646.0, 2223.0, 323.0),
                line("DECISION", 405.0, 971.0, 1948.0, 316.0),
                line("the", 971.0, 1361.0, 1362.0, 225.0),
                line("UNITED", 348.0, 1583.0, 1948.0, 328.0),
                line("STATES", 418.0, 1937.0, 1834.0, 321.0),
                line("Leaves the", 728.0, 2420.0, 1599.0, 237.0),
                line("GOLD", 526.0, 2645.0, 1802.0, 443.0),
                line("Standard", 362.0, 3090.0, 1972.0, 255.0),
                line("JORDAN SCHWARZ", 499.0, 3500.0, 1841.0, 142.0),
                line("$2.45", 4373.0, 7956.0, 4740.0, 161.0),
            ],
        });
        assert_eq!(
            values(&guess),
            (
                Some("1933: ROOSEVELT'S DECISION the UNITED STATES Leaves the GOLD Standard"),
                Some("JORDAN SCHWARZ")
            ),
            "{guess:?}"
        );
    }

    #[test]
    fn a_publisher_logo_and_a_stock_number_are_not_titles() {
        let mut stock = line("20663", 4091.0, 180.0, 4693.0, 154.0);
        stock.kind = LineKind::Header;
        let guess = guess(&PageLines {
            width: 4984.0,
            height: 8025.0,
            lines: vec![
                line("SWSa PUBLICATION", 109.0, 53.0, 2576.0, 301.0),
                stock,
                line("by", 1448.0, 6943.0, 1679.0, 202.0),
                line("ROBERT M. BROWN", 764.0, 7189.0, 2451.0, 168.0),
            ],
        });
        assert_eq!(values(&guess), (None, Some("ROBERT M. BROWN")), "{guess:?}");
    }

    #[test]
    fn a_big_logo_at_the_bottom_does_not_beat_the_top_half_title() {
        let mut lines = vec![
            line("1933:", 831.0, 357.0, 1566.0, 310.0),
            line("ROOSEVELT'S", 129.0, 646.0, 2223.0, 323.0),
            line("DECISION", 405.0, 971.0, 1948.0, 316.0),
            line("JORDAN SCHWARZ", 499.0, 3500.0, 1841.0, 142.0),
            line("NRA", 2623.0, 6349.0, 4157.0, 434.0),
            line("Chelsea House/Vintage", 247.0, 7869.0, 1682.0, 174.0),
        ];
        // Banknote artwork the recogniser reads as text.
        for (row, text) in ["AVENOZE", "WAEHIROTOR.DC", "Tinker", "ONE"]
            .iter()
            .enumerate()
        {
            lines.push(line(
                text,
                2000.0,
                4300.0 + row as f32 * 500.0,
                3000.0,
                120.0,
            ));
        }
        let guess = guess(&PageLines {
            width: 4857.0,
            height: 8236.0,
            lines,
        });
        assert_eq!(
            values(&guess),
            (Some("1933: ROOSEVELT'S DECISION"), Some("JORDAN SCHWARZ")),
            "{guess:?}"
        );
    }

    #[test]
    fn library_stamps_are_not_titles_and_offices_are_not_authors() {
        let guess = guess(&page(vec![
            line("W2 A2", 51.0, 50.0, 201.0, 80.0),
            line("qW2f", 19.0, 156.0, 247.0, 97.0),
            line("10.21-100", 22.0, 221.0, 279.0, 73.0),
            line("WAR DEPARTMENT", 378.0, 418.0, 837.0, 60.0),
            line("BASIC FIELD MANUAL", 312.0, 595.0, 901.0, 53.0),
            line("SOLDIER'S HANDBOOK", 237.0, 813.0, 977.0, 61.0),
            line("JAN 17 1952", 297.0, 1601.0, 590.0, 69.0),
            line("LIBRAR349694", 263.0, 1632.0, 905.0, 127.0),
        ]));
        assert!(
            guess
                .title
                .as_ref()
                .is_none_or(|title| !title.value.contains("LIBRAR")),
            "{guess:?}"
        );
        assert_eq!(values(&guess).1, None, "{guess:?}");
        assert!(!person_name_like("WAR DEPARTMENT"));
        assert!(!person_name_like("Georgia Power"));
        assert!(wordlike("Brownie Automatic Movie Camera f/2.3"));
        assert!(!wordlike("W2 A2 qW2f 10.21-100"));
    }

    #[test]
    fn people_lists_are_recognised() {
        assert!(person_list_like("Jane Doe and John Roe"));
        assert!(person_list_like("Ann Lee, Bo Kim & Cy Young"));
        assert!(person_list_like("Doe, Jane"));
        assert!(!person_list_like("EDMOND CLOUTIER, C.M.G., O.A., D.S.P."));
        assert!(!person_list_like("Craig Lee for The New York Times"));
    }
}
