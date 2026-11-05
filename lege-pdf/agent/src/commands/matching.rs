//! Finding a query in page text the way a reader would recognise it.
//!
//! Text extracted from a PDF is not the text the author typed: lines end in
//! `\r\n`, a phrase is broken across them, "fi" may be one ligature
//! character, quotes are typographic, and a word hyphenated at a line end
//! comes back joined (or not, depending on the producer). A byte-for-byte
//! search misses all of these, and the agent asking has no way to know which
//! spelling the page happens to use. So by default both the page and the
//! query are folded to a common form before matching, and every match is
//! mapped back to the characters of the original text it covers.

use anyhow::{Result, bail};

/// How a query is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryOptions {
    /// The query is a regular expression rather than a phrase.
    pub regex: bool,
    pub case_insensitive: bool,
    /// Match the query character for character: no whitespace, hyphen,
    /// ligature or quote folding. Ignored for a regular expression, which
    /// states its own tolerance.
    pub exact: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            regex: false,
            case_insensitive: true,
            exact: false,
        }
    }
}

/// A query prepared once and run against many pages.
#[derive(Debug)]
pub struct Query {
    matcher: Matcher,
    fold: Fold,
}

#[derive(Debug)]
enum Matcher {
    Literal(Vec<char>),
    Regex(regex::Regex),
}

/// Which differences between two spellings are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fold {
    /// Any run of whitespace, line breaks included, is one space.
    whitespace: bool,
    /// Ligatures are their letters; typographic quotes are plain ones; soft
    /// hyphens are nothing.
    presentation: bool,
    /// Hyphens are nothing, so `re-use`, `reuse` and a `re-` / `use` line
    /// break all match one another.
    hyphens: bool,
    case: bool,
}

/// A match, as a range of characters of the original text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub first_char: usize,
    pub char_count: usize,
}

impl Query {
    pub fn new(query: &str, options: QueryOptions) -> Result<Self> {
        if options.regex {
            let regex = regex::RegexBuilder::new(query)
                .case_insensitive(options.case_insensitive)
                // A pattern is caller input; bound what compiling it may cost.
                .size_limit(4 << 20)
                .build()
                .map_err(|err| anyhow::anyhow!("invalid regular expression: {err}"))?;
            return Ok(Self {
                matcher: Matcher::Regex(regex),
                // The pattern is matched against text with line breaks and
                // ligatures out of the way, but nothing it could name is
                // removed: hyphens and case stay as the pattern treats them.
                fold: Fold {
                    whitespace: true,
                    presentation: true,
                    hyphens: false,
                    case: false,
                },
            });
        }

        let fold = Fold {
            whitespace: !options.exact,
            presentation: !options.exact,
            hyphens: !options.exact,
            case: options.case_insensitive,
        };
        let folded = Folded::new(query, fold);
        let needle: Vec<char> = if options.exact {
            folded.chars
        } else {
            // Leading and trailing spaces in a phrase are not part of it.
            let text: String = folded.chars.iter().collect();
            text.trim().chars().collect()
        };
        if needle.is_empty() {
            if query.is_empty() {
                bail!("search query must not be empty");
            }
            bail!(
                "search query {query:?} has nothing left to match once whitespace and hyphens are set aside; search for it exactly instead"
            );
        }
        Ok(Self {
            matcher: Matcher::Literal(needle),
            fold,
        })
    }

    /// Every non-overlapping match in `text`, in order, up to `limit`
    /// (0 = no limit). The second value is true when the limit cut the list.
    pub fn find(&self, text: &str, limit: usize) -> (Vec<Span>, bool) {
        let folded = Folded::new(text, self.fold);
        let mut spans = Vec::new();
        let mut cut = false;
        let mut push = |start: usize, end: usize| -> bool {
            if limit > 0 && spans.len() >= limit {
                cut = true;
                return false;
            }
            let first_char = folded.origin[start];
            let last_char = folded.origin[end - 1];
            spans.push(Span {
                first_char,
                char_count: last_char + 1 - first_char,
            });
            true
        };

        match &self.matcher {
            Matcher::Literal(needle) => {
                let (hay, len) = (&folded.chars, needle.len());
                let mut at = 0;
                while at + len <= hay.len() {
                    if hay[at..at + len] == needle[..] {
                        if !push(at, at + len) {
                            break;
                        }
                        at += len;
                    } else {
                        at += 1;
                    }
                }
            }
            Matcher::Regex(regex) => {
                let hay: String = folded.chars.iter().collect();
                // Byte offset of each folded character, plus the end.
                let mut offsets: Vec<usize> = hay.char_indices().map(|(at, _)| at).collect();
                offsets.push(hay.len());
                for found in regex.find_iter(&hay) {
                    if found.is_empty() {
                        continue;
                    }
                    let (Ok(start), Ok(end)) = (
                        offsets.binary_search(&found.start()),
                        offsets.binary_search(&found.end()),
                    ) else {
                        continue;
                    };
                    if !push(start, end) {
                        break;
                    }
                }
            }
        }
        (spans, cut)
    }
}

/// Text reduced to the form it is compared in, with each character's
/// position in the original kept beside it.
struct Folded {
    chars: Vec<char>,
    /// `origin[i]` is the index, in characters of the original text, that
    /// `chars[i]` was produced from.
    origin: Vec<usize>,
}

impl Folded {
    fn new(text: &str, fold: Fold) -> Self {
        let mut chars = Vec::with_capacity(text.len());
        let mut origin = Vec::with_capacity(text.len());
        let mut push = |character: char, index: usize| {
            if fold.case {
                for lower in character.to_lowercase() {
                    chars.push(lower);
                    origin.push(index);
                }
            } else {
                chars.push(character);
                origin.push(index);
            }
        };
        let mut in_space = false;
        for (index, character) in text.chars().enumerate() {
            if fold.whitespace && (character.is_whitespace() || character == '\u{a0}') {
                if !in_space {
                    push(' ', index);
                    in_space = true;
                }
                continue;
            }
            if fold.presentation {
                let expanded: Option<&str> = match character {
                    '\u{ad}' => Some(""),
                    '\u{fb00}' => Some("ff"),
                    '\u{fb01}' => Some("fi"),
                    '\u{fb02}' => Some("fl"),
                    '\u{fb03}' => Some("ffi"),
                    '\u{fb04}' => Some("ffl"),
                    '\u{fb05}' | '\u{fb06}' => Some("st"),
                    '\u{2018}' | '\u{2019}' | '\u{201b}' | '\u{2032}' => Some("'"),
                    '\u{201c}' | '\u{201d}' | '\u{201f}' | '\u{2033}' => Some("\""),
                    _ => None,
                };
                if let Some(expanded) = expanded {
                    for part in expanded.chars() {
                        push(part, index);
                    }
                    if !expanded.is_empty() {
                        in_space = false;
                    }
                    continue;
                }
            }
            if fold.hyphens && matches!(character, '-' | '\u{2010}' | '\u{2011}') {
                continue;
            }
            // A dash between words reads as a hyphen to someone typing a query.
            let character = match character {
                '\u{2012}' | '\u{2013}' | '\u{2212}' if fold.presentation => '-',
                other => other,
            };
            if fold.hyphens && character == '-' {
                continue;
            }
            push(character, index);
            in_space = false;
        }
        Self { chars, origin }
    }
}

/// `text[first..first + count]` in characters, with `context` characters on
/// each side, as one line: runs of whitespace become single spaces, so a
/// match that crosses a line break reads as the phrase it is.
pub fn excerpt(chars: &[char], span: Span, context: usize) -> (String, String) {
    let start = span.first_char.min(chars.len());
    let end = (span.first_char + span.char_count).min(chars.len());
    let one_line = |slice: &[char]| -> String {
        let mut out = String::with_capacity(slice.len());
        let mut in_space = false;
        for &character in slice {
            if character.is_whitespace() {
                if !in_space {
                    out.push(' ');
                }
                in_space = true;
            } else {
                out.push(character);
                in_space = false;
            }
        }
        out
    };
    let matched = one_line(&chars[start..end]);
    let before = start.saturating_sub(context);
    let after = (end + context).min(chars.len());
    let around = one_line(&chars[before..after]);
    (matched, around.trim().to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn spans(text: &str, query: &str, options: QueryOptions) -> Vec<(usize, usize)> {
        Query::new(query, options)
            .expect("query")
            .find(text, 0)
            .0
            .into_iter()
            .map(|span| (span.first_char, span.char_count))
            .collect()
    }

    fn matched(text: &str, query: &str, options: QueryOptions) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        Query::new(query, options)
            .expect("query")
            .find(text, 0)
            .0
            .into_iter()
            .map(|span| excerpt(&chars, span, 0).0)
            .collect()
    }

    #[test]
    fn a_phrase_matches_across_a_line_break() {
        let text = "the halftone\r\nregion segment";
        assert_eq!(
            matched(text, "halftone region", QueryOptions::default()),
            vec!["halftone region"]
        );
        // The span covers the original characters, line break included.
        assert_eq!(
            spans(text, "halftone region", QueryOptions::default()),
            vec![(4, 16)]
        );
    }

    #[test]
    fn hyphenation_and_ligatures_do_not_hide_a_word() {
        let options = QueryOptions::default();
        assert_eq!(matched("a re-use of it", "reuse", options), vec!["re-use"]);
        assert_eq!(matched("a reuse of it", "re-use", options), vec!["reuse"]);
        assert_eq!(
            matched("the e\u{fb03}cient way", "efficient", options).len(),
            1
        );
        assert_eq!(
            matched("\u{201c}quoted\u{201d}", "\"quoted\"", options).len(),
            1
        );
        assert_eq!(matched("co\u{ad}operate", "cooperate", options).len(), 1);
    }

    #[test]
    fn exact_matching_folds_nothing_but_case_when_asked() {
        let exact = QueryOptions {
            exact: true,
            ..QueryOptions::default()
        };
        assert!(matched("a reuse of it", "re-use", exact).is_empty());
        assert_eq!(matched("a Re-Use of it", "re-use", exact), vec!["Re-Use"]);
        let case_exact = QueryOptions {
            exact: true,
            case_insensitive: false,
            ..QueryOptions::default()
        };
        assert!(matched("a Re-Use of it", "re-use", case_exact).is_empty());
    }

    #[test]
    fn offsets_stay_aligned_when_folding_changes_lengths() {
        // `İ` lowercases to two characters and the ligature to three; the
        // match must still land on the right original characters.
        let text = "İ e\u{fb03}x café";
        assert_eq!(spans(text, "CAFÉ", QueryOptions::default()), vec![(6, 4)]);
        assert_eq!(spans(text, "ffi", QueryOptions::default()), vec![(3, 1)]);
    }

    #[test]
    fn a_regular_expression_sees_one_line_of_text() {
        let regex = QueryOptions {
            regex: true,
            ..QueryOptions::default()
        };
        let text = "Table 12 shows\nFigure 3 and  Table 7";
        assert_eq!(
            matched(text, r"(table|figure) \d+", regex),
            vec!["Table 12", "Figure 3", "Table 7"]
        );
        assert!(Query::new("(unclosed", regex).is_err());
        // An expression that can match nothing is not an endless list.
        assert!(matched(text, r"x*", regex).is_empty());
    }

    #[test]
    fn a_query_of_only_folded_characters_is_refused_with_a_way_out() {
        let error = Query::new(" - ", QueryOptions::default())
            .expect_err("nothing to match")
            .to_string();
        assert!(error.contains("exactly"), "{error}");
        let exact = QueryOptions {
            exact: true,
            ..QueryOptions::default()
        };
        assert_eq!(matched("a - b", "-", exact), vec!["-"]);
    }

    #[test]
    fn the_limit_is_reported_when_it_cuts() {
        let query = Query::new("a", QueryOptions::default()).expect("query");
        let (found, cut) = query.find("a a a a", 2);
        assert_eq!(found.len(), 2);
        assert!(cut);
        let (found, cut) = query.find("a a", 2);
        assert_eq!(found.len(), 2);
        assert!(!cut);
    }

    #[test]
    fn context_is_one_line_in_characters() {
        let text = "your café\r\nis ready";
        let chars: Vec<char> = text.chars().collect();
        let span = Query::new("café is", QueryOptions::default())
            .expect("query")
            .find(text, 0)
            .0[0];
        let (matched, around) = excerpt(&chars, span, 3);
        assert_eq!(matched, "café is");
        assert_eq!(around, "ur café is re");
    }
}
