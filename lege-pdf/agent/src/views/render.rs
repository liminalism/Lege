use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RenderPageData {
    pub unit: &'static str,
    /// Where the image was written; empty when it was returned inline.
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub dpi: f64,
    pub scale: f64,
    pub format: String,
    /// What the file stores: `gray`, `rgb` or `rgba`. An opaque page whose
    /// pixels are all neutral is written as `gray`.
    pub color: &'static str,
    /// Size of the encoded image.
    pub bytes: u64,
    pub degraded_draws: u32,
    pub recovery_notes: Vec<String>,
    /// For a cropped render, the PDF-space rectangle (points) the image
    /// covers after clamping to the page and snapping to whole pixels.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::rounded_box_opt"
    )]
    pub region: Option<[f64; 4]>,
}

/// Several pages tiled into one image.
#[derive(Debug, Clone, Serialize)]
pub struct ContactSheetData {
    pub unit: &'static str,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub color: &'static str,
    pub bytes: u64,
    pub columns: u32,
    pub rows: u32,
    /// Where each page sits on the sheet, in reading order.
    pub pages: Vec<SheetCell>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SheetCell {
    pub page: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
