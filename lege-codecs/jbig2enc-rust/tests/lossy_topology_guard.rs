//! A one-pixel opening is significant even when the old small-glyph signature
//! deliberately omits hole counting. The final guard must survive every remap.
mod common;
use common::pbm::{Pbm, assert_pixels_eq};
use jbig2enc_rust::decode::{DecodeOptions, decode_embedded, decode_file};
use jbig2enc_rust::jbig2enc::Jbig2Encoder;
use jbig2enc_rust::{Array2, Jbig2Config};

#[test]
fn tiny_refinement_reference_offsets_are_portable_across_decoders() {
    use jbig2enc_rust::jbig2enc::{
        SymbolInstance, encode_symbol_dict_with_order, encode_text_region_with_refinement,
    };
    use jbig2enc_rust::jbig2structs::{PageInfo, Segment, SegmentType};
    use jbig2enc_rust::jbig2sym::{BitImage, Rect};
    let mut prototype = BitImage::new(3, 1).unwrap();
    for x in 0..3 {
        prototype.set_usize(x, 0, true);
    }
    let mut target = prototype.clone();
    target.set_usize(1, 0, false);
    let config = Jbig2Config::text_lossless();
    let (dictionary, _) = encode_symbol_dict_with_order(&[&prototype], &config, 0).unwrap();
    for dy in [i32::MIN, -2, -1, 1, 2, i32::MAX] {
        let instances = [2, 12].map(|x| SymbolInstance {
            symbol_index: 0,
            position: Rect {
                x,
                y: 2,
                width: 3,
                height: 1,
            },
            instance_bitmap: target.clone(),
            needs_refinement: true,
            refinement_dx: 0,
            refinement_dy: dy,
        });
        let text = encode_text_region_with_refinement(
            &instances,
            &config,
            &[prototype.clone()],
            &[0],
            1,
            &[u32::MAX],
            0,
        )
        .unwrap();
        let mut stream = Vec::new();
        for (number, kind, referred_to, payload) in [
            (
                0,
                SegmentType::PageInformation,
                vec![],
                PageInfo {
                    width: 31,
                    height: 9,
                    ..Default::default()
                }
                .to_bytes(),
            ),
            (1, SegmentType::SymbolDictionary, vec![], dictionary.clone()),
            (2, SegmentType::ImmediateLosslessTextRegion, vec![1], text),
        ] {
            Segment {
                number,
                seg_type: kind,
                deferred_non_retain: false,
                retain_flags: 0,
                page_association_type: 0,
                referred_to,
                page: Some(1),
                payload,
            }
            .write_into(&mut stream)
            .unwrap();
        }
        let mut expected = Pbm::new(31, 9, vec![0; 31 * 9]);
        for left in [2, 12] {
            for x in [0, 2] {
                expected.pixels[2 * 31 + left + x] = 1;
            }
        }
        let decoded = decode_embedded(None, &stream, &DecodeOptions::default()).unwrap();
        for y in 0..9 {
            for x in 0..31 {
                assert_eq!(decoded.get(x, y), expected.get(x, y) != 0);
            }
        }
        if let Some(oracle) = common::oracle::decode_embedded(None, &stream) {
            assert_pixels_eq(&expected, &oracle.unwrap(), "tiny refinement jbig2dec");
        }
        for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
            if let Some(rendered) =
                common::pdf::render(backend, &common::pdf::wrap_page(31, 9, None, &stream))
            {
                let control =
                    common::pdf::render(backend, &common::pdf::wrap_uncompressed(&expected))
                        .unwrap();
                assert_pixels_eq(&control, &rendered, backend);
            }
        }
    }
}

fn source() -> Pbm {
    let (width, height) = (257, 65);
    let mut image = Pbm::new(width, height, vec![0; (width * height) as usize]);
    for index in 0..48 {
        let (left, top) = (4 + (index % 24) * 10, 4 + (index / 24) * 20);
        for y in 0..7 {
            for x in 0..5 {
                let border = x == 0 || x == 4 || y == 0 || y == 6;
                let opening = index % 2 == 1 && x == 4 && y == 2;
                if border && !opening {
                    image.pixels[((top + y) * width + left + x) as usize] = 1;
                }
            }
        }
    }
    image
}

#[test]
fn bounded_measured_lossy_planner_distinguishes_substitution_and_refinement() {
    let mut source = Pbm::new(193, 193, vec![0; 193 * 193]);
    let mut glyph = vec![0u8; 32 * 32];
    let mut random = 0x12345678u32;
    for y in 0..32 {
        for x in 0..32 {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            glyph[y * 32 + x] = u8::from(y % 2 == 0 || x == 0 || x == 31 || random >> 29 < 3);
        }
    }
    for index in 0..16 {
        let mut bitmap = glyph.clone();
        if index >= 4 {
            bitmap[3 * 32 + 2 + index - 4] ^= 1;
        }
        let (left, top) = (5 + (index % 4) * 40, 5 + (index / 4) * 40);
        for y in 0..32 {
            for x in 0..32 {
                source.pixels[(top + y) * 193 + left + x] = bitmap[y * 32 + x];
            }
        }
    }
    let input = Array2::from_shape_vec((193, 193), source.pixels.clone()).unwrap();
    let config = Jbig2Config::text_lossy_max_compression(1);
    let mut encoder = Jbig2Encoder::new(&config);
    encoder.add_page(&input).unwrap();
    encoder.add_page(&input).unwrap();
    let split = encoder.flush_pdf_split().unwrap();
    let metrics = encoder.metrics_snapshot();
    let report = &metrics.optimization;
    println!(
        "bounded lossy baseline={} selected={} refined={} substituted={}",
        report.baseline_bytes,
        report.selected_bytes,
        report.refined_instances,
        report.substituted_instances
    );
    assert!(report.trials.len() <= config.symbol_trial_limit);
    assert!(
        report.candidate_comparisons
            <= metrics.symbol_stats.symbols_discovered * config.symbol_candidate_limit
    );
    assert!(
        report
            .trials
            .iter()
            .any(|trial| trial.representation.contains("direct substitution"))
    );
    assert!(report.substituted_instances > 0);
    assert!(report.selected_bytes < report.baseline_bytes);
    let mut differences = 0usize;
    for stream in &split.page_streams {
        let globals = split.global_segments.as_deref();
        let decoded = decode_embedded(globals, stream, &DecodeOptions::default()).unwrap();
        let bitmap = Pbm::new(
            193,
            193,
            (0..193)
                .flat_map(|y| {
                    let decoded = &decoded;
                    (0..193).map(move |x| u8::from(decoded.get(x, y)))
                })
                .collect(),
        );
        differences += source
            .pixels
            .iter()
            .zip(&bitmap.pixels)
            .filter(|(a, b)| a != b)
            .count();
        if let Some(oracle) = common::oracle::decode_embedded(globals, stream) {
            assert_pixels_eq(&bitmap, &oracle.unwrap(), "measured lossy jbig2dec");
        }
        for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
            let pdf = common::pdf::wrap_page(193, 193, globals, stream);
            if let Some(rendered) = common::pdf::render(backend, &pdf) {
                let control =
                    common::pdf::render(backend, &common::pdf::wrap_uncompressed(&bitmap)).unwrap();
                assert_pixels_eq(&control, &rendered, backend);
            }
        }
    }
    assert!(differences > 0);
    assert!(differences <= report.substituted_instances);
    let mut invalid = config.clone();
    invalid.is_lossless = true;
    assert!(Jbig2Encoder::try_new(&invalid).is_err());
}

#[test]
fn final_guard_corrects_one_pixel_hole_closures_after_lossy_unification() {
    let source = source();
    let input = Array2::from_shape_vec(
        (source.height as usize, source.width as usize),
        source.pixels.clone(),
    )
    .unwrap();
    for pdf in [false, true] {
        let mut observed = Vec::new();
        for safe in [false, true] {
            let config = if safe {
                Jbig2Config::text_symbol_unify_safe()
            } else {
                Jbig2Config::text_symbol_unify()
            };
            let mut encoder = Jbig2Encoder::new(&config);
            encoder.add_page(&input).unwrap();
            let decoded = if pdf {
                let split = encoder.flush_pdf_split().unwrap();
                let globals = split.global_segments.as_deref();
                let stream = &split.page_streams[0];
                let decoded = decode_embedded(globals, stream, &DecodeOptions::default()).unwrap();
                if safe {
                    if let Some(oracle) = common::oracle::decode_embedded(globals, stream) {
                        assert_pixels_eq(&source, &oracle.unwrap(), "guarded jbig2dec");
                    }
                    let pdf = common::pdf::wrap_page(source.width, source.height, globals, stream);
                    for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
                        if let Some(rendered) = common::pdf::render(backend, &pdf) {
                            let control = common::pdf::render(
                                backend,
                                &common::pdf::wrap_uncompressed(&source),
                            )
                            .unwrap();
                            assert_pixels_eq(&control, &rendered, backend);
                        }
                    }
                }
                decoded
            } else {
                let stream = encoder.flush().unwrap();
                if safe {
                    if let Some(oracle) = common::oracle::decode_standalone(&stream) {
                        assert_pixels_eq(&source, &oracle.unwrap(), "guarded standalone jbig2dec");
                    }
                }
                decode_file(&stream, &DecodeOptions::default())
                    .unwrap()
                    .pages
                    .remove(0)
                    .bitmap
            };
            let differences = (0..source.height)
                .flat_map(|y| (0..source.width).map(move |x| (x, y)))
                .filter(|&(x, y)| (source.get(x, y) != 0) != decoded.get(x, y))
                .count();
            if safe {
                assert_eq!(
                    differences, 0,
                    "all components in this fixture are retained"
                );
                assert!(encoder.metrics_snapshot().symbol_stats.topology_refinements > 0);
            }
            observed.push(differences);
        }
        assert!(
            observed[0] > 0,
            "fixture must expose the legacy substitution"
        );
        println!(
            "topology pdf={pdf}: legacy_differences={} guarded_differences={}",
            observed[0], observed[1]
        );
    }
}
