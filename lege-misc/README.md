# lege-misc

Shared assets and tools that are not a processing crate.

| Path | What it is |
|---|---|
| `assets/` | Application icons used by packaging. Logo explorations under `logo-concepts/` and `logo-export/` are local and gitignored. |
| `docs/documentation.html` | The Readme page `lege-gui` embeds and opens from About. |
| `docs/licenses.html` | The third-party notices page opened from About. |
| `docs/documentation-sheetmusic.html` | The sheet-music edition's About page. |
| `docs/linux-packaging.md` | `.deb` and AppImage flow. |
| `xtask/` | `cargo appimage`. |
| `packaging/macos/` | macOS icon and bundled libraries used by the Mac packaging scripts. |
| `bindings/lege-render-ffi/` | C ABI over `lege-pdf-read`. AGPL-3.0-only, same as the renderer it links. [bindings/lege-render-ffi/README.md](bindings/lege-render-ffi/README.md). |
| `bindings/jbig2enc-py/` | PyO3 module, built with maturin. Excluded from the Cargo workspace. [bindings/jbig2enc-py/README.md](bindings/jbig2enc-py/README.md). |
| `dev-misc/` | Local build and release scripts. |
| `bin/`, `examples/` | One-off binarization and encoding experiments. |

```sh
cargo xtask-help
cargo build -p lege-render-ffi
```
