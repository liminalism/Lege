//! Fixture selection without codec or decoder dependencies.
#![allow(dead_code)]

pub const ORIGINAL_FIXTURES: [&str; 3] = [
    "tests/fixtures/original/blank.pbm",
    "tests/fixtures/original/symbols.pbm",
    "tests/fixtures/original/marks.pbm",
];

/// Explicit legacy runs fail on missing external inputs; no download/fallback.
pub fn fixture_paths() -> Vec<&'static str> {
    if std::env::var_os("JBIG2_LEGACY_FIXTURES").is_some() {
        vec![
            "tests/fixtures/test_image.pbm",
            "tests/fixtures/test_image1.pbm",
        ]
    } else {
        ORIGINAL_FIXTURES.to_vec()
    }
}

pub fn test_blank_pbm() -> &'static str {
    fixture_paths()[0]
}
pub fn test_symbols_pbm() -> &'static str {
    fixture_paths()[1]
}
