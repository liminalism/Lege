use super::symbol::types::PlannedDocument;
use crate::jbig2sym::BitImage;
use anyhow::{Result, bail};

/// Explicit budgets supplied by the application after measuring its readers.
/// Bitmap limits count word-aligned pixel storage, excluding allocator,
/// arithmetic-context and container overhead. Work is a pixel-operation proxy,
/// not a prediction of milliseconds or a bound on total decoder memory.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecoderResourceLimits {
    pub max_encoded_global_bytes: Option<usize>,
    pub max_global_bitmap_bytes: Option<usize>,
    /// Global plus local exported dictionary bitmaps needed by any one page.
    pub max_page_dictionary_bitmap_bytes: Option<usize>,
    pub max_cold_page_work_pixels: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct PageResourceMetrics {
    pub page_pixels: u64,
    pub local_dictionary_bitmap_bytes: usize,
    pub local_dictionary_pixels: u64,
    pub exact_instances: usize,
    pub refined_instances: usize,
    pub substituted_instances: usize,
    pub generic_instances: usize,
    /// Sum of the areas of emitted generic regions, including their white space.
    pub generic_region_pixels: u64,
    /// Sum of black source pixels assigned to generic coding. Overlaps count
    /// once per instance; this is not a union-area measurement.
    pub generic_source_ink_pixels: u64,
    /// Source-instance area; refinement adds the reference area it also reads.
    pub text_work_pixels: u64,
    /// Global/local dictionary areas + text work + generic-region areas.
    pub cold_work_pixels: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DocumentResourceMetrics {
    pub encoded_global_bytes: usize,
    pub global_dictionary_bitmap_bytes: usize,
    pub global_dictionary_pixels: u64,
    pub pages: Vec<PageResourceMetrics>,
}

pub(super) fn bitmap_bytes(image: &BitImage) -> usize {
    std::mem::size_of_val(image.packed_words())
}

pub(super) fn pixels(image: &BitImage) -> u64 {
    (image.width as u64).saturating_mul(image.height as u64)
}

/// Account for exported positions once, even if an input map aliases an export.
pub(super) fn dictionary_storage(symbols: &[BitImage], positions: &[u32]) -> (usize, u64) {
    let mut seen = rustc_hash::FxHashSet::default();
    positions
        .iter()
        .enumerate()
        .filter(|&(_, &position)| position != u32::MAX && seen.insert(position))
        .fold((0usize, 0u64), |(bytes, area), (index, _)| {
            (
                bytes.saturating_add(bitmap_bytes(&symbols[index])),
                area.saturating_add(pixels(&symbols[index])),
            )
        })
}

impl DocumentResourceMetrics {
    pub(super) fn from_plan(plan: &PlannedDocument) -> Result<Self> {
        let mut globals = Vec::new();
        for segment in &plan.global_segments {
            segment.write_into(&mut globals)?;
        }
        let mut pages: Vec<_> = plan
            .pages
            .iter()
            .map(|page| page.resources.clone())
            .collect();
        for page in &mut pages {
            page.cold_work_pixels = plan
                .global_dictionary_pixels
                .saturating_add(page.local_dictionary_pixels)
                .saturating_add(page.text_work_pixels)
                .saturating_add(page.generic_region_pixels);
        }
        Ok(Self {
            encoded_global_bytes: globals.len(),
            global_dictionary_bitmap_bytes: plan.global_bitmap_bytes,
            global_dictionary_pixels: plan.global_dictionary_pixels,
            pages,
        })
    }
}

impl DecoderResourceLimits {
    pub fn check(&self, metrics: &DocumentResourceMetrics) -> Result<()> {
        if self
            .max_encoded_global_bytes
            .is_some_and(|limit| metrics.encoded_global_bytes > limit)
        {
            bail!("encoded globals exceed caller resource limit");
        }
        if self
            .max_global_bitmap_bytes
            .is_some_and(|limit| metrics.global_dictionary_bitmap_bytes > limit)
        {
            bail!("global dictionary bitmap storage exceeds caller resource limit");
        }
        for (index, page) in metrics.pages.iter().enumerate() {
            let dictionary_bytes = metrics
                .global_dictionary_bitmap_bytes
                .saturating_add(page.local_dictionary_bitmap_bytes);
            if self
                .max_page_dictionary_bitmap_bytes
                .is_some_and(|limit| dictionary_bytes > limit)
            {
                bail!(
                    "page {} dictionary bitmap storage exceeds caller resource limit",
                    index + 1
                );
            }
            if self
                .max_cold_page_work_pixels
                .is_some_and(|limit| page.cold_work_pixels > limit)
            {
                bail!(
                    "page {} cold decode work exceeds caller resource limit",
                    index + 1
                );
            }
        }
        Ok(())
    }
}
