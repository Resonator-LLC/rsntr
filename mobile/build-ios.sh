#!/bin/sh
# Builds the iOS static libraries, regenerates the Swift bindings, and
# packs mobile/dist/ResonatorFFI.xcframework. Requires Xcode (xcodebuild)
# and the rust targets aarch64-apple-ios + aarch64-apple-ios-sim.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v xcodebuild >/dev/null 2>&1; then
    echo "error: xcodebuild not found; install Xcode to build the xcframework" >&2
    exit 1
fi

rustup target add aarch64-apple-ios aarch64-apple-ios-sim

# The web console server rides along so WebView shells (ios/) can call
# serveWeb(); the host bindgen build needs the same features because the
# Swift bindings are generated from that dylib.
FEATURES="${RSNTR_IOS_FEATURES:-web}"

echo "==> building staticlibs (release, features: $FEATURES)"
cargo build -p resonator-ffi --release --features "$FEATURES" --target aarch64-apple-ios
cargo build -p resonator-ffi --release --features "$FEATURES" --target aarch64-apple-ios-sim

# uniffi warns and carries on when swiftformat is missing, which reads as
# noise in a long log. Say plainly whether the bindings will be formatted,
# since a silent no-op is the class of thing this script exists to avoid.
if ! command -v swiftformat >/dev/null 2>&1; then
    echo "note: swiftformat is not installed; the generated bindings will be unformatted"
    echo "      (functionally identical; brew install swiftformat to silence this)"
fi

echo "==> regenerating the Swift bindings (host library mode)"
cargo build -p resonator-ffi --features "$FEATURES"
cargo run -q -p resonator-ffi --features "bindgen,$FEATURES" --bin uniffi-bindgen -- \
    generate --library target/debug/libresonator_ffi.dylib \
    --language swift --out-dir mobile/swift

# The xcframework wants a headers dir whose modulemap is named
# module.modulemap; the generated one keeps its module name inside.
HDR="mobile/dist/headers"
rm -rf mobile/dist/ResonatorFFI.xcframework "$HDR"
mkdir -p "$HDR"
cp mobile/swift/resonator_ffiFFI.h "$HDR/"
cp mobile/swift/resonator_ffiFFI.modulemap "$HDR/module.modulemap"

echo "==> packing the xcframework"
xcodebuild -create-xcframework \
    -library target/aarch64-apple-ios/release/libresonator_ffi.a -headers "$HDR" \
    -library target/aarch64-apple-ios-sim/release/libresonator_ffi.a -headers "$HDR" \
    -output mobile/dist/ResonatorFFI.xcframework

# The Swift package compiles the generated bindings as its source.
mkdir -p mobile/ios/Sources/Resonator
cp mobile/swift/resonator_ffi.swift mobile/ios/Sources/Resonator/

# Bindings and library must come from one build; prove it rather than
# trust it, so a mismatch never reaches a consumer as a link error.
echo "==> verifying the bindings against the packed xcframework"
./mobile/check-ios-sync.sh

echo "done: mobile/dist/ResonatorFFI.xcframework"
echo "      mobile/ios (Swift package; see mobile/README.md)"
