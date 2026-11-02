//! A byte-level interoperability probe, independent of coder implementation.
//! Only arithmetic segments produced by the test encoder are transformed.
mod common;

use common::pbm::{Pbm, assert_pixels_eq};
use jbig2enc_rust::decode::file::parse_auto;
use jbig2enc_rust::decode::segment::RetainFlags;
use jbig2enc_rust::decode::{DecodeLimits, DecodeOptions, decode_embedded, decode_file};
use jbig2enc_rust::jbig2structs::Segment;
use jbig2enc_rust::{Jbig2Config, Jbig2Context, encode_single_image_with_config};

fn collapse_duplicate_stop_prefix(stream: &[u8]) -> (Vec<u8>, usize) {
    let parsed = parse_auto(stream, &DecodeLimits::default()).unwrap();
    let mut output = Vec::new();
    let mut changed = 0;
    for segment in parsed.segments {
        let header = segment.header;
        let mut payload = segment.data.to_vec();
        if matches!(header.type_code, 0 | 4 | 6 | 7 | 36 | 38 | 39)
            && payload.ends_with(&[0xff, 0xff, 0xac])
        {
            payload.remove(payload.len() - 3);
            changed += 1;
        }
        let seg_type = header.segment_type().unwrap();
        let RetainFlags::Short(retain_flags) = header.retain_flags else {
            panic!("probe only handles the encoder's short reference form")
        };
        Segment {
            number: header.number,
            seg_type,
            deferred_non_retain: header.deferred_non_retain,
            retain_flags,
            page_association_type: 0,
            referred_to: header.referred_to,
            page: Some(header.page_association),
            payload,
        }
        .write_into(&mut output)
        .unwrap();
    }
    (output, changed)
}

fn native(source: &Pbm, globals: Option<&[u8]>, page: &[u8]) {
    let decoded = decode_embedded(globals, page, &DecodeOptions::default()).unwrap();
    check_bitmap(source, &decoded);
}

fn check_bitmap(source: &Pbm, decoded: &jbig2enc_rust::shared::bitmap::MonoBitmap) {
    let pixels = (0..source.height)
        .flat_map(|y| {
            let decoded = &decoded;
            (0..source.width).map(move |x| decoded.get(x, y) as u8)
        })
        .collect();
    assert_pixels_eq(
        source,
        &Pbm::new(source.width, source.height, pixels),
        "native padding probe",
    );
}

#[test]
fn canonical_termination_is_explicit_for_legacy_callers_and_enabled_in_new_presets() {
    for cfg in [
        Jbig2Config::default(),
        Jbig2Config::generic(),
        Jbig2Config::lossless(),
        Jbig2Config::text(),
        Jbig2Config::text_symbol_unify(),
    ] {
        assert!(!cfg.canonical_arithmetic_termination);
    }
    for cfg in [
        Jbig2Config::text_lossless(),
        Jbig2Config::text_lossless_max_compression(),
        Jbig2Config::text_lossy_max_compression(1),
        Jbig2Config::text_symbol_unify_safe(),
    ] {
        assert!(cfg.canonical_arithmetic_termination);
    }
}

#[test]
fn duplicate_stop_prefix_is_redundant_in_arithmetic_segments() {
    let required = std::env::var("JBIG2_REQUIRE_PDF_ORACLES").is_ok_and(|s| s == "1");
    assert!(!required || common::oracle::jbig2dec_path().is_some());
    let mut total_changed = 0;
    let mut poppler_before = 0;
    let mut report = String::from(
        "mode\tfixture\tremoved_bytes\tpoppler_warnings_before\tpoppler_warnings_after\n",
    );
    for (mode, mut config) in [
        ("generic", Jbig2Config::generic()),
        ("exact", Jbig2Config::text_lossless()),
    ] {
        // Explicitly reproduce the legacy padding even when a new preset
        // enables canonical termination by default.
        config.canonical_arithmetic_termination = false;
        for path in common::fixtures::ORIGINAL_FIXTURES {
            let source = Pbm::from_p4(&std::fs::read(path).unwrap()).unwrap();
            let encoded = encode_single_image_with_config(
                &source.pixels,
                source.width,
                source.height,
                Jbig2Context::with_config(config.clone(), true),
            )
            .unwrap();
            let (page, changed_page) = collapse_duplicate_stop_prefix(&encoded.page_data);
            let (globals, changed_globals) = encoded
                .global_data
                .as_deref()
                .map(collapse_duplicate_stop_prefix)
                .unwrap_or_default();
            let globals = encoded.global_data.as_ref().map(|_| globals.as_slice());
            let changed = changed_page + changed_globals;
            total_changed += changed;
            let mut canonical_config = config.clone();
            canonical_config.canonical_arithmetic_termination = true;
            let canonical = encode_single_image_with_config(
                &source.pixels,
                source.width,
                source.height,
                Jbig2Context::with_config(canonical_config.clone(), true),
            )
            .unwrap();
            assert_eq!(
                canonical.page_data, page,
                "encoder must match the independent byte transform"
            );
            assert_eq!(canonical.global_data.as_deref(), globals);
            let standalone = encode_single_image_with_config(
                &source.pixels,
                source.width,
                source.height,
                Jbig2Context::with_config(canonical_config, false),
            )
            .unwrap();
            let document = decode_file(&standalone.page_data, &DecodeOptions::default()).unwrap();
            check_bitmap(&source, document.first_page().unwrap());
            if let Some(result) = common::oracle::decode_standalone(&standalone.page_data) {
                assert_pixels_eq(&source, &result.unwrap(), "jbig2dec canonical standalone");
            }
            native(&source, encoded.global_data.as_deref(), &encoded.page_data);
            native(&source, globals, &page);
            if let Some(result) = common::oracle::decode_embedded(globals, &page) {
                assert_pixels_eq(&source, &result.unwrap(), "jbig2dec padding probe");
            }
            let old_pdf = common::pdf::wrap_page(
                source.width,
                source.height,
                encoded.global_data.as_deref(),
                &encoded.page_data,
            );
            let new_pdf = common::pdf::wrap_page(source.width, source.height, globals, &page);
            let control = common::pdf::wrap_uncompressed(&source);
            let mut warnings = (0, 0);
            for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
                if let Some((new_image, new_stderr)) =
                    common::pdf::render_with_diagnostics(backend, &new_pdf)
                {
                    let (old_image, old_stderr) =
                        common::pdf::render_with_diagnostics(backend, &old_pdf).unwrap();
                    let control_image = common::pdf::render(backend, &control).unwrap();
                    assert_pixels_eq(
                        &control_image,
                        &new_image,
                        "rendered padding probe vs source control",
                    );
                    assert_pixels_eq(
                        &old_image,
                        &new_image,
                        "rendered padding probe before/after",
                    );
                    if backend == "PDFTOPPM" {
                        warnings = (
                            old_stderr.matches("extraneous byte").count(),
                            new_stderr.matches("extraneous byte").count(),
                        );
                        poppler_before += warnings.0;
                        assert_eq!(
                            warnings.1, 0,
                            "duplicate-prefix removal must resolve this diagnostic"
                        );
                    }
                }
            }
            report.push_str(&format!(
                "{mode}\t{path}\t{changed}\t{}\t{}\n",
                warnings.0, warnings.1
            ));
        }
    }
    assert!(
        total_changed > 0,
        "fixtures must exercise a duplicate marker prefix"
    );
    if required {
        assert!(
            poppler_before > 0,
            "probe must reproduce the original warning"
        );
    }
    if let Some(path) = std::env::var_os("JBIG2_PADDING_REPORT") {
        std::fs::write(path, &report).unwrap();
    }
    eprintln!("{report}");
}
