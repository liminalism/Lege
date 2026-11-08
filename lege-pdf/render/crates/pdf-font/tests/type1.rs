#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! Font Phase 5: the native Type 1 engine, through `FontProgram`.
//!
//! The fixture is synthesized by `pdf_test_support::fonts::minimal_type1`
//! (real Type 1 faces in the wild are licensed), but it is a genuine Type 1
//! program: both Adobe ciphers are applied, so this exercises eexec and
//! charstring decryption for real.

use std::sync::Arc;

use pdf_font::{FontProgram, OutlineVerb};
use pdf_test_support::fonts::{minimal_type1, type1_number, type1_subr, type1_with};

fn program() -> FontProgram {
    FontProgram::parse(Arc::from(minimal_type1())).expect("bare Type 1 parses")
}

/// Subroutine body: a 50×400 bar at (500, 100), then `return`.
fn bar_subr() -> Vec<u8> {
    [
        type1_number(500),
        type1_number(100),
        vec![21], // rmoveto
        type1_number(50),
        vec![6], // hlineto
        type1_number(400),
        vec![7], // vlineto
        type1_number(-50),
        vec![6, 9, 11], // hlineto closepath return
    ]
    .concat()
}

/// Glyph `B`: `0 600 hsbw`, `index callsubr`, then a square of its own, so it
/// draws something whether or not the subroutine exists.
fn calls_subr(index: i32) -> Vec<u8> {
    [
        type1_number(0),
        type1_number(600),
        vec![13], // hsbw
        type1_number(index),
        vec![10], // callsubr
        type1_number(100),
        type1_number(100),
        vec![21], // rmoveto
        type1_number(200),
        vec![6], // hlineto
        type1_number(200),
        vec![7], // vlineto
        type1_number(-200),
        vec![6, 9, 14], // hlineto closepath endchar
    ]
    .concat()
}

/// A font whose `/Subrs` holds `return` at 0 and the bar at `index`, with a
/// glyph `B` that calls subroutine 1000000.
fn font_with_bar_at(index: &str) -> Vec<u8> {
    let mut subrs = b"/Subrs 2 array\n".to_vec();
    subrs.extend(type1_subr("0", &[11]));
    subrs.extend(type1_subr(index, &bar_subr()));
    type1_with(&subrs, &[("B", &calls_subr(1_000_000))])
}

/// B9 regression. The `/Subrs` index came from the font and sized a dense
/// table: `dup 10000000000000000` aborted the process on the allocation (no
/// `catch_unwind` contains that), `dup 1000000000000000000` panicked with
/// "capacity overflow", and `dup 18446744073709551615` wrapped `index + 1` in
/// release builds and panicked. Each is now skipped. The font still parses,
/// `A` is untouched, and `B` draws its own square without the missing
/// subroutine: a degraded glyph, not a dead process.
#[test]
fn hostile_subr_indices_degrade_the_glyph_not_the_process() {
    for index in [
        "10000000000000000",
        "1000000000000000000",
        "18446744073709551615",
    ] {
        let p = FontProgram::parse(Arc::from(font_with_bar_at(index)))
            .unwrap_or_else(|| panic!("dup {index}: the font must still parse"));
        let a = p.gid_for_name(b"A").unwrap();
        assert_eq!(
            p.outline(a).unwrap().points,
            vec![[100.0, 50.0], [900.0, 50.0], [500.0, 650.0]],
            "dup {index}: A is untouched"
        );
        let b = p.gid_for_name(b"B").unwrap();
        let o = p.outline(b).expect("B still draws its own square");
        assert_eq!(o.verbs.len(), 5, "dup {index}: only B's square");
        assert_eq!(p.advance(b), Some(600.0));
    }
}

/// A large index a charstring can call (1000000) is stored and called: `B`
/// draws the bar from the subroutine and its own square.
#[test]
fn a_large_callable_subr_index_still_draws() {
    let p = FontProgram::parse(Arc::from(font_with_bar_at("1000000"))).expect("parses");
    let b = p.gid_for_name(b"B").unwrap();
    let o = p.outline(b).expect("draws");
    let close = OutlineVerb::Close;
    assert_eq!(
        o.verbs.iter().filter(|v| **v == close).count(),
        2,
        "the subroutine's bar and B's square"
    );
    assert_eq!(
        o.points[0],
        [500.0, 100.0],
        "the bar comes from subr 1000000"
    );
}

#[test]
fn bare_type1_is_recognized_and_routed_to_the_native_engine() {
    let p = program();
    assert!(p.is_type1(), "not an SFNT: Skrifa cannot read it");
    assert_eq!(p.units_per_em(), 1000, "from /FontMatrix");
    assert_eq!(p.num_glyphs(), 2); // .notdef + A
}

#[test]
fn charstrings_decrypt_and_draw() {
    let p = program();
    let gid = p.gid_for_name(b"A").expect("charstring name lookup");
    let o = p.outline(gid).expect("'A' has an outline");
    assert_eq!(
        o.verbs,
        vec![
            OutlineVerb::MoveTo,
            OutlineVerb::LineTo,
            OutlineVerb::LineTo,
            OutlineVerb::Close
        ]
    );
    // The exact triangle the charstring describes, in font units.
    assert_eq!(o.points, vec![[100.0, 50.0], [900.0, 50.0], [500.0, 650.0]]);
}

/// The font caches charge `retained_bytes`. It counts the parsed program as
/// well as its bytes, and it stays proportional to the font. When a
/// `/Subrs` index sized the table, `dup 1000000` in a ~1 KB font retained
/// ~24 MB while the caches charged ~1 KB.
#[test]
fn retained_bytes_counts_the_parsed_program() {
    let data = minimal_type1();
    let p = FontProgram::parse(Arc::from(data.clone())).unwrap();
    assert!(
        p.retained_bytes() > data.len(),
        "the parsed glyphs count too"
    );

    let hostile = font_with_bar_at("1000000");
    let p = FontProgram::parse(Arc::from(hostile.clone())).unwrap();
    assert!(
        p.retained_bytes() < hostile.len() + 16 * 1024,
        "{} bytes retained for a {}-byte font",
        p.retained_bytes(),
        hostile.len()
    );
}

#[test]
fn hsbw_supplies_the_advance() {
    let p = program();
    let gid = p.gid_for_name(b"A").unwrap();
    assert_eq!(p.advance(gid), Some(600.0));
}

#[test]
fn the_fonts_builtin_encoding_resolves_codes() {
    // `dup 65 /A put` in the cleartext /Encoding — this is the built-in
    // encoding a symbolic simple font relies on.
    let p = program();
    let by_code = p.gid_for_code(65).expect("code 65 is encoded");
    let by_name = p.gid_for_name(b"A").unwrap();
    assert_eq!(by_code, by_name);
    assert_eq!(p.gid_for_code(66), None, "unencoded code");
}

#[test]
fn unicode_resolves_through_glyph_names() {
    let p = program();
    assert_eq!(p.gid_for_char('A'), p.gid_for_name(b"A"));
}

#[test]
fn type1_never_claims_hinting() {
    // Font Phase 5 is unhinted by design: the caller must fall back to the
    // exact outline rather than get silently wrong geometry.
    let p = program();
    let gid = p.gid_for_name(b"A").unwrap();
    assert!(p.outlines_hinted(&[gid], 12.0).is_none());
}

#[test]
fn garbage_is_rejected_not_guessed() {
    assert!(FontProgram::parse(Arc::from(&b"not a font at all"[..])).is_none());
    // A Type 1 header with no charstrings must not produce an empty font.
    let truncated: Vec<u8> = b"%!PS-AdobeFont-1.0: X\ncurrentfile eexec\n".to_vec();
    assert!(FontProgram::parse(Arc::from(truncated)).is_none());
}
