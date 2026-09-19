#!/usr/bin/env bash
# Build (optional) and preflight the Linux TensorRT OCR worker, then run doctor.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
document_root="$(cd "$script_dir/.." && pwd)"
workspace_root="$(cd "$document_root/.." && pwd)"
turboocr_root="${TURBOOCR_ROOT:-$document_root/turboocr}"
skip_build=0
backend="${BACKEND:-auto}"

usage() {
  echo "Usage: $0 [--skip-build] [--backend auto|tensorrt-paddle]" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) skip_build=1; shift ;;
    --backend) backend="${2:-}"; shift 2 ;;
    -h|--help) usage ;;
    *) usage ;;
  esac
done

if [[ "$skip_build" -eq 0 ]]; then
  "$script_dir/build_linux_tensorrt.sh"
fi

worker="$turboocr_root/build-linux-trt-text/turboocr-text"
for required in \
  "$worker" \
  "$turboocr_root/models/det_tiny.onnx" \
  "$turboocr_root/models/rec_tiny.onnx" \
  "$turboocr_root/models/keys_tiny.txt"; do
  if [[ ! -e "$required" ]]; then
    echo "required TensorRT OCR file missing: $required" >&2
    exit 1
  fi
done

export TURBO_OCR_CUDA_GRAPHS="${TURBO_OCR_CUDA_GRAPHS:-0}"
export TRT_OPT_LEVEL="${TRT_OPT_LEVEL:-3}"

echo "Probing $worker"
(
  cd "$turboocr_root"
  "$worker" --probe \
    --det models/det_tiny.onnx \
    --rec models/rec_tiny.onnx \
    --dict models/keys_tiny.txt \
    --rec-batch 8
)

cd "$workspace_root"
cargo build --profile debug-fast -p lege-document-ocr-cli
target/debug-fast/lege-ocr doctor \
  --backend "$backend" \
  --tensorrt-ocr-root "$turboocr_root" \
  --json
