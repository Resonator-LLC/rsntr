# Device plan, phase 4 (D: device mode and embedding): handoff

What `docs/device-plan.md` section D asked for now exists:
- a device mode;
- contacts in place of the directory;
- the node as a library (`resonator-node`), with a thin `resonator` binary over it;
- a UniFFI crate (`resonator-ffi`) with generated Swift and Kotlin bindings;
- the client in device mode.

With `mode = "server"` (the default), nothing changes:
- no token is made;
- no account is created;
- `/contacts` is 404;
- `/session` has no `mode` or `device` keys;
- `resonator config` prints neither `mode` nor `[device]`.

## What exists

### Config (`resonator-server/src/config/device.rs`)
- **`mode`:** `"server"` (the default) or `"device"`. It is also set by `--mode` or `RESONATOR_MODE`.
- **`[device]`:**
  - `account`: default `"me"`. Also set by `--device-account`.
  - `allow_remote`: default false. Also set by `--device-allow-remote`.
  - `origins`: default `["resonator://app", "https://appassets.androidplatform.net"]`.
  - Validation: the account must be a valid new user name; each origin must be `scheme://authority`.
- **Helpers on `Config`:**
  - `is_device()`;
  - `device_address()`, which is `<account>@<domain>`. In person mode the domain is the person domain, so the account is the same on every device of the person.
  - `registration_open()` and `directory_open()`, both false in device mode.

### Server (`resonator-server`)
- **Account:**
  - `App::prepare_with` creates the device account if it is missing: a human account with no password.
  - `DeviceAccess` (on `AppState`) holds the account and a random 32-byte token, base64url-encoded and new at every start.
  - Its `Debug` output hides the token, and it compares tokens in constant time.
- **The gate** (`http/device_handlers.rs::gate`) runs in `context_layer`, before authentication:
  - A non-loopback peer gets 403 unless `allow_remote` is set. The peer comes from `ConnectInfo`; IPv4-mapped loopback counts as loopback.
  - `GET` or `HEAD /?token=…` from a loopback peer that is not a trusted proxy, with the right token, creates a session (`AuthService::create_session`). The answer is a 303 to `/` with the usual `WSESSIONID` cookie, so the WebSocket `socket_token` flow is unchanged.
  - Any other token, or a remote peer, gets 403 "Invalid token.". It is logged without the token.
- **Origins:** `origin_allowed` also accepts `device.origins` in device mode.
- **`device.url`:** `App` writes `<data dir>/device.url` (mode 600) containing `http://127.0.0.1:<port>/?token=…`. Only its path is logged, and the file is removed on shutdown. It is used by:
  - launchers on the same machine;
  - the e2e tests.

  Related `App` methods: `device_url()`, `local_url()` and `device_url_path()`.
- **`/session`:**
  - adds `"mode": "device"` and `"device": {account, name, allow_remote, key?}`. `key` is the z32 device key; it is set by `FederationSetup::serve_app`.
  - `registration_enabled` is false in device mode.
- **`/directory`:** 404 in device mode.
- **Contacts:**
  - Storage: migration 13 adds `contacts(owner, address, added_at)`, accessed through `ContactStore` (`Database::contact_store()`).
  - Listing: the stored contacts, unioned with the participants of conversational wavelets that the owner shares (from the search index). It leaves out the owner and `@domain` participants, puts humans first, and matches the query case-insensitively.
  - `GET /contacts?query=&limit=`: the same shape as `/directory` (`{"accounts":[{address,name,image_url,kind}]}`), plus `added`.
  - `POST /contacts {address}` or `{link: "resonator:contact/<address>"}`: 201 for a new contact, 200 if it was already there. An address of another domain is probed with the federation probe when federation runs, and the result is returned as `probe`.
  - `DELETE /contacts/{address}`: 204, or 404 if absent.
  - All three return 404 outside device mode.

### Federation (`resonator-federation`)
- **`Federation::own_devices() -> Vec<OwnDevice{key, anchor, reachable}>`:** the person's other devices, ordered with anchors first and then reachable devices. It reads `Leases::known_anchors()` and `Leases::reachable()`.
- **`Federation::recheck_lease()`:** wakes the lease task.
- **`Federation::network_change()`:** calls iroh's `Endpoint::network_change()`.
- **`FederationSetup::serve_app`:** also passes the device key to `AppState::set_device_key`.

### `crates/resonator-node` (new)
- Holds `web.rs` and `build.rs`, moved from `crates/resonator`: the embedded client from `client/dist`.
- **`launch(config)` and `launch_with(config, LaunchOptions{tuning, resolver})`** return a `Node`. They run the former `main.rs` assembly in the same order:
  1. device preparation;
  2. `configure_person`;
  3. federation setup with the delta signer;
  4. `App::prepare_with`;
  5. subscribe and `serve_app`;
  6. `Integrations::mount`;
  7. `serve(routes + web)`;
  8. `federation.start`.

  `Node::shutdown()` stops them in order: federation, then the agents, then the app.
- **`Node`** has these methods:
  - `config()`, `app()`, `server()`, `state()`, `integrations()`, `federation()`;
  - `local_addrs()`, `local_url()`;
  - `token()`, `signed_in_url()`;
  - `lease_state()`;
  - `enter_background(budget)`, which returns a `BackgroundReport`;
  - `enter_foreground(budget)`, which returns a `ForegroundReport`;
  - `shutdown()`.
- **Device preparation** (`prepare_device`):
  - The default listen address becomes `127.0.0.1:0`.
  - The data directory is created.
  - On the first start of an admin device in person mode with no certificate and no key, a person key is made in `person.key` (FileKeyStore, mode 600). The phrase is never logged; `resonator person show --phrase` shows it.
- **`device_config(data_dir, overrides_toml)`:** device defaults, with TOML overrides merged in key by key. The defaults are:
  - `mode = "device"`;
  - listen on `127.0.0.1:0`;
  - `resonator.db` in the data directory;
  - federation on;
  - person mode on, as an admin device.
- **`enter_background(budget)`:**
  1. If this device holds the lease, it calls `handover()` to the first entry of `own_devices()`: an anchor, then a reachable device, then any other.
  2. It waits until that device holds the lease (`handover_complete`).
  3. It runs `drain_pending` within the budget and records `pending_left`.
  4. It calls `server.flush()`.
- **`enter_foreground(budget)`:**
  1. `network_change()`;
  2. `recheck_lease()`;
  3. `sync_with` every own device;
  4. `drain_pending`;
  5. `recheck_lease()` again, then reports the lease.
- **`start(config)` and `start_with`** return a `Handle` that owns its own multi-thread tokio runtime with 2 workers. It is for FFI and other callers outside a runtime.
  - Methods: `local_url()`, `token()`, `signed_in_url()`, `is_running()`, `lease_state()`, `enter_background()`, `enter_foreground()` and `stop()`.
  - `stop()` is idempotent and also runs on drop; later calls return `NodeError::Stopped`.
  - `Debug` hides the token.
- **`backup(config, path)`.**

### `crates/resonator` (the binary)
- `serve` is now `resonator_node::launch` → `stop_signal` → `shutdown`.
- In device mode it logs the path of `device.url`.
- The CLI is unchanged.
- `axum`, `rust-embed` and `resonator-agents` moved to dev-dependencies or out entirely.

### `crates/resonator-ffi` (new)
- **Interface:** UniFFI 0.32.2, proc-macros only. It builds `lib`, `cdylib` and `staticlib`.
  - `ResonatorNode.start(NodeConfig{dataDir, account?, deviceName?, allowRemote, extraToml?})`.
  - Node methods: `localUrl()`, `token()`, `signedInUrl()`, `leaseState() -> LeaseInfo?`, `enterBackground(budgetMs) -> BackgroundResult`, `enterForeground(budgetMs) -> ForegroundResult`, `isRunning()`, `stop()`.
  - `contactLink(address)`.
  - Errors: `ResonatorError{Config, Node, Stopped}`.
- **Bindgen:** the `uniffi-bindgen` binary is in the crate. `uniffi.toml` sets the Swift module `ResonatorFFI` and the Kotlin package `am.resonator.client.ffi`.
- **`bindings/`:** `ResonatorFFI.swift`, `ResonatorFFIFFI.h`, `ResonatorFFIFFI.modulemap` and `am/resonator/client/ffi/resonator_ffi.kt`, generated from the host build with `--no-format` (neither swift-format nor ktlint is installed).
- **`README.md`:** building for each platform, starting the node, the WebView and token, cleartext to loopback on Android, lifecycle hooks (`scenePhase`, `onStop`/`onStart`), and key storage.

### Client
- **`SessionInfo.device?`** is `{account, name, key, allowRemote}`. It is parsed only when `mode` is `"device"`.
- **API:** `api.contacts(query, limit)`, `api.addContact(addressOrLink)` and `CONTACT_LINK_PREFIX`.
- **`WavePanel`:** the `directory` prop is `contacts` in device mode. This covers @mention and the add-participant dialog.
- **`TopBar`:** in device mode it shows a `.topbar-device` pill with the device name ("this device" if it has none), and the title "<address> on <device>". There is no sign-out button. The pill is hidden on narrow screens, as the address is.
- **`AuthScreen`:** in device mode without a session, and without `allow_remote`, it shows "Open Resonator from its app" instead of the form.

## Tests

| What | Where |
| --- | --- |
| Token signs in; wrong token 403; no registration or directory; `device.url` is mode 600 and removed on stop; new token per start | `resonator-node/tests/device.rs` `the_token_signs_the_device_account_in` |
| Non-loopback refused through `ConnectInfo` (`router(...).oneshot`); IPv6 and IPv4-mapped loopback accepted; `allow_remote` serves remote peers but never takes the token from them | `requests_from_other_machines_are_refused` |
| Contacts: added by address or link, de-duplicated, invalid or self refused, from shared waves, query, delete, no session | `contacts_stand_in_for_the_directory` |
| Default device config: person key made once; account on the person domain; `/session` device key | `a_first_device_makes_its_person` |
| `Handle`: own runtime; served to a plain TCP client from another thread; background and foreground without federation; `stop()` twice; `Stopped`; start, stop and start again on the same data | `a_handle_starts_stops_and_starts_again_on_the_same_data` |
| `enter_background` hands the lease to the other device and waits for it; `enter_foreground` syncs | `resonator/tests/persons.rs` `a_device_going_to_the_background_hands_the_lease_over` (run 3 times, about 8 s each) |
| FFI start, stop and errors; contact link | `resonator-ffi` unit tests |
| Gate helpers, token parameter, contact request parsing | `device_handlers` unit tests |
| `getSession` device info; contacts and addContact | `client/src/net/tests/http.test.ts` |
| Playwright: no token gives a pointer to the app and no form; a wrong token 403; `/?token=` lands in the inbox signed in, with no sign-out and the device pill; a new wave whose @mention suggestions come from contacts (a local one and one of another domain); no `/directory` call; a phone-sized screen without overflow | `client/e2e/app/device.spec.ts` (`TestServers.start({device: true})`, with `deviceUrl` read from `device.url`) |

Counts:
- `cargo test --workspace`: 1250 passed (baseline 1238).
- Client `npm test`: 1177 (baseline 1174).
- Playwright: 32 (baseline 29; 3 new in `device.spec.ts`).
- Clippy (`--workspace --all-targets`), `cargo fmt --check` and `tsc` are clean, and `npm run build` succeeds.

## Mobile targets
- **`aarch64-linux-android`:** `cargo ndk -t arm64-v8a --platform 26 check -p resonator-ffi` and `build --lib` both succeed. The result is a 200 MB debug `.so`; release builds are stripped and LTO'd.
- **`aarch64-apple-ios`:** not tried. The rustup target is not installed, and there is no Xcode or iOS SDK; the C build scripts (bundled SQLite, ring) need the SDK. It needs macOS with Xcode: `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`, then the commands in the FFI README.

## Deviations
- **Device mode lives in `resonator-server`,** not only in the node, so the `resonator-server` binary supports it too. Playwright uses it.
- **Device mode does not require person mode.** The account is `<account>@<domain>`. In person mode (the `device_config` defaults) the domain is the person domain, which makes the account the same on every device.
- **The background test hands over to a non-anchor desktop, not an anchor.** With a reachable anchor, a non-anchor holder's lease task already hands the lease to it within a renewal interval, so a test with an anchor would race that. `own_devices()` puts anchors first, so `enter_background` prefers one when it does the handover itself.
- **The `Handle` API takes a budget:** `enter_background` and `enter_foreground` take a `Duration` (`budget_ms` over FFI) and return reports.
- **`LeaseState`** is exposed as `Option`, being `None` without person mode.

## Open issues
- **No password for `allow_remote`.** With `allow_remote`, the device account has no password, so remote sign-in cannot work until one is set. A `resonator device password` command, or a settings page, is still to do.
- **Keys are files.** The person key and device key are files in the data directory. The `KeyStore` foreign trait (Keychain, Android Keystore) is still to do; see the FFI README. The device key (`federation.key`) needs the same treatment.
- **No UI for adding contacts.** The API exists, but there is no QR or link UI yet. The client does not register a handler for `resonator:contact/` links; the wrapper app must pass them to `addContact`.
- **Contacts do not record whether a remote address is reachable.** The probe result is returned when the contact is added, but it is not stored.
- **`enter_background` does not wait for remote hosts.** It waits for the new holder within the budget but not for remote hosts beyond it. Pending edits stay queued and are sent at the next foreground or by the retry loop.
- **Pairing (section E) is not done.** A second device still needs `resonator person cert` by hand; a config example is in the FFI README.
- **Bindings are unformatted** (no swift-format or ktlint here). They are regenerated from the host build and must be regenerated whenever the FFI interface changes.
- **The FFI cannot set up logging.** A tracing subscriber that writes to `os_log` or logcat is still to do; logs never contain tokens or phrases.
