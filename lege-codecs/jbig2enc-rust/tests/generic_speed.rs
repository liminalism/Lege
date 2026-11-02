//! Ignored, single-threaded generic-kernel worker for matched C/Rust runs.
//! Packed input loading and verification/output files are outside the timer.
use jbig2enc_rust::jbig2arith::Jbig2ArithCoder;
use std::{hint::black_box, path::PathBuf, time::Instant};

struct Page {
    width: usize,
    height: usize,
    words: Vec<u32>,
}

fn encode(page: &Page) -> Vec<u8> {
    let mut coder = Jbig2ArithCoder::new();
    coder
        .encode_generic_region_inner(
            &page.words,
            page.width,
            page.height,
            0,
            &[(3, -1), (-3, -1), (2, -2), (-2, -2)],
        )
        .unwrap();
    coder.flush(true);
    coder.as_bytes().to_vec()
}

#[test]
#[ignore = "matched generic-kernel performance worker; requires GENERIC_INPUT"]
fn generic_kernel_worker() {
    let bytes = std::fs::read(std::env::var_os("GENERIC_INPUT").expect("GENERIC_INPUT")).unwrap();
    assert_eq!(&bytes[..8], b"GREGION1");
    let mut cursor = 8;
    fn number(bytes: &[u8], cursor: &mut usize) -> usize {
        let value = u32::from_le_bytes(bytes[*cursor..*cursor + 4].try_into().unwrap());
        *cursor += 4;
        value as usize
    }
    let count = number(&bytes, &mut cursor);
    let mut pages = Vec::new();
    for _ in 0..count {
        let width = number(&bytes, &mut cursor);
        let height = number(&bytes, &mut cursor);
        let words = number(&bytes, &mut cursor);
        assert_eq!(words, width.div_ceil(32) * height);
        let words = (0..words)
            .map(|_| number(&bytes, &mut cursor) as u32)
            .collect();
        pages.push(Page {
            width,
            height,
            words,
        });
    }
    assert_eq!(cursor, bytes.len());
    let rounds: usize = std::env::var("GENERIC_ROUNDS")
        .unwrap_or("3".into())
        .parse()
        .unwrap();
    assert!(rounds > 0);
    for page in &pages {
        black_box(encode(page));
    }
    #[cfg(all(feature = "profiling", not(windows)))]
    let guard = std::env::var_os("GENERIC_PROFILE").map(|_| {
        pprof::ProfilerGuardBuilder::default()
            .frequency(1000)
            .blocklist(&["libc", "libgcc", "pthread", "vdso"])
            .build()
            .unwrap()
    });
    let start = Instant::now();
    let mut encoded_bytes = 0usize;
    for _ in 0..rounds {
        for page in &pages {
            let output = encode(page);
            encoded_bytes += output.len();
            black_box(&output);
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "GENERIC_RESULT\t{seconds:.9}\t{encoded_bytes}\t{rounds}\t{}",
        pages.len()
    );
    #[cfg(all(feature = "profiling", not(windows)))]
    if let Some(guard) = guard {
        let report = guard.report().build().unwrap();
        let path = PathBuf::from(std::env::var_os("GENERIC_PROFILE").unwrap());
        report
            .flamegraph(std::fs::File::create(&path).unwrap())
            .unwrap();
        std::fs::write(path.with_extension("txt"), format!("{report:?}")).unwrap();
    }
    if let Some(root) = std::env::var_os("GENERIC_OUTPUT") {
        let root = PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        for (index, page) in pages.iter().enumerate() {
            std::fs::write(root.join(format!("{index:04}.bin")), encode(page)).unwrap();
        }
    }
}
