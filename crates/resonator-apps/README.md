# resonator-apps

The runtime of wave apps. A wave app is a WebAssembly component that runs
on every node holding the wave: the same artifact on a phone, a laptop and
an anchor server. The instances share state through the app wavelet and
talk directly over iroh; the browser only draws what the app renders.

This crate is only the runtime. It knows nothing of the wave server: the
node feeds an instance events and carries out the effects it returns.

## The world

`wit/app.wit`, package `resonator:app@0.1.0`, world `wave-app`.

| Import | What |
|---|---|
| `wave` | `messages(since)`, `post(kind, body)`: the app wavelet |
| `kv` | `get`, `set`, `delete`: a store local to this node |
| `peers` | `list`, `send`, `open`, `write`, `close`: devices running the same app |
| `ui` | `invalidate(view)`: ask for a new render |
| `log` | `log(level, message)` |

The app exports `init`, `on-messages`, `on-peers-changed`, `on-peer-data`,
`on-stream`, `on-stream-data`, `on-stream-closed`, `render` and `on-ui`.
WIT has no recursive types, so `render` returns a flat list of `ui-node`s
in document order, each naming its parent by index.

## Rules

- Reads come from a `Snapshot` the node keeps current. Writes are queued
  as `Effect`s and carried out after the call returns, so a call never
  blocks on the node.
- The manifest grants permissions (`wave`, `kv`, `peers`, `streams`,
  `media`, `camera`, `video`). An ungranted write returns `Err("permission denied")`; an
  ungranted read returns nothing.
- Every call has a deadline (200 ms for events, 50 ms for `render`),
  enforced by epoch interruption with a 10 ms tick. Memory is limited to
  the manifest's `memory_mb` (64 by default, at most 256).
- A trap or a missed deadline fails the instance; it takes no more calls.
- WASI is linked with an empty context: no files, sockets, environment or
  stdio. Clocks and random work.
- Cranelift compiles to native code; on iOS, which does not allow code
  generation at run time, it compiles to Pulley bytecode for the Pulley
  interpreter. Compiled code is cached by wasmtime's own cache.

## Bundles

A zip with `manifest.json` and `app.wasm`. Its id is `sha256:<hex>` of
the zip.

```json
{ "name": "walkie", "version": "1.0", "permissions": ["peers", "streams", "media"], "memory_mb": 32 }
```

Apps are written with `crates/resonator-app-sdk` (Rust) or
componentize-js; see `examples/wave-apps/` and `docs/wave-apps.md`.

## Test guests

`tests/guests/counter-rs` (Rust, wit-bindgen) and `tests/guests/counter-js`
(JavaScript, componentize-js) behave identically; the tests check that.
Their builds are checked in under `tests/fixtures/`, so `cargo test` needs
neither toolchain. To rebuild them:

```sh
rustup target add wasm32-wasip2
crates/resonator-apps/build-guests.sh
```
