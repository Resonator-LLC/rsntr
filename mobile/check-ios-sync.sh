#!/usr/bin/env bash
# Fails when the checked-in Swift bindings and the packed xcframework
# disagree, which is a link-time error in the consumer's app and nothing
# earlier: the bindings name uniffi symbols the static library must
# export, and regenerating one without repacking the other ships a
# package that cannot compile. (Reported by the kontrolator iOS
# engineer, 2026-09-10: bindings regenerated on a Linux host, xcframework
# left at the previous build.)
#
#   ./mobile/check-ios-sync.sh [path/to/ResonatorFFI.xcframework]
#
# build-ios.sh runs this at the end, so a mismatch cannot ship silently.
# Run it standalone to diagnose an existing artefact. `nm` must be able
# to read the slices, so run it where they were built (macOS) or with a
# toolchain nm that understands Mach-O.
set -euo pipefail
cd "$(dirname "$0")/.."

BINDINGS=mobile/swift/resonator_ffi.swift
FRAMEWORK=${1:-mobile/dist/ResonatorFFI.xcframework}

[ -f "$BINDINGS" ] || { echo "error: $BINDINGS is missing; run mobile/build-ios.sh" >&2; exit 1; }
if [ ! -d "$FRAMEWORK" ]; then
    echo "error: no xcframework at $FRAMEWORK." >&2
    echo "       mobile/dist is not checked in; build it on a Mac with ./mobile/build-ios.sh" >&2
    exit 1
fi

# Every uniffi entry point the generated bindings call.
required=$(grep -o 'uniffi_resonator_ffi_fn_[a-z0-9_]*' "$BINDINGS" | sort -u)
[ -n "$required" ] || { echo "error: no uniffi symbols found in $BINDINGS" >&2; exit 1; }

libs=$(find "$FRAMEWORK" -name 'libresonator_ffi.a' -o -name 'libresonator_ffi.dylib' | sort)
[ -n "$libs" ] || { echo "error: no library slices inside $FRAMEWORK" >&2; exit 1; }

# The slices are Mach-O; GNU nm cannot read them, and a reader that
# returns nothing must not be mistaken for a library exporting nothing.
NM=${NM:-}
if [ -z "$NM" ]; then
    for cand in llvm-nm nm; do
        command -v "$cand" >/dev/null 2>&1 && { NM=$cand; break; }
    done
fi
[ -n "$NM" ] || { echo "error: no nm found; set NM=<path to an nm that reads Mach-O>" >&2; exit 2; }

status=0
for lib in $libs; do
    slice=$(basename "$(dirname "$lib")")
    exported=$("$NM" -gU "$lib" 2>/dev/null | grep -o 'uniffi_resonator_ffi_fn_[a-z0-9_]*' | sort -u || true)
    if [ -z "$exported" ]; then
        echo "error: $NM read no uniffi symbols from $slice." >&2
        echo "       Mach-O slices need macOS nm or llvm-nm; on Linux GNU nm reports" >&2
        echo "       'file format not recognized' and every symbol would look missing." >&2
        echo "       Run this on the Mac that builds the xcframework, or set NM=llvm-nm." >&2
        exit 2
    fi
    missing=$(comm -23 <(echo "$required") <(echo "$exported"))
    if [ -n "$missing" ]; then
        status=1
        echo "MISMATCH in slice $slice: the bindings call symbols this library does not export:" >&2
        echo "$missing" | sed 's/^/    /' >&2
    else
        echo "ok: $slice exports every symbol the bindings call ($(echo "$required" | wc -l) checked)"
    fi
done

if [ "$status" -ne 0 ]; then
    cat >&2 <<'MSG'

The Swift package will fail at link time with "undefined symbols" on exactly
those names. Fix by rebuilding both halves from one source tree, on a Mac:

    ./mobile/build-ios.sh

(It rebuilds the device and simulator slices, regenerates the bindings from
that same build, and repacks the xcframework, so the two cannot drift.)
MSG
fi
exit "$status"
