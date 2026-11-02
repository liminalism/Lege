//! Streaming corpus worker: whole-book and bounded runs use the same loading
//! and verification strategy, retaining only one source page outside the codec.
use super::*;
use jbig2enc_rust::jbig2sym::BitImage;
use jbig2enc_rust::{DocumentResourceMetrics, PdfGroupEncoder, PdfGroupOptions};
use std::time::Duration;

pub(super) fn print_resources(first: usize, resources: &DocumentResourceMetrics) {
    println!(
        "QUALITY_RESOURCES\tglobals_encoded={}\tglobals_bitmap={}\tglobals_pixels={}",
        resources.encoded_global_bytes,
        resources.global_dictionary_bitmap_bytes,
        resources.global_dictionary_pixels
    );
    for (offset, page) in resources.pages.iter().enumerate() {
        println!(
            "QUALITY_PAGE_RESOURCES\t{}\tpage_pixels={}\tlocal_bitmap={}\texact={}\trefined={}\tsubstituted={}\tgeneric_instances={}\tgeneric_region_pixels={}\tgeneric_ink={}\tcold_work={}",
            first + offset,
            page.page_pixels,
            page.local_dictionary_bitmap_bytes,
            page.exact_instances,
            page.refined_instances,
            page.substituted_instances,
            page.generic_instances,
            page.generic_region_pixels,
            page.generic_source_ink_pixels,
            page.cold_work_pixels
        );
    }
}

pub(super) fn run(
    paths: &[PathBuf],
    mode: &str,
    base: &str,
    cap: usize,
    render_pdf: bool,
    require_oracle: bool,
) {
    let cfg = config(base);
    let canonical_termination = cfg.canonical_arithmetic_termination;
    let oracle_available = common::oracle::jbig2dec_path().is_some();
    assert!(!require_oracle || oracle_available);
    let (mut bytes, mut global_bytes, mut local_bytes, mut residual_bytes) =
        (0usize, 0usize, 0usize, 0usize);
    let (mut differences_total, mut oracle_total) = (0u64, 0u64);
    let (mut cold_seconds, mut warm_seconds) = (0f64, 0f64);
    let (mut dictionary_memory, mut comparisons, mut exact, mut refined) =
        (0usize, 0usize, 0usize, 0usize);
    let mut verification = Duration::ZERO;
    let mut encode_duration = Duration::ZERO;
    let mut encoder = PdfGroupEncoder::new(&cfg, PdfGroupOptions { max_pages: cap,
        max_source_bitmap_bytes: None }, |group| {
        let start_verification = Instant::now();
        let split = &group.output;
        let globals = split.global_segments.as_deref();
        global_bytes += globals.map_or(0, <[u8]>::len);
        bytes += globals.map_or(0, <[u8]>::len) + split.page_streams.iter().map(Vec::len).sum::<usize>();
        local_bytes += split.local_dict_bytes_per_page.iter().sum::<usize>();
        residual_bytes += split.generic_region_bytes_per_page.iter().sum::<usize>();
        comparisons += group.metrics.symbol_stats.comparator_calls;
        exact += group.metrics.symbol_stats.exact_hits;
        refined += group.metrics.symbol_stats.refined_hits;
        print_resources(group.first_page, &group.metrics.resources);
        println!("QUALITY_MATCHING\tcomponents={}\tingest_comparisons={}\tplanner_comparisons={}\tlegacy_post_ingest_comparisons=unreported",
            group.metrics.symbol_stats.input_components, group.metrics.symbol_stats.comparator_calls,
            group.metrics.optimization.candidate_comparisons);
        println!("QUALITY_GROUP\t{}\tfirst={}\tpages={}\tsource_bitmap_bytes={}",
            group.group_index, group.first_page, split.page_streams.len(), group.source_bitmap_bytes);
        let options = DecodeOptions::default();
        let decoded_globals = globals.map(|bytes| decode_globals(bytes, &options).unwrap());
        let actual_memory = decoded_globals.as_ref().map_or(0, |g| g.retained_bytes());
        assert_eq!(actual_memory, group.metrics.resources.global_dictionary_bitmap_bytes);
        dictionary_memory = dictionary_memory.max(actual_memory);
        let mut context = DecoderContext::new();
        for (offset, stream) in split.page_streams.iter().enumerate() {
            let index = group.first_page + offset;
            let source = Pbm::from_p4(&std::fs::read(&paths[index]).unwrap()).unwrap();
            let start = Instant::now();
            let cold = decode_embedded(globals, stream, &options).unwrap();
            let cold_s = start.elapsed().as_secs_f64();
            let difference = differences(&source, &cold);
            let warm = decode_embedded_with_globals(decoded_globals.as_ref(), stream, &options, &mut context).unwrap();
            assert_eq!(cold, warm);
            let start = Instant::now();
            let warm = decode_embedded_with_globals(decoded_globals.as_ref(), stream, &options, &mut context).unwrap();
            let warm_s = start.elapsed().as_secs_f64();
            assert_eq!(cold, warm);
            let decoded = Pbm::new(source.width, source.height,
                (0..source.height).flat_map(|y| { let cold = &cold;
                    (0..source.width).map(move |x| u8::from(cold.get(x,y))) }).collect());
            let oracle_diff = if oracle_available {
                let oracle = common::oracle::decode_embedded(globals, stream).unwrap().unwrap();
                common::pbm::assert_pixels_eq(&decoded, &oracle, "group native/jbig2dec");
                let diff = oracle_differences(&source,&oracle);
                oracle_total += diff;
                diff.to_string()
            } else { "unavailable".into() };
            if render_pdf {
                let pdf = common::pdf::wrap_page(source.width,source.height,globals,stream);
                let control = common::pdf::wrap_uncompressed(&decoded);
                for backend in ["PDFTOPPM", "PDFIUM_PYTHON"] {
                    let (rendered,diagnostics) = common::pdf::render_with_diagnostics(backend,&pdf).expect("required renderer");
                    let trailing_warnings = diagnostics.matches("extraneous byte").count();
                    if canonical_termination { assert_eq!(trailing_warnings,0,"canonical grouped {backend} stream has trailing arithmetic bytes"); }
                    println!("QUALITY_PDF_DIAGNOSTICS\t{index}\t{backend}\ttrailing_bytes={trailing_warnings}");
                    let baseline = common::pdf::render(backend,&control).expect("required control");
                    common::pbm::assert_pixels_eq(&baseline,&rendered,backend);
                    if backend == "PDFIUM_PYTHON" { common::pbm::assert_pixels_eq(&decoded,&rendered,backend); }
                    println!("QUALITY_PDF\t{index}\t{backend}\tpass");
                }
            }
            println!("QUALITY_PAGE\t{index}\t{}\t{difference}\t{cold_s:.9}\t{warm_s:.9}\t{oracle_diff}",stream.len());
            differences_total += difference;
            cold_seconds += cold_s;
            warm_seconds += warm_s;
        }
        verification += start_verification.elapsed();
        Ok(())
    }).unwrap();
    for path in paths {
        // Loading and conversion are outside encode time in both bounded and
        // whole-book cases. RSS still includes the source page and verification.
        let source = Pbm::from_p4(&std::fs::read(path).unwrap()).unwrap();
        let mut image = BitImage::new(source.width, source.height).unwrap();
        for y in 0..source.height {
            for x in 0..source.width {
                if source.get(x, y) != 0 {
                    image.set_usize(x as usize, y as usize, true);
                }
            }
        }
        drop(source);
        let start = Instant::now();
        encoder.push_page(image).unwrap();
        encode_duration += start.elapsed();
    }
    let start = Instant::now();
    encoder.finish().unwrap();
    encode_duration += start.elapsed();
    let encode_seconds = encode_duration
        .checked_sub(verification)
        .expect("verification occurs inside push/finish")
        .as_secs_f64();
    if cfg.is_lossless {
        assert_eq!(differences_total, 0, "grouped lossless contract");
    }
    println!(
        "QUALITY_RESULT\t{mode}\t{}\t{}\t{bytes}\t{global_bytes}\t{local_bytes}\t{residual_bytes}\t{encode_seconds:.9}\t{differences_total}\t{cold_seconds:.9}\t{warm_seconds:.9}\t{dictionary_memory}\t{comparisons}\t{exact}\t{refined}\t{}",
        paths.len(),
        cfg.auto_thresh,
        if oracle_available {
            oracle_total.to_string()
        } else {
            "unavailable".into()
        }
    );
}
