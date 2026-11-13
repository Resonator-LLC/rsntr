# Device plan, phase 3 (C: host lease and device sync): handoff

What `docs/device-plan.md` section C asked for now exists: a signed host lease per person, device sync (`sync-summary`), a persistent queue of pending submissions with an interim "pending" answer to the client, and attachments and read state across the devices of a person.

With federation off, or with `[person] enabled = false`, nothing changes:
- no lease task and no sync task run;
- Hello carries no lease;
- the `lease` and `sync-summary` ops are refused like any unknown op;
- attachments behave as before (`shared_domain` is false).

The pending queue does apply in server mode as well, to a remote host that cannot be reached. Before, the submission failed after `REMOTE_ECHO_WAIT`. Now it is kept and sent later, which is the intended replacement of that failure path.

## What exists

### Model (`resonator-model/src/proto/`)
- **`federation_rpc.rs`:**
  - `HostLease{person, domain, term, device, issued_at, expires_at, prev, handover, anchor}` and `SignedHostLease{lease, signature}`.
  - `HostHandover{person, term, from, to, issued_at}` and `SignedHostHandover{handover, signature}`.
  - `Hello` tag 8 `host_lease` (repeated): the sender's view of its own person's lease.
  - New op 29, `Lease{host_lease[], handover?}` → `LeaseResult{host_lease[], anchor}`.
  - New op 30, `SyncSummary{entry[]}` → `SyncSummaryResult{entry[]}`, where `SyncEntry{wavelet_name, version (hashed), epoch, holder, epoch_issued_at}`.
- **`waveclient_rpc.rs`:** `ProtocolSubmitResponse` tag 4 `pending` (optional bool): an interim answer.

All new fields are optional, so old peers and clients ignore them.

### Storage (migration 12; migrations 1–11 untouched)
- **`pending_submits`:**
  - Columns: `seq` (autoincrement), `wavelet`, `author`, `request_id` (unique), `signed_delta`, `target_version`, `created_at`, `attempts`, `last_error`.
  - Indexed on `(wavelet, seq)`.
  - Accessed through `PendingStore` (`Database::pending_store()`): `push`, `all`, `has_wavelet`, `remove`, `failed`.
- **`host_leases`:** `person` (primary key), the signed lease and `learned_at`. Accessed through `FederationStore::{put_host_lease, host_leases}`.

### Config (`[person]`)
- `anchor = true`: this device is a preferred holder.
- `lease_renew_anchor_secs` (0 means 600) and `lease_renew_secs` (0 means 120): how often an anchor or another holder renews.
- `holder` is deprecated. If set, it is still followed until a lease is known; otherwise submissions wait for a lease (`PersonHosting::unknown`).
- `Tuning` (federation crate) has `lease_renew_anchor`, `lease_renew` and `sync_interval` (60 s). Tests use 500 ms, 400 ms and 300 ms.

### The lease (`resonator-federation/src/lease.rs`)
- **Signing:**
  - A lease is signed with the context `resonator/host-lease/1\0` by the holder device.
  - A handover is signed with `resonator/host-handover/1\0` by the device handing over.
  - `verify_lease` checks the signature. If the lease carries a handover, it also checks that the handover is from the same person, for term − 1, and to this lease's device.
- **`Trust::learn_lease`** also checks:
  - that the device holds a certificate of the person and is not revoked;
  - that the person speaks for the lease's domain.

  It then:
  - offers the lease to `Leases`;
  - persists it;
  - routes the domain to the holder (`PersonBook::set_route`) unless the domain is local.

  Leases arrive in Hello, in `lease` requests and answers, and from storage at start.
- **Order (`VerifiedLease::beats`):**
  - A renewal is the same term and device with a later expiry.
  - Otherwise the higher `(term, device key)` wins.

  A higher term is always taken, so a split brain ends when the two sides meet. Two different devices in the same term is "contested": the holder that sees it takes its wavelets over again above the current epochs.

#### The state machine (one task per device, `lease::run`)
It ticks every `min(renew) / 4` (at least 20 ms), and wakes when a lease is learned.

1. **Spread.** Send `Lease` with the known lease to:
   - every own device (cert book plus static devices);
   - every connected peer;
   - every static peer.

   Each request has a timeout of `min(renew) / 2`, capped at 10 s. Note who answered: own devices are "reachable". This device is "online" if any peer answered or the home relay is connected. The answers also say which devices are anchors.
2. **Follow.** Point `WaveServer::set_person_hosting` at the holder, and notify the pending drain when it changes:
   - this device, if it holds the lease (`WaveletHost::Local`);
   - otherwise `Delegated{holder}`;
   - with no lease, the config `holder` if set, or "unknown".
3. **Holder** (the lease's device is this device and it is not revoked):
   - **Renew** every `lease_renew_anchor` (anchor) or `lease_renew` (others): same term, `expires_at = now + 3 × renew`.
   - **If contested:** take every own wavelet over at its latest epoch + 1.
   - **If not an anchor and an anchor is reachable:** `hand_over(anchor)`, at most once per renew interval.
4. **Not the holder:**
   - **If a handover to this device was offered** (verified in `lease::serve`, from the current holder, for the current term):
     1. `sync::exchange` with the device handing over;
     2. then claim term + 1, carrying the handover.
   - **Otherwise claim only if `may_claim`:**
     - not revoked;
     - online, or alone (no other own device known);
     - and either a lease is known that has expired for longer than a grace (0 for an anchor or a lone device, `lease_renew` for the others), or no lease is known and this device is an anchor, alone, or has waited `3 × lease_renew + grace` since starting.

   An offline device never takes over.

**Claim:**
1. Sign `term + 1`, with `prev` = the old holder.
2. Learn it.
3. Follow it.
4. Run `take_over_wavelets` over every wavelet of the own domain (`get_wave_ids` / `get_wavelet_ids`). This calls `epoch::take_over` at the latest epoch + 1, skipping wavelets whose latest epoch is already this device's, or was issued during this term.
5. Spread.

**Stepping down:** the old holder learns the higher term (Hello, `lease`, or the new holder's spread) and follows it. When it receives the new epoch, through the outbox Update/Commit or sync's `history` `epoch_start`, phase 1's fencing applies: it rewinds and resubmits its orphans to the new holder.

### The handover API (for phase 4)
On `Federation`:
- **`handover(to: PublicKey) -> Result<(), LeaseError>`:**
  - Must be called on the current holder. It signs `HostHandover{person, term, from: me, to}` and sends it with the current lease to `to` (op `lease`).
  - It returns once `to` has accepted the request. `to` then syncs with this device, claims `term + 1`, and takes the wavelets over. This device steps down when it learns the new lease.
  - Errors: `NotPerson` (person mode off), `NotHolder`, `Request(..)`.
- **`lease_state() -> LeaseState{holder, term, expires_at, held_here, anchor, online}`:** this is the plan's `state()`.
- **`lease() -> Option<VerifiedLease>`:** the newest lease of the own person, with `handover_from` and `prev`.
- **`sync_with(device) -> Result<usize, RequestError>`:** syncs with one own device now.
- **`drain_pending() -> usize`:** sends queued submissions now.

A UI for "make this device the host" in phase 4 only needs `handover(key)`, which can be called on the holder. Holders that are not anchors already hand over to a reachable anchor on their own.

### Device sync (`resonator-federation/src/sync.rs`)
- **`summary`:** every wavelet this device has (hosted, replicas and user-data wavelets), with its hashed version, latest epoch, the holder of that epoch, and when that epoch was issued.
- **`exchange(key)`:** sends `SyncSummary` to an own device and pulls what that device has more of. **`serve`:** answers with its own summary and pulls from the asker in the background. It refuses anyone who is not an own device (`NotAuthorized`).
- **`pull` and `pull_wavelet`:** under the wavelet's replica queue lock, `history` from that device, from the current version to the device's announced version, paged.
  - Epoch statements in the answer are adopted, which fences and rewinds as in phase 1.
  - Deltas are verified with `replica::verify`, then applied: `apply_replica_delta` for a replica, or the new `WaveServer::import_delta` for a wavelet hosted here. Both must continue the local hash chain exactly. A delta of an older epoch than the one active is refused.
  - Own wavelets imported by the holder are then taken over (`take_over_wavelets` with `since` set to the term start).
- **Split brain:** if the holder sees, for one of its own wavelets, an epoch from another device that is at least as new as its own, not its own, and issued before this holder's term began, it takes the wavelet over above it. Something issued during the term is a deliberate move, such as `Federation::take_over`, and is left alone.
- **`host::history`** now lets an own device read any wavelet this device has, replicas included, without the participation check.
- **Tasks:**
  - `sync::run` exchanges with every other own device every `sync_interval` and whenever a Hello arrives.
  - `sync::drain_pending` runs every `min(sync_interval, retry_max)`, on Hello, and when the hosting changes.
  - Both, and `lease::run`, are dropped at once at shutdown.
- **Fan-out to all devices** was already in place: `Context::targets(domain)` expands a person domain to every known device. The new test shows that a wave of Q reaches P through the one device Q can reach.

### Pending submissions (`resonator-server`)
- **`WaveServer::submit` for a remote host** (another server, or the lease holder) goes through `submit_remote`:
  - If the wavelet already has queued submissions, the new one is queued behind them, so order is kept.
  - Otherwise it is sent with a fresh request id. On `SubmitError::Unreachable`, it is queued.

  The federation side reports `Unreachable` for:
  - `Connect`, `Timeout` and `Closed`;
  - an `UndefinedCondition` answer, which is what a device that is no longer the host now returns;
  - a Fenced answer.

  Other errors are returned as before.
- **A queued submission:**
  - It answers at once with `SubmitResult{operations_applied: 0, version at the target, pending: Some(seq)}`.
  - The frontend sends that as an interim `ProtocolSubmitResponse{pending: true}` on the same request.
  - It then waits on `wait_pending(seq)` and sends the final answer on the same request.
- **`drain_pending`:**
  - Runs one at a time.
  - Works per wavelet in `seq` order and sends each row with its stored request id; the host's request-id dedup and the container's identical-delta dedup make retries idempotent.
  - If the holder is now this device, it applies locally.
  - A row that cannot be sent stops its wavelet's queue for this round (`attempts` and `last_error` are recorded).
  - A bad request drops the row and answers the client with the error.
- **Restarts:** the queue survives a restart. It is drained at start, on Hello, and when the lease moves. A client that was waiting has lost its connection by then; its unacknowledged delta is resubmitted on reconnect and deduplicated by the host.
- **Revocation:** a revoked device refuses submissions to remote hosts at once, with a bad request, instead of queueing them.
- **`REMOTE_ECHO_WAIT`** now only logs when the host's echo is late; it never fails a submission.

### Attachments
- **`AttachmentSettings.shared_domain`** (federation and person mode both on) together with `AttachmentService::is_fetchable(id)`: an attachment of the own domain that is missing here is fetched like a remote one.
- **`RemoteOrigin`:** for the own domain, asks each other own device in turn.
- **`attachments::serve`:**
  - Own devices may fetch any attachment of the person.
  - For a peer of another person, a device that lacks the attachment fetches it from the person's other devices first. This is why Q can download what alice uploaded on her laptop through whichever device Q reaches.
  - Copies from own devices (origin = own domain) are served on. Copies of other servers' attachments still are not.
- **Copies ahead (later addition, `crates/resonator-federation/src/copies.rs`):** with `[federation] replicate_own_attachments` (default true; person mode only), devices copy the person's attachments before anyone opens them.
  - **Announce:** `AttachmentService` calls its `UploadListener` after every local upload (`finish_upload`, never for fetched copies). The person-mode listener sends op 37 `attachment-copies` (`AttachmentNotice{attachment_id, wavelet_name, size, sha256, image, created_at}`) to every other own device in a background task, so the upload's answer does not wait. Only ids of the own domain and up to `replicate_max_mb` (default 25) are announced.
  - **Pull:** the receiver (own devices only; `Ack` at once) copies through `AttachmentService::fetch_copy`. That uses the same single-flight fetch, `finish_fetch` size/SHA-256 checks, storage and thumbnail as a user's fetch, through a `RemoteOrigin` that asks the announcing device first and never wakes a sleeping one. It does not record the 15 s unreachable back-off. At most 2 copies run at a time.
  - **Catch-up:** after each successful device sync, a background round asks the other device with op 38 `own-attachments` for pages (128) of the attachments of the own domain it holds (uploaded or copied, not evicted), ordered by `(created_at, id)` from an in-memory position per device. A round copies what is missing for wavelets held here, up to 256 items and 4 × `replicate_max_mb` bytes. It stops at the first copy that cannot be had now, so the next round retries from there. The position resets hourly, because a copy can be older than the position.
  - **Cache budget:** copies count against `attachment_cache_mb` and stay evictable. Before pulling, a device checks `cache_free()` and skips a copy that does not fit (it stays lazy), so copying ahead never evicts anything. The rationale: the uploading device keeps the original, which is never evicted, so an evicted or skipped copy falls back to the lazy fetch with push wake. Pinning would let one busy tablet fill a phone's storage with no bound the user set.
  - Other people's devices and server mode are unaffected.

### Accounts and read state
- Users are registered per device with the same `name@<person domain>`.
- Read state lives in the user-data wavelet `user+<address>` of the person's domain. It is hosted by the lease holder and reaches other devices by fan-out and device sync. The test marks a wave read on the laptop while the phone is away and sees it read on the phone after the phone returns.

### Client
- **`wire/waveclientRpc.ts`:** `ProtocolSubmitResponse.pending` (field 4).
- **`net/rpcChannel.ts`:** `callWithCallback(..., interim?)`. A response that `interim` claims, by returning true, does not end the call.
- **`client/rpcWaveTransport.ts`** and **`waveRpc.ts`:** `submit(request, onResponse, onFailure, onPending?)`.
- **`client/remoteWaveViewService.ts`:**
  - On a pending answer, the submit callback is not called. The concurrency control therefore keeps the delta unacknowledged, without an error, until the final answer.
  - `onAwaitingHost(+1 / -1)` counts the deltas waiting.
- **`client/waveClient.ts`:** an `awaitingHost` event and getter.
- **`ui/app/appStore.ts`:** an `awaitingHost` signal.
- **`ui/app/HostPendingIndicator.tsx`** (new, styled inline): "Waiting for host", shown in the top bar next to the saved state while the count is above zero.
- No file under `ui/wave`, `ui/editor`, `ui/styles` or `index.html` was touched. `concurrencyControl.ts` needed no change.

## Tests
- **`crates/resonator/tests/persons.rs`:** the harness now uses `anchor` instead of `holder`, and `Running::start` waits until both persons have a holder. Five new tests:
  - **`the_host_lease_follows_a_persons_devices`:** A (anchor), L, F and Q.
    - A gets the lease.
    - F is cut off (restarted with no peers on an unknown port) and A is stopped. L takes over at epoch 1, and Q's and L's edits converge.
    - A wave of Q reaches P through L alone.
    - F, cut off, never claims and stays offline.
    - A returns: L hands over to A (`handover_from == L`), and A hosts at epoch 2.
    - The wave is marked read on L while F is away.
    - F returns and catches up: both waves converge, read state included.
  - **`a_signed_handover_moves_the_lease`:**
    - A non-holder's `handover` fails.
    - A hands over to F, and F hands back to the anchor (term + 2, `handover_from == F`, `prev == F`).
    - Epochs are 2 everywhere, and edits converge.
  - **`pending_edits_are_sent_once_after_a_restart`:**
    - F, cut off, edits P's wave (hosted on A) twice and Q's wave once; 3 are queued.
    - The queue survives a restart while cut off.
    - After reconnecting, every marker appears exactly once and the queue empties.
  - **`a_split_brain_heals_without_losing_edits`:**
    - A is cut off and keeps its lease, while L takes over. Both host the wave and both get edits.
    - A rejoins: one holder remains and every edit appears once.
  - **`attachments_travel_between_a_persons_devices`:**
    - An upload on L is downloaded on A and on both of Q's devices.
    - A user who is not on the wave gets 403.
- **Flakiness:** the lease, handover, split-brain and pending tests ran together 21 times, 84 test runs in all. They passed every time except once: `Node::federate` failed (`setup.start(..).expect("federation")`) at a device restart, most likely a UDP port race in the harness (`free_udp_port`, or rebinding the port just released). No lease, sync or queue logic failed.
- **Totals:** `cargo test --workspace` 1238 passed; client `npm test` 1174; Playwright 29 passed. Clippy (`--workspace --all-targets`) and `cargo fmt --check` are clean, and `tsc` and `npm run build` succeed.

## Open issues
- **Term start after a restart:** a holder restarted mid-term treats its latest renewal as the term start, because the term start is not persisted. A deliberate `take_over` by another device between the claim and the restart could then be fought once by sync's split-brain rule. Persisting the term start with the lease would fix this.
- **Order of robot edits while pending:** two Data API edits made one after the other on a device whose host cannot be reached are both generated against the same, stale replica version. They are sent in order, but transform can place the second before the first. The web client is not affected, because its concurrency control composes against its own unacknowledged delta.
- **Pending answers across reconnects:** the final answer to a pending submission reaches the client only on the connection it was submitted on. After a reconnect the client resubmits, and the host deduplicates the delta. This has not been exercised in a browser.
- **No Playwright test for the indicator:** a real pending answer needs a federated host that cannot be reached, which the single-server e2e harness (`TestServers`) cannot set up, and `holdUploads` holds the request rather than producing a pending answer. The interim path is unit-tested in `rpcChannel.test.ts` and `remoteWaveViewService.test.ts`.
- **Sync cost:** `summary` lists every wavelet a device has, and runs every `sync_interval` (60 s) per own device. That is fine for a person's devices, but a digest or a "since" cursor would be needed for large stores.
- **`online`:** this is "some peer answered, or the home relay is connected". With relay and discovery off and no static peers, a lone device counts as alone and may claim. That is intended, so that single-device persons keep working.
