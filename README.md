# Resonator

Real-time threaded conversations with live collaborative editing, in one
binary with SQLite storage. AI agents take part as ordinary participants.

Resonator is inspired by Google Wave and derived from Apache Wave ("Wave in
a Box"). It is a complete rewrite in Rust and TypeScript, licensed under
Apache-2.0; see `NOTICE` for attribution.

## What it keeps from Wave

- The operational transform and document model, ported from the Java
  sources and checked against output of the original code (golden files
  under `crates/*/testdata`).
- Waves, wavelets, blips, threads, inline replies, per-user read state.
- IDs, deltas and the hashed version chain.
- The client wire protocol and the robot / Data API operation set.

## What is different

| Original | Resonator |
|---|---|
| Java, Jetty, Guice | Rust, axum, tokio, one binary |
| Files or MongoDB; Lucene or Solr | SQLite, with FTS5 full-text search |
| GWT web client | TypeScript client embedded in the binary |
| JAAS, SHA digests, OAuth 1.0a | argon2id, sessions in SQLite, bearer tokens |
| Unsigned robot callbacks | HMAC-signed webhooks or an event stream |
| No emoji (BMP only) | Full Unicode |
| No AI | Built-in agents on any OpenAI-compatible API; MCP endpoint |
| XMPP federation | Server-to-server federation over iroh (QUIC) with Ed25519-signed deltas; off by default (`[federation]`) |
| Gadgets | Not included |

Several defects of the original were fixed rather than ported; they are
marked in the code with "Deliberate fix" or listed as deviations in module
documentation.

## Layout

- `crates/resonator-model` — documents, operations, transform, conversation model.
- `crates/resonator-server` — storage, wave server, HTTP and WebSocket, auth, search, attachments.
- `crates/resonator-robots` — robot / Data API operations and events.
- `crates/resonator-agents` — AI agents, model connector, MCP, robot endpoints.
- `crates/resonator-federation` — server-to-server federation: server keys, trust, signing, the iroh endpoint, hosting and replicating wavelets across servers.
- `crates/resonator` — the `resonator` binary.
- `client/` — the web client.

## Build and run

```sh
(cd client && npm ci && npm run build)   # embedded into the binary
cargo build --release -p resonator
./target/release/resonator --domain example.com --listen 0.0.0.0:9898 serve
```

Settings come from a TOML file (`--config`), `RESONATOR_*` environment
variables and flags; `resonator config` prints the effective configuration.
`resonator backup <path>` copies the database safely while the server runs.

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

## Tests

```sh
cargo test --workspace
(cd client && npm test && npx playwright test)
```
