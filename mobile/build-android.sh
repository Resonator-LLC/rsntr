#!/bin/sh
# Builds libresonator_ffi.so for aarch64-linux-android and regenerates
# the Kotlin bindings. Requires an Android NDK: set ANDROID_NDK_HOME, or
# install one under $ANDROID_HOME/ndk (Android Studio: SDK Manager ->
# SDK Tools -> NDK (Side by side)).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

API="${ANDROID_API:-24}"

# Locate the NDK: $ANDROID_NDK_HOME, else the newest under the SDK.
NDK="${ANDROID_NDK_HOME:-}"
if [ -z "$NDK" ]; then
    for sdk in "${ANDROID_HOME:-}" "$HOME/Library/Android/sdk" "$HOME/Android/Sdk"; do
        [ -n "$sdk" ] && [ -d "$sdk/ndk" ] || continue
        NDK="$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1)"
        [ -n "$NDK" ] && break
    done
fi
if [ -z "$NDK" ] || [ ! -d "$NDK" ]; then
    echo "error: no Android NDK found; set ANDROID_NDK_HOME" >&2
    exit 1
fi

HOST_TAG="$(uname -s | tr '[:upper:]' '[:lower:]')-x86_64"
[ "$(uname -s)" = "Darwin" ] && HOST_TAG="darwin-x86_64" # NDK ships fat binaries under darwin-x86_64
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin"
if [ ! -x "$TOOLCHAIN/aarch64-linux-android$API-clang" ]; then
    echo "error: $TOOLCHAIN/aarch64-linux-android$API-clang not found" >&2
    exit 1
fi

rustup target add aarch64-linux-android

export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$TOOLCHAIN/aarch64-linux-android$API-clang"
export CC_aarch64_linux_android="$TOOLCHAIN/aarch64-linux-android$API-clang"
export AR_aarch64_linux_android="$TOOLCHAIN/llvm-ar"

echo "==> building libresonator_ffi.so (release, api $API)"
cargo build -p resonator-ffi --release --target aarch64-linux-android

echo "==> regenerating the Kotlin bindings (host library mode)"
cargo build -p resonator-ffi
cargo run -q -p resonator-ffi --features bindgen --bin uniffi-bindgen -- \
    generate --library target/debug/libresonator_ffi.dylib \
    --language kotlin --out-dir mobile/kotlin

JNI="mobile/android/src/main/jniLibs/arm64-v8a"
mkdir -p "$JNI"
cp target/aarch64-linux-android/release/libresonator_ffi.so "$JNI/"

echo "done: $JNI/libresonator_ffi.so"
echo "      mobile/kotlin (bindings; the gradle module sources them directly)"
