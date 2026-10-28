# lege-gpu

Internal GPU compute for Lege. Package `lege-gpu`, library `lege_gpu`.

The crate loads ONNX models, folds constants, and lowers the graph to WGSL compute shaders on wgpu. There is no ONNX Runtime or TensorRT dependency on this path. Consumers are the processor and `lege-ocr`.

| Module | Role |
|---|---|
| `vision` | ONNX runtime used for layout detection and PP-OCR |
| `binarization` | Sauvola binarization |
| `resize` | Letterbox and direct resize |
| `compute` / `wgpu_setup` | Device and pipeline setup |
| `presentation` | Optional winit presentation (`presentation` feature) |
| `android` | Android backend policy (`android` feature, Android targets only) |

wgpu backends: Vulkan on Linux, DX12 on Windows, Metal on macOS, Vulkan on Android. WGSL shaders are embedded with `include_str!`.

```sh
cargo gpu
cargo gpu-check
```

The `layout_compare` example needs `--features layout-detection`. A convolution note from earlier optimization work is in [../dev-notes/lege-gpu/CONV_OPTIMIZATION.md](../dev-notes/lege-gpu/CONV_OPTIMIZATION.md).
