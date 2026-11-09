//! Page and overlay rendering.
//!
//! The page is resampled into a canvas-sized raster that is cached between
//! frames: it only changes when the page, zoom, pan or canvas size changes.
//! A box being dragged costs one row copy of that raster plus a few outline
//! strokes, not a resample of the page.
//!
//! Sampling: below 1:1 the nearest mip level at or above the display size is
//! sampled bilinearly (no aliasing on text when zoomed out); from 1:1 to 2:1
//! bilinear on the original pixels; from 2:1 up, nearest neighbour so every
//! original pixel is visible as a crisp square. Outlines are always one
//! device pixel wide, whatever the zoom.

use liminalkit_raster::{Painter, Rect};

use crate::annotation::geometry::Box2;
use crate::image_io::decode::{Level, PageImage};

use super::viewport::ViewTransform;

/// Background around the page.
pub const CANVAS_BG: u32 = 0x0024_272b;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RasterKey {
    pub image_id: usize,
    pub origin_x: f64,
    pub origin_y: f64,
    pub scale: f64,
    pub w: u32,
    pub h: u32,
    pub x: i32,
    pub y: i32,
}

/// The cached page raster for the canvas rectangle.
#[derive(Default)]
pub struct PageRaster {
    pub px: Vec<u32>,
    pub w: u32,
    pub h: u32,
    pub key: Option<RasterKey>,
}

#[inline]
fn lerp_px(a: u32, b: u32, w: u32) -> u32 {
    let iw = 256 - w;
    let rb = (((a & 0x00ff_00ff) * iw + (b & 0x00ff_00ff) * w) >> 8) & 0x00ff_00ff;
    let g = (((a & 0x0000_ff00) * iw + (b & 0x0000_ff00) * w) >> 8) & 0x0000_ff00;
    rb | g
}

#[derive(Clone, Copy)]
struct Tap {
    i0: u32,
    i1: u32,
    w: u32,
    valid: bool,
}

#[allow(clippy::too_many_arguments)]
fn taps(
    n: u32,
    screen0: f64,
    origin: f64,
    scale: f64,
    level_div: f64,
    level_n: u32,
    image_n: f64,
    nearest: bool,
) -> Vec<Tap> {
    (0..n)
        .map(|i| {
            let s = screen0 + i as f64 + 0.5;
            let u = (s - origin) / scale; // original-image coordinate
            let valid = u >= 0.0 && u < image_n;
            if nearest {
                let idx = (u / level_div).floor().clamp(0.0, (level_n - 1) as f64) as u32;
                return Tap {
                    i0: idx,
                    i1: idx,
                    w: 0,
                    valid,
                };
            }
            let f = u / level_div - 0.5;
            let f0 = f.floor();
            let w = ((f - f0) * 256.0).round() as u32;
            let max = (level_n - 1) as i64;
            let i0 = (f0 as i64).clamp(0, max) as u32;
            let i1 = (f0 as i64 + 1).clamp(0, max) as u32;
            Tap {
                i0,
                i1,
                w: w.min(256),
                valid,
            }
        })
        .collect()
}

fn choose_level(img: &PageImage, view: &ViewTransform) -> (usize, bool) {
    let scale = view.scale;
    if scale >= 2.0 {
        return (0, true);
    }
    // Exactly 1:1 on whole pixels: a straight copy (bilinear would only
    // reproduce the same pixels, more slowly).
    if scale == 1.0 && view.origin.x.fract() == 0.0 && view.origin.y.fract() == 0.0 {
        return (0, true);
    }
    if scale >= 1.0 {
        return (0, false);
    }
    let l = (1.0 / scale).log2().floor() as usize;
    (l.min(img.levels.len() - 1), false)
}

/// The sampling plan for one view: taps per column and row, and the
/// contiguous column range that falls on the page.
struct Plan {
    xt: Vec<Tap>,
    yt: Vec<Tap>,
    x_valid: (usize, usize),
    nearest: bool,
    /// Nearest sampling where consecutive columns read consecutive source
    /// pixels (exactly 100%): rows are straight copies.
    contiguous: bool,
    level: usize,
}

fn plan(img: &PageImage, view: &ViewTransform, x: i32, y: i32, w: u32, h: u32) -> Plan {
    let (level, nearest) = choose_level(img, view);
    let lvl = &img.levels[level];
    let div = img.width() as f64 / lvl.w as f64;
    let divy = img.height() as f64 / lvl.h as f64;
    let xt = taps(
        w,
        x as f64,
        view.origin.x,
        view.scale,
        div,
        lvl.w,
        img.width() as f64,
        nearest,
    );
    let yt = taps(
        h,
        y as f64,
        view.origin.y,
        view.scale,
        divy,
        lvl.h,
        img.height() as f64,
        nearest,
    );
    let xa = xt.iter().position(|t| t.valid).unwrap_or(xt.len());
    let xb = xt.iter().rposition(|t| t.valid).map_or(xa, |i| i + 1);
    let contiguous = nearest && xt[xa..xb].windows(2).all(|p| p[1].i0 == p[0].i0 + 1);
    Plan {
        xt,
        yt,
        x_valid: (xa, xb),
        nearest,
        contiguous,
        level,
    }
}

/// Source rows resampled horizontally, two at a time.
///
/// Bilinear output is a horizontal blend of two source rows followed by a
/// vertical blend of the results. Consecutive output rows read the same or
/// adjacent source rows, so each source row is resampled once and reused
/// instead of once per output row that touches it.
struct HRows {
    id: [usize; 2],
    buf: [Vec<u32>; 2],
}

impl HRows {
    fn new(n: usize) -> HRows {
        HRows {
            id: [usize::MAX; 2],
            buf: [vec![0; n], vec![0; n]],
        }
    }

    /// The slot holding source row `row` resampled over `xt`, never evicting
    /// slot `keep`. Rows are requested in non-decreasing order, so the
    /// smaller id is the stale one.
    fn get(&mut self, row: usize, keep: Option<usize>, lvl: &Level, xt: &[Tap]) -> usize {
        if let Some(slot) = self.id.iter().position(|&r| r == row) {
            return slot;
        }
        let slot = match keep {
            Some(k) => 1 - k,
            None => usize::from(self.id[0] > self.id[1]),
        };
        let lw = lvl.w as usize;
        let src = &lvl.px[row * lw..row * lw + lw];
        row_blend_x(&mut self.buf[slot], src, xt);
        self.id[slot] = row;
        slot
    }
}

// Bilinear row kernels, kept out of line so each loop is compiled on its own.

/// Horizontal blend of one source row into `out`.
#[inline(never)]
fn row_blend_x(out: &mut [u32], a: &[u32], xt: &[Tap]) {
    for (o, tx) in out.iter_mut().zip(xt) {
        *o = lerp_px(a[tx.i0 as usize], a[tx.i1 as usize], tx.w);
    }
}

/// Vertical blend of two horizontally resampled rows.
#[inline(never)]
fn row_blend_y(out: &mut [u32], top: &[u32], bot: &[u32], wy: u32) {
    for ((o, &t), &b) in out.iter_mut().zip(top).zip(bot) {
        *o = lerp_px(t, b, wy);
    }
}

/// Render columns `cols` of the raster rows held in `dst` (full raster
/// width `w`, first row `row0`). No per-pixel validity test: the page's
/// column range is resolved once per row.
fn render_rows(
    dst: &mut [u32],
    w: usize,
    row0: usize,
    cols: (usize, usize),
    p: &Plan,
    lvl: &Level,
    bg: u32,
) {
    if p.nearest {
        render_rows_nearest(dst, w, row0, cols, p, lvl, bg);
    } else {
        render_rows_bilinear(dst, w, row0, cols, p, lvl, bg);
    }
}

/// The page's column range on the raster, clipped to `cols`.
fn page_cols(p: &Plan, cols: (usize, usize)) -> (usize, usize) {
    let a0 = p.x_valid.0.clamp(cols.0, cols.1);
    (a0, p.x_valid.1.clamp(a0, cols.1))
}

/// Nearest-neighbour rows: a gather, or a straight copy at exactly 100%.
fn render_rows_nearest(
    dst: &mut [u32],
    w: usize,
    row0: usize,
    cols: (usize, usize),
    p: &Plan,
    lvl: &Level,
    bg: u32,
) {
    let lw = lvl.w as usize;
    let (c0, c1) = cols;
    let (a0, a1) = page_cols(p, cols);
    for (r, row) in dst.chunks_exact_mut(w).enumerate() {
        let ty = p.yt[row0 + r];
        let row = &mut row[c0..c1];
        if !ty.valid || a0 == a1 {
            row.fill(bg);
            continue;
        }
        row[..a0 - c0].fill(bg);
        row[a1 - c0..].fill(bg);
        let out = &mut row[a0 - c0..a1 - c0];
        let xt = &p.xt[a0..a1];
        let a = &lvl.px[ty.i0 as usize * lw..ty.i0 as usize * lw + lw];
        if p.contiguous {
            let start = xt[0].i0 as usize;
            out.copy_from_slice(&a[start..start + out.len()]);
            continue;
        }
        row_gather(out, a, xt);
    }
}

/// Nearest-neighbour gather of one row.
#[inline(never)]
fn row_gather(out: &mut [u32], a: &[u32], xt: &[Tap]) {
    for (o, tx) in out.iter_mut().zip(xt) {
        *o = a[tx.i0 as usize];
    }
}

/// Bilinear rows, with each source row resampled horizontally once.
fn render_rows_bilinear(
    dst: &mut [u32],
    w: usize,
    row0: usize,
    cols: (usize, usize),
    p: &Plan,
    lvl: &Level,
    bg: u32,
) {
    let lw = lvl.w as usize;
    let (c0, c1) = cols;
    let (a0, a1) = page_cols(p, cols);
    let xt = &p.xt[a0..a1];
    let mut hrows: Option<HRows> = None;
    for (r, row) in dst.chunks_exact_mut(w).enumerate() {
        let ty = p.yt[row0 + r];
        let row = &mut row[c0..c1];
        if !ty.valid || a0 == a1 {
            row.fill(bg);
            continue;
        }
        row[..a0 - c0].fill(bg);
        row[a1 - c0..].fill(bg);
        let out = &mut row[a0 - c0..a1 - c0];
        if ty.w == 0 {
            let a = &lvl.px[ty.i0 as usize * lw..ty.i0 as usize * lw + lw];
            row_blend_x(out, a, xt);
            continue;
        }
        let hrows = hrows.get_or_insert_with(|| HRows::new(a1 - a0));
        let top = hrows.get(ty.i0 as usize, None, lvl, xt);
        let bot = hrows.get(ty.i1 as usize, Some(top), lvl, xt);
        row_blend_y(out, &hrows.buf[top], &hrows.buf[bot], ty.w);
    }
}

/// Render the raster-relative, half-open sub-rectangle `rows × cols`,
/// splitting rows across threads when the area is large.
fn render_region(
    raster: &mut PageRaster,
    img: &PageImage,
    p: &Plan,
    rows: (usize, usize),
    cols: (usize, usize),
    threads: usize,
) {
    let wu = raster.w as usize;
    let (r0, r1) = rows;
    if r0 >= r1 || cols.0 >= cols.1 {
        return;
    }
    let lvl = &img.levels[p.level];
    let region = &mut raster.px[r0 * wu..r1 * wu];
    let area = (r1 - r0) * (cols.1 - cols.0);
    let n = if area < 250_000 {
        1
    } else {
        threads.clamp(1, 16)
    };
    if n == 1 {
        render_rows(region, wu, r0, cols, p, lvl, CANVAS_BG);
        return;
    }
    let rows_per = (r1 - r0).div_ceil(n);
    std::thread::scope(|s| {
        for (i, chunk) in region.chunks_mut(rows_per * wu).enumerate() {
            s.spawn(move || render_rows(chunk, wu, r0 + i * rows_per, cols, p, lvl, CANVAS_BG));
        }
    });
}

/// Resample `img` under `view` into `raster` for the canvas at screen
/// `(x, y)` of size `w × h`.
#[allow(clippy::too_many_arguments)]
pub fn render_page(
    raster: &mut PageRaster,
    img: &PageImage,
    view: &ViewTransform,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    threads: usize,
) {
    raster.w = w;
    raster.h = h;
    raster.px.resize((w as usize) * (h as usize), CANVAS_BG);
    if w == 0 || h == 0 {
        return;
    }
    let p = plan(img, view, x, y, w, h);
    render_region(raster, img, &p, (0, h as usize), (0, w as usize), threads);
}

/// A pan by whole pixels: shift the cached raster by `(dx, dy)` and resample
/// only the strips that came into view. Bit-identical to a full render,
/// because every kept pixel maps to the same image point as before.
/// Returns false (doing nothing) when a full render is needed instead.
#[allow(clippy::too_many_arguments)]
pub fn scroll_page(
    raster: &mut PageRaster,
    img: &PageImage,
    view: &ViewTransform,
    x: i32,
    y: i32,
    dx: f64,
    dy: f64,
    threads: usize,
) -> bool {
    let (w, h) = (raster.w as i64, raster.h as i64);
    if dx.fract() != 0.0 || dy.fract() != 0.0 || dx.abs() >= w as f64 || dy.abs() >= h as f64 {
        return false;
    }
    let (dx, dy) = (dx as i64, dy as i64);
    if dx == 0 && dy == 0 {
        return true;
    }
    let wu = w as usize;
    // Rows are visited so that each source row is read before it is overwritten.
    let (src0, dst0, len) = if dx >= 0 {
        (0, dx, w - dx)
    } else {
        (-dx, 0, w + dx)
    };
    let mut shift_row = |r: i64| {
        let src_r = r - dy;
        if (0..h).contains(&src_r) {
            let s0 = (src_r * w + src0) as usize;
            let d0 = (r * w + dst0) as usize;
            raster.px.copy_within(s0..s0 + len as usize, d0);
        }
    };
    if dy > 0 {
        (0..h).rev().for_each(&mut shift_row);
    } else {
        (0..h).for_each(&mut shift_row);
    }
    let p = plan(img, view, x, y, raster.w, raster.h);
    let (kr0, kr1) = if dy >= 0 {
        (dy as usize, h as usize)
    } else {
        (0, (h + dy) as usize)
    };
    if dy > 0 {
        render_region(raster, img, &p, (0, dy as usize), (0, wu), threads);
    } else if dy < 0 {
        render_region(
            raster,
            img,
            &p,
            ((h + dy) as usize, h as usize),
            (0, wu),
            threads,
        );
    }
    if dx > 0 {
        render_region(raster, img, &p, (kr0, kr1), (0, dx as usize), threads);
    } else if dx < 0 {
        render_region(
            raster,
            img,
            &p,
            (kr0, kr1),
            ((w + dx) as usize, wu),
            threads,
        );
    }
    true
}

/// Copy the cached raster into the window buffer at `(x, y)`.
pub fn blit_raster(
    pixels: &mut [u32],
    buf_w: usize,
    buf_h: usize,
    raster: &PageRaster,
    x: i32,
    y: i32,
) {
    let w = raster.w as usize;
    for r in 0..raster.h as usize {
        let by = y as usize + r;
        if by >= buf_h || x < 0 || x as usize + w > buf_w {
            continue;
        }
        let dst = &mut pixels[by * buf_w + x as usize..by * buf_w + x as usize + w];
        dst.copy_from_slice(&raster.px[r * w..r * w + w]);
    }
}

/// Integer pixel rectangle covered by a half-open screen box: the outline
/// sits on the first and last covered pixel of each axis.
pub fn pixel_rect(sb: Box2) -> Rect {
    let x0 = sb.x0.round() as i32;
    let y0 = sb.y0.round() as i32;
    let x1 = (sb.x1.round() as i32).max(x0 + 1);
    let y1 = (sb.y1.round() as i32).max(y0 + 1);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

pub fn outline(p: &mut Painter<'_>, r: Rect, color: u32, thickness: i32) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    let t = thickness.max(1);
    p.fill_rect(Rect::new(r.x, r.y, r.w, t.min(r.h)), color);
    p.fill_rect(Rect::new(r.x, r.bottom() - t, r.w, t.min(r.h)), color);
    p.fill_rect(Rect::new(r.x, r.y, t.min(r.w), r.h), color);
    p.fill_rect(Rect::new(r.right() - t, r.y, t.min(r.w), r.h), color);
}

/// A dashed one-pixel outline. The dash phase runs continuously around the
/// rectangle so corners look deliberate.
pub fn dashed_outline(p: &mut Painter<'_>, r: Rect, color: u32, dash: i32, gap: i32) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    let period = (dash + gap).max(2);
    let clip = p.clip();
    let span = |p: &mut Painter<'_>, horizontal: bool, fixed: i32, from: i32, len: i32| {
        let mut t = 0;
        while t < len {
            let seg = dash.min(len - t);
            let rect = if horizontal {
                Rect::new(from + t, fixed, seg, 1)
            } else {
                Rect::new(fixed, from + t, 1, seg)
            };
            if !clip.intersect(rect).is_empty() {
                p.fill_rect(rect, color);
            }
            t += period;
        }
    };
    span(p, true, r.y, r.x, r.w);
    span(p, true, r.bottom() - 1, r.x, r.w);
    span(p, false, r.x, r.y, r.h);
    span(p, false, r.right() - 1, r.y, r.h);
}

/// Darken or lighten a colour for contrast against the page.
pub fn text_on(color: u32) -> u32 {
    let r = (color >> 16) & 0xff;
    let g = (color >> 8) & 0xff;
    let b = color & 0xff;
    let luma = (r * 299 + g * 587 + b * 114) / 1000;
    if luma > 150 { 0x0010_1010 } else { 0x00ff_ffff }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::geometry::Point;

    #[test]
    fn one_to_one_render_reproduces_pixels() {
        let px: Vec<u32> = (0..64u32).map(|i| i * 0x010203).collect();
        let img = PageImage::from_level0(Level {
            w: 8,
            h: 8,
            px: px.clone(),
        });
        let view = ViewTransform {
            origin: Point::new(0.0, 0.0),
            scale: 1.0,
        };
        let mut r = PageRaster::default();
        render_page(&mut r, &img, &view, 0, 0, 8, 8, 1);
        assert_eq!(r.px, px);
    }

    #[test]
    fn magnified_render_is_nearest() {
        let img = PageImage::from_level0(Level {
            w: 2,
            h: 1,
            px: vec![0x000000, 0xffffff],
        });
        let view = ViewTransform {
            origin: Point::new(0.0, 0.0),
            scale: 4.0,
        };
        let mut r = PageRaster::default();
        render_page(&mut r, &img, &view, 0, 0, 8, 4, 1);
        assert_eq!(
            &r.px[0..8],
            &[0, 0, 0, 0, 0xffffff, 0xffffff, 0xffffff, 0xffffff]
        );
        assert!(r.px[8 * 3..].iter().all(|&p| p == 0 || p == 0xffffff));
    }

    #[test]
    fn outside_page_is_background() {
        let img = PageImage::from_level0(Level {
            w: 2,
            h: 2,
            px: vec![0x111111; 4],
        });
        let view = ViewTransform {
            origin: Point::new(3.0, 3.0),
            scale: 1.0,
        };
        let mut r = PageRaster::default();
        render_page(&mut r, &img, &view, 0, 0, 6, 6, 1);
        assert_eq!(r.px[0], CANVAS_BG);
        assert_eq!(r.px[3 * 6 + 3], 0x111111);
        assert_eq!(r.px[5 * 6 + 5], CANVAS_BG);
    }

    #[test]
    fn scrolling_matches_a_full_render() {
        let img = crate::image_io::decode::synthetic_page(300, 400, 9);
        for (scale, dx, dy) in [
            (0.73, 13.0, -7.0),
            (1.0, -40.0, 25.0),
            (2.5, 3.0, 0.0),
            (0.31, 0.0, -90.0),
            (1.0, 199.0, 1.0),
        ] {
            let v0 = ViewTransform {
                origin: Point::new(-20.25, 10.5),
                scale,
            };
            let mut a = PageRaster::default();
            render_page(&mut a, &img, &v0, 5, 40, 200, 150, 2);
            let v1 = ViewTransform {
                origin: Point::new(v0.origin.x + dx, v0.origin.y + dy),
                scale,
            };
            assert!(scroll_page(&mut a, &img, &v1, 5, 40, dx, dy, 2));
            let mut b = PageRaster::default();
            render_page(&mut b, &img, &v1, 5, 40, 200, 150, 2);
            assert!(a.px == b.px, "scale {scale} d ({dx},{dy})");
        }
        let mut a = PageRaster::default();
        let v = ViewTransform {
            origin: Point::new(0.0, 0.0),
            scale: 1.0,
        };
        render_page(&mut a, &img, &v, 0, 0, 50, 50, 1);
        assert!(
            !scroll_page(&mut a, &img, &v, 0, 0, 0.5, 0.0, 1),
            "fractional pans re-render"
        );
    }

    /// Image w, h, scale, origin, canvas x, y, w, h, threads.
    type Case = (u32, u32, f64, (f64, f64), i32, i32, u32, u32, usize);

    /// The original per-pixel formulation: one bilinear sample per output
    /// pixel (horizontal blend of two source rows, then a vertical blend),
    /// with no row reuse and no copy path. `render_page` must match it bit
    /// for bit.
    fn reference_render(
        img: &PageImage,
        view: &ViewTransform,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) -> Vec<u32> {
        let p = plan(img, view, x, y, w, h);
        let lvl = &img.levels[p.level];
        let lw = lvl.w as usize;
        let mut out = Vec::with_capacity((w * h) as usize);
        for ty in &p.yt {
            for tx in &p.xt {
                out.push(if !ty.valid || !tx.valid {
                    CANVAS_BG
                } else if p.nearest {
                    lvl.px[ty.i0 as usize * lw + tx.i0 as usize]
                } else {
                    let at = |row: u32, col: u32| lvl.px[row as usize * lw + col as usize];
                    let top = lerp_px(at(ty.i0, tx.i0), at(ty.i0, tx.i1), tx.w);
                    if ty.w == 0 {
                        top
                    } else {
                        let bot = lerp_px(at(ty.i1, tx.i0), at(ty.i1, tx.i1), tx.w);
                        lerp_px(top, bot, ty.w)
                    }
                });
            }
        }
        out
    }

    #[test]
    fn render_matches_the_per_pixel_reference() {
        // Minified by mips, bilinear 1x-2x, nearest from 2x, 1:1 on whole and
        // fractional origins, pages partly or wholly off the canvas, and a
        // canvas large enough to split across threads.
        let cases: &[Case] = &[
            (300, 400, 0.31, (-20.25, 10.5), 5, 40, 200, 150, 1),
            (300, 400, 0.5, (3.0, 3.0), 0, 0, 160, 210, 1),
            (300, 400, 0.73, (-20.25, 10.5), 5, 40, 200, 150, 2),
            (301, 257, 0.9, (17.3, -4.7), 0, 0, 333, 211, 1),
            (300, 400, 1.0, (-20.25, 10.5), 0, 0, 250, 300, 1),
            (300, 400, 1.37, (-20.25, 10.5), 0, 0, 250, 300, 3),
            (300, 400, 1.99, (11.1, 12.2), 0, 0, 640, 480, 3),
            (300, 400, 2.0, (11.1, 12.2), 0, 0, 640, 480, 3),
            (300, 400, 3.7, (-90.5, -300.25), 9, 11, 700, 500, 4),
            (300, 400, 0.6, (5000.0, 5000.0), 0, 0, 100, 100, 1),
            (300, 400, 0.6, (-5000.0, 0.0), 0, 0, 100, 100, 1),
            (1, 1, 1.5, (0.0, 0.0), 0, 0, 20, 20, 1),
            (2, 400, 0.8, (4.5, 0.0), 0, 0, 30, 400, 1),
            (700, 900, 0.8, (30.1, 10.7), 0, 0, 900, 800, 4),
        ];
        for (n, &(iw, ih, scale, (ox, oy), x, y, w, h, threads)) in cases.iter().enumerate() {
            let img = crate::image_io::decode::synthetic_page(iw, ih, 7 + n as u64);
            let view = ViewTransform {
                origin: Point::new(ox, oy),
                scale,
            };
            let mut r = PageRaster::default();
            render_page(&mut r, &img, &view, x, y, w, h, threads);
            let want = reference_render(&img, &view, x, y, w, h);
            let bad = r.px.iter().zip(&want).position(|(a, b)| a != b);
            assert!(
                bad.is_none(),
                "case {n} {iw}x{ih} scale {scale}: first mismatch at {bad:?}"
            );
        }
    }

    /// At exactly 100% on whole pixels each raster pixel is a lookup of one
    /// page pixel, or the background off the page. Independent of the taps.
    #[test]
    fn exact_copy_is_a_pixel_lookup() {
        let img = crate::image_io::decode::synthetic_page(250, 180, 5);
        let lvl0 = &img.levels[0];
        for (ox, oy, x, y, w, h, threads) in [
            (0.0, 0.0, 0, 0, 250u32, 180u32, 1usize),
            (13.0, -7.0, 5, 40, 300, 200, 1),
            (-60.0, 90.0, 0, 0, 400, 300, 3),
            (200.0, 150.0, 3, 3, 120, 100, 1),
            (-300.0, 0.0, 0, 0, 100, 100, 1),
            (7.0, 5.0, 11, 13, 700, 500, 4),
        ] {
            let view = ViewTransform {
                origin: Point::new(ox, oy),
                scale: 1.0,
            };
            let mut r = PageRaster::default();
            render_page(&mut r, &img, &view, x, y, w, h, threads);
            for row in 0..h as i32 {
                for col in 0..w as i32 {
                    let (px, py) = (x + col - ox as i32, y + row - oy as i32);
                    let want = if (0..250).contains(&px) && (0..180).contains(&py) {
                        lvl0.px[py as usize * 250 + px as usize]
                    } else {
                        CANVAS_BG
                    };
                    assert_eq!(
                        r.px[row as usize * w as usize + col as usize],
                        want,
                        "origin ({ox},{oy}) canvas ({x},{y}) pixel ({col},{row})"
                    );
                }
            }
        }
    }

    #[test]
    fn lerp_endpoints() {
        assert_eq!(lerp_px(0x102030, 0xf0e0d0, 0), 0x102030);
        assert_eq!(lerp_px(0x102030, 0xf0e0d0, 256), 0xf0e0d0);
    }
}
