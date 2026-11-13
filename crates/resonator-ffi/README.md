# resonator-ffi

The resonator node for apps. It is a [UniFFI](https://mozilla.github.io/uniffi-rs/) 0.32 interface over `resonator-node`. It uses proc-macros and has no UDL file. The generated Swift and Kotlin are in `bindings/`.

An app embeds a whole node in **device mode**:

- one account, signed in from loopback with a token;
- the web client, served by the node and shown in a WebView;
- federation over iroh to the person's other devices and to other people.

## Building

The crate builds a `cdylib` (Android, desktop) and a `staticlib` (iOS).

```sh
# Host (Linux/macOS), with the bindings regenerated from the built library:
cargo build -p resonator-ffi --lib
cargo run -p resonator-ffi --bin uniffi-bindgen -- generate \
  --library target/debug/libresonator_ffi.so \
  --language swift --language kotlin --out-dir crates/resonator-ffi/bindings
#   (on macOS the library is target/debug/libresonator_ffi.dylib)

# Android (NDK + cargo-ndk), one ABI per target:
cargo ndk -t arm64-v8a -t x86_64 --platform 26 -o app/src/main/jniLibs \
  build -p resonator-ffi --lib --release

# iOS (macOS with Xcode):
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build -p resonator-ffi --lib --release --target aarch64-apple-ios
cargo build -p resonator-ffi --lib --release --target aarch64-apple-ios-sim
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libresonator_ffi.a -headers crates/resonator-ffi/bindings \
  -library target/aarch64-apple-ios-sim/release/libresonator_ffi.a -headers crates/resonator-ffi/bindings \
  -output ResonatorFFI.xcframework
```

The web client is embedded at build time. Run `npm run build` in `client/` first, or the node serves a placeholder page.

`uniffi.toml` names the generated modules:
- Swift module `ResonatorFFI`, with the C module `ResonatorFFIFFI`;
- Kotlin package `am.resonator.client.ffi`, which loads `libresonator_ffi.so`.

On Android, the Kotlin bindings also need JNA (`net.java.dev.jna:jna:5.x@aar`).

## The interface

| Swift / Kotlin | What it does |
| --- | --- |
| `ResonatorNode.start(config: NodeConfig)` | Starts the node and blocks until it serves on `127.0.0.1:<free port>`. |
| `node.signedInUrl()` | Returns `http://127.0.0.1:<port>/?token=<token>`, the page to load. **Never log it.** |
| `node.localUrl()`, `node.token()` | Return the parts of that URL. |
| `node.enterBackground(budgetMs:)` | Hands the host lease over, sends queued edits and flushes. |
| `node.enterForeground(budgetMs:)` | Syncs with the person's other devices, sends queued edits and looks at the lease again. |
| `node.leaseState()` | Returns the person's host lease (`LeaseInfo`). |
| `node.pairInvite(admin:)` | On an admin device: returns a `PairInvite` (10 minutes, once): `code`, a short code to type such as `482913-ocean-tiger` (none if the pairing rendezvous is off or cannot be reached), `link`, a `resonator:pair/…` link for a QR code, and `id`, `expiresAtMs`, `admin`. **Never log the code or the link.** |
| `node.pairJoin(codeOrLink:deviceName:waitMs:)` | Pairs this fresh device by such a code (case, hyphens or spaces do not matter) or link and restarts the node as a device of that person (same URL and token). Returns `PairJoinResult`. |
| `node.setPushToken(platform:token:)` | Keeps this device's push token (`PushPlatform.apns`, `.apnsSandbox` or `.fcm`; APNs tokens as hex) so other nodes can have the device woken through the push relay. Call it after `start` and whenever the platform hands out a new token. Throws `Config` for a malformed token. **Never log the token.** |
| `node.clearPushToken()` | Forgets the push token (notifications turned off). |
| `node.stop()` | Stops the node and closes the database. It is idempotent. |
| `contactLink(address:)` | Returns `resonator:contact/<address>`, for sharing as a QR code or a message. |

`NodeConfig` has these fields:

- `dataDir`: required. A directory of the app's own; it is not backed up to other devices.
- `account`: optional, default `me`.
- `deviceName`: optional.
- `allowRemote`: default false.
- `extraToml`: optional. TOML laid over the device defaults, for example `[federation] relay = "..."`, `[person] anchor = true` or `[ai]`.

Every call blocks. Call them off the main thread (a background `Task`, or `Dispatchers.IO`). The node runs on its own small tokio runtime. Errors come back as `ResonatorError`, which is one of `Config`, `Node` or `Stopped`.

### What the node does on the first start

On its first start, a device makes a **person key** and stores it in `dataDir/person.key`. The account becomes `me@<person domain>`, where the person domain is the key's z-base-32 form. That is the same account on every device of the person.

To add another device:

1. Run `resonator person cert` on a device that holds the key.
2. Pass the printed certificate to the new device as `extraToml = "[person]\nadmin = false\ncert = \"…\""`.

## Starting and loading the WebView

```swift
// iOS (SwiftUI)
import ResonatorFFI

let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("resonator").path
let node = try await Task.detached {
    try ResonatorNode.start(config: NodeConfig(dataDir: dir, account: nil,
        deviceName: UIDevice.current.name, allowRemote: false, extraToml: nil))
}.value
webView.load(URLRequest(url: URL(string: node.signedInUrl()!)!))
```

```kotlin
// Android
import am.resonator.client.ffi.*

val node = withContext(Dispatchers.IO) {
    ResonatorNode.start(NodeConfig(dataDir = filesDir.resolve("resonator").path,
        deviceName = Build.MODEL))
}
webView.settings.javaScriptEnabled = true
webView.settings.domStorageEnabled = true
webView.loadUrl(node.signedInUrl()!!)
```

### Signing in with the token

`GET /?token=…` from a loopback peer creates a session for the device's account. The node answers with a `303` to `/` and sets the usual `WSESSIONID` cookie, so the WebView's cookie store keeps the session from then on.

The token:
- is new at every start;
- works only from loopback, never through a proxy;
- is compared in constant time.

When `allowRemote` is off, which is the default, every request that does not come from loopback is refused. When it is on, other machines get the normal password sign-in.

The client in device mode has no sign-in screen and no sign-out button.

### Other origins

Requests that change state check `Origin`. The node accepts:
- its own origin;
- the wrapper origins in `[device] origins`. The defaults are `resonator://app` (a custom-scheme handler on iOS) and `https://appassets.androidplatform.net` (Android's `WebViewAssetLoader`).

Add yours with `extraToml = "[device]\norigins = [\"…\"]"`.

### Android: cleartext to loopback

Android blocks cleartext HTTP to loopback unless you allow it. Allow it in `res/xml/network_security_config.xml`:

```xml
<network-security-config>
  <domain-config cleartextTrafficPermitted="true">
    <domain includeSubdomains="false">127.0.0.1</domain>
  </domain-config>
</network-security-config>
```

On iOS, App Transport Security already exempts loopback.

## Lifecycle

A phone suspends apps, so the node must not be the only holder of the person's host lease while it sleeps.

```swift
// iOS
.onChange(of: scenePhase) { phase in
    switch phase {
    case .background:
        let task = UIApplication.shared.beginBackgroundTask()
        Task.detached {
            _ = try? node.enterBackground(budgetMs: 4_000)
            await UIApplication.shared.endBackgroundTask(task)
        }
    case .active:
        Task.detached { _ = try? node.enterForeground(budgetMs: 10_000) }
    default: break
    }
}
```

```kotlin
// Android (an Activity, or ProcessLifecycleOwner for the whole app)
override fun onStop() { super.onStop()
    lifecycleScope.launch(Dispatchers.IO) { node.enterBackground(4_000u) } }
override fun onStart() { super.onStart()
    lifecycleScope.launch(Dispatchers.IO) { node.enterForeground(10_000u) } }
```

`enterBackground` does these steps:

1. If this device holds the lease, it hands the lease to a reachable anchor, or failing that to another reachable device of the person.
2. It waits, up to the budget, until that device has caught up and taken the lease.
3. It sends the pending queue and flushes the database.

iOS gives an app about 5 to 30 s in the background, so keep the budget well under that.

`enterForeground` does these steps:

1. It tells iroh that the network may have changed.
2. It syncs with every other device of the person.
3. It drains the pending queue and wakes the lease task.

Call `stop()` when the app is terminated (it is also safe to just let the process die: SQLite's WAL keeps the database consistent).

## Key storage

The device key (`federation.key`) and the person key (`person.key`, which holds the recovery phrase) are files in `dataDir` for now. Keep `dataDir` out of backups and out of shared storage:
- on iOS, use Application Support with `isExcludedFromBackup`;
- on Android, use `filesDir` with `android:allowBackup="false"` or backup rules excluding it.

The person key is read through the `KeyStore` trait of `resonator-federation` (`load`, `store`). The next step is a foreign implementation passed in through UniFFI (`#[uniffi::export(with_foreign)]`):
- **iOS:** Keychain (`kSecClassGenericPassword`, `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`).
- **Android:** a key in the Android Keystore that wraps the phrase, kept in `EncryptedSharedPreferences` or DataStore.

The device key follows the same way. Until then, the files are owner-only (mode 600). The recovery phrase is never logged and never crosses the FFI.
