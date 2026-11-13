#!/bin/sh
# Builds Resonator.app (a window on the resonator node of this Mac) into
# target/macos/. Needs the Xcode command line tools.
#
#   apps/macos/build.sh            # the server-mode node of this checkout: data/mac
#   SERVER_URL=http://host:9898/ SERVER_CONFIG= apps/macos/build.sh   # a node elsewhere; never started
#   SERVER_DATA="$HOME/Library/Application Support/Resonator" apps/macos/build.sh
#                                  # a device-mode node with its data (resonator.toml, device.url) there
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out="$root/target/macos"
app="$out/Resonator.app"
url=${SERVER_URL:-http://127.0.0.1:9899/}
config=${SERVER_CONFIG-data/mac/resonator.toml}
log=""
url_file=""
if [ -n "${SERVER_DATA:-}" ]; then
  config="$SERVER_DATA/resonator.toml"
  log="$SERVER_DATA/server.log"
  url_file="
  <key>ServerURLFile</key><string>$SERVER_DATA/device.url</string>"
elif [ -n "$config" ]; then
  log="$(dirname "$config")/server.log"
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
swiftc -O -swift-version 5 -o "$app/Contents/MacOS/Resonator" "$here/main.swift"

icon="$root/client/dist/icon-512.png"
if [ -f "$icon" ]; then
  set_dir="$out/Resonator.iconset"
  rm -rf "$set_dir" && mkdir -p "$set_dir"
  for size in 16 32 128 256; do
    sips -z $size $size "$icon" --out "$set_dir/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$icon" --out "$set_dir/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns -o "$app/Contents/Resources/Resonator.icns" "$set_dir"
  rm -rf "$set_dir"
fi

server_keys=""
if [ -n "$config" ]; then
  server_keys="
  <key>ServerDirectory</key><string>$root</string>
  <key>ServerCommand</key><string>target/release/resonator</string>
  <key>ServerArguments</key><array><string>--config</string><string>$config</string><string>serve</string></array>
  <key>ServerLog</key><string>$log</string>"
fi
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Resonator</string>
  <key>CFBundleDisplayName</key><string>Resonator</string>
  <key>CFBundleIdentifier</key><string>am.resonator.client.mac</string>
  <key>CFBundleExecutable</key><string>Resonator</string>
  <key>CFBundleIconFile</key><string>Resonator</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSAppTransportSecurity</key>
  <dict>
    <key>NSAllowsLocalNetworking</key><true/>
    <key>NSAllowsArbitraryLoadsInWebContent</key><true/>
  </dict>
  <key>ServerURL</key><string>$url</string>$url_file$server_keys
</dict>
</plist>
PLIST
codesign --force --sign - "$app" >/dev/null 2>&1 || true
echo "$app"
