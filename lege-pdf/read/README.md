# lege-pdf-read

Lege-owned document intake over the native PDF engine.

Renderer types stay private to this crate. The processor, the print crate, and the C ABI in `lege-misc/bindings/lege-render-ffi` call this API: open a document, read metadata and outlines, decide whether a page already has a text layer, and render a page or a region to a raster with a cancellation token.

```sh
cargo check -p lege-pdf-read
```

The engine underneath is [../render/README.md](../render/README.md). The public module list is the `pub use` block at the top of `src/lib.rs`.
