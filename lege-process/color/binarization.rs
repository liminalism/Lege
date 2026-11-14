//! Document binarization.
//!
//! Two methods. "Best", the default: the tileable binarizer model on the GPU.
//! "Fast": the adaptive algorithm (Sauvola fused with background-normalized
//! Otsu) on the wgpu kernels, about twice the pages per second. Where no
//! hardware GPU is available both come down to the adaptive algorithm on the
//! CPU, the last resort. Blank pages take a fixed
//! threshold on the CPU fallback so that they come out white, not noise.

use crate::color::BinarizationOptions;
use anyhow::{Result, anyhow};
use image::{GrayImage, RgbImage};
use rayon::prelude::*;
use rayon::slice::ParallelSliceMut;
use std::cell::RefCell;
use std::cmp::{max, min};

thread_local! {
    /// Per-worker arena for the model's whole-region RGB input. The model
    /// remains shared, while Rayon workers retain their largest region buffer
    /// instead of allocating it again for every page.
    static HEAVY_SAUVOLA_RGB_ARENA: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Model binarization. The tileable binarizer runs on the GPU; when there is
/// no hardware GPU, or the GPU run fails, the CPU fallback takes the page.
/// Output convention: 0 = ink, 255 = paper.
pub struct HeavySauvolaProcessor {
    cpu: CpuBinarizer,
    /// Tileable binarizer on the wgpu executor; `None` without a hardware
    /// GPU, when forced off, or when it failed to load.
    student: Option<lege_gpu::vision::SauvolaProcessor>,
    /// Set after the first GPU failure so later pages go straight to the CPU.
    student_failed: std::sync::atomic::AtomicBool,
}

/// What binarizes a page when the GPU model cannot.
enum CpuBinarizer {
    /// The adaptive algorithm: the default.
    Adaptive,
    /// The DIBCO Sauvola model, only from a `sauvola.onnx` runtime asset or
    /// the `sauvola-model` feature. It uses global instance normalization, so
    /// it must see the whole region at once and unpadded.
    Sauvola(lege_gpu::vision::SauvolaCpuProcessor),
}

/// Spatial multiple the model's input is padded to on the GPU: a coarser
/// multiple than its stride (8) lets nearby page sizes share one compiled graph.
const STUDENT_PAD_MULTIPLE: u32 = 64;

/// Largest page, in pixels after padding, that the model takes in one run.
/// The runtime keeps a graph's activations in one buffer of about 1.6 kB per
/// pixel, and wgpu refuses a buffer over 4 GiB: a 3000 px high page asked for
/// 9.8 GB and the job died on a validation error. Larger pages are tiled.
const STUDENT_MAX_PIXELS: usize = 1_600_000;

/// Side of a tile of a large page, and the border of each tile that is
/// computed but not used. The model is fully convolutional with no global
/// statistics, so a tile's interior does not depend on what lies beyond its
/// border; `tiles_reproduce_the_whole_page_mask` measures that.
const STUDENT_TILE: u32 = 1024;
const STUDENT_TILE_HALO: u32 = 128;

/// Tiles along one axis of length `n`: (start, length, kept range). Starts
/// are multiples of `tile - 2 * halo`, which must be a multiple of the pad
/// multiple: the model downsamples by 8, so a tile whose origin is off that
/// grid sees the page shifted and flips stroke-edge pixels. Kept ranges abut;
/// the last tile is as long as what is left of the axis.
fn tile_spans(n: u32, tile: u32, halo: u32) -> Vec<(u32, u32, std::ops::Range<u32>)> {
    let stride = tile - 2 * halo;
    let mut spans = Vec::new();
    let mut start = 0u32;
    loop {
        let last = start + tile >= n;
        let kept_from = if start == 0 { 0 } else { start + halo };
        let kept_to = if last { n } else { start + tile - halo };
        spans.push((start, tile.min(n - start), kept_from..kept_to));
        if last {
            return spans;
        }
        start += stride;
    }
}

/// The model's mask of a page on the GPU: in one run when the page is small
/// enough, in overlapping tiles otherwise.
fn binarize_on_gpu(
    student: &lege_gpu::vision::SauvolaProcessor,
    img: &RgbImage,
) -> Result<GrayImage> {
    binarize_on_gpu_with(
        student,
        img,
        STUDENT_MAX_PIXELS,
        STUDENT_TILE,
        STUDENT_TILE_HALO,
    )
}

fn binarize_on_gpu_with(
    student: &lege_gpu::vision::SauvolaProcessor,
    img: &RgbImage,
    max_pixels: usize,
    tile: u32,
    halo: u32,
) -> Result<GrayImage> {
    let (width, height) = img.dimensions();
    let padded = |n: u32| n.div_ceil(STUDENT_PAD_MULTIPLE) as usize * STUDENT_PAD_MULTIPLE as usize;
    if padded(width) * padded(height) <= max_pixels {
        return student.binarize_rgb_padded(img, STUDENT_PAD_MULTIPLE);
    }
    let mut mask = GrayImage::new(width, height);
    for (top, tile_h, rows) in tile_spans(height, tile, halo) {
        for (left, tile_w, cols) in tile_spans(width, tile, halo) {
            let view = image::imageops::crop_imm(img, left, top, tile_w, tile_h).to_image();
            let part = student.binarize_rgb_padded(&view, STUDENT_PAD_MULTIPLE)?;
            for y in rows.clone() {
                for x in cols.clone() {
                    mask.put_pixel(x, y, *part.get_pixel(x - left, y - top));
                }
            }
        }
    }
    Ok(mask)
}

/// The adaptive algorithm's mask of a region: on the wgpu kernels when a
/// hardware GPU is there, on the CPU as the last resort. Input inversion is
/// part of the mask.
fn adaptive_mask(img: &RgbImage, opt: &BinarizationOptions) -> Result<GrayImage> {
    let (width, height) = img.dimensions();
    let (w, h) = (width as usize, height as usize);
    let invert = |gray: &mut Vec<u8>| {
        if opt.invert_input {
            gray.par_iter_mut().for_each(|pixel| *pixel = 255 - *pixel);
        }
    };
    let wrap = |bin: Vec<u8>| {
        GrayImage::from_raw(width, height, bin)
            .ok_or_else(|| anyhow!("failed to wrap the adaptive mask"))
    };
    if !opt.disable_gpu && adaptive_kernels_usable() {
        // The plane the kernels have always thresholded: integer Rec. 601.
        let mut gray = vec![0u8; w * h];
        gray.par_iter_mut()
            .zip(img.as_raw().par_chunks_exact(3))
            .for_each(|(out, rgb)| {
                *out = ((rgb[0] as u32 * 77 + rgb[1] as u32 * 150 + rgb[2] as u32 * 29) >> 8) as u8;
            });
        invert(&mut gray);
        if let Some(bin) = adaptive_on_gpu(&gray, w, h) {
            return wrap(bin);
        }
    }
    let mut gray = linearized_rgb_to_gray(img.as_raw(), w, h);
    invert(&mut gray);
    wrap(improved_binarize(&gray, w, h))
}

/// Whether the adaptive wgpu kernels may be tried: a hardware GPU, and no
/// request for the CPU. Asked once per process.
fn adaptive_kernels_usable() -> bool {
    static USABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *USABLE.get_or_init(|| !cpu_forced_by_env() && lege_gpu::vision::hardware_gpu_available())
}

/// The adaptive algorithm on the wgpu kernels (`lege_gpu::binarization`):
/// Sauvola and the background-normalized Otsu arm fused on the device, with
/// the two page constants (30th percentile, Otsu threshold) found here.
/// `None` when the kernels refuse or fail; the caller then runs the CPU.
fn adaptive_on_gpu(gray: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
    let params = lege_gpu::binarization::BinarizationParams {
        width: width as u32,
        height: height as u32,
        mode: lege_gpu::binarization::BinarizationMode::Adaptive,
        invert_output: false,
        k_factor: DEFAULT_K_FACTOR,
        fixed_threshold: 180,
        adaptive: compute_adaptive_gpu_constants(gray, width, height),
        debug_mode: 0,
    };
    lege_gpu::binarization::try_binarize_gray_with(gray, &params, |data| {
        data[..width * height].to_vec()
    })
}

pub fn compute_adaptive_gpu_constants(
    gray: &[u8],
    width: usize,
    height: usize,
) -> lege_gpu::binarization::AdaptiveBinarizeGpuConstants {
    let n = width * height;
    assert_eq!(gray.len(), n);

    // percentile_c (the normalization constant `c`) is the 30th percentile of the
    // RAW gray histogram, matching `improved_binarize` (line ~580).
    let hist = hist256(gray);
    let percentile_c = percentile_from_hist(&hist, 0.30, n);

    // The Otsu threshold MUST be computed on the bg-NORMALIZED image, exactly as the
    // CPU `improved_binarize` does (it thresholds `normalized > t_otsu`). Computing
    // Otsu on the raw histogram is a *different distribution*: on layout-mode pages,
    // premasking injects a large 255 spike, and raw-Otsu's between-class variance can
    // lock onto the paper|masked-white split instead of ink|paper, yielding t above
    // the normalized paper level (~c). The Otsu arm then classifies nearly the whole
    // page as ink; since the fusion is union-of-ink, the page blackens. Normalizing
    // first collapses the masked spike onto the paper mode (bg≈255 there, so
    // normalized≈c), so Otsu re-splits a clean ink|paper histogram — matching CPU.
    let bg_window = odd_background_window(height);
    let mut normalized = vec![0u8; n];
    dilate_square_reflect(gray, width, height, bg_window, &mut normalized);
    normalize_by_bg_inplace(gray, percentile_c, &mut normalized);
    let otsu_threshold = otsu_threshold_u8(&normalized);

    lege_gpu::binarization::AdaptiveBinarizeGpuConstants {
        sauvola_window: sauvola_window_for(height, width) as u32,
        bg_window: bg_window as u32,
        percentile_c,
        otsu_threshold,
    }
}

/// Sauvola k-factor of the adaptive CPU fallback. Lower values keep more ink
/// in flat regions; 0.05 balances stroke preservation against background
/// noise on white and yellowed paper.
const DEFAULT_K_FACTOR: f32 = 0.05;

fn load_cpu_binarizer() -> CpuBinarizer {
    use lege_gpu::vision::SauvolaCpuProcessor;
    // Runtime assets remain deliberate model-rollout overrides.
    if let Some(path) = crate::runtime_asset_path_if_exists("sauvola.prepared.onnx")
        .or_else(|| crate::runtime_asset_path_if_exists("sauvola.onnx"))
    {
        match SauvolaCpuProcessor::from_model_path(&path) {
            Ok(model) => return CpuBinarizer::Sauvola(model),
            Err(_e) => {
                #[cfg(feature = "debug-logging")]
                crate::encoding::streamline::log_debug_message(&format!(
                    "Sauvola model override {} failed to load: {_e:#}",
                    path.display()
                ));
            }
        }
    }
    #[cfg(feature = "sauvola-model")]
    if let Ok(model) = SauvolaCpuProcessor::from_model_bytes(crate::EMBEDDED_SAUVOLA_MODEL) {
        return CpuBinarizer::Sauvola(model);
    }
    CpuBinarizer::Adaptive
}

/// True when the user or a test asked for the CPU fallback outright.
fn cpu_forced_by_env() -> bool {
    std::env::var("LEGE_FORCE_CPU_BINARIZATION").is_ok_and(|v| v != "0")
        || std::env::var("LEGE_BINARIZATION_BACKEND").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"))
}

fn load_student_binarizer() -> Option<lege_gpu::vision::SauvolaProcessor> {
    if cpu_forced_by_env() || !lege_gpu::vision::hardware_gpu_available() {
        return None;
    }
    let loaded = match crate::runtime_asset_path_if_exists("binarizer-tilenet.onnx") {
        Some(path) => lege_gpu::vision::SauvolaProcessor::from_model_path(&path),
        None => {
            lege_gpu::vision::SauvolaProcessor::from_model_bytes(crate::EMBEDDED_TILENET_BINARIZER)
        }
    };
    loaded.ok()
}

impl HeavySauvolaProcessor {
    pub fn new() -> Result<Self> {
        Ok(Self {
            cpu: load_cpu_binarizer(),
            student: load_student_binarizer(),
            student_failed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// True while pages go through the model on the GPU.
    fn on_gpu(&self) -> bool {
        self.student.is_some()
            && !self
                .student_failed
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Runs the model on the GPU when it is available and allowed.
    /// Returns `None` to fall back to the CPU.
    fn student_mask(&self, img: &RgbImage, opt: &BinarizationOptions) -> Option<GrayImage> {
        use std::sync::atomic::Ordering;
        let student = self.student.as_ref()?;
        if opt.disable_gpu || self.student_failed.load(Ordering::Relaxed) {
            return None;
        }
        match binarize_on_gpu(student, img) {
            Ok(mask) => Some(mask),
            Err(error) => {
                // Said once, on stderr: from here the job is on the fallback.
                if !self.student_failed.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "The binarizer model failed on the GPU; pages are binarized on the CPU from here: {error:#}"
                    );
                }
                None
            }
        }
    }

    /// The CPU fallback for one region. Input inversion is part of the mask.
    fn cpu_mask(&self, img: &RgbImage, opt: &BinarizationOptions) -> Result<GrayImage> {
        match &self.cpu {
            CpuBinarizer::Sauvola(model) => {
                let mut mask = model.binarize_rgb(img)?;
                if opt.invert_input {
                    mask.par_iter_mut().for_each(|pixel| *pixel = 255 - *pixel);
                }
                Ok(mask)
            }
            CpuBinarizer::Adaptive => adaptive_mask(img, opt),
        }
    }

    /// Binarize one region of an RGB page at native resolution; the mask comes
    /// back at the region's own size. Output: 0 = ink, 255 = paper.
    pub fn process_rgb_region(
        &self,
        rgb_data: &[u8],
        page_width: u32,
        _page_height: u32,
        region_x: u32,
        region_y: u32,
        region_w: u32,
        region_h: u32,
        opt: &BinarizationOptions,
    ) -> Result<GrayImage> {
        // The region must lie fully within the page: a silent zero-fill would
        // feed black pixels into the model, so fail hard instead.
        if region_w == 0 || region_h == 0 {
            return Err(anyhow!("empty region ({region_w}x{region_h})"));
        }
        let row_bytes = region_w as usize * 3;
        let last_src_row = (region_y + region_h - 1) as usize;
        let last_src_end =
            (last_src_row * page_width as usize + (region_x + region_w) as usize) * 3;
        if last_src_end > rgb_data.len() {
            return Err(anyhow!(
                "region ({region_x},{region_y} {region_w}x{region_h}) exceeds source \
                 buffer (needs {last_src_end} bytes, have {})",
                rgb_data.len()
            ));
        }
        let required = region_h as usize * row_bytes;
        let mut rgb = HEAVY_SAUVOLA_RGB_ARENA.with(|arena| {
            let mut arena = arena.borrow_mut();
            let mut buffer = std::mem::take(&mut *arena);
            buffer.resize(required, 0);
            buffer
        });
        for y in 0..region_h as usize {
            let src_y = region_y as usize + y;
            let src_start = (src_y * page_width as usize + region_x as usize) * 3;
            let dst_start = y * row_bytes;
            rgb[dst_start..dst_start + row_bytes]
                .copy_from_slice(&rgb_data[src_start..src_start + row_bytes]);
        }
        let img = RgbImage::from_raw(region_w, region_h, rgb)
            .ok_or_else(|| anyhow!("failed to create RgbImage from region"))?;

        let model_mask = if opt.adaptive {
            None
        } else {
            self.student_mask(&img, opt)
        };
        let mask = match model_mask {
            Some(mut mask) => {
                if opt.invert_input {
                    mask.par_iter_mut().for_each(|pixel| *pixel = 255 - *pixel);
                }
                Ok(mask)
            }
            None if opt.adaptive => adaptive_mask(&img, opt),
            None => self.cpu_mask(&img, opt),
        };
        let mut recycled = img.into_raw();
        recycled.clear();
        HEAVY_SAUVOLA_RGB_ARENA.with(|arena| {
            *arena.borrow_mut() = recycled;
        });
        mask
    }

    /// Process the whole RGB image as a single region.
    pub fn process_rgb_patch(
        &self,
        rgb_data: &[u8],
        width: u32,
        height: u32,
        opt: &BinarizationOptions,
    ) -> Result<GrayImage> {
        self.process_rgb_region(rgb_data, width, height, 0, 0, width, height, opt)
    }
}

/// The process-wide binarizer. It is internally synchronized, so it is shared
/// by `&` and concurrent page workers do not serialize on one lock.
static HEAVY_PROCESSOR_CACHE: std::sync::OnceLock<HeavySauvolaProcessor> =
    std::sync::OnceLock::new();

fn heavy_processor() -> &'static HeavySauvolaProcessor {
    HEAVY_PROCESSOR_CACHE.get_or_init(|| HeavySauvolaProcessor {
        cpu: load_cpu_binarizer(),
        student: load_student_binarizer(),
        student_failed: std::sync::atomic::AtomicBool::new(false),
    })
}

/// True when pages are binarized by the model on the GPU; false when the CPU
/// fallback is in use (no hardware GPU, forced off, or a failed GPU run).
/// The first call loads the model.
pub fn binarizes_on_gpu() -> bool {
    heavy_processor().on_gpu()
}

/// Binarizes an RGB image (interleaved R,G,B) and returns it as PBM P4.
///
/// # Panics
/// Panics if the input image size does not match `width * height * 3`.
pub fn binarize_image(
    image: &[u8],
    width: usize,
    height: usize,
    options: &BinarizationOptions,
) -> Vec<u8> {
    let bin = binarize_image_raw(image, width, height, options);
    pbm::make_pbm_p4(&bin, width, height)
}

/// Binarizes an RGB image and returns raw binary data, one byte per pixel
/// (0 = ink, 255 = paper before `options.invert`).
///
/// # Panics
/// Panics if the input image size does not match `width * height * 3`.
pub fn binarize_image_raw(
    image: &[u8],
    width: usize,
    height: usize,
    options: &BinarizationOptions,
) -> Vec<u8> {
    assert_eq!(
        image.len(),
        width * height * 3,
        "Input image size does not match width*height*3"
    );
    if let Some(threshold) = options.blank_threshold {
        let gray = linearized_rgb_to_gray(image, width, height);
        return threshold_gray(&gray, width, height, threshold, options);
    }
    binarize_rgb_model(image, width, height, options)
}

/// Binarizes an 8-bit grayscale image and returns raw binary data. The model
/// takes RGB, so the plane is replicated for it.
///
/// # Panics
/// Panics if the input image size does not match `width * height`.
pub fn binarize_gray(
    gray: &[u8],
    width: usize,
    height: usize,
    options: &BinarizationOptions,
) -> Vec<u8> {
    assert_eq!(
        gray.len(),
        width * height,
        "Input grayscale size does not match width*height"
    );
    if let Some(threshold) = options.blank_threshold {
        return threshold_gray(gray, width, height, threshold, options);
    }
    let rgb = gray
        .iter()
        .flat_map(|value| [*value, *value, *value])
        .collect::<Vec<_>>();
    binarize_rgb_model(&rgb, width, height, options)
}

/// Callback variant of [`binarize_image_raw`]: hands the binarized bytes
/// (exactly `width * height`) to `f`.
pub fn binarize_image_raw_with<F, R>(
    image: &[u8],
    width: usize,
    height: usize,
    options: &BinarizationOptions,
    f: F,
) -> R
where
    F: FnOnce(&[u8]) -> R,
{
    f(&binarize_image_raw(image, width, height, options))
}

fn binarize_rgb_model(
    image: &[u8],
    width: usize,
    height: usize,
    options: &BinarizationOptions,
) -> Vec<u8> {
    let mut bin =
        match heavy_processor().process_rgb_patch(image, width as u32, height as u32, options) {
            Ok(mask) => mask.into_raw(),
            Err(_e) => {
                // The region is the whole page, so only a failed CPU model lands
                // here: the adaptive algorithm cannot fail.
                crate::warn_log!("Binarizer failed, using the adaptive CPU algorithm: {_e:#}");
                let mut gray = linearized_rgb_to_gray(image, width, height);
                if options.invert_input {
                    gray.par_iter_mut().for_each(|pixel| *pixel = 255 - *pixel);
                }
                improved_binarize(&gray, width, height)
            }
        };
    if options.invert {
        bin.par_iter_mut().for_each(|pixel| *pixel = 255 - *pixel);
    }
    bin
}

fn threshold_gray(
    gray: &[u8],
    width: usize,
    height: usize,
    threshold: u8,
    options: &BinarizationOptions,
) -> Vec<u8> {
    let mut result = vec![0u8; width * height];
    if options.invert_input {
        let inverted: Vec<u8> = gray.par_iter().map(|pixel| 255 - *pixel).collect();
        apply_threshold(&inverted, threshold, &mut result, width, height);
    } else {
        apply_threshold(gray, threshold, &mut result, width, height);
    }
    if options.invert {
        result
            .par_iter_mut()
            .for_each(|pixel| *pixel = 255 - *pixel);
    }
    result
}

fn linearized_rgb_to_gray(image: &[u8], width: usize, height: usize) -> Vec<u8> {
    let linear_rgb = crate::color::linearize::linearize_rgb_bytes_to_f32(image);
    let mut gray = vec![0; width * height];
    crate::color::linearize::linearized_rgb_to_grayscale(&linear_rgb, &mut gray);
    gray
}

/// The adaptive CPU algorithm: Sauvola, background-normalized Otsu, and their
/// AND fusion. 0 = ink, 255 = paper.
///
/// `scratch` rotates through the dilated background, the normalized image and
/// the Otsu result, and is finally AND'd into the Sauvola buffer.
fn improved_binarize(gray: &[u8], width: usize, height: usize) -> Vec<u8> {
    let n = width * height;
    assert!(gray.len() == n);

    let win = sauvola_window_for(height, width);

    let mut sauvola_bin = vec![0u8; n];
    sauvola_via_integral(gray, width, height, win, DEFAULT_K_FACTOR, &mut sauvola_bin);

    let s = odd_background_window(height);
    let mut scratch = vec![0u8; n];
    dilate_square_reflect(gray, width, height, s, &mut scratch);

    let hist = hist256(gray);
    let c = percentile_from_hist(&hist, 0.30, n);

    // In-place: normalize_by_bg reads gray + scratch(bg) and writes back into scratch.
    normalize_by_bg_inplace(gray, c, &mut scratch);

    let t_otsu = otsu_threshold_u8(&scratch);
    scratch
        .par_iter_mut()
        .for_each(|v| *v = if *v > t_otsu { 255 } else { 0 });

    sauvola_bin
        .par_iter_mut()
        .zip(scratch.par_iter())
        .for_each(|(s, &o)| {
            *s = if *s != 0 && o != 0 { 255 } else { 0 };
        });

    sauvola_bin
}

/// Adaptive window size for Sauvola
#[inline]
pub fn sauvola_window_for(h: usize, w: usize) -> usize {
    let mut win = (h.min(w) / 40).max(31).min(101);
    if win % 2 == 0 {
        win += 1;
    }
    win
}

#[inline]
pub fn odd_background_window(height: usize) -> usize {
    let mut s = max(3, min(height / 200, 15));
    if s % 2 == 0 {
        s += 1;
    }
    s
}

/// Sauvola via integral images.
///
/// Memory layout: previously materialized a padded gray buffer + two index tables
/// (reflect_x/reflect_y) before building the two u64 integral images. The padded
/// buffer (~W*H bytes for typical windows) and reflection Vecs are now eliminated:
/// the row prefix-sum pass reads reflected source pixels directly from `gray`.
fn sauvola_via_integral(
    gray: &[u8],
    width: usize,
    height: usize,
    win: usize,
    k: f32,
    bin: &mut [u8],
) {
    let r = (win / 2) as isize;
    let pad_w = width + 2 * (r as usize);
    let pad_h = height + 2 * (r as usize);

    let iw = pad_w + 1;
    let mut integ = vec![0u64; iw * (pad_h + 1)];
    let mut integ_sq = vec![0u64; iw * (pad_h + 1)];

    // Row prefix sums (parallelized per row). Each row pulls reflected source
    // pixels directly from `gray`, avoiding a separate padded image allocation.
    let w_isz = width as isize;
    let h_isz = height as isize;
    integ
        .par_chunks_mut(iw)
        .zip(integ_sq.par_chunks_mut(iw))
        .enumerate()
        .skip(1) // Skip row 0 (remains zeros)
        .for_each(|(y, (integ_row, integ_sq_row))| {
            let src_y = reflect_101((y - 1) as isize - r as isize, h_isz) as usize;
            let row_base = src_y * width;
            let mut sum = 0u64;
            let mut sum_sq = 0u64;
            for x in 1..=pad_w {
                let src_x = reflect_101((x - 1) as isize - r as isize, w_isz) as usize;
                let v = gray[row_base + src_x] as u64;
                sum += v;
                sum_sq += v * v;
                integ_row[x] = sum;
                integ_sq_row[x] = sum_sq;
            }
        });

    // Column prefix (sequential, requires cumulative sum)
    for x in 1..=pad_w {
        for y in 2..=pad_h {
            let idx = y * iw + x;
            let prev_idx = (y - 1) * iw + x;
            integ[idx] += integ[prev_idx];
            integ_sq[idx] += integ_sq[prev_idx];
        }
    }

    // Threshold per pixel
    let inv_area = 1.0f64 / (win as f64 * win as f64);
    bin.par_chunks_mut(width)
        .enumerate()
        .for_each(|(y_orig, bin_row)| {
            let gray_row = &gray[y_orig * width..(y_orig + 1) * width];
            for x_orig in 0..width {
                let x_padded_center = x_orig + r as usize;
                let y_padded_center = y_orig + r as usize;
                let x0 = x_padded_center - r as usize;
                let x1 = x_padded_center + r as usize;
                let y0 = y_padded_center - r as usize;
                let y1 = y_padded_center + r as usize;

                let d = integ[(y1 + 1) * iw + (x1 + 1)] as f64;
                let b = integ[(y1 + 1) * iw + x0] as f64;
                let c = integ[y0 * iw + (x1 + 1)] as f64;
                let a = integ[y0 * iw + x0] as f64;
                let sum = d + a - b - c;

                let d_sq = integ_sq[(y1 + 1) * iw + (x1 + 1)] as f64;
                let b_sq = integ_sq[(y1 + 1) * iw + x0] as f64;
                let c_sq = integ_sq[y0 * iw + (x1 + 1)] as f64;
                let a_sq = integ_sq[y0 * iw + x0] as f64;
                let sum_sq = d_sq + a_sq - b_sq - c_sq;

                let mean = sum * inv_area;
                let var = (sum_sq * inv_area) - mean * mean;
                let std = if var > 0.0 { var.sqrt() } else { 0.0 };

                let thr = sauvola_threshold(mean as f32, std as f32, k);
                let px = gray_row[x_orig] as f32;
                bin_row[x_orig] = if px > thr { 255 } else { 0 };
            }
        });
}

#[inline]
fn sauvola_threshold(mu: f32, sigma: f32, k: f32) -> f32 {
    mu * (1.0 + k * (sigma / 127.0 - 1.0))
}

// 1-D sliding max filter (van Herk / Gil-Werman) for a line of u8
fn maxfilter_1d_u8(src: &[u8], k: usize, dst: &mut [u8]) {
    let n = src.len();
    assert!(k >= 1 && k % 2 == 1 && dst.len() == n);
    let r = k / 2;
    let mut g = vec![0u8; n];
    let mut h = vec![0u8; n];

    // forward
    for i in 0..n {
        g[i] = if i % k == 0 {
            src[i]
        } else {
            g[i - 1].max(src[i])
        }
    }
    // backward
    for i in (0..n).rev() {
        h[i] = if i + 1 == n || (i + 1) % k == 0 {
            src[i]
        } else {
            h[i + 1].max(src[i])
        }
    }
    // combine: Van Herk/Gil — h gives suffix max from i-r, g gives prefix max to i+r
    for i in 0..n {
        let a = if i >= r { h[i - r] } else { h[0] };
        let b = if i + r < n { g[i + r] } else { g[n - 1] };
        dst[i] = a.max(b);
    }
}

// 2-D separable max filter with reflect-101 borders (approximates ellipse with square)
fn dilate_square_reflect(gray: &[u8], w: usize, h: usize, k: usize, out: &mut [u8]) {
    let mut tmp = vec![0u8; w * h];
    // horizontal
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let mut line = vec![0u8; w + 2 * (k / 2)];
        // reflect-101 pad into `line`
        for x in 0..line.len() {
            let rx = reflect_101(x as isize - (k / 2) as isize, w as isize) as usize;
            line[x] = gray[y * w + rx];
        }
        let mut maxed = vec![0u8; line.len()];
        maxfilter_1d_u8(&line, k, &mut maxed);
        row.copy_from_slice(&maxed[k / 2..k / 2 + w]);
    });
    // vertical - process columns in parallel (each column writes to disjoint positions)
    let r = k / 2;
    (0..w).into_par_iter().for_each(|x| {
        // build padded column
        let mut col = vec![0u8; h + 2 * r];
        for py in 0..col.len() {
            let ry = reflect_101(py as isize - r as isize, h as isize) as usize;
            col[py] = tmp[ry * w + x];
        }

        // apply 1D max filter on the column
        let mut maxed = vec![0u8; col.len()];
        maxfilter_1d_u8(&col, k, &mut maxed);

        // write back the central region into the output column (safe: each thread writes disjoint x)
        for row_idx in 0..h {
            let val = maxed[r + row_idx];
            // SAFETY: Each thread processes a unique column x, so writes to out[row_idx * w + x] are disjoint
            unsafe {
                let ptr = out.as_ptr() as *mut u8;
                *ptr.add(row_idx * w + x) = val;
            }
        }
    });
}

/// Histogram for u8 data
fn hist256(data: &[u8]) -> [u32; 256] {
    let mut h = [0u32; 256];
    data.iter().for_each(|&v| h[v as usize] += 1);
    h
}

/// Percentile from histogram
fn percentile_from_hist(h: &[u32; 256], p: f32, n: usize) -> u8 {
    let target = (p * (n as f32)).round() as u32;
    let mut acc = 0u32;
    for i in 0..256 {
        acc += h[i];
        if acc >= target {
            return i as u8;
        }
    }
    255
}

/// In-place: `bg_then_out` enters as the dilated background and is overwritten
/// with the normalized result. Reads `gray[i]` and uses `bg_then_out[i]` as the
/// divisor before writing the new value.
fn normalize_by_bg_inplace(gray: &[u8], c: u8, bg_then_out: &mut [u8]) {
    let c = c as f32;
    bg_then_out
        .par_iter_mut()
        .zip(gray.par_iter())
        .for_each(|(slot, &g)| {
            let b = *slot as f32;
            let val = c / (b + 1e-6) * (g as f32);
            *slot = val.clamp(0.0, 255.0) as u8;
        });
}

/// Otsu threshold from a pre-built histogram.
fn otsu_from_hist(h: &[u32; 256], n: usize) -> u8 {
    let total = n as f64;
    let mut sum_all = 0.0f64;
    for i in 0..256 {
        sum_all += (i as f64) * (h[i] as f64);
    }
    let mut sum_b = 0.0f64;
    let mut w_b = 0.0f64;
    let mut max_var = -1.0f64;
    let mut thresh = 0u8;
    for t in 0..256 {
        w_b += h[t] as f64;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += (t as f64) * (h[t] as f64);
        let m_b = sum_b / w_b;
        let m_f = (sum_all - sum_b) / w_f;
        let var_between = w_b * w_f * (m_b - m_f).powi(2);
        if var_between > max_var {
            max_var = var_between;
            thresh = t as u8;
        }
    }
    thresh
}

/// Otsu threshold
fn otsu_threshold_u8(data: &[u8]) -> u8 {
    let h = hist256(data);
    otsu_from_hist(&h, data.len())
}

#[inline(always)]
fn reflect_101(idx: isize, len: isize) -> isize {
    if idx < 0 {
        -idx - 1
    } else if idx >= len {
        2 * len - idx - 1
    } else {
        idx
    }
}

/// Applies a mask to an already binarized image.
/// Pixels in the binary data corresponding to a 255 in the mask will be set to white (255).
pub fn apply_mask_to_binary(binary_data: &mut [u8], mask: &[u8]) {
    assert_eq!(
        binary_data.len(),
        mask.len(),
        "Binary data and mask must have the same dimensions"
    );

    // This can be parallelized for large images
    binary_data
        .par_iter_mut()
        .zip(mask.par_iter())
        .for_each(|(pixel, mask_val)| {
            if *mask_val == 255 {
                *pixel = 255; // Set to white
            }
        });
}

/// Apply fixed threshold.
///
/// This is safe to parallelize by row because each worker writes to a disjoint
/// output slice. Keep smaller images on the serial path to avoid Rayon overhead.
pub(crate) fn apply_threshold(gray: &[u8], thr: u8, bin: &mut [u8], width: usize, height: usize) {
    const PARALLEL_THRESHOLD_PIXELS: usize = 256 * 256;

    if width.saturating_mul(height) < PARALLEL_THRESHOLD_PIXELS || height < 8 {
        for y in 0..height {
            let start = y * width;
            let row = &mut bin[start..start + width];
            let gray_row = &gray[start..start + width];
            for i in 0..width {
                row[i] = if gray_row[i] > thr { 255 } else { 0 };
            }
        }
        return;
    }

    bin.par_chunks_mut(width)
        .zip(gray.par_chunks(width))
        .for_each(|(row, gray_row)| {
            for i in 0..width {
                row[i] = if gray_row[i] > thr { 255 } else { 0 };
            }
        });
}

/// Invert binary image (parallel using par_chunks_mut)
///
/// Only exercised by `invert_binary_flips_all_pixels_by_row` below — no
/// production caller — so it is gated to the test build that uses it.
#[cfg(test)]
pub(crate) fn invert_binary(bin: &mut [u8], _height: usize, width: usize) {
    bin.par_chunks_mut(width).for_each(|row| {
        for pixel in row.iter_mut() {
            *pixel = 255 - *pixel;
        }
    });
}

/// PBM (P4) and 1-bit packing
pub mod pbm {
    use rayon::iter::{IndexedParallelIterator, ParallelIterator};
    use rayon::prelude::ParallelSliceMut;

    /// Encode PBM P4 header + data (black=bit1 MSB-first) - parallel over rows
    pub fn make_pbm_p4(bin: &[u8], width: usize, height: usize) -> Vec<u8> {
        let header = format!("P4\n{} {}\n", width, height);
        let row_bytes = (width + 7) >> 3;
        let mut out = vec![0u8; header.len() + row_bytes * height];
        out[0..header.len()].copy_from_slice(header.as_bytes());

        // Parallel processing for PBM packing using par_chunks_mut
        out[header.len()..]
            .par_chunks_mut(row_bytes)
            .enumerate()
            .for_each(|(y, dst)| {
                let src = &bin[y * width..y * width + width];
                for x in 0..width {
                    if src[x] == 0 {
                        dst[x >> 3] |= 0x80 >> (x & 7);
                    }
                }
            });
        out
    }

    /// Pack raw 1-bit data (no header) - parallel over rows
    ///
    /// Only exercised by `pbm_pack_handles_odd_width_msb_first` below — no
    /// production caller (production PBM output goes through `make_pbm_p4`)
    /// — so it is gated to the test build that uses it.
    #[cfg(test)]
    pub(crate) fn pack_1bit_data(bin: &[u8], width: usize, height: usize) -> Vec<u8> {
        let row_bytes = (width + 7) >> 3;
        let mut out = vec![0u8; row_bytes * height];

        // Parallel processing using par_chunks_mut
        out.par_chunks_mut(row_bytes)
            .enumerate()
            .for_each(|(y, dst)| {
                let src = &bin[y * width..y * width + width];
                for x in 0..width {
                    if src[x] == 0 {
                        dst[x >> 3] |= 0x80 >> (x & 7);
                    }
                }
            });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_threshold, binarize_gray, binarize_image_raw, improved_binarize, invert_binary,
    };
    use crate::color::BinarizationOptions;

    #[cfg(feature = "sauvola-model")]
    #[test]
    fn embedded_sauvola_model_loads_from_bytes() {
        lege_gpu::vision::SauvolaCpuProcessor::from_model_bytes(crate::EMBEDDED_SAUVOLA_MODEL)
            .expect("embedded Sauvola model should parse and pass compatibility checks");
    }

    #[test]
    fn embedded_tilenet_binarizer_loads_from_bytes() {
        lege_gpu::vision::SauvolaProcessor::from_model_bytes(crate::EMBEDDED_TILENET_BINARIZER)
            .expect("embedded tileable binarizer should parse and pass compatibility checks");
    }

    /// The tileable binarizer pads to its stride internally: a page whose sides are
    /// not multiples of 8 must come back at its own size, binary, with dark ink as
    /// ink. GPU-gated like the other wgpu tests.
    #[test]
    fn tilenet_binarizer_keeps_odd_page_size_and_polarity() {
        if std::env::var("LEGE_RUN_GPU_TESTS").ok().as_deref() != Some("1") {
            return;
        }
        let student =
            lege_gpu::vision::SauvolaProcessor::from_model_bytes(crate::EMBEDDED_TILENET_BINARIZER)
                .expect("embedded tileable binarizer loads");
        let (w, h) = (203u32, 141u32);
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let ink = (60..140).contains(&x) && (50..90).contains(&y) && (x / 6 + y / 6) % 2 == 0;
            if ink {
                image::Rgb([20, 20, 20])
            } else {
                image::Rgb([225, 215, 190])
            }
        });
        let mask = student
            .binarize_rgb_padded(&img, 64)
            .expect("student runs on wgpu");
        assert_eq!(mask.dimensions(), (w, h));
        assert!(mask.iter().all(|&v| v == 0 || v == 255));
        assert_eq!(
            mask.get_pixel(5, 5).0[0],
            255,
            "paper corner must stay paper"
        );
        let ink = mask.iter().filter(|&&v| v == 0).count();
        assert!(
            ink > 200,
            "the dark checker block must produce ink, got {ink} pixels"
        );
    }

    #[test]
    fn apply_threshold_sets_expected_binary_values() {
        let gray = vec![0u8, 100, 200, 255, 50, 150, 151, 149];
        let mut bin = vec![0u8; gray.len()];

        apply_threshold(&gray, 150, &mut bin, 4, 2);

        assert_eq!(bin, vec![0, 0, 255, 255, 0, 0, 255, 0,]);
    }

    #[test]
    fn invert_binary_flips_all_pixels_by_row() {
        let mut bin = vec![0u8, 255, 128, 1, 254, 42];
        invert_binary(&mut bin, 2, 3);
        assert_eq!(bin, vec![255, 0, 127, 254, 1, 213]);
    }

    #[test]
    fn pbm_pack_handles_odd_width_msb_first() {
        let bin = vec![0u8, 255, 0, 255, 0, 255, 0, 255, 0, 255];
        let packed = super::pbm::pack_1bit_data(&bin, 5, 2);
        assert_eq!(packed, vec![0b1010_1000, 0b0101_0000]);
    }

    #[test]
    fn improved_binarize_preserves_dimensions_for_odd_width() {
        let width = 37usize;
        let height = 29usize;
        let gray: Vec<u8> = (0..width * height)
            .map(|i| ((i * 17 + i / 3) % 256) as u8)
            .collect();
        let out = improved_binarize(&gray, width, height);
        assert_eq!(out.len(), width * height);
        assert!(out.iter().all(|&v| v == 0 || v == 255));
    }

    fn text_like_page(width: usize, height: usize) -> Vec<u8> {
        (0..width * height)
            .flat_map(|index| {
                let (x, y) = (index % width, index / width);
                let ink = (20..width - 20).contains(&x) && y % 12 < 3 && (x / 5) % 2 == 0;
                if ink { [25u8, 25, 25] } else { [228, 220, 200] }
            })
            .collect()
    }

    /// The fallback for machines without a GPU: the adaptive algorithm,
    /// reached with `disable_gpu`, keeps size, polarity and inversion.
    #[test]
    fn cpu_fallback_binarizes_text_and_honors_inversion() {
        let (width, height) = (160usize, 120usize);
        let rgb = text_like_page(width, height);
        let options = BinarizationOptions {
            disable_gpu: true,
            ..BinarizationOptions::default()
        };
        let bin = binarize_image_raw(&rgb, width, height, &options);
        assert_eq!(bin.len(), width * height);
        assert!(bin.iter().all(|&v| v == 0 || v == 255));
        assert_eq!(bin[0], 255, "paper stays paper");
        assert_eq!(bin[width + 20], 0, "ink stays ink");

        let inverted = BinarizationOptions {
            invert: true,
            ..options.clone()
        };
        let flipped = binarize_image_raw(&rgb, width, height, &inverted);
        assert!(bin.iter().zip(&flipped).all(|(a, b)| *a == 255 - *b));
    }

    /// "Fast" kept off the GPU is the adaptive CPU algorithm: the same mask
    /// as the no-GPU fallback of "Best".
    #[test]
    fn fast_on_the_cpu_is_the_adaptive_fallback() {
        let (width, height) = (160usize, 120usize);
        let rgb = text_like_page(width, height);
        let fast = BinarizationOptions {
            adaptive: true,
            disable_gpu: true,
            ..BinarizationOptions::default()
        };
        let fallback = BinarizationOptions {
            disable_gpu: true,
            ..BinarizationOptions::default()
        };
        assert_eq!(
            binarize_image_raw(&rgb, width, height, &fast),
            binarize_image_raw(&rgb, width, height, &fallback)
        );
    }

    /// "Fast" on the wgpu kernels gives a text mask of the right polarity and
    /// agrees with the CPU algorithm on nearly every pixel (the two threshold
    /// slightly different gray planes). GPU-gated.
    #[test]
    fn fast_runs_on_the_gpu_kernels() {
        if std::env::var("LEGE_RUN_GPU_TESTS").ok().as_deref() != Some("1") {
            return;
        }
        let (width, height) = (640usize, 480usize);
        let rgb = text_like_page(width, height);
        let gray: Vec<u8> = rgb
            .chunks_exact(3)
            .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
            .collect();
        let gpu = super::adaptive_on_gpu(&gray, width, height).expect("the kernels run");
        assert_eq!(gpu.len(), width * height);
        assert!(gpu.iter().all(|&v| v == 0 || v == 255));
        assert_eq!(gpu[0], 255, "paper stays paper");
        let cpu = improved_binarize(&gray, width, height);
        let differing = gpu.iter().zip(&cpu).filter(|(a, b)| a != b).count();
        assert!(
            differing * 100 <= width * height,
            "{differing} pixels differ between the kernels and the CPU algorithm"
        );
    }

    /// A blank page takes the fixed threshold and comes out white.
    #[test]
    fn blank_threshold_turns_a_noisy_blank_page_white() {
        let (width, height) = (64usize, 48usize);
        let gray: Vec<u8> = (0..width * height)
            .map(|i| 215 + ((i * 7) % 20) as u8)
            .collect();
        let options = BinarizationOptions {
            blank_threshold: Some(180),
            ..BinarizationOptions::default()
        };
        assert!(
            binarize_gray(&gray, width, height, &options)
                .iter()
                .all(|&v| v == 255)
        );
    }

    #[test]
    fn tile_spans_cover_an_axis_once_with_equal_tiles() {
        for (n, tile, halo) in [
            (900u32, 1024u32, 128u32),
            (1024, 1024, 128),
            (1025, 1024, 128),
            (3000, 1024, 128),
            (4500, 1024, 128),
            (700, 256, 32),
        ] {
            let spans = super::tile_spans(n, tile, halo);
            let mut next = 0;
            for (index, (start, length, kept)) in spans.iter().enumerate() {
                assert_eq!(start % 64, 0, "tiles start on the model's grid ({n})");
                assert_eq!(kept.start, next, "kept ranges abut ({n})");
                assert!(kept.end > kept.start && *length <= tile);
                assert!(*start <= kept.start && kept.end <= start + length);
                // Away from the page edge a kept pixel has a full halo around it.
                if index > 0 {
                    assert!(kept.start >= start + halo, "{n}: left halo");
                }
                if index + 1 < spans.len() {
                    assert!(kept.end + halo <= start + length, "{n}: right halo");
                }
                next = kept.end;
            }
            assert_eq!(next, n);
        }
    }

    /// A page binarized in tiles is the page binarized whole: the tile border
    /// that is thrown away covers the model's reach. GPU-gated.
    #[test]
    fn tiles_reproduce_the_whole_page_mask() {
        if std::env::var("LEGE_RUN_GPU_TESTS").ok().as_deref() != Some("1") {
            return;
        }
        let student =
            lege_gpu::vision::SauvolaProcessor::from_model_bytes(crate::EMBEDDED_TILENET_BINARIZER)
                .expect("embedded binarizer loads");
        let (width, height) = (1100usize, 900usize);
        // Text-like strokes over a paper gradient with a darker band.
        let rgb: Vec<u8> = (0..width * height)
            .flat_map(|index| {
                let (x, y) = (index % width, index / width);
                let paper = 200 + ((x / 40 + y / 60) % 30) as u8
                    - if (400..520).contains(&y) { 60 } else { 0 };
                let ink = y % 14 < 4 && (x / 7) % 3 != 0 && (30..width - 30).contains(&x);
                if ink {
                    [30u8, 28, 26]
                } else {
                    [paper, paper - 6, paper - 20]
                }
            })
            .collect();
        let img = image::RgbImage::from_raw(width as u32, height as u32, rgb).unwrap();
        let whole = super::binarize_on_gpu_with(&student, &img, usize::MAX, 1024, 128).unwrap();
        let tiled = super::binarize_on_gpu_with(&student, &img, 0, 512, 128).unwrap();
        let differing = whole
            .iter()
            .zip(tiled.iter())
            .filter(|(a, b)| a != b)
            .count();
        eprintln!(
            "TILE PARITY: {differing} of {} pixels differ",
            width * height
        );
        assert_eq!(
            differing, 0,
            "pixels differ between the tiled and the whole-page mask"
        );
    }

    /// With a hardware GPU the model takes the page. GPU-gated like the other
    /// wgpu tests.
    #[test]
    fn model_binarizes_text_on_the_gpu() {
        if std::env::var("LEGE_RUN_GPU_TESTS").ok().as_deref() != Some("1") {
            return;
        }
        assert!(super::binarizes_on_gpu(), "a hardware GPU is expected here");
        let (width, height) = (203usize, 141usize);
        let rgb = text_like_page(width, height);
        let bin = binarize_image_raw(&rgb, width, height, &BinarizationOptions::default());
        assert_eq!(bin.len(), width * height);
        assert!(bin.iter().all(|&v| v == 0 || v == 255));
        assert_eq!(bin[0], 255, "paper stays paper");
        assert!(bin.iter().filter(|&&v| v == 0).count() > 200);
    }
}
