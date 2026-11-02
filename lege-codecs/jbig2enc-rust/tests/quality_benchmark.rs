//! Isolated corpus worker for tools/quality_benchmark.py. No corpus/tool skips:
//! an explicitly requested benchmark must fail when its inputs are unavailable.
mod common;
#[path = "quality_benchmark/grouped.rs"]
mod grouped;
#[path = "quality_benchmark/memory.rs"]
mod memory;

// ENCODE_MEMORY_WORKER_BEGIN
#[test]
#[ignore = "encoding-only child; tools/measure_encode_memory.py verifies after exit"]
fn quality_encode_only_worker() {
    memory::run();
}
// ENCODE_MEMORY_WORKER_END

use common::pbm::Pbm;
use jbig2enc_rust::decode::{
    DecodeOptions, DecoderContext, decode_embedded, decode_embedded_with_globals, decode_globals,
};
use jbig2enc_rust::jbig2enc::Jbig2Encoder;
use jbig2enc_rust::{Jbig2Config, PdfDictionaryPolicy, encode_single_image_lossless};
use ndarray::Array2;
use std::path::PathBuf;
use std::time::Instant;

fn config(mode: &str) -> Jbig2Config {
    let mut cfg = match mode {
        "generic" => Jbig2Config::lossless(),
        "exact" => Jbig2Config::text_lossless(),
        "exact_optimized" => Jbig2Config::text_lossless_max_compression(),
        "lossy_optimized" => Jbig2Config::text_lossy_max_compression(1),
        "exact_allglobal" => {
            let mut cfg = Jbig2Config::text_lossless();
            cfg.pdf_dictionary_policy = PdfDictionaryPolicy::AllGlobal;
            cfg
        }
        "symbol_shipping" | "symbol_fixed" => Jbig2Config::text(),
        "symbol_shared" | "symbol_local" => {
            let mut cfg = Jbig2Config::text();
            cfg.pdf_dictionary_policy = if mode == "symbol_shared" {
                PdfDictionaryPolicy::SharedGlobal
            } else {
                PdfDictionaryPolicy::SharedGlobalWithLocal
            };
            cfg.pack_residuals = true;
            cfg
        }
        "unify_shipping" | "unify_fixed" => Jbig2Config::text_symbol_unify(),
        "unify_safe" => Jbig2Config::text_symbol_unify_safe(),
        "refine_shipping" => {
            let mut cfg = Jbig2Config::text();
            cfg.refine = true;
            cfg.text_refine = true;
            cfg
        }
        _ => panic!("unknown QUALITY_MODE {mode}"),
    };
    if mode.ends_with("_fixed") {
        cfg.auto_thresh = false;
    }
    cfg.want_full_headers = false;
    if let Ok(limit) = std::env::var("QUALITY_GLOBAL_BITMAP_LIMIT") {
        cfg.decoder_resource_limits.max_global_bitmap_bytes =
            Some(limit.parse().expect("global bitmap cap"));
    }
    cfg
}

fn differences(source: &Pbm, decoded: &jbig2enc_rust::shared::bitmap::MonoBitmap) -> u64 {
    assert_eq!(
        (source.width, source.height),
        (decoded.width(), decoded.height())
    );
    (0..source.height)
        .flat_map(|y| (0..source.width).map(move |x| (x, y)))
        .filter(|&(x, y)| (source.get(x, y) != 0) != decoded.get(x, y))
        .count() as u64
}

fn oracle_differences(source: &Pbm, oracle: &Pbm) -> u64 {
    assert_eq!((source.width, source.height), (oracle.width, oracle.height));
    source
        .pixels
        .iter()
        .zip(&oracle.pixels)
        .filter(|(a, b)| a != b)
        .count() as u64
}

#[test]
#[ignore = "run with tools/quality_benchmark.py; requires an explicit corpus and mode"]
fn quality_corpus_worker() {
    let directory = PathBuf::from(std::env::var_os("QUALITY_CORPUS").expect("QUALITY_CORPUS"));
    let mode = std::env::var("QUALITY_MODE").expect("QUALITY_MODE");
    let render_pdf = std::env::var("QUALITY_PDF_ORACLES").is_ok_and(|value| value == "1");
    let require_oracle = std::env::var("QUALITY_REQUIRE_ORACLE").is_ok_and(|v| v == "1");
    let oracle_available = common::oracle::jbig2dec_path().is_some();
    assert!(
        !require_oracle || oracle_available,
        "required jbig2dec oracle is unavailable"
    );
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pbm"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "corpus has no PBM pages");
    if let Some((base, size)) = mode.rsplit_once("_group") {
        let cap = if size == "whole" {
            usize::MAX
        } else {
            size.parse().expect("group page count")
        };
        grouped::run(&paths, &mode, base, cap, render_pdf, require_oracle);
        return;
    }
    let pages: Vec<_> = paths
        .iter()
        .map(|path| Pbm::from_p4(&std::fs::read(path).unwrap()).unwrap())
        .collect();
    let arrays: Vec<_> = pages
        .iter()
        .map(|page| {
            Array2::from_shape_vec(
                (page.height as usize, page.width as usize),
                page.pixels.clone(),
            )
            .unwrap()
        })
        .collect();
    let cfg = config(&mode);
    let canonical_termination = cfg.canonical_arithmetic_termination;
    let start = Instant::now();
    let mut encoder = Jbig2Encoder::new(&cfg);
    for page in &arrays {
        encoder.add_page(page).unwrap();
    }
    let split = encoder.flush_pdf_split().unwrap();
    let encode_seconds = start.elapsed().as_secs_f64();
    let metrics = encoder.metrics_snapshot();
    grouped::print_resources(0, &metrics.resources);
    println!(
        "QUALITY_MATCHING\tcomponents={}\tingest_comparisons={}\tplanner_comparisons={}\tlegacy_post_ingest_comparisons=unreported",
        metrics.symbol_stats.input_components,
        metrics.symbol_stats.comparator_calls,
        metrics.optimization.candidate_comparisons
    );
    println!(
        "QUALITY_TOPOLOGY\tcorrected_instances={}",
        metrics.symbol_stats.topology_refinements
    );
    for trial in &metrics.optimization.trials {
        println!(
            "QUALITY_TRIAL\t{:?}\t{}\t{:?}\t{:?}\t{:?}\t{}\traw={:?}",
            trial.symbol,
            trial.representation,
            trial.policy,
            trial.estimated_gain_bytes,
            trial.measured_gain_bytes,
            trial.encoded_bytes,
            trial.raw_estimated_gain_bytes
        );
    }
    if !metrics.optimization.trials.is_empty() {
        println!(
            "QUALITY_OPTIMIZATION\ttrials={}\tcomparisons={}\tbaseline={}\tgeneric={}\tselected={}\texact_instances={}\trefined_instances={}\tgeneric_instances={}\tsubstituted_instances={}",
            metrics.optimization.trials.len(),
            metrics.optimization.candidate_comparisons,
            metrics.optimization.baseline_bytes,
            metrics.optimization.generic_bytes,
            metrics.optimization.selected_bytes,
            metrics.optimization.exact_instances,
            metrics.optimization.refined_instances,
            metrics.optimization.generic_instances,
            metrics.optimization.substituted_instances
        );
    }
    println!(
        "QUALITY_STAGES\tcc={:.9}\tmatch={:.9}\tcluster={:.9}\tplan={:.9}\tdict={:.9}\ttext={:.9}\tgeneric={:.9}",
        metrics.symbol_mode.cc_extraction.as_secs_f64(),
        metrics.symbol_mode.matching_dedup.as_secs_f64(),
        metrics.symbol_mode.clustering.as_secs_f64(),
        metrics.symbol_mode.planning.as_secs_f64(),
        metrics.symbol_mode.symbol_dict_encoding.as_secs_f64(),
        metrics.symbol_mode.text_region_encoding.as_secs_f64(),
        metrics.symbol_mode.generic_region_encoding.as_secs_f64()
    );
    assert_eq!(split.page_streams.len(), pages.len());
    // Encoding time excludes verification; encoder is dropped before decoding.
    drop(encoder);
    let options = DecodeOptions::default();
    let globals = split.global_segments.as_deref();
    let artifacts = std::env::var_os("QUALITY_ARTIFACT_DIR").map(PathBuf::from);
    if let Some(directory) = &artifacts {
        std::fs::create_dir_all(directory).unwrap();
        if let Some(globals) = globals {
            std::fs::write(directory.join("globals.jb2"), globals).unwrap();
        }
        for (index, stream) in split.page_streams.iter().enumerate() {
            std::fs::write(directory.join(format!("page-{index:04}.jb2")), stream).unwrap();
        }
    }
    let decoded_globals = globals.map(|bytes| decode_globals(bytes, &options).unwrap());
    let dictionary_memory = decoded_globals.as_ref().map_or(0, |g| g.retained_bytes());
    assert_eq!(
        dictionary_memory, metrics.resources.global_dictionary_bitmap_bytes,
        "predicted exported bitmap storage must equal native retained globals"
    );
    let mut context = DecoderContext::new();
    let mut total_differences = 0;
    let mut cold_seconds = 0.0;
    let mut warm_seconds = 0.0;
    let mut oracle_total = 0;
    for (index, (source, stream)) in pages.iter().zip(&split.page_streams).enumerate() {
        // Cold includes globals parsing and decoding on each page. It is a
        // library cold-load measurement, not an OS cache flush or device test.
        let start = Instant::now();
        let cold = decode_embedded(globals, stream, &options).unwrap();
        let cold_s = start.elapsed().as_secs_f64();
        let difference = differences(source, &cold);
        // Prime the worker context before timing the warm decode.
        let warm =
            decode_embedded_with_globals(decoded_globals.as_ref(), stream, &options, &mut context)
                .unwrap();
        assert_eq!(cold, warm);
        let start = Instant::now();
        let warm =
            decode_embedded_with_globals(decoded_globals.as_ref(), stream, &options, &mut context)
                .unwrap();
        let warm_s = start.elapsed().as_secs_f64();
        assert_eq!(cold, warm);
        let oracle_diff = if oracle_available {
            let oracle = common::oracle::decode_embedded(globals, stream)
                .unwrap()
                .unwrap();
            // A lossy encoder may change the source; independent decoders must
            // nevertheless agree with each other on every output pixel.
            assert_eq!(
                oracle.pixels,
                (0..source.height)
                    .flat_map(|y| {
                        let cold = &cold;
                        (0..source.width).map(move |x| u8::from(cold.get(x, y)))
                    })
                    .collect::<Vec<_>>(),
                "decoder disagreement on page {index}"
            );
            let diff = oracle_differences(source, &oracle);
            oracle_total += diff;
            diff.to_string()
        } else {
            "unavailable".to_string()
        };
        if render_pdf {
            let decoded = Pbm::new(
                source.width,
                source.height,
                (0..source.height)
                    .flat_map(|y| {
                        let cold = &cold;
                        (0..source.width).map(move |x| u8::from(cold.get(x, y)))
                    })
                    .collect(),
            );
            let pdf = common::pdf::wrap_page(source.width, source.height, globals, stream);
            let control = common::pdf::wrap_uncompressed(&decoded);
            if let Some(directory) = &artifacts {
                std::fs::write(directory.join(format!("page-{index:04}.pdf")), &pdf).unwrap();
                std::fs::write(
                    directory.join(format!("decoded-{index:04}.pbm")),
                    decoded.to_p4(),
                )
                .unwrap();
            }
            for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
                let (rendered, diagnostics) = common::pdf::render_with_diagnostics(backend, &pdf)
                    .expect("required PDF backend");
                let trailing_warnings = diagnostics.matches("extraneous byte").count();
                if canonical_termination {
                    assert_eq!(
                        trailing_warnings, 0,
                        "canonical {backend} stream has trailing arithmetic bytes"
                    );
                }
                println!(
                    "QUALITY_PDF_DIAGNOSTICS\t{index}\t{backend}\ttrailing_bytes={trailing_warnings}"
                );
                let baseline =
                    common::pdf::render(backend, &control).expect("required PDF control");
                common::pbm::assert_pixels_eq(
                    &baseline,
                    &rendered,
                    &format!("{backend} rendered control equality"),
                );
                if backend == "PDFIUM_PYTHON" {
                    common::pbm::assert_pixels_eq(
                        &decoded,
                        &rendered,
                        "PDFium native bitmap equality",
                    );
                }
                println!("QUALITY_PDF\t{index}\t{backend}\tpass");
            }
        }
        println!(
            "QUALITY_PAGE\t{index}\t{}\t{difference}\t{cold_s:.9}\t{warm_s:.9}\t{oracle_diff}",
            stream.len()
        );
        total_differences += difference;
        cold_seconds += cold_s;
        warm_seconds += warm_s;
    }
    if cfg.is_lossless {
        assert_eq!(total_differences, 0, "lossless input-pixel contract");
    }
    let globals_bytes = globals.map_or(0, |g| g.len());
    let bytes = globals_bytes + split.page_streams.iter().map(Vec::len).sum::<usize>();
    let residual_bytes: usize = split.generic_region_bytes_per_page.iter().sum();
    let local_bytes: usize = split.local_dict_bytes_per_page.iter().sum();
    println!(
        "QUALITY_RESULT\t{mode}\t{}\t{}\t{bytes}\t{globals_bytes}\t{local_bytes}\t{residual_bytes}\t{encode_seconds:.9}\t{total_differences}\t{cold_seconds:.9}\t{warm_seconds:.9}\t{dictionary_memory}\t{}\t{}\t{}\t{}",
        pages.len(),
        cfg.auto_thresh,
        metrics.symbol_stats.comparator_calls,
        metrics.symbol_stats.exact_hits,
        metrics.symbol_stats.refined_hits,
        if oracle_available {
            oracle_total.to_string()
        } else {
            "unavailable".into()
        }
    );
}

#[test]
fn shipping_and_fixed_threshold_controls_are_distinct() {
    for prefix in ["symbol", "unify"] {
        assert!(config(&format!("{prefix}_shipping")).auto_thresh);
        assert!(!config(&format!("{prefix}_fixed")).auto_thresh);
    }
}

#[test]
fn lossless_api_preserves_isolated_marks_at_odd_widths() {
    for width in [1, 7, 9, 31, 33, 65, 129] {
        let height = 37;
        let mut pixels = vec![0; (width * height) as usize];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            *pixel = u8::from(index % 41 == 0 || index % 67 == 3);
        }
        let source = Pbm::new(width, height, pixels);
        for pdf in [false, true] {
            let encoded = encode_single_image_lossless(&source.pixels, width, height, pdf).unwrap();
            let decoded = if pdf {
                decode_embedded(
                    encoded.global_data.as_deref(),
                    &encoded.page_data,
                    &DecodeOptions::default(),
                )
                .unwrap()
            } else {
                jbig2enc_rust::decode::decode_file(&encoded.page_data, &DecodeOptions::default())
                    .unwrap()
                    .pages
                    .remove(0)
                    .bitmap
            };
            assert_eq!(differences(&source, &decoded), 0, "{width}px pdf={pdf}");
        }
    }
}
