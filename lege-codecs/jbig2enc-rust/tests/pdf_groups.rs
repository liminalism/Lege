mod common;
use jbig2enc_rust::decode::{DecodeOptions, decode_embedded, decode_globals};
use jbig2enc_rust::jbig2enc::Jbig2Encoder;
use jbig2enc_rust::jbig2sym::BitImage;
use jbig2enc_rust::{DecoderResourceLimits, Jbig2Config, PdfGroupEncoder, PdfGroupOptions};

fn page(index: usize) -> BitImage {
    let mut image = BitImage::new(129, 65).unwrap();
    for y in 3..60 {
        for x in 2..125 {
            if (x % 13 == 0 || y % 13 == 0) && (x / 13 + index) % 3 != 0 {
                image.set_usize(x, y, true);
            }
        }
    }
    image.set_usize(index, 64, true);
    image
}

#[test]
fn bounded_groups_have_explicit_globals_associations_and_exact_pages() {
    let cfg = Jbig2Config::text_lossless();
    let mut groups = Vec::new();
    let mut encoder = PdfGroupEncoder::new(
        &cfg,
        PdfGroupOptions {
            max_pages: 3,
            max_source_bitmap_bytes: Some(2 * 5 * 4 * 65),
        },
        |group| {
            groups.push(group);
            Ok(())
        },
    )
    .unwrap();
    for index in 0..7 {
        encoder.push_page(page(index)).unwrap();
    }
    encoder.finish().unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|g| g.output.page_streams.len())
            .collect::<Vec<_>>(),
        [2, 2, 2, 1]
    );
    let options = DecodeOptions::default();
    for (group_index, group) in groups.iter().enumerate() {
        assert_eq!(group.group_index, group_index);
        assert_eq!(group.first_page, group_index * 2);
        assert!(group.source_bitmap_bytes <= 2 * 5 * 4 * 65);
        let globals = group.output.global_segments.as_deref();
        let actual_storage = globals
            .map(|bytes| decode_globals(bytes, &options).unwrap().retained_bytes())
            .unwrap_or(0);
        assert_eq!(
            actual_storage,
            group.metrics.resources.global_dictionary_bitmap_bytes
        );
        for (offset, stream) in group.output.page_streams.iter().enumerate() {
            let source = page(group.first_page + offset);
            let decoded = decode_embedded(globals, stream, &options).unwrap();
            for y in 0..65 {
                for x in 0..129 {
                    assert_eq!(source.get_usize(x, y), decoded.get(x as u32, y as u32));
                }
            }
            if let Some(oracle) = common::oracle::decode_embedded(globals, stream) {
                let oracle = oracle.unwrap();
                for y in 0..65 {
                    for x in 0..129 {
                        assert_eq!(source.get_usize(x, y), oracle.get(x as u32, y as u32) != 0);
                    }
                }
            }
            let expected = common::pbm::Pbm::new(
                129,
                65,
                (0..65)
                    .flat_map(|y| {
                        let source = &source;
                        (0..129).map(move |x| u8::from(source.get_usize(x, y)))
                    })
                    .collect(),
            );
            let encoded_pdf = common::pdf::wrap_page(129, 65, globals, stream);
            let control_pdf = common::pdf::wrap_uncompressed(&expected);
            for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
                if let Some(encoded) = common::pdf::render(backend, &encoded_pdf) {
                    let control = common::pdf::render(backend, &control_pdf).unwrap();
                    common::pbm::assert_pixels_eq(&control, &encoded, backend);
                }
            }
        }
    }
}

#[test]
fn constraints_choose_an_exact_feasible_candidate_or_return_error() {
    let mut cfg = Jbig2Config::text_lossless_max_compression();
    cfg.decoder_resource_limits = DecoderResourceLimits {
        max_encoded_global_bytes: Some(0),
        max_global_bitmap_bytes: Some(0),
        max_page_dictionary_bitmap_bytes: Some(0),
        max_cold_page_work_pixels: None,
    };
    let mut encoder = Jbig2Encoder::new(&cfg);
    encoder.add_page_bitimage(page(0)).unwrap();
    encoder.add_page_bitimage(page(1)).unwrap();
    let split = encoder.flush_pdf_split().unwrap();
    assert!(split.global_segments.is_none());
    assert_eq!(split.local_dict_bytes_per_page, [0, 0]);
    let metrics = encoder.metrics_snapshot();
    cfg.decoder_resource_limits
        .check(&metrics.resources)
        .unwrap();
    assert!(
        metrics
            .optimization
            .trials
            .iter()
            .any(|trial| !trial.meets_resource_limits)
    );
    assert!(
        metrics
            .resources
            .pages
            .iter()
            .all(|page| page.exact_instances == 0 && page.substituted_instances == 0)
    );
    cfg.decoder_resource_limits.max_cold_page_work_pixels = Some(0);
    let mut encoder = Jbig2Encoder::new(&cfg);
    encoder.add_page_bitimage(page(0)).unwrap();
    assert!(
        encoder
            .flush_pdf_split()
            .unwrap_err()
            .to_string()
            .contains("cold decode work")
    );
}

#[test]
fn group_error_contract_and_empty_input() {
    let cfg = Jbig2Config::text_lossless();
    let mut calls = 0;
    PdfGroupEncoder::new(
        &cfg,
        PdfGroupOptions {
            max_pages: 2,
            max_source_bitmap_bytes: None,
        },
        |_| {
            calls += 1;
            Ok(())
        },
    )
    .unwrap()
    .finish()
    .unwrap();
    assert_eq!(calls, 0);
    let mut encoder = PdfGroupEncoder::new(
        &cfg,
        PdfGroupOptions {
            max_pages: 1,
            max_source_bitmap_bytes: None,
        },
        |_| anyhow::bail!("sink failure"),
    )
    .unwrap();
    assert!(
        encoder
            .push_page(page(0))
            .unwrap_err()
            .to_string()
            .contains("sink failure")
    );
    assert!(
        encoder
            .push_page(page(1))
            .unwrap_err()
            .to_string()
            .contains("failed state")
    );
    assert!(encoder.finish().is_err());
    let mut groups = 0;
    let mut encoder = PdfGroupEncoder::new(
        &cfg,
        PdfGroupOptions {
            max_pages: 2,
            max_source_bitmap_bytes: Some(1300),
        },
        |_| {
            groups += 1;
            Ok(())
        },
    )
    .unwrap();
    encoder.push_page(page(0)).unwrap();
    assert!(encoder.push_page(BitImage::new(320, 320).unwrap()).is_err());
    encoder.finish().unwrap();
    assert_eq!(groups, 1);
}
