# Device plan, phase 5 (E: pairing, recovery and the ownerless-wave cases): handoff

What `docs/device-plan.md` section E asked for now exists:
- pairing by link or QR code;
- recovery from the phrase on a fresh device;
- the settings page of a device node;
- "continue as new wave" for a wave whose host is gone;
- co-hosts.

With `[person] enabled = false` (or federation off), nothing changes:
- `/person…` answers 404;
- `/waves/host-status` says `local` or not frozen, and `/waves/continue` answers 409;
- the client shows no settings button outside device mode, and no "continue" notice without person mode;
- migration 14 only adds empty tables.

## Pairing (`resonator-federation/src/pairing.rs`, `control.rs`)

### Protocol
1. **The invitation.** An admin device (it holds the person key) makes an invitation with `Inviter::invite`. It holds:
   - the person key;
   - the inviting device's key and addresses;
   - an id;
   - a one-time 128-bit secret;
   - an expiry 10 minutes ahead.

   It is encoded as `resonator:pair/<base64url>`, plus a QR code (`pairing::qr_svg`, SVG). The secret only travels in the link. The record lives in `pair_invitations` (migration 14); the secret is kept there only while needed to check the MAC (until the invitation is used, expires or is cancelled) and is never logged.
2. **The request.** The new device dials the inviter on `PAIR_ALPN` and sends `Pair{id, mac}`, where `mac` = HMAC-SHA256(secret, `"resonator/pair/1\0"` ‖ inviter key ‖ joiner key ‖ id). The joiner key is the QUIC connection's key, so the MAC binds the secret to both device keys, and the secret is never on the wire.
3. **The check.** The inviter checks the MAC in constant time; 5 wrong MACs burn the invitation. An expired or used invitation is refused.
4. **The answer:**
   - the new device's `DeviceCert` (name as asked);
   - the person's other certificates;
   - the revocation list, the host lease and the co-host grants;
   - the devices' addresses, stored in `device_addrs`.

   The invitation is then used up.
5. **Admin invitations** (the new device also gets the person key):
   - The first valid request makes the record `requested`. The inviter confirms it with `confirm`: in the CLI, the API or the settings page.
   - The joiner retries until it is confirmed or `CONFIRM_WAIT` (180 s) runs out.
   - The phrase's BIP39 entropy travels XORed with HKDF-SHA256(secret, `"resonator/pair/1 key"` ‖ inviter ‖ joiner), inside the authenticated QUIC stream.
6. **Install.** The joiner checks the certificate (its key, the invitation's person; for admin pairings, the key derived from the phrase). Then `install` writes `device.cert` (and `person.key`), setting aside an older person key or cert, and `remember` stores what it learned.
7. **Restart.** The node restarts as a device of the person and syncs.

### The restart and the account
- The new device's account is `me@<person domain>`, the same as the person's other devices.
- An empty database may move to the new domain once: the `domain-rebind` marker (`resonator_server::app::REBIND_MARKER`) is honoured only when the database has no deltas.
- The node restarts itself after pairing or recovery:
  - `AppState::request_restart`, `Node::restart_requested` and `Node::relaunch` keep the listen address and the device token (`Config.device_token`, never serialised, Debug redacted);
  - the `resonator serve` loop relaunches;
  - the FFI `Handle` has a supervisor thread for it.
- An admin device keeps the certificate pairing gave it (and its name) instead of self-issuing a new one.

### Where to find it
- **CLI:**
  - `resonator person pair invite [--admin]`;
  - `person pair list`;
  - `person pair confirm <id>`;
  - `person pair cancel <id>`;
  - `person pair join <link> [--name] [--wait]`.

  Invite and join work offline on the database (`control::offline_invite`/`offline_join`). Join with the node stopped.
- **HTTP** (device mode, the device's signed-in account only):
  - `GET/POST /person/pair` (POST answers `{id, admin, expires_at, state, link, qr}`);
  - `POST /person/pair/{id}/confirm`;
  - `DELETE /person/pair/{id}`;
  - `POST /person/pair/join {link, name}`.
- **FFI:** `ResonatorNode::pair_invite(admin)` returns the link, and `pair_join(link, device_name, wait_ms)` returns `PairJoinResult{person, admin, inviter}` (relaunches the node). The Swift and Kotlin bindings were regenerated, and the README table updated.

## Recovery (`recovery.rs`, `control.rs`)

`resonator person recover [--revoke-old] [--peer <key[@ip:port]>]…` (the phrase on stdin), `POST /person/recover {phrase, revoke_old, peers}` or the settings page.

1. **Preparing** (`prepare_recovery`):
   - sets aside the old key and cert;
   - stores the phrase;
   - allows the domain rebind;
   - writes `recovery.json` `{revoke_old, peers}`;
   - then the node restarts. The device self-issues an admin cert.
2. **At start, while the marker exists:**
   - the lease task is held back (`Leases::hold_back`), so nothing is taken over half pulled;
   - `PersonOps::finish_recovery` runs `recovery::recover`, retrying with backoff until a peer answers. After 120 s without answers the hold is lifted so a lone device still hosts.
3. **`recover`:**
   1. Asks the given, connected and static peers `PersonInfo` (certs, revocations, lease, co-hosts) and learns the answers.
   2. Asks those peers `Holdings` (the wavelets they hold that the person owns or takes part in). It pulls each with `sync::pull_wavelet_from`, verifying signatures and the hash chain like any replica.
   3. Publishes revocation list `seq+1`; with `revoke_old`, every other certified device is revoked. It sends the list to the peers that answered, and the watcher spreads it.
   4. Wakes the lease task, which claims the lease (at once if the holder was revoked) and takes the wavelets over.

   Waves come before revocations so the old devices' epochs and deltas verify as they were made.

   Then the marker is removed.
4. **`epoch::adopt`** no longer re-checks statements it has already recorded. A holder revoked later loses new epochs, not the ones it held; this is what lets a recovered device pull history whose epochs were signed by now-revoked devices.

## Settings UI (`client/src/ui/settings/`, `client/src/net/person.ts`)

- **Opening it:** a gear button in the top bar in device mode, route `#settings` (`AppStore.settingsOpen`, `openSettings`/`closeSettings`; browser back works). One column of grouped cards; full width with no side scrolling on phones.
- **Sections:**
  - **You:** the person domain, this device and its name, admin.
  - **Devices:** name, badges (this device, admin, hosts your waves, always on, revoked), last seen (`Leases::note_device_seen` on Hello), revoke (admins, with an inline confirmation).
  - **Hosting:**
    - lease state text and the anchor checkbox, persisted in `person-anchor`; `Leases.anchor` is now settable;
    - co-hosts (admins): grant for 30 days, withdraw.
  - **Pair a device** (admins):
    - "Pair a device" or "Pair an admin device" shows the QR code (the server's SVG) and the link, with Copy and Cancel;
    - pending admin requests offer Confirm or Refuse;
    - the page polls every 2 s until the invitation is used or expires.
  - **Recovery phrase** (admins):
    - a warning confirmation comes first; then `POST /person/phrase {confirm:true}` (`Cache-Control: no-store`);
    - the words are shown for at most 60 s;
    - the phrase is held only in a signal while shown and dropped on Hide, after the timeout and on leaving the page; it is never stored.
  - **Contacts:** add by address or `resonator:contact/` link, and remove (those added by hand), using the existing `/contacts`.
  - **Join with a link** and **Recover from a phrase:** shown while this device is the person's only one (a fresh device). Both end with "restarting, open Resonator again".
- **`SettingsStore`** holds the logic (busy state, messages, polling, phrase lifetime). `PersonApi` is on `WaveHttpApi.person`.
- The new paths were added to the dev proxy (`vite.config.ts`) and the e2e front.

## Fork: continue as new wave (`resonator-server/src/waveserver/fork.rs`, `http/person_handlers.rs`)

- **`WaveServer::fork_wave(source, author)`** makes a new wave on the caller's domain in one delta at version zero:
  - the participants, author first;
  - the `conversation` manifest;
  - every blip document's content at the latest version (threads, text, annotations);
  - a `continued-from` data document `<link version wave wavelet/>`.

  An old `continued-from` is not copied, so a fork of a fork links to its direct parent and the chain can be followed back.
- **`GET /waves/host-status?wave=`** returns `{host, local, frozen, reason}` (`PersonOps::host_status`). It is frozen when the lease holder was revoked, or when the lease expired and no unrevoked device of the person connects within 3 s (a co-host holding the lease is not frozen).
- **`POST /waves/continue {wave}`** requires a participant who can write and `frozen`, else 409. It returns `{wave}`.
- **Client:** for a wave of another domain in device and person mode, `ContinueWaveNotice` (under the wave toolbar) checks the host status at open and every 30 s. When frozen it shows "Continue as new wave" (`data-action=continue-wave`) and goes to the new wave.

## Co-host (`cohost.rs`, `trust.rs`, `epoch.rs`)

- **The grant:** `CoHost{person, delegate_domain, until}`, signed by the person key and stored in `co_hosts`. It spreads like certs: in Hello, by the `CoHosts` push, and in `PersonInfo`. The newest grant per person and domain wins, and `until` 0 withdraws.
- **Taking over:**
  - `CoHostAware` wraps the epoch authority, so a delegate domain may hold epochs while its grant is active.
  - `cohost::run` on the delegate claims the lease only when the known lease has expired (plus grace), no device of the person said Hello since, the grant is active and the node is online. The lease carries `co_host_domain`, and the node takes the wavelets over at epoch+1.
  - It renews the lease only while no device of the person is back.
- **Loading:** `cohost::run` reloads stored grants every 5 s, so grants the CLI makes reach a running node.
- **Where to find it:**
  - CLI: `resonator person cohost grant <domain> [--days 30]` and `person cohost revoke <domain>`;
  - HTTP: `POST /person/cohost {domain, until}`;
  - the settings page.

## Tests

### Rust
`cargo test --workspace`: **1259 passed** (baseline 1250). Clippy `--workspace --all-targets` and `cargo fmt --all --check` are clean.

The new tests are in `crates/resonator/tests/persons.rs` (in-process nodes; the file passed 3 times in a row, 16/16, and each new test also on its own 3 or more times):
- `a_device_pairs_by_link_once_and_catches_up`:
  - a wrong secret, an expired link, an expired record and a reused link are refused;
  - an admin pairing waits for confirmation, then gets the phrase;
  - the new devices restart on the person domain with the wave version and read state (unread 0);
  - the names and admin flags are right.
- `a_person_recovered_on_a_fresh_device_pulls_its_waves_back`:
  - both P devices are gone; a fresh device recovers from the phrase with `--revoke-old` and Q's address;
  - the shared wave's whole history comes back from Q, and Q learns the revocations;
  - the recovered device holds the lease and hosts, and edits converge;
  - the old device that comes back is refused (`Revoked`). Passed 5 out of 5 after the lease hold-back fix.
- `a_frozen_wave_continues_as_a_new_wave`:
  - not frozen with P up, and 409;
  - frozen after P's devices stop;
  - the fork has the same text and participants, and a `continued-from` with the source wave and version;
  - it is local and edits converge on Q's devices;
  - a local wave gets 409, and a nested fork links to the first fork only.
- `a_co_host_takes_over_only_after_the_lease_ran_out`:
  - no claim while P renews;
  - after P stops, Q claims with `co_host_domain`, hosts the wave and edits converge;
  - a withdrawn grant never claims.
- `crates/resonator-node/tests/device.rs::the_person_settings_over_http` (one test, outside `persons.rs`):
  - overview fields; invite link and QR; list and cancel;
  - the phrase only with `confirm` and `no-store`;
  - a bad link or phrase gives 400; the anchor persists;
  - 403 when not signed in, 404 outside person mode.

### Client
`npm test`: **1198 passed** (baseline 1177; 21 new in `src/net/tests/person.test.ts` and `src/ui/settings/settings.test.tsx`, the latter rendering the page with happy-dom). `tsc` is clean and `npm run build` is OK.

### Playwright
**35 passed** (baseline 32). `e2e/app/settings.spec.ts` covers:
- the person and this device;
- the pair link and QR, then cancel;
- the phrase behind a confirmation;
- contacts added by address and by link, then removed;
- 375×667 touch: full width, no sideways scroll (also with the link and the phrase shown), browser back.

`TestServers.start({device, person})` writes a person-mode config (relay and discovery off, loopback). It uses `RESONATOR_NODE_BINARY` (default `target/release/resonator`), because person mode needs federation, which `resonator-server` lacks.

The suite was run as:

```
RESONATOR_BINARY=<release>/resonator-server RESONATOR_NODE_BINARY=<release>/resonator npx playwright test
```

Both binaries were built into a separate `CARGO_TARGET_DIR`.

## Deviations and notes

- **"Continue as new wave"** is a notice with a button under the wave toolbar, shown only when the host is frozen, rather than an item in an always-present menu. The wave view has no wave-level menu, and a dead item for every healthy wave would only confuse.
- **A revoked device that comes back** is refused at connect (`Revoked` verdict) and so never hears of its own revocation. It keeps running alone until someone tells it. The test asserts the refusal on the other side.
- **`epoch::adopt`** skips statements identical to ones already recorded instead of re-verifying them (see Recovery). New statements are checked as before, including the revocation check.
- **Recovery's lease hold-back:** without it, the lease task could take a wavelet over at a version below the one being pulled, and peers would rewind (an orphan). The hold is lifted after 120 s if no peer answers.
- **Co-host grant length:** the UI and the CLI default to 30 days.
- **Join and recover restart the node.** In a browser the page says to open Resonator again (the session belongs to the old domain). The FFI `Handle` and `resonator serve` relaunch by themselves.

## Open issues

- **Settings sees only grants of its own person.** The co-host task on a delegate does show its claims in the lease state.
- **No camera QR scanning in the web client:** the app shell would scan and call `pair_join`.
- **The pairing poll is plain polling** (2 s) while an invitation is open. A push would be nicer.
- **`host_status`** probes each unrevoked device for up to 3 s in sequence. Fine for a few devices; parallelise if persons get many.
- **No automatic retry for nested forks:** they are plain new waves. Following a `continued-from` link in the UI (showing "continued from …" with a link) is not done yet; the data is there.
