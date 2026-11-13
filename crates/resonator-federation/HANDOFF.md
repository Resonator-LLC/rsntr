# resonator-federation: handoff after phases 1–7

Plan: `docs/federation-plan.md`. Built: phase 1 (transport spike), phase 2 (identity and
trust), phase 3 (delta signing), phase 4 (host side: `submit`, `history`, `signer-info`,
`post-signer-info`, fan-out through a durable outbox), phase 5 (remote replicas fed by
`update`/`commit`, local submissions forwarded to the host), phase 6 (attachments:
`fetch-attachment`, a cache of other servers' attachments) and phase 7 (the `resonator federation
peers` CLI, `GET /federation/probe`, remote participants in the client). The crate uses `iroh`
1.4.1 straight from crates.io and nothing else.

## What exists

| Module | Contents |
|---|---|
| `key` | `ServerKey`: an Ed25519 key in `<data_dir>/federation.key` (64 hex chars, created with mode 600, written atomically through a temp file and a hard link). Key domains: the 52-char z-base-32 form of a key. Also `key_domain`, `parse_key_domain` (which rejects non-canonical encodings), `is_key_domain`, and `signer_id` (the SHA-256 of the key). |
| `dns` | The `_resonator.<domain>` TXT record `v=wn1; k=<z32>[; k=<z32>]`. Covers `record_text`, `zone_line`, `parse_record`, `lookup_keys`, the `TxtResolver` trait, `HickoryTxtResolver` (system resolver; NXDOMAIN or no records counts as an empty answer) and `StaticTxtResolver` (a fake for tests). |
| `rotation` | `sign_rotation`, `verify_rotation` and `find_rotation` for `SignedKeyRotation`. The old key signs `KeyRotation{domain, old_key, new_key, at}`, prefixed with `resonator/key-rotation/1\0`. |
| `trust` | `Policy` (allowlist or open), `Authorisation` and `Trust`. TOFU pinning works over `federation_peers`. `check(peer, domain, rotations)`, `authorise(peer, domain) -> bool`, `resolve(domain)` (the key to dial), `is_signer_bound(key, domain)` (used for signature checks; it accepts retired keys) and `decide(domain, key, approve)` (the admin's decision). |
| `signing` | `Ed25519DeltaSigner` implements `resonator_server::waveserver::DeltaSigner`, and the signature covers `resonator/delta/1\0` followed by the delta bytes. Also `signer_info`, `parse_signer_info`, `signer_info_record`, `signing_domains`, `verify_delta` and `verify_applied_delta`. Verification needs exactly one signature per domain of the address path, plus the author's domain (as in Java's `verifyDelta`). |
| `codec` | Frames are a big-endian u32 length followed by a protobuf `FederationFrame`, at most 4 MiB. The length is checked before the body is read. |
| `endpoint` | `FederationEndpoint`: the ALPN `resonator/fed/1`, a Hello on the first bi stream, then one bi stream per request (the dialer writes and finishes, the acceptor answers). Requests carry a ULID id and have a timeout. Outbound connections are cached per peer key and redialled once if dead. The accept loop limits concurrent requests per connection and shuts down gracefully (`shutdown()`). Supporting types: `PeerAuthority` (implemented by `Trust`; `KeyDomainAuthority` for tests), `RequestHandler`, `EchoHandler`, `EndpointOptions` (`from_config`, `local()`) and `LocalIdentity`. |
| `service` | `FederationSetup::load(&Config)` returns `None` when federation is disabled; otherwise it loads or creates the key and refuses a key domain that belongs to another key. `with_tuning` and `with_resolver` replace the timings and the DNS resolver (tests). `delta_signer()` and `start(&WaveServer)` record our signer info, pin the static peers (`[[federation.peers]]`, see `StaticPeer`), start the endpoint with `Trust` and `ServerHandler`, give the wave server a `RemoteHost` (`remote::Forwarder`), and start the fan-out, the outbox worker and the pruning of old request ids. `ServerHandler` answers `probe`, `echo` and the wave operations; `submit`, `post-signer-info`, `update` and `commit` are answered once per request id (`federation_applied` keeps the encoded answer, and a retry gets it again). `Federation::{request, request_with_id, outbox, hold_outbox, shutdown}`. |
| `context` (private) | `Context`: what the operations share (wave server, `FederationStore`, `Trust`, endpoint, per-replica locks, signer info posted per peer key, outbox wake-up), `Tuning` (backoff 0.5 s doubling to 60 s, expiry 7 days, pages of 3 MiB / 1000 deltas, commit wait 10 s), wire/model conversions. |
| `host` | `submit` (wavelet hosted here; the peer speaks for the author's domain; not at version 0; signatures verify against stored signer info, unknown signer = `NOT_AUTHORIZED`; then `WaveServer::submit_remote_request`, which needs the author to be a participant). `history` (only if a domain the peer speaks for has a participant on the wavelet, else `ITEM_NOT_FOUND`; paged under the byte budget, `version_truncated_at` set when cut). `signer-info` (only if `is_delta_signer` at that version). `post-signer-info` (the peer speaks for the signer's domain and the key is the peer's or bound via `Trust::verify_signer`). |
| `replica` | `update` and `commit` from a host. Refused if the wavelet is hosted here or the peer does not speak for the wavelet's domain. Each applied delta: unknown signers fetched with `signer-info` and checked with `verify_signer`, then `verify_applied_delta`, then `WaveServer::apply_replica_delta` (hash chain check, transform of the original against the history since its target, operations count check). Deltas ahead of the replica are queued and the gap is filled with `history` pages; a `commit` is acknowledged once the replica holds that exact hashed version. |
| `outbox` | `fan_out` turns wave bus updates of hosted wavelets into `federation_outbox` rows for the remote domains of the participants after the deltas plus those the deltas removed (`domains_to_notify`). `worker` delivers due rows: `update` pages from the row's version, then waits for the local commit and sends `commit`. Success marks the row delivered (unless a newer notification arrived meanwhile); failure keeps what got through and retries with backoff; a row pending longer than the expiry is dropped. |
| `remote` | `Forwarder`, the `RemoteHost` of the wave server: posts our signer info to the host once per run and peer key, sends `submit`, maps errors onto `SubmitError::Remote` (and retries once after re-posting signer info on `NOT_AUTHORIZED`). `Prober`, the server's `RemoteProbe`: policy check, then `probe` to the address's server; reachable / allowed / exists. |
| `attachments` (private) | `fetch-attachment`. `serve` answers as the origin: the id's domain is ours, the row is not a cached copy, its `wave_ref` is the wavelet asked about, its size is within our `attachment_max_size`, and a domain the peer speaks for has a participant on the wavelet (`host::peer_participates`); one `ITEM_NOT_FOUND` for every refusal. The answer (`FetchAttachmentResult`: size, SHA-256, file name, uploader, time) is followed on the same stream by `Chunk` frames of 256 KiB and the end of the stream. `RemoteOrigin` (the store's `AttachmentOrigin`) dials the id's domain, checks the announced size against our own limit and streams the chunks into an `Upload`, refusing more bytes than announced. |
| `admin` | `resonator federation peers`: `format_peers` and `decide(store, <domain or key>, Approve/Deny)`. Approving a domain trusts its one quarantined key; denying a domain denies its trusted and quarantined keys; a key names itself (or its own key domain if never seen). Works on the database; safe while the server runs (WAL); applies from the next connection. |

The server and model crates hold the iroh-free parts:

- `resonator-model/src/proto/federation.rs`:
  - `SignatureAlgorithm::Ed25519 = 2`;
  - `ProtocolSignerInfo.ed25519_public_key` (tag 16) and `.key_rotation` (tag 17).
- `resonator-model/src/proto/federation_rpc.rs`:
  - `Hello`, `KeyRotation`, `SignedKeyRotation`, `Probe`, `ProbeResult`, `Echo`;
  - `FederationRequest`, `FederationResponse`, `FederationFrame`;
  - `PROTOCOL_VERSION = 1`.
- `federation_error.rs`: a port of `FederationError`.
- `federation_rpc.rs` (phase 4–5): `Submit`/`SubmitResult` (20), `History`/`HistoryResult` (21), `GetSignerInfo`/`SignerInfoResult` (22), `PostSignerInfo` (23), `Update` (24), `Commit` (25), answered with `Ack` (23) where there is nothing else to say. Phase 6: `FetchAttachment` (request 26) / `FetchAttachmentResult` (response 24), and `FederationFrame.chunk` (3) for bytes following a response. Tags up to 39 stay reserved.
- `resonator-server`:
  - `config::FederationConfig` (`[federation]`: enabled, mode, allow, relay, discovery, listen, request_timeout_secs, `[[federation.peers]]` with domain/key/addr) with `RESONATOR_FEDERATION_*` and `--federation-*` overrides;
  - `waveserver`: `RemoteHost` and `WaveServer::set_remote_host` (without one, a submit to a remote wavelet still fails with `NotLocal`); a remote submit returns once the replica has the delta back or after 10 s. `submit_remote_request`, `apply_replica_delta` (`ReplicaOutcome`), `current_version`, `hashed_version_at`, `is_delta_signer`; `SubmitError::Remote`. Replica containers are the ordinary containers loaded with `DeltaStore::open_replica` (`wavelets.hosted_by` set), so the bus, clients, search, robots and agents see them like local wavelets;
  - `FederationStore`: outbox (`outbox_enqueue` coalescing per domain and wavelet, `outbox_due`, `outbox_next_at`, `outbox_entries`, `outbox_delivered`, `outbox_retry`, `outbox_expire`) and applied requests (`applied_request`, `record_applied`, `prune_applied`);
  - `Config::federation_key_path()`;
  - `storage::FederationStore` (peers, signer info);
  - `waveserver::{DeltaSigner, SharedDeltaSigner}` and `WaveServerOptions::delta_signer` (default `None`, so output stays unsigned and byte-identical);
  - `app::start_with` and `App::prepare_with`.
- Migration 7:
  - `federation_peers`, with a unique partial index giving at most one `trusted` key per domain;
  - `signer_info`;
  - `federation_outbox (domain, wavelet, from_version, request_id, attempts, next_at, status)`;
  - `federation_applied (request_id, domain, applied_at, response)`;
  - `wavelets.hosted_by` (NULL means hosted here).
  - Migration 8 adds `federation_outbox.pending_since` (for expiry).
  - Migration 9 adds `attachments.origin` (the domain of a cached copy; NULL for uploads here), `last_used` (LRU order) and `evicted` (the files of a copy were dropped; the row stays and pins the SHA-256).
- Attachments (`resonator-server::attachment`): `AttachmentOrigin` (installed by federation with `set_origin`), `fetch_remote(id, wavelet)` with single-flight per (id, wavelet) and a 15 s back-off for an origin found unreachable, `finish_fetch` (size and hash against the announcement and against the pinned row, then the same `place` path as uploads: temp file, fsync, rename, thumbnail with the decode limits), `evict_over_budget` (`federation.attachment_cache_mb`, default 1024, LRU; cached copies are excluded from user quotas). The HTTP handlers take `?waveRef=` to name the wavelet for an attachment not here yet (checked with the caller's access first); 503 `unavailable from <domain>` if the origin cannot be reached, 502 if what it sent is rejected. With federation, uploads of ids naming another domain are refused (400). There is no malware scanner anywhere (`malware` is always NULL); fetched copies go through the same storing path as uploads, so a scanner added there covers both.
- HTTP: `GET /session` says `"federation": true` when enabled (the field is absent otherwise). `GET /federation/probe?address=` (signed-in users, 30 per minute each; 404 without federation) answers `{reachable, allowed, exists?, message?}` through the `RemoteProbe` that `FederationSetup::serve_app` installs.
- Client: with `federation` in the session, attachment ids are `<domain>/<token>` and requests for other domains' attachments carry `waveRef`; an attachment whose origin answers 503 shows "unavailable from <domain>" and is asked for again after 30 s. The add-participant dialog offers a full address of another domain with the probe's verdict (checking / reachable / not known there / unreachable / not allowed); the directory stays local. Participants of other domains have a small domain badge.
- The `resonator` binary:
  - `resonator federation key` prints the key, the key file and the TXT zone line;
  - `resonator federation peers [approve|deny <domain|key>]`;
  - `serve` with federation enabled signs deltas, subscribes to the wave bus before the HTTP server starts (`FederationSetup::subscribe`), joins the HTTP side (`serve_app`), starts the endpoint after the HTTP server, and stops it first.
  - `tests/federation.rs`: two or three servers in one process on 127.0.0.1 (static peers, no relay, no discovery, short backoff): replication to a remote participant's server (search and WebSocket included), remote submissions and concurrent edits converging, catch-up after the remote server was down and the host restarted, gap filling with `history`, tampered and badly signed deltas, the refusals (forged author domain, history without a participant, version 0, peer not on the allowlist, updates for wavelets hosted here or from a non-host), a duplicate request id applied once (also eight concurrent copies), deltas applied before `start` fanned out, attachments travelling from their origin (download and thumbnail on the other server, a non-participant domain refused, tampered bytes rejected, cached copies served and others 503 while the origin is down), the probe endpoint, and an agent on the remote server answering on a hosted wave. About 15 s.

## What comes next

- Rotation statements (`resonator federation rotate`), periodic pin refresh, DNSSEC (see below).
- An admin page over `FederationStore::peers()` and `Federation::outbox()` if the CLI is not enough.

## Measurements

- **Binary size**, measured in release (thin LTO, stripped) with temporary examples linking `resonator-server` (since deleted):
  - the server alone: 10.5 MB;
  - plus a bare iroh endpoint: 25.4 MB (+14.9 MB);
  - plus all of this crate (hickory, trust, signing): 27.7 MB (+17.2 MB in total).
  - The `portmapper` feature (UPnP, PCP and NAT-PMP) accounts for about 2.9 MB of that. Dropping it gives +14.3 MB.
  - The rest is iroh's relay client (its own reqwest 0.13 and websockets), the netwatch and netlink stack, noq (QUIC) and the n0 DNS resolver.
- **Shared dependencies** (single versions): tokio 1.54.1, rustls 0.23.46, ring 0.17, hyper 1.
- **Duplicates iroh adds**:
  - reqwest 0.12 and 0.13;
  - rand 0.9 and 0.10 (rand_core 0.6, 0.9 and 0.10);
  - getrandom 0.2, 0.3 and 0.4;
  - sha2 and digest 0.10 and 0.11;
  - netlink 0.8 and 0.9;
  - base64 0.22 and 0.23;
  - syn 2 and 3.
  - None of them conflict; they only cost build time and size.

## Open issues

- **Fan-out from the start (fixed).** `FederationSetup::subscribe` takes the wave bus before `App::serve`, so deltas applied before `start` are queued once the fan-out runs; at start, `outbox::reconcile` also re-queues delivered rows whose wavelet moved past what was delivered (deltas of a run that crashed before queuing them). Expired rows stay expired.
- **First update of a new replica.** The outbox row starts at the first delta of the bus event (the one adding the remote participant), so a new replica always fills the gap from version 0 with one `history` round trip. Sending from 0 for a domain with no row yet would save it.
- **Idempotency (fixed for submits).** A request id is claimed in memory before anything is applied (`Context::claim`); a concurrent copy waits and gets the first answer. A submitted delta and its `federation_applied` row (with the encoded answer) are written in the same database job (`SubmitReceipt` through `WaveletContainer::submit_request_with_receipt` to `DeltaStore::enqueue_append`), and the claim is released only after the commit. `update`, `commit` and `post-signer-info` are still recorded after they are applied; applying them twice changes nothing.
- **Attachments.** Ids without a domain (uploaded before federation was enabled, or by a client that does not qualify them) cannot be fetched by other servers. The thumbnail of a cached copy is made locally. Requests carry the wave's root wavelet (`waveRef` of the wave); attachments of other wavelets of a wave (private replies) need the client to send that wavelet. The info endpoint fetches missing remote attachments one after another.
- **Probe.** `exists` tells peers which addresses exist (as in the original); the result is rate-limited per user but not cached. A `deny` from the CLI applies to the next connection; one already open stays until it closes.
- **Remote submit latency.** A forwarded submission waits for the replica echo up to 10 s; if the host's outbox is slow (or held), the local client waits that long, then gets the result without the delta in the replica yet.
- **Replica cleanup.** Replicas stay when the last local participant is removed; the host stops notifying (the removal itself is the last update sent). There is no garbage collection of replicas.
- **Expired notifications.** After 7 days of failures a row is dropped; the remote catches up only when the wavelet changes again.
- **Queue overflow.** A replica holding more than 1000 out-of-order deltas drops its queue and relies on `history`.
- **`signer-info`** is answered to any authorised peer, as in the original.
- **Endpoint shutdown** takes about 3 s when a peer it was connected to has gone away (iroh closes connections gracefully).

- **DNSSEC.** `HickoryTxtResolver` uses the system resolver without DNSSEC validation. Since TOFU pins the first key seen, a spoofed first answer pins the wrong key. Consider DNSSEC, DoH, or showing new pins to admins.
- **Pin refresh.** Pins are never re-checked against DNS once trusted. If a domain drops our pinned key from its record without a rotation, we keep trusting it until an admin acts. Re-check periodically (`last_verified` is already stored).
- **Rotation statements in Hello.** `LocalIdentity.rotations` is always empty; `service` does not yet load our own statements. A `resonator federation rotate` command should sign one with the old key, keep it, and include it in Hello and signer info.
- **Inbound connections.** They are not cached for outbound use. Both sides dial each other, so two peers can hold two connections. That is harmless, but it could be merged.
- **Endpoint test duration.** `tests/endpoint.rs` takes about 8 s because one case waits for a request to a shut-down peer to time out (5 s).
- **Discovery.** `discovery = "default"` publishes and resolves through n0's pkarr and DNS service. With `"off"`, peers come from `[[federation.peers]]` (a DNS domain listed with a key is pinned as if by an admin).
- **Config output.** The `[federation]` section now appears in `resonator config` output, even when federation is disabled.
