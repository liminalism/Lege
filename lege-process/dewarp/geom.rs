//! Grid geometry of page flattening, ported from the model's standalone
//! tester (`dewarp-tester/src/geom.rs`). Every constant of the passes was
//! tuned with these exact functions; change them only against the tester.
//!
//! Conventions: values are normalized image positions, where -1 and +1 are
//! the outer pixel edges. A grid is (rows, cols) nodes holding the source
//! position each node of the flat page comes from; the corner nodes are the
//! page corners. Sizes are (height, width).

use rayon::prelude::*;

pub type P2 = [f64; 2];

#[derive(Clone, Debug)]
pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub nodes: Vec<P2>,
}

impl Grid {
    pub fn at(&self, row: usize, col: usize) -> P2 {
        self.nodes[row * self.cols + col]
    }

    pub fn is_finite(&self) -> bool {
        self.nodes
            .iter()
            .all(|p| p[0].is_finite() && p[1].is_finite())
    }

    /// Nodes as source pixel-edge points (0 and W are the outer edges).
    pub fn pixels(&self, size: (usize, usize)) -> Vec<P2> {
        let (height, width) = (size.0 as f64, size.1 as f64);
        self.nodes
            .iter()
            .map(|p| [(p[0] + 1.0) / 2.0 * width, (p[1] + 1.0) / 2.0 * height])
            .collect()
    }
}

/// Normalized positions of `count` pixel centers.
pub fn center_position(index: usize, count: usize) -> f64 {
    (2.0 * index as f64 + 1.0) / count as f64 - 1.0
}

/// Boundary nodes of a grid as source pixel-edge points.
pub fn outline_points(grid: &Grid, size: (usize, usize)) -> Vec<P2> {
    let points = grid.pixels(size);
    let at = |row: usize, col: usize| points[row * grid.cols + col];
    let mut outline = Vec::with_capacity(2 * (grid.rows + grid.cols));
    for col in 0..grid.cols {
        outline.push(at(0, col));
    }
    for col in 0..grid.cols {
        outline.push(at(grid.rows - 1, col));
    }
    for row in 1..grid.rows - 1 {
        outline.push(at(row, 0));
    }
    for row in 1..grid.rows - 1 {
        outline.push(at(row, grid.cols - 1));
    }
    outline
}

pub fn bounds_of(points: &[P2]) -> (P2, P2) {
    let mut low = [f64::INFINITY; 2];
    let mut high = [f64::NEG_INFINITY; 2];
    for p in points {
        for axis in 0..2 {
            low[axis] = low[axis].min(p[axis]);
            high[axis] = high[axis].max(p[axis]);
        }
    }
    (low, high)
}

/// Median node distance between two grids over the second grid's page diagonal.
pub fn median_drift(first: &Grid, second: &Grid, size: (usize, usize)) -> f64 {
    let scale = [size.1 as f64 / 2.0, size.0 as f64 / 2.0];
    let mut distance: Vec<f64> = first
        .nodes
        .iter()
        .zip(&second.nodes)
        .map(|(a, b)| ((a[0] - b[0]) * scale[0]).hypot((a[1] - b[1]) * scale[1]))
        .collect();
    distance.sort_by(|a, b| a.total_cmp(b));
    let n = distance.len();
    let median = if n % 2 == 1 {
        distance[n / 2]
    } else {
        (distance[n / 2 - 1] + distance[n / 2]) / 2.0
    };
    let (low, high) = bounds_of(&outline_points(second, size));
    let diagonal = (high[0] - low[0]).hypot(high[1] - low[1]);
    if diagonal > 0.0 {
        median / diagonal
    } else {
        f64::INFINITY
    }
}

/// Node-by-node distance between two grids in source pixels: (mean, max).
pub fn node_distance_px(first: &Grid, second: &Grid, size: (usize, usize)) -> (f64, f64) {
    let scale = [size.1 as f64 / 2.0, size.0 as f64 / 2.0];
    let mut total = 0.0;
    let mut worst = 0.0f64;
    for (a, b) in first.nodes.iter().zip(&second.nodes) {
        let d = ((a[0] - b[0]) * scale[0]).hypot((a[1] - b[1]) * scale[1]);
        total += d;
        worst = worst.max(d);
    }
    (total / first.nodes.len() as f64, worst)
}

/// 1-D linear interpolation taps at a fractional node index, the end value held.
fn taps(index: f64, n: usize) -> (usize, f64) {
    let t = index.clamp(0.0, (n - 1) as f64);
    let left = (t.floor() as usize).min(n - 2);
    (left, t - left as f64)
}

/// Dense backward map (row-major, height x width) of an edge-aligned grid:
/// output pixel centers looked up bilinearly between the nodes.
pub fn dense_map(grid: &Grid, height: usize, width: usize) -> Vec<[f32; 2]> {
    let columns: Vec<(usize, f64)> = (0..width)
        .map(|x| {
            taps(
                (center_position(x, width) + 1.0) / 2.0 * (grid.cols - 1) as f64,
                grid.cols,
            )
        })
        .collect();
    let mut map = vec![[0f32; 2]; height * width];
    map.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (top, wy) = taps(
            (center_position(y, height) + 1.0) / 2.0 * (grid.rows - 1) as f64,
            grid.rows,
        );
        for (x, out) in row.iter_mut().enumerate() {
            let (left, wx) = columns[x];
            let (a, b) = (grid.at(top, left), grid.at(top, left + 1));
            let (c, d) = (grid.at(top + 1, left), grid.at(top + 1, left + 1));
            for axis in 0..2 {
                let upper = a[axis] * (1.0 - wx) + b[axis] * wx;
                let lower = c[axis] * (1.0 - wx) + d[axis] * wx;
                out[axis] = (upper * (1.0 - wy) + lower * wy) as f32;
            }
        }
    });
    map
}

/// The part of a grid between column positions `low` and `high` (-1 to 1),
/// resampled linearly onto as many columns: one page of a spread.
pub fn split_grid(grid: &Grid, low: f64, high: f64) -> Grid {
    let mut nodes = Vec::with_capacity(grid.nodes.len());
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            let position = low + (high - low) * col as f64 / (grid.cols - 1) as f64;
            let t = (position + 1.0) / 2.0 * (grid.cols - 1) as f64;
            let left = (t.floor().max(0.0) as usize).min(grid.cols - 2);
            let weight = t - left as f64;
            let (a, b) = (grid.at(row, left), grid.at(row, left + 1));
            nodes.push([
                a[0] * (1.0 - weight) + b[0] * weight,
                a[1] * (1.0 - weight) + b[1] * weight,
            ]);
        }
    }
    Grid {
        rows: grid.rows,
        cols: grid.cols,
        nodes,
    }
}

// ---------------------------------------------------------------- output size

const SNAP_TOLERANCE: f64 = 0.015;
const MAX_SIDE: f64 = 8192.0;
const MAX_PIXELS: f64 = 16_000_000.0;

fn arc_lengths_of(points: &[P2], rows: usize, cols: usize) -> (Vec<f64>, Vec<f64>) {
    let at = |row: usize, col: usize| points[row * cols + col];
    let row_lengths = (0..rows)
        .map(|row| {
            (1..cols)
                .map(|col| {
                    let (a, b) = (at(row, col - 1), at(row, col));
                    (b[0] - a[0]).hypot(b[1] - a[1])
                })
                .sum()
        })
        .collect();
    let col_lengths = (0..cols)
        .map(|col| {
            (1..rows)
                .map(|row| {
                    let (a, b) = (at(row - 1, col), at(row, col));
                    (b[0] - a[0]).hypot(b[1] - a[1])
                })
                .sum()
        })
        .collect();
    (row_lengths, col_lengths)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn arc_ratio(points: &[P2], rows: usize, cols: usize) -> Option<f64> {
    let (row_lengths, col_lengths) = arc_lengths_of(points, rows, cols);
    let (r, c) = (mean(&row_lengths), mean(&col_lengths));
    (r > 0.0 && c > 0.0).then_some(r / c)
}

/// Solves an 8x8 linear system by Gaussian elimination with partial pivoting.
fn solve8(mut a: [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-14 {
            return None;
        }
        a.swap(col, pivot);
        for row in 0..8 {
            if row != col {
                let factor = a[row][col] / a[col][col];
                for k in col..9 {
                    a[row][k] -= factor * a[col][k];
                }
            }
        }
    }
    let mut x = [0.0; 8];
    for i in 0..8 {
        x[i] = a[i][8] / a[i][i];
    }
    Some(x)
}

/// Page aspect (width / height) with the perspective of a pinhole camera
/// removed: the rectangle behind the corner quad, times the curl the arc
/// lengths show.
fn perspective_aspect(grid: &Grid, size: (usize, usize), focal_px: f64) -> Option<f64> {
    let (height, width) = (size.0 as f64, size.1 as f64);
    let camera: Vec<P2> = grid
        .pixels(size)
        .iter()
        .map(|p| {
            [
                (p[0] - width / 2.0) / focal_px,
                (p[1] - height / 2.0) / focal_px,
            ]
        })
        .collect();
    let at = |row: usize, col: usize| camera[row * grid.cols + col];
    let corners = [
        at(0, 0),
        at(0, grid.cols - 1),
        at(grid.rows - 1, grid.cols - 1),
        at(grid.rows - 1, 0),
    ];
    let edges: Vec<P2> = (0..4)
        .map(|i| {
            [
                corners[(i + 1) % 4][0] - corners[i][0],
                corners[(i + 1) % 4][1] - corners[i][1],
            ]
        })
        .collect();
    let turns: Vec<f64> = (0..4)
        .map(|i| edges[i][0] * edges[(i + 1) % 4][1] - edges[i][1] * edges[(i + 1) % 4][0])
        .collect();
    let scale: f64 = edges.iter().map(|e| e[0] * e[0] + e[1] * e[1]).sum();
    if !(turns.iter().all(|&t| t > 1e-9 * scale) || turns.iter().all(|&t| t < -1e-9 * scale)) {
        return None;
    }
    let mut system = [[0.0; 9]; 8];
    for (i, ((u, v), corner)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .iter()
        .zip(corners)
        .enumerate()
    {
        let (x, y) = (corner[0], corner[1]);
        system[2 * i] = [*u, *v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x, x];
        system[2 * i + 1] = [0.0, 0.0, 0.0, *u, *v, 1.0, -u * y, -v * y, y];
    }
    let h = solve8(system)?;
    let m = [[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]];
    let mut flat = Vec::with_capacity(camera.len());
    let mut positive = 0usize;
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            let (u, v) = (
                col as f64 / (grid.cols - 1) as f64,
                row as f64 / (grid.rows - 1) as f64,
            );
            let depth = m[2][0] * u + m[2][1] * v + m[2][2];
            if depth > 0.0 {
                positive += 1;
            }
            flat.push([
                (m[0][0] * u + m[0][1] * v + m[0][2]) / depth,
                (m[1][0] * u + m[1][1] * v + m[1][2]) / depth,
            ]);
        }
    }
    if positive != 0 && positive != camera.len() {
        return None;
    }
    let column = |j: usize| (m[0][j] * m[0][j] + m[1][j] * m[1][j] + m[2][j] * m[2][j]).sqrt();
    let rect = column(0) / column(1);
    let aspect =
        rect * arc_ratio(&camera, grid.rows, grid.cols)? / arc_ratio(&flat, grid.rows, grid.cols)?;
    (aspect.is_finite() && aspect > 0.0).then_some(aspect)
}

pub fn focal_px_from_35mm(focal_35mm: f64, size: (usize, usize)) -> f64 {
    focal_35mm * (size.1 as f64).hypot(size.0 as f64) / 36f64.hypot(24.0)
}

/// The estimate snapped to a common paper aspect when it is within tolerance.
fn snap_aspect(aspect: f64) -> Option<f64> {
    let standards = [
        1.0 / 2f64.sqrt(), // ISO
        152.0 / 225.0,     // singukpan
        128.0 / 188.0,     // 4x6
        8.5 / 11.0,        // letter
    ];
    let mut best: Option<(f64, f64)> = None;
    for ratio in standards {
        for candidate in [ratio, 1.0 / ratio] {
            let error = (aspect - candidate).abs() / candidate;
            if error <= SNAP_TOLERANCE && best.is_none_or(|(_, least)| error < least) {
                best = Some((candidate, error));
            }
        }
    }
    best.map(|(ratio, _)| ratio)
}

fn round_half_up(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// Output size of one flattened page, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputPlan {
    pub height: usize,
    pub width: usize,
}

/// Output size for a grid: as wide as its longest row in source pixels, with
/// the page aspect from the grid's arc lengths, or from perspective when the
/// focal length is known, snapped to common paper sizes when close.
pub fn plan_output(grid: &Grid, size: (usize, usize), focal_px: Option<f64>) -> Option<OutputPlan> {
    let points = grid.pixels(size);
    let (row_lengths, col_lengths) = arc_lengths_of(&points, grid.rows, grid.cols);
    let longest = row_lengths.iter().cloned().fold(0.0, f64::max);
    if !(longest > 0.0 && mean(&col_lengths) > 0.0) {
        return None;
    }
    let estimate = match focal_px.and_then(|f| perspective_aspect(grid, size, f)) {
        Some(aspect) => aspect,
        None => arc_ratio(&points, grid.rows, grid.cols)?,
    };
    let aspect = snap_aspect(estimate).unwrap_or(estimate);
    if !(aspect.is_finite() && aspect > 0.0) {
        return None;
    }
    let (width, height) = (longest, longest / aspect);
    let limit = (MAX_SIDE / width.max(height))
        .min((MAX_PIXELS / (width * height)).sqrt())
        .min(1.0);
    let width = if limit < 1.0 {
        (width * limit).floor()
    } else {
        round_half_up(width)
    }
    .max(1.0) as usize;
    let height = round_half_up(width as f64 / aspect).max(1.0) as usize;
    Some(OutputPlan { height, width })
}

/// Share of grid cells whose orientation is flipped against the page's (a
/// folded map).
pub fn folded_cell_fraction(grid: &Grid) -> f64 {
    let mut signs = Vec::with_capacity((grid.rows - 1) * (grid.cols - 1));
    for row in 1..grid.rows {
        for col in 1..grid.cols {
            let (a, b, c) = (
                grid.at(row - 1, col - 1),
                grid.at(row - 1, col),
                grid.at(row, col - 1),
            );
            signs.push((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]));
        }
    }
    let positive = signs.iter().filter(|&&s| s > 0.0).count();
    let minority = positive.min(signs.len() - positive);
    minority as f64 / signs.len() as f64
}

// ---------------------------------------------------------------- distance from "do nothing"

/// How far a grid is from the identity map of an image of `size`.
#[derive(Clone, Copy, Debug)]
pub struct Displacement {
    /// 99th percentile of the move, in pixels.
    pub p99_px: f64,
    /// Largest move left after the best affine map (shift, zoom, turn, shear)
    /// is taken out: the part that bends the page. Infinite when no affine
    /// map fits.
    pub bend_max_px: f64,
    pub zoom: f64,
    pub rotation_deg: f64,
}

fn percentile(sorted: &[f64], share: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * share).round() as usize]
}

/// Least-squares affine map from `ideal` to `got`: (matrix, offset, centroid).
fn affine_fit(ideal: &[P2], got: &[P2]) -> Option<([[f64; 2]; 2], P2, P2)> {
    let n = ideal.len() as f64;
    let center = [
        ideal.iter().map(|p| p[0]).sum::<f64>() / n,
        ideal.iter().map(|p| p[1]).sum::<f64>() / n,
    ];
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    let mut cross = [[0.0f64; 2]; 2];
    let mut mean = [0.0f64; 2];
    for (p, g) in ideal.iter().zip(got) {
        let (x, y) = (p[0] - center[0], p[1] - center[1]);
        sxx += x * x;
        sxy += x * y;
        syy += y * y;
        for axis in 0..2 {
            cross[axis][0] += x * g[axis];
            cross[axis][1] += y * g[axis];
            mean[axis] += g[axis] / n;
        }
    }
    let det = sxx * syy - sxy * sxy;
    if det.abs() < 1e-9 {
        return None;
    }
    let mut matrix = [[0.0; 2]; 2];
    for axis in 0..2 {
        matrix[axis][0] = (cross[axis][0] * syy - cross[axis][1] * sxy) / det;
        matrix[axis][1] = (cross[axis][1] * sxx - cross[axis][0] * sxy) / det;
    }
    Some((matrix, mean, center))
}

/// Displacement of an edge-aligned grid from the identity.
pub fn grid_displacement(grid: &Grid, size: (usize, usize)) -> Displacement {
    let (height, width) = (size.0 as f64, size.1 as f64);
    let got = grid.pixels(size);
    let mut ideal = Vec::with_capacity(got.len());
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            ideal.push([
                col as f64 / (grid.cols - 1) as f64 * width,
                row as f64 / (grid.rows - 1) as f64 * height,
            ]);
        }
    }
    let mut total: Vec<f64> = ideal
        .iter()
        .zip(&got)
        .map(|(a, b)| (a[0] - b[0]).hypot(a[1] - b[1]))
        .collect();
    total.sort_by(|a, b| a.total_cmp(b));
    let mut result = Displacement {
        p99_px: percentile(&total, 0.99),
        bend_max_px: f64::INFINITY,
        zoom: f64::INFINITY,
        rotation_deg: f64::INFINITY,
    };
    if !result.p99_px.is_finite() {
        result.p99_px = f64::INFINITY;
    }
    if let Some((m, offset, center)) = affine_fit(&ideal, &got) {
        let bend = ideal
            .iter()
            .zip(&got)
            .map(|(p, g)| {
                let (x, y) = (p[0] - center[0], p[1] - center[1]);
                let fx = m[0][0] * x + m[0][1] * y + offset[0];
                let fy = m[1][0] * x + m[1][1] * y + offset[1];
                (fx - g[0]).hypot(fy - g[1])
            })
            .fold(
                0.0f64,
                |worst, d| if d > worst || d.is_nan() { d } else { worst },
            );
        result.bend_max_px = if bend.is_finite() {
            bend
        } else {
            f64::INFINITY
        };
        result.zoom = (m[0][0] * m[1][1] - m[0][1] * m[1][0]).abs().sqrt();
        result.rotation_deg = m[1][0].atan2(m[0][0]).to_degrees();
    }
    result
}
