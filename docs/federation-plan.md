# Resonator federation over iroh

## Context
Federation was deferred in resonator, but its seams were kept:
- `WaveletHost::{Local, Remote}`, marked "FEDERATION SEAM" (`crates/resonator-server/src/waveserver/mod.rs:154`)
- the unsigned `ProtocolSignedDelta` built at `mod.rs:301`
- `SubmitError::NotLocal`, `get_applied_history`, `container.is_delta_signer`
- the ported federation protos (`crates/resonator-model/src/proto/federation.rs`)

Rafael wants servers to federate with two kinds of address:
- `user@key`: the domain part *is* the server's public key.
- `user@domain`: a DNS record on the domain points to the key.

**Decision: use iroh directly, with no dependency on the earlier Resonator code (rsntr).** This project (formerly Wavenator, now Resonator) does not reuse that stack. The earlier code was studied for ideas only (per-request streams, a durable outbox, deduplication by request id); none of its crates, record names or wire formats are used.

**Choices made (so far):**
- both address forms
- allowlist by default
- own ALPN with protobuf frames

### Why iroh
- **Identity:** an endpoint's identity is an Ed25519 key (`EndpointId`). QUIC with TLS 1.3 and raw public keys means both sides are proven by the handshake, with no CA.
- **Connectivity:** hole punching with relay fallback, and dialing by key through pkarr/DNS lookup.
- **Firewall:** it only needs *outbound* UDP/HTTPS, so mothership's firewall stays closed.
- **Same stack:** it is tokio-based, like resonator.

### Wave's federation model (unchanged — we keep it)
**Hosting:**
- The wavelet id's domain is the single host. It orders and transforms deltas and owns the hash chain.
- Version 0 hashes the `wave://domain/...` URI, so **a wavelet can never move host**. That makes the stability of the domain string critical.

**Operations, host side:**
- `submitRequest(signed delta)` → (ops applied, hashed version, time)
- `requestHistory(name, from, to, limit)` → applied-delta bytes
- `getDeltaSignerInfo` and `postSignerInfo`

**Operations, remote side** (they must be acknowledged and retried):
- `waveletDeltaUpdate`
- `waveletCommitUpdate`

**Remotes:**
- They keep read-only replicas.
- They check signatures and the hash chain, fill gaps with `requestHistory`, and may not create a wavelet at version 0 on someone else's host.

## Recommended design

### 1. Identity: one rule for both address forms
**Server key:**
- Every server has one Ed25519 server key, generated on first start and stored next to the database (mode 600).
- It is both the iroh endpoint's secret key and the delta-signing key.
- **Key domain:** the key's z-base32 form, 52 characters. This is the encoding iroh/pkarr use, it is one valid DNS label, and it already passes `is_valid_domain` (lower-case `a-z0-9`).

**Domain part of an address = the name of a server, which resolves to a key:**
- **`user@<52-char z-base32>`** is self-certifying: no DNS and no CA. A domain part is recognised as a key domain when it is a single label that decodes to exactly 32 bytes.
  - It suits small or home servers with no domain.
  - It can never rotate keys: a new key means new addresses and new wavelet ids.
- **`user@example.com`** resolves through a TXT record: `_resonator.example.com. TXT "v=wn1; k=<z32 key>[; k=<next key>]"`.
  - It uses the `hickory-resolver` crate (a DNS resolver) and validates DNSSEC when the zone is signed.
  - The record may list several keys, which allows rollover.
  - Because the domain is the stable name, the key can rotate under it.
  - The earlier Resonator code also used `_resonator`; its records are not compatible with these.

**Trust for DNS domains:**
- TOFU-pin domain→key in a new table, `federation_peers(domain, key, first_seen, last_verified, status)`.
- A key change is accepted automatically only when:
  - the new key is in the TXT record, **and**
  - the old key has signed a handover statement, `KeyRotation{domain, old, new, at}`, sent in the Hello.
- Otherwise the peer is quarantined and an admin approves it.

**Binding rule:**
- The QUIC handshake proves the peer's key.
- A request is only authorised for domain D if D resolves (or is pinned) to that key.
- So `@example.com` deltas can only come from example.com's key.

### 2. Transport and wire: iroh, ALPN `resonator/fed/1`
**Endpoint:**
- New crate `crates/resonator-federation` depends on `iroh` directly (current 1.x; check the version and API on docs.rs during the spike).
- It builds the endpoint from the server secret key and registers ALPN `resonator/fed/1`.
- It runs an accept loop and keeps a connection cache per peer key.

**Finding peers:**
- The endpoint publishes its addresses through iroh's pkarr/DNS lookup, so a peer can dial it by key alone.
- `relay = "default" | "<own relay url>"` and `discovery = "default" | "off"` in config. "Off" means static addresses only; this is also what the tests use.

**Choreography:**
- The first bi stream carries a `Hello`: protocol version, server domain(s), capabilities, and any `KeyRotation` statements.
- After that, each request uses a fresh bi stream: the dialer writes and finishes, the acceptor writes its response.

**Frames:**
- A u32 length followed by one protobuf `FederationFrame { oneof request/response }`, in a new `proto/federation_rpc.rs` next to the ported protos.
- Applied-delta bytes travel as raw `bytes` fields, exact as the hash chain requires.
- Frames are capped at 4 MiB; `history` pages by the existing `lengthLimit`.

**Requests and responses:**
- Each request carries a ULID used as its idempotency key. The receiver keeps a `federation_applied(ulid)` table, pruned after 30 days.
- Responses are `Ok` with the op's result, or `Error` with Wave's federation error codes. Port the missing `federation_error.protodevel` for this.

**Ops:**
- `submit`, `history`, `signer-info`, `post-signer-info`
- `update` (applied deltas) and `commit`, sent host→remote
- `probe`, used by the UI to check that a remote address exists

- `fetch-attachment`: see section 7.

### 3. Signing
- Add `SignatureAlgorithm::Ed25519` to `ProtocolSignature`.
- `ProtocolSignerInfo` carries the domain, the raw key in place of the certificate chain, and the rotation statements. The signer id is the SHA-256 of the key.
- Use the `ed25519-dalek` crate, which iroh's keys already build on.
- Sign at the single seam, `waveserver/mod.rs:301`. Verify on incoming `submit` and `update`: one signature per domain in the address path, as Java's `CertificateManagerImpl.verifyDelta` does.
- Store signer info in a new `signer_info(signer_id, domain, key, info)` table, so the host can answer `signer-info` for old deltas after the key rotates.

### 4. Host side
- Handle `submit` → `container.submit_request` (`waveserver/container.rs:245`) after these checks:
  - signature
  - author domain bound to the peer key
  - author is a participant
  - version ≠ 0
- Handle `history` → `get_applied_history`, **only if the requesting domain has a participant**. This fixes a gap in the Java original.
- **Fan-out:**
  - On `WaveBusEvent::Update`, notify the domains of current participants plus participants the delta removed, minus the local domain.
  - Notifications go through a durable `federation_outbox(domain, wavelet, from_version, ulid, attempts, next_at, status)`: exponential backoff, coalesced per wavelet, expiry.
  - An `Ok` from the remote is the acknowledgement.

### 5. Remote side
- `WaveletHost::Remote` gets a replica container. It reuses the `wavelets`/`deltas` tables plus a new `hosted_by` marker column, added in a new migration.
- **Replica flow:**
  - verify signatures and the hash chain (`proto/convert.rs:660-735`)
  - queue out-of-order deltas
  - fill gaps with `history`
  - hold commits until the deltas they cover arrive
- Local users' submits to remote wavelets are signed and sent as `submit`; the client gets the applied version back.
- Search, unread state, robots and agents then see replicas like local wavelets.
- An agent only answers on its own server, which is correct.

### 7. Attachments
**Today (`crates/resonator-server/src/attachment/mod.rs`):**
- Bytes are files on disk, with metadata in the `attachments` table: `wave_ref`, name, mime, size, creator, image and thumbnail sizes, malware flag, `sha256`.
- The id is `<domain>/<id>`, where the domain is the server it was uploaded to.
- A blip references it as `<image attachment="id">`.
- Whoever can read the attachment's wavelet can read it.
- Attachments are immutable: never overwritten.

**Federated:**
- **The attachment stays on its origin**, the domain in its id. That is the uploader's own server, even when the wave is hosted elsewhere. Upload is unchanged: Bob on B uploads to B, and the row's `wave_ref` names A's wavelet, which B holds as a replica.
- **Clients only ever talk to their own server.** `/attachment/<id>` and `/thumbnail/<id>` on X first check that the local user can read the wavelet. If the id's domain isn't X and X has no copy, X pulls it:
  - X sends `fetch-attachment{id, wavelet}` to the origin over iroh.
  - The origin serves it only if the row's `wave_ref` is that wavelet **and** the requesting domain has a participant on it. This is the same rule as `history`.
  - The response is a metadata frame, then the bytes streamed on that bi stream in chunks (not one 4 MiB frame), up to the origin's `max attachment size`.
- **Integrity:** X checks size and SHA-256 against the metadata before the file goes into place. It uses the same temp → fsync → rename path as an upload.
  - The hash is pinned on first fetch, and because attachments are immutable, a later different hash is rejected.
  - A hash attribute in the document would bind it from the start, but it would change the conversation schema, so it is left out of this milestone.
- **Thumbnails are generated locally** from the fetched bytes by the existing `thumbnail.rs`, with its decode limits. X never trusts an origin's thumbnail, and the malware hook runs as for uploads.
- **Cache:**
  - Copies are marked by a new `origin` column (migration 7).
  - They count against a separate `federation.attachment_cache_mb` budget with LRU eviction, not against the user's quota, because the origin keeps the master copy.
  - The pull is lazy (on first view); concurrent requests for the same id share one fetch.
- **Failure:**
  - If the origin is offline, the client gets a 503 and shows an "unavailable from <domain>" placeholder, then retries later.
  - A cached copy keeps working while the origin is down. Access is always checked against X's own replica's participants.

### 8. Policy and operations
- **Config:**

  ```toml
  [federation]
  enabled = false
  mode = "allowlist" | "open"   # default: allowlist
  allow = ["example.com", "<z32>"]
  relay = "default" | "<url>"
  discovery = "default" | "off"
  ```

- **CLI:**
  - `resonator federation key` prints the key and the TXT record to publish.
  - `resonator federation peers` lists peers and lets an admin approve or deny one.
- **UI:**
  - The add-participant dialog accepts remote addresses. It checks them with `probe` and marks them "remote".
  - The directory stays local-only.

## Phases
1. **Spike.** Two in-process endpoints with discovery off exchange a Hello and an echo request. This checks:
   - the iroh version and its API
   - tokio and rustls compatibility
   - how much iroh adds to the binary size, measured
2. **Identity.** Server key, z32 key domains, TXT resolver, `federation_peers` pinning and rotation, binding check.
3. **Signing.** Ed25519 signatures, the signer-info store, verify-on-receive. Local behaviour is unchanged.
4. **Host.** The `submit`/`history`/`signer-info` handlers and the federation outbox fan-out.
5. **Remote replica.** The container, gap fill, commits, and forwarding of client submits.
6. **Attachments.** `fetch-attachment`, cache, local thumbnails.
7. **Product.** Config, CLI, UI for remote participants, search and agents over replicas.

## Critical files
- `crates/resonator-server/src/waveserver/mod.rs` (`WaveletHost`, `submit`, the signing seam), `container.rs`, `bus.rs`
- `crates/resonator-model/src/proto/federation.rs` (Ed25519 algorithm), new `proto/federation_rpc.rs`, `proto/convert.rs`, `id/participant.rs`, `id/identifiers.rs`
- `crates/resonator-server/src/storage/migrations.rs`: migration 7 adds `federation_peers`, `signer_info`, `federation_outbox`, `federation_applied` and `wavelets.hosted_by`
- New: `crates/resonator-federation` (iroh endpoint, resolver, codec and ops, outbox worker, replica), mounted from `crates/resonator/src/main.rs` like `Integrations::mount`
- Java reference for behaviour: `reference/apache-wave/.../WaveServerImpl.java`, `RemoteWaveletContainerImpl.java`, `CertificateManagerImpl.java`, `WaveletNotificationDispatcher.java`

## Verification
- Unit tests:
  - z32 key-domain parsing
  - TXT parsing and rotation rules
  - Ed25519 sign/verify of applied deltas
  - frame codec limits
  - outbox backoff
- Golden test: an applied delta signed, serialised, re-parsed, and the hash chain still matching.
- Two-server integration tests (in process, iroh discovery off, direct addresses):
  - alice@A adds bob@B
  - both edit concurrently and converge
  - B goes offline, then catches up through the outbox and `history`
  - a forged-domain submit is denied
  - a non-participant domain's `history` is denied
  - a key rotation with and without a handover statement
  - an allowlist refusal
  - bob@B's image shows for alice@A
  - a tampered hash is rejected
  - a non-participant domain's fetch is denied
  - origin offline: a cached copy is served, and an uncached one returns 503
- Manual: a second instance on mothership (another port and data dir) federating with the trial server through `user@<z32>`. Then a real domain with a `_resonator` TXT record.
