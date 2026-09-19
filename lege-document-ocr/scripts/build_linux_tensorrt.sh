#!/usr/bin/env bash
# Build the lean TurboOCR TensorRT text worker for Linux.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
document_root="$(cd "$script_dir/.." && pwd)"
turboocr_root="${TURBOOCR_ROOT:-$document_root/turboocr}"
build_dir="${BUILD_DIR:-$turboocr_root/build-linux-trt-text}"
arch="${CMAKE_CUDA_ARCHITECTURES:-89}"

resolve_nvcc() {
  local candidate
  for candidate in \
    "${CUDACXX:-}" \
    "${CUDA_PATH:+$CUDA_PATH/bin/nvcc}" \
    /usr/local/cuda/bin/nvcc \
    "${CUDA_HOME:+$CUDA_HOME/bin/nvcc}"; do
    if [[ -n "$candidate" && -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

nvcc="$(resolve_nvcc || true)"
cuda_root="$(cd "$(dirname "${nvcc:-/usr/local/cuda/bin/nvcc}")/.." && pwd)"

if [[ ! -d "$turboocr_root" ]]; then
  echo "TurboOCR root not found: $turboocr_root" >&2
  exit 1
fi
if [[ ! -x "$nvcc" ]]; then
  echo "nvcc not found at $nvcc; set CUDA_PATH or CUDACXX" >&2
  exit 1
fi

export PATH="$(dirname "$nvcc"):${PATH:-}"
export CUDACXX="$nvcc"

cmake -S "$turboocr_root" -B "$build_dir" \
  -DTURBO_TRT_RUNTIME_ONLY=ON \
  -DTURBO_TRT_TEXT_PIPELINE=ON \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_CUDA_ARCHITECTURES="$arch" \
  -DCMAKE_CUDA_COMPILER="$nvcc"

cmake --build "$build_dir" --target turboocr-text -j"$(nproc)"

worker="$build_dir/turboocr-text"
if [[ ! -x "$worker" ]]; then
  echo "expected worker was not produced: $worker" >&2
  exit 1
fi
echo "Built $worker"
