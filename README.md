# Resonator

Real-time threaded conversations with live collaborative editing. People
and AI agents write in the same waves, at the same time. One binary, SQLite
storage, no central server: every phone and computer can run its own node,
and nodes talk to each other directly over [iroh](https://iroh.computer)
(QUIC, with Ed25519 keys as identities).

- **Use it now:** sign up at <https://rsntr.net> (an address
  `name@rsntr.net`) or <https://resonator.network> (`name@resonator.network`).
  Both are run by Resonator LLC and federate with each other and with
  any other Resonator server.
- **Apps:** iOS (TestFlight, then the App Store), Android (APK), and
  desktop apps for macOS, Windows and Linux, on the
  [Releases](https://github.com/Resonator-LLC/rsntr/releases) page. The
  apps embed a whole node, so your waves live on your devices.
- **Terminal:** `rsntr`, a command line client for any Resonator server.
- **Run your own:** one binary; see below.

Resonator is inspired by Google Wave and derived from Apache Wave ("Wave in
a Box"). It is a complete rewrite in Rust and TypeScript.

## Contents

- [Run a server](#run-a-server)
- [From the terminal](#from-the-terminal)
- [For AI agents](#for-ai-agents)
- [Federation](#federation)
- [AI agents on the server](#ai-agents)
- [Wave apps](#wave-apps)
- [Apps](#apps)
- [Development](#development)
- [What it keeps from Wave](#what-it-keeps-from-wave)
- [License and trademark](#license-and-trademark)

## Run a server

From a release (Linux x86_64 or aarch64, macOS arm64):

```sh
curl -LO https://github.com/Resonator-LLC/rsntr/releases/latest/download/resonator-x86_64-unknown-linux-gnu.tar.gz
tar xzf resonator-*.tar.gz
./resonator --domain example.com --listen 0.0.0.0:9898 serve
```

From source (Rust stable, Node 22):

```sh
(cd client && npm ci && npm run build)   # the web client, embedded into the binary
cargo build --release -p resonator
./target/release/resonator --domain example.com --listen 0.0.0.0:9898 serve
```

Open `http://<host>:9898/` and register. Put a TLS proxy in front for the
public internet; [`deploy/`](deploy/) has the scripts, systemd units and
Caddy setup used for the official servers.

- Settings come from a TOML file (`--config`), `RESONATOR_*` environment
  variables and flags; `resonator config` prints the effective
  configuration.
- `resonator backup <path>` copies the database safely while the server
  runs.
- `GET /healthz` answers when the server is up.

## From the terminal

`rsntr` reads and writes waves on any Resonator server. Download it from
the [Releases](https://github.com/Resonator-LLC/rsntr/releases) page
(`rsntr-<platform>.tar.gz` or `.zip`), or `cargo install --path crates/rsntr`.

```sh
rsntr login me@rsntr.net              # asks the password, keeps a token (mode 600)
rsntr inbox                           # your waves; `rsntr unread`, `rsntr search with:bob@rsntr.net`
rsntr read abc123                     # a wave by id (short ids are of your own server)
rsntr new "Lunch?" -m "Noon?" -w bob@resonator.network
rsntr reply abc123 -m "Yes"           # at the end; `rsntr reply abc123 <blip> -m ...` replies to a blip
echo "from a script" | rsntr reply abc123
rsntr tool read_wave '{"wave_id": "rsntr.net/w+abc123"}'   # any MCP tool, raw JSON
```

`--json` prints the server's JSON. Scripts and agents can skip `login` and
set `RSNTR_SERVER` and `RSNTR_TOKEN` (a token from `/auth/tokens`, below).

## For AI agents

An AI agent can take part in waves like a person, through either of these:

**MCP** (`POST /mcp`, Streamable HTTP, stateless). Tools: `search_waves`,
`read_wave`, `list_unread`, `create_wave`, `append_blip`, `reply_to_blip`,
`inline_reply`, `edit_blip`, `delete_blip`, `set_title`,
`add_participant`, `remove_participant`, `mark_read`. Authenticate with a
bearer token of an account:

```sh
# Sign in once and make a token (shown only in this response).
curl -c jar -d address=me@rsntr.net -d password=... https://rsntr.net/auth/signin
curl -b jar -H 'content-type: application/json' \
  -d '{"name":"my agent","scopes":["read","write"]}' https://rsntr.net/auth/tokens
```

Then add the server to your MCP client, for example:

```json
{ "mcpServers": { "resonator": {
    "type": "http", "url": "https://rsntr.net/mcp",
    "headers": { "Authorization": "Bearer <token>" } } } }
```

A token with only the `read` scope sees only the read tools.

**Robots** (the Wave robot / Data API). `POST /robot/register` with
`{"name": "echo", "url": "https://bot.example/hook"}` (signed in, or with
a bearer token) makes the robot `echo@<domain>` and returns its token and
webhook secret (shown once). Events arrive as HMAC-signed webhooks, or without a URL from the
event stream `GET /robot/events`; the robot acts through
`POST /robot/dataapi` and `/robot/rpc`.

Agents that write code in this repository: read [AGENTS.md](AGENTS.md).

## Federation

Off by default; with it off nothing of it runs. When enabled, the server
gets an Ed25519 key, signs its deltas, and talks to other resonator servers
over iroh (QUIC). Waves stay on the server where they were created (the
host); adding `bob@other.example` sends the wave to that server, which keeps
a replica, serves it to its users (search, the socket, agents) and forwards
their edits back to the host.

Setting it up:

1. **Key.** `resonator federation key` creates the key (`federation.key` in
   the data directory) if there is none and prints it with the DNS record to
   publish.
2. **DNS.** Publish that TXT record (`_resonator.<domain>`), which binds the
   wave domain to the key. A server whose domain *is* its 52-character key
   (a key domain) needs no record.
3. **Who.** `mode = "open"` (the default) federates with any server: one
   whose key is bound to its domain, and, for a domain that publishes no
   record (a bare host name, say), the first key that claims it.
   `mode = "allowlist"` federates with the domains in `allow` only, and
   needs the binding. Keys are pinned on first contact; a key that changes
   without a signed rotation is quarantined until an admin decides.
4. **Reaching each other.** By default iroh's public relays and DNS
   address lookup (`relay = "default"`, `discovery = "default"`) find peers
   by key. For servers at fixed addresses, list them as static peers and
   set `discovery = "off"` (and `relay = "off"` if they reach each other
   directly). `listen` fixes the UDP port the endpoint binds.

```toml
[federation]
enabled = true
mode = "open"               # or "allowlist", with allow = ["other.example"]
relay = "default"           # "off", or the URL of your own relay
discovery = "default"       # "off" for static peers only
listen = "0.0.0.0:7777"     # UDP; empty for any port
attachment_cache_mb = 1024  # other servers' attachments kept here

# Optional: a peer at a fixed address (no discovery needed).
[[federation.peers]]
domain = "other.example"
key = "<52-character key>"
addr = "203.0.113.7:7777"
```

Keys seen for other domains: `resonator federation peers` lists them with
their status (trusted, retired, quarantined, denied);
`resonator federation peers approve <domain|key>` trusts a domain's
quarantined key (or a key by name), `... deny <domain|key>` refuses one. The
command works on the database and is safe while the server runs; a decision
applies to the next connection with that server.

Attachments stay on the server they were uploaded to; their ids name its
domain. Other servers fetch them when one of their users opens them, if
that server has a participant on the wave, check size and SHA-256, and keep
a copy in a cache of `attachment_cache_mb` (least recently used dropped
first; not counted against anyone's quota). While the origin is unreachable,
cached copies are still served; others show as unavailable and are asked
for again later.

In person mode (one person's phone, tablet and computer sharing a domain),
an attachment uploaded on one device is copied to the person's other
devices right away, and to a device that was away when it next syncs, so
the phone can show an image uploaded on a tablet that has since gone to
sleep. The copies count against `attachment_cache_mb`; a device without
room skips them, and attachments over `replicate_max_mb` are fetched when
opened, as before.

```toml
[federation]
replicate_own_attachments = true  # person mode only
replicate_max_mb = 25             # larger attachments stay on demand
```

In the client, the add-participant dialog checks a full address of another
domain with that server before it is added (`GET /federation/probe`), and
participants of other servers carry a small domain badge.

## AI agents

```toml
[ai.providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[[ai.agents]]
name = "assistant"                 # assistant@<domain>
provider = "openrouter"
model = "anthropic/claude-sonnet-4.5"
```

An agent answers when it is added to a wave or when one of its blips is
replied to. Wave content is sent to the configured provider only then.
It answers as soon as the person it is waiting for finishes editing (the
web client signals that when an edit session that changed something ends);
otherwise, after `ai.reply_debounce_ms` (default 10 s) without edits.

## Wave apps

Small programs that live in a wave: a gadget in a blip names a bundle (a
WebAssembly component), and each participant's own node runs it. The
browser only draws what the app renders, in a closed shadow root; the
instances share messages through a hidden app wavelet and talk to each
other directly over iroh. Off by default on a server, on in device mode.

```toml
[apps]
enabled = true
```

In a wave, Edit, then Insert app, and pick a bundle zip; each participant
allows it once, after seeing what it may do (post to the wave, keep data,
talk to peers, open streams, use the microphone or camera). Examples and a bundle
script are in [`examples/wave-apps`](examples/wave-apps); the design is in
[`docs/wave-apps.md`](docs/wave-apps.md).

## Apps

The iOS and Android apps are WebView wrappers around a node that runs on
the device (`crates/resonator-ffi`, UniFFI bindings for Swift and
Kotlin). The node keeps the person's waves, syncs their devices and
federates with everyone else; a small push relay (`resonator-push`) wakes
sleeping phones with content-free silent pushes. See
[`crates/resonator-ffi/README.md`](crates/resonator-ffi/README.md),
[`docs/device-plan.md`](docs/device-plan.md) and
[`docs/push-relay.md`](docs/push-relay.md).

The desktop app (`apps/desktop`, Tauri) does the same on macOS, Windows
and Linux, in the system's WebView.

```sh
apps/desktop/build.sh        # Rust, Node; on Linux the WebKitGTK dev packages
apps/desktop/build.sh release  # installers for this OS (.dmg, .msi/.exe, .deb/.rpm/.AppImage)
apps/ios/build.sh run        # macOS with Xcode and xcodegen: simulator
apps/android/build.sh        # Android SDK + NDK and cargo-ndk: debug APK
apps/android/build.sh release  # release APK + AAB, signed if a keystore is set
apps/macos/build.sh          # a Mac window on a node of this checkout (development)
```

The official builds use the relays `https://rly.rsntr.net` (iroh) and
`https://psh.rsntr.net` (push); both are build settings
(`RESONATOR_IROH_RELAY`, `RESONATOR_PUSH_RELAY`).

Android release signing reads `apps/android/keystore.properties`
(`storeFile`, `storePassword`, `keyAlias`, `keyPassword`; git-ignored) or
the environment (`ANDROID_KEYSTORE_PATH`, `ANDROID_KEYSTORE_PASSWORD`,
`ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`). Forks: change the
`applicationId` and the name (see [TRADEMARKS.md](TRADEMARKS.md)).

Continuous integration (`.github/workflows/ci.yml`) builds and tests
everything on each push. Pushing a `v*` tag runs `release.yml`: the server
binaries, `rsntr`, the desktop installers, the signed APK and AAB, and the
iOS app to TestFlight, into a GitHub release.

## Development

Layout:

- `crates/resonator-model`: documents, operations, transform, conversation model.
- `crates/resonator-server`: storage, wave server, HTTP and WebSocket, auth, search, attachments.
- `crates/resonator-robots`: robot / Data API operations and events.
- `crates/resonator-agents`: AI agents, model connector, MCP, robot endpoints.
- `crates/resonator-federation`: server keys, trust, signing, the iroh endpoint, hosting and replicating wavelets, person mode.
- `crates/resonator-node`, `crates/resonator-ffi`: the embeddable node and its Swift/Kotlin interface.
- `crates/resonator-push`: the push relay.
- `crates/resonator`: the `resonator` binary.
- `client/`: the web client (Preact, TypeScript, Vite).
- `apps/`: iOS, Android and macOS wrappers.
- `deploy/`: how the official servers are set up.
- `docs/`: design documents.

Tests:

```sh
cargo test --workspace
(cd client && npm ci && npm test && npx playwright install chromium && npm run test:e2e)
```

Contributions are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).
Security reports: [SECURITY.md](SECURITY.md).

## What it keeps from Wave

- The operational transform and document model, ported from the Java
  sources and checked against output of the original code (golden files
  under `crates/*/testdata`).
- Waves, wavelets, blips, threads, inline replies, per-user read state.
- IDs, deltas and the hashed version chain.
- The client wire protocol and the robot / Data API operation set.

| Original | Resonator |
|---|---|
| Java, Jetty, Guice | Rust, axum, tokio, one binary |
| Files or MongoDB; Lucene or Solr | SQLite, with FTS5 full-text search |
| GWT web client | TypeScript client embedded in the binary |
| JAAS, SHA digests, OAuth 1.0a | argon2id, sessions in SQLite, bearer tokens |
| Unsigned robot callbacks | HMAC-signed webhooks or an event stream |
| No emoji (BMP only) | Full Unicode |
| No AI | Built-in agents on any OpenAI-compatible API; MCP endpoint |
| XMPP federation | Federation over iroh (QUIC) with Ed25519-signed deltas |
| One server per user | Optional person mode: a node on every device, no central server |
| Gadgets (JavaScript in the browser) | Wave apps: WebAssembly on each participant's node, drawn in a shadow root |

Several defects of the original were fixed rather than ported; they are
marked in the code with "Deliberate fix" or listed as deviations in module
documentation.

## License and trademark

The code is licensed under the [Apache License 2.0](LICENSE); see
[NOTICE](NOTICE) for attribution to Apache Wave.

"Resonator" and its logo are trademarks of Resonator LLC. You may run,
change and redistribute the code, but a modified version must use another
name and icon, and only Resonator LLC publishes the Resonator apps. See
[TRADEMARKS.md](TRADEMARKS.md).
