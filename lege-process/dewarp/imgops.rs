//! Resampling for page flattening, ported from the model's standalone tester
//! (`dewarp-tester/src/imgops.rs`). The model input must be produced by
//! exactly this antialiasing filter (PyTorch's `interpolate(bilinear,
//! antialias=True)`): a plain bilinear shrink of a 24 MP photo aliases, and
//! the gate constants were tuned on these pixels.

use image::RgbImage;
use rayon::prelude::*;

/// A borrowed 8-bit RGB image, row-major, interleaved.
#[derive(Clone, Copy)]
pub struct RgbRef<'a> {
    pub width: usize,
    pub height: usize,
    pub data: &'a [u8],
}

impl<'a> RgbRef<'a> {
    pub fn of(image: &'a RgbImage) -> Self {
        Self {
            width: image.width() as usize,
            height: image.height() as usize,
            data: image.as_raw(),
        }
    }

    #[inline]
    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// The whole-pixel box (left, top, right, bottom), copied.
    pub fn crop(&self, left: usize, top: usize, right: usize, bottom: usize) -> RgbImage {
        let (width, height) = (right - left, bottom - top);
        let mut data = Vec::with_capacity(width * height * 3);
        for y in top..bottom {
            let start = (y * self.width + left) * 3;
            data.extend_from_slice(&self.data[start..start + width * 3]);
        }
        RgbImage::from_raw(width as u32, height as u32, data).expect("crop buffer matches its size")
    }
}

#[inline]
fn unit(value: u8) -> f32 {
    value as f32 / 255.0
}

/// Triangle-filter weights of `interpolate(bilinear, antialias=True)` along one axis.
fn antialias_taps(size: usize, count: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = size as f32 / count as f32;
    let support = if scale >= 1.0 { scale } else { 1.0 };
    let inverse = if scale >= 1.0 { 1.0 / scale } else { 1.0 };
    (0..count)
        .map(|index| {
            let center = (scale as f64 * (index as f64 + 0.5)) as f32;
            let low = (((center - support) as f64 + 0.5) as i64).max(0) as usize;
            let high = ((((center + support) as f64 + 0.5) as i64).max(0) as usize).min(size);
            let mut weights: Vec<f32> = (low..high)
                .map(|j| {
                    let offset = (j as f32 - center) as f64 + 0.5;
                    (1.0 - ((offset * inverse as f64) as f32).abs()).max(0.0)
                })
                .collect();
            let total: f32 = weights.iter().sum();
            if total != 0.0 {
                for weight in &mut weights {
                    *weight /= total;
                }
            }
            (low, weights)
        })
        .collect()
}

/// The whole image squeezed to (height, width) with antialiasing, as a CHW
/// float tensor clipped to [0, 1]: the model's input.
pub fn resize_antialias_chw(image: RgbRef<'_>, height: usize, width: usize) -> Vec<f32> {
    // Along the rows first, then the columns, as PyTorch.
    let columns = antialias_taps(image.width, width);
    let mut wide = vec![0f32; image.height * width * 3];
    wide.par_chunks_mut(width * 3)
        .enumerate()
        .for_each(|(y, row)| {
            let source = &image.data[y * image.width * 3..(y + 1) * image.width * 3];
            for (x, (low, weights)) in columns.iter().enumerate() {
                let mut sum = [0f32; 3];
                for (k, weight) in weights.iter().enumerate() {
                    let i = (low + k) * 3;
                    sum[0] += unit(source[i]) * weight;
                    sum[1] += unit(source[i + 1]) * weight;
                    sum[2] += unit(source[i + 2]) * weight;
                }
                row[x * 3..x * 3 + 3].copy_from_slice(&sum);
            }
        });
    let rows = antialias_taps(image.height, height);
    let mut hwc = vec![0f32; height * width * 3];
    hwc.par_chunks_mut(width * 3)
        .enumerate()
        .for_each(|(y, row)| {
            let (low, weights) = &rows[y];
            for (k, weight) in weights.iter().enumerate() {
                let source = &wide[(low + k) * width * 3..(low + k + 1) * width * 3];
                for (out, value) in row.iter_mut().zip(source) {
                    *out += value * weight;
                }
            }
            for value in row.iter_mut() {
                *value = value.clamp(0.0, 1.0);
            }
        });
    let plane = height * width;
    let mut chw = vec![0f32; plane * 3];
    for i in 0..plane {
        chw[i] = hwc[i * 3];
        chw[plane + i] = hwc[i * 3 + 1];
        chw[2 * plane + i] = hwc[i * 3 + 2];
    }
    chw
}

/// One bilinear sample at normalized position (x, y) in [-1, 1] (outer pixel
/// edges), `align_corners=False`, border padding. Values in [0, 1].
#[inline]
fn sample(image: RgbRef<'_>, position: [f32; 2]) -> [f32; 3] {
    let (width, height) = (image.width, image.height);
    let x = (((position[0] + 1.0) as f64 * (width as f64 / 2.0) - 0.5) as f32)
        .clamp(0.0, (width - 1) as f32);
    let y = (((position[1] + 1.0) as f64 * (height as f64 / 2.0) - 0.5) as f32)
        .clamp(0.0, (height - 1) as f32);
    let (x0, y0) = (x.floor(), y.floor());
    let (east, south) = (x - x0, y - y0);
    let (west, north) = (1.0 - east, 1.0 - south);
    let (left, top) = (x0 as usize, y0 as usize);
    let (right, bottom) = ((left + 1).min(width - 1), (top + 1).min(height - 1));
    let (p00, p01) = (image.pixel(left, top), image.pixel(right, top));
    let (p10, p11) = (image.pixel(left, bottom), image.pixel(right, bottom));
    let mut out = [0f32; 3];
    for c in 0..3 {
        out[c] = unit(p00[c]) * (north * west)
            + unit(p01[c]) * (north * east)
            + unit(p10[c]) * (south * west)
            + unit(p11[c]) * (south * east);
    }
    out
}

#[inline]
fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
}

/// The source sampled once through a dense backward map, bilinear, with
/// `fill` where the map leaves the photo.
pub fn render(
    image: RgbRef<'_>,
    map: &[[f32; 2]],
    height: usize,
    width: usize,
    fill: [u8; 3],
) -> RgbImage {
    assert_eq!(map.len(), height * width);
    let mut data = vec![0u8; height * width * 3];
    data.par_chunks_mut(width * 3)
        .zip(map.par_chunks(width))
        .for_each(|(row, positions)| {
            for (x, &p) in positions.iter().enumerate() {
                let valid =
                    p[0].is_finite() && p[1].is_finite() && p[0].abs() <= 1.0 && p[1].abs() <= 1.0;
                let rgb = if valid {
                    let value = sample(image, p);
                    [quantize(value[0]), quantize(value[1]), quantize(value[2])]
                } else {
                    fill
                };
                row[x * 3..x * 3 + 3].copy_from_slice(&rgb);
            }
        });
    RgbImage::from_raw(width as u32, height as u32, data).expect("render buffer matches its size")
}
