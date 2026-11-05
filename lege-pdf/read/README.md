# lege-pdf-read

Lege-owned document intake over the native PDF engine.

Renderer types stay private to this crate. The processor, the print crate, and the C ABI in `lege-misc/bindings/lege-render-ffi` call this API: open a document, read metadata and outlines, decide whether a page already has a text layer, and render a page or a region to a raster with a cancellation token.

Text helpers and `RenderSession::compile` share a single interpretation that
retains both semantic and raster artifacts. Concurrent requests for a page
share the compilation, while unrelated pages can compile in parallel.
Sessions retain the eight most recently used pages by default. Use
`open_with_cache_capacity` to choose a page-count bound (zero disables
retention), and `clear_page_cache` to release retained artifacts without
invalidating caller-held page handles. This bounds retained page count,
not bytes or the separate parser and renderer resource caches.

`compile_cancellable` checks cancellation before cache access, while waiting
for another caller and during interpretation. Cancelled or failed compilations
are not retained. Cancelling a waiting caller leaves the owner's work intact;
clearing the cache also prevents existing in-flight work from refilling it.

```sh
cargo check -p lege-pdf-read
```

The engine underneath is [../render/README.md](../render/README.md). The public module list is the `pub use` block at the top of `src/lib.rs`.
