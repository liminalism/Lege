//! Training-resolution image export: stretch to `S × S` (Lege's runtime
//! contract stretches, it does not letterbox), with a triangle filter whose
//! support widens on downscale — the same family as PIL's `BILINEAR`, which
//! `train/eval_layout.py` uses for inference preprocessing.

use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;

pub fn stretch_image(
    src: &Path,
    dst: &Path,
    target_w: u32,
    target_h: u32,
    quality: u8,
) -> Result<(u32, u32), String> {
    let img = image::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let out = image::imageops::resize(&rgb, target_w, target_h, FilterType::Triangle);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let file = std::fs::File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    JpegEncoder::new_with_quality(&mut writer, quality)
        .encode_image(&out)
        .map_err(|e| format!("{}: {e}", dst.display()))?;
    Ok((w, h))
}
