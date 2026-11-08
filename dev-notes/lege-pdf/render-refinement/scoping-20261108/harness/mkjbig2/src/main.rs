//! mkjbig2 <outdir> <page.pbm>... : PDF-embeddable JBIG2 (symbol mode, shared globals).
use jbig2enc_rust::Array2;
use jbig2enc_rust::Jbig2Config;
use std::io::{BufRead, BufReader, Read};

fn read_pbm(path: &str) -> Array2<u8> {
    let mut r = BufReader::new(std::fs::File::open(path).expect("open pbm"));
    let mut header = Vec::new();
    // P4\n<w> <h>\n (Pillow writes no comments)
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "P4");
    loop {
        line.clear();
        r.read_line(&mut line).unwrap();
        if !line.starts_with('#') && !line.trim().is_empty() {
            break;
        }
    }
    let mut it = line.split_whitespace();
    let w: usize = it.next().unwrap().parse().unwrap();
    let h: usize = it.next().unwrap().parse().unwrap();
    r.read_to_end(&mut header).unwrap();
    let stride = w.div_ceil(8);
    assert!(header.len() >= stride * h);
    Array2::from_shape_fn((h, w), |(y, x)| (header[y * stride + x / 8] >> (7 - (x % 8))) & 1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = &args[0];
    let images: Vec<Array2<u8>> = args[1..].iter().map(|p| read_pbm(p)).collect();
    let config = Jbig2Config { dpi: 300, symbol_mode: true, ..Jbig2Config::default() };
    let t = std::time::Instant::now();
    let split = jbig2enc_rust::encode_document_pdf_split(&images, &config).expect("encode");
    eprintln!("encoded {} pages in {:?}", images.len(), t.elapsed());
    std::fs::create_dir_all(out).unwrap();
    if let Some(g) = &split.global_segments {
        std::fs::write(format!("{out}/globals.jb2"), g).unwrap();
        eprintln!("globals: {} bytes", g.len());
    }
    for (i, p) in split.page_streams.iter().enumerate() {
        std::fs::write(format!("{out}/page-{i:02}.jb2"), p).unwrap();
        eprintln!("page {i}: {} bytes", p.len());
    }
}
