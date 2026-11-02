//! Encoding-only child for isolated process RSS. External verification happens
//! after this child exits, in tools/measure_encode_memory.py. No decode or PDF
//! rendering takes place here. File writes force all encoded bytes to be used.
use super::{common::pbm::Pbm, config};
use jbig2enc_rust::{PdfGroupEncoder, PdfGroupOptions, jbig2sym::BitImage};
use std::path::PathBuf;

pub(super) fn run() {
    let corpus = PathBuf::from(std::env::var_os("QUALITY_CORPUS").expect("corpus"));
    let directory =
        PathBuf::from(std::env::var_os("QUALITY_ARTIFACT_DIR").expect("stream directory"));
    let mode = std::env::var("QUALITY_MODE").expect("mode");
    let (base, size) = mode
        .rsplit_once("_group")
        .expect("memory mode requires explicit grouping");
    let cap = if size == "whole" {
        usize::MAX
    } else {
        size.parse().expect("page cap")
    };
    let config = config(base);
    let mut paths: Vec<_> = std::fs::read_dir(corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "pbm"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    let mut total_bytes = 0usize;
    let mut encoder = PdfGroupEncoder::new(
        &config,
        PdfGroupOptions {
            max_pages: cap,
            max_source_bitmap_bytes: None,
        },
        |group| {
            let output = directory.join(format!("group-{:05}", group.group_index));
            std::fs::create_dir_all(&output)?;
            if let Some(globals) = group.output.global_segments {
                total_bytes += globals.len();
                std::fs::write(output.join("globals.jb2"), globals)?;
            }
            for (offset, stream) in group.output.page_streams.iter().enumerate() {
                total_bytes += stream.len();
                std::fs::write(
                    output.join(format!("page-{:06}.jb2", group.first_page + offset)),
                    stream,
                )?;
            }
            println!(
                "QUALITY_MEMORY_GROUP\t{}\tfirst={}\tpages={}\tsource_bitmap_bytes={}",
                group.group_index,
                group.first_page,
                group.output.page_streams.len(),
                group.source_bitmap_bytes
            );
            Ok(())
        },
    )
    .unwrap();
    for path in &paths {
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
        encoder.push_page(image).unwrap();
    }
    encoder.finish().unwrap();
    println!(
        "QUALITY_MEMORY_RESULT\t{mode}\t{}\t{total_bytes}",
        paths.len()
    );
}
