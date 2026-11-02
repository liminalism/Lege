# Regression fixtures

The distributed suite uses `original/blank.pbm`, `original/symbols.pbm` and
`original/marks.pbm`. These are original geometric constructions under
MIT: no external scan, font, image or codec output supplies pixels.
`tools/generate_test_fixtures.py` contains the complete rules and reproduces
their hashes, dimensions and black-pixel counts in `original/manifest.json`.

From the crate directory:

```sh
python3 tools/generate_test_fixtures.py --check
cargo test --test encode_byte_identical
cargo test --test encoder --test decode_roundtrip
```

`original_encode_hashes.txt` contains 40 stream hashes covering five existing
encoder modes in standalone/PDF form and a multipage shared-global document.
It was captured using the historical HEAD production encoder, whose 57 source
files were verified against git before running the current test harness.
The current encoder matches it without an update switch. The
[baseline record](../../docs/validation/2026-11-01/original-fixture-baseline.json)
pins that source, harness and fixture inventory. This is a synthetic regression
corpus; real scanned-book benchmark evidence remains separate.

Without the `symboldict` feature, the same test compares its 12 generic/lossless
entries, rather than pretending symbol mode ran. Full builds check all 40.
Reduced builds cannot overwrite either complete manifest. Fixture selection
does not import decoder-only helpers into the encode-only hash test.

## Retained external legacy gate

The two old inputs predate the 0.7 work. Their encoded byte-hash baselines have not
been regenerated during the component-extractor replacement.
Both PBMs are excluded from the Cargo package. Their unchanged hash manifest
and explicit test remain, so removing the scan from distribution does not
replace its regression baseline with an easier expected result.

The legacy hash test is intentionally ignored by default, with its reason
visible in test output. In a checkout containing the original inputs, run:

```sh
cargo test --test encode_byte_identical -- --ignored --exact encoder_output_is_byte_identical
JBIG2_LEGACY_FIXTURES=1 cargo test --test encoder --test decode_roundtrip
```

These explicit runs fail if the old input is unavailable. They neither download
it nor substitute the generated corpus. They passed after the separation, with
the old fixture and `encode_hashes.txt` bytes unchanged. `UPDATE_ENCODE_HASHES`
is the old manifest's manual update switch; it was not used for this work.
The new corpus has a separate `UPDATE_ORIGINAL_ENCODE_HASHES` switch and manifest.

`test_image1.pbm` is a resized copy of the French page in
[doc_coder's test/test.pbm](https://github.com/thrasibule/doc_coder/blob/ad14a6ffa20833f6c47d9f4a43690f4366f7584c/test/test.pbm).
The user identified this source and confirmed that only the image was used,
not the upstream C codec. Upstream is 1956×3186; this copy is 956×1557 and has
an IrfanView header. The exact resampling settings are unrecorded.

The upstream [license](https://github.com/thrasibule/doc_coder/blob/ad14a6ffa20833f6c47d9f4a43690f4366f7584c/license)
addresses the codec and restricts its commercial use. It supplies no separate
unrestricted permission for the scanned image. The scan is **not** represented
as Apache-2.0 by this crate's package declaration. Its redistribution rights
remain unresolved; the crate avoids distributing it rather than asserting a
new license. Identifying its download source did not establish a grant.

`test_image.pbm` is a 2×2 all-white image with an IrfanView header. Its generator
is not separately recorded. The [fixture provenance report](../../docs/audits/2026-11-01/fixture-provenance.json)
records both local hashes and the pinned upstream identity. No new upstream
image or codec source is copied into this package.
