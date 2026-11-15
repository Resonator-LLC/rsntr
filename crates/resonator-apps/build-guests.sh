#!/usr/bin/env bash
# Rebuilds the test guests into tests/fixtures. Not needed for cargo test:
# the fixtures are checked in. Needs the wasm32-wasip2 target
# (rustup target add wasm32-wasip2) and Node.js 22.
set -euo pipefail
cd "$(dirname "$0")"

(cd tests/guests/counter-rs && cargo build --release --target wasm32-wasip2)
cp tests/guests/counter-rs/target/wasm32-wasip2/release/counter_rs.wasm tests/fixtures/
(cd tests/guests/peers-rs && cargo build --release --target wasm32-wasip2)
cp tests/guests/peers-rs/target/wasm32-wasip2/release/peers_rs.wasm tests/fixtures/

# The walkie example, for the media tests.
(cd ../../examples/wave-apps/walkie && cargo build --release --target wasm32-wasip2)
cp ../../examples/wave-apps/walkie/target/wasm32-wasip2/release/walkie.wasm tests/fixtures/

(cd tests/guests/counter-js && npm ci --no-audit --no-fund && node build.mjs)
# The JS guest carries its engine (about 12 MiB); it is kept compressed.
gzip -9 -n -c tests/guests/counter-js/counter_js.wasm > tests/fixtures/counter_js.wasm.gz
ls -l tests/fixtures
