//! Component extraction using a graph of horizontal foreground spans.
//!
//! Each maximal row span is a vertex. Adjacent-row spans are connected when
//! their inclusive intervals overlap or touch diagonally. One sweep over
//! consecutive rows joins touching spans in a union-find; components are
//! then numbered in order of their first span, as a traversal seeded in span
//! order would number them. No DjVu-specific segmentation is involved.
//!
//! The public staged API and historical threshold/order behavior are retained.
//! This is a new implementation, not a clean-room claim: the author had access
//! to the old module. Historical DjVuLibre attribution remains in NOTICE and
//! PROVENANCE.md. Appropriate licensing review is still a release gate.

use crate::jbig2sym::BitImage;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Run {
    pub y: i32,
    pub x1: i32,
    pub x2: i32,
    pub ccid: i32,
}

/// Half-open page bounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BBox {
    pub xmin: i32,
    pub ymin: i32,
    pub xmax: i32,
    pub ymax: i32,
}
impl BBox {
    pub fn width(&self) -> i32 {
        self.xmax.saturating_sub(self.xmin)
    }
    pub fn height(&self) -> i32 {
        self.ymax.saturating_sub(self.ymin)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CC {
    pub bb: BBox,
    pub npix: i32,
    pub nrun: i32,
    pub frun: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct ShapeRef {
    pub ccid: usize,
    pub bbox: BBox,
    pub black_pixels: usize,
    pub run_count: usize,
}

/// Staged extraction. analyze(0) never erases, merges or splits components.
pub struct CCImage {
    pub width: i32,
    pub height: i32,
    pub runs: Vec<Run>,
    pub ccs: Vec<CC>,
    pub nregularccs: usize,
    pub largesize: i32,
    pub smallsize: i32,
    pub tinysize: i32,
}

impl CCImage {
    pub fn new(width: i32, height: i32, dpi: i32) -> Self {
        let resolution = dpi.clamp(200, 900);
        Self {
            width,
            height,
            runs: Vec::new(),
            ccs: Vec::new(),
            nregularccs: 0,
            largesize: resolution.clamp(64, 500),
            smallsize: (resolution / 150).max(2),
            tinysize: (resolution * resolution / 20_000 - 1).max(0),
        }
    }

    pub fn add_single_run(&mut self, y: i32, x1: i32, x2: i32) {
        if y >= 0 && y < self.height && x1 <= x2 && x2 >= 0 && x1 < self.width {
            self.runs.push(Run {
                y,
                x1: x1.max(0),
                x2: x2.min(self.width - 1),
                ccid: 0,
            });
        }
    }

    pub fn add_bitmap_runs(&mut self, bitmap: &BitImage) {
        for y in 0..bitmap.height.min(self.height.max(0) as usize) {
            let width = bitmap.width.min(self.width.max(0) as usize);
            let row = bitmap.row_words(y);
            let mut cursor = 0;
            while let Some(left) = crate::encode::sym::row_first_one(row, cursor, width) {
                let end = crate::encode::sym::row_first_zero(row, left, width).unwrap_or(width);
                self.add_single_run(y as i32, left as i32, (end - 1) as i32);
                cursor = end;
            }
        }
    }

    /// Canonicalize the foreground set, also accepting unsorted/overlapping
    /// caller-supplied spans, before constructing the adjacency graph.
    fn normalize_runs(&mut self) {
        let already_canonical = self.runs.iter().all(|r| {
            r.y >= 0 && r.y < self.height && r.x1 >= 0 && r.x1 <= r.x2 && r.x2 < self.width
        }) && self.runs.windows(2).all(|pair| {
            pair[0].y < pair[1].y
                || (pair[0].y == pair[1].y && pair[0].x2.saturating_add(1) < pair[1].x1)
        });
        if already_canonical {
            for run in &mut self.runs {
                run.ccid = -1;
            }
            return;
        }
        self.runs.retain(|r| {
            r.y >= 0 && r.y < self.height && r.x1 <= r.x2 && r.x2 >= 0 && r.x1 < self.width
        });
        for run in &mut self.runs {
            run.x1 = run.x1.max(0);
            run.x2 = run.x2.min(self.width - 1);
            run.ccid = -1;
        }
        self.runs.sort_unstable_by_key(|r| (r.y, r.x1, r.x2));
        let mut used = 0;
        for index in 0..self.runs.len() {
            if used > 0
                && self.runs[used - 1].y == self.runs[index].y
                && self.runs[index].x1 <= self.runs[used - 1].x2.saturating_add(1)
            {
                self.runs[used - 1].x2 = self.runs[used - 1].x2.max(self.runs[index].x2);
            } else {
                self.runs.swap(used, index);
                used += 1;
            }
        }
        self.runs.truncate(used);
    }

    /// Label the 8-connected span graph. Spans are canonical (sorted by row,
    /// then column, and separated within a row), so touching spans of two
    /// consecutive rows are found by one merge-like pass over both rows.
    pub fn make_ccids_by_analysis(&mut self) {
        self.normalize_runs();
        let count = self.runs.len();
        let mut parent: Vec<u32> = (0..count as u32).collect();
        let mut previous = 0..0;
        let mut start = 0;
        while start < count {
            let y = self.runs[start].y;
            let mut end = start + 1;
            while end < count && self.runs[end].y == y {
                end += 1;
            }
            if previous.start < previous.end && self.runs[previous.start].y == y - 1 {
                let (mut above, mut below) = (previous.start, start);
                while above < previous.end && below < end {
                    let (upper, lower) = (&self.runs[above], &self.runs[below]);
                    if upper.x2 >= lower.x1 - 1 && upper.x1 <= lower.x2 + 1 {
                        let upper_root = span_root(&mut parent, above as u32);
                        let lower_root = span_root(&mut parent, below as u32);
                        let (keep, merge) = if upper_root < lower_root {
                            (upper_root, lower_root)
                        } else {
                            (lower_root, upper_root)
                        };
                        parent[merge as usize] = keep;
                    }
                    if self.runs[above].x2 < self.runs[below].x2 {
                        above += 1;
                    } else {
                        below += 1;
                    }
                }
            }
            previous = start..end;
            start = end;
        }
        let mut labels = vec![-1i32; count];
        let mut next_label = 0;
        for index in 0..count {
            let root = span_root(&mut parent, index as u32) as usize;
            if labels[root] < 0 {
                labels[root] = next_label;
                next_label += 1;
            }
            self.runs[index].ccid = labels[root];
        }
        self.ccs.clear();
        self.nregularccs = 0;
    }

    /// Compact labels and derive descriptors from labeled span groups.
    /// A negative label explicitly excludes a span.
    pub fn make_ccs_from_ccids(&mut self) {
        let regular_label_limit = self.nregularccs as i32;
        let mut regular_count = 0;
        self.runs.retain(|r| r.ccid >= 0 && r.x1 <= r.x2);
        if let Some(max_label) = self.runs.iter().map(|run| run.ccid as usize).max() {
            if max_label < self.runs.len() {
                // Graph labels are dense. Stable bucket placement avoids a
                // comparison sort of every span in a text-heavy page.
                let mut next = vec![0; max_label + 1];
                for run in &self.runs {
                    next[run.ccid as usize] += 1;
                }
                let mut offset = 0;
                for count in &mut next {
                    let length = *count;
                    *count = offset;
                    offset += length;
                }
                let mut grouped = self.runs.clone();
                for run in &self.runs {
                    let slot = &mut next[run.ccid as usize];
                    grouped[*slot] = run.clone();
                    *slot += 1;
                }
                self.runs = grouped;
            } else {
                // Arbitrary sparse public labels must not allocate by label ID.
                self.runs.sort_unstable_by_key(|r| (r.ccid, r.y, r.x1));
            }
        }
        self.ccs.clear();
        let mut previous_label = None;
        for index in 0..self.runs.len() {
            let run = &mut self.runs[index];
            let label = run.ccid;
            if previous_label != Some(label) {
                regular_count += usize::from(label < regular_label_limit);
                self.ccs.push(CC {
                    bb: BBox {
                        xmin: run.x1,
                        ymin: run.y,
                        xmax: run.x2 + 1,
                        ymax: run.y + 1,
                    },
                    frun: index as i32,
                    ..CC::default()
                });
                previous_label = Some(label);
            }
            run.ccid = (self.ccs.len() - 1) as i32;
            let descriptor = self
                .ccs
                .last_mut()
                .expect("descriptor inserted for each label");
            descriptor.nrun += 1;
            descriptor.npix = descriptor.npix.saturating_add(run.x2 - run.x1 + 1);
            descriptor.bb.xmin = descriptor.bb.xmin.min(run.x1);
            descriptor.bb.xmax = descriptor.bb.xmax.max(run.x2 + 1);
            descriptor.bb.ymin = descriptor.bb.ymin.min(run.y);
            descriptor.bb.ymax = descriptor.bb.ymax.max(run.y + 1);
        }
        // The historical default leaves this at zero. Reading-order sorting
        // is opt-in for staged callers; changing it changes legacy stream bytes.
        self.nregularccs = regular_count;
        for component in &self.ccs {
            let start = component.frun as usize;
            self.runs[start..start + component.nrun as usize]
                .sort_unstable_by_key(|run| (run.y, run.x1));
        }
    }

    pub fn erase_tiny_ccs(&mut self) {
        for (index, component) in self.ccs.iter_mut().enumerate() {
            if component.npix <= self.tinysize {
                let start = component.frun.max(0) as usize;
                let end = start
                    .saturating_add(component.nrun.max(0) as usize)
                    .min(self.runs.len());
                for run in &mut self.runs[start.min(end)..end] {
                    if run.ccid == index as i32 {
                        run.ccid = -1;
                    }
                }
                component.npix = 0;
                component.nrun = 0;
            }
        }
    }

    /// Optional grid regrouping for the historical public API. This operation
    /// does not run in analyze(). Small fragments share a cell; large objects
    /// are clipped into cells; all original foreground pixels are preserved.
    pub fn merge_and_split_ccs(&mut self) {
        let cell = self.largesize.max(1);
        let mut groups: BTreeMap<(u8, i32, i32, i32), Vec<Run>> = BTreeMap::new();
        for (index, component) in self.ccs.iter().enumerate() {
            let start = component.frun.max(0) as usize;
            let end = start
                .saturating_add(component.nrun.max(0) as usize)
                .min(self.runs.len());
            for run in &self.runs[start.min(end)..end] {
                if run.ccid < 0 {
                    continue;
                }
                if component.bb.width() <= self.smallsize && component.bb.height() <= self.smallsize
                {
                    let x = component.bb.xmin + component.bb.width() / 2;
                    let y = component.bb.ymin + component.bb.height() / 2;
                    groups
                        .entry((1, y.div_euclid(cell), x.div_euclid(cell), 0))
                        .or_default()
                        .push(run.clone());
                } else if component.bb.width() >= cell || component.bb.height() >= cell {
                    let mut left = run.x1;
                    while left <= run.x2 {
                        let column = left.div_euclid(cell);
                        let right = run.x2.min((column + 1).saturating_mul(cell) - 1);
                        groups
                            .entry((2, run.y.div_euclid(cell), column, index as i32))
                            .or_default()
                            .push(Run {
                                y: run.y,
                                x1: left,
                                x2: right,
                                ccid: 0,
                            });
                        left = right + 1;
                    }
                } else {
                    groups
                        .entry((0, index as i32, 0, 0))
                        .or_default()
                        .push(run.clone());
                }
            }
        }
        let regular = groups.keys().filter(|key| key.0 == 0).count();
        self.runs.clear();
        for (label, (_, mut runs)) in groups.into_iter().enumerate() {
            for run in &mut runs {
                run.ccid = label as i32;
            }
            self.runs.extend(runs);
        }
        self.nregularccs = regular;
        self.make_ccs_from_ccids();
        self.nregularccs = regular;
    }

    /// Sort descriptors in short row bands without moving their run storage.
    pub fn sort_in_reading_order(&mut self) {
        let regular = self.nregularccs.min(self.ccs.len());
        let mut order: Vec<usize> = (0..regular).collect();
        order.sort_by_key(|&i| {
            let c = &self.ccs[i];
            (c.bb.ymin, c.bb.xmin, c.frun)
        });
        let band_height = (self.width / 40).max(32);
        let mut start = 0;
        while start < order.len() {
            let bottom = self.ccs[order[start]].bb.ymin.saturating_add(band_height);
            let end = start + order[start..].partition_point(|&i| self.ccs[i].bb.ymin <= bottom);
            order[start..end].sort_by_key(|&i| self.ccs[i].bb.xmin);
            start = end;
        }
        order.extend(regular..self.ccs.len());
        let mut remap = vec![0; self.ccs.len()];
        let reordered = order
            .into_iter()
            .enumerate()
            .map(|(new, old)| {
                remap[old] = new as i32;
                self.ccs[old].clone()
            })
            .collect();
        self.ccs = reordered;
        for run in &mut self.runs {
            if run.ccid >= 0 {
                run.ccid = remap[run.ccid as usize];
            }
        }
    }

    pub fn get_bitmap_for_cc(&self, ccid: usize) -> Option<BitImage> {
        let component = self.ccs.get(ccid)?;
        if component.nrun <= 0 || component.bb.width() <= 0 || component.bb.height() <= 0 {
            return None;
        }
        let mut bitmap =
            BitImage::new(component.bb.width() as u32, component.bb.height() as u32).ok()?;
        let start = usize::try_from(component.frun).ok()?;
        let count = usize::try_from(component.nrun).ok()?;
        for run in self.runs.get(start..start.checked_add(count)?)? {
            if run.ccid != ccid as i32 {
                continue;
            }
            // Pixels outside the component box are ignored, as per-pixel
            // writes would ignore them.
            let x_start = run.x1.max(component.bb.xmin);
            let x_end = run.x2.min(component.bb.xmax - 1);
            if run.y < component.bb.ymin || run.y >= component.bb.ymax || x_start > x_end {
                continue;
            }
            bitmap.set_span(
                (run.y - component.bb.ymin) as usize,
                (x_start - component.bb.xmin) as usize,
                (x_end - component.bb.xmin) as usize,
            );
        }
        Some(bitmap)
    }

    pub fn analyze(&mut self, losslevel: i32) {
        self.make_ccids_by_analysis();
        self.make_ccs_from_ccids();
        if losslevel > 0 {
            self.erase_tiny_ccs();
        }
        self.sort_in_reading_order();
    }

    pub fn extract_shape_refs(&self) -> Vec<ShapeRef> {
        self.ccs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.nrun > 0)
            .map(|(ccid, c)| ShapeRef {
                ccid,
                bbox: c.bb,
                black_pixels: c.npix.max(0) as usize,
                run_count: c.nrun.max(0) as usize,
            })
            .collect()
    }

    pub fn extract_shapes(&self) -> Vec<(BitImage, BBox)> {
        self.extract_shape_refs()
            .into_iter()
            .filter_map(|shape| {
                self.get_bitmap_for_cc(shape.ccid)
                    .map(|bitmap| (bitmap, shape.bbox))
            })
            .collect()
    }
}

pub fn analyze_page(image: &BitImage, dpi: i32, losslevel: i32) -> CCImage {
    let mut components = CCImage::new(image.width as i32, image.height as i32, dpi);
    components.add_bitmap_runs(image);
    components.analyze(losslevel);
    components
}

pub fn extract_symbols_for_jbig2(components: &CCImage) -> Vec<(BitImage, i32, i32, i32, i32)> {
    components
        .extract_shapes()
        .into_iter()
        .map(|(bitmap, bounds)| {
            (
                bitmap,
                bounds.xmin,
                bounds.ymin,
                bounds.width(),
                bounds.height(),
            )
        })
        .collect()
}

fn span_root(parent: &mut [u32], mut node: u32) -> u32 {
    while parent[node as usize] != node {
        let grandparent = parent[parent[node as usize] as usize];
        parent[node as usize] = grandparent;
        node = grandparent;
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Labels from a depth-first traversal seeded in span order, over an
    /// explicit quadratic adjacency test: the numbering the sweep must match.
    fn traversal_labels(runs: &[Run]) -> Vec<i32> {
        let touches =
            |a: &Run, b: &Run| (a.y - b.y).abs() == 1 && a.x2 >= b.x1 - 1 && a.x1 <= b.x2 + 1;
        let mut labels = vec![-1; runs.len()];
        let mut next = 0;
        for seed in 0..runs.len() {
            if labels[seed] >= 0 {
                continue;
            }
            labels[seed] = next;
            let mut pending = vec![seed];
            while let Some(index) = pending.pop() {
                for other in 0..runs.len() {
                    if labels[other] < 0 && touches(&runs[index], &runs[other]) {
                        labels[other] = next;
                        pending.push(other);
                    }
                }
            }
            next += 1;
        }
        labels
    }

    #[test]
    fn row_sweep_labels_match_span_traversal() {
        let mut state = 0x1234_5678_9abc_def1u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..400 {
            let width = 1 + (next() % 90) as usize;
            let height = 1 + (next() % 40) as usize;
            let density = next() % 100;
            let mut bitmap = BitImage::new(width as u32, height as u32).unwrap();
            for y in 0..height {
                // Skip some rows entirely so non-adjacent rows are exercised.
                if next() % 7 == 0 {
                    continue;
                }
                for x in 0..width {
                    if next() % 100 < density {
                        bitmap.set_usize(x, y, true);
                    }
                }
            }
            let mut image = CCImage::new(width as i32, height as i32, 300);
            image.add_bitmap_runs(&bitmap);
            image.make_ccids_by_analysis();
            let expected = traversal_labels(&image.runs);
            let actual: Vec<i32> = image.runs.iter().map(|run| run.ccid).collect();
            assert_eq!(actual, expected, "case {case} {width}x{height}");
        }
    }
}
