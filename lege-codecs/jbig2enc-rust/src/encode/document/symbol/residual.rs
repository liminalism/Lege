//! Measured generic residual layouts. Component membership is unchanged; every
//! candidate is an OR of the same source bitmaps at the same page coordinates.
use super::super::Jbig2Encoder;
use super::types::SymbolInstance;
use crate::jbig2structs::{Segment, SegmentType};
use anyhow::Result;
use std::collections::BTreeMap;

// Bounds candidate encodes and the segment-number reservation in planning.
pub(crate) const MAX_RESIDUAL_REGIONS: usize = 64;

#[cfg(all(test, feature = "decode"))]
#[path = "../../../../tests/common/pbm.rs"]
mod test_pbm;

fn encoded_bytes(segments: &[Segment]) -> Result<usize> {
    let mut bytes = Vec::new();
    for segment in segments {
        segment.write_into(&mut bytes)?;
    }
    Ok(bytes.len())
}

impl<'a> Jbig2Encoder<'a> {
    fn encode_residual_groups(
        &self,
        groups: &[Vec<&SymbolInstance>],
        first_number: u32,
        page_number: u32,
    ) -> Result<Vec<Segment>> {
        let mut segments = Vec::with_capacity(groups.len());
        for group in groups {
            if let Some((bitmap, x, y)) = self.build_instance_residual_bitmap(group)? {
                segments.push(Segment {
                    number: first_number + segments.len() as u32,
                    seg_type: if self.config.is_lossless && self.config.symbol_mode {
                        SegmentType::ImmediateLosslessGenericRegion
                    } else {
                        SegmentType::ImmediateGenericRegion
                    },
                    deferred_non_retain: false,
                    retain_flags: 0,
                    page_association_type: 0,
                    referred_to: Vec::new(),
                    page: Some(page_number),
                    payload: self.region_generic_payload(&bitmap, x, y)?,
                });
            }
        }
        Ok(segments)
    }

    pub(crate) fn encode_residual_regions(
        &self,
        instances: &[&SymbolInstance],
        first_number: u32,
        page_number: u32,
        max_regions: usize,
    ) -> Result<Vec<Segment>> {
        let nonempty: Vec<_> = instances
            .iter()
            .copied()
            .filter(|instance| instance.instance_bitmap.count_ones() != 0)
            .collect();
        if nonempty.is_empty() {
            return Ok(Vec::new());
        }
        let mut best =
            self.encode_residual_groups(&[nonempty.clone()], first_number, page_number)?;
        // Splitting overlapping rectangles preserves composition for OR. Other
        // operators retain the existing single-region representation.
        if !self.config.pack_residuals
            || self.config.generic.comb_operator != 0
            || nonempty.len() < 2
            || max_regions < 2
        {
            return Ok(best);
        }
        let mut best_bytes = encoded_bytes(&best)?;
        let limit = max_regions.min(MAX_RESIDUAL_REGIONS);
        // Three bounded alternatives: one region per component, spatial cells,
        // and text-like row bands. No quadratic pairwise clustering search.
        for strategy in 0..3 {
            if strategy == 0 && nonempty.len() > limit {
                continue;
            }
            let mut groups: BTreeMap<(u32, u32), Vec<&SymbolInstance>> = BTreeMap::new();
            for (index, instance) in nonempty.iter().copied().enumerate() {
                let key = match strategy {
                    0 => (index as u32, 0),
                    1 => (instance.position.y / 128, instance.position.x / 256),
                    _ => (instance.position.y / 64, 0),
                };
                groups.entry(key).or_default().push(instance);
            }
            if groups.len() < 2 || groups.len() > limit {
                continue;
            }
            let groups: Vec<_> = groups.into_values().collect();
            let candidate = self.encode_residual_groups(&groups, first_number, page_number)?;
            let candidate_bytes = encoded_bytes(&candidate)?;
            if candidate_bytes < best_bytes {
                best = candidate;
                best_bytes = candidate_bytes;
            }
        }
        Ok(best)
    }
}

#[cfg(all(test, feature = "decode"))]
mod tests {
    use super::*;
    use crate::decode::{DecodeOptions, decode_embedded};
    use crate::jbig2structs::{Jbig2Config, PageInfo};
    use crate::jbig2sym::{BitImage, Rect};
    use std::time::Instant;

    fn mark(x: u32, y: u32, width: u32, height: u32, empty: bool) -> SymbolInstance {
        let mut bitmap = BitImage::new(width, height).unwrap();
        if !empty {
            for y in 0..height {
                for x in 0..width {
                    bitmap.set(x, y, (x + y) % 3 != 0);
                }
            }
        }
        SymbolInstance {
            symbol_index: 0,
            position: Rect {
                x,
                y,
                width,
                height,
            },
            instance_bitmap: bitmap,
            needs_refinement: false,
            refinement_dx: 0,
            refinement_dy: 0,
        }
    }

    fn check_layout(
        instances: &[SymbolInstance],
        width: u32,
        height: u32,
        page: u32,
    ) -> (usize, usize, usize) {
        let refs: Vec<_> = instances.iter().collect();
        let mut cfg = Jbig2Config::text();
        let encoder = Jbig2Encoder::new(&cfg);
        let start = Instant::now();
        let baseline = encoder
            .encode_residual_regions(&refs, 256, page, MAX_RESIDUAL_REGIONS)
            .unwrap();
        let baseline_time = start.elapsed();
        cfg.pack_residuals = true;
        let encoder = Jbig2Encoder::new(&cfg);
        let start = Instant::now();
        let packed = encoder
            .encode_residual_regions(&refs, 256, page, MAX_RESIDUAL_REGIONS)
            .unwrap();
        let packed_time = start.elapsed();
        let baseline_bytes = encoded_bytes(&baseline).unwrap();
        let packed_bytes = encoded_bytes(&packed).unwrap();
        assert!(packed_bytes <= baseline_bytes);
        assert!(packed.len() <= MAX_RESIDUAL_REGIONS);
        let mut stream = Vec::new();
        Segment {
            number: 255,
            seg_type: SegmentType::PageInformation,
            deferred_non_retain: false,
            retain_flags: 0,
            page_association_type: 0,
            referred_to: vec![],
            page: Some(page),
            payload: PageInfo {
                width,
                height,
                ..Default::default()
            }
            .to_bytes(),
        }
        .write_into(&mut stream)
        .unwrap();
        for (index, segment) in packed.iter().enumerate() {
            assert_eq!(segment.number, 256 + index as u32);
            segment.write_into(&mut stream).unwrap();
        }
        Segment {
            number: 256 + MAX_RESIDUAL_REGIONS as u32,
            seg_type: SegmentType::EndOfPage,
            deferred_non_retain: false,
            retain_flags: 0,
            page_association_type: 0,
            referred_to: vec![],
            page: Some(page),
            payload: vec![],
        }
        .write_into(&mut stream)
        .unwrap();
        let decoded = decode_embedded(None, &stream, &DecodeOptions::default()).unwrap();
        let mut expected = BitImage::new(width, height).unwrap();
        for instance in instances {
            for y in 0..instance.instance_bitmap.height {
                for x in 0..instance.instance_bitmap.width {
                    if instance.instance_bitmap.get_usize(x, y) {
                        expected.set(
                            instance.position.x + x as u32,
                            instance.position.y + y as u32,
                            true,
                        );
                    }
                }
            }
        }
        for y in 0..height {
            for x in 0..width {
                assert_eq!(decoded.get(x, y), expected.get(x, y), "pixel ({x},{y})");
            }
        }
        if let Ok(oracle) = std::env::var("JBIG2DEC") {
            let directory = tempfile::tempdir().unwrap();
            let input = directory.path().join("residual.jb2");
            let output = directory.path().join("residual.pbm");
            std::fs::write(&input, &stream).unwrap();
            let result = std::process::Command::new(oracle)
                .args(["-e", "-o"])
                .arg(&output)
                .arg(&input)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let oracle = super::test_pbm::Pbm::from_p4(&std::fs::read(output).unwrap()).unwrap();
            assert_eq!((oracle.width, oracle.height), (width, height));
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(
                        oracle.get(x, y) != 0,
                        decoded.get(x, y),
                        "oracle pixel ({x},{y})"
                    );
                }
            }
        }
        println!(
            "residual {width}x{height}: baseline={baseline_bytes} packed={packed_bytes} regions={} baseline_us={} packed_us={}",
            packed.len(),
            baseline_time.as_micros(),
            packed_time.as_micros()
        );
        (baseline_bytes, packed_bytes, packed.len())
    }

    #[test]
    fn sparse_residual_layout_wins_and_preserves_every_pixel() {
        let instances = [
            mark(3, 7, 7, 9, false),
            mark(4080, 4080, 7, 9, false),
            mark(0, 0, 1, 1, true),
        ];
        let (baseline, packed, regions) = check_layout(&instances, 4097, 4097, 1);
        assert!(packed < baseline);
        assert_eq!(regions, 2);
    }

    #[test]
    fn dense_overlapping_residuals_do_not_lose_pixels_or_pay_extra_headers() {
        let instances = [
            mark(3, 7, 7, 9, false),
            mark(4, 8, 7, 9, false),
            mark(11, 8, 7, 9, false),
        ];
        let (_, _, regions) = check_layout(&instances, 33, 31, 256);
        assert_eq!(regions, 1);
    }

    #[test]
    fn candidate_limits_cover_many_marks_and_multiple_row_bands() {
        let instances: Vec<_> = (0..130)
            .map(|i| mark((i % 13) * 21, (i / 13) * 33, 3, 5, false))
            .collect();
        check_layout(&instances, 273, 337, 1);
    }

    #[test]
    fn empty_residuals_emit_no_segment() {
        let cfg = Jbig2Config::text();
        let encoder = Jbig2Encoder::new(&cfg);
        let empty = mark(0, 0, 1, 1, true);
        assert!(
            encoder
                .encode_residual_regions(&[&empty], 1, 1, 64)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn packed_regions_stay_before_end_of_page_in_standalone_page_builder() {
        use super::super::types::PlannedPageLayout;
        use crate::encode::document::PageData;

        let mut cfg = Jbig2Config::text();
        cfg.pack_residuals = true;
        let mut encoder = Jbig2Encoder::new(&cfg);
        let mut instances = vec![mark(3, 7, 7, 9, false), mark(4080, 4080, 7, 9, false)];
        instances[1].symbol_index = 1;
        encoder.pages.push(PageData {
            image: BitImage::new(4097, 4097).unwrap(),
            symbol_instances: instances,
        });
        let layout = PlannedPageLayout {
            page_index: 0,
            page_number: 1,
            page_info_segment_number: 1,
            local_dict_segment_numbers: vec![],
            local_dict_layout: None,
            region_segment_number: 2,
            residual_region_segment_number: Some(3),
            end_of_page_segment_number: 3 + MAX_RESIDUAL_REGIONS as u32,
            local_symbols: vec![],
            residual_symbols: vec![0, 1],
            residual_anchor_remaps: Default::default(),
            use_generic_region: false,
        };
        let built = encoder.build_planned_page(&layout, &[], 0, &[]).unwrap();
        let segments = &built.page.segments;
        assert_eq!(
            segments
                .iter()
                .filter(|s| s.seg_type == SegmentType::ImmediateGenericRegion)
                .count(),
            2
        );
        assert!(
            segments
                .windows(2)
                .all(|pair| pair[0].number < pair[1].number)
        );
        assert_eq!(segments.last().unwrap().seg_type, SegmentType::EndOfPage);
        let mut stream = Vec::new();
        for segment in segments {
            segment.write_into(&mut stream).unwrap();
        }
        let decoded = decode_embedded(None, &stream, &DecodeOptions::default()).unwrap();
        assert!(decoded.get(4, 7));
        assert!(decoded.get(4081, 4080));
    }
}
