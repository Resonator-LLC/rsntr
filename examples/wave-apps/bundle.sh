#!/usr/bin/env bash
# Builds a wave app and packs it as a bundle: <dir>/<dir>.zip holding
# manifest.json and app.wasm, ready for Insert app.
#   ./bundle.sh counter
# Rust apps need the wasm32-wasip2 target (rustup target add wasm32-wasip2),
# JavaScript apps Node.js 22.
set -euo pipefail
cd "$(dirname "$0")/$1"
if [ -f Cargo.toml ]; then
  cargo build --release --target wasm32-wasip2
  wasm=$(ls target/wasm32-wasip2/release/*.wasm | head -1)
else
  npm ci --no-audit --no-fund
  node build.mjs
  wasm=$(ls ./*.wasm | head -1)
fi
python3 - "$wasm" "$(basename "$PWD").zip" <<'PY'
import sys, zipfile
wasm, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_STORED) as z:
    z.write("manifest.json", "manifest.json")
    z.write(wasm, "app.wasm")
print(out)
PY
