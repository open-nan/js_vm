#!/usr/bin/env sh
set -eu

ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
WASM_OPT="${WASM_OPT:-}"

if [ -z "$WASM_OPT" ]; then
  WASM_OPT="$(command -v wasm-opt || true)"
fi

if [ -z "$WASM_OPT" ]; then
  WASM_OPT="$HOME/Library/Caches/.wasm-pack/wasm-opt-50385c9e73ccee70/bin/wasm-opt"
fi

cd "$ROOT_DIR"

rm -rf pkg/compiler pkg/executor

wasm-pack build crates/compiler --target web --out-dir ../../pkg/compiler --release
wasm-pack build crates/runtime --target web --out-dir ../../pkg/executor --release

if [ -x "$WASM_OPT" ]; then
  "$WASM_OPT" \
    pkg/compiler/js_vm_compiler_bg.wasm \
    -Oz \
    --enable-bulk-memory \
    --enable-nontrapping-float-to-int \
    -o pkg/compiler/js_vm_compiler_bg.wasm
  "$WASM_OPT" \
    pkg/executor/js_vm_runtime_bg.wasm \
    -Oz \
    --enable-bulk-memory \
    --enable-nontrapping-float-to-int \
    -o pkg/executor/js_vm_runtime_bg.wasm
else
  printf '%s\n' "warning: wasm-opt not found; wasm output was built but not post-optimized" >&2
fi

wc -c \
  pkg/compiler/js_vm_compiler_bg.wasm \
  pkg/compiler/js_vm_compiler.js \
  pkg/executor/js_vm_runtime_bg.wasm \
  pkg/executor/js_vm_runtime.js
