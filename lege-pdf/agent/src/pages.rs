//! One-based page ranges for the public CLI boundary.

use anyhow::{Context, Result, bail};

/// A zero-based page index resolved against a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageZero(pub u32);

impl PageZero {
    pub fn one_based(self) -> u32 {
        self.0 + 1
    }
}

/// Parse a one-based page number from the CLI (rejects 0).
pub fn parse_one_based(raw: &str) -> Result<u32> {
    let n: u32 = raw
        .trim()
        .parse()
        .with_context(|| format!("invalid page number {raw:?}"))?;
    if n == 0 {
        bail!("page numbers are one-based; got 0");
    }
    Ok(n)
}

/// Convert a one-based page number to a zero-based index, checking range.
pub fn to_zero_based(page_one: u32, page_count: u32) -> Result<PageZero> {
    if page_one == 0 {
        bail!("page numbers are one-based; got 0");
    }
    let index = page_one - 1;
    if page_count == 0 || index >= page_count {
        bail!("page {page_one} out of range (document has {page_count} pages)");
    }
    Ok(PageZero(index))
}

/// Parse a page range expression against `page_count`.
///
/// Accepted forms: `all`, `1`, `1-3`, `5-` (to the last page), `-3` (from the
/// first page), and comma lists of those (one-based, inclusive). Returns
/// unique zero-based indices in ascending order, capped by `max_pages`.
///
/// A range that starts inside the document and runs past its end is clamped
/// with a warning: the caller usually does not know the page count yet, and
/// "pages 10 onward" is a reasonable thing to mean by `10-999`. A page or a
/// range that lies wholly outside the document is still an error.
pub fn parse_page_range(
    spec: Option<&str>,
    page_count: u32,
    max_pages: u32,
) -> Result<(Vec<PageZero>, Vec<String>)> {
    let mut warnings = Vec::new();
    if page_count == 0 {
        return Ok((Vec::new(), warnings));
    }

    let mut indices: Vec<u32> = match spec.map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("all") => (0..page_count).collect(),
        Some(raw) => {
            let mut out = Vec::new();
            for part in raw.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                if let Some((a, b)) = part.split_once('-') {
                    let (a, b) = (a.trim(), b.trim());
                    if a.is_empty() && b.is_empty() {
                        bail!("invalid page range {part:?}");
                    }
                    let start = if a.is_empty() { 1 } else { parse_one_based(a)? };
                    let mut end = if b.is_empty() {
                        page_count
                    } else {
                        parse_one_based(b)?
                    };
                    if !b.is_empty() && start > end {
                        bail!("invalid page range {part:?}: start > end");
                    }
                    if start > page_count {
                        bail!("page range {part:?} out of range (document has {page_count} pages)");
                    }
                    if end > page_count {
                        warnings.push(format!(
                            "page range {part:?} clamped to {start}-{page_count} (document has {page_count} pages)"
                        ));
                        end = page_count;
                    }
                    out.extend(start - 1..end);
                } else {
                    let z = to_zero_based(parse_one_based(part)?, page_count)?;
                    out.push(z.0);
                }
            }
            out.sort_unstable();
            out.dedup();
            out
        }
    };

    if max_pages > 0 && indices.len() as u32 > max_pages {
        let total = indices.len();
        indices.truncate(max_pages as usize);
        let last = indices.last().map_or(0, |index| index + 1);
        warnings.push(format!(
            "page selection truncated from {total} to max-pages={max_pages}; pages after {last} were not processed"
        ));
    }

    Ok((indices.into_iter().map(PageZero).collect(), warnings))
}

/// A page selection for a command that reads every page it is pointed at.
#[derive(Debug)]
pub struct Scan {
    /// The pages to read: the selection, up to the caller's page limit.
    pub pages: Vec<PageZero>,
    /// How many pages the selection names.
    pub selected: usize,
    /// One-based number of the first selected page the limit left out.
    pub next_page: Option<u32>,
    /// What the parser had to say, the limit's effect included.
    pub warnings: Vec<String>,
}

/// Parse a page range for a scan: `limit` (0 for none) is applied here so
/// its effect can be reported as a resume point, not just as a shorter list.
pub fn parse_scan_range(spec: Option<&str>, page_count: u32, limit: u32) -> Result<Scan> {
    let (mut pages, mut warnings) = parse_page_range(spec, page_count, 0)?;
    let selected = pages.len();
    let mut next_page = None;
    if limit > 0 && selected > limit as usize {
        let next = pages[limit as usize].one_based();
        pages.truncate(limit as usize);
        warnings.push(format!(
            "page limit max-pages={limit} reached: {limit} of {selected} selected pages read; resume from page {next}"
        ));
        next_page = Some(next);
    }
    Ok(Scan {
        pages,
        selected,
        next_page,
        warnings,
    })
}

/// Parse `X0,Y0,X1,Y1` into a PDF-space rectangle.
pub fn parse_bbox(raw: &str) -> Result<[f64; 4]> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        bail!("bbox must be X0,Y0,X1,Y1; got {raw:?}");
    }
    let mut vals = [0.0; 4];
    for (i, part) in parts.iter().enumerate() {
        vals[i] = part
            .parse::<f64>()
            .with_context(|| format!("invalid bbox component {part:?}"))?;
        if !vals[i].is_finite() {
            bail!("bbox components must be finite");
        }
    }
    Ok(vals)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_simple_range() {
        let (pages, warnings) = parse_page_range(Some("1,3-4"), 5, 50).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(pages.iter().map(|p| p.0).collect::<Vec<_>>(), vec![0, 2, 3]);
    }

    #[test]
    fn a_scan_limit_names_the_page_to_resume_from() {
        let scan = parse_scan_range(Some("2-"), 9, 3).unwrap();
        assert_eq!(
            scan.pages.iter().map(|p| p.0).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!((scan.selected, scan.next_page), (8, Some(5)));
        assert!(scan.warnings[0].contains("max-pages=3"));

        let scan = parse_scan_range(None, 9, 0).unwrap();
        assert_eq!((scan.pages.len(), scan.next_page), (9, None));
        assert!(scan.warnings.is_empty());
    }

    #[test]
    fn rejects_zero() {
        assert!(parse_one_based("0").is_err());
        assert!(parse_page_range(Some("0"), 3, 50).is_err());
    }

    #[test]
    fn truncates_max_pages() {
        let (pages, warnings) = parse_page_range(Some("all"), 10, 3).unwrap();
        assert_eq!(pages.len(), 3);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn open_ended_ranges_run_to_the_document_edge() {
        let pages = |spec: &str| -> Vec<u32> {
            let (pages, warnings) = parse_page_range(Some(spec), 5, 50).unwrap();
            assert!(warnings.is_empty(), "{spec}: {warnings:?}");
            pages.iter().map(|p| p.one_based()).collect()
        };
        assert_eq!(pages("4-"), vec![4, 5]);
        assert_eq!(pages("-2"), vec![1, 2]);
        assert_eq!(pages("1,4-"), vec![1, 4, 5]);
        assert!(parse_page_range(Some("-"), 5, 50).is_err());
    }

    #[test]
    fn a_range_past_the_end_is_clamped_but_one_outside_is_an_error() {
        let (pages, warnings) = parse_page_range(Some("4-20"), 5, 50).unwrap();
        assert_eq!(
            pages.iter().map(|p| p.one_based()).collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("clamped to 4-5"), "{warnings:?}");

        assert!(parse_page_range(Some("6-9"), 5, 50).is_err());
        assert!(parse_page_range(Some("6-"), 5, 50).is_err());
        assert!(parse_page_range(Some("9"), 5, 50).is_err());
        assert!(parse_page_range(Some("3-2"), 5, 50).is_err());
    }

    #[test]
    fn parses_bbox() {
        assert_eq!(parse_bbox("0,0,100,200").unwrap(), [0.0, 0.0, 100.0, 200.0]);
    }
}
