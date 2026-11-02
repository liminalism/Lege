//! Fidelity-preserving ingest, independent of fuzzy/lossy matching decisions.
use super::super::{HashKey, Jbig2Encoder, PageData};
use super::types::SymbolInstance;
use crate::jbig2cc::analyze_page;
use crate::jbig2sym::{BitImage, Rect};
use anyhow::{Result, anyhow};
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

fn bitmap_key(bitmap: &BitImage) -> HashKey {
    let mut hasher = FxHasher::default();
    bitmap.width.hash(&mut hasher);
    bitmap.height.hash(&mut hasher);
    bitmap.packed_words().hash(&mut hasher);
    HashKey(hasher.finish())
}

impl<'a> Jbig2Encoder<'a> {
    pub(crate) fn add_page_exact_symbols(&mut self, image: BitImage) -> Result<()> {
        let page = self.pages.len();
        self.page_symbol_indices.push(Vec::new());
        let start = Instant::now();
        // Zero is a pixel-preservation policy, not a matching-quality setting.
        let components = analyze_page(&image, self.config.dpi.min(i32::MAX as u32) as i32, 0);
        let shapes = components.extract_shape_refs();
        self.metrics.symbol_stats.input_components += shapes.len();
        self.metrics.symbol_mode.cc_extraction += start.elapsed();
        let start = Instant::now();
        let mut instances = Vec::with_capacity(shapes.len());
        for shape in shapes {
            let bitmap = components
                .get_bitmap_for_cc(shape.ccid)
                .ok_or_else(|| anyhow!("nonempty exact component could not be materialized"))?;
            let key = bitmap_key(&bitmap);
            // A hash only discovers candidates. Full bitmap equality remains
            // mandatory, including on collisions and one-pixel components.
            let existing = self.hash_map.get(&key).and_then(|bucket| {
                bucket
                    .iter()
                    .copied()
                    .find(|&index| self.global_symbols[index] == bitmap)
            });
            let symbol_index = if let Some(index) = existing {
                self.symbol_usage[index] += 1;
                self.note_symbol_page(index, page);
                self.metrics.symbol_stats.exact_hits += 1;
                index
            } else {
                let pixels = bitmap.count_ones();
                let index = self.push_symbol(bitmap.clone(), pixels, page);
                self.hash_map.entry(key).or_default().push(index);
                self.metrics.symbol_stats.symbols_discovered += 1;
                index
            };
            instances.push(SymbolInstance {
                symbol_index,
                position: Rect {
                    x: shape.bbox.xmin as u32,
                    y: shape.bbox.ymin as u32,
                    width: bitmap.width as u32,
                    height: bitmap.height as u32,
                },
                instance_bitmap: bitmap,
                needs_refinement: false,
                refinement_dx: 0,
                refinement_dy: 0,
            });
        }
        self.metrics.symbol_mode.matching_dedup += start.elapsed();
        self.pages.push(PageData {
            image,
            symbol_instances: instances,
        });
        Ok(())
    }
}
