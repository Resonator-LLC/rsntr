#!/bin/sh
# Builds the Android app: the web client, the node library
# (crates/resonator-ffi) for arm64 and x86_64 phones and emulators, and the
# APK.
#
#   apps/android/build.sh            # debug APK
#   apps/android/build.sh release    # release APK and AAB (signed when a
#                                    # keystore is configured, see README)
#
# Needs the Android SDK (ANDROID_HOME) with NDK 28.2.13676358, JDK 17+,
# cargo-ndk and the Rust targets aarch64-linux-android and
# x86_64-linux-android.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
what=${1:-debug}

[ -f "$root/client/dist/index.html" ] || (cd "$root/client" && npm ci && npm run build)

export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-${ANDROID_HOME:?set ANDROID_HOME}/ndk/28.2.13676358}"
profile=--release
[ "$what" = debug ] && [ -z "${RESONATOR_RELEASE_LIB:-}" ] && profile=
libs="$here/app/src/main/jniLibs"
rm -rf "$libs"
(cd "$root" && cargo ndk -t arm64-v8a -t x86_64 --platform 26 \
  -o "$libs" build -p resonator-ffi --lib $profile)
# cargo-ndk copies every cdylib it finds (iroh builds some); the app needs
# only the node.
find "$libs" -name '*.so' ! -name libresonator_ffi.so -delete

cd "$here"
case "$what" in
  debug) ./gradlew --no-daemon assembleDebug
         echo "$here/app/build/outputs/apk/debug/app-debug.apk" ;;
  release) ./gradlew --no-daemon assembleRelease bundleRelease
         ls "$here"/app/build/outputs/apk/release/*.apk "$here"/app/build/outputs/bundle/release/*.aab ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac
