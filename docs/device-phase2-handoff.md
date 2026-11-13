# Device plan, phase 2 (B: person identity): handoff

What `docs/device-plan.md` section B asked for now exists in storage, the model, federation, the server config and the CLI. No client code changed.

With `[person] enabled = false` (the default), nothing changes:
- no person key or certificate is loaded;
- Hello carries no cert or revocation fields;
- signer infos carry no cert;
- pins, DNS, the default epoch authority (`SameDomain`) and outbox targets are as before.

All federation and epochs tests pass unchanged.

## What exists

### Model (`resonator-model/src/proto/`)
- **`federation_rpc.rs`:**
  - `DeviceCert{person, device_key, name, admin, issued_at}` and `SignedDeviceCert{cert, signature}`.
  - `RevocationList{person, seq, revoked[], issued_at}` and `SignedRevocationList{list, signature}`.
- **`Hello`** gains:
  - tag 5 `device_cert`: the sender's cert, in person mode;
  - tag 6 `revocation_seq`: the highest seq the sender knows for its own person;
  - tag 7 `revocations`: lists the sender pushes, namely its own person's list and newer lists of the receiver's persons.
- **New op `Revocations{person, list?}` → `RevocationsResult{list?}`:**
  - with `list` it pushes a list;
  - without it, it fetches the current list of `person`.
- **`ProtocolSignerInfo`** tag 18 `device_cert`: the cert of a device signer.

All new fields are optional, so old peers ignore them.

### Storage (migration 11; migrations 1–10 untouched)
- **`device_certs`:**
  - Primary key `(device_key, person)`, plus name, admin, issued_at, the signed cert and learned_at.
  - An upsert keeps the newest `issued_at`.
- **`revocations`:**
  - Primary key `person`, plus seq, the signed list and learned_at.
  - `put_revocations` writes only a higher seq and returns whether it did.
- `FederationStore::{put_device_cert, device_certs, put_revocations, revocations}`, with `DeviceCertRecord` and `RevocationRecord`.
- The server-mode `peers` pin table is unchanged: one trusted key per domain, for server keys only. Device keys are never pinned.

### Person key and certificates (`resonator-federation/src/person.rs`)
- **`RecoveryPhrase`:** 24 BIP39 words; zeroized on drop, with a redacted `Debug`.
- **`PersonKey::from_phrase`:**
  - BIP39 seed (empty passphrase) → HKDF-SHA256 (salt `resonator`, info `resonator/person-key/1`) → Ed25519.
  - The derivation is deterministic. A unit test pins one vector.
- **The person domain:**
  - the key's z32 form (`PersonKey::domain()`), or
  - a DNS domain whose `_resonator` TXT lists `k=<person z32>`.
- **`issue_cert` / `verify_cert`:** signatures carry the context `resonator/device-cert/1\0`.
- **`revoke(previous, devices, at)` / `verify_revocations`:**
  - The context is `resonator/revocations/1\0`.
  - Lists are cumulative and seq = previous + 1. A seq of 0 is refused.
- **`KeyStore` trait with `FileKeyStore`:**
  - It stores the phrase words in a mode-600 file, created exclusively through a temp file plus a hard link.
  - It refuses to overwrite a file that holds a different phrase.
  - The default path is `<data_dir>/person.key` (`Config::person_key_path`).
- **`PersonBook`:** in-memory certs, revocation lists, DNS person bindings, static devices and routes (domain → device key). `revocations_changed()` wakes the watcher.
- **`PersonDevices`:** an `EpochAuthority`. It allows a takeover when the holder domain is the wavelet's domain and the holder key is not revoked. The holder's cert binding is checked by `Trust::verify_signer` in `record_of`.
- **DB helpers for the CLI:** `save_cert`, `stored_certs`, `stored_revocations` and `save_revocations`.

### Trust (`trust.rs`)
`Trust::check(key, domain)` tries the cert path first:
- If `key` has a cert from a person P that binds `domain`:
  - revoked → `Authorisation::Revoked`;
  - else `Authorised` when the domain is local or the policy allows it;
  - else `NotAllowed`.
- P binds the domain when one of these holds:
  - the domain is P's z32;
  - the domain is the local domain and P is this node's own person;
  - DNS lists P (cached in the book).

Otherwise the server-mode path (pins and DNS) runs unchanged.

Other methods:
- `verify_signer`: the same rule, but only an unrevoked cert counts. It is used for new deltas.
- `verify_historical_signer`: any cert binding counts, revoked or not. It is used when verifying history (replicas, resubmits and epoch signers), so revocation never rewinds.
- `learn_cert`: verifies, stores to the DB and adds to the book.
- `learn_revocations`: verifies, refuses seq ≤ the known seq (no rollback), stores, and drops routes to revoked keys.
- `load_persons`: loads the DB on start.
- `is_revoked`, `devices_of(domain)`, and `with_persons(Arc<PersonBook>)`.

### Endpoint (`endpoint.rs`)
- `FederationEndpoint::bind_device(key, options, identity, device_cert, authority, handler)`. `bind` delegates to it with no cert.
- **`PeerAuthority` default hooks:** `hello_received`, `complete_hello`, `is_revoked` and `revocations`. `Trust` implements them:
  - it learns the peer's cert and lists before `authorised_domains`;
  - it adds its own seq and list to the answering Hello.
- **Revocation takes effect at once:**
  - `outbound()` refuses revoked keys;
  - an inbound connection from a revoked key is closed when it opens its next stream;
  - `disconnect(&[PublicKey])` cuts open connections, and the revocation watcher calls it.
- `connected_peers()`.

### Signing, hosting and routing
- **Signing:**
  - A person-mode node signs with its device key (the iroh key, as before).
  - Its signer info comes from `device_signer_info(key, domain, cert)`, which carries the cert.
  - Receivers learn the cert from the signer info (`Context::learn_signer_cert`) before verifying.
- **`WaveletHost::Delegated{domain, holder}`** (`resonator-server/src/waveserver/mod.rs`, documented there): the wavelet's domain is this node's own, but another device of the person (`holder`, a device key) hosts it.
  - It is chosen by `host_of` from the wavelet's epoch: a holder key that is not this device.
  - With no epoch, it comes from `PersonHosting{device, holder}` set by `WaveServer::set_person_hosting`.
  - `submit` forwards it exactly like `Remote`, through `RemoteHost`, to the holder's key (`Context::host_key`).
  - A version-0 submit from another device of the person creates the wavelet on the holder (`submit_remote_request_with_receipt`, and the check in `host.rs`).
- **Holder selection:** config-chosen. `[person] holder = "<device z32>"` names the holder, and empty means this device. Holders hold the person key and self-issue their cert.
- **Routing:**
  - `Trust::resolve(domain)` returns the domain's route first: a device key.
  - Routes come from static peers (the first device listed for a domain) and from the host whose updates a replica receives.
  - `Context::targets(domain)` expands a person domain into device targets (`"domain/<device z32>"` outbox entries), excluding self. In person mode the own domain's other devices are included, so replicas on own devices get updates.
- **Addresses:** `name@<person z32>`, or `name@domain` with a DNS binding. The wave domain is the person domain: `FederationSetup::configure_person(&mut config)` sets `config.domain` before the server starts, and `load` refuses a mismatch.

### Config (`[person]`, `resonator-server/src/config/person.rs`)
| key | meaning |
|---|---|
| `enabled` | person mode; needs `[federation] enabled` |
| `key_path` | person key file (default `<data_dir>/person.key`) |
| `device_name` | name in self-issued certs |
| `admin` | this device holds the person key and self-issues its cert at each start |
| `domain` | (extra) a DNS person domain; default is the person key's z32 |
| `holder` | (extra) the device key hosting the person's wavelets; empty = this device |
| `cert` | (extra) hex cert for a non-admin device; alternatively `<data_dir>/device.cert` |

### Revocation spreading (`service.rs`)
`revocation_watcher` runs in person mode. It wakes on `revocations_changed` and polls the DB every `Tuning::revocation_poll` (10 s), so a `person revoke` run against a live server's DB is picked up. For each new list it:
1. disconnects the revoked keys;
2. pushes the `Revocations` op to connected peers, static devices, certified devices and static peers.

Peers also learn lists from each Hello. In code, `Federation::publish_revocations` and `Federation::fetch_revocations` do the same.

### CLI (`crates/resonator/src/main.rs`)
- `resonator person init`: generates the key, writes the phrase file and prints the domain and the phrase once.
- `resonator person recover`: reads the phrase from stdin and writes the key file.
- `resonator person cert <device-key> [--admin] [--name N]`: prints the hex cert and stores it in the DB if one exists.
- `resonator person revoke <device-key>`: writes the next cumulative list to the DB. A running server spreads it.
- `resonator person show [--phrase]`: shows the domain, this device's key and the known devices (revoked ones marked). It prints the phrase only with `--phrase`.
- `serve` calls `configure_person` first.

## Tests
- **Unit tests in `person.rs` (7):**
  - deterministic derivation, with a pinned vector;
  - phrase parsing;
  - cert sign/verify, including a forged key and a wrong person;
  - revocation sign/verify and the seq rules;
  - the key store.
- **`resonator-federation/tests/person_trust.rs` (4):**
  - certs bind only their person's domains;
  - a DNS person domain binds;
  - revocation, including rollback and a restart;
  - own devices speak for the own domain, and another person's device does not.
- **`crates/resonator/tests/persons.rs` (6, in-process, 2 people × 2 devices; each passed 5 runs in a row):**
  - all devices converge;
  - a non-holder submits and creates through the holder (`Delegated`);
  - Q-certified and forged certs for P's domain are refused at Hello;
  - a revoked device is refused at the next Hello and its new submits rejected, while its old delta stays valid and is accepted by a device that comes online later;
  - a key recovered from the phrase on a fresh device self-certifies and is accepted, and its certs verify;
  - takeover between P's devices under `PersonDevices`.
- Workspace: 1233 tests pass (baseline 1216). Clippy `--all-targets` and fmt are clean.

## How phase 3's lease plugs in
- **Holder choice:** today it is static (`[person] holder`). The lease decides it instead:
  - call `WaveServer::set_person_hosting(PersonHosting{device, holder})` when the lease changes hands;
  - call `PersonBook::set_route(own domain, holder key)` so others dial the holder;
  - start epoch + 1 per wavelet through the existing `Federation::take_over` (allowed by `PersonDevices`).
- **`Delegated`:** this is already the "my wave, another device hosts" case. Non-holders forward through `RemoteHost`, so a lease only changes which key `host_key` returns. The phase-3 pending queue should sit in front of the `Delegated`/`Remote` forward in `WaveServer::submit`.
- **Holder key in `HostLease`:** `device` should be a certified, unrevoked device key. `Trust::verify_signer(device, person domain)` is the check.
- **Device sync:** use `Trust::devices_of(domain)` and `Context::targets` as the device set for fan-out and `sync-summary`.

## Open issues
- **Attachments in person mode:** they are stored per device and fetched from the holder only through the existing remote-attachment path. Untested.
- **Per-device state:** accounts, passwords, tokens, the probe store and user-data wavelets are per device. Users register separately on each device (the tests do). Phase 3 sync should cover user data.
- **Routes:** the first static peer listed for a person domain becomes the route. A dead holder is not routed around until a takeover (phase 3).
- **DNS person domains:** they work in `Trust`, but the integration tests use z32 domains only.
- **Self-issued certs:** a self-issued admin cert gets a new `issued_at` on each start. Certs never expire, and only revocation withdraws them.
- **Revocation lists:** they are cumulative and unbounded.
- **Cut-off delay:** a revoked inbound connection is closed when it opens its next stream or when the watcher spreads the list, not instantly while it is idle and the list arrives by another route.
- **Pairing (phase 5):** `person cert` prints hex to copy into the device's config by hand. Pairing should replace this.
