# Pairing by code: handoff

A new device can now pair by typing a short code such as `482913-ocean-tiger`, beside the `resonator:pair/…` link and QR code (which work as before). Every invitation gets both, unless the inviting device cannot reach the pairing rendezvous, in which case it has a link only.

Nothing changes with person mode off: no invitations are made and no rendezvous is contacted. With federation `discovery = "off"` and no `pairing_rendezvous` set, codes are off too, and invitations carry only a link (all tests that predate this keep their behaviour and stay offline).

## The code

`<nameplate>-<word>-<word>`:
- **Nameplate:** 6 random digits (uniform, rejection-sampled). It is not secret; it only finds the inviting device's rendezvous record. That gives 10^6 slots. Before publishing, the inviting device checks that the slot is free, both locally and on the rendezvous, and tries up to 5 nameplates.
- **Words:** 2 words from the BIP39 English list (2048 words, 11 bits each, 22 bits in all). They are the PAKE password.

**Why 2 words.** SPAKE2 allows no offline guessing: each guess costs a protocol run with the inviting device, and the invitation burns after 3 wrong ones. An attacker who knows the nameplate succeeds with probability at most 3/2^22, about 7×10^-7, per invitation, and an invitation lives 10 minutes. A third word would bring that to about 3.5×10^-10, at the cost of a code that is a third longer to read out and type on a phone. Magic Wormhole makes the same trade (2 words by default). The word count is a constant (`CODE_WORDS` in Rust and TS) if more margin is wanted.

**Reading.** Case does not matter. Parts may be separated by hyphens, spaces or both, and surrounding space is trimmed. A word may also be given by its first 4 or more letters (BIP39 words are unique in their first four). Rust: `pair_code::PairCode::parse`. TS: `client/src/net/pairCode.ts`, which mirrors it.

## Rendezvous (`crates/resonator-federation/src/pair_code.rs`)

**Key.** Seed = HKDF-SHA256(ikm = `"resonator/pair-nameplate/1\0"` ‖ nameplate, info `"ed25519 seed"`), used as an Ed25519 secret key (`nameplate_key`). Anyone can derive it. A unit test pins the derivation.

**Record.** A pkarr signed packet with TXT records at `_resonator-pair`:
- `v=1`;
- `node=<z32 key>`;
- `exp=<ms>`, the invitation's expiry;
- `relay=<url>` (optional);
- `addr=<ip:port>`, at most 12.

The joiner ignores a record that is expired, empty or unreadable.

**Transport.** The pkarr relay HTTP protocol: `PUT`/`GET <relay>/<z32 key>`, with the body = signature ‖ timestamp ‖ DNS packet, and 404 meaning "none". This is the same format and the same relay that iroh's own address lookup uses. A `Rendezvous` trait has two implementations:
- `PkarrRendezvous`, over reqwest with a 10 s timeout;
- `MemoryRendezvous`, for tests.

Tests inject one with `FederationSetup::with_rendezvous`; otherwise it comes from the configuration.

**Configuration.** `[federation] pairing_rendezvous`:
- `default`: n0's `https://dns.iroh.link/pkarr` when `discovery` is on, none when it is off;
- `off`;
- or an http(s) URL of a pkarr relay.

The CLI uses the same setting (`control::config_rendezvous`).

**Removal.** When the invitation is used, burnt or cancelled, the inviting device publishes an empty packet with a newer timestamp. This happens in the background, except for cancel, which awaits it so that the CLI does not exit first. If the removal fails, the record goes inert at `exp`.

**Hijacking.** Anyone can derive the nameplate key, so anyone can overwrite the record. That only sends the joiner to a device that cannot complete the PAKE: it is a denial of service, never a way in.

**LAN/mDNS:** skipped. In iroh 1.4 mDNS is a separate crate (`iroh-mdns-address-lookup`) with its own service; adding it only for pairing was not cheap.

## Proof (SPAKE2, `spake2` 0.4, Ed25519 group)

All of it runs on the existing `PAIR_ALPN` (`resonator/pair/1`), one request per stream.

1. **Joiner → inviter: `PairCode{nameplate, pake}`** (new request op, tag 35).
   - The joiner is side A, the inviting device side B.
   - The password is the canonical `word-word` in lower case.
   - The identities are `"resonator/pair-code/1 joiner\0"` ‖ nameplate ‖ `\0` ‖ joiner key, and the same with `inviter` and the inviter key. The joiner key is the QUIC connection's.
2. **The inviting device answers.** It looks the invitation up by nameplate: not used, not expired, fewer than 3 code failures, otherwise 404 "code not found". It finishes SPAKE2 and derives the **session key**:
   - HKDF-SHA256(PAKE key, info `"resonator/pair-code/1 session\0"` ‖ len ‖ invitation id ‖ person key ‖ admin byte ‖ len ‖ account);
   - it keeps the session in memory, keyed by the joiner's key, up to 64 sessions, until the invitation expires;
   - it answers `PairCodeResult{pake, invitation, person, admin, account, expires_at}` (response tag 33). Nothing in that answer can confirm a guess.
3. **Joiner → inviter: the existing `Pair{invitation, mac}`.** The MAC is computed as for links, with the session key in place of the link secret.
   - **Wrong MAC:** `code_failures` goes up (migration 15) and the session is dropped. At 3, the invitation is deleted and its record withdrawn. The answer is `PairResult{tries_left}`, which the joiner turns into "wrong code (N tries left)".
   - **One at a time:** checks run under a mutex, so no more than 3 wrong ones ever get through.
   - **What counts:** only wrong MACs. A PAKE that is started and abandoned costs nothing and reveals nothing.
4. **Key confirmation by the inviting device.** Every `PairResult` of a code session, including the "waiting for confirmation" ones, carries `confirm` = HMAC(session, `"resonator/pair-code/1 inviter\0"` ‖ inviter ‖ joiner ‖ id). The joiner verifies it before trusting anything. This matters because, unlike with a link, the joiner does not know the person key in advance.
5. **Then everything works as for links:**
   - the certificate is issued, and the joiner checks that it is for this device and for the person from step 2;
   - an admin pairing still needs confirmation on the inviting device;
   - the person key travels XORed with `seal_pad(session, …)`;
   - the joiner refuses an invitation for another account (the account is also bound in the session key).

Link mode is unchanged: 5 wrong secrets, using the `failures` column. A link of an invitation that also has a code works until either one is used.

**Secrets.** The code, its words and the session key are never logged. `PairCode`, `NewInvitation` and `PairInvite` (FFI) leave them out of `Debug`. `pair_invitations.code` holds the words only while the invitation lives, as the link secret already did.

## Surfaces

**CLI**
- `resonator person pair invite [--admin]` prints the code (when published) and the link.
- `person pair join <code-or-link>` accepts a code as one argument or as several words, and prints its stages to stderr.
- `person pair cancel <id>` also withdraws the record.

**HTTP**
- `POST /person/pair` answers `code`, which is null when there is no code.
- `POST /person/pair/join` takes `{code}` or `{link}`, plus `name`. Errors:
  - malformed code: 400;
  - code not found or expired: 404;
  - wrong code (N tries left) and other refusals: 409.
- **New:** `GET /person/pair/join` returns `{stage}`, one of `finding`, `verifying`, `waiting` or `done`, or null when no join runs. The settings page polls it.

**FFI**
- `pairInvite(admin:)` now returns a `PairInvite{id, link, code?, expiresAtMs, admin}` record instead of a string.
- `pairJoin(codeOrLink:…)` takes either.
- The Swift and Kotlin bindings were regenerated, and the README table was updated.

**Node:** `Node::pair_invite` returns `NewInvitation`, and `pair_join` takes a code or a link.

**GUI** (`client/src/ui/settings/`)
- **Pair a device:**
  - the code is shown large, in monospace (`clamp(1.375rem, 6.5vw, 2rem)`); the number is accented and the parts are spaced, wrapping between parts and never inside one;
  - a "Copy code" button;
  - "Expires in m:ss", ticking each second;
  - below that, "Or scan this QR code, or paste the link" with the QR code and link as before.

  Without a code, it explains that only the QR code and link are available.
- **Join with a code** (the old "Join with a link" section):
  - the code field: `autocapitalize=off`, `autocorrect=off`, `spellcheck=false`, `inputmode=text`, `enterkeyhint=go`, 16px monospace;
  - up to 4 BIP39 suggestions for the word being typed, which complete it on tap;
  - a hint as you type ("\"tigr\" is not one of the code words.", "The number has 6 digits.");
  - a link field under it ("Or a pairing link");
  - progress steps: finding your device, verifying the code, waiting for confirmation on your device, done;
  - errors: "Code not found or expired. Check the digits, or make a new code on your other device." and "Wrong code (2 tries left)".

  The BIP39 list is bundled in `client/src/net/bip39English.ts`.

## Files

**Rust**
- New: `crates/resonator-federation/src/pair_code.rs`.
- Changed:
  - `crates/resonator-federation/`: `src/pairing.rs` (Inviter: rendezvous, code sessions, `start_code`, code mode in `answer`; joiner: shared `exchange`, `join_code`, `join_with_progress`, `JoinStage`), `src/endpoint.rs` (`PairService::pair_code`, dispatch), `src/control.rs` (`join_any`, progress, `config_rendezvous`, `offline_invite`/`offline_join` with a rendezvous, `offline_cancel`), `src/service.rs` (`with_rendezvous`), `src/lib.rs`, `Cargo.toml` (`spake2 0.4`, `iroh-dns 1.4.1`, `reqwest 0.12`);
  - `crates/resonator-model/src/proto/federation_rpc.rs` (`PairCode`, `PairCodeResult`, `PairResult.confirm`/`tries_left`);
  - `crates/resonator-server/`: `src/storage/migrations.rs` (migration 15: `nameplate`, `code`, `code_failures` and an index), `src/storage/federation_store.rs`, `src/config/federation.rs`, `src/config.rs`, `src/http/person_handlers.rs`, `src/http/mod.rs`;
  - `crates/resonator/src/main.rs`;
  - `crates/resonator-node/src/{lib,handle}.rs`;
  - `crates/resonator-ffi/src/lib.rs`, `README.md`, `bindings/`.

**Client**
- New: `src/net/pairCode.ts`, `src/net/bip39English.ts`, `src/net/tests/pairCode.test.ts`.
- Changed: `src/net/person.ts`, `src/net/index.ts`, `src/ui/settings/{SettingsPage.tsx,settingsStore.ts,settings.test.tsx}`, `src/ui/styles/settings.css`, `src/net/tests/person.test.ts`, `e2e/app/support/servers.ts` (a pkarr relay in miniature for person mode), `e2e/app/settings.spec.ts`.

## Tests

**Rust unit tests** (`pair_code.rs`, 10, 1 of them ignored):
- generating and reading codes, including loose typing and refusals;
- the nameplate key: stable, distinct and pinned;
- records round-tripping as signed packets;
- the memory rendezvous: newest wins, expiry, withdrawal;
- `PkarrRendezvous` against an in-process HTTP relay (404, withdrawal, unreachable);
- SPAKE2: agreement on the right code only, and identities binding the keys and the nameplate;
- the session key binding the id, person, admin flag and account;
- confirmation tags.

The config test is `the_pairing_rendezvous_follows_discovery_by_default`.

**Integration test.** `crates/resonator/tests/persons.rs::a_device_pairs_by_code_and_a_code_takes_three_wrong_tries` runs in-process nodes on a shared `MemoryRendezvous`, with no internet. It covers:
- a malformed code and an unknown code;
- wrong words, with "2 tries left" and "1 try left" and the counter at 1 in the database;
- the right code typed in upper case with spaces;
- reuse, which gets "not found", and the record being withdrawn;
- burning after 3 wrong codes (the right code is then not found, and the link is dead too);
- expiry at the inviting device;
- a cancelled code;
- an admin pairing by code that waits for confirmation and then installs the person key;
- a restart and sync of the wave.

It passed on 4 runs, together with the link test, which now also runs with codes published. `crates/resonator-node/tests/device.rs` additionally checks the HTTP surface: `code` is null without a rendezvous, a malformed code gives 400, codes off gives 409, an empty body gives 400, and `GET` progress gives null.

**Counts**
- `cargo test --workspace`: 1272 passed, 0 failed, 3 ignored (the ignored ones include the 2 real-internet tests). The baseline was 1261.
- `clippy --workspace --all-targets`: clean.
- `fmt --check`: clean.
- Client `npm test`: 1210 passed (baseline 1200); tsc and build are clean.
- Playwright: 37 passed, run with release `resonator-server` and a release `resonator` built into `~/.cache/resonator-e2e-target`, outside the synced tree. The settings and device specs passed again on a second run. The first full run had one failure in `device.spec.ts` that did not come back.

**Real internet** (ignored tests, run by hand; both pass):
- `cargo test -p resonator-federation --lib -- --ignored n0_relay` publishes, resolves and withdraws a record on `https://dns.iroh.link/pkarr`.
- `cargo test -p resonator --test persons -- --ignored over_n0` pairs two in-process nodes by code through n0's relay; the devices themselves talk over loopback.

## Deviations and notes

- **Not the `pkarr` crate.** iroh 1.4 implements pkarr itself in `iroh-dns` (`iroh_dns::pkarr::SignedPacket`). We use that, plus a small reqwest client for the relay protocol, rather than the `pkarr` crate, which would pull in mainline-DHT dependencies. The wire format and the relay are the same.
- **Progress over HTTP.** Join progress is reported through a new `GET /person/pair/join`, and `PersonControl` has a new method, `join_progress`.
- **FFI change.** `pair_invite` now returns a record instead of a string, which breaks the API. There were no callers in the repository.
- **Join section.** The settings section is now titled "Join with a code" and keeps the link field under the code field.

## Open issues

- **Enumeration.** Someone who probes nameplates (10^6 of them) can learn, for a live invitation, the inviting device's address, the person key (public anyway) and the account name, from `PairCodeResult`. This does not help with guessing, which is still capped at 3. Withholding `account` until after key confirmation would close the account-name part.
- **Session eviction.** A flood of joiner keys can evict a legitimate PAKE session between its two requests (a DoS on the pairing only). The cap is 64.
- **Rate limits.** n0's relay rate-limits by IP. Each invitation costs one GET (the free-slot check) and one PUT, plus one PUT when the code is used.
- **No LAN discovery.** A device without internet access cannot pair by code; it has to use the link or QR code.
- **No real-internet pairing in CI.** The real-relay paths are covered only by the two ignored tests, run by hand. Two devices on different networks, reaching each other through an iroh relay, were not tried.
