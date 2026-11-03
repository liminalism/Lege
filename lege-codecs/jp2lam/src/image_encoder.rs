//! [`image::ImageEncoder`] for JPEG 2000, behind the `image` feature, so
//! `DynamicImage::write_with_encoder` and other `image`-crate plumbing can
//! write JP2 or J2K through jp2lam.

use std::io::Write;

use image::error::{EncodingError, ImageFormatHint, UnsupportedError, UnsupportedErrorKind};
use image::{ExtendedColorType, ImageEncoder, ImageError, ImageResult};

use crate::model::{ColorEncoding, EncodeOptions, IccComponentModel, ImageView};

/// Writes one image to `writer` with the given [`EncodeOptions`].
///
/// Accepts `L8`, `L16`, `Rgb8` and `Rgb16` samples (16-bit samples in native
/// endianness, as the `image` crate supplies them, coded at full 16-bit
/// precision), and `La8` / `Rgba8` with the alpha channel coded as a
/// straight-opacity component. Other colour types are reported as
/// unsupported.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let picture = image::open("page.png")?;
/// let mut out = std::fs::File::create("page.jp2")?;
/// picture.write_with_encoder(jp2lam::Jp2Encoder::new(&mut out))?;
/// # Ok(()) }
/// ```
pub struct Jp2Encoder<W: Write> {
    writer: W,
    options: EncodeOptions,
}

impl<W: Write> Jp2Encoder<W> {
    /// Lossless JP2 ([`EncodeOptions::default`] with lossless rate control).
    pub fn new(writer: W) -> Self {
        Self::with_options(
            writer,
            EncodeOptions {
                rate_control: Some(crate::model::RateControl::Lossless),
                ..EncodeOptions::default()
            },
        )
    }

    /// Encode with caller-chosen options.
    pub fn with_options(writer: W, options: EncodeOptions) -> Self {
        Self { writer, options }
    }
}

fn hint() -> ImageFormatHint {
    ImageFormatHint::Name("JPEG 2000".into())
}

fn unsupported_color(color_type: ExtendedColorType) -> ImageError {
    ImageError::Unsupported(UnsupportedError::from_format_and_kind(
        hint(),
        UnsupportedErrorKind::Color(color_type),
    ))
}

fn encoding_error(error: crate::error::Jp2LamError) -> ImageError {
    ImageError::Encoding(EncodingError::new(hint(), error))
}

fn native_u16(buf: &[u8]) -> Vec<u16> {
    buf.chunks_exact(2)
        .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
        .collect()
}

impl<W: Write> ImageEncoder for Jp2Encoder<W> {
    fn write_image(
        mut self,
        buf: &[u8],
        width: u32,
        height: u32,
        color_type: ExtendedColorType,
    ) -> ImageResult<()> {
        let wide;
        let view = match color_type {
            ExtendedColorType::L8 => ImageView::from_gray8(width, height, buf),
            ExtendedColorType::La8 => ImageView::from_gray_alpha8_interleaved(width, height, buf),
            ExtendedColorType::Rgb8 => ImageView::from_rgb8_interleaved(width, height, buf),
            ExtendedColorType::Rgba8 => ImageView::from_rgba8_interleaved(width, height, buf),
            ExtendedColorType::L16 => {
                wide = native_u16(buf);
                ImageView::from_gray16(width, height, &wide, 16)
            }
            ExtendedColorType::Rgb16 => {
                wide = native_u16(buf);
                ImageView::from_rgb16_interleaved(width, height, &wide, 16)
            }
            other => return Err(unsupported_color(other)),
        }
        .map_err(encoding_error)?;
        crate::encode::encode_view_to_writer(view, &self.options, &mut self.writer)
            .map_err(encoding_error)
    }

    /// Embeds `icc_profile` by the JP2 Restricted ICC method. Only gray or RGB
    /// matrix-shaper profiles qualify (see [`ColorEncoding::restricted_icc`]);
    /// the samples are not converted.
    fn set_icc_profile(&mut self, icc_profile: Vec<u8>) -> Result<(), UnsupportedError> {
        let model = match icc_profile.get(16..20) {
            Some(b"GRAY") => IccComponentModel::Gray,
            _ => IccComponentModel::Rgb,
        };
        let encoding = ColorEncoding::restricted_icc(icc_profile, model).map_err(|error| {
            UnsupportedError::from_format_and_kind(
                hint(),
                UnsupportedErrorKind::GenericFeature(error.to_string()),
            )
        })?;
        self.options.color_encoding = Some(encoding);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Jp2Encoder;
    use image::{ExtendedColorType, ImageEncoder};

    #[test]
    fn image_crate_encoder_writes_lossless_jp2_for_8_and_16_bit() {
        let gray8: Vec<u8> = (0..48u8).collect();
        let mut out = Vec::new();
        Jp2Encoder::new(&mut out)
            .write_image(&gray8, 8, 6, ExtendedColorType::L8)
            .expect("L8");
        let decoded = crate::decode_jp2(&out).expect("decode L8");
        assert_eq!(
            decoded.components[0].data,
            gray8.iter().map(|&v| i32::from(v)).collect::<Vec<_>>()
        );

        let rgb16: Vec<u16> = (0..4 * 3 * 3).map(|i| (i * 1999 % 65536) as u16).collect();
        let bytes: Vec<u8> = rgb16.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let mut out = Vec::new();
        Jp2Encoder::new(&mut out)
            .write_image(&bytes, 4, 3, ExtendedColorType::Rgb16)
            .expect("Rgb16");
        let decoded = crate::decode_jp2(&out).expect("decode Rgb16");
        for (channel, component) in decoded.components.iter().enumerate() {
            assert_eq!(component.precision, 16);
            let expected: Vec<i32> = rgb16
                .iter()
                .skip(channel)
                .step_by(3)
                .map(|&v| i32::from(v))
                .collect();
            assert_eq!(component.data, expected);
        }

        let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 37 % 256) as u8).collect();
        let mut out = Vec::new();
        Jp2Encoder::new(&mut out)
            .write_image(&rgba, 4, 3, ExtendedColorType::Rgba8)
            .expect("Rgba8");
        let meta = crate::inspect_jp2(&out).expect("inspect");
        assert_eq!(meta.in_data_alpha.map(|a| a.component), Some(3));
        let decoded = crate::decode_jp2(&out).expect("decode Rgba8");
        assert_eq!(decoded.components.len(), 4);
        for (channel, component) in decoded.components.iter().enumerate() {
            let expected: Vec<i32> = rgba
                .iter()
                .skip(channel)
                .step_by(4)
                .map(|&v| i32::from(v))
                .collect();
            assert_eq!(component.data, expected);
        }

        let err = Jp2Encoder::new(Vec::new())
            .write_image(&[0; 8], 2, 2, ExtendedColorType::Rgb32F)
            .expect_err("float samples");
        assert!(matches!(err, image::ImageError::Unsupported(_)), "{err}");
    }
}
