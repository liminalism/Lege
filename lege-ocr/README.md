# lege-ocr

OCR library for the Lege processor. Package `lege-ocr`, library `lege_ocr`.

This is the slow page pipeline: line segmentation, reading order, coordinate mapping back to the page, and hOCR assembly. The processor calls `default_engine()` and does not implement an OCR backend of its own.

| Backend | When |
|---|---|
| PP-OCR (`paddle-ocr`) | Default on Linux, macOS, and Android. Embedded models, GPU execution through `lege-gpu`. |
| Windows OCR | Default on Windows. WinRT, in-process. |
| Tesseract (`tesseract-ocr`) | Opt-in on Linux and macOS. Needs the system library and language data. |
| TensorRT | Optional out-of-process worker on desktop. No Android build. |

The debug binary is `lege-ocr-debug` and needs `--features debug-bin`. It is not the batch product.

The batch CLI whose executable is named `lege-ocr` is a different package, `lege-document-ocr-cli`, documented in [../lege-document-ocr/README.md](../lege-document-ocr/README.md). That product depends on this library.

```sh
cargo ocr
cargo ocr-check
cargo run -p lege-ocr --bin lege-ocr-debug --features debug-bin
```
