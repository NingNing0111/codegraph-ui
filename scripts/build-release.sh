#!/usr/bin/env bash
set -euo pipefail

TARGETS=(
  "aarch64-apple-darwin"
  "x86_64-apple-darwin"
)

if [[ "${BUILD_WINDOWS:-}" == "1" ]]; then
  TARGETS+=("x86_64-pc-windows-msvc")
fi

for target in "${TARGETS[@]}"; do
  rustup target add "$target"
  cargo build --release --target "$target"
done
