# Lege executable size and input admission, 2026-11-08

Measured against `5b24f8e412ef8a6f9b58b015382d4d07f5d4a8f2` plus the GUI admission changes in this working tree, on Windows. MB below means 1,000,000 bytes. No archived previous executable was available in `target/release`, so this attributes the embedded payloads rather than claiming an exact before/after executable-code comparison.

## Executable size

The existing `target/release/lege.exe` is **85,720,064 bytes** (85.72 MB / 81.75 MiB). Its PE sections contain 19,703,808 bytes of `.text` and 65,426,944 bytes of `.rdata`. Searching the executable for the complete model byte sequences gave:

| Model | File bytes | Present in executable | Storage |
| --- | ---: | --- | --- |
| RF-DETR-S book layout | 57,844,802 | Yes, offset 20,085,042 | FP16 weights |
| Previous PP-DocLayout-M | 11,637,620 | No | FP16 weights |
| TileNet binarizer | 1,647,549 | Yes, offset 78,369,584 | FP32 weights |
| Existing Sauvola binarizer | 439,690 | Yes, offset 77,929,844 | Mixed constants, FP32 weights |

Replacing the previous layout payload adds **46,207,182 bytes (46.21 MB / 44.07 MiB)**. TileNet is also new relative to the parent of `8f2285d8`, adding 1.65 MB. Other changes to code and dependencies in that commit can offset some of this growth.

A protobuf-wire inspection of ONNX graph initializers found:

- RF-DETR-S: 334 FP16 tensors, 57,417,532 raw bytes = 28,708,766 FP16 elements. Only eleven FP32 scalar initializers remain (44 bytes total); integer and Boolean initializers account for another 2,376 raw bytes. ONNX graph/metadata overhead is approximately 0.43 MB.
- PP-DocLayout-M: 236 FP16 tensors, 11,540,708 raw bytes = 5,770,354 FP16 elements.

Both layout models already use half precision. The new model has about **five times as many FP16 initializer elements**. An accidentally unconverted FP32 model, an embedded copy of the old model, and debug symbols do not explain the increase. The model documentation's 58 MB description agrees with the actual file. The active RF-DETR decision already records an expected 46 MB increase.

Further material size reductions would require a separate choice: compress and decompress the embedded weights, validate lower precision storage such as INT8, use a smaller model, or distribute the weights separately. Reapplying FP16 conversion would not recover the increase.

## Why adding a PDF waited

Previously `app::probe` called `lege --probe-json` serially for every selected PDF/ZIP, accumulating the results before sending a single `Message::Inputs`. Consequently, no input tile appeared until the last file's metadata subprocess exited.

The PDF probe in `core/main.rs` reads the **entire file** into a `Vec<u8>`. `pagerender::count_pdf_pages_from_bytes` then copies that slice into an `Arc<[u8]>` and opens a full `RenderSession`. Opening builds the cross-reference structure and page-tree index, with reconstruction/recovery for damaged PDFs. It does not render pages or run layout inference, and does not initialize the layout model. This work is repeated when processing starts; the probe's document session belongs to a short-lived subprocess.

Three direct CLI measurements per document, warm-run medians:

| PDF | File MB | Pages | Probe ms |
| --- | ---: | ---: | ---: |
| beans | 0.66 | 3 | 35.0 |
| centralparktest-repair | 28.55 | 136 | 56.1 |
| chinese-medicine | 84.68 | 392 | 103.6 |
| sahib | 179.18 | 356 | 172.0 |
| Structures | 46.83 | 632 | 83.7 |
| viaduct | 2.14 | 3 | 40.2 |
| vogtle | 1.92 | 22 | 40.5 |
| xenosystems | 3.15 | 305 | 48.7 |

Those medians sum to approximately 675 ms for the eight PDFs, before GUI subprocess supervision overhead. A version-only CLI response took 3,581 ms on the first launch, then 37.0 and 35.4 ms. That demonstrates a separate startup delay without PDF parsing; its OS/loader/security cause was not profiled. The first full probe in the later GUI-path timing run took about 1.68 seconds. Do not interpret those single first-launch observations as stable cold-start benchmarks.

## Admission change and verification

The GUI now checks supported extensions, regular-file status and readability with a read of at most **1 KiB per PDF/ZIP**. It queues all admitted files and wakes the window **before starting any metadata subprocess**. Counts arrive through separate `InputMetadata` messages and update existing items in place. Old-generation results are ignored; metadata never adds another item. Missing, empty and unreadable files produce a notice. This cursory check intentionally leaves format validity to the existing PDF/ZIP readers, including their repair support.

Image-folder enumeration retains its existing behavior. Metadata probes still run serially with the existing 30-second per-file timeout, and Process/settings remain gated until the admission task finishes so page-range validation still sees available counts. Full PDF parsing and processing throughput have not been accelerated by this change.

An opt-in timing test used the actual release CLI on all ten PDFs in `D:/pdf-test` (the eight above plus two processed outputs):

- All ten input paths published in **3.287 ms**, before any metadata result.
- First 56-pixel Windows shell icon ready at **66.044 ms**.
- First page count at **1,676.943 ms**; all ten at **2,649.405 ms**.
- Counts agreed with the direct CLI probes. This measures admission and icon retrieval, not presentation latency of a native window frame.

Reproduce in PowerShell after building the CLI:

```powershell
$env:PATH = "${PWD}/target/release;$env:PATH"
$env:LEGE_GUI_PROBE_INPUT_DIR = 'D:/pdf-test'
cargo test -p lege-gui-liminalui --lib real_input_admission_latency -- --ignored --nocapture
```

`cargo test -p lege-gui-liminalui --lib`: **78 passed, 0 failed, 5 ignored**. Three regression tests exercise admission while metadata is deliberately held, incremental count updates without new tiles, metadata failure/stale results, and missing/empty/unsupported inputs. Existing file-drop and bounded-channel wakeup tests pass. Build emits the existing unused localization-field warnings.

`cargo build --release -p lege --bin lege-gui` passed (full LTO, 3m 50s) and rebuilt `target/release/lege-gui.exe` to 3,167,232 bytes. The processor `lege.exe` was not changed by the admission fix.

AKR validation passed with no errors. Its 15 warnings comprise 13 existing scopes naming moved/deleted files, the new report's not-yet-committed scope, and an uncommitted watched-path warning. Generated views and the lock were refreshed through `akr build`.

Raw baseline, wire/PE inspection script and admission capture are retained in `.agent/scratch/lege-input-latency-20261108/`.
