//! JPEG 2000 encoder facade backed by jp2lam.

use crate::encoding::{EncodingError, Result};

/// OpenJPEG-style JPEG 2000 parameters, translated field by field onto
/// jp2lam's `RateControl`, `CodingOptions` and `TilePolicy` by [`encode`].
///
/// `rates` is OpenJPEG's layer list: compression ratios, most compressed
/// first, one quality layer each, the last being the final stream. jp2lam has
/// no PSNR target, so `psnrs` is rejected rather than guessed at. A `rate` of
/// 1 or less, or `irreversible: Some(false)`, means lossless.
#[derive(Debug, Clone, Default)]
pub struct Jp2Settings {
    pub num_resolutions: Option<i32>,
    pub prog_order: Option<i32>,
    pub rate: Option<f32>,
    pub rates: Option<Vec<f32>>,
    pub psnrs: Option<Vec<f32>>,
    pub irreversible: Option<bool>,
    pub tile_size: Option<(i32, i32)>,
    pub codeblock: Option<(i32, i32)>,
}

pub mod jp2_config {
    use super::Jp2Settings;

    const BASE_TARGET_KB: f64 = 120.0;
    const COVER_TARGET_KB: f64 = 220.0;
    const AREA_REF_PX: f64 = 1_000_000.0;
    const IMAGE_MIN_KB: f64 = 40.0;
    const COVER_MIN_KB: f64 = 80.0;
    const IMAGE_BPPX_MIN: f64 = 0.08;
    const IMAGE_BPPX_MAX: f64 = 0.30;
    const COVER_BPPX_MIN: f64 = 0.10;
    const COVER_BPPX_MAX: f64 = 0.40;
    const SCALING_EXPONENT: f64 = 1.0;
    const IMAGE_BIAS_BETA: f64 = 0.30;
    const COVER_BIAS_BETA: f64 = 0.25;

    fn base_target_kb(area: f64, is_cover: bool) -> f64 {
        let base = if is_cover {
            COVER_TARGET_KB
        } else {
            BASE_TARGET_KB
        };
        base * (area / AREA_REF_PX).powf(SCALING_EXPONENT)
    }

    pub fn texture_score(image_data: &[u8], width: u32, height: u32, channels: u8) -> f64 {
        if width < 3 || height < 3 {
            return 0.0;
        }
        let w = width as usize;
        let h = height as usize;
        let ch = channels.max(1) as usize;
        let area = (w * h) as f64;
        let stride = (area / 100_000.0).sqrt().ceil().max(1.0) as usize;
        let xs = (1..w - 1).step_by(stride);
        let ys = (1..h - 1).step_by(stride);

        let mut sum_grad = 0.0_f64;
        let mut count = 0_u64;
        let mut nonwhite = 0_u64;

        let lum = |x: usize, y: usize| -> f64 {
            let idx = (y * w + x) * ch;
            if ch == 1 {
                image_data.get(idx).copied().unwrap_or(255) as f64
            } else {
                let r = *image_data.get(idx).unwrap_or(&255) as f64;
                let g = *image_data.get(idx + 1).unwrap_or(&255) as f64;
                let b = *image_data.get(idx + 2).unwrap_or(&255) as f64;
                0.299 * r + 0.587 * g + 0.114 * b
            }
        };

        for y in ys.clone() {
            for x in xs.clone() {
                let c = lum(x, y);
                sum_grad += (lum(x + 1, y) - c).abs() + (lum(x, y + 1) - c).abs();
                if c < 245.0 {
                    nonwhite += 1;
                }
                count += 1;
            }
        }
        if count == 0 {
            return 0.0;
        }

        let mut histogram = [0u32; 256];
        for y in ys.clone() {
            for x in xs.clone() {
                histogram[lum(x, y).round().clamp(0.0, 255.0) as usize] += 1;
            }
        }

        let total_samples = (ys.len() * xs.len()) as f64;
        let mut entropy = 0.0;
        if total_samples > 0.0 {
            for &count in &histogram {
                if count > 0 {
                    let p = count as f64 / total_samples;
                    entropy -= p * p.log2();
                }
            }
        }

        let edge_density = ((sum_grad / count as f64) / 510.0).clamp(0.0, 1.0);
        let nonwhite_ratio = (nonwhite as f64 / count as f64).clamp(0.0, 1.0);
        let entropy_norm = (entropy / 8.0).clamp(0.0, 1.0);
        (0.4 * edge_density + 0.3 * nonwhite_ratio + 0.3 * entropy_norm).clamp(0.0, 1.0)
    }

    pub fn calculate_jp2_target_bytes(height: u32, width: u32, is_cover: bool) -> usize {
        let area = (height as f64) * (width as f64);
        let min_kb_floor = if is_cover {
            if area < 250_000.0 {
                12.0
            } else if area < 1_000_000.0 {
                24.0
            } else {
                COVER_MIN_KB
            }
        } else if area < 100_000.0 {
            4.0
        } else if area < 1_000_000.0 {
            10.0
        } else {
            IMAGE_MIN_KB
        };
        let floored_kb = base_target_kb(area, is_cover).max(min_kb_floor);
        let (bppx_min, bppx_max) = if is_cover {
            (COVER_BPPX_MIN, COVER_BPPX_MAX)
        } else {
            (IMAGE_BPPX_MIN, IMAGE_BPPX_MAX)
        };
        let clamped_kb = floored_kb.clamp((bppx_min * area) / 1024.0, (bppx_max * area) / 1024.0);
        (clamped_kb * 1024.0) as usize
    }

    pub fn calculate_jp2_target_bytes_content_aware(
        image_data: &[u8],
        width: u32,
        height: u32,
        channels: u8,
        is_cover: bool,
    ) -> usize {
        let area = (height as f64) * (width as f64);
        let beta = if is_cover {
            COVER_BIAS_BETA
        } else {
            IMAGE_BIAS_BETA
        };
        let mut kb = base_target_kb(area, is_cover)
            * (1.0 + beta * texture_score(image_data, width, height, channels));
        let min_kb_floor = if is_cover {
            if area < 250_000.0 {
                12.0
            } else if area < 1_000_000.0 {
                24.0
            } else {
                COVER_MIN_KB
            }
        } else if area < 100_000.0 {
            4.0
        } else if area < 1_000_000.0 {
            10.0
        } else {
            IMAGE_MIN_KB
        };
        kb = kb.max(min_kb_floor);
        let (bppx_min, bppx_max) = if is_cover {
            (COVER_BPPX_MIN, COVER_BPPX_MAX)
        } else {
            (IMAGE_BPPX_MIN, IMAGE_BPPX_MAX)
        };
        (kb.clamp((bppx_min * area) / 1024.0, (bppx_max * area) / 1024.0) * 1024.0) as usize
    }

    pub fn calculate_jp2_rate(original_bytes: usize, target_bytes: usize) -> f32 {
        (original_bytes as f64 / target_bytes as f64).clamp(2.0, 100.0) as f32
    }

    pub fn get_jp2_settings_for_target(
        image_data: &[u8],
        width: u32,
        height: u32,
        channels: u8,
        is_cover: bool,
    ) -> Jp2Settings {
        let target_bytes =
            calculate_jp2_target_bytes_content_aware(image_data, width, height, channels, is_cover);
        let rate = calculate_jp2_rate(image_data.len(), target_bytes);
        let area = (width as u64) * (height as u64);
        Jp2Settings {
            num_resolutions: Some(6),
            prog_order: None,
            rate: Some(rate),
            rates: None,
            psnrs: None,
            irreversible: Some(true),
            tile_size: None,
            codeblock: if area < 250_000 {
                Some((32, 32))
            } else if area > 4_000_000 {
                Some((64, 64))
            } else {
                None
            },
        }
    }

    pub fn get_jp2_settings_with_logging(
        image_data: &[u8],
        width: u32,
        height: u32,
        channels: u8,
        is_cover: bool,
    ) -> Jp2Settings {
        let settings = get_jp2_settings_for_target(image_data, width, height, channels, is_cover);

        #[cfg(feature = "debug-logging")]
        crate::encoding::streamline::log_debug_message(&format!(
            "JP2 settings created: {}x{} -> target: {}KB, rate: {:.1}, num_resolutions: {:?}, irreversible: {:?}, codeblock: {:?}",
            width,
            height,
            calculate_jp2_target_bytes(height, width, is_cover) / 1024,
            settings.rate.unwrap_or(0.0),
            settings.num_resolutions,
            settings.irreversible,
            settings.codeblock
        ));

        settings
    }
}

/// jp2lam display profile for `settings`: a 1-channel source is scored as
/// e-ink display luminance, a 3-channel one as a colour panel, and the source
/// is resampled down into the box before encoding.
pub fn display_profile(
    settings: &crate::encoding::Jp2DisplaySettings,
    channels: u8,
) -> jp2lam::DisplayProfile {
    let w = settings.max_width.max(1);
    let h = settings.max_height.max(1);
    if channels == 1 {
        jp2lam::DisplayProfile::eink(w, h)
    } else {
        jp2lam::DisplayProfile::tablet(w, h)
    }
    .resampling_source()
}

/// Encode at a verified SSIMULACRA2 floor measured at the reader's display
/// size, pre-resizing the source into that box.
///
/// **The emitted stream can be smaller than `width` x `height`** — read the
/// real dimensions back with [`jp2_dimensions`] before placing it.
pub fn encode_display(
    input: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    settings: &crate::encoding::Jp2DisplaySettings,
) -> Result<Vec<u8>> {
    validate_input(input, width, height, channels)?;
    let image = match channels {
        1 => jp2lam::Image::from_gray_bytes(width, height, input),
        _ => jp2lam::Image::from_rgb_bytes(width, height, input),
    }
    .map_err(|e| EncodingError::InvalidInput(e.to_string()))?;

    let profile = display_profile(settings, channels);
    // A degenerate display box has no verified encode to try.
    let Ok(options) =
        jp2lam::EncodeOptions::display_photo(f64::from(settings.floor.min(99)), profile)
    else {
        return encode_image(&image, settings.fallback_quality);
    };
    match jp2lam::encode(&image, &options) {
        Ok(data) => Ok(data),
        // The verified search can refuse: no stream it tried cleared the floor,
        // or the source is outside what the metric scores. (A source below the
        // metric's 8x8 minimum is not a refusal: jp2lam encodes it lossless.)
        // The legacy preset keeps the page; it emits at source size, which
        // `jp2_dimensions` reports correctly.
        Err(error)
            if error.is_perceptual_target_missed()
                || matches!(error, jp2lam::Jp2LamError::InvalidInput(_)) =>
        {
            encode_image(&image, settings.fallback_quality)
        }
        // Anything else is an encoder fault, not a refusal. Keep the page, but
        // say so instead of hiding it behind the fallback.
        Err(error) => {
            crate::error_log!(
                "verified JP2 display encode failed ({error}); using the preset fallback"
            );
            encode_image(&image, settings.fallback_quality)
        }
    }
}

/// Pixel dimensions of an encoded JP2 — what the PDF XObject's `/Width` and
/// `/Height` must be. Header-only, no image reconstruction.
pub fn jp2_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let meta = jp2lam::inspect_jp2(bytes)
        .map_err(|e| EncodingError::EncoderError(format!("JP2 inspect: {e}")))?;
    Ok((meta.width, meta.height))
}

pub fn encode_rgb(data: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    let image = jp2lam::Image::from_rgb_bytes(width, height, data)
        .map_err(|e| EncodingError::InvalidInput(e.to_string()))?;
    encode_image(&image, quality)
}

pub fn encode_gray(data: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    let image = jp2lam::Image::from_gray_bytes(width, height, data)
        .map_err(|e| EncodingError::InvalidInput(e.to_string()))?;
    encode_image(&image, quality)
}

/// Encode with OpenJPEG-style [`Jp2Settings`]. Every field reaches jp2lam: a
/// `rate` is an exact compression ratio, `num_resolutions`, `prog_order`,
/// `codeblock` and `tile_size` set the codestream's coding parameters, and
/// `irreversible: Some(false)` selects lossless. Settings jp2lam cannot honour
/// (PSNR targets, lossy reversible coding) are an error, not a guess.
///
/// An explicit rate is the caller's decision and is honoured exactly, even
/// where it lands below the [`JP2_MIN_QUALITY`] preset; production paths that
/// must respect that floor use [`encode_to_target_size`] or the presets.
pub fn encode(
    input: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    settings: &Jp2Settings,
) -> Result<Vec<u8>> {
    validate_input(input, width, height, channels)?;
    let image = image_from_bytes(input, width, height, channels)?;
    let options = options_from_settings(settings)?;
    jp2lam::encode(&image, &options).map_err(|e| EncodingError::EncoderError(e.to_string()))
}

/// Translate [`Jp2Settings`] into jp2lam options. The last entry of `rates`
/// is the stream's compression ratio; the entries before it become
/// intermediate quality layers at those ratios.
fn options_from_settings(settings: &Jp2Settings) -> Result<jp2lam::EncodeOptions> {
    let invalid = |message: String| EncodingError::InvalidInput(message);
    if settings.psnrs.is_some() {
        return Err(invalid(
            "JP2 PSNR layer targets are not supported by jp2lam; use a rate".into(),
        ));
    }
    let ratio = settings.rate.or_else(|| {
        settings
            .rates
            .as_ref()
            .and_then(|rates| rates.last().copied())
    });
    // OpenJPEG's `-r` list runs from the most compressed layer to the final
    // one; jp2lam validates the intermediate entries, the final one is checked
    // against them here.
    if let Some(rates) = settings.rates.as_deref()
        && let Some(pair) = rates.windows(2).find(|pair| pair[1] >= pair[0])
    {
        return Err(invalid(format!(
            "JP2 rates must be strictly descending compression ratios, got {} then {}",
            pair[0], pair[1]
        )));
    }
    let layers = match settings.rates.as_deref() {
        Some([intermediate @ .., _final]) if !intermediate.is_empty() => {
            jp2lam::QualityLayers::Ratios(intermediate.to_vec())
        }
        _ => jp2lam::QualityLayers::One,
    };
    let rate_control = match (settings.irreversible, ratio) {
        (Some(false), Some(ratio)) if ratio > 1.0 => {
            return Err(invalid(format!(
                "JP2 rate {ratio} needs irreversible coding; jp2lam's reversible 5/3 is lossless-only"
            )));
        }
        (_, Some(ratio)) if !ratio.is_finite() => {
            return Err(invalid(format!("JP2 rate {ratio} is not finite")));
        }
        (_, Some(ratio)) if ratio > 1.0 => jp2lam::RateControl::CompressionRatio(ratio),
        // OpenJPEG's convention: a rate of 1 (or none) is a lossless layer.
        _ => jp2lam::RateControl::Lossless,
    };

    let decomposition_levels = match settings.num_resolutions {
        None => None,
        Some(resolutions @ 1..=33) => Some((resolutions - 1) as u8),
        Some(resolutions) => {
            return Err(invalid(format!(
                "JP2 num_resolutions must be in 1..=33, got {resolutions}"
            )));
        }
    };
    let progression_order = match settings.prog_order {
        None | Some(0) => jp2lam::ProgressionOrder::Lrcp,
        Some(1) => jp2lam::ProgressionOrder::Rlcp,
        Some(2) => jp2lam::ProgressionOrder::Rpcl,
        Some(3) => jp2lam::ProgressionOrder::Pcrl,
        Some(4) => jp2lam::ProgressionOrder::Cprl,
        Some(order) => {
            return Err(invalid(format!(
                "JP2 prog_order must be 0 (LRCP) to 4 (CPRL), got {order}"
            )));
        }
    };
    let positive = |(w, h): (i32, i32), what: &str| match (u32::try_from(w), u32::try_from(h)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(invalid(format!("JP2 {what} {w}x{h} must be positive"))),
    };
    let code_block_size = settings
        .codeblock
        .map(|size| positive(size, "code-block size"))
        .transpose()?;
    let tile_policy = match settings.tile_size {
        None => jp2lam::TilePolicy::Single,
        Some(size) => {
            let (width, height) = positive(size, "tile size")?;
            jp2lam::TilePolicy::Fixed { width, height }
        }
    };

    Ok(jp2lam::EncodeOptions {
        rate_control: Some(rate_control),
        format: jp2lam::OutputFormat::Jp2,
        profile: jp2lam::ContentProfile::General,
        tile_policy,
        coding: jp2lam::CodingOptions {
            decomposition_levels,
            code_block_size,
            progression_order,
            layers,
            ..jp2lam::CodingOptions::default()
        },
        ..jp2lam::EncodeOptions::default()
    })
}

fn image_from_bytes(input: &[u8], width: u32, height: u32, channels: u8) -> Result<jp2lam::Image> {
    match channels {
        1 => jp2lam::Image::from_gray_bytes(width, height, input),
        3 => jp2lam::Image::from_rgb_bytes(width, height, input),
        _ => {
            return Err(EncodingError::InvalidInput(
                "JP2 requires 1 (grayscale) or 3 (RGB) channels".to_string(),
            ));
        }
    }
    .map_err(|e| EncodingError::InvalidInput(e.to_string()))
}

pub fn encode_with_quality(
    input: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    quality: u8,
) -> Result<Vec<u8>> {
    validate_input(input, width, height, channels)?;
    match channels {
        1 => encode_gray(input, width, height, quality),
        3 => encode_rgb(input, width, height, quality),
        _ => Err(EncodingError::InvalidInput(
            "JP2 requires 1 (grayscale) or 3 (RGB) channels".to_string(),
        )),
    }
}

/// Encode to `target_bytes` with jp2lam's exact byte-target rate control, and
/// return the achieved compression ratio (source bytes / output bytes) with
/// the stream.
///
/// The program-wide quality floor still wins over the budget: when even the
/// [`JP2_MIN_QUALITY`] preset does not fit, that preset's stream is returned
/// and the target is missed, as the old quality ladder did. Otherwise the
/// output is the best stream jp2lam can fit in `target_bytes`, instead of the
/// first rung of a fixed ladder that happened to fit.
pub fn encode_to_target_size(
    input: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    target_bytes: usize,
) -> Result<(f32, Vec<u8>)> {
    validate_input(input, width, height, channels)?;
    if target_bytes == 0 {
        return Err(EncodingError::InvalidInput(
            "JP2 target size must be greater than zero".to_string(),
        ));
    }
    let image = image_from_bytes(input, width, height, channels)?;
    let floor = encode_image(&image, JP2_MIN_QUALITY)?;
    let data = if floor.len() > target_bytes {
        floor
    } else {
        let options = jp2lam::EncodeOptions {
            rate_control: Some(jp2lam::RateControl::TargetBytes(target_bytes as u64)),
            format: jp2lam::OutputFormat::Jp2,
            profile: jp2lam::ContentProfile::General,
            ..jp2lam::EncodeOptions::default()
        };
        jp2lam::encode(&image, &options).map_err(|e| EncodingError::EncoderError(e.to_string()))?
    };
    let source_bytes = width as usize * height as usize * channels as usize;
    Ok((source_bytes as f32 / data.len() as f32, data))
}

fn encode_image(image: &jp2lam::Image, quality: u8) -> Result<Vec<u8>> {
    encode_image_with_profile(image, quality, jp2lam::ContentProfile::General)
}

/// Quality floor for every jp2lam encode in Lege: output below 50 is visibly
/// artifacted and unacceptable anywhere in the program.
pub const JP2_MIN_QUALITY: u8 = 50;

fn encode_image_with_profile(
    image: &jp2lam::Image,
    quality: u8,
    profile: jp2lam::ContentProfile,
) -> Result<Vec<u8>> {
    jp2lam::encode(image, &preset_options(quality, profile))
        .map_err(|e| EncodingError::EncoderError(e.to_string()))
}

/// jp2lam options for Lege's quality presets, with the rate control spelled
/// out rather than left to `EncodeOptions::quality`'s compatibility default:
/// 100 is lossless and anything else is jp2lam's open-loop
/// `ApproxQuality` preset, floored at [`JP2_MIN_QUALITY`]. That preset is not
/// jp2lam's verified SSIMULACRA2 scale (`RateControl::Quality`) - its
/// perceptual result drifts with resolution - which is why page and region
/// images go through [`encode_display`] and only MRC background layers, whose
/// presets were tuned on this scale, stay here.
fn preset_options(quality: u8, profile: jp2lam::ContentProfile) -> jp2lam::EncodeOptions {
    let quality = quality.clamp(JP2_MIN_QUALITY, 100);
    jp2lam::EncodeOptions {
        quality,
        rate_control: Some(if quality >= 100 {
            jp2lam::RateControl::Lossless
        } else {
            jp2lam::RateControl::ApproxQuality(quality)
        }),
        format: jp2lam::OutputFormat::Jp2,
        profile,
        ..jp2lam::EncodeOptions::default()
    }
}

/// Encode a full cleaned document page (text/line art on a flat background)
/// as grayscale JP2 using jp2lam's document profile: quantization-driven rate
/// control with a light measured-ΔMSE PCRD trim. ~30–35% smaller than
/// [`encode_gray`] at equal quality on cleaned text pages. Do NOT use for
/// continuous-tone (photo/figure) regions — use [`encode_gray`] for those.
pub fn encode_gray_document(data: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    let image = jp2lam::Image::from_gray_bytes(width, height, data)
        .map_err(|e| EncodingError::InvalidInput(e.to_string()))?;
    encode_image_with_profile(&image, quality, jp2lam::ContentProfile::Document)
}

fn validate_input(input: &[u8], width: u32, height: u32, channels: u8) -> Result<()> {
    if channels != 1 && channels != 3 {
        return Err(EncodingError::InvalidInput(
            "JP2 requires 1 (grayscale) or 3 (RGB) channels".to_string(),
        ));
    }
    if width == 0 || height == 0 {
        return Err(EncodingError::InvalidDimensions {
            format: "JP2",
            width,
            height,
        });
    }
    let expected_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(channels as usize))
        .ok_or_else(|| EncodingError::InvalidInput("JP2 dimensions are too large".to_string()))?;
    if input.len() < expected_len {
        return Err(EncodingError::InvalidInput(
            "Input buffer too small for JP2 dimensions".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ComponentSampleScale {
    levels: u64,
    minimum: i64,
}

impl ComponentSampleScale {
    fn new(component: &jp2lam::Component) -> crate::encoding::Result<Self> {
        let precision = component.precision;
        if !(1..=32).contains(&precision) {
            return Err(crate::encoding::EncodingError::EncoderError(format!(
                "JP2 component precision {precision} is unsupported"
            )));
        }

        Ok(Self {
            levels: (1u64 << precision) - 1,
            minimum: if component.signed {
                -(1i64 << (precision - 1))
            } else {
                0
            },
        })
    }

    #[inline]
    fn to_u8(self, sample: i32) -> u8 {
        let normalized = (i64::from(sample) - self.minimum).clamp(0, self.levels as i64) as u64;
        ((normalized * 255) / self.levels) as u8
    }
}

fn validate_decoded_component(
    component: &jp2lam::Component,
    pixel_count: usize,
    channel_name: &str,
) -> crate::encoding::Result<()> {
    if component.width == 0 || component.height == 0 || component.data.len() < pixel_count {
        return Err(crate::encoding::EncodingError::EncoderError(format!(
            "JP2 {channel_name} component is smaller than the decoded image"
        )));
    }
    Ok(())
}

/// Convert a decoded `jp2lam::Image` to an interleaved RGB byte buffer.
/// Grayscale is expanded to RGB. High-precision samples are scaled to 8-bit.
fn jp2_image_to_rgb(img: jp2lam::Image) -> crate::encoding::Result<(u32, u32, Vec<u8>)> {
    let width = img.width;
    let height = img.height;
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| {
            crate::encoding::EncodingError::EncoderError(
                "JP2 decoded dimensions are too large".into(),
            )
        })?;
    let rgb_capacity = pixel_count.checked_mul(3).ok_or_else(|| {
        crate::encoding::EncodingError::EncoderError(
            "JP2 decoded RGB buffer would be too large".into(),
        )
    })?;

    match img.colorspace {
        jp2lam::ColorSpace::Gray => {
            let comp = img.components.into_iter().next().ok_or_else(|| {
                crate::encoding::EncodingError::EncoderError(
                    "JP2 gray image has no component".into(),
                )
            })?;
            validate_decoded_component(&comp, pixel_count, "gray")?;
            let scale = ComponentSampleScale::new(&comp)?;
            let mut rgb = Vec::with_capacity(rgb_capacity);
            for sample in comp.data.iter().take(pixel_count) {
                let v = scale.to_u8(*sample);
                rgb.push(v);
                rgb.push(v);
                rgb.push(v);
            }
            Ok((width, height, rgb))
        }
        jp2lam::ColorSpace::Srgb | jp2lam::ColorSpace::Rgb => {
            let mut comps = img.components.into_iter();
            let r_comp = comps.next().ok_or_else(|| {
                crate::encoding::EncodingError::EncoderError(
                    "JP2 RGB image missing R component".into(),
                )
            })?;
            let g_comp = comps.next().ok_or_else(|| {
                crate::encoding::EncodingError::EncoderError(
                    "JP2 RGB image missing G component".into(),
                )
            })?;
            let b_comp = comps.next().ok_or_else(|| {
                crate::encoding::EncodingError::EncoderError(
                    "JP2 RGB image missing B component".into(),
                )
            })?;
            validate_decoded_component(&r_comp, pixel_count, "red")?;
            validate_decoded_component(&g_comp, pixel_count, "green")?;
            validate_decoded_component(&b_comp, pixel_count, "blue")?;
            let r_scale = ComponentSampleScale::new(&r_comp)?;
            let g_scale = ComponentSampleScale::new(&g_comp)?;
            let b_scale = ComponentSampleScale::new(&b_comp)?;
            let mut rgb = Vec::with_capacity(rgb_capacity);
            for i in 0..pixel_count {
                let r = r_scale.to_u8(r_comp.data[i]);
                let g = g_scale.to_u8(g_comp.data[i]);
                let b = b_scale.to_u8(b_comp.data[i]);
                rgb.push(r);
                rgb.push(g);
                rgb.push(b);
            }
            Ok((width, height, rgb))
        }
        other => Err(crate::encoding::EncodingError::EncoderError(format!(
            "JP2 colorspace {other:?} is not supported for decoding"
        ))),
    }
}

/// Decode a JP2 file from in-memory bytes to an interleaved RGB byte buffer.
///
/// Returns `(width, height, rgb_bytes)` where `rgb_bytes` has length `width * height * 3`.
/// Grayscale JP2 images are expanded to RGB by repeating the luma channel.
/// High-precision (>8-bit) samples are scaled down to 8-bit range.
///
/// Uses the jp2lam batch API internally. For processing many images from the same
/// source use [`Jp2BatchDecoder`] to benefit from profile consistency validation.
pub fn decode_jp2_bytes(bytes: &[u8]) -> crate::encoding::Result<(u32, u32, Vec<u8>)> {
    let img = jp2lam::BatchDecoder::new()
        .decode_one(bytes)
        .map_err(|e| crate::encoding::EncodingError::EncoderError(format!("JP2 decode: {e}")))?;
    jp2_image_to_rgb(img)
}

/// Stateful encoder that validates all images share the same profile.
/// Create one instance per processing job and call [`encode_one`](Jp2BatchEncoder::encode_one)
/// for each image from that source. Enforces consistent dimensions and colour space.
pub struct Jp2BatchEncoder {
    inner: jp2lam::BatchEncoder,
}

impl Jp2BatchEncoder {
    pub fn new(quality: u8) -> Self {
        Self {
            inner: jp2lam::BatchEncoder::new(preset_options(
                quality,
                jp2lam::ContentProfile::General,
            )),
        }
    }

    /// Encode one image and validate it matches the profile of previously encoded images.
    pub fn encode_one(
        &mut self,
        input: &[u8],
        width: u32,
        height: u32,
        channels: u8,
    ) -> Result<Vec<u8>> {
        validate_input(input, width, height, channels)?;
        let image = match channels {
            1 => jp2lam::Image::from_gray_bytes(width, height, input)
                .map_err(|e| EncodingError::InvalidInput(e.to_string()))?,
            3 => jp2lam::Image::from_rgb_bytes(width, height, input)
                .map_err(|e| EncodingError::InvalidInput(e.to_string()))?,
            _ => unreachable!(),
        };
        self.inner
            .encode_one(&image)
            .map_err(|e| EncodingError::EncoderError(e.to_string()))
    }
}

/// Stateful decoder that validates all images share the same profile.
/// Create one instance per processing job and call [`decode_one`](Jp2BatchDecoder::decode_one)
/// for each image from that source. Enforces consistent dimensions and colour space.
pub struct Jp2BatchDecoder {
    inner: jp2lam::BatchDecoder,
}

impl Jp2BatchDecoder {
    pub fn new() -> Self {
        Self {
            inner: jp2lam::BatchDecoder::new(),
        }
    }

    /// Decode one JP2 image and validate it matches the profile of previously decoded images.
    /// Returns `(width, height, rgb_bytes)`.
    pub fn decode_one(&mut self, bytes: &[u8]) -> crate::encoding::Result<(u32, u32, Vec<u8>)> {
        let img = self.inner.decode_one(bytes).map_err(|e| {
            crate::encoding::EncodingError::EncoderError(format!("JP2 batch decode: {e}"))
        })?;
        jp2_image_to_rgb(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(data: Vec<i32>, precision: u32, signed: bool) -> jp2lam::Component {
        jp2lam::Component {
            data,
            width: 1,
            height: 1,
            precision,
            signed,
            dx: 1,
            dy: 1,
        }
    }

    #[test]
    fn decoded_rgb_scales_each_component_at_its_own_precision() {
        let image = jp2lam::Image {
            width: 1,
            height: 1,
            components: vec![
                component(vec![255], 8, false),
                component(vec![15], 4, false),
                component(vec![1023], 10, false),
            ],
            colorspace: jp2lam::ColorSpace::Rgb,
        };

        let (_, _, rgb) = jp2_image_to_rgb(image).expect("mixed precision RGB");
        assert_eq!(rgb, vec![255, 255, 255]);
    }

    #[test]
    fn decoded_component_length_is_validated_before_indexing() {
        let image = jp2lam::Image {
            width: 2,
            height: 1,
            components: vec![
                jp2lam::Component {
                    width: 2,
                    ..component(vec![255], 8, false)
                },
                jp2lam::Component {
                    width: 2,
                    ..component(vec![255, 255], 8, false)
                },
                jp2lam::Component {
                    width: 2,
                    ..component(vec![255, 255], 8, false)
                },
            ],
            colorspace: jp2lam::ColorSpace::Rgb,
        };

        assert!(jp2_image_to_rgb(image).is_err());
    }

    fn textured_rgb(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|i| {
                let (x, y) = (i % width, i / width);
                let t = ((x * 7 + y * 3) % 23) as u8;
                [
                    (x * 200 / width) as u8 + t,
                    (y * 200 / height) as u8 + t,
                    ((x + y) * 100 / (width + height)) as u8 + 2 * t,
                ]
            })
            .collect()
    }

    /// Every `Jp2Settings` field used to be dropped except `rate`, which went
    /// through a made-up rate-to-quality curve and always landed on the q50
    /// floor. Each field now reaches the codestream.
    #[test]
    fn jp2_settings_reach_the_codestream() {
        let (w, h) = (160u32, 120u32);
        let rgb = textured_rgb(w, h);
        let settings = Jp2Settings {
            num_resolutions: Some(4),
            prog_order: Some(4),
            rate: Some(12.0),
            irreversible: Some(true),
            tile_size: Some((128, 128)),
            codeblock: Some((32, 16)),
            ..Jp2Settings::default()
        };
        let data = encode(&rgb, w, h, 3, &settings).expect("settings encode");
        let meta = jp2lam::inspect_jp2(&data).expect("inspect");
        let cod = &meta.codestream.cod;
        assert_eq!(cod.decomposition_levels, 3);
        assert_eq!(cod.progression_order, jp2lam::ProgressionOrder::Cprl);
        assert_eq!((cod.code_block_width, cod.code_block_height), (32, 16));
        assert_eq!(meta.codestream.siz.tile_width, 128);
        let ratio = rgb.len() as f64 / data.len() as f64;
        assert!((ratio - 12.0).abs() < 0.6, "ratio {ratio}");
        decode_jp2_bytes(&data).expect("decodes");
    }

    #[test]
    fn jp2_rates_list_becomes_quality_layers() {
        let (w, h) = (120u32, 90u32);
        let rgb = textured_rgb(w, h);
        let data = encode(
            &rgb,
            w,
            h,
            3,
            &Jp2Settings {
                rates: Some(vec![40.0, 20.0, 10.0]),
                ..Jp2Settings::default()
            },
        )
        .expect("layered encode");
        let meta = jp2lam::inspect_jp2(&data).expect("inspect");
        assert_eq!(meta.codestream.cod.layers, 3);
        let ratio = rgb.len() as f64 / data.len() as f64;
        assert!((ratio - 10.0).abs() < 0.6, "ratio {ratio}");
        // Ascending ratios are not a layer ladder.
        assert!(
            encode(
                &rgb,
                w,
                h,
                3,
                &Jp2Settings {
                    rates: Some(vec![10.0, 20.0]),
                    ..Jp2Settings::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn jp2_settings_lossless_and_refusals() {
        let (w, h) = (40u32, 30u32);
        let rgb = textured_rgb(w, h);
        for settings in [
            Jp2Settings::default(),
            Jp2Settings {
                irreversible: Some(false),
                ..Jp2Settings::default()
            },
            Jp2Settings {
                rate: Some(1.0),
                ..Jp2Settings::default()
            },
        ] {
            let data = encode(&rgb, w, h, 3, &settings).expect("lossless");
            assert_eq!(
                decode_jp2_bytes(&data).expect("decode").2,
                rgb,
                "{settings:?}"
            );
        }
        for settings in [
            Jp2Settings {
                psnrs: Some(vec![40.0]),
                ..Jp2Settings::default()
            },
            Jp2Settings {
                irreversible: Some(false),
                rate: Some(10.0),
                ..Jp2Settings::default()
            },
            Jp2Settings {
                prog_order: Some(5),
                ..Jp2Settings::default()
            },
            Jp2Settings {
                codeblock: Some((48, 48)),
                ..Jp2Settings::default()
            },
        ] {
            assert!(encode(&rgb, w, h, 3, &settings).is_err(), "{settings:?}");
        }
    }

    /// The old ladder stepped through seven encodes and returned whichever
    /// fixed quality landed nearest; the byte target is now jp2lam's own.
    #[test]
    fn target_size_is_met_by_one_exact_encode() {
        let (w, h) = (200u32, 150u32);
        let rgb = textured_rgb(w, h);
        let target = 6_000;
        let (ratio, data) = encode_to_target_size(&rgb, w, h, 3, target).expect("target");
        assert!(
            data.len() <= target && data.len() + 64 >= target,
            "{} bytes",
            data.len()
        );
        assert!((ratio - rgb.len() as f32 / data.len() as f32).abs() < 1e-3);

        // Below what the q50 floor needs, the floor wins and the target is missed.
        let floor = encode_with_quality(&rgb, w, h, 3, JP2_MIN_QUALITY).expect("floor");
        let (_, starved) = encode_to_target_size(&rgb, w, h, 3, 200).expect("starved");
        assert_eq!(starved, floor);
    }

    #[test]
    fn preset_rate_control_is_explicit() {
        let lossless = preset_options(100, jp2lam::ContentProfile::General);
        assert_eq!(lossless.rate_control, Some(jp2lam::RateControl::Lossless));
        let floored = preset_options(20, jp2lam::ContentProfile::Document);
        assert_eq!(
            floored.rate_control,
            Some(jp2lam::RateControl::ApproxQuality(JP2_MIN_QUALITY))
        );
    }

    #[test]
    fn jp2_input_rejects_zero_dimensions() {
        assert!(matches!(
            validate_input(&[], 0, 1, 1),
            Err(EncodingError::InvalidDimensions { .. })
        ));
    }
}

#[cfg(test)]
mod display_tests {
    use crate::encoding::Jp2DisplaySettings;

    /// The emitted JP2 must carry the box dimensions, not the source ones —
    /// the PDF XObject is sized from them.
    #[test]
    fn display_encode_emits_the_box_size_and_reports_it() {
        let (w, h) = (320u32, 240u32);
        let rgb: Vec<u8> = (0..w * h * 3)
            .map(|i| ((i * 7 % 251) as u8).wrapping_add((i / 97) as u8))
            .collect();
        let settings = Jp2DisplaySettings {
            max_width: 80,
            max_height: 60,
            floor: Jp2DisplaySettings::DEFAULT_FLOOR,
            fallback_quality: 72,
        };
        assert_eq!(settings.encoded_size(w, h), (80, 60));

        let data = super::encode_display(&rgb, w, h, 3, &settings).expect("display encode");
        assert_eq!(super::jp2_dimensions(&data).expect("dims"), (80, 60));

        // A box the source already fits must not resample or upscale.
        let same = Jp2DisplaySettings {
            max_width: w,
            max_height: h,
            ..settings
        };
        assert_eq!(same.encoded_size(w, h), (w, h));
    }
}
