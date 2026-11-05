# lege-pdf-print

Office printing for the Lege PDF stack: paper, margins, scaling, N-up, booklet, duplex, copies, and page ranges.

The renderer stays a display rasterizer. This crate owns the print job.

Windows spooling is **experimental**: it is compile-tested, but has not yet
been verified against a real printer queue. Failed jobs are aborted rather
than committed as partial documents.

Windows uses one sheet orientation for each job. In auto mode it considers all
selected pages before choosing that orientation; pages can still rotate inside
their cells. Hardware margins are queried for the requested paper in the actual
orientation. Driver configuration failures or mismatched physical page sizes
return an error before a document is started.

Booklets fold along the vertical centre of the imposed sheet. Automatic
duplex must use short-edge binding on landscape sheets or long-edge binding
on portrait sheets; an incompatible binding returns an error before
submission. Simplex remains available for manual duplex.

A job takes one of two routes:

- **Pass-through** spools the original PDF bytes when nothing changes page geometry and the queue accepts `application/pdf`.
- **Composed** imposes sheets and spools bitmaps when the job uses N-up, booklet, margins, or any other geometry change. Windows always uses this route.

Source page compilations share the read session's bounded cache across sheets,
previews and copies. Spooler output still allocates a full sheet raster;
streaming bands to the backends is a remaining publication task.

For previews without a printer, create a `PrintJob` from an open read session,
call `impose(&job, Margins::ZERO)`, and pass the sheets to
`preview_sheets(&session, &sheets, PreviewOptions::default())`. The iterator
renders and encodes one PNG per requested item. It preserves sheet order and
reports each sheet's error separately.

To preview the actual device plan, obtain its capabilities and call
`Spooler::plan_sheets(&printer, &job, &capabilities)`, then pass those sheets
to the same iterator. This queries geometry without submitting a print job.

`lege-pdf print` is the CLI in front of this library. Prepress — CMYK separations, output ICC, overprint, PDF/X — is outside the crate. The design and the list of what shipped are in [../../dev-notes/lege-pdf/print-PLAN.md](../../dev-notes/lege-pdf/print-PLAN.md).

```sh
cargo check -p lege-pdf-print
```
