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
| `pdf-titler/` | `lege-titler`: OCRs page 1 of every PDF in a folder (lege-document-ocr Thorough + layout), guesses title (large text, top half) and author (smaller name-like text, bottom half, or a "by" line), and appends them to each file's Info dictionary and XMP as an incremental update. Fills empty fields only unless `--overwrite`; `--dry-run` and `--verbose` show the guesses. `cargo titler-run -- <folder>`. |
| `packaging/macos/` | macOS icon and bundled libraries used by the Mac packaging scripts. |
| `bindings/lege-render-ffi/` | C ABI over the `exprim` document API. AGPL-3.0-only as part of Lege (exprim itself is MIT). [bindings/lege-render-ffi/README.md](bindings/lege-render-ffi/README.md). |
| `dev-misc/` | Local build and release scripts. |
| `bin/`, `examples/` | One-off binarization and encoding experiments. |
| `liminal-annotator/` | Box review and dataset export for the layout-detection model (LiminalKit GUI, SQLite). Its own Cargo workspace. [liminal-annotator/README.md](liminal-annotator/README.md). |

```sh
cargo xtask-help
cargo build -p lege-render-ffi
```
