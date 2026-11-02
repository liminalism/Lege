use super::super::first_black_pixel;
#[cfg(all(test, feature = "refine"))]
use super::text_region::{uf_find, uf_union};
#[cfg(all(test, feature = "refine"))]
use super::types::RefinementPlan;
use super::types::{
    EncodedSymbolDictionary, SymbolDictDiagnostics, SymbolDictLayout, SymbolInstance,
};
use crate::jbig2arith::{IntProc, Jbig2ArithCoder};
#[cfg(all(test, feature = "refine"))]
use crate::jbig2classify::{
    FamilyBucketKey, SymbolSignature, compute_symbol_signature as compute_symbol_signature_shared,
    family_bucket_key_for_symbol, family_bucket_neighbors, family_match_details,
    refine_compare_score,
};
use crate::jbig2comparator::{Comparator, MAX_DIMENSION_DELTA};
#[cfg(all(test, feature = "refine"))]
use crate::jbig2cost::symbol_dictionary_entry_bytes;
use crate::jbig2structs::{Jbig2Config, SymbolDictParams};
use crate::jbig2sym::{BitImage, Rect};
use crate::{debug, trace};
use anyhow::{Result, anyhow};
#[cfg(all(test, feature = "refine"))]
use std::collections::HashMap;

pub fn encode_symbol_dict(
    symbols: &[&BitImage],
    _config: &Jbig2Config,
    num_imported_symbols: u32,
) -> Result<Vec<u8>> {
    let (payload, _order) = encode_symbol_dict_with_order(symbols, _config, num_imported_symbols)?;
    Ok(payload)
}

/// Computes the canonical encoding order for a list of symbols.
///
/// Returns a `Vec<usize>` where each element is an index into the input `symbols` slice,
/// giving the order symbols will appear in the encoded dictionary (after filtering out
/// zero-size symbols, deduplication, and sorting by height class then width).
///
/// This order must be used when mapping symbol instance IDs in text regions.
pub fn canonicalize_dict_symbols(symbols: &[&BitImage]) -> Vec<usize> {
    // Step 1: Filter zero-size, tracking original indices
    let mut valid: Vec<(usize, &BitImage)> = symbols
        .iter()
        .enumerate()
        .filter(|(_, sym)| sym.width > 0 && sym.height > 0)
        .map(|(i, sym)| (i, *sym))
        .collect();

    // Step 2: Sort by (height ASC, width ASC) — same order as sort_symbols_for_dictionary
    // Use stable sort to preserve input order for identical dimensions.
    // No dedup here: the encoder already deduplicates during extraction + auto_threshold.
    // Removing a symbol would leave text region instances without a valid dictionary mapping.
    valid.sort_by(|a, b| (a.1.height, a.1.width).cmp(&(b.1.height, b.1.width)));

    // Return original indices in canonical order
    valid.into_iter().map(|(orig_idx, _)| orig_idx).collect()
}

pub(crate) fn plan_symbol_dictionary_layout(
    symbols: &[&BitImage],
    config: &Jbig2Config,
    usage_weights: Option<&[usize]>,
) -> Result<SymbolDictLayout> {
    let canonical_order = canonicalize_dict_symbols(symbols);
    if canonical_order.is_empty() {
        return Err(anyhow!(
            "encode_symbol_dict: no valid symbols supplied (all symbols had zero width or height)"
        ));
    }

    let _ = (config, usage_weights);
    Ok(SymbolDictLayout {
        export_input_indices: canonical_order,
        refinements: vec![None; symbols.len()],
        diagnostics: SymbolDictDiagnostics {
            singleton_family_count: symbols.len(),
            exported_member_count: symbols.len(),
            ..Default::default()
        },
    })
}

/// Dictionary-level refinement-family collapse policy. Not used by the
/// production pipeline: `plan_symbol_dictionary_layout` is a deliberate stub
/// that never collapses families, because the encoder performs refinement at
/// the text-region instance level instead, not in the dictionary (see the
/// Phase 3 findings in jbig2dec-gaps-plan.md; commit 4977e24). Kept as a
/// tested reference implementation behind the `refine` feature — see
/// `tests.rs`'s `refinement_layout_collapses_to_prototypes`.
#[cfg(all(test, feature = "refine"))]
pub(crate) fn build_refinement_family_layout(
    symbols: &[&BitImage],
    canonical_order: &[usize],
    usage_weights: Option<&[usize]>,
) -> SymbolDictLayout {
    let mut comparator = Comparator::default();
    let signatures: Vec<SymbolSignature> = symbols
        .iter()
        .map(|sym| compute_symbol_signature_shared(sym))
        .collect();
    let black_counts: Vec<usize> = symbols.iter().map(|sym| sym.count_ones()).collect();

    let mut canonical_pos = vec![usize::MAX; symbols.len()];
    for (pos, &input_index) in canonical_order.iter().enumerate() {
        canonical_pos[input_index] = pos;
    }

    let mut bucket_map: HashMap<FamilyBucketKey, Vec<usize>> = HashMap::new();
    for &input_index in canonical_order {
        let key = family_bucket_key_for_symbol(symbols[input_index], &signatures[input_index]);
        bucket_map.entry(key).or_default().push(input_index);
    }

    let mut parent: Vec<usize> = (0..symbols.len()).collect();
    let mut rank = vec![0u32; symbols.len()];

    for &input_index in canonical_order {
        let symbol = symbols[input_index];
        let key = family_bucket_key_for_symbol(symbol, &signatures[input_index]);

        for neighbor in family_bucket_neighbors(key) {
            let Some(bucket) = bucket_map.get(&neighbor) else {
                continue;
            };
            for &other_input_index in bucket {
                if canonical_pos[other_input_index] >= canonical_pos[input_index] {
                    continue;
                }
                if family_match_details(
                    &mut comparator,
                    symbol,
                    input_index,
                    symbols[other_input_index],
                    other_input_index,
                    &signatures,
                    &black_counts,
                )
                .is_some()
                {
                    uf_union(&mut parent, &mut rank, input_index, other_input_index);
                }
            }
        }
    }

    let mut families: HashMap<usize, Vec<usize>> = HashMap::new();
    for &input_index in canonical_order {
        let root = uf_find(&mut parent, input_index);
        families.entry(root).or_default().push(input_index);
    }

    let mut export_input_indices = Vec::new();
    let mut refinements = vec![None; symbols.len()];
    let mut diagnostics = SymbolDictDiagnostics::default();

    let mut family_members: Vec<Vec<usize>> = families.into_values().collect();
    family_members.sort_by_key(|members| canonical_pos[members[0]]);
    diagnostics.family_count = family_members.len();

    for mut members in family_members {
        members.sort_by_key(|&input_index| canonical_pos[input_index]);
        if members.len() == 1 {
            diagnostics.singleton_family_count += 1;
            diagnostics.exported_member_count += 1;
            export_input_indices.push(members[0]);
            continue;
        }

        let prototype_input_index = choose_family_prototype(
            &members,
            symbols,
            usage_weights,
            &canonical_pos,
            &signatures,
            &black_counts,
        );
        if diagnostics.sample_lines.len() < 128 {
            diagnostics.sample_lines.push(format!(
                "refine family: prototype={} members={} prototype_usage={}",
                prototype_input_index,
                members.len(),
                usage_weights
                    .and_then(|weights| weights.get(prototype_input_index).copied())
                    .unwrap_or(1)
            ));
        }
        export_input_indices.push(prototype_input_index);
        diagnostics.exported_member_count += 1;

        for &member_input_index in &members {
            if member_input_index == prototype_input_index {
                continue;
            }

            let maybe_match = family_match_details(
                &mut comparator,
                symbols[member_input_index],
                member_input_index,
                symbols[prototype_input_index],
                prototype_input_index,
                &signatures,
                &black_counts,
            );

            match maybe_match {
                Some((err, dx, dy))
                    if family_should_refine(
                        symbols[member_input_index],
                        symbols[prototype_input_index],
                        err,
                        dx,
                        dy,
                        usage_weights
                            .and_then(|weights| weights.get(member_input_index).copied())
                            .unwrap_or(1),
                    ) =>
                {
                    refinements[member_input_index] = Some(RefinementPlan {
                        prototype_input_index,
                        refinement_dx: dx,
                        refinement_dy: dy,
                    });
                    diagnostics.refined_member_count += 1;
                    if diagnostics.sample_lines.len() < 128 {
                        diagnostics.sample_lines.push(format!(
                            "  refine member={} -> prototype={} dx={} dy={} err={} usage={}",
                            member_input_index,
                            prototype_input_index,
                            dx,
                            dy,
                            err,
                            usage_weights
                                .and_then(|weights| weights.get(member_input_index).copied())
                                .unwrap_or(1)
                        ));
                    }
                }
                _ => {
                    export_input_indices.push(member_input_index);
                    diagnostics.exported_member_count += 1;
                    if diagnostics.sample_lines.len() < 128 {
                        diagnostics.sample_lines.push(format!(
                            "  export member={} as standalone usage={}",
                            member_input_index,
                            usage_weights
                                .and_then(|weights| weights.get(member_input_index).copied())
                                .unwrap_or(1)
                        ));
                    }
                }
            }
        }
    }

    export_input_indices.sort_by_key(|&input_index| canonical_pos[input_index]);

    SymbolDictLayout {
        export_input_indices,
        refinements,
        diagnostics,
    }
}

#[cfg(all(test, feature = "refine"))]
fn family_refinement_gain(
    target: &BitImage,
    reference: &BitImage,
    err: u32,
    dx: i32,
    dy: i32,
) -> i64 {
    let plain_cost = symbol_dictionary_entry_bytes(target) as i64 + 10;
    let refine_cost = 10
        + err as i64
        + ((dx.abs() + dy.abs()) as i64 * 3)
        + (target.width.abs_diff(reference.width) + target.height.abs_diff(reference.height))
            as i64
            * 2;
    plain_cost - refine_cost
}

#[cfg(all(test, feature = "refine"))]
fn family_should_refine(
    target: &BitImage,
    reference: &BitImage,
    err: u32,
    dx: i32,
    dy: i32,
    usage_count: usize,
) -> bool {
    if usage_count > 1 {
        return false;
    }
    let export_gain = family_refinement_gain(target, reference, err, dx, dy);
    export_gain > 12
}

#[cfg(all(test, feature = "refine"))]
fn choose_family_prototype(
    members: &[usize],
    symbols: &[&BitImage],
    usage_weights: Option<&[usize]>,
    canonical_pos: &[usize],
    signatures: &[SymbolSignature],
    black_counts: &[usize],
) -> usize {
    if members.len() == 1 {
        return members[0];
    }

    let mut comparator = Comparator::default();
    let mut best_idx = members[0];
    let mut best_cost = u64::MAX;
    let mut best_support = 0u64;

    for &candidate in members {
        let mut total_cost = 0u64;
        for &other in members {
            if candidate == other {
                continue;
            }
            let weight = usage_weights
                .and_then(|weights| weights.get(other).copied())
                .unwrap_or(1) as u64;
            match family_match_details(
                &mut comparator,
                symbols[other],
                other,
                symbols[candidate],
                candidate,
                signatures,
                black_counts,
            ) {
                Some((err, dx, dy)) => {
                    total_cost += (refine_compare_score(err, dx, dy) as u64 + 4) * weight;
                }
                None => total_cost += 1_000_000 * weight,
            }
        }

        let candidate_support = usage_weights
            .and_then(|weights| weights.get(candidate).copied())
            .unwrap_or(1) as u64;
        let score_close = if best_cost == u64::MAX {
            false
        } else {
            total_cost <= best_cost + best_cost / 50
        };

        if total_cost < best_cost
            || (score_close && candidate_support > best_support)
            || (total_cost == best_cost
                && candidate_support == best_support
                && canonical_pos[candidate] < canonical_pos[best_idx])
        {
            best_cost = total_cost;
            best_idx = candidate;
            best_support = candidate_support;
        }
    }

    best_idx
}

pub(crate) fn encode_symbol_dictionary_segments(
    symbols: &[&BitImage],
    config: &Jbig2Config,
    layout: &SymbolDictLayout,
) -> Result<EncodedSymbolDictionary> {
    encode_symbol_dictionary_segments_resumed(symbols, None, config, layout)
}

/// `encode_symbol_dictionary_segments`, resuming the plain base dictionary
/// from a prefix cache when one is supplied with the symbols' identities.
pub(crate) fn encode_symbol_dictionary_segments_resumed(
    symbols: &[&BitImage],
    resume: Option<(&[usize], &mut DictionaryPrefixCache)>,
    config: &Jbig2Config,
    layout: &SymbolDictLayout,
) -> Result<EncodedSymbolDictionary> {
    let mut encoded = EncodedSymbolDictionary {
        payload: Vec::new(),
        input_to_exported_pos: vec![u32::MAX; symbols.len()],
        exported_symbol_count: 0,
    };

    let subset = &layout.export_input_indices;
    let subset_symbols: Vec<&BitImage> = subset.iter().map(|&i| symbols[i]).collect();
    let (dict_payload, subset_order) = match resume {
        Some((identities, cache)) => {
            let subset_identities: Vec<usize> = subset.iter().map(|&i| identities[i]).collect();
            encode_symbol_dict_with_order_resumed(
                &subset_symbols,
                &subset_identities,
                config,
                0,
                cache,
            )?
        }
        None => encode_symbol_dict_with_order(&subset_symbols, config, 0)?,
    };
    let base_order: Vec<usize> = subset_order.into_iter().map(|i| subset[i]).collect();
    for (dict_pos, &input_index) in base_order.iter().enumerate() {
        encoded.input_to_exported_pos[input_index] = dict_pos as u32;
    }
    encoded.exported_symbol_count = base_order.len() as u32;
    encoded.payload = dict_payload;

    for (input_index, refinement) in layout.refinements.iter().enumerate() {
        if let Some(refinement) = refinement {
            let prototype_pos = encoded.input_to_exported_pos[refinement.prototype_input_index];
            if prototype_pos != u32::MAX {
                encoded.input_to_exported_pos[input_index] = prototype_pos;
            }
        }
    }

    Ok(encoded)
}

/// The symbol dictionary stream in construction: symbols are pushed in
/// canonical (height, width) order and the height-class structure, the
/// width deltas and the bitmap coding are emitted as each one arrives, so the
/// state after any symbol is a complete prefix that can be cloned and resumed.
/// Pushing every symbol then calling [`DictionaryStream::finish`] produces
/// exactly the bytes of the former one-shot encoder.
#[derive(Clone)]
pub(crate) struct DictionaryStream {
    coder: Jbig2ArithCoder,
    last_height: i32,
    last_width: i32,
    class_open: bool,
    pushed: u32,
}

impl DictionaryStream {
    pub(crate) fn new() -> Self {
        Self {
            coder: Jbig2ArithCoder::new(),
            last_height: 0,
            last_width: 0,
            class_open: false,
            pushed: 0,
        }
    }

    /// Symbols pushed so far.
    pub(crate) fn len(&self) -> usize {
        self.pushed as usize
    }

    /// Code the next symbol in canonical order.
    pub(crate) fn push(&mut self, symbol: &BitImage) -> Result<()> {
        if symbol.width > (1 << 24) || symbol.height > (1 << 24) {
            return Err(anyhow!(
                "Symbol at index {} exceeds maximum dimensions ({}x{})",
                self.pushed,
                symbol.width,
                symbol.height
            ));
        }
        let height = symbol.height as i32;
        if self.class_open && height != self.last_height {
            // OOB marks the end of the previous height class.
            self.coder.encode_oob(IntProc::Iadw)?;
            self.class_open = false;
        }
        if !self.class_open {
            // A. Encode Delta Height
            self.coder
                .encode_integer(IntProc::Iadh, height - self.last_height)?;
            self.last_height = height;
            self.last_width = 0;
            self.class_open = true;
        }

        // I. Encode Delta Width
        let delta_w = symbol.width as i32 - self.last_width;
        debug!(
            "Height class {}, Symbol {}: width={}, last_width={}, delta_w={}",
            height, self.pushed, symbol.width, self.last_width, delta_w
        );
        self.coder.encode_integer(IntProc::Iadw, delta_w)?;
        self.last_width = symbol.width as i32;

        // II. Encode Symbol Bitmap using Generic Region Procedure
        let packed = symbol.packed_words();

        // Verify bit-order correctness: first black pixel should match between symbol and packed data
        if let Some(expected_first_pixel) = first_black_pixel(symbol) {
            let actual_first_pixel =
                crate::jbig2sym::first_black_pixel_in_packed(packed, symbol.width, symbol.height);
            assert_eq!(
                actual_first_pixel,
                Some(expected_first_pixel),
                "bit-order / row-order mismatch in symbol dict packer! Expected first black pixel at {:?}, got {:?}",
                expected_first_pixel,
                actual_first_pixel
            );
        }

        self.coder.encode_generic_region(
            packed,
            symbol.width,
            symbol.height,
            0, // Use standard template 0
            &[(3, -1), (-3, -1), (2, -2), (-2, -2)],
        )?;
        self.pushed += 1;
        Ok(())
    }

    /// Close the last height class, code the export flags, terminate the
    /// arithmetic stream and prepend the segment parameters.
    pub(crate) fn finish(
        mut self,
        config: &Jbig2Config,
        num_imported_symbols: u32,
    ) -> Result<Vec<u8>> {
        if self.class_open {
            self.coder.encode_oob(IntProc::Iadw)?;
        }
        let num_new_syms = self.pushed;
        // Per T.88 §7.4.2.1.6, SDNUMEXSYMS is the total count of exported symbols which
        // includes any imported symbols carried forward from referenced dictionaries.
        let num_export_syms = num_imported_symbols.saturating_add(num_new_syms);

        // Create symbol dictionary parameters
        let params = SymbolDictParams {
            sd_template: 0, // Use standard template 0
            // Match jbig2enc's template-0 adaptive pixels for symbol dictionaries.
            at: [(3, -1), (-3, -1), (2, -2), (-2, -2)],
            refine_aggregate: false,
            refine_template: 0,
            refine_at: [(0, 0), (0, 0)],
            exsyms: num_export_syms,
            newsyms: num_new_syms,
        };

        if cfg!(debug_assertions) {
            debug!("encode_symbol_dict: Exporting {} symbols", num_export_syms);
            trace!("encode_symbol_dict: SymbolDictParams details: {:?}", params);
        }

        // Export flags come after the symbol bitmap data (run-length form).
        // T.88 §7.4.2.2: the run-length must cover SDNUMINSYMS + SDNUMNEWSYMS slots.
        // We export every symbol (both imported and new), so the sequence is a single
        // "all exported" run covering num_export_syms = num_imported + num_new.
        self.coder.encode_integer(IntProc::Iaex, 0)?;
        let num_export_syms =
            i32::try_from(num_export_syms).map_err(|_| anyhow!("too many exported symbols"))?;
        self.coder.encode_integer(IntProc::Iaex, num_export_syms)?;

        // flush the coder ONCE
        self.coder
            .flush_with_termination(true, config.canonical_arithmetic_termination);

        // The segment parameters precede the single, complete arithmetic payload.
        let mut payload = params.to_bytes();
        payload.extend(self.coder.as_bytes());
        Ok(payload)
    }
}

/// Encodes a symbol dictionary, returning both the payload and the mapping from
/// encoded dictionary position → input index.
pub fn encode_symbol_dict_with_order(
    symbols: &[&BitImage],
    config: &Jbig2Config,
    num_imported_symbols: u32,
) -> Result<(Vec<u8>, Vec<usize>)> {
    // Compute canonical order (filter + dedup + sort)
    let canonical_order = canonicalize_dict_symbols(symbols);
    if canonical_order.is_empty() {
        return Err(anyhow!(
            "encode_symbol_dict: no valid symbols supplied (all symbols had zero width or height)"
        ));
    }
    let mut stream = DictionaryStream::new();
    for &index in &canonical_order {
        stream.push(symbols[index])?;
    }
    let payload = stream.finish(config, num_imported_symbols)?;
    Ok((payload, canonical_order))
}

/// Prefix checkpoints of recent dictionary streams, so an encode whose
/// canonical symbol sequence shares a prefix with an earlier one resumes from
/// the last checkpoint inside that prefix instead of starting over. The
/// measured planner's trials differ from one another by one symbol or one
/// family, so most of every dictionary is a prefix of a dictionary it has
/// already coded. Resuming replays exactly the operations the one-shot encoder
/// would perform from that point, so the bytes are unchanged; only the work is.
#[derive(Default)]
pub(crate) struct DictionaryPrefixCache {
    sets: Vec<PrefixSet>,
}

struct PrefixSet {
    /// Symbol identities in canonical coding order.
    identities: Vec<usize>,
    /// Stream state after `position` symbols, shared with the sets it was
    /// resumed from.
    checkpoints: Vec<(usize, std::sync::Arc<DictionaryStream>)>,
}

/// Recent sequences retained; the planner alternates between at most three
/// dictionary policies.
const PREFIX_SETS: usize = 3;
/// Checkpoints per sequence are bounded, so the retained coder states stay
/// within a few megabytes however many symbols a page has.
const MAX_CHECKPOINTS: usize = 16;
const MIN_CHECKPOINT_INTERVAL: usize = 8;

impl DictionaryPrefixCache {
    fn interval(symbols: usize) -> usize {
        symbols
            .div_ceil(MAX_CHECKPOINTS)
            .max(MIN_CHECKPOINT_INTERVAL)
    }

    /// The best stored state for `identities`: the stream after the furthest
    /// checkpoint inside the longest common prefix, with that prefix's
    /// checkpoints to carry forward.
    fn resume(
        &self,
        identities: &[usize],
    ) -> (
        DictionaryStream,
        Vec<(usize, std::sync::Arc<DictionaryStream>)>,
    ) {
        let mut best: Option<(usize, &PrefixSet)> = None;
        for set in &self.sets {
            let common = set
                .identities
                .iter()
                .zip(identities)
                .take_while(|(a, b)| a == b)
                .count();
            if common > best.map_or(0, |(length, _)| length) {
                best = Some((common, set));
            }
        }
        let Some((common, set)) = best else {
            return (DictionaryStream::new(), Vec::new());
        };
        let carried: Vec<_> = set
            .checkpoints
            .iter()
            .take_while(|(position, _)| *position <= common)
            .cloned()
            .collect();
        match carried.last() {
            Some((_, state)) => (DictionaryStream::clone(state), carried),
            None => (DictionaryStream::new(), carried),
        }
    }

    fn store(
        &mut self,
        identities: Vec<usize>,
        checkpoints: Vec<(usize, std::sync::Arc<DictionaryStream>)>,
    ) {
        if let Some(existing) = self
            .sets
            .iter()
            .position(|set| set.identities == identities)
        {
            self.sets.remove(existing);
        } else if self.sets.len() >= PREFIX_SETS {
            self.sets.remove(0);
        }
        self.sets.push(PrefixSet {
            identities,
            checkpoints,
        });
    }
}

/// `encode_symbol_dict_with_order`, resuming from and extending `cache`.
/// `identities[i]` names `symbols[i]` across calls; two symbols with the same
/// identity must be the same bitmap.
pub(crate) fn encode_symbol_dict_with_order_resumed(
    symbols: &[&BitImage],
    identities: &[usize],
    config: &Jbig2Config,
    num_imported_symbols: u32,
    cache: &mut DictionaryPrefixCache,
) -> Result<(Vec<u8>, Vec<usize>)> {
    debug_assert_eq!(symbols.len(), identities.len());
    let canonical_order = canonicalize_dict_symbols(symbols);
    if canonical_order.is_empty() {
        return Err(anyhow!(
            "encode_symbol_dict: no valid symbols supplied (all symbols had zero width or height)"
        ));
    }
    let ordered: Vec<usize> = canonical_order.iter().map(|&i| identities[i]).collect();
    let (mut stream, mut checkpoints) = cache.resume(&ordered);
    let interval = DictionaryPrefixCache::interval(ordered.len());
    for &index in &canonical_order[stream.len()..] {
        stream.push(symbols[index])?;
        let position = stream.len();
        if position.is_multiple_of(interval) && position < ordered.len() {
            checkpoints.push((position, std::sync::Arc::new(stream.clone())));
        }
    }
    let payload = stream.finish(config, num_imported_symbols)?;
    cache.store(ordered, checkpoints);
    Ok((payload, canonical_order))
}
/// Represents a single symbol instance in a text region, with refinement info.
#[derive(Debug, Clone)]
pub struct TextRegionSymbolInstance {
    /// The ID of the symbol in the dictionary.
    pub symbol_id: u32,
    /// The x-coordinate of the instance's top-left corner.
    pub x: i32,
    /// The y-coordinate of the instance's top-left corner.
    pub y: i32,
    /// The horizontal refinement offset.
    pub dx: i32,
    /// The vertical refinement offset.
    pub dy: i32,
    /// Whether this instance is a refinement of a dictionary symbol.
    pub is_refinement: bool,
}

impl TextRegionSymbolInstance {
    /// Returns the position of this symbol instance as a Rect.
    pub fn position(&self) -> crate::jbig2sym::Rect {
        crate::jbig2sym::Rect {
            x: self.x as u32,
            y: self.y as u32,
            width: 0,  // These will be set by the caller
            height: 0, // These will be set by the caller
        }
    }

    /// Returns the symbol index for this instance.
    pub fn symbol_index(&self) -> usize {
        self.symbol_id as usize
    }

    /// Converts to a SymbolInstance
    pub fn to_symbol_instance(&self, symbol_bitmap: &BitImage) -> SymbolInstance {
        SymbolInstance {
            symbol_index: self.symbol_id as usize,
            position: self.position(),
            instance_bitmap: symbol_bitmap.clone(),
            needs_refinement: self.is_refinement,
            refinement_dx: self.dx,
            refinement_dy: self.dy,
        }
    }
}

pub fn build_dictionary_and_get_instances(
    symbols: &[(Rect, BitImage)],
    comparator: &mut Comparator,
) -> (Vec<BitImage>, Vec<TextRegionSymbolInstance>) {
    let mut dictionary_symbols: Vec<BitImage> = Vec::with_capacity(symbols.len());
    let mut dictionary_black_pixels = Vec::with_capacity(symbols.len());
    let mut instances = Vec::with_capacity(symbols.len());

    for (rect, symbol_image) in symbols.iter() {
        let mut found_match = false;
        // Use a 10% error threshold for matching, as recommended.
        let max_err = ((symbol_image.width * symbol_image.height) / 10).max(3) as u32;
        let symbol_black_pixels = symbol_image.count_ones();

        for (dict_idx, dict_symbol) in dictionary_symbols.iter().enumerate() {
            if symbol_image.width.abs_diff(dict_symbol.width) > MAX_DIMENSION_DELTA
                || symbol_image.height.abs_diff(dict_symbol.height) > MAX_DIMENSION_DELTA
            {
                continue;
            }

            if symbol_black_pixels.abs_diff(dictionary_black_pixels[dict_idx]) > max_err as usize {
                continue;
            }

            // Use a low max_err for finding near-duplicates
            if let Some((err, dx, dy)) = comparator.distance(symbol_image, dict_symbol, max_err) {
                instances.push(TextRegionSymbolInstance {
                    symbol_id: dict_idx as u32,
                    x: rect.x as i32,
                    y: rect.y as i32,
                    dx,
                    dy,
                    is_refinement: err > 0,
                });
                found_match = true;
                break;
            }
        }

        if !found_match {
            let new_idx = dictionary_symbols.len();
            dictionary_symbols.push(symbol_image.clone());
            dictionary_black_pixels.push(symbol_black_pixels);
            instances.push(TextRegionSymbolInstance {
                symbol_id: new_idx as u32,
                x: rect.x as i32,
                y: rect.y as i32,
                dx: 0,
                dy: 0,
                is_refinement: false,
            });
        }
    }

    (dictionary_symbols, instances)
}

#[cfg(test)]
mod prefix_cache_tests {
    use super::{
        DictionaryPrefixCache, encode_symbol_dict_with_order, encode_symbol_dict_with_order_resumed,
    };
    use crate::jbig2structs::Jbig2Config;
    use crate::jbig2sym::BitImage;

    fn glyph(seed: &mut u64, width: u32, height: u32) -> BitImage {
        let mut image = BitImage::new(width, height).unwrap();
        for y in 0..height as usize {
            for x in 0..width as usize {
                *seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                if (*seed >> 33).is_multiple_of(3) {
                    image.set_usize(x, y, true);
                }
            }
        }
        image
    }

    #[test]
    fn resumed_dictionaries_match_one_shot_encodes() {
        let mut seed = 0x11u64;
        let pool: Vec<BitImage> = (0..90)
            .map(|i| glyph(&mut seed, 5 + (i * 7) % 29, 6 + (i * 5) % 23))
            .collect();
        let config = Jbig2Config::text_lossless_max_compression();
        let mut cache = DictionaryPrefixCache::default();
        // The full set, then sets that drop one symbol or a family, reinsert,
        // and a disjoint set: every sequence shares some prefix with a stored
        // one or none at all.
        let mut sets: Vec<Vec<usize>> = vec![(0..90).collect()];
        for drop in [0usize, 1, 7, 8, 15, 16, 17, 40, 63, 64, 88, 89] {
            sets.push((0..90).filter(|&i| i != drop).collect());
        }
        sets.push((0..90).filter(|&i| i % 10 != 3).collect());
        sets.push((0..90).collect());
        sets.push((30..60).collect());
        sets.push((0..20).rev().collect());
        for (round, set) in sets.iter().enumerate() {
            let symbols: Vec<&BitImage> = set.iter().map(|&i| &pool[i]).collect();
            let expected = encode_symbol_dict_with_order(&symbols, &config, 0).unwrap();
            let actual =
                encode_symbol_dict_with_order_resumed(&symbols, set, &config, 0, &mut cache)
                    .unwrap();
            assert_eq!(actual.1, expected.1, "order, round {round}");
            assert_eq!(actual.0, expected.0, "payload, round {round}");
        }
        assert!(cache.sets.len() <= super::PREFIX_SETS);
    }
}
