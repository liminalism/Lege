mod common;

use common::pbm::{Pbm, assert_pixels_eq};
use jbig2enc_rust::decode::{DecodeOptions, decode_embedded};
use jbig2enc_rust::{Array2, Jbig2Config, PdfDictionaryPolicy, encode_document_pdf_split};

fn page(local_width: u32, shared: bool) -> Pbm {
    let (width, height) = (193, 97);
    let mut image = Pbm::new(width, height, vec![0; (width * height) as usize]);
    for index in 0..8 {
        let (left, top, w, h) = if index < 4 && shared {
            (5 + index * 21, 5, 7, 11)
        } else {
            (5 + (index % 4) * 31, 45, local_width, 17)
        };
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || y == h - 1 || (x == w - 1 && y > 4) {
                    image.pixels[((top + y) * width + left + x) as usize] = 1;
                }
            }
        }
    }
    image
}

fn verify(shared: bool) {
    if std::env::var("JBIG2_REQUIRE_PDF_ORACLES").is_ok_and(|v| v == "1") {
        assert!(
            common::oracle::jbig2dec_path().is_some(),
            "required jbig2dec oracle unavailable"
        );
    }
    let pages = [page(9, shared), page(15, shared), page(21, shared)];
    let arrays: Vec<_> = pages
        .iter()
        .map(|p| {
            Array2::from_shape_vec((p.height as usize, p.width as usize), p.pixels.clone()).unwrap()
        })
        .collect();
    for policy in [
        PdfDictionaryPolicy::AllGlobal,
        PdfDictionaryPolicy::SharedGlobal,
        PdfDictionaryPolicy::SharedGlobalWithLocal,
    ] {
        let mut cfg = Jbig2Config::text();
        cfg.auto_thresh = false;
        cfg.pdf_dictionary_policy = policy;
        cfg.pack_residuals = true;
        let split = encode_document_pdf_split(&arrays, &cfg).unwrap();
        assert_eq!(split.page_streams.len(), pages.len());
        if policy == PdfDictionaryPolicy::SharedGlobalWithLocal {
            assert!(
                split
                    .local_dict_bytes_per_page
                    .iter()
                    .all(|&bytes| bytes > 0),
                "local dictionaries must actually be emitted"
            );
        } else {
            assert!(
                split
                    .local_dict_bytes_per_page
                    .iter()
                    .all(|&bytes| bytes == 0)
            );
        }
        if !shared && policy != PdfDictionaryPolicy::AllGlobal {
            assert!(
                split.global_segments.is_none(),
                "no artificial global dictionary"
            );
        }
        for (source, stream) in pages.iter().zip(&split.page_streams) {
            let globals = split.global_segments.as_deref();
            let decoded = decode_embedded(globals, stream, &DecodeOptions::default()).unwrap();
            let pixels = (0..source.height)
                .flat_map(|y| {
                    let decoded = &decoded;
                    (0..source.width).map(move |x| u8::from(decoded.get(x, y)))
                })
                .collect();
            assert_pixels_eq(
                source,
                &Pbm::new(source.width, source.height, pixels),
                "native PDF policy",
            );
            if let Some(oracle) = common::oracle::decode_embedded(globals, stream) {
                assert_pixels_eq(source, &oracle.unwrap(), "jbig2dec PDF policy");
            }
            let pdf = common::pdf::wrap_page(source.width, source.height, globals, stream);
            for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
                if let Some(rendered) = common::pdf::render(backend, &pdf) {
                    let control =
                        common::pdf::render(backend, &common::pdf::wrap_uncompressed(source))
                            .unwrap();
                    assert_pixels_eq(&control, &rendered, backend);
                    if backend == "PDFIUM_PYTHON" {
                        assert_pixels_eq(source, &rendered, "PDFium source equality");
                    }
                }
            }
        }
    }
}

#[test]
fn pdf_shared_globals_and_local_dictionaries_preserve_odd_width_pages() {
    verify(true);
}

#[test]
fn pdf_local_dictionaries_work_without_any_globals_stream() {
    verify(false);
}

#[test]
fn pdf_renderer_generic_control() {
    let source = page(9, true);
    let encoded = jbig2enc_rust::encode_single_image_lossless(
        &source.pixels,
        source.width,
        source.height,
        true,
    )
    .unwrap();
    let pdf = common::pdf::wrap_page(
        source.width,
        source.height,
        encoded.global_data.as_deref(),
        &encoded.page_data,
    );
    for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
        if let Some(rendered) = common::pdf::render(backend, &pdf) {
            let control =
                common::pdf::render(backend, &common::pdf::wrap_uncompressed(&source)).unwrap();
            assert_pixels_eq(&control, &rendered, backend);
        }
    }
}

#[test]
fn pdf_policy_settings_preserve_standalone_behavior() {
    let pages = [page(9, true), page(15, true)];
    let mut baseline = None;
    for policy in [
        PdfDictionaryPolicy::AllGlobal,
        PdfDictionaryPolicy::SharedGlobal,
        PdfDictionaryPolicy::SharedGlobalWithLocal,
    ] {
        let mut cfg = Jbig2Config::text();
        cfg.auto_thresh = false;
        cfg.pack_residuals = true;
        cfg.pdf_dictionary_policy = policy;
        let mut encoder = jbig2enc_rust::jbig2enc::Jbig2Encoder::new(&cfg);
        for p in &pages {
            encoder
                .add_page(
                    &Array2::from_shape_vec(
                        (p.height as usize, p.width as usize),
                        p.pixels.clone(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let stream = encoder.flush().unwrap();
        if let Some(baseline) = &baseline {
            assert_eq!(baseline, &stream);
        } else {
            baseline = Some(stream.clone());
        }
        let decoded =
            jbig2enc_rust::decode::decode_file(&stream, &DecodeOptions::default()).unwrap();
        assert_eq!(decoded.pages.len(), pages.len());
        for (source, page) in pages.iter().zip(&decoded.pages) {
            for y in 0..source.height {
                for x in 0..source.width {
                    assert_eq!(source.get(x, y) != 0, page.bitmap.get(x, y));
                }
            }
        }
    }
}
