use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use pdf_document::DocumentSnapshot;
use pdf_text::TextPageOptions;

use crate::bounds::Bounds;
use crate::commands::matching::{Query, QueryOptions, Span, excerpt};
use crate::commands::text::{self, OcrMode, TextLayout};
use crate::commands::{PageOutcome, emit_failed, for_pages, resolve_snapshot};
use crate::open::DocumentIdentity;
use crate::pages::{PageZero, parse_scan_range};
use crate::schema::{Envelope, OutputMode, Status};
use crate::views::search::{MatchView, SearchPageData, SearchSummary};
use crate::views::text::WordView;

#[derive(Debug)]
pub struct SearchArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    pub spec: SearchSpec<'a>,
    pub fail_fast: bool,
    pub output: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

/// What to look for and how much of it to return.
#[derive(Debug, Clone, Copy)]
pub struct SearchSpec<'a> {
    pub query: &'a str,
    pub options: QueryOptions,
    /// Characters of surrounding text returned with each match.
    pub context: usize,
    /// Stop after this many matches in total (0 = no limit).
    pub max_matches: usize,
    pub ocr: OcrMode,
    pub ocr_language: &'a str,
    pub system_fonts: bool,
    pub bounds: Bounds,
}

/// One thing a search run reports as it goes.
#[derive(Debug)]
pub enum SearchEvent {
    /// A page with at least one match.
    Page(PageZero, SearchPageData, Vec<String>),
    /// A page that could not be read.
    Failed(PageZero, String, Vec<String>),
}

pub fn run(args: SearchArgs<'_>) -> Result<i32> {
    let (identity, snapshot) =
        resolve_snapshot(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    // Searching reads every page it is pointed at: only a page limit the
    // caller asked for applies, never the default one for bulky output.
    let scan = parse_scan_range(
        args.pages,
        snapshot.page_count(),
        args.spec.bounds.scan_pages,
    )?;
    let page_indices = scan.pages;
    let mode = args.output;
    let mut first_warnings = Some(scan.warnings);

    let mut summary = search_pages(&snapshot, &page_indices, &args.spec, |event| {
        match event {
            SearchEvent::Page(pz, data, mut warnings) => {
                if let Some(mut first) = first_warnings.take() {
                    first.append(&mut warnings);
                    warnings = first;
                }
                let env =
                    Envelope::page_ok(&document, pz.one_based(), serde_json::to_value(&data)?)
                        .with_warnings(warnings);
                emit(&env, &data, mode)?;
            }
            SearchEvent::Failed(pz, error, warnings) => {
                let env =
                    Envelope::page_failed(&document, pz.one_based(), error).with_warnings(warnings);
                emit_failed(&env, mode, true)?;
                if args.fail_fast {
                    return Ok(ControlFlow::Break(()));
                }
            }
        }
        Ok(ControlFlow::Continue(()))
    })?;

    summary.limited(scan.selected, scan.next_page);
    let mut warnings = first_warnings.take().unwrap_or_default();
    warnings.extend(summary.notes());
    let failed = summary.pages_failed > 0 || summary.timed_out;
    match mode {
        OutputMode::Human => {
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            eprintln!(
                "{} match{} on {} page{} ({} searched)",
                summary.match_count,
                if summary.match_count == 1 { "" } else { "es" },
                summary.pages_with_matches,
                if summary.pages_with_matches == 1 {
                    ""
                } else {
                    "s"
                },
                summary.pages_searched,
            );
        }
        OutputMode::Json | OutputMode::Jsonl => {
            let truncated = summary.truncated;
            let env = Envelope::ok(&document, serde_json::json!({ "summary": summary }))
                .with_warnings(warnings)
                .set_status(if truncated {
                    Status::Truncated
                } else {
                    Status::Ok
                });
            env.write_jsonl()?;
        }
    }
    Ok(i32::from(failed))
}

/// Search `pages` in parallel, reporting pages with matches (and pages that
/// failed) to `on` in page order, and return what the run covered.
///
/// Pages without a match are counted, not reported: a search of a long
/// document is a handful of hits, and a record per empty page buries them.
pub fn search_pages(
    snapshot: &DocumentSnapshot,
    pages: &[PageZero],
    spec: &SearchSpec<'_>,
    mut on: impl FnMut(SearchEvent) -> Result<ControlFlow<()>>,
) -> Result<SearchSummary> {
    let query = Query::new(spec.query, spec.options)?;
    let mut summary = SearchSummary {
        query: spec.query.to_owned(),
        pages_selected: pages.len(),
        ..SearchSummary::default()
    };
    // Per page, never more than the run may still return in total.
    let page_limit = match (spec.bounds.max_items as usize, spec.max_matches) {
        (0, limit) | (limit, 0) => limit,
        (items, matches) => items.min(matches),
    };
    let mut failure: Option<anyhow::Error> = None;

    for_pages(
        pages,
        // OCR goes through one engine; its pages are not run side by side.
        if spec.ocr == OcrMode::Never {
            spec.bounds.threads
        } else {
            1
        },
        || text::PageState::new(spec.system_fonts),
        |state, pz, warnings| search_page(snapshot, pz, state, &query, spec, page_limit, warnings),
        |index, pz, outcome, warnings| {
            let step = || -> Result<ControlFlow<()>> {
                let (mut data, cut) = match outcome {
                    PageOutcome::Done(found) => found,
                    PageOutcome::TimedOut => {
                        summary.timed_out = true;
                        summary.stop(pz.one_based(), crate::deadline::message(index, pages.len()));
                        return Ok(ControlFlow::Break(()));
                    }
                    PageOutcome::Failed(error) => {
                        summary.pages_searched += 1;
                        summary.pages_failed += 1;
                        return on(SearchEvent::Failed(pz, format!("{error:#}"), warnings));
                    }
                };
                summary.pages_searched += 1;
                if data.provenance.page_content_kind == "scanned-image"
                    && data.provenance.source == "native"
                {
                    summary.pages_without_text += 1;
                }
                if data.matches.is_empty() {
                    summary.keep_notes(pz, warnings);
                    return Ok(ControlFlow::Continue(()));
                }

                // The run's allowance, not just the page's.
                let (mut reached, mut resume_here) = (false, false);
                if spec.max_matches > 0 {
                    let left = spec.max_matches - summary.match_count;
                    if data.matches.len() > left {
                        data.matches.truncate(left);
                        data.match_count = left;
                        (reached, resume_here) = (true, true);
                    } else if data.matches.len() == left {
                        resume_here = cut;
                        reached = cut || index + 1 < pages.len();
                    }
                }
                let mut warnings = warnings;
                if cut && !reached {
                    warnings.push(format!(
                        "matches on this page truncated to max-items={}",
                        spec.bounds.max_items
                    ));
                }
                summary.match_count += data.matches.len();
                summary.pages_with_matches += 1;
                let flow = on(SearchEvent::Page(pz, data, warnings))?;
                if reached {
                    let next = if resume_here || index + 1 >= pages.len() {
                        pz.one_based()
                    } else {
                        pages[index + 1].one_based()
                    };
                    summary.stop(
                        next,
                        format!("match limit max-matches={} reached", spec.max_matches),
                    );
                    return Ok(ControlFlow::Break(()));
                }
                Ok(flow)
            };
            match step() {
                Ok(flow) => flow,
                Err(error) => {
                    failure = Some(error);
                    ControlFlow::Break(())
                }
            }
        },
    );

    match failure {
        Some(error) => Err(error),
        None => Ok(summary),
    }
}

/// The matches on one page, and whether `limit` cut them short.
fn search_page(
    snapshot: &DocumentSnapshot,
    pz: PageZero,
    state: &mut text::PageState,
    query: &Query,
    spec: &SearchSpec<'_>,
    limit: usize,
    warnings: &mut Vec<String>,
) -> Result<(SearchPageData, bool)> {
    let extracted = text::extract_page(
        snapshot,
        &state.compiler,
        pz,
        &TextPageOptions::default(),
        TextLayout::Words,
        None,
        spec.ocr,
        spec.ocr_language,
        &spec.bounds,
        warnings,
        &mut state.ocr_backend,
        &mut state.glyphs,
    )?;
    let haystack = extracted.text.as_deref().unwrap_or_default();
    let words = extracted.words.as_deref().unwrap_or_default();
    let (matches, cut) = find_matches(haystack, words, query, spec.context, limit);
    Ok((
        SearchPageData {
            unit: "pdf_points",
            query: spec.query.to_owned(),
            match_count: matches.len(),
            matches,
            provenance: extracted.provenance,
        },
        cut,
    ))
}

/// Every match of `query` in `haystack`, with `context` characters (not
/// bytes) of surrounding text and the box of the words it covers.
///
/// `first_char` and `char_count` index the characters of `haystack` itself,
/// whatever folding the query needed, so they line up with the `words` and
/// `chars` layouts of the same page.
fn find_matches(
    haystack: &str,
    words: &[WordView],
    query: &Query,
    context: usize,
    limit: usize,
) -> (Vec<MatchView>, bool) {
    let (spans, cut) = query.find(haystack, limit);
    if spans.is_empty() {
        return (Vec::new(), cut);
    }
    let chars: Vec<char> = haystack.chars().collect();
    let matches = spans
        .into_iter()
        .map(|span| {
            let (text, context) = excerpt(&chars, span, context);
            MatchView {
                text,
                // A contents line is mostly leader dots.
                context: crate::views::tidy_line(&context),
                bbox: covering_box(words, span),
                first_char: span.first_char,
                char_count: span.char_count,
            }
        })
        .collect();
    (matches, cut)
}

/// The box around every word a match touches: for a phrase, the whole
/// phrase, not just its first word.
fn covering_box(words: &[WordView], span: Span) -> Option<[f64; 4]> {
    let end = span.first_char + span.char_count;
    let mut covered = words
        .iter()
        .filter(|word| word.first_char < end && word.first_char + word.char_count > span.first_char)
        .map(|word| word.bbox);
    let first = covered.next()?;
    Some(covered.fold(first, |a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    }))
}

fn emit(env: &Envelope, data: &SearchPageData, mode: OutputMode) -> Result<()> {
    match mode {
        OutputMode::Human => {
            let page = env.page.unwrap_or(0);
            for found in &data.matches {
                println!("{}", match_line(page, found));
            }
            for warning in &env.warnings {
                eprintln!("warning: {warning}");
            }
            Ok(())
        }
        OutputMode::Json | OutputMode::Jsonl => env.write_jsonl(),
    }
}

/// `p.12 [72,540.1,210.3,552]: text around the match`
pub(crate) fn match_line(page: u32, found: &MatchView) -> String {
    match found.bbox {
        Some(bbox) => format!(
            "p.{page} [{}]: {}",
            bbox.map(|edge| crate::views::round2(edge).to_string())
                .join(","),
            found.context
        ),
        None => format!("p.{page}: {}", found.context),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn matches_for(
        haystack: &str,
        query: &str,
        context: usize,
        case_insensitive: bool,
    ) -> Vec<MatchView> {
        let query = Query::new(
            query,
            QueryOptions {
                case_insensitive,
                ..QueryOptions::default()
            },
        )
        .expect("query");
        find_matches(haystack, &[], &query, context, 0).0
    }

    fn word(text: &str, bbox: [f64; 4], first_char: usize) -> WordView {
        WordView {
            text: text.to_owned(),
            bbox,
            first_char,
            char_count: text.chars().count(),
            continued: false,
        }
    }

    /// Non-ASCII text around a match must never panic, and `context` must be
    /// interpreted as characters, not bytes: slicing at a raw byte offset
    /// derived from a character count would land inside `é`'s 2-byte UTF-8
    /// encoding.
    #[test]
    fn context_around_multibyte_text_is_char_boundary_safe() {
        let haystack = "your café is ready";
        let found = matches_for(haystack, "café", 2, false);
        assert_eq!(found.len(), 1);
        let m = &found[0];
        assert_eq!(m.text, "café");
        assert_eq!(m.first_char, 5);
        assert_eq!(m.char_count, 4);
        // 2 chars of context on each side, trimmed: "r" + "café" + " i".
        assert_eq!(m.context, "r café i");
    }

    /// Case folding is not byte-length-preserving (`İ` U+0130 folds to two
    /// characters). Indices must stay valid against the *original* haystack
    /// even when an earlier character's case-folded form changes length.
    #[test]
    fn case_insensitive_search_stays_aligned_after_length_changing_fold() {
        let haystack = "İ marks the spot: café";
        let found = matches_for(haystack, "CAFÉ", 0, true);
        assert_eq!(found.len(), 1);
        let m = &found[0];
        assert_eq!(m.text, "café");
        assert_eq!(m.context, "café");
        assert_eq!(m.first_char, 18);
        assert_eq!(m.char_count, 4);
    }

    #[test]
    fn repeated_matches_get_correct_char_offsets() {
        let haystack = "café one café two café three";
        let found = matches_for(haystack, "café", 0, false);
        let first_chars: Vec<usize> = found.iter().map(|m| m.first_char).collect();
        assert_eq!(first_chars, vec![0, 9, 18]);
        for m in &found {
            assert_eq!(m.char_count, 4);
            assert_eq!(m.text, "café");
        }
    }

    /// Word boxes are looked up by character offset, and a phrase gets the
    /// box around all of its words, across a line break too.
    #[test]
    fn a_match_is_boxed_by_every_word_it_covers() {
        let haystack = "the halftone\r\nregion here";
        let words = [
            word("the", [10.0, 100.0, 25.0, 110.0], 0),
            word("halftone", [28.0, 100.0, 70.0, 110.0], 4),
            word("region", [10.0, 88.0, 40.0, 98.0], 14),
            word("here", [43.0, 88.0, 60.0, 98.0], 21),
        ];
        let query = Query::new("halftone region", QueryOptions::default()).expect("query");
        let (found, _) = find_matches(haystack, &words, &query, 0, 0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "halftone region");
        assert_eq!(found[0].bbox, Some([10.0, 88.0, 70.0, 110.0]));

        let query = Query::new("the", QueryOptions::default()).expect("query");
        let (found, _) = find_matches(haystack, &words, &query, 0, 0);
        assert_eq!(found[0].bbox, Some([10.0, 100.0, 25.0, 110.0]));
    }

    #[test]
    fn a_match_line_reads_as_page_box_and_context() {
        let found = MatchView {
            text: "café".into(),
            context: "your café is".into(),
            bbox: Some([72.0, 540.126, 210.3, 552.0]),
            first_char: 5,
            char_count: 4,
        };
        assert_eq!(
            match_line(12, &found),
            "p.12 [72,540.13,210.3,552]: your café is"
        );
    }
}
