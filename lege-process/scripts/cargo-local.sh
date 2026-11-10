#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

for codec in jp2lam jbig2enc-rust; do
  if [[ ! -f "$ROOT/../exprim/codecs/$codec/Cargo.toml" ]]; then
    echo "Missing codec checkout: $ROOT/../exprim/codecs/$codec" >&2
    exit 1
  fi
done

cd "$ROOT"

# The ecosystem workspace patches the historical Git dependency coordinates to
# ../exprim/codecs/{jp2lam,jbig2enc-rust} in its root Cargo.toml, so a normal Cargo
# invocation already uses the local codec sources.
cargo --offline "$@"
