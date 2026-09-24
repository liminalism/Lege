//! Per-glyph coverage cache. Editing looks up a glyph; it does not rasterize
//! the paragraph again.

use std::collections::HashMap;

use pixelkit_raster::{FillRule, RasterKernel};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::engine::Face;
use crate::error::TypesetError;

/// Largest em size rasterized; bigger requests are drawn at this size.
const MAX_PX: f32 = 1024.0;

/// Cache key. Size is quantized to a quarter pixel, subpixel offset to a
/// quarter pixel, and the face is part of the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AtlasKey {
    /// The face's identity.
    pub face: u64,
    /// Glyph id.
    pub glyph: u16,
    /// Size in quarter pixels.
    pub quarter_px: u32,
    /// Subpixel offset in quarter pixels.
    pub subpixel: u8,
}

/// A rasterized glyph. Coverage is row-major, one byte per pixel.
#[derive(Clone, Copy, Debug)]
pub struct GlyphBitmap<'a> {
    /// Coverage, row-major, one byte per pixel.
    pub coverage: &'a [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixels from the pen position to the bitmap's left edge.
    pub left: i32,
    /// Pixels from the baseline up to the bitmap's top row.
    pub top: i32,
}

#[derive(Clone, Debug)]
struct Entry {
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    coverage: Vec<u8>,
}

/// Glyph atlas owned by docwrite, keyed so a keystroke reuses rasters.
#[derive(Debug, Default)]
pub struct GlyphAtlas {
    entries: HashMap<AtlasKey, Entry>,
}

impl GlyphAtlas {
    /// An empty atlas.
    pub fn new() -> Self {
        Self::default()
    }

    /// Glyph rasters held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been rasterized yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Rasterize `glyph` if it is not already cached. Returns whether it
    /// has any ink.
    pub fn ensure(
        &mut self,
        face: &Face,
        glyph: u16,
        size_px: f32,
        subpixel: f32,
    ) -> Result<bool, TypesetError> {
        self.with_glyph(face, glyph, size_px, subpixel, |bitmap| {
            bitmap.coverage.iter().any(|sample| *sample > 0)
        })
    }

    /// Rasterize `glyph` if needed, then hand its bitmap to `draw`.
    pub fn with_glyph<T>(
        &mut self,
        face: &Face,
        glyph: u16,
        size_px: f32,
        subpixel: f32,
        draw: impl FnOnce(GlyphBitmap<'_>) -> T,
    ) -> Result<T, TypesetError> {
        let size_px = size_px.clamp(1.0, MAX_PX);
        let quarter_px = (size_px * 4.0).round() as u32;
        let subpixel = (subpixel.rem_euclid(1.0) * 4.0).round() as u8 % 4;
        let key = AtlasKey {
            face: face.id(),
            glyph,
            quarter_px,
            subpixel,
        };
        if let std::collections::hash_map::Entry::Vacant(e) = self.entries.entry(key) {
            let entry = rasterize(
                face,
                glyph,
                quarter_px as f32 / 4.0,
                f32::from(subpixel) / 4.0,
            )?;
            e.insert(entry);
        }
        match self.entries.get(&key) {
            Some(entry) => Ok(draw(GlyphBitmap {
                coverage: &entry.coverage,
                width: entry.width,
                height: entry.height,
                left: entry.left,
                top: entry.top,
            })),
            None => Err(TypesetError::Font("glyph raster is missing".into())),
        }
    }
}

/// Collects an outline in pixel units, font y up, flattening curves.
struct Pen {
    points: Vec<[f32; 2]>,
    subpaths: Vec<(usize, usize)>,
    start: usize,
    cursor: Option<[f32; 2]>,
    /// Line segments per curve: more for bigger glyphs.
    steps: u32,
}

impl Pen {
    fn new(size_px: f32) -> Self {
        Self {
            points: Vec::new(),
            subpaths: Vec::new(),
            start: 0,
            cursor: None,
            steps: (size_px / 6.0).clamp(4.0, 48.0) as u32,
        }
    }

    fn push(&mut self, x: f32, y: f32) {
        self.points.push([x, y]);
        self.cursor = Some([x, y]);
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.points.len() > self.start {
            self.subpaths.push((self.start, self.points.len()));
        }
        self.start = self.points.len();
        self.push(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let Some([x0, y0]) = self.cursor else {
            self.push(x, y);
            return;
        };
        for step in 1..=self.steps {
            let t = step as f32 / self.steps as f32;
            let u = 1.0 - t;
            self.push(
                u * u * x0 + 2.0 * u * t * cx + t * t * x,
                u * u * y0 + 2.0 * u * t * cy + t * t * y,
            );
        }
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let Some([x0, y0]) = self.cursor else {
            self.push(x, y);
            return;
        };
        for step in 1..=self.steps {
            let t = step as f32 / self.steps as f32;
            let u = 1.0 - t;
            self.push(
                u * u * u * x0 + 3.0 * u * u * t * c1x + 3.0 * u * t * t * c2x + t * t * t * x,
                u * u * u * y0 + 3.0 * u * u * t * c1y + 3.0 * u * t * t * c2y + t * t * t * y,
            );
        }
    }

    fn close(&mut self) {
        if self.points.len() > self.start {
            self.subpaths.push((self.start, self.points.len()));
            self.start = self.points.len();
        }
    }
}

fn rasterize(face: &Face, glyph: u16, size_px: f32, subpixel: f32) -> Result<Entry, TypesetError> {
    let empty = Entry {
        width: 0,
        height: 0,
        left: 0,
        top: 0,
        coverage: Vec::new(),
    };
    let font =
        FontRef::from_index(face.bytes(), 0).map_err(|err| TypesetError::Font(err.to_string()))?;
    let outlines = font.outline_glyphs();
    let Some(outline) = outlines.get(GlyphId::new(u32::from(glyph))) else {
        return Ok(empty);
    };
    let settings = DrawSettings::unhinted(Size::new(size_px), LocationRef::default());
    let mut pen = Pen::new(size_px);
    outline
        .draw(settings, &mut pen)
        .map_err(|err| TypesetError::Font(err.to_string()))?;
    pen.close();
    if pen.subpaths.is_empty() || pen.points.is_empty() {
        return Ok(empty);
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for [x, y] in &pen.points {
        let x = x + subpixel;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(*y);
        max_y = max_y.max(*y);
    }
    let left = min_x.floor() as i32;
    let top = max_y.ceil() as i32;
    let width = (max_x.ceil() as i32 - left + 1).max(1) as usize;
    let height = (top - min_y.floor() as i32 + 1).max(1) as usize;
    // Bitmap space: x from the left edge, y down from the top row.
    let points: Vec<[f32; 2]> = pen
        .points
        .iter()
        .map(|[x, y]| [x + subpixel - left as f32, top as f32 - y])
        .collect();
    let mut coverage = vec![0u8; width * height];
    let mut kernel = RasterKernel::new();
    kernel.fill(
        &points,
        &pen.subpaths,
        width,
        height,
        FillRule::NonZero,
        |y, x0, x1, row| {
            if y < height && x0 <= x1 {
                let end = x1.min(width - 1);
                let start = x0.min(end);
                coverage[y * width + start..=y * width + end].copy_from_slice(&row[start..=end]);
            }
        },
    );
    Ok(Entry {
        width: width as u32,
        height: height as u32,
        left,
        top,
        coverage,
    })
}
