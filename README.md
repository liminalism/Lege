# Lege

Lege prepares scanned books and documents for e-ink readers. It takes PDFs, sequential image folders, and Internet Archive JP2 collections — the usual output of a book scanner, or an Archive.org download whose pages have yellowed and whose file is hundreds of megabytes — and writes a PDF or DjVu that turns pages quickly and stays small. Layout detection can keep photographs and diagrams out of the text binarizer, so the text becomes a compact bi-level layer while pictures stay continuous-tone.

The same processing core ships as the `lege` command-line tool and the `lege-gui` desktop app. A page is rendered, optionally segmented, then encoded by region: JBIG2 or CCITT Group 4 for text, JPEG 2000 or JPEG for images, or a layered DjVu of JB2 plus IW44. OCR is optional. The default build embeds PP-OCR and runs it through Lege's own GPU runtime; Windows uses the system OCR service, and Tesseract is an opt-in legacy backend on Linux and macOS. Version 1.5.1 is the current processor release. The full guide, with the interface screenshots, build steps, and pipeline stages, is [lege-process/README.md](lege-process/README.md).

## Projects

### lege-process

The e-ink processor: library, `lege` CLI, Freya desktop GUI (`lege-gui`), and the sheet-music edition (`lege-music-gui`). This is the application the two paragraphs above describe. [lege-process/README.md](lege-process/README.md).

### lege-pdf

The PDF stack the processor and the sibling Liminaldoc tools share. `read` is the intake seam, `write` is the typed append-only emitter, `render` is the native document engine and CPU rasterizer, `print` imposes and spools office print jobs, `agent` is the structured `lege-pdf` CLI, and `pdf-integrity` is a separate forensic-triage workspace. [lege-pdf/README.md](lege-pdf/README.md).

### lege-gpu

Shared wgpu compute: Sauvola binarization, image resize, and the ONNX runtime that runs layout detection and PP-OCR. No ONNX Runtime or TensorRT dependency on the default path. [lege-gpu/README.md](lege-gpu/README.md).

### lege-ocr

The OCR library behind the processor (`lege_ocr`): line segmentation, hOCR assembly, embedded PP-OCR, Windows OCR, and an opt-in Tesseract backend. The batch product whose binary is also named `lege-ocr` lives in `lege-document-ocr`. [lege-ocr/README.md](lege-ocr/README.md).

### lege-codecs

In-tree JBIG2 (`jbig2enc-rust`), JPEG 2000 (`jp2lam`), and DjVu (`djvulibrust`). These keep their own build graphs and licenses. The root workspace patches the first two in, and the processor links the DjVu library directly. [lege-codecs/README.md](lege-codecs/README.md).

### lege-document-ocr

Business-grade batch OCR. The `lege-ocr` CLI classifies pages, keeps trustworthy native text, OCRs the rest, and writes text, Markdown, and other exports with resume support. It is a separate product from the e-ink converter. [lege-document-ocr/README.md](lege-document-ocr/README.md).

### lege-android

JNI library and Gradle host for arm64 Android 9 and later, aimed at e-ink devices. It runs the same pipeline in-process, including DjVu, and shows native progress. [lege-android/README.md](lege-android/README.md).

### lege-misc

Icons, the HTML pages the desktop app opens from About, Linux and macOS packaging notes, the `xtask` AppImage helper, the C ABI over the renderer, and the Python JBIG2 binding. [lege-misc/README.md](lege-misc/README.md).

### freya-main

Vendored fork of the Freya UI toolkit. `lege-gui` links it with the `cpu-renderer` feature (tiny-skia and softbuffer). A changeover to the sibling PixelKit toolkit is scoped for processor 1.5.2 in [to-do-plans/lege-process-1.5.2-pixelkit-gui.md](to-do-plans/lege-process-1.5.2-pixelkit-gui.md) and has not started. [freya-main/README.md](freya-main/README.md).

The native reader and writer build from the sibling `../liminaldoc` checkout. They use these PDF crates through path dependencies.

## Build

From this directory:

```sh
cargo build          # lege CLI + lege-gui
cargo cli            # CLI only
cargo gui            # CLI + Freya GUI
cargo music-sheet    # sheet-music edition, separate target dir
cargo document-ocr   # batch OCR CLI
cargo gpu
cargo ocr
cargo pdf-write
cargo lege-pdf       # agent CLI
cargo jbig2
cargo jp2
cargo djvu
```

`cargo viewer` and `cargo liminaldoc` build the sibling Liminaldoc workspace. Profiles and shared dependency versions belong in the root `Cargo.toml`.

Working notes that used to sit beside crate sources — renderer sweep history, implementation plans, benchmarks — are indexed in [dev-notes/README.md](dev-notes/README.md).

## License

The workspace and application packages are AGPL-3.0-only. See `LICENSE` and `NOTICE`. Codec crates, the vendored Freya tree, and `lege-pdf/pdf-integrity` keep the licenses in their own manifests.
