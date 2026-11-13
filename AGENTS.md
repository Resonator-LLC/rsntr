# AGENTS.md

Guide for coding agents (and humans) working in this repository. The
README is for users; this file is for changing the code.

## What this is

Resonator: Wave-style real-time conversations (operational transform,
waves, blips, threads) in Rust, with a TypeScript web client embedded in
the binary, federation over iroh, and a node that embeds in mobile apps.
Ported from Apache Wave; see NOTICE.

## Map

| Path | What |
|---|---|
| `crates/resonator-model` | Documents, operations, OT, conversation model. Pure, no I/O. Golden files in `testdata/`. |
| `crates/resonator-server` | SQLite storage and migrations, wave server, HTTP/WebSocket, auth, search, attachments, config. |
| `crates/resonator-robots` | Robot / Data API operations and events. |
| `crates/resonator-agents` | Built-in AI agents, model connector, `/mcp`, `/robot/*` endpoints. |
| `crates/resonator-federation` | Server keys, trust, signed deltas, iroh endpoint, host/replica, person mode, leases. |
| `crates/resonator-node` | A node in device mode, for embedding. |
| `crates/resonator-ffi` | UniFFI interface (Swift, Kotlin) over the node; generated bindings in `bindings/`. |
| `crates/resonator-push` | The push relay binary (APNs, FCM). |
| `crates/resonator` | The `resonator` CLI and server binary. |
| `client/` | Web client: Preact + signals, Vite, Vitest, Playwright. |
| `apps/ios`, `apps/android`, `apps/macos` | WebView wrappers around the embedded node. |
| `deploy/` | Provisioning and deploy scripts of the official servers. |
| `docs/` | Design documents (federation, devices, push). |

## Build and test

The commands CI runs (`.github/workflows/ci.yml`); run them before you
say a change is done:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd client && npm ci && npm run typecheck && npm test && npm run build
cargo build -p resonator -p resonator-server
cd client && npx playwright install chromium webkit && \
  RESONATOR_BINARY=../target/debug/resonator-server \
  RESONATOR_NODE_BINARY=../target/debug/resonator npm run test:e2e
```

- Build `client/dist` before building the binary or the apps; it is
  embedded at compile time (without it a placeholder page is served).
- Run a local server: `cargo run -p resonator -- --domain localhost
  --listen 0.0.0.0:9898 serve`. Client dev server with hot reload:
  `cd client && RESONATOR_BACKEND=http://127.0.0.1:9898 npm run dev`.
- Set `CARGO_INCREMENTAL=0` on small disks; release builds use thin LTO.
- iOS needs macOS (Xcode, xcodegen); Android needs the SDK, NDK and
  `cargo-ndk`. See `apps/*/build.sh`.
- After changing the FFI interface, regenerate the bindings (see
  `crates/resonator-ffi/README.md`) and commit them. Do not name UniFFI
  error fields `message` (Kotlin exceptions already have one).
- The node runs in Android's app sandbox too: no hard links (SELinux denies
  them), no netlink route dumps. Test device changes on an emulator
  (`apps/android/build.sh`, then `adb install`).

## Rules the code keeps

- **Migrations only append.** Never edit an existing migration in
  `crates/resonator-server/src/storage/migrations.rs`; add a new one.
  Databases in the field are at every earlier version.
- **Wave compatibility.** The OT, ids, hashed version chain and wire
  protocol follow Apache Wave and are checked against golden files from
  the Java code. A difference from the original is either a bug or marked
  "Deliberate fix" / documented as a deviation in the module docs. Do not
  "fix" behavior silently.
- **Wave ids embed the host domain** (`wave://domain/...` in the version-0
  hash), so a wave never moves host. Never write code that renames domains
  in stored data.
- **Off by default.** Federation, person mode and push are off unless
  configured; with them off nothing of them runs and behavior is unchanged.
- **Secrets.** Never log or print tokens, pairing codes, pairing links,
  push tokens, recovery phrases or keys. Secrets are read from files named
  in config, never from committed files. `*.p8` and `data/` are ignored.
- **Positions** in documents and tools are UTF-16 code units, as in the
  original.
- Tests come with changes: Rust unit tests next to the code, integration
  tests in `crates/*/tests`, client tests next to the source
  (`*.test.ts`), end-to-end in `client/e2e`.

## Style

- Rust 2024 edition, `rustfmt` defaults, no `unsafe` outside FFI.
- Comments and docs: plain, short sentences, ASCII.
- Commits: one topic each, imperative subject, signed off (`git commit -s`).

## Docs worth reading first

- `README.md`: configuration, federation, agents.
- `docs/federation-plan.md`, `docs/device-plan.md`, `docs/push-relay.md`.
- `crates/resonator-ffi/README.md`: the app interface and lifecycle.
- `crates/resonator-federation/HANDOFF.md`, `crates/resonator-agents/HANDOFF.md`:
  state of those crates.
