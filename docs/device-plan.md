# Resonator: devices as nodes (person key + device keys, host lease, rewind)

## Context
Resonator federation is built: an iroh endpoint and the `resonator-federation` crate, with host and replica roles, an outbox, attachments and trust. Its plan is in `docs/federation-plan.md`.

Rafael now wants **no central server**. Every phone or computer runs a resonator node, and the iOS and Android apps are WebView wrappers around it. Decisions so far:
- **Ordering:** keep Wave's OT, where one host orders each wave, plus an optional always-on node ("anchor").
- **Identity:** a person key plus device keys, and a person's devices sync with each other.
- **Waking phones:** a tiny stateless push relay. This is a separate, later track.
- **Dead host:** handled by host leases with epochs, rewind and rebase on a fork, takeover, revocation, recovery, fork-as-new-wave and co-host delegation.

**Intended outcome:**
- A wave belongs to a *person* (`name@<z32 person key>` or `name@domain`). The host role moves between that person's devices without changing any wave id.
- A dead or returning host never loses edits; at worst they are delayed and rebased.
- The node embeds in mobile apps.

Today's multi-user "server mode" (a domain with many accounts) keeps working unchanged. Person mode is added beside it.

## Findings that shape the plan
- **No rewind exists.** `DeltaStore::delete` (`storage/delta_store.rs:419`) removes only a whole wavelet. Writes are append-only.
  - The search upsert refuses lower versions (`search/index.rs:262-275`); startup `stale_wavelets` (`:446`) is the only repair.
  - The client throws `NOT_RECOVERABLE` (`client/src/concurrencycontrol/client/concurrencyControl.ts:225`) when the server's history no longer contains its version.
- **Hosting is one domain per server:** `host_of` is `wavelet.domain() == options.domain` (`waveserver/mod.rs:338`).
- **Trust allows one trusted key per domain:** a unique partial index on `federation_peers` (migration 7). Signer verification maps a signer to a single `{domain, key}` (`signing.rs:148`, `trust.rs:280`).
- **Reusable pieces:**
  - `RemoteHost` forwarding (`mod.rs:227,364`)
  - `apply_replica_delta` with its `Ahead`/`AlreadyApplied`/`InvalidHash` results (`container.rs:520`)
  - `transform_submitted_delta` (`resonator-model/src/operation/transform.rs:240`)
  - the outbox, history paging, and Hello authorisation (`endpoint.rs:794,876`)
  - rotation statements (`rotation.rs`)
  - the in-process app API (`resonator-server/src/app.rs`: `prepare_with`, `serve_until`)
- **The `resonator` crate is bin-only.** The assembly code in `main.rs:118-173` must move into a library before it can be embedded.

## Design

### A. Epochs and rewind (core; build first)
- **Epoch on every applied delta:**
  - Each applied delta carries the host epoch it was ordered under: a new optional field on `ProtocolAppliedWaveletDelta`, absent = 0. It sits inside the hashed bytes, so new deltas are bound to their epoch, and old deltas hash exactly as before.
  - A new table, `wavelet_epochs(wavelet, epoch, start_version, start_hash, statement)`, records each epoch's history.
  - A delta at version v is valid only under the epoch active at v.
- **Rewind:** `DeltaStore::truncate_from(wavelet, v)` deletes the deltas and snapshots at version v or later in one transaction. The container then reloads (a new `WaveMap::invalidate(wavelet)`) and derived state is reset:
  - a forced search reindex (a `reindex_wavelet` that bypasses the `>=` guard, including `search_text`, `search_participants` and `search_archive`)
  - `federation_outbox.from_version` capped to v
  - `agent_triggers` left as they are (the fingerprint logic re-answers if text changed)
- **Orphans:** deltas after the fork point keep their *signed originals*. On rewind, every node that held them resubmits them to the current host.
  - The host deduplicates by the SHA-256 of the signed original (a new `submitted_originals` index), so orphans arriving from several nodes apply once.
  - Authors' signatures stay valid, so nothing is re-signed.
- **Clients:** a new `WaveletReset{version}` message on the WebSocket. The client re-opens the wavelet and keeps its unacknowledged and queued ops, transforming them against the new history (a reconnect path with a reset baseline in `concurrencyControl.ts`), instead of throwing `NOT_RECOVERABLE`.

### B. Person identity
- **Keys:**
  - A person key (Ed25519) is derived from a 24-word recovery phrase (BIP39 → HKDF).
  - Each device key is the device's iroh key.
  - `DeviceCert{person, device, name, issued_at}` is signed by the person key.
  - `RevocationList{person, seq, revoked[]}` is signed by the person key, has a monotonic `seq`, and spreads in the Hello and during sync.
- **Domains:** a person domain is `<z32 person key>`, or a DNS domain whose `_resonator` TXT `k=` lists the person key.
- **Trust:**
  - `Trust::check` accepts a peer key for domain D if it is pinned or listed directly (server mode), **or** it carries an unrevoked `DeviceCert` from D's person key.
  - Certs and revocations go in new tables, `device_certs` and `revocations`. The one-trusted-key index stays for server-mode pins only.
  - `Hello` gains `device_cert` and `revocation_seq`.
  - Signer verification accepts a device key with a valid cert for the delta's domain.
- **Storage of the person key (decided):**
  - It lives in the OS keychain (or a mode-600 file on desktop/anchor) only on devices the person marks as **admin**, and in the 24-word recovery phrase.
  - Non-admin devices hold only their own device key. A stolen non-admin device is handled by revoking it.
  - Only admin devices can pair, revoke or co-host.
  - The person key goes over the network only when making a new admin device, end-to-end inside the pairing channel, and only with an explicit confirmation.

### C. Host lease and device sync
- **Lease:**
  - Granularity is one lease per person, covering all of that person's waves, not one per wave.
  - `HostLease{person, epoch, device, expires_at, prev}` is signed by the holder device.
  - `WaveletHost` gains `Delegated{holder}`: my wave, but hosted by another of my devices. Local submits go through the existing `RemoteHost` forwarding to the holder.
- **Lease rules:**
  - The anchor, if configured, is preferred: when it is reachable, others hand over to it.
  - The holder renews to all reachable own devices: anchor 10 min, other devices 2 min.
  - Takeover only happens:
    - with a signed handover (for example, the app going to the background hands over to the anchor), **or**
    - after expiry, and only by a device that is itself online (it reaches the relay or another peer). An offline phone never takes over.
  - A takeover starts epoch + 1 at each wavelet's last known version and hash.
- **Sync among own devices:**
  - A new op `sync-summary{wavelet → (version, hash, epoch)}` is exchanged on connect and periodically. Missing ranges are pulled with the existing `history` op.
  - This covers my hosted waves, replicas of other people's waves, and my user-data (read state) wavelets.
  - Other people's hosts fan out to any reachable device of mine, using `fan_out` with the person's device set; my devices then spread it further through sync.
- **Pending queue:**
  - When the host is unreachable, local submits are stored durably in a new `pending_submits` table and acknowledged to the client as "pending". The client keeps them as unacknowledged ops.
  - They are sent when the host returns. This applies to my waves on a non-host device and to other people's waves.
  - It replaces today's 10 s failure.

### D. Device mode and embedding
- **Device mode:** `mode = "device"` in config.
  - One person account is created automatically and has no password.
  - Registration and the server directory are off.
  - A loopback token is produced at start. `GET /?token=` (loopback peer only) mints the session cookie, so the WebSocket `socket_token` flow keeps working.
  - The wrapper's origin is added to the allowed origins.
- **Contacts** replace the directory in device mode: everyone you share waves with, plus anyone added by address or QR. The add-participant and @mention suggestions read from it.
- **Embedding library:**
  - A new crate `crates/resonator-node` exposes `start(config) -> Handle`, `Handle::{stop, local_url, token, enter_background, enter_foreground}`, built from `main.rs:118-173`.
  - The `resonator` binary becomes a thin user of it.
  - `crates/resonator-ffi` uses UniFFI for Swift and Kotlin.
  - `enter_background` hands the lease over and flushes; `enter_foreground` resyncs.

### E. Pairing, recovery and the ownerless-wave cases
- **Pairing:** an existing device shows a QR code or link containing `{person, device ticket, one-time 128-bit secret}`. The new device dials over iroh, proves the secret, receives a `DeviceCert`, and starts syncing.
- **Recovery:** entering the phrase on a new device restores the person key. It issues a new cert and revocation list and pulls shared waves from other participants. Their copies are verified by signatures and the hash chain.
- **Settings:** device list, revoke, recovery phrase, anchor.
- **Fork as new wave:** any participant can continue a frozen wave as a new wave they own. The history is copied in as a snapshot plus a `continued-from` link.
- **Co-host:** `CoHost{person, delegate_domain, until}` is signed by the person. It lets the delegate's devices take the lease for that person's waves, for example a company anchor hosting employees' team waves.

## Phases (agent runs)
1. **Epochs and rewind** (A). This is the risky core and is built first with a forked two-host test. Server mode only; no person keys needed yet.
2. **Person identity** (B): person key, certs, revocation, trust and signer changes, Hello fields, `name@<person key>` addresses.
3. **Lease, sync and pending queue** (C).
4. **Device mode and embedding** (D): auth via the loopback token, contacts, `resonator-node`, `resonator-ffi`, lifecycle hooks.
5. **Pairing, recovery, fork-as-new-wave and co-host** (E), with the settings UI.
6. Separate tracks, not in this plan: the push relay binary, and the iOS/Android wrapper projects (Xcode/Gradle around `resonator-ffi`).

Each run follows the established rules:
- federation and person mode are off by default, so nothing changes for existing setups
- new migrations only (10 and up)
- `CARGO_INCREMENTAL=0`; no release build of `resonator` while the trial server runs
- all suites stay green

## Critical files
- `crates/resonator-model/src/proto/federation.rs`: the epoch field on applied deltas
- `crates/resonator-model/src/proto/federation_rpc.rs`: Hello fields, sync-summary, lease, pairing messages
- `crates/resonator-server/src/storage/{delta_store.rs, migrations.rs, federation_store.rs}`: truncate, new tables
- `crates/resonator-server/src/waveserver/{mod.rs, container.rs, wave_map.rs}`: `Delegated` host, rewind, invalidate, pending queue
- `crates/resonator-server/src/search/index.rs`: forced reindex
- `crates/resonator-server/src/auth/mod.rs` and `http/mod.rs`: device credential, loopback token
- `crates/resonator-federation/src/{trust.rs, signing.rs, endpoint.rs, outbox.rs, replica.rs, remote.rs}`; new `lease.rs`, `sync.rs`, `person.rs`, `pairing.rs`
- `client/src/concurrencycontrol/client/concurrencyControl.ts`: reset and rebase instead of `NOT_RECOVERABLE`
- New crates: `crates/resonator-node`, `crates/resonator-ffi`
- `crates/resonator/src/main.rs`: becomes thin

## Verification
- **Phase 1:**
  - Three in-process nodes. Host H1 orders deltas, then is cut off. H2 takes over at epoch 2. Both apply concurrent edits. H1 returns.
  - Assert:
    - all nodes converge to the same text and hash chain
    - every edit from both sides is present exactly once
    - clients connected to H1 get a reset and keep their unsent ops
    - search and the outbox are correct after the rewind
  - Run it repeatedly (at least 3 times) for flakiness.
- **Phase 2:**
  - Certs accepted and revoked.
  - A revoked device's signatures are refused after the revocation `seq`.
  - Two people, each with two devices, exchange waves.
- **Phase 3:**
  - Lease handover, expiry takeover, and no takeover while offline.
  - The pending queue drains after the host returns.
  - Read state syncs between one's own devices.
  - The anchor is preferred.
- **Phase 4:**
  - Device-mode start via the library API in a test.
  - The loopback token signs in; a non-loopback request is refused.
  - Playwright runs against a device-mode node.
- **Phase 5:**
  - Pair by token, recover from the phrase on a fresh node and pull shared waves, revoke a device.
  - A fork-as-new-wave keeps its history, and a co-host takes over a frozen wave.
- **Manual:** three nodes on mothership in device mode (anchor plus two "devices") on separate ports bound to 0.0.0.0, exercised from Rafael's browser.
