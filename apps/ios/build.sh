#!/bin/sh
# Builds the iOS app: the node library (crates/resonator-ffi) for devices
# and the simulator, an XCFramework of it, the Xcode project (xcodegen) and
# the app.
#
#   apps/ios/build.sh                      # simulator build
#   apps/ios/build.sh run                  # simulator build, installed and launched
#   RESONATOR_TEAM=ABCDE12345 apps/ios/build.sh device   # signed build for iPhones
#   RESONATOR_TEAM=... ASC_KEY_PATH=AuthKey.p8 ASC_KEY_ID=... ASC_ISSUER_ID=... \
#     BUILD_NUMBER=42 apps/ios/build.sh archive          # Release archive, uploaded to TestFlight
#
# The relays default to the official ones (project.yml); set
# RESONATOR_IROH_RELAY / RESONATOR_PUSH_RELAY to override.
#
# Needs Xcode, xcodegen and the Rust targets aarch64-apple-ios and
# aarch64-apple-ios-sim. The web client is embedded from client/dist.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
what=${1:-simulator}
gen="$here/Generated"
derived="$root/target/ios/DerivedData"

[ -f "$root/client/dist/index.html" ] || (cd "$root/client" && npm ci && npm run build)

targets="aarch64-apple-ios-sim"
case "$what" in device | archive) targets="aarch64-apple-ios aarch64-apple-ios-sim" ;; esac
for target in $targets; do
  (cd "$root" && cargo build -p resonator-ffi --lib --release --target "$target")
done

# The XCFramework: the static library per platform with the C header and
# its module map (named module.modulemap, as Xcode looks it up).
rm -rf "$gen" && mkdir -p "$gen/include"
cp "$root/crates/resonator-ffi/bindings/ResonatorFFIFFI.h" "$gen/include/"
cp "$root/crates/resonator-ffi/bindings/ResonatorFFIFFI.modulemap" "$gen/include/module.modulemap"
cp "$root/crates/resonator-ffi/bindings/ResonatorFFI.swift" "$gen/"
set --
for target in $targets; do
  set -- "$@" -library "$root/target/$target/release/libresonator_ffi.a" -headers "$gen/include"
done
xcodebuild -create-xcframework "$@" -output "$gen/ResonatorFFI.xcframework" >/dev/null

# The app icon, from the client's.
icons="$here/Resonator/Assets.xcassets/AppIcon.appiconset"
mkdir -p "$icons"
sips -z 1024 1024 "$root/client/dist/icon-512.png" --out "$icons/icon-1024.png" >/dev/null
# iOS draws icons without transparency.
sips -s format png -s formatOptions default "$icons/icon-1024.png" >/dev/null
cat > "$icons/Contents.json" <<JSON
{ "images": [ { "filename": "icon-1024.png", "idiom": "universal", "platform": "ios", "size": "1024x1024" } ],
  "info": { "author": "xcode", "version": 1 } }
JSON
printf '{ "info": { "author": "xcode", "version": 1 } }\n' > "$here/Resonator/Assets.xcassets/Contents.json"

(cd "$here" && RESONATOR_TEAM=${RESONATOR_TEAM:-} xcodegen generate --quiet)

# Relay overrides, passed to xcodebuild as build settings.
set --
[ -n "${RESONATOR_IROH_RELAY+x}" ] && set -- "$@" "RESONATOR_IROH_RELAY=$RESONATOR_IROH_RELAY"
[ -n "${RESONATOR_PUSH_RELAY+x}" ] && set -- "$@" "RESONATOR_PUSH_RELAY=$RESONATOR_PUSH_RELAY"

case "$what" in
  archive)
    : "${RESONATOR_TEAM:?set RESONATOR_TEAM to the Apple developer team ID}"
    : "${ASC_KEY_PATH:?set ASC_KEY_PATH, ASC_KEY_ID and ASC_ISSUER_ID (App Store Connect API key)}"
    auth="-allowProvisioningUpdates -authenticationKeyPath $ASC_KEY_PATH -authenticationKeyID ${ASC_KEY_ID:?} -authenticationKeyIssuerID ${ASC_ISSUER_ID:?}"
    archive="$root/target/ios/Resonator.xcarchive"
    rm -rf "$archive" "$root/target/ios/export"
    # shellcheck disable=SC2086
    xcodebuild -project "$here/Resonator.xcodeproj" -scheme Resonator -configuration Release \
      -destination generic/platform=iOS -archivePath "$archive" $auth \
      CURRENT_PROJECT_VERSION="${BUILD_NUMBER:-1}" "$@" archive | tail -3
    # shellcheck disable=SC2086
    xcodebuild -exportArchive -archivePath "$archive" -exportOptionsPlist "$here/ExportOptions.plist" \
      -exportPath "$root/target/ios/export" $auth | tail -3
    echo "uploaded build ${BUILD_NUMBER:-1} to App Store Connect"
    ;;
  device)
    : "${RESONATOR_TEAM:?set RESONATOR_TEAM to the Apple developer team ID}"
    xcodebuild -project "$here/Resonator.xcodeproj" -scheme Resonator -configuration Debug \
      -destination generic/platform=iOS -derivedDataPath "$derived" \
      -allowProvisioningUpdates "$@" build | tail -3
    echo "$derived/Build/Products/Debug-iphoneos/Resonator.app"
    ;;
  simulator | run)
    xcodebuild -project "$here/Resonator.xcodeproj" -scheme Resonator -configuration Debug \
      -destination generic/platform='iOS Simulator' -derivedDataPath "$derived" \
      CODE_SIGNING_ALLOWED=NO "$@" build | tail -3
    app="$derived/Build/Products/Debug-iphonesimulator/Resonator.app"
    echo "$app"
    if [ "$what" = run ]; then
      device=${SIMULATOR:-"iPhone 16 Pro"}
      xcrun simctl boot "$device" 2>/dev/null || true
      open -a Simulator
      xcrun simctl install "$device" "$app"
      xcrun simctl launch "$device" am.resonator.client.ios
    fi
    ;;
  *) echo "usage: $0 [simulator|run|device|archive]" >&2; exit 2 ;;
esac
