#!/bin/sh
# Builds the desktop app (Tauri): the web client, then the app with the
# node inside.
#
#   apps/desktop/build.sh            # debug build: target/debug/resonator-desktop
#   apps/desktop/build.sh release    # installers for this OS: target/release/bundle/
#                                    #   macOS .app/.dmg, Windows .msi/.exe, Linux .deb/.rpm/.AppImage
#
# Needs Rust, Node 22, and on Linux the WebKitGTK development packages
# (Debian/Ubuntu: libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev
# libayatana-appindicator3-dev patchelf). Set RESONATOR_IROH_RELAY to
# change the relay (empty: iroh's public relays).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
what=${1:-debug}

[ -f "$root/client/dist/index.html" ] || (cd "$root/client" && npm ci && npm run build)

cd "$here"
case "$what" in
  debug) cargo build && echo "$here/target/debug/resonator-desktop" ;;
  release) npx --yes @tauri-apps/cli@2 build ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac
