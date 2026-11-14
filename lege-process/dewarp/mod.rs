//! Page flattening ("Flatten page"): a photographed book page or two-page
//! spread goes in, one flat image per page comes out. Pages that are already
//! flat (scans) are detected and returned untouched.
//!
//! The Lege-Dewarp model serves every pass. It takes a view of the photo
//! squeezed to 576x384 (portrait) or 384x576 (landscape) and returns the
//! correction to the identity map, a page mask with a gutter heat map, and
//! two scores: `bend` (the page is not flat) and `spread` (two pages).
//!
//! This is a port of the model's standalone tester (`dewarp-tester`, kept
//! with the training photos outside this repository). Every constant in
//! [`Settings`] was tuned or verified with these exact functions; the
//! measurements are in the ledger under `lege-ecosystem.work.integrate-lege-dewarp`.
//!
//! Flattening never fails a page: on a model error, a non-finite or folded
//! grid, or a grid with no extent, the photo is returned as it came.

pub mod geom;
pub mod imgops;

use anyhow::{Context, Result};
use image::RgbImage;
use lege_gpu::vision::{DewarpNet, DewarpOutput};

use geom::{Grid, P2};
use imgops::RgbRef;

/// Margin around the predicted page for the crop pass, per side.
const MARGIN: f64 = 0.05;
/// Long side of the working copy a refinement round looks at.
const REFINE_LONG_SIDE: f64 = 1100.0;
/// A spread is cut at the gutter peak when its mean heat reaches this.
const GUTTER_PEAK: f64 = 0.15;
/// A page pass of a spread is kept when it stays this close to the cut grid
/// (median drift over the page diagonal); beyond it the pass has wandered
/// onto the facing page.
const PAGE_PASS_DRIFT: f64 = 0.06;
/// A final grid with more flipped cells than this is a folded map: the photo
/// is passed through. 252 good pages of the test sets peak at 0.003.
const MAX_FOLDED_CELLS: f64 = 0.05;

/// The tested values of every setting (the tester's defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The page is left untouched when the 99th percentile of the
    /// whole-frame correction is below this share of the photo's diagonal and
    /// the bend score is below 0.5.
    pub gate_share: f64,
    /// The limit (share of the diagonal) for pages whose bend score is under
    /// 0.1: a flat page lying slightly off.
    pub flat_share: f64,
    /// A page that fills the frame is left alone when its measured bend
    /// (largest non-affine displacement, share of the diagonal) is under this.
    pub fill_bend: f64,
    /// The largest turn (degrees) of a page that fills the frame and is left
    /// untouched.
    pub fill_turn: f64,
    /// Extra passes on the flattened page itself, each composed onto its map.
    pub refine: usize,
    /// A refinement is kept only if it moves the page by less than this share
    /// of its diagonal (99th percentile).
    pub refine_limit: f64,
    /// Degrees (across, down) of the polynomial surface fitted to each
    /// page's map; `None` switches smoothing off.
    pub smooth: Option<(usize, usize)>,
    /// 0 decides per photo; 1 treats every photo as one page, 2 as a spread.
    pub force_pages: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gate_share: 0.004,
            flat_share: 0.03,
            fill_bend: 0.012,
            fill_turn: 2.0,
            refine: 3,
            refine_limit: 0.30,
            smooth: Some((5, 3)),
            force_pages: 0,
        }
    }
}

impl Settings {
    /// One pass per page: no refinement, no smoothing. Four model calls per
    /// spread instead of ten and about half the time; measured a little less
    /// straight on hand-held photos.
    pub fn fast() -> Self {
        Self {
            refine: 0,
            smooth: None,
            ..Self::default()
        }
    }
}

/// What the survey decided for one photo.
#[derive(Clone, Debug)]
pub enum PhotoPlan {
    /// The page is flat (or flattening failed): use the photo as it came.
    Untouched,
    /// One grid per output page, left to right.
    Pages(Vec<Grid>),
}

impl PhotoPlan {
    pub fn page_count(&self) -> usize {
        match self {
            PhotoPlan::Untouched => 1,
            PhotoPlan::Pages(grids) => grids.len(),
        }
    }
}

struct Prediction {
    rows: usize,
    cols: usize,
    /// Photo positions each cell reads (already placed in the photo).
    q: Vec<P2>,
    /// Gutter heat sampled along the predicted page, per cell.
    gutter_on_page: Vec<f32>,
    bend: f32,
    spread: f32,
}

/// A crop of the photo in pixel edges (left, top, right, bottom), inside it.
type View = [f64; 4];

/// The resident model and the passes over it.
pub struct Flattener {
    net: DewarpNet,
}

impl Flattener {
    /// Load the model: runtime assets beside the executable (or in
    /// `LEGE_DATA_DIR`) override the embedded files.
    pub fn load() -> Result<Self> {
        let asset = |stem: &str| {
            crate::runtime_asset_path_if_exists(&format!("{stem}.onnx"))
                .or_else(|| crate::runtime_asset_path_if_exists(&format!("{stem}.fp16.onnx")))
        };
        if let (Some(portrait), Some(landscape)) =
            (asset("lege-dewarp-576x384"), asset("lege-dewarp-384x576"))
        {
            return Ok(Self {
                net: DewarpNet::from_model_paths(&portrait, &landscape)?,
            });
        }
        #[cfg(feature = "dewarp")]
        {
            Ok(Self {
                net: DewarpNet::from_model_bytes(
                    crate::EMBEDDED_DEWARP_PORTRAIT,
                    crate::EMBEDDED_DEWARP_LANDSCAPE,
                )?,
            })
        }
        #[cfg(not(feature = "dewarp"))]
        {
            anyhow::bail!(
                "this build embeds no flatten model; put lege-dewarp-576x384.onnx and \
                 lege-dewarp-384x576.onnx beside the executable"
            )
        }
    }

    /// The process-wide flattener, or why there is none (no hardware GPU, no
    /// model). The first call loads and compiles the model.
    pub fn shared() -> std::result::Result<&'static Flattener, &'static str> {
        static SHARED: std::sync::OnceLock<std::result::Result<Flattener, String>> =
            std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| Flattener::load().map_err(|error| format!("{error:#}")))
            .as_ref()
            .map_err(String::as_str)
    }

    fn predict(&self, photo: RgbRef<'_>, view: View) -> Result<Prediction> {
        let (vw, vh) = (view[2] - view[0], view[3] - view[1]);
        let wide = vw > vh;
        let (height, width) = DewarpNet::input_size(wide);
        let whole = view[0] == 0.0
            && view[1] == 0.0
            && view[2] as usize == photo.width
            && view[3] as usize == photo.height;
        let input = if whole {
            imgops::resize_antialias_chw(photo, height, width)
        } else {
            let crop = photo.crop(
                view[0] as usize,
                view[1] as usize,
                view[2] as usize,
                view[3] as usize,
            );
            imgops::resize_antialias_chw(RgbRef::of(&crop), height, width)
        };
        let DewarpOutput {
            rows,
            cols,
            flow,
            maps,
            scores,
        } = self.net.run(wide, input)?;
        let plane = rows * cols;
        let (pw, ph) = (photo.width as f64, photo.height as f64);
        let mut q = Vec::with_capacity(plane);
        let mut gutter_on_page = Vec::with_capacity(plane);
        for i in 0..rows {
            for j in 0..cols {
                let k = i * cols + j;
                // View positions, then photo positions.
                let vx = geom::center_position(j, cols) + flow[k] as f64;
                let vy = geom::center_position(i, rows) + flow[plane + k] as f64;
                q.push([
                    (view[0] + (vx + 1.0) / 2.0 * vw) / pw * 2.0 - 1.0,
                    (view[1] + (vy + 1.0) / 2.0 * vh) / ph * 2.0 - 1.0,
                ]);
                gutter_on_page.push(sample_map(&maps[plane..], rows, cols, vx, vy));
            }
        }
        Ok(Prediction {
            rows,
            cols,
            q,
            gutter_on_page,
            bend: scores[0],
            spread: scores[1],
        })
    }

    /// Decide what to do with one decoded, upright photo at its native size.
    /// Never fails: an error leaves the photo untouched and is logged.
    pub fn survey(
        &self,
        photo: &RgbImage,
        focal_length_35mm: Option<f64>,
        settings: &Settings,
    ) -> PhotoPlan {
        if photo.width() < 16 || photo.height() < 16 {
            return PhotoPlan::Untouched;
        }
        match self.scan(RgbRef::of(photo), focal_length_35mm, settings) {
            Ok(plan) => plan,
            Err(error) => {
                log::warn!("Flatten page: photo left as it came: {error:#}");
                PhotoPlan::Untouched
            }
        }
    }

    fn scan(
        &self,
        photo: RgbRef<'_>,
        focal_length_35mm: Option<f64>,
        settings: &Settings,
    ) -> Result<PhotoPlan> {
        let size = (photo.height, photo.width);
        let whole: View = [0.0, 0.0, photo.width as f64, photo.height as f64];
        let first = self.predict(photo, whole)?;
        let grid0 = centred_to_grid(&first);
        // How far the whole-frame answer is from "do nothing".
        let moved = geom::grid_displacement(&grid0, size);
        let diagonal = (photo.width as f64).hypot(photo.height as f64);
        // A page the model itself calls flat (bend near 0) is left alone
        // unless it would be moved a long way (it lies turned or in
        // perspective); otherwise the strict limit.
        let limit = if first.bend < 0.1 {
            settings.flat_share.max(settings.gate_share)
        } else {
            settings.gate_share
        } * diagonal;
        // Independent of the bend score: a page that already fills the frame
        // (hardly any zoom or turn) and whose measured bend is small is a
        // scan, perhaps with a dark surround; correcting it would only trim it.
        let fills_frame = moved.bend_max_px < settings.fill_bend * diagonal
            && (moved.zoom - 1.0).abs() < 0.08
            && moved.rotation_deg.abs() < settings.fill_turn;
        let untouched = settings.force_pages == 0
            && ((first.bend < 0.5 && moved.p99_px < limit) || fills_frame);
        if untouched {
            return Ok(PhotoPlan::Untouched);
        }

        // The page's own crop.
        let view1 = view_of(&grid0, size, MARGIN);
        let second = self.predict(photo, view1)?;
        let grid1 = centred_to_grid(&second);
        let spread = match settings.force_pages {
            1 => false,
            2 => true,
            _ => second.spread >= 0.5,
        };
        let mut grids = Vec::new();
        if spread {
            let (column, peak) = gutter_column(&second);
            let cut = if peak >= GUTTER_PEAK {
                (column + 0.5) / second.cols as f64 * 2.0 - 1.0
            } else {
                0.0
            };
            for (low, high) in [(-1.0, cut), (cut, 1.0)] {
                let half = geom::split_grid(&grid1, low, high);
                let view = view_of(&half, size, MARGIN);
                let page = self.predict(photo, view)?;
                let refined = centred_to_grid(&page);
                let drift = geom::median_drift(&resample(&half, &refined), &refined, size);
                // A page pass that wanders onto the facing page moves a whole side.
                let keep = drift <= PAGE_PASS_DRIFT && refined.is_finite();
                grids.push(if keep { refined } else { half });
            }
        } else {
            grids.push(grid1);
        }

        let focal_px = focal_length_35mm.map(|f| geom::focal_px_from_35mm(f, size));
        // Refinement: flatten the page, ask the model what is still bent in
        // the result, and compose that correction onto the page's map. The
        // working copies are model input only.
        for _ in 0..settings.refine {
            for grid in grids.iter_mut() {
                let plan = geom::plan_output(grid, size, focal_px)
                    .context("the page grid has no extent")?;
                let scale = (REFINE_LONG_SIDE / plan.height.max(plan.width) as f64).min(1.0);
                let (h, w) = (
                    ((plan.height as f64 * scale) as usize).max(16),
                    ((plan.width as f64 * scale) as usize).max(16),
                );
                let map = geom::dense_map(grid, h, w);
                let flat = imgops::render(photo, &map, h, w, [255, 255, 255]);
                let again = self.predict(RgbRef::of(&flat), [0.0, 0.0, w as f64, h as f64])?;
                let correction = centred_to_grid(&again);
                let share = geom::grid_displacement(&correction, (h, w)).p99_px
                    / (h as f64).hypot(w as f64);
                if correction.is_finite() && share < settings.refine_limit {
                    let nodes = correction
                        .nodes
                        .iter()
                        .map(|node| sample_grid(grid, node[0], node[1]))
                        .collect();
                    *grid = Grid {
                        rows: correction.rows,
                        cols: correction.cols,
                        nodes,
                    };
                }
            }
        }
        if let Some((across, down)) = settings.smooth {
            if spread || settings.force_pages == 1 || first.spread < 0.5 {
                for grid in grids.iter_mut() {
                    *grid = smooth_grid(grid, across, down);
                }
            }
        }
        for grid in &grids {
            anyhow::ensure!(grid.is_finite(), "the page grid is not finite");
            geom::plan_output(grid, size, focal_px).context("the page grid has no extent")?;
            let folded = geom::folded_cell_fraction(grid);
            anyhow::ensure!(
                folded <= MAX_FOLDED_CELLS,
                "the page map is folded ({:.1} % of its cells)",
                folded * 100.0
            );
        }
        Ok(PhotoPlan::Pages(grids))
    }
}

/// One page rendered from the photo through its grid: sampled exactly once,
/// bilinear, white where the map leaves the photo. `None` when the grid has
/// no extent (the survey rules that out).
pub fn render_page(
    photo: &RgbImage,
    grid: &Grid,
    focal_length_35mm: Option<f64>,
) -> Option<RgbImage> {
    let size = (photo.height() as usize, photo.width() as usize);
    let focal_px = focal_length_35mm.map(|f| geom::focal_px_from_35mm(f, size));
    let plan = geom::plan_output(grid, size, focal_px)?;
    let map = geom::dense_map(grid, plan.height, plan.width);
    Some(imgops::render(
        RgbRef::of(photo),
        &map,
        plan.height,
        plan.width,
        [255, 255, 255],
    ))
}

/// Bilinear read of a (rows, cols) map at view positions (x, y); 0 outside.
fn sample_map(map: &[f32], rows: usize, cols: usize, x: f64, y: f64) -> f32 {
    let fx = (x + 1.0) / 2.0 * cols as f64 - 0.5;
    let fy = (y + 1.0) / 2.0 * rows as f64 - 0.5;
    // Written to reject NaN as well as positions off the map.
    if !(fx >= -0.5 && fy >= -0.5 && fx <= cols as f64 - 0.5 && fy <= rows as f64 - 0.5) {
        return 0.0;
    }
    let (x0, y0) = (
        fx.floor().clamp(0.0, (cols - 1) as f64),
        fy.floor().clamp(0.0, (rows - 1) as f64),
    );
    let (x1, y1) = (
        (x0 + 1.0).min((cols - 1) as f64),
        (y0 + 1.0).min((rows - 1) as f64),
    );
    let (wx, wy) = (
        (fx - x0).clamp(0.0, 1.0) as f32,
        (fy - y0).clamp(0.0, 1.0) as f32,
    );
    let at = |yy: f64, xx: f64| map[yy as usize * cols + xx as usize];
    (at(y0, x0) * (1.0 - wx) + at(y0, x1) * wx) * (1.0 - wy)
        + (at(y1, x0) * (1.0 - wx) + at(y1, x1) * wx) * wy
}

fn bilinear(a: P2, b: P2, c: P2, d: P2, wx: f64, wy: f64) -> P2 {
    let mut node = [0.0; 2];
    for axis in 0..2 {
        node[axis] = (a[axis] * (1.0 - wx) + b[axis] * wx) * (1.0 - wy)
            + (c[axis] * (1.0 - wx) + d[axis] * wx) * wy;
    }
    node
}

/// The cell-centred map as an edge-aligned grid of (rows + 1, cols + 1)
/// nodes: bilinear between cell centres, linearly extended over the outer
/// half cell.
fn centred_to_grid(prediction: &Prediction) -> Grid {
    let (rows, cols) = (prediction.rows, prediction.cols);
    let taps = |node: usize, n: usize| -> (usize, f64) {
        let position = node as f64 - 0.5;
        let left = (position.floor().max(0.0) as usize).min(n - 2);
        (left, position - left as f64)
    };
    let at = |r: usize, c: usize| prediction.q[r * cols + c];
    let mut nodes = Vec::with_capacity((rows + 1) * (cols + 1));
    for i in 0..=rows {
        let (top, wy) = taps(i, rows);
        for j in 0..=cols {
            let (left, wx) = taps(j, cols);
            nodes.push(bilinear(
                at(top, left),
                at(top, left + 1),
                at(top + 1, left),
                at(top + 1, left + 1),
                wx,
                wy,
            ));
        }
    }
    Grid {
        rows: rows + 1,
        cols: cols + 1,
        nodes,
    }
}

/// `grid` replaced by the least-squares polynomial surface of degrees
/// (`across`, `down`) through its nodes (Chebyshev basis, Huber reweighting).
/// A single page is smooth, so the fit removes the model's cell-to-cell wobble.
fn smooth_grid(grid: &Grid, across: usize, down: usize) -> Grid {
    let (rows, cols) = (grid.rows, grid.cols);
    let (nx, ny) = (across + 1, down + 1);
    let n = nx * ny;
    let basis = |t: f64, count: usize| -> Vec<f64> {
        let mut values = vec![1.0; count];
        if count > 1 {
            values[1] = t;
        }
        for k in 2..count {
            values[k] = 2.0 * t * values[k - 1] - values[k - 2];
        }
        values
    };
    let bx: Vec<Vec<f64>> = (0..cols)
        .map(|j| basis(j as f64 / (cols - 1) as f64 * 2.0 - 1.0, nx))
        .collect();
    let by: Vec<Vec<f64>> = (0..rows)
        .map(|i| basis(i as f64 / (rows - 1) as f64 * 2.0 - 1.0, ny))
        .collect();
    let row_of = |i: usize, j: usize| -> Vec<f64> {
        let mut row = Vec::with_capacity(n);
        for a in 0..ny {
            for b in 0..nx {
                row.push(by[i][a] * bx[j][b]);
            }
        }
        row
    };
    let mut weights = vec![1.0f64; rows * cols];
    let mut fitted = grid.nodes.clone();
    for _ in 0..3 {
        let mut normal = vec![0.0f64; n * n];
        let mut right = vec![[0.0f64; 2]; n];
        for i in 0..rows {
            for j in 0..cols {
                let (w, row, node) = (weights[i * cols + j], row_of(i, j), grid.at(i, j));
                for a in 0..n {
                    for b in 0..n {
                        normal[a * n + b] += w * row[a] * row[b];
                    }
                    right[a][0] += w * row[a] * node[0];
                    right[a][1] += w * row[a] * node[1];
                }
            }
        }
        // Gaussian elimination with partial pivoting; the basis is well conditioned.
        for k in 0..n {
            let pivot = (k..n)
                .max_by(|&a, &b| normal[a * n + k].abs().total_cmp(&normal[b * n + k].abs()))
                .unwrap();
            if normal[pivot * n + k].abs() < 1e-12 {
                return grid.clone();
            }
            if pivot != k {
                for c in 0..n {
                    normal.swap(k * n + c, pivot * n + c);
                }
                right.swap(k, pivot);
            }
            for r in k + 1..n {
                let factor = normal[r * n + k] / normal[k * n + k];
                if factor != 0.0 {
                    for c in k..n {
                        normal[r * n + c] -= factor * normal[k * n + c];
                    }
                    right[r][0] -= factor * right[k][0];
                    right[r][1] -= factor * right[k][1];
                }
            }
        }
        let mut solution = vec![[0.0f64; 2]; n];
        for k in (0..n).rev() {
            for axis in 0..2 {
                let mut value = right[k][axis];
                for c in k + 1..n {
                    value -= normal[k * n + c] * solution[c][axis];
                }
                solution[k][axis] = value / normal[k * n + k];
            }
        }
        let mut residuals = Vec::with_capacity(rows * cols);
        for i in 0..rows {
            for j in 0..cols {
                let row = row_of(i, j);
                let mut node = [0.0; 2];
                for a in 0..n {
                    node[0] += row[a] * solution[a][0];
                    node[1] += row[a] * solution[a][1];
                }
                let actual = grid.at(i, j);
                residuals.push((node[0] - actual[0]).hypot(node[1] - actual[1]));
                fitted[i * cols + j] = node;
            }
        }
        let mut sorted = residuals.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let scale = (1.5 * sorted[sorted.len() / 2]).max(1e-6);
        for (weight, residual) in weights.iter_mut().zip(&residuals) {
            *weight = if *residual <= scale {
                1.0
            } else {
                scale / residual
            };
        }
    }
    Grid {
        rows,
        cols,
        nodes: fitted,
    }
}

/// Bilinear read of an edge-aligned grid at flat-page positions (x, y), where
/// -1 and +1 are its outer nodes; linear continuation outside.
fn sample_grid(grid: &Grid, x: f64, y: f64) -> P2 {
    let fx = (x + 1.0) / 2.0 * (grid.cols - 1) as f64;
    let fy = (y + 1.0) / 2.0 * (grid.rows - 1) as f64;
    let left = (fx.floor().max(0.0) as usize).min(grid.cols - 2);
    let top = (fy.floor().max(0.0) as usize).min(grid.rows - 2);
    bilinear(
        grid.at(top, left),
        grid.at(top, left + 1),
        grid.at(top + 1, left),
        grid.at(top + 1, left + 1),
        fx - left as f64,
        fy - top as f64,
    )
}

/// `grid` resampled to the node count of `like` (the two passes may differ in shape).
fn resample(grid: &Grid, like: &Grid) -> Grid {
    if grid.rows == like.rows && grid.cols == like.cols {
        return grid.clone();
    }
    let mut nodes = Vec::with_capacity(like.rows * like.cols);
    for i in 0..like.rows {
        let fy = i as f64 * (grid.rows - 1) as f64 / (like.rows - 1) as f64;
        let top = (fy.floor() as usize).min(grid.rows - 2);
        let wy = fy - top as f64;
        for j in 0..like.cols {
            let fx = j as f64 * (grid.cols - 1) as f64 / (like.cols - 1) as f64;
            let left = (fx.floor() as usize).min(grid.cols - 2);
            nodes.push(bilinear(
                grid.at(top, left),
                grid.at(top, left + 1),
                grid.at(top + 1, left),
                grid.at(top + 1, left + 1),
                fx - left as f64,
                wy,
            ));
        }
    }
    Grid {
        rows: like.rows,
        cols: like.cols,
        nodes,
    }
}

/// The bounds of a grid's outline plus a margin, clipped to the photo, in
/// whole pixels.
fn view_of(grid: &Grid, size: (usize, usize), margin: f64) -> View {
    let (low, high) = geom::bounds_of(&geom::outline_points(grid, size));
    let (w, h) = (high[0] - low[0], high[1] - low[1]);
    let left = (low[0] - margin * w)
        .floor()
        .clamp(0.0, size.1 as f64 - 2.0);
    let top = (low[1] - margin * h)
        .floor()
        .clamp(0.0, size.0 as f64 - 2.0);
    let right = (high[0] + margin * w)
        .ceil()
        .clamp(left + 2.0, size.1 as f64);
    let bottom = (high[1] + margin * h)
        .ceil()
        .clamp(top + 2.0, size.0 as f64);
    [left, top, right, bottom]
}

/// The page column where the gutter heat, read along the predicted page,
/// peaks, within the middle of the spread; and the peak's mean heat.
fn gutter_column(prediction: &Prediction) -> (f64, f64) {
    let (rows, cols) = (prediction.rows, prediction.cols);
    let mut profile = vec![0f64; cols];
    for i in 0..rows {
        for j in 0..cols {
            profile[j] += prediction.gutter_on_page[i * cols + j] as f64 / rows as f64;
        }
    }
    let (low, high) = ((0.3 * cols as f64) as usize, (0.7 * cols as f64) as usize);
    let mut best = low;
    for j in low..high {
        if profile[j] > profile[best] {
            best = j;
        }
    }
    // Sub-cell position from the peak's neighbours.
    let (a, b, c) = (
        profile[best.saturating_sub(1)],
        profile[best],
        profile[(best + 1).min(cols - 1)],
    );
    let offset = if (a - 2.0 * b + c).abs() > 1e-9 {
        0.5 * (a - c) / (a - 2.0 * b + c)
    } else {
        0.0
    };
    (best as f64 + offset.clamp(-0.5, 0.5), profile[best])
}

/// Decode a photo with its EXIF orientation applied, and read the 35 mm
/// focal length from the same block. The model expects upright pages, and a
/// phone or camera stores a turned photo as a flag, not as turned pixels.
pub fn decode_upright<R: std::io::BufRead + std::io::Seek>(
    reader: image::ImageReader<R>,
) -> Result<(RgbImage, Option<f64>)> {
    use image::ImageDecoder;
    let mut decoder = reader
        .with_guessed_format()?
        .into_decoder()
        .map_err(anyhow::Error::msg)?;
    let orientation = decoder.orientation().ok();
    let focal_length_35mm = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .and_then(|raw| focal_length_35mm_from_exif(&raw));
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(anyhow::Error::msg)?;
    if let Some(orientation) = orientation {
        decoded.apply_orientation(orientation);
    }
    Ok((decoded.into_rgb8(), focal_length_35mm))
}

/// The 35 mm equivalent focal length from a raw EXIF (TIFF) block, for the
/// aspect estimate. `None` when the tag is absent or the block is malformed.
pub fn focal_length_35mm_from_exif(exif: &[u8]) -> Option<f64> {
    const EXIF_IFD: u16 = 0x8769;
    const FOCAL_LENGTH_IN_35MM_FILM: u16 = 0xA405;
    let big = match exif.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = exif.get(at..at + 2)?.try_into().ok()?;
        Some(if big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = exif.get(at..at + 4)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    // (type, first value) of `tag` in the directory at `ifd`.
    let find = |ifd: usize, tag: u16| -> Option<(u16, usize)> {
        let count = u16_at(ifd)? as usize;
        (0..count)
            .map(|i| ifd + 2 + i * 12)
            .find(|&entry| u16_at(entry) == Some(tag))
            .and_then(|entry| Some((u16_at(entry + 2)?, entry + 8)))
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd0 = u32_at(4)? as usize;
    let (_, pointer) = find(ifd0, EXIF_IFD)?;
    let exif_ifd = u32_at(pointer)? as usize;
    let (kind, value) = find(exif_ifd, FOCAL_LENGTH_IN_35MM_FILM)?;
    let focal = match kind {
        3 => u16_at(value)? as u32, // SHORT
        4 => u32_at(value)?,        // LONG
        _ => return None,
    };
    (focal > 0).then_some(focal as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_prediction(rows: usize, cols: usize) -> Prediction {
        let mut q = Vec::new();
        for i in 0..rows {
            for j in 0..cols {
                q.push([
                    geom::center_position(j, cols),
                    geom::center_position(i, rows),
                ]);
            }
        }
        Prediction {
            rows,
            cols,
            q,
            gutter_on_page: vec![0.0; rows * cols],
            bend: 0.0,
            spread: 0.0,
        }
    }

    /// A zero correction is the identity map: its edge-aligned grid has its
    /// corners on the image corners and moves nothing.
    #[test]
    fn a_zero_correction_is_the_identity_grid() {
        let grid = centred_to_grid(&identity_prediction(72, 48));
        assert_eq!((grid.rows, grid.cols), (73, 49));
        for (node, corner) in [
            (grid.at(0, 0), [-1.0, -1.0]),
            (grid.at(0, 48), [1.0, -1.0]),
            (grid.at(72, 0), [-1.0, 1.0]),
            (grid.at(72, 48), [1.0, 1.0]),
        ] {
            assert!((node[0] - corner[0]).abs() < 1e-9 && (node[1] - corner[1]).abs() < 1e-9);
        }
        let moved = geom::grid_displacement(&grid, (3200, 2000));
        assert!(moved.p99_px < 1e-6 && moved.bend_max_px < 1e-6);
        assert!((moved.zoom - 1.0).abs() < 1e-9 && moved.rotation_deg.abs() < 1e-9);
        // The identity plans the photo's own size (an aspect that is not near
        // a paper format, which would snap).
        let plan = geom::plan_output(&grid, (3200, 2000), None).unwrap();
        assert_eq!((plan.height, plan.width), (3200, 2000));
        assert_eq!(geom::folded_cell_fraction(&grid), 0.0);
    }

    #[test]
    fn the_identity_grid_renders_the_photo_unchanged() {
        let photo = RgbImage::from_fn(60, 96, |x, y| {
            image::Rgb([(x * 3) as u8, (y * 2) as u8, ((x + y) % 256) as u8])
        });
        let grid = centred_to_grid(&identity_prediction(12, 8));
        let page = render_page(&photo, &grid, None).unwrap();
        assert_eq!(page.dimensions(), photo.dimensions());
        assert_eq!(page.as_raw(), photo.as_raw());
    }

    /// Halving a grid gives two pages that meet at the cut; smoothing a
    /// smooth grid leaves it where it was.
    #[test]
    fn a_spread_splits_at_the_cut_and_smoothing_keeps_a_smooth_grid() {
        let grid = centred_to_grid(&identity_prediction(48, 72));
        let (left, right) = (
            geom::split_grid(&grid, -1.0, 0.0),
            geom::split_grid(&grid, 0.0, 1.0),
        );
        assert!((left.at(10, left.cols - 1)[0] - 0.0).abs() < 1e-9);
        assert!((right.at(10, 0)[0] - 0.0).abs() < 1e-9);
        assert!((right.at(10, right.cols - 1)[0] - 1.0).abs() < 1e-9);
        let smoothed = smooth_grid(&left, 5, 3);
        let (mean, worst) = geom::node_distance_px(&left, &smoothed, (2000, 3000));
        assert!(mean < 1e-6 && worst < 1e-5, "{mean} {worst}");
    }

    #[test]
    fn the_focal_length_is_read_from_a_little_endian_exif_block() {
        // IFD0 at 8 with one entry (ExifIFD -> 26); the Exif IFD holds
        // FocalLengthIn35mmFilm = 35 as a SHORT.
        let mut exif = Vec::new();
        exif.extend_from_slice(b"II");
        exif.extend_from_slice(&42u16.to_le_bytes());
        exif.extend_from_slice(&8u32.to_le_bytes());
        exif.extend_from_slice(&1u16.to_le_bytes());
        exif.extend_from_slice(&0x8769u16.to_le_bytes());
        exif.extend_from_slice(&4u16.to_le_bytes());
        exif.extend_from_slice(&1u32.to_le_bytes());
        exif.extend_from_slice(&26u32.to_le_bytes());
        exif.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(exif.len(), 26);
        exif.extend_from_slice(&1u16.to_le_bytes());
        exif.extend_from_slice(&0xA405u16.to_le_bytes());
        exif.extend_from_slice(&3u16.to_le_bytes());
        exif.extend_from_slice(&1u32.to_le_bytes());
        exif.extend_from_slice(&35u16.to_le_bytes());
        exif.extend_from_slice(&[0, 0]);
        exif.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(focal_length_35mm_from_exif(&exif), Some(35.0));
        assert_eq!(focal_length_35mm_from_exif(b"II*"), None);
        assert_eq!(focal_length_35mm_from_exif(&exif[..20]), None);
    }

    /// The port computes the tester's grids. Point `LEGE_FLATTEN_PHOTOS` at a
    /// folder of photos and `LEGE_FLATTEN_RECORDS` at the folder of per-photo
    /// JSON records the tester wrote for them (`dewarp-tester own --out DIR`),
    /// on a machine with a GPU.
    #[test]
    #[ignore = "needs the photo sets, the tester's records and a GPU"]
    fn the_grids_match_the_tester_on_real_photos() {
        let photos = std::path::PathBuf::from(
            std::env::var("LEGE_FLATTEN_PHOTOS").expect("LEGE_FLATTEN_PHOTOS"),
        );
        let records = std::path::PathBuf::from(
            std::env::var("LEGE_FLATTEN_RECORDS").expect("LEGE_FLATTEN_RECORDS"),
        );
        let flattener = Flattener::shared().expect("the flatten model loads on a GPU");
        let settings = Settings::default();
        let (mut compared, mut pages, mut total, mut worst) = (0usize, 0usize, 0.0f64, 0.0f64);
        let mut record_paths: Vec<_> = std::fs::read_dir(&records)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        record_paths.sort();
        for record_path in record_paths {
            let stem = record_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let Some(photo_path) = ["JPG", "jpg", "jpeg", "png", "tif"]
                .iter()
                .map(|ext| photos.join(format!("{stem}.{ext}")))
                .find(|path| path.is_file())
            else {
                continue;
            };
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
            let (photo, focal) =
                decode_upright(image::ImageReader::open(&photo_path).unwrap()).unwrap();
            let size = (photo.height() as usize, photo.width() as usize);
            let plan = flattener.survey(&photo, focal, &settings);
            let expected_untouched = record["gate"]["untouched"].as_bool().unwrap();
            let expected_pages = record["pages"].as_array().unwrap();
            match &plan {
                PhotoPlan::Untouched => {
                    assert!(
                        expected_untouched,
                        "{stem}: the tester flattened this photo"
                    )
                }
                PhotoPlan::Pages(grids) => {
                    assert!(
                        !expected_untouched,
                        "{stem}: the tester left this photo alone"
                    );
                    assert_eq!(grids.len(), expected_pages.len(), "{stem}: page count");
                    for (grid, page) in grids.iter().zip(expected_pages) {
                        let rows: Vec<Vec<P2>> =
                            serde_json::from_value(page["grid"].clone()).unwrap();
                        let expected = Grid {
                            rows: rows.len(),
                            cols: rows[0].len(),
                            nodes: rows.into_iter().flatten().collect(),
                        };
                        assert_eq!((grid.rows, grid.cols), (expected.rows, expected.cols));
                        let (mean, max) = geom::node_distance_px(grid, &expected, size);
                        total += mean;
                        worst = worst.max(max);
                        pages += 1;
                        let plan = geom::plan_output(
                            grid,
                            size,
                            focal.map(|f| geom::focal_px_from_35mm(f, size)),
                        )
                        .unwrap();
                        assert_eq!(
                            (plan.height as u64, plan.width as u64),
                            (
                                page["output"]["height"].as_u64().unwrap(),
                                page["output"]["width"].as_u64().unwrap()
                            ),
                            "{stem}: output size"
                        );
                    }
                }
            }
            compared += 1;
        }
        assert!(compared > 0, "no photo matched a record");
        let mean = if pages > 0 { total / pages as f64 } else { 0.0 };
        eprintln!(
            "FLATTEN PARITY: {compared} photos, {pages} pages, mean node distance {mean:.4} px, worst node {worst:.3} px"
        );
        assert!(mean < 1.0, "mean node distance {mean} px");
    }
}
