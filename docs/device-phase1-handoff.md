# Device plan, phase 1 (A: epochs and rewind): handoff

What `docs/device-plan.md` section A asked for now exists, server, federation and client. With federation off nothing changes. No epoch is ever recorded, deltas carry no epoch field, the bytes are identical, and every path below is skipped. A web socket client never receives a reset.

## What exists

### Storage (migration 10; migrations 1–9 untouched)
- **`wavelet_epochs`:**
  - Columns: wavelet, epoch, start version and hash, holder key, holder domain, signed statement.
  - Epoch 0 is implicit and never stored.
- **`orphan_deltas`:**
  - Columns: wavelet, SHA-256 of the signed original, fork version and hash, seq, the serialized applied delta, and status `pending`/`done`.
- **`DeltaStore` (`storage/epoch_store.rs`):**
  - `truncate_from(wavelet, v)` deletes deltas and snapshots at version v or later.
  - `rewind(...)` does all of the following in **one write job**:
    - truncates
    - records the epoch
    - stores the orphans
    - caps `federation_outbox.from_version` at v
    - lowers the `search_wavelets` version, so the next index write is accepted
    - sets `hosted_by`
  - `put_epoch`, `epochs`, `pending_orphans`, `orphans_done(wavelet, shas)`.

### Model
- **`ProtocolAppliedWaveletDelta`:**
  - Field 16 `epoch`. When absent the epoch is 0, and old deltas hash and serialize exactly as before.
  - Field 17 `rebased_operations`: the operations an orphan was rebased to, when they differ from the signed original.
- **Builders:** `convert::build_applied_delta_in_epoch` and `applied_delta_epoch`.
- **Federation RPC:**
  - `EpochStart{wavelet_name, epoch, start_version, holder_key, holder_domain}` and `SignedEpochStart`.
  - Request tag 27 `Resubmit{wavelet_name, fork_version, signer_info[], applied_delta[]}`.
  - Response tag 25 `ResubmitResult{applied, skipped}`; response tag 26 `Fenced{epoch_start[]}`.
  - `epoch_start[]` (tag 3) on `Update` and `Commit`, and (tag 4) on `HistoryResult`.

### Wave server (`waveserver/`)
- **`EpochBook` (`epoch.rs`):** the in-memory epochs per wavelet, loaded at start by `WaveServer::load_epochs()`.
- **Hosting and validation:**
  - `host_of` / `host_domain(name)` follow the latest epoch's holder; otherwise the wavelet domain is used.
  - `epoch_at(name, v)` returns the epoch with the greatest start ≤ v. The host builds every new delta with `epoch_at(current)`.
  - Replicas reject a delta whose epoch is not the one active at its version.
- **`adopt_epoch(name, EpochRecord) -> EpochAdoption`.** The record must already be verified. Outcomes:
  - `Known`.
  - `Stale(latest)`.
  - `Recorded` when the start equals the current version.
  - `Rewound{fork, orphans}` when the history goes past the start. This path:
    1. Calls `LocalWaveletContainer::change_epoch` and stores the rewind (one job).
    2. Calls `WaveMap::invalidate`, so the container reloads from storage.
    3. Sends `WaveBus::publish_reset(name, fork, rewound)`.
  - A history that differs *at* the start (a fork of a fork) is refused.
- **`resubmit_orphans(name, fork, chain) -> ResubmitOutcome{applied, skipped, version}`** runs on the host:
  1. Takes the SHA-256 of each signed original.
  2. Skips orphans whose original is already in the history since `fork`. This is the dedup across nodes, so duplicates apply once.
  3. Transforms the rest through the history since `fork`.
  4. Applies them under the current epoch, with `rebased_operations` set when the transform changed them.
  - Nothing is re-signed.
- **`rewind_and_replay(name, version)`:** a single-server stand-in for a change of host. It takes a new local epoch at `version` (empty statement), then replays the orphans. It is used only by the end-to-end hook below.
- **Search** reindexes a wavelet on reset (`search/mod.rs`), bypassing the version guard.

### Client front end (`frontend/mod.rs`)
- **New fields and hooks:**
  - `WaveViewUpdate.reset_version`, sent as `ProtocolWaveletUpdate` field 8 `reset_version`. The update's deltas are the deltas taken back.
  - `UpdateSink::accepts_resets()`. Only the web socket sink (`rpc::SocketSink`) returns true.
  - Any other sink (agents, robots, the Data API) has its subscription failed with "Wave view reset after a change of host; reopen the wave". These sinks reopen as they do after any failure.
- **Reset handling, per channel:**
  - The reset waits while a submit is outstanding (`pending_reset`), so the client first gets the ack for its own delta. A second reset during the same submit fails the subscription.
  - Held-back deltas are sent first. Then everything the channel has seen past the fork goes back in one update, and the channel continues from the fork.

### Federation (`resonator-federation`)
- **`epoch.rs`:**
  - Statements:
    - `EPOCH_SIGNATURE_CONTEXT = b"resonator/epoch/1\0"`.
    - `sign_epoch_start` / `verify_epoch_start` use the server key (the iroh secret key).
  - `EpochAuthority`:
    - Trait: `may_host(wavelet, holder_domain, statement)`.
    - `WaveletDomainOnly`, the default, allows only the wavelet's domain to host.
    - `CoHosts::new(domains)` lets any of the listed domains host; it is the admin/test hook.
    - Set it with `FederationSetup::with_epoch_authority(Arc<dyn EpochAuthority>)`.
  - Adopting a statement requires all of these:
    - a valid signature
    - the statement is about the same wavelet
    - the authority says yes
    - the holder key is bound to the holder domain through `Trust::verify_signer` (skipped for our own key)
  - `orphan_worker`:
    - Groups pending orphans by wavelet and fork.
    - If we host the wavelet, it applies them locally; otherwise it sends `Resubmit` to `host_domain`.
    - On `Fenced` it adopts and retries. It backs off on failure.
    - It is woken after every rewind and by `Federation::resubmit_orphans()`.
- **Fencing.** A server that knows epochs answers `Fenced{its statements}` to an `Update`, `Commit` or `Submit` from anyone but the current holder:
  - `replica.rs` for updates and commits.
  - `host.rs` for submits to a wavelet no longer local.
  - The sender adopts the statements, which steps it down:
    - `outbox::step_down` handles Fenced on delivery.
    - `remote::forward` handles Fenced on a forwarded submit, and the user gets "moved to another host; try again".
  - Outbox rows for a wavelet that is no longer local are dropped.
- **Replica catch-up.**
  - Updates, commits and `HistoryResult` carry the statements.
  - A replica adopts them under its queue lock: it rewinds, clears its queue, and catches up from the fork via `host_domain`.
  - Signer info is fetched from the current host.
- **Resubmit (server side).** The peer must speak for the wavelet domain or a participant. The handler learns the signer infos, verifies every original's signature, then calls `WaveServer::resubmit_orphans`.
- **Capability** `"resubmit"` is advertised in Hello.

### Web client (`client/src`)
- **Wire and service:**
  - `wire/waveclientRpc.ts`: `resetVersion` (field 8).
  - `client/remoteWaveViewService.ts`:
    - Turns a reset update into a `WaveViewServiceUpdate` with `getResetVersion()`.
    - Its deltas are the rewound ones, ending at the last version reported.
    - It rewinds `VersionSignatures`.
- **Channel plumbing:**
  - `viewChannelImpl` calls `ViewChannelListener.onReset?(waveletId, version, rewound)`.
  - The mux (`operationChannelMultiplexerImpl`) calls `Stacklet.onRewind`, which calls `OperationChannelImpl.onRewind` and then `CC.onReset`, then `reconnect(null)`.
  - Reconnecting reopens the view at the fork, ignores late acks from the old connection, and resends what is unacknowledged.
- **Concurrency control.** `CC.onReset(fork, rewound)`:
  1. Inverts the rewound deltas (`model/operation/invert.ts`).
  2. Transforms the inverse against the unacked and queued operations, and retargets the unacked delta to the fork.
  3. Applies the transformed inverse as server operations.
  - The user keeps everything typed. A failure here is `NOT_RECOVERABLE`, as before.

### End-to-end hook
`resonator-server serve` with `RESONATOR_E2E_HOOKS=1` (and only then) adds `POST /e2e/rewind?wavelet=<wire name>&back=N`. It rewinds the last N deltas and replays them through `rewind_and_replay`. `TestServers.start({ hooks: true })` sets the variable. `holdUploads()` / `releaseUploads()` hold back browser-to-server WebSocket traffic in the test proxy.

## Tests
- **`crates/resonator/tests/epochs.rs`** runs three in-process nodes (h1, h2, h3.example, all with `CoHosts`), with snapshots every 2 deltas so that some lie on both sides of the fork:
  - `a_returning_old_host_rewinds_and_its_edits_are_rebased_once` is the main scenario:
    1. H2 takes over while H1 is away.
    2. H1 returns, is fenced, rewinds, and resubmits.
    3. The edits apply once everywhere.
    4. A duplicate Resubmit gives applied 0 / skipped 3.
    5. Lower-epoch `Update` and `Commit` are fenced.
    6. Search follows, the outbox settles, and a restart and a reload from storage agree.
  - `orphans_held_by_a_replica_are_resubmitted_without_the_old_host`: H3 resubmits when H1 never returns.
  - `pending_orphans_survive_a_restart_of_the_old_host`.
  - `a_takeover_needs_a_higher_epoch`.
  - **Flakiness:** 5 consecutive runs after the last change, and 9 earlier, with no failures (about 10.6 s per run).
- **Server unit tests:**
  - `frontend/tests.rs`: the rewind sends the taken-back deltas; a subscription that cannot follow fails; a reset waits for an outstanding submit; own deltas go back too.
  - `tests/delta_store.rs`: truncation of deltas and snapshots.
- **Client unit tests:**
  - `concurrencycontrol/client/tests/reset.test.ts` (3).
  - The mux test `muxRewindsAChannelAndReconnectsAtTheFork`.
  - The `remoteWaveViewService` test "a rewind…".
- **Playwright, `e2e/app/rewind.spec.ts`:**
  1. A reply is typed while its submit is held in the proxy.
  2. The server rewinds the two deltas before it and replays them.
  3. The browser receives exactly one reset and is released.
  4. Everything ("Alpha.", "Beta.", "Gamma." once) is there, also after a reload, with no console errors.
  - 6 runs, all passing.

## How phase 3's lease should use this
- **Taking over (the device that wins the lease for a wavelet):**
  - Call `Federation::take_over(&wavelet, epoch)` with `epoch` greater than every known epoch. Use `server.epochs(&wavelet).last().map_or(1, |e| e.epoch + 1)`, or the lease's own term number if that is monotonic.
  - It returns the epoch's start, which is this node's current version. It records the epoch, then queues the statement for every participant domain and the wavelet domain.
  - Deltas the old host applied past that start become orphans on any node that held them, and they are resubmitted here automatically.
- **Authority.**
  - Replace `CoHosts` with an authority that checks the lease, for example `may_host` = "`holder_domain` is a device of the wavelet owner's person and the statement's epoch matches a lease term signed by the person key".
  - Every node must use the same rule, because a statement the receiver's authority refuses is ignored.
  - The statement is signed by the holder's iroh key. The holder domain must resolve to that key through `Trust` (for person domains this will be the DeviceCert check from section B).
- **Stepping down** needs no call: a node that sees a higher epoch (Fenced, Update, Commit or HistoryResult) adopts it and stops hosting. To step down voluntarily, have the new holder `take_over` first. `Federation::adopt_epochs(&wavelet, &statements)` lets the lease layer push statements it learned out of band.
- **Orphans:**
  - Call `Federation::resubmit_orphans()` after connectivity returns, to skip the backoff.
  - Pending orphans survive restarts.

## Deviations from the plan
- **Dedup without a stored index.** There is no `submitted_originals` index. The host computes the SHA-256 set of the history since the fork when it rebases; that history is bounded by the fork and only paid on resubmit.
- **Replicas trust `rebased_operations`.** They verify the author's signature on the original, but take the host's word for the rebased form, as they do for any transformed delta.
- **A web-socket reset is a field on `ProtocolWaveletUpdate`**, not a separate message, so it is ordered with the updates on the same channel.
- **The Playwright test runs on one server** through `rewind_and_replay`, not on a federation. Federation is covered in Rust.

## Open issues
- **Nested forks:** a rewind whose start the local history does not contain (a fork of a fork) is refused.
- **Clients disconnected across a rewind:**
  - Their known version (an old hash) is not in the new history, so the reopen gets a snapshot.
  - Their pending operations are then lost: the client fails NOT_RECOVERABLE and reloads.
  - The fix is to send several known versions (the client has older signatures) so that the server can resync from the fork.
- **In-flight submit at the old host:**
  - A submit that is in flight at the old host when it rewinds is refused (unknown version).
  - The browser resends it after its reset. A robot or agent resubmits on failure as usual.
- **Undo is incomplete.** Inverse operations restore content, but not blip metadata (last-modified times, contributors) nor a participant's position in the list. The reload after a reconnect corrects the view.
- **Non-socket subscribers** (agents, robots, Data API) are failed on reset instead of following it.
- **Agent triggers:**
  - `agent_triggers` are keyed by blip, so they are left alone, as the plan says.
  - An answer whose delta was orphaned comes back by resubmission. One transformed away leaves the trigger `answered` with no answer.
- **Unread state:**
  - Read versions in user-data wavelets refer to the old numbering.
  - After a rewind a blip can show unread although it was read, or the reverse until it changes again.
  - This is harmless but visible.
- **`federation_applied` receipts** for truncated deltas are not withdrawn.
- **Epochs are per server key**, not per person. Section B will need `holder_domain` to accept person domains.
- **The e2e hook records an epoch with an empty statement.** Use it only on throwaway databases, never on one that federates.
