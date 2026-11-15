# Wave apps

Small programs that run inside a wave: a gadget in a blip names a bundle,
and every participant's own node runs it for them. See
[docs/wave-apps.md](../../docs/wave-apps.md) for how they work.

| App | Language | Permissions | What it does |
|---|---|---|---|
| `counter` | Rust | wave, kv | A count everyone in the wave shares. |
| `chat-js` | JavaScript | wave, kv | A chat in a box; the draft survives a restart. |
| `walkie` | Rust | peers, streams, media | Push to talk with the others who have it open. |
| `template-js` | JavaScript | none | A start for your own. |

## Build a bundle

```sh
./bundle.sh counter        # writes counter/counter.zip
```

Then, in a wave, Edit, Insert app (the toolbar), and pick the zip. The
bundle is uploaded as an attachment of the wave and the gadget names its
hash; each participant allows it once.

Rust apps need `rustup target add wasm32-wasip2` and use
[`crates/resonator-app-sdk`](../../crates/resonator-app-sdk). JavaScript
apps are built with componentize-js (Node.js 22) and carry their engine,
so they are about 12 MB.

## A bundle

A zip with two files:

- `manifest.json`: `name`, `version`, `permissions` (any of `wave`, `kv`,
  `peers`, `streams`, `media`) and optionally `memory_mb` (64 by default,
  up to 256).
- `app.wasm`: a WebAssembly component of the world `wave-app` in
  [`crates/resonator-apps/wit/app.wit`](../../crates/resonator-apps/wit/app.wit).

The app id is the sha256 of the zip; a changed bundle is a new app, which
people allow again.
