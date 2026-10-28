# PixelKit processor GUI

This is the desktop frontend. The package is `lege-gui-pixelkit` and the binary
is `lege-gui`. Preserve mode is included. `cargo build` and `cargo gui` build
this package together with the `lege` worker.

From the workspace root:

```sh
cargo gui
cargo gui-run
```

`cargo gui-pixelkit` is the same build. The GUI uses `lege --gui-worker` beside
its executable. Its dependency graph contains PixelKit and `lege-ipc`, without
the processor or a Freya/Skia crate.

Settings, the worker supervisor, localization, and the Sanzo palettes live in
this crate. The bundled Noto Sans face is read from the vendored
`freya-main` tree at compile time.

The text raster cache is bounded to 512 entries and 2 MiB of bitmap payload.
Idle frames wait for native input or worker wakes. A clipboard thread starts
only when a clipboard shortcut is first used.

English and German builds use the same locale files:

```sh
cargo build -p lege-gui-pixelkit --features german,debug-logging
cargo test -p lege-gui-pixelkit --all-features
```

Headless snapshots use the actual renderer. Dimensions are physical pixels and
the final positional value is the display scale:

```sh
target/debug/lege-gui --snapshot /tmp/lege-preview.png 940 700 1
target/debug/lege-gui --snapshot /tmp/lege-preserve.png 1410 1050 1.5 --preserve --dark
target/debug/lege-gui --snapshot /tmp/lege-heavy.png 940 700 1 --threshold --heavy
```

The two ignored worker smoke tests require an explicit input PDF, an output
directory, and the built worker on `PATH`. They run conversion and cancellation:

```sh
PATH="$PWD/target/debug:$PATH" \
XDG_DATA_HOME=/tmp/lege-gui-smoke-data \
LEGE_GUI_SMOKE_INPUT=/absolute/path/to/test.pdf \
LEGE_GUI_SMOKE_OUTPUT=/tmp/lege-gui-smoke-output \
cargo test -p lege-gui-pixelkit real_worker_smoke -- --ignored --test-threads=1
```

See [the checkpoint report](../../../docs/PIXELKIT-GUI-PREVIEW.md) for measurements,
validation and remaining work. The active plan remains in AKR.
