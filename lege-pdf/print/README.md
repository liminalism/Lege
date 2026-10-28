# lege-pdf-print

Office printing for the Lege PDF stack: paper, margins, scaling, N-up, booklet, duplex, copies, and page ranges.

The renderer stays a display rasterizer. This crate owns the print job.

A job takes one of two routes:

- **Pass-through** spools the original PDF bytes when nothing changes page geometry and the queue accepts `application/pdf`.
- **Composed** imposes sheets and spools bitmaps when the job uses N-up, booklet, margins, or any other geometry change. Windows always uses this route.

`lege-pdf print` is the CLI in front of this library. Prepress — CMYK separations, output ICC, overprint, PDF/X — is outside the crate. The design and the list of what shipped are in [../../dev-notes/lege-pdf/print-PLAN.md](../../dev-notes/lege-pdf/print-PLAN.md).

```sh
cargo check -p lege-pdf-print
```
