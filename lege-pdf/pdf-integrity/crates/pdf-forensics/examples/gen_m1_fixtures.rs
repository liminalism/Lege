//! Reproducible generator for the M1 fixture files.
//!
//! Most M1 fixtures live only as `pdf-test-support` builders, constructed in
//! memory by `tests/m1_fixtures.rs`. The one exception is the PDF-Verse-style
//! export, which also exists as a committed file so the CLI can be pointed at
//! it directly:
//!
//! ```sh
//! cargo run -p pdf-forensics --example gen_m1_fixtures
//! cargo run -p lege-pdf-audit -- inspect fixtures/pdfverse-export.pdf --json -
//! ```
//!
//! The generator writes `fixtures/pdfverse-export.pdf`: three full-page-raster
//! pages and no fonts. The M1 suite pins those bytes against the builder
//! output, so regenerating must be a no-op unless the builder changed on
//! purpose.

use std::path::PathBuf;

/// Pages in the committed export. Mirrored by `PDFVERSE_PAGES` in
/// `tests/m1_fixtures.rs`; the byte-equality check there keeps the two in
/// step.
const PDFVERSE_PAGES: u32 = 3;

fn main() -> std::io::Result<()> {
    let dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "fixtures"]
        .iter()
        .collect();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("pdfverse-export.pdf");
    let bytes = pdf_test_support::builder::raster_scan(PDFVERSE_PAGES, false);
    std::fs::write(&path, bytes)?;
    println!("wrote {}", path.display());
    Ok(())
}
