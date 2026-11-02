//! Bounded counterfactual stream encodes for the explicit maximum-compression
//! exact preset. Discovery is heuristic; acceptance uses serialized bytes of
//! complete candidate streams, never a differing-pixel count as the final cost.
use super::super::{Jbig2Encoder, SymbolCostTrial, SymbolOptimizationMetrics};
use super::dictionary::plan_symbol_dictionary_layout;
use super::residual::MAX_RESIDUAL_REGIONS;
use super::types::{PlannedDocument, PlannedPageLayout, RefinementPlan};
use crate::jbig2classify::{
    FamilyBucketKey, compute_symbol_signature, family_bucket_key_for_symbol,
    family_bucket_neighbors,
};
use crate::jbig2comparator::Comparator;
use crate::jbig2structs::{FileHeader, PdfDictionaryPolicy, Segment, SegmentType};
use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Dictionary,
    Refinement { reference: usize, dx: i32, dy: i32 },
    Generic,
    Substitution { reference: usize },
}

struct Trial {
    document: PlannedDocument,
    bytes: usize,
    feasible: bool,
    global_symbols: usize,
    local_symbols: usize,
    exact_instances: usize,
    refined_instances: usize,
    generic_instances: usize,
    substituted_instances: usize,
}

/// Online correction of shortlist estimates using *earlier* complete-stream
/// observations. Keep family/single and refinement/substitution policies apart:
/// fixed signaling and allocation overheads differ. This model only schedules
/// trials; measured bytes and resource feasibility remain authoritative.
#[derive(Default)]
struct CostCalibration {
    errors: std::collections::BTreeMap<(&'static str, u8), Vec<i64>>,
    samples: usize,
}

impl CostCalibration {
    fn key(kind: &'static str, policy: PdfDictionaryPolicy) -> (&'static str, u8) {
        (
            kind,
            match policy {
                PdfDictionaryPolicy::AllGlobal => 0,
                PdfDictionaryPolicy::SharedGlobal => 1,
                PdfDictionaryPolicy::SharedGlobalWithLocal => 2,
            },
        )
    }

    fn predict(&self, kind: &'static str, policy: PdfDictionaryPolicy, raw: i64) -> i64 {
        let Some(errors) = self.errors.get(&Self::key(kind, policy)) else {
            return raw;
        };
        raw.saturating_sub(errors[errors.len() / 2])
    }

    fn observe(
        &mut self,
        kind: &'static str,
        policy: PdfDictionaryPolicy,
        raw: i64,
        measured: i64,
    ) {
        let errors = self.errors.entry(Self::key(kind, policy)).or_default();
        let error = raw.saturating_sub(measured);
        let position = errors
            .binary_search(&error)
            .unwrap_or_else(|position| position);
        errors.insert(position, error);
        self.samples += 1;
    }
}

fn family_kind(substitution: bool) -> &'static str {
    if substitution {
        "family + direct substitution"
    } else {
        "family + refinement"
    }
}

fn choice_kind(choice: Choice) -> &'static str {
    match choice {
        Choice::Generic => "generic residual",
        Choice::Substitution { .. } => "direct substitution",
        _ => "reference + refinement",
    }
}

fn referenced_symbols(choices: &[Choice]) -> FxHashSet<usize> {
    choices
        .iter()
        .filter_map(|choice| match *choice {
            Choice::Refinement { reference, .. } | Choice::Substitution { reference } => {
                Some(reference)
            }
            _ => None,
        })
        .collect()
}

#[derive(Default)]
struct ByteCount(usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("encoded size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Trial {
    fn preferable_to(&self, best: &Self) -> bool {
        (self.feasible && !best.feasible)
            || (self.feasible == best.feasible && self.bytes < best.bytes)
    }
}

impl Choice {
    fn reference(self, symbol: usize) -> Option<usize> {
        match self {
            Self::Dictionary => Some(symbol),
            Self::Refinement { reference, .. } | Self::Substitution { reference } => {
                Some(reference)
            }
            Self::Generic => None,
        }
    }
}

impl<'a> Jbig2Encoder<'a> {
    fn encode_exact_trial(
        &mut self,
        choices: &[Choice],
        policy: PdfDictionaryPolicy,
        include_header: bool,
    ) -> Result<Trial> {
        let mut spans = vec![0; self.global_symbols.len()];
        let page_references: Vec<BTreeSet<usize>> = self
            .pages
            .iter()
            .map(|page| {
                let references: BTreeSet<_> = page
                    .symbol_instances
                    .iter()
                    .filter_map(|instance| {
                        choices[instance.symbol_index].reference(instance.symbol_index)
                    })
                    .collect();
                for &symbol in &references {
                    spans[symbol] += 1;
                }
                references
            })
            .collect();
        let globals: Vec<_> = spans
            .iter()
            .enumerate()
            .filter(|&(_, &span)| {
                span > 0 && (policy == PdfDictionaryPolicy::AllGlobal || span > 1)
            })
            .map(|(index, _)| index)
            .collect();
        let mut global_positions = vec![u32::MAX; self.global_symbols.len()];
        let mut global_segments = Vec::new();
        let mut next = self.next_segment_number;
        self.global_dict_segment_numbers.clear();
        if !globals.is_empty() {
            let symbols: Vec<_> = globals
                .iter()
                .map(|&index| &self.global_symbols[index])
                .collect();
            let layout = plan_symbol_dictionary_layout(&symbols, self.config, None)?;
            let encoded = self.symbol_dictionary_for(&globals, &layout)?;
            for (input, &position) in encoded.input_to_exported_pos.iter().enumerate() {
                global_positions[globals[input]] = position;
            }
            self.global_dict_segment_numbers.push(next);
            global_segments.push(Segment {
                number: next,
                seg_type: SegmentType::SymbolDictionary,
                deferred_non_retain: false,
                retain_flags: 0,
                page_association_type: 2,
                referred_to: vec![],
                page: None,
                payload: encoded.payload,
            });
            next += 1;
        }
        let global_count = globals.len() as u32;
        let refinements: Vec<_> = choices
            .iter()
            .map(|choice| match *choice {
                Choice::Refinement { reference, dx, dy } => Some(RefinementPlan {
                    prototype_input_index: reference,
                    refinement_dx: dx,
                    refinement_dy: dy,
                }),
                _ => None,
            })
            .collect();
        let mut pages = Vec::with_capacity(self.pages.len());
        let page_start = next;
        let mut local_count = 0;
        let (
            mut exact_instances,
            mut refined_instances,
            mut generic_instances,
            mut substituted_instances,
        ) = (0, 0, 0, 0);
        for (page_index, references) in page_references.iter().enumerate() {
            if self.state.pdf_mode {
                next = page_start;
            }
            let page_number = if self.state.pdf_mode {
                1
            } else {
                page_index as u32 + 1
            };
            let locals: Vec<_> = if policy == PdfDictionaryPolicy::SharedGlobalWithLocal {
                references
                    .iter()
                    .copied()
                    .filter(|&index| global_positions[index] == u32::MAX)
                    .collect()
            } else {
                Vec::new()
            };
            let only_locals = references
                .iter()
                .all(|&index| global_positions[index] == u32::MAX);
            let use_generic_region = references.is_empty()
                || (policy == PdfDictionaryPolicy::SharedGlobal && only_locals)
                || (only_locals
                    && locals.len() <= 2
                    && self.pages[page_index].symbol_instances.len() <= 2
                    && locals
                        .iter()
                        .map(|&index| {
                            self.estimate_local_symbol_gain(&self.pages[page_index], index)
                        })
                        .sum::<i64>()
                        <= 0);
            let mut residual = BTreeSet::new();
            let mut substitution_remaps = FxHashMap::default();
            for instance in &self.pages[page_index].symbol_instances {
                let reference = choices[instance.symbol_index].reference(instance.symbol_index);
                let generic = use_generic_region
                    || reference.is_none()
                    || (policy == PdfDictionaryPolicy::SharedGlobal
                        && reference.is_some_and(|index| global_positions[index] == u32::MAX));
                if generic {
                    residual.insert(instance.symbol_index);
                    generic_instances += 1;
                } else if let Choice::Substitution { reference } = choices[instance.symbol_index] {
                    substitution_remaps.insert(instance.symbol_index, reference);
                    substituted_instances += 1;
                } else if matches!(choices[instance.symbol_index], Choice::Refinement { .. }) {
                    refined_instances += 1;
                } else {
                    exact_instances += 1;
                }
            }
            let page_info_segment_number = next;
            next += 1;
            let local_dict_layout = if locals.is_empty() {
                None
            } else {
                let symbols: Vec<_> = locals
                    .iter()
                    .map(|&index| &self.global_symbols[index])
                    .collect();
                Some(plan_symbol_dictionary_layout(&symbols, self.config, None)?)
            };
            let local_dict_segment_numbers = if locals.is_empty() {
                Vec::new()
            } else {
                let number = next;
                next += 1;
                vec![number]
            };
            if !use_generic_region {
                local_count += locals.len();
            }
            let region_segment_number = next;
            next += 1;
            let residual_region_segment_number = if residual.is_empty() || use_generic_region {
                None
            } else {
                let number = next;
                next += if self.config.pack_residuals {
                    MAX_RESIDUAL_REGIONS as u32
                } else {
                    1
                };
                Some(number)
            };
            let end_of_page_segment_number = next;
            next += 1;
            let layout = PlannedPageLayout {
                page_index,
                page_number,
                page_info_segment_number,
                local_dict_segment_numbers,
                local_dict_layout,
                region_segment_number,
                residual_region_segment_number,
                end_of_page_segment_number,
                local_symbols: locals,
                residual_symbols: residual.into_iter().collect(),
                residual_anchor_remaps: substitution_remaps,
                use_generic_region,
            };
            pages.push(
                self.build_planned_page(&layout, &global_positions, global_count, &refinements)?
                    .page,
            );
        }
        let document = PlannedDocument {
            file_header: include_header.then_some(FileHeader {
                organisation_type: false,
                unknown_n_pages: false,
                n_pages: pages.len() as u32,
            }),
            global_segments,
            pages,
            eof_segment: Some(Segment {
                number: next,
                seg_type: SegmentType::EndOfFile,
                deferred_non_retain: false,
                retain_flags: 0,
                page_association_type: 2,
                referred_to: vec![],
                page: None,
                payload: vec![],
            }),
            next_segment_number: next + 1,
            global_bitmap_bytes: super::super::resources::dictionary_storage(
                &self.global_symbols,
                &global_positions,
            )
            .0,
            global_dictionary_pixels: super::super::resources::dictionary_storage(
                &self.global_symbols,
                &global_positions,
            )
            .1,
        };
        let mut count = ByteCount::default();
        if let Some(header) = &document.file_header {
            count.write_all(&header.to_bytes())?;
        }
        for segment in document
            .global_segments
            .iter()
            .chain(document.pages.iter().flat_map(|page| &page.segments))
        {
            segment.write_into(&mut count)?;
        }
        if !self.state.pdf_mode {
            if let Some(eof) = &document.eof_segment {
                eof.write_into(&mut count)?;
            }
        }
        let resources = super::super::DocumentResourceMetrics::from_plan(&document)?;
        let feasible = self
            .config
            .decoder_resource_limits
            .check(&resources)
            .is_ok();
        Ok(Trial {
            document,
            feasible,
            bytes: count.0,
            global_symbols: globals.len(),
            local_symbols: local_count,
            exact_instances,
            refined_instances,
            generic_instances,
            substituted_instances,
        })
    }

    pub(crate) fn plan_optimized_exact_document(
        &mut self,
        include_header: bool,
    ) -> Result<PlannedDocument> {
        self.begin_trial_memo();
        let planned = self.plan_measured_trials(include_header);
        self.end_trial_memo();
        planned
    }

    fn plan_measured_trials(&mut self, include_header: bool) -> Result<PlannedDocument> {
        self.config
            .validate_exact_symbol_mode()
            .map_err(anyhow::Error::msg)?;
        let started = Instant::now();
        let policies = [
            PdfDictionaryPolicy::AllGlobal,
            PdfDictionaryPolicy::SharedGlobal,
            PdfDictionaryPolicy::SharedGlobalWithLocal,
        ];
        let mut report = SymbolOptimizationMetrics::default();
        let mut calibration = CostCalibration::default();
        let mut choices = vec![Choice::Dictionary; self.global_symbols.len()];
        let mut best: Option<Trial> = None;
        for policy in policies {
            let trial = self.encode_exact_trial(&choices, policy, include_header)?;
            report.trials.push(SymbolCostTrial {
                symbol: None,
                representation: "dictionary control",
                policy,
                estimated_gain_bytes: None,
                raw_estimated_gain_bytes: None,
                measured_gain_bytes: None,
                encoded_bytes: trial.bytes,
                meets_resource_limits: trial.feasible,
            });
            if best.as_ref().is_none_or(|best| trial.preferable_to(best)) {
                best = Some(trial);
            }
        }
        let mut best = best.expect("three dictionary control trials");
        report.baseline_bytes = report
            .trials
            .iter()
            .map(|trial| trial.encoded_bytes)
            .min()
            .unwrap();

        // Bucket indices in usage order so candidate budgets are spent on the
        // most reusable anchors first. Large components remain eligible for
        // generic coding but do not trigger expensive fuzzy comparisons.
        let mut by_usage: Vec<_> = (0..self.global_symbols.len()).collect();
        by_usage.sort_by_key(|&index| (std::cmp::Reverse(self.symbol_usage[index]), index));
        let mut buckets: FxHashMap<FamilyBucketKey, Vec<usize>> = FxHashMap::default();
        let mut keys = vec![None; self.global_symbols.len()];
        for &index in &by_usage {
            let symbol = &self.global_symbols[index];
            if symbol.width <= 256 && symbol.height <= 256 && self.symbol_pixel_counts[index] > 1 {
                let signature = compute_symbol_signature(symbol);
                let key = family_bucket_key_for_symbol(symbol, &signature);
                keys[index] = Some(key);
                buckets.entry(key).or_default().push(index);
            }
        }
        let mut targets: Vec<_> = (0..self.global_symbols.len()).collect();
        targets.sort_by_key(|&index| {
            (
                self.symbol_usage[index] > 2,
                std::cmp::Reverse(
                    self.global_symbols[index].width * self.global_symbols[index].height,
                ),
                index,
            )
        });
        let mut comparator = Comparator::default();
        let limit = self.config.symbol_trial_limit;
        let mut near_candidates = vec![Vec::new(); choices.len()];
        for &target in &targets {
            if limit == 4 {
                break;
            }
            let mut candidates = Vec::new();
            let mut visited = FxHashSet::default();
            if let Some(key) = keys[target] {
                'neighbors: for neighbor in family_bucket_neighbors(key) {
                    let Some(bucket) = buckets.get(&neighbor) else {
                        continue;
                    };
                    for &reference in bucket {
                        if reference == target || !visited.insert(reference) {
                            continue;
                        }
                        if visited.len() > self.config.symbol_candidate_limit {
                            break 'neighbors;
                        }
                        if choices[reference] != Choice::Dictionary
                            || (self.symbol_usage[reference], std::cmp::Reverse(reference))
                                < (self.symbol_usage[target], std::cmp::Reverse(target))
                        {
                            continue;
                        }
                        let symbol = &self.global_symbols[target];
                        let max_error = (symbol.width * symbol.height
                            / self.config.match_tolerance.max(1) as usize)
                            .max(2) as u32;
                        report.candidate_comparisons += 1;
                        if let Some(result) = comparator.compare_for_refine_family(
                            symbol,
                            &self.global_symbols[reference],
                            max_error,
                            2,
                            1,
                        ) {
                            candidates.push((
                                result.total_err
                                    + (result.dx.unsigned_abs() + result.dy.unsigned_abs()) * 3,
                                Choice::Refinement {
                                    reference,
                                    dx: result.dx,
                                    dy: result.dy,
                                },
                            ));
                        }
                    }
                }
            }
            candidates.sort_by_key(|candidate| candidate.0);
            candidates.truncate(2);
            if self.config.optimize_lossy_symbols {
                let substitutions = candidates
                    .iter()
                    .filter_map(|&(score, choice)| {
                        let Choice::Refinement {
                            reference,
                            dx: 0,
                            dy: 0,
                        } = choice
                        else {
                            return None;
                        };
                        let source = &self.global_symbols[target];
                        let prototype = &self.global_symbols[reference];
                        if (source.width, source.height) != (prototype.width, prototype.height) {
                            return None;
                        }
                        let errors: u32 = source
                            .packed_words()
                            .iter()
                            .zip(prototype.packed_words())
                            .map(|(left, right)| (left ^ right).count_ones())
                            .sum();
                        (errors <= self.config.symbol_substitution_error_limit
                            && super::topology::substitution_preserves_topology(source, prototype))
                        .then_some((score, Choice::Substitution { reference }))
                    })
                    .collect::<Vec<_>>();
                candidates.extend(substitutions);
            }
            near_candidates[target] = candidates;
        }

        // Refinement has a fixed text-region signaling cost. A family can win
        // jointly even when moving any one member alone loses. Measure these
        // coordinated proposals before greedy per-symbol changes.
        let mut families: std::collections::BTreeMap<(usize, bool), Vec<(usize, u32, Choice)>> =
            Default::default();
        for (target, candidates) in near_candidates.iter().enumerate() {
            for &(score, choice) in candidates {
                if let Some(reference) = choice.reference(target) {
                    families
                        .entry((reference, matches!(choice, Choice::Substitution { .. })))
                        .or_default()
                        .push((target, score, choice));
                }
            }
        }
        let mut families: Vec<_> = families
            .into_iter()
            .filter(|(_, members)| members.len() >= 2)
            .collect();
        let family_budget = report.trials.len() + ((limit - 4) / 2).min(18);
        while !families.is_empty() && report.trials.len() < family_budget {
            let referenced = referenced_symbols(&choices);
            families.retain(|((reference, _), members)| {
                choices[*reference] == Choice::Dictionary
                    && members
                        .iter()
                        .filter(|&&(target, _, choice)| {
                            choices[target] != choice && !referenced.contains(&target)
                        })
                        .take(2)
                        .count()
                        == 2
            });
            if families.is_empty() {
                break;
            }
            let selected = families
                .iter()
                .enumerate()
                .max_by_key(|(_, ((reference, substitution), members))| {
                    let raw = members
                        .iter()
                        .map(|&(target, score, _)| {
                            self.global_symbols[target]
                                .width
                                .saturating_mul(self.global_symbols[target].height)
                                .div_ceil(8) as i64
                                - score as i64
                        })
                        .sum::<i64>();
                    let predicted = policies
                        .iter()
                        .map(|&policy| calibration.predict(family_kind(*substitution), policy, raw))
                        .max()
                        .unwrap();
                    (
                        predicted,
                        std::cmp::Reverse(*reference),
                        std::cmp::Reverse(*substitution),
                    )
                })
                .unwrap()
                .0;
            let ((reference, substitution), members) = families.swap_remove(selected);
            if choices[reference] != Choice::Dictionary {
                continue;
            }
            let mut proposal = choices.clone();
            let mut estimate = 0;
            let mut changed = 0;
            for (target, score, choice) in members {
                if choices[target] == choice || referenced.contains(&target) {
                    continue;
                }
                proposal[target] = choice;
                estimate += self.global_symbols[target]
                    .width
                    .saturating_mul(self.global_symbols[target].height)
                    .div_ceil(8) as i64
                    - score as i64;
                changed += 1;
            }
            if changed < 2 {
                continue;
            }
            for policy in policies {
                if report.trials.len() >= family_budget {
                    break;
                }
                let kind = family_kind(substitution);
                let prediction = calibration.predict(kind, policy, estimate);
                let trial = self.encode_exact_trial(&proposal, policy, include_header)?;
                let measured = best.bytes as i64 - trial.bytes as i64;
                calibration.observe(kind, policy, estimate, measured);
                report.trials.push(SymbolCostTrial {
                    symbol: Some(reference),
                    representation: kind,
                    policy,
                    estimated_gain_bytes: Some(prediction),
                    raw_estimated_gain_bytes: Some(estimate),
                    measured_gain_bytes: Some(measured),
                    encoded_bytes: trial.bytes,
                    meets_resource_limits: trial.feasible,
                });
                if trial.preferable_to(&best) {
                    best = trial;
                    choices = proposal.clone();
                }
            }
        }

        for target in targets {
            if report.trials.len() >= limit - 1 {
                break;
            }
            // A chosen reference remains stored; changing it underneath already
            // accepted refinements would invalidate their dictionary mapping.
            if referenced_symbols(&choices).contains(&target) {
                continue;
            }
            // A deliberately rough shortlist estimate. Every accepted change is
            // measured with dictionaries, placements, residuals and headers in
            // the contexts in which they will actually be emitted.
            let bitmap_bytes = self.global_symbols[target]
                .width
                .saturating_mul(self.global_symbols[target].height)
                .div_ceil(8) as i64;
            let proposals = near_candidates[target].iter().copied().filter(|&(_, choice)| {
                matches!(choice, Choice::Refinement { reference, .. } | Choice::Substitution { reference } if choices[reference] == Choice::Dictionary)
            }).map(|(score, choice)| (choice, Some(bitmap_bytes - score as i64))).collect::<Vec<_>>();
            let mut proposals: Vec<_> = proposals
                .into_iter()
                .chain(std::iter::once((Choice::Generic, None)))
                .collect();
            proposals.sort_by_key(|&(choice, raw)| {
                std::cmp::Reverse(
                    raw.map(|raw| {
                        policies
                            .iter()
                            .map(|&policy| calibration.predict(choice_kind(choice), policy, raw))
                            .max()
                            .unwrap()
                    })
                    .unwrap_or(0),
                )
            });
            for (proposal, estimate) in proposals {
                if choices[target] == proposal {
                    continue;
                }
                for policy in policies {
                    if report.trials.len() >= limit - 1 {
                        break;
                    }
                    let previous = choices[target];
                    choices[target] = proposal;
                    let trial = self.encode_exact_trial(&choices, policy, include_header)?;
                    let representation = choice_kind(proposal);
                    let prediction =
                        estimate.map(|raw| calibration.predict(representation, policy, raw));
                    let measured = best.bytes as i64 - trial.bytes as i64;
                    if let Some(raw) = estimate {
                        calibration.observe(representation, policy, raw, measured);
                    }
                    report.trials.push(SymbolCostTrial {
                        symbol: Some(target),
                        representation,
                        policy,
                        estimated_gain_bytes: prediction,
                        raw_estimated_gain_bytes: estimate,
                        measured_gain_bytes: Some(measured),
                        encoded_bytes: trial.bytes,
                        meets_resource_limits: trial.feasible,
                    });
                    if trial.preferable_to(&best) {
                        best = trial;
                    } else {
                        choices[target] = previous;
                    }
                }
            }
        }
        let generic = self.encode_exact_trial(
            &vec![Choice::Generic; choices.len()],
            PdfDictionaryPolicy::AllGlobal,
            include_header,
        )?;
        report.generic_bytes = generic.bytes;
        report.trials.push(SymbolCostTrial {
            symbol: None,
            representation: "whole-page generic control",
            policy: PdfDictionaryPolicy::AllGlobal,
            estimated_gain_bytes: None,
            raw_estimated_gain_bytes: None,
            measured_gain_bytes: Some(best.bytes as i64 - generic.bytes as i64),
            encoded_bytes: generic.bytes,
            meets_resource_limits: generic.feasible,
        });
        if generic.preferable_to(&best) {
            best = generic;
        }
        report.selected_bytes = best.bytes;
        report.exact_instances = best.exact_instances;
        report.refined_instances = best.refined_instances;
        report.generic_instances = best.generic_instances;
        report.substituted_instances = best.substituted_instances;
        report.calibration_samples = calibration.samples;
        self.global_dict_segment_numbers = best
            .document
            .global_segments
            .iter()
            .map(|segment| segment.number)
            .collect();
        self.metrics.symbol_stats.global_symbol_count = best.global_symbols;
        self.metrics.symbol_stats.local_symbol_count = best.local_symbols;
        self.metrics.symbol_stats.symbols_exported = best.global_symbols + best.local_symbols;
        self.metrics.symbol_mode.planning += started.elapsed();
        self.metrics.optimization = report;
        Ok(best.document)
    }
}
