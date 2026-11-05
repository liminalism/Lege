//! Sheet -> PNG, for a GUI print preview. Phase 2 aimed at a file rather
//! than a spooler.

use crate::compose::{SheetRaster, compose_sheet};
use crate::{ComposeOptions, PrintError, Sheet};

/// Preview settings. Deliberately a thin wrapper: a preview is a low-DPI
/// composition and nothing else.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewOptions {
    pub dpi: f64,
    pub grayscale: bool,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            dpi: 96.0,
            grayscale: false,
        }
    }
}

impl PreviewOptions {
    #[must_use]
    pub fn to_compose(self) -> ComposeOptions {
        ComposeOptions {
            dpi: self.dpi,
            grayscale: self.grayscale,
            ..ComposeOptions::default()
        }
    }
}

/// Compose `sheet` at preview resolution and encode it as PNG.
pub fn render_preview_png(
    session: &lege_pdf_read::RenderSession,
    sheet: &Sheet,
    options: PreviewOptions,
) -> Result<Vec<u8>, PrintError> {
    let raster = compose_sheet(session, sheet, &options.to_compose())?;
    encode_sheet_png(&raster)
}

/// Render planned sheets to PNGs on demand, in sheet order, without printer
/// access. Constructing or dropping the iterator does no rendering. Each
/// requested item reports its own error; later sheets remain available.
///
/// Use [`crate::impose`] with [`crate::Margins::ZERO`] for a printer-independent
/// plan, or pass a device-specific plan from [`crate::Spooler::plan_sheets`].
pub fn preview_sheets<'a>(
    session: &'a lege_pdf_read::RenderSession,
    sheets: &'a [Sheet],
    options: PreviewOptions,
) -> impl ExactSizeIterator<Item = Result<Vec<u8>, PrintError>> + DoubleEndedIterator + 'a {
    sheets
        .iter()
        .map(move |sheet| render_preview_png(session, sheet, options))
}

/// Encode an already-composed sheet as PNG.
///
/// Split out from [`render_preview_png`] so a caller that composed for a
/// spooler — or that wants one composition written to disk *and* handed to a
/// driver — does not compose twice. [`SheetRaster`] is tightly packed, so
/// this is the encoder and nothing else.
pub fn encode_sheet_png(raster: &SheetRaster) -> Result<Vec<u8>, PrintError> {
    let color = match raster.channels {
        1 => png::ColorType::Grayscale,
        3 => png::ColorType::Rgb,
        other => {
            return Err(PrintError::InvalidOptions(format!(
                "a sheet raster has 1 or 3 channels, not {other}"
            )));
        }
    };
    let expected = raster.stride().saturating_mul(raster.height as usize);
    if raster.pixels.len() != expected {
        return Err(PrintError::Encode(format!(
            "sheet raster is {} bytes, expected {expected} for {}x{}x{}",
            raster.pixels.len(),
            raster.width,
            raster.height,
            raster.channels
        )));
    }

    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, raster.width, raster.height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| encode_error("writing PNG header", &error))?;
    writer
        .write_image_data(&raster.pixels)
        .map_err(|error| encode_error("writing PNG pixels", &error))?;
    writer
        .finish()
        .map_err(|error| encode_error("finishing PNG stream", &error))?;
    Ok(out)
}

fn encode_error(stage: &str, error: &png::EncodingError) -> PrintError {
    PrintError::Encode(format!("{stage}: {error}"))
}
