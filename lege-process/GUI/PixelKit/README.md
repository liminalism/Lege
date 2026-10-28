# PixelKit processor GUI preview

The first runnable migration checkpoint retains the Lege palette, two settings
cards and four main bands, with Preserve mode included. Freya remains the default
while visual and platform parity checks continue. The preview has a distinct
binary and does not replace its desktop launcher.

From the workspace root:

```sh
cargo gui-pixelkit
cargo gui-pixelkit-run
```

The build alias produces both the current worker and the new frontend. The GUI
uses `lege --gui-worker` beside its executable; its dependency graph contains
PixelKit and `lege-ipc`, without the processor, Freya or the Skia engine.

The toolkit-independent models, worker supervisor, log/settings support,
localization and Sanzo palettes are shared by source with `../Freya/src` during
the port. The bundled Noto Sans face comes from the existing vendored tree. This
avoids another copy of protocol and option rules while both frontends exist.

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
target/debug/lege-gui-pixelkit --snapshot /tmp/lege-preview.png 940 700 1
target/debug/lege-gui-pixelkit --snapshot /tmp/lege-preserve.png 1410 1050 1.5 --preserve --dark
target/debug/lege-gui-pixelkit --snapshot /tmp/lege-heavy.png 940 700 1 --threshold --heavy
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
