# lege-pdf-write

Typed, append-only, image-oriented PDF emitter.

Workers submit `PdfPageArtifact` values in whatever order they finish. The writer stores those objects immediately and, at finalization, writes the page tree, outlines, metadata, xref, and trailer. Image streams are the encoder's bytes unchanged: JPEG, JPEG 2000, JBIG2 (including globals and image masks), CCITT Group 4, and indexed color. Text is an invisible layer for selection and search.

This crate does not parse, edit, encrypt, or linearize a PDF. The closed input vocabulary is documented in [../../dev-notes/lege-pdf/write-PLAN.md](../../dev-notes/lege-pdf/write-PLAN.md).

```sh
cargo pdf-write
cargo run -p lege-pdf-write --example wrap_jpeg
```
