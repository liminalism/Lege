mod common;
use common::pbm::{Pbm, assert_pixels_eq};
use jbig2enc_rust::decode::{DecodeOptions, decode_embedded, decode_file};
use jbig2enc_rust::jbig2enc::Jbig2Encoder;
use jbig2enc_rust::jbig2structs::LossySymbolMode;
use jbig2enc_rust::{Array2, Jbig2Config, PdfDictionaryPolicy};

fn fixture(variation: u32) -> Pbm {
    let (width, height) = (321, 129);
    let mut pixels = vec![0; (width * height) as usize];
    let mut set = |x: u32, y: u32| pixels[(y * width + x) as usize] = 1;
    for index in 0..12 {
        let (left, top) = (5 + (index % 6) * 30, 7 + (index / 6) * 30);
        for y in 0..13 {
            for x in 0..9 {
                if x == 0 || x == 8 || y == 0 || y == 12 || (index % 3 == 1 && y == 6) {
                    set(left + x, top + y);
                }
            }
        }
        if index % 3 == 2 {
            set(left + 1 + variation % 3, top + 1);
        }
    }
    // Single pixels, touching diagonals, a dense component and long thin rule.
    for (x, y) in [
        (0, 0),
        (320, 128),
        (1, 126),
        (157, 124),
        (319, 2),
        (230, 80),
        (231, 81),
    ] {
        set(x, y);
    }
    for y in 90..110 {
        for x in 230..260 {
            set(x, y);
        }
    }
    for x in 3..300 {
        set(x, 119);
    }
    Pbm::new(width, height, pixels)
}

fn array(source: &Pbm) -> Array2<u8> {
    Array2::from_shape_vec(
        (source.height as usize, source.width as usize),
        source.pixels.clone(),
    )
    .unwrap()
}

fn assert_native(source: &Pbm, decoded: &jbig2enc_rust::shared::bitmap::MonoBitmap) {
    let pixels = (0..source.height)
        .flat_map(|y| (0..source.width).map(move |x| u8::from(decoded.get(x, y))))
        .collect();
    assert_pixels_eq(
        source,
        &Pbm::new(decoded.width(), decoded.height(), pixels),
        "exact native",
    );
}

#[test]
fn exact_symbols_preserve_all_components_in_pdf_and_standalone() {
    let sources = [fixture(0), fixture(1), fixture(2)];
    for policy in [
        PdfDictionaryPolicy::AllGlobal,
        PdfDictionaryPolicy::SharedGlobal,
        PdfDictionaryPolicy::SharedGlobalWithLocal,
    ] {
        let mut config = Jbig2Config::text_lossless();
        config.pdf_dictionary_policy = policy;
        // These tuning switches must not enable fuzzy clustering or aliasing.
        config.auto_thresh = true;
        config.refine = true;
        config.text_refine = true;
        for pdf in [false, true] {
            let mut encoder = Jbig2Encoder::new(&config);
            for source in &sources {
                encoder.add_page(&array(source)).unwrap();
            }
            if pdf {
                let split = encoder.flush_pdf_split().unwrap();
                assert!(
                    split.text_region_bytes_per_page.iter().sum::<usize>() > 0,
                    "must exercise symbol compression"
                );
                for (source, stream) in sources.iter().zip(&split.page_streams) {
                    let globals = split.global_segments.as_deref();
                    assert_native(
                        source,
                        &decode_embedded(globals, stream, &DecodeOptions::default()).unwrap(),
                    );
                    if let Some(oracle) = common::oracle::decode_embedded(globals, stream) {
                        assert_pixels_eq(source, &oracle.unwrap(), "exact jbig2dec");
                    }
                    let pdf = common::pdf::wrap_page(source.width, source.height, globals, stream);
                    for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
                        if let Some(rendered) = common::pdf::render(backend, &pdf) {
                            let control = common::pdf::render(
                                backend,
                                &common::pdf::wrap_uncompressed(source),
                            )
                            .unwrap();
                            assert_pixels_eq(&control, &rendered, backend);
                            if backend == "PDFIUM_PYTHON" {
                                assert_pixels_eq(source, &rendered, "exact PDFium");
                            }
                        }
                    }
                }
            } else {
                let bytes = encoder.flush().unwrap();
                let decoded = decode_file(&bytes, &DecodeOptions::default()).unwrap();
                assert_eq!(sources.len(), decoded.pages.len());
                for (source, page) in sources.iter().zip(&decoded.pages) {
                    assert_native(source, &page.bitmap);
                }
            }
            assert!(encoder.metrics_snapshot().symbol_stats.exact_hits > 0);
        }
    }
}

#[test]
fn exact_symbols_preserve_sparse_odd_widths_blank_and_solid_pages() {
    for width in [1, 7, 9, 31, 33, 65, 129] {
        for pattern in 0..3 {
            let height = 37;
            let pixels = (0..width * height)
                .map(|index| u8::from(pattern == 1 || (pattern == 2 && index % 41 == 0)))
                .collect();
            let source = Pbm::new(width, height, pixels);
            let cfg = Jbig2Config::text_lossless();
            let mut encoder = Jbig2Encoder::new(&cfg);
            encoder.add_page(&array(&source)).unwrap();
            let split = encoder.flush_pdf_split().unwrap();
            assert_native(
                &source,
                &decode_embedded(
                    split.global_segments.as_deref(),
                    &split.page_streams[0],
                    &DecodeOptions::default(),
                )
                .unwrap(),
            );
        }
    }
}

#[test]
fn conflicting_exact_fidelity_settings_fail_before_ingest() {
    for conflict in 0..9 {
        let mut cfg = Jbig2Config::text_lossless();
        match conflict {
            0 => cfg.lossy_symbol_mode = LossySymbolMode::SymbolUnify,
            1 => cfg.text_comb_op = 1,
            2 => cfg.generic.comb_operator = 4,
            3 => cfg.default_pixel = true,
            4 => cfg.text_transposed = true,
            5 => cfg.text_ref_corner = 0,
            6 => cfg.text_refine_template = 1,
            7 => cfg.text_log_strips = 4,
            _ => cfg.text_ds_offset = 16,
        }
        assert!(Jbig2Encoder::try_new(&cfg).is_err());
        let mut encoder = Jbig2Encoder::new(&cfg);
        assert!(encoder.add_page(&array(&fixture(0))).is_err());
        assert_eq!(encoder.get_page_count(), 0);
        assert!(encoder.flush_pdf_split().is_err());
    }
}

#[test]
fn exact_text_strip_sizes_and_signed_offsets_preserve_source_pixels() {
    let source = fixture(0);
    for strips in 0..=3 {
        for offset in [-16, 0, 15] {
            let mut cfg = Jbig2Config::text_lossless();
            cfg.text_log_strips = strips;
            cfg.text_ds_offset = offset;
            let mut encoder = Jbig2Encoder::new(&cfg);
            encoder.add_page(&array(&source)).unwrap();
            let split = encoder.flush_pdf_split().unwrap();
            assert_native(
                &source,
                &decode_embedded(
                    split.global_segments.as_deref(),
                    &split.page_streams[0],
                    &DecodeOptions::default(),
                )
                .unwrap(),
            );
        }
    }
}

#[test]
fn optimized_exact_streams_measure_real_costs_with_a_hard_trial_budget() {
    for pdf in [false, true] {
        let sources = [fixture(0), fixture(1), fixture(2)];
        let mut cfg = Jbig2Config::text_lossless_max_compression();
        cfg.symbol_trial_limit = 16;
        cfg.symbol_candidate_limit = 8;
        let mut encoder = Jbig2Encoder::new(&cfg);
        for source in &sources {
            encoder.add_page(&array(source)).unwrap();
        }
        let actual_bytes;
        if pdf {
            let split = encoder.flush_pdf_split().unwrap();
            actual_bytes = split.global_segments.as_ref().map_or(0, Vec::len)
                + split.page_streams.iter().map(Vec::len).sum::<usize>();
            for (source, stream) in sources.iter().zip(&split.page_streams) {
                assert_native(
                    source,
                    &decode_embedded(
                        split.global_segments.as_deref(),
                        stream,
                        &DecodeOptions::default(),
                    )
                    .unwrap(),
                );
                if let Some(oracle) =
                    common::oracle::decode_embedded(split.global_segments.as_deref(), stream)
                {
                    assert_pixels_eq(source, &oracle.unwrap(), "optimized exact jbig2dec");
                }
            }
        } else {
            let bytes = encoder.flush().unwrap();
            actual_bytes = bytes.len();
            let decoded = decode_file(&bytes, &DecodeOptions::default()).unwrap();
            for (source, page) in sources.iter().zip(&decoded.pages) {
                assert_native(source, &page.bitmap);
            }
        }
        let report = encoder.metrics_snapshot().optimization;
        assert_eq!(
            report.selected_bytes, actual_bytes,
            "must include serialized headers"
        );
        assert!(report.selected_bytes <= report.baseline_bytes);
        assert!(report.selected_bytes <= report.generic_bytes);
        for policy in [
            PdfDictionaryPolicy::AllGlobal,
            PdfDictionaryPolicy::SharedGlobal,
            PdfDictionaryPolicy::SharedGlobalWithLocal,
        ] {
            let mut baseline_config = Jbig2Config::text_lossless();
            baseline_config.pdf_dictionary_policy = policy;
            let mut baseline = Jbig2Encoder::new(&baseline_config);
            for source in &sources {
                baseline.add_page(&array(source)).unwrap();
            }
            let baseline_bytes = if pdf {
                let split = baseline.flush_pdf_split().unwrap();
                split.global_segments.as_ref().map_or(0, Vec::len)
                    + split.page_streams.iter().map(Vec::len).sum::<usize>()
            } else {
                baseline.flush().unwrap().len()
            };
            assert!(
                report.selected_bytes <= baseline_bytes,
                "optimizer must retain cheaper exact policy {policy:?}"
            );
        }
        assert!(report.trials.len() <= cfg.symbol_trial_limit);
        assert!(
            report.candidate_comparisons
                <= encoder.metrics_snapshot().symbol_stats.symbols_discovered
                    * cfg.symbol_candidate_limit
        );
        assert!(
            report
                .trials
                .iter()
                .any(|trial| trial.representation.ends_with("refinement"))
        );
        println!(
            "optimized pdf={pdf}: baseline={} generic={} selected={} trials={} exact={} refined={} generic_instances={}",
            report.baseline_bytes,
            report.generic_bytes,
            report.selected_bytes,
            report.trials.len(),
            report.exact_instances,
            report.refined_instances,
            report.generic_instances
        );
    }
}

#[test]
fn measured_refinement_is_selected_when_it_reduces_complete_stream_cost() {
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
    let mut cfg = Jbig2Config::text_lossless_max_compression();
    cfg.symbol_trial_limit = 64;
    let mut encoder = Jbig2Encoder::new(&cfg);
    encoder.add_page(&array(&source)).unwrap();
    encoder.add_page(&array(&source)).unwrap();
    let split = encoder.flush_pdf_split().unwrap();
    let report = encoder.metrics_snapshot().optimization;
    println!(
        "refinement candidate summary: baseline={} generic={} selected={} comparisons={} refined={} generic_instances={}",
        report.baseline_bytes,
        report.generic_bytes,
        report.selected_bytes,
        report.candidate_comparisons,
        report.refined_instances,
        report.generic_instances
    );
    for trial in report.trials.iter().take(16) {
        println!("{trial:?}");
    }
    assert!(
        report.refined_instances > 0,
        "counterfactual planner must select real refinement"
    );
    assert!(report.selected_bytes < report.baseline_bytes);
    for stream in &split.page_streams {
        let globals = split.global_segments.as_deref();
        assert_native(
            &source,
            &decode_embedded(globals, stream, &DecodeOptions::default()).unwrap(),
        );
        if let Some(oracle) = common::oracle::decode_embedded(globals, stream) {
            assert_pixels_eq(&source, &oracle.unwrap(), "selected refinement jbig2dec");
        }
        let pdf = common::pdf::wrap_page(source.width, source.height, globals, stream);
        for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
            if let Some(rendered) = common::pdf::render(backend, &pdf) {
                let control =
                    common::pdf::render(backend, &common::pdf::wrap_uncompressed(&source)).unwrap();
                assert_pixels_eq(&control, &rendered, backend);
            }
        }
    }
    println!(
        "refinement win: baseline={} generic={} selected={} refined_instances={} trials={}",
        report.baseline_bytes,
        report.generic_bytes,
        report.selected_bytes,
        report.refined_instances,
        report.trials.len()
    );
}
