//! Reuse of pure sub-encodes between the measured planner's trial streams.
//!
//! The maximum-compression presets build up to `symbol_trial_limit` complete
//! candidate streams. Most of each candidate is unchanged from earlier ones:
//! a page coded as one generic region is identical for every trial that
//! chooses that layout, residual groups untouched by a proposal produce the
//! same bitmaps, and the all-global and globals-plus-locals controls encode
//! the same symbol set into a global or a page dictionary. Those encodes are
//! pure functions of their inputs, so they are computed once per flush.
//!
//! Entries are keyed by inputs that cannot change while the memo is alive
//! (page index, the ordered symbol indices of a dictionary) or by the full
//! residual bitmap, which is compared exactly on every hit. The memo only
//! changes how often an encode runs, never its bytes, and exists only for the
//! duration of one measured planning call.
use super::super::Jbig2Encoder;
use super::types::{EncodedSymbolDictionary, SymbolDictLayout};
use crate::jbig2sym::BitImage;
use anyhow::Result;
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::{Hash, Hasher};

/// Retained residual bitmap words before the region table is emptied.
const MAX_REGION_WORDS: usize = 16 << 20;

#[derive(Default)]
pub(crate) struct TrialMemo {
    pages: FxHashMap<usize, Vec<u8>>,
    regions: FxHashMap<u64, Vec<RegionEntry>>,
    region_words: usize,
    dictionaries: FxHashMap<Vec<usize>, DictionaryEntry>,
    /// Checkpointed prefixes of recent dictionary streams: a trial that drops
    /// one symbol from a dictionary re-codes only the symbols after it.
    prefixes: super::dictionary::DictionaryPrefixCache,
    /// Text-region payloads by their complete input: the all-global and
    /// globals-plus-locals controls of a one-page document assign the same
    /// dictionary positions, so they code the same region.
    text_regions: FxHashMap<u64, Vec<(TextRegionKey, Vec<u8>)>>,
}

/// Everything a text-region encode reads, in order: the page, which coder
/// (plain or refinement), the symbol-code length input, and per instance its
/// dictionary position, symbol, placement and refinement plan.
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct TextRegionKey {
    page_index: usize,
    refinement_coder: bool,
    total_dictionary_symbols: u32,
    instances: Vec<(u32, usize, u32, u32, bool, i32, i32)>,
}

impl TextRegionKey {
    pub(crate) fn new(
        page_index: usize,
        refinement_coder: bool,
        total_dictionary_symbols: u32,
        instances: &[super::types::SymbolInstance],
        global_sym_to_dict_pos: &[u32],
        local_sym_to_dict_pos: &[u32],
    ) -> Self {
        let instances = instances
            .iter()
            .map(|instance| {
                let global = global_sym_to_dict_pos
                    .get(instance.symbol_index)
                    .copied()
                    .unwrap_or(u32::MAX);
                let local = local_sym_to_dict_pos
                    .get(instance.symbol_index)
                    .copied()
                    .unwrap_or(u32::MAX);
                // The coder reads the global position when it exists and the
                // local one otherwise; both are recorded so neither mapping
                // can alias the other.
                let position = if global != u32::MAX { global } else { local };
                (
                    position,
                    instance.symbol_index,
                    instance.position.x,
                    instance.position.y,
                    instance.needs_refinement,
                    instance.refinement_dx,
                    instance.refinement_dy,
                )
            })
            .collect();
        Self {
            page_index,
            refinement_coder,
            total_dictionary_symbols,
            instances,
        }
    }

    fn digest(&self) -> u64 {
        let mut hasher = FxHasher::default();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

struct RegionEntry {
    x: u32,
    y: u32,
    bitmap: BitImage,
    payload: Vec<u8>,
}

struct DictionaryEntry {
    export_input_indices: Vec<usize>,
    encoded: EncodedSymbolDictionary,
}

fn region_key(bitmap: &BitImage, x: u32, y: u32) -> u64 {
    let mut hasher = FxHasher::default();
    (bitmap.width, bitmap.height, x, y).hash(&mut hasher);
    bitmap.packed_words().hash(&mut hasher);
    hasher.finish()
}

impl<'a> Jbig2Encoder<'a> {
    fn with_trial_memo<T>(&self, f: impl FnOnce(&mut TrialMemo) -> T) -> Option<T> {
        let memo = self.trial_memo.as_ref()?;
        let mut memo = memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(f(&mut memo))
    }

    #[cfg(feature = "symboldict")]
    pub(crate) fn begin_trial_memo(&mut self) {
        self.trial_memo = Some(std::sync::Mutex::new(TrialMemo::default()));
    }

    #[cfg(feature = "symboldict")]
    pub(crate) fn end_trial_memo(&mut self) {
        self.trial_memo = None;
    }

    /// A text-region payload already coded for exactly `key`, if any.
    pub(crate) fn cached_text_region(&self, key: &TextRegionKey) -> Option<Vec<u8>> {
        self.with_trial_memo(|memo| {
            memo.text_regions.get(&key.digest()).and_then(|entries| {
                entries
                    .iter()
                    .find(|(stored, _)| stored == key)
                    .map(|(_, payload)| payload.clone())
            })
        })
        .flatten()
    }

    pub(crate) fn remember_text_region(&self, key: TextRegionKey, payload: &[u8]) {
        self.with_trial_memo(|memo| {
            memo.text_regions
                .entry(key.digest())
                .or_default()
                .push((key, payload.to_vec()));
        });
    }

    /// Whole-page generic payload of `self.pages[page_index]` at the origin.
    pub(crate) fn page_generic_payload(&self, page_index: usize) -> Result<Vec<u8>> {
        if let Some(Some(payload)) =
            self.with_trial_memo(|memo| memo.pages.get(&page_index).cloned())
        {
            return Ok(payload);
        }
        let payload = self.encode_generic_region_payload_at(&self.pages[page_index].image, 0, 0)?;
        self.with_trial_memo(|memo| memo.pages.insert(page_index, payload.clone()));
        Ok(payload)
    }

    /// Generic payload of a residual bitmap placed at (`x`, `y`).
    pub(crate) fn region_generic_payload(
        &self,
        bitmap: &BitImage,
        x: u32,
        y: u32,
    ) -> Result<Vec<u8>> {
        if self.trial_memo.is_none() {
            return self.encode_generic_region_payload_at(bitmap, x, y);
        }
        let key = region_key(bitmap, x, y);
        let cached = self.with_trial_memo(|memo| {
            memo.regions.get(&key).and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.x == x && entry.y == y && entry.bitmap == *bitmap)
                    .map(|entry| entry.payload.clone())
            })
        });
        if let Some(Some(payload)) = cached {
            return Ok(payload);
        }
        let payload = self.encode_generic_region_payload_at(bitmap, x, y)?;
        self.with_trial_memo(|memo| {
            let words = bitmap.packed_words().len();
            if memo.region_words + words > MAX_REGION_WORDS {
                memo.regions.clear();
                memo.region_words = 0;
            }
            memo.region_words += words;
            memo.regions.entry(key).or_default().push(RegionEntry {
                x,
                y,
                bitmap: bitmap.clone(),
                payload: payload.clone(),
            });
        });
        Ok(payload)
    }

    /// Dictionary of `self.global_symbols[symbols[..]]`, in that input order.
    pub(crate) fn symbol_dictionary_for(
        &self,
        symbols: &[usize],
        layout: &SymbolDictLayout,
    ) -> Result<EncodedSymbolDictionary> {
        let refs: Vec<&BitImage> = symbols.iter().map(|&i| &self.global_symbols[i]).collect();
        // Only plain layouts are shared; the encode also depends on their order.
        if self.trial_memo.is_none() || layout.refinements.iter().any(Option::is_some) {
            return super::dictionary::encode_symbol_dictionary_segments(
                &refs,
                self.config,
                layout,
            );
        }
        let cached = self.with_trial_memo(|memo| {
            memo.dictionaries
                .get(symbols)
                .filter(|entry| entry.export_input_indices == layout.export_input_indices)
                .map(|entry| entry.encoded.clone())
        });
        if let Some(Some(encoded)) = cached {
            return Ok(encoded);
        }
        // Global symbol indices identify bitmaps for the whole planning call.
        let encoded = self
            .with_trial_memo(|memo| {
                super::dictionary::encode_symbol_dictionary_segments_resumed(
                    &refs,
                    Some((symbols, &mut memo.prefixes)),
                    self.config,
                    layout,
                )
            })
            .expect("trial memo checked above")?;
        self.with_trial_memo(|memo| {
            memo.dictionaries.insert(
                symbols.to_vec(),
                DictionaryEntry {
                    export_input_indices: layout.export_input_indices.clone(),
                    encoded: encoded.clone(),
                },
            )
        });
        Ok(encoded)
    }
}
