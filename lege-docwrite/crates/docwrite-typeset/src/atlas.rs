//! Per-glyph coverage cache. Editing looks up a glyph; it does not rasterize
//! the paragraph again.

use std::collections::HashMap;

use pixelkit_raster::{FillRule, RasterKernel};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::engine::Face;
use crate::error::TypesetError;

/// Cache key. Subpixel is quantized to a quarter pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AtlasKey {
    pub glyph: u16,
    pub size_px: u16,
    pub subpixel: u8,
}

#[derive(Clone, Debug)]
struct Entry {
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

/// Glyph atlas owned by docwrite, keyed so a keystroke reuses rasters.
#[derive(Default)]
pub struct GlyphAtlas {
    entries: HashMap<AtlasKey, Entry>,
}

impl GlyphAtlas {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Rasterize `glyph` if it is not already cached. Returns whether any
    /// coverage was produced.
    pub fn ensure(&mut self, face: &Face, glyph: u16, size_px: f32, subpixel: f32) -> Result<bool, TypesetError> {
        let key = AtlasKey {
            glyph,
            size_px: size_px.round().max(1.0) as u16,
            subpixel: (subpixel.fract() * 4.0).round() as u8,
        };
        if self.entries.contains_key(&key) {
            return Ok(self.entries.get(&key).is_some_and(|entry| entry.coverage.iter().any(|c| *c > 0)));
        }
        let entry = rasterize(face, glyph, size_px, subpixel)?;
        let ink = entry.coverage.iter().any(|sample| *sample > 0);
        self.entries.insert(key, entry);
        Ok(ink)
    }
}

struct Pen {
    points: Vec<[f32; 2]>,
    subpaths: Vec<(usize, usize)>,
    start: usize,
    cursor: Option<[f32; 2]>,
    origin_x: f32,
}

impl Pen {
    fn new(origin_x: f32) -> Self {
        Self {
            points: Vec::new(),
            subpaths: Vec::new(),
            start: 0,
            cursor: None,
            origin_x,
        }
    }

    fn push(&mut self, x: f32, y: f32) {
        // Font y grows up; the bitmap y grows down. Flip around a 64px box.
        self.points.push([x + self.origin_x, 48.0 - y]);
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
        for step in 1..=4 {
            let t = step as f32 / 4.0;
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
        for step in 1..=4 {
            let t = step as f32 / 4.0;
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
    let font = FontRef::from_index(face.bytes(), 0).map_err(|err| TypesetError::Font(err.to_string()))?;
    let outlines = font.outline_glyphs();
    let Some(outline) = outlines.get(GlyphId::new(glyph as u32)) else {
        return Ok(Entry { width: 0, height: 0, coverage: Vec::new() });
    };
    let settings = DrawSettings::unhinted(Size::new(size_px), LocationRef::default());
    let mut pen = Pen::new(subpixel.fract());
    outline
        .draw(settings, &mut pen)
        .map_err(|err| TypesetError::Font(err.to_string()))?;
    pen.close();
    let mut coverage = vec![0u8; 64 * 64];
    let mut kernel = RasterKernel::new();
    if !pen.subpaths.is_empty() {
        kernel.fill(
            &pen.points,
            &pen.subpaths,
            64,
            64,
            FillRule::NonZero,
            |y, x0, x1, row| {
                if y < 64 {
                    let end = x1.min(63);
                    let start = x0.min(end);
                    coverage[y * 64 + start..=y * 64 + end].copy_from_slice(&row[start..=end]);
                }
            },
        );
    }
    Ok(Entry { width: 64, height: 64, coverage })
}
