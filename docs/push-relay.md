# Push relay: waking sleeping devices

A phone in the background is not on the network, so nobody can deliver to
it. When a node has something for such a device, it asks a small relay
(`resonator-push`) to send it a **silent push**. iOS or Android then wakes
the app for a few seconds. The app calls `enterForeground`, and the node
reconnects and collects what is waiting through federation.

**Wakes carry no content.** A push is `{"aps":{"content-available":1}}` on
APNs or `{"resonator":"wake"}` on FCM. It holds no wave, no sender and no
text. Apple and Google learn only that a device was woken, and when.

The relay keeps no state. Its rate limits live in memory. It stores no
tokens and logs no tokens, sealed tokens, keys or JWTs; logs show only the
platform and the outcome.

## The flow

1. **Seal.** The app gives its node the platform push token
   (`setPushToken`, FFI). If `[push] relay` is set, the node sends the token
   to its relay (`POST /seal`) and gets back a *sealed token*. Only that
   relay can open it. The node seals again when the token or the relay
   changes. The raw token stays on the device's own node and the relay.
2. **Advertise.** The node signs a `PushTarget {device key, relay, sealed,
   issued_at}` with its device key. It sends the target in a `push-targets`
   request (op 36) to its person's other devices and to the hosts of the
   waves it is in.
   - It re-sends when a target changes, and otherwise every six hours.
   - A person's devices pass each other's targets on, so a laptop that is
     up can tell a host how to wake the phone.
   - Receivers keep the newest target per device (migration 16,
     `push_targets`). They accept a target only for the sending device or
     for another device of the same person domain.
   - An empty `sealed` withdraws the target, which happens when the app
     clears its token.
3. **Wake.** A node sends `POST /wake` to the device's relay, signed with
   the node's federation key, in three cases:
   - an update for one of the device's waves cannot be delivered, and no
     other device of that person is connected (the outbox);
   - device sync with an own device fails and something changed since the
     last sync;
   - a lease handover target does not answer.

   The node then retries soon: the outbox every `wake_retry` (2 s) for
   `wake_window` (30 s), and a handover for up to `wake_wait` (20 s).
4. **Forget.** If the relay answers `gone` (the token is dead), the node
   deletes the target.

A node wakes a device at most once per 2 minutes. While wakes go
unanswered, the gap doubles each time, up to 1 hour. It drops back to
2 minutes once the device is reached again.

A node only calls relays it is configured for: its own `relay` and
`other_relays`. A target that names any other relay is kept but never
called, so a peer cannot make the node send requests to arbitrary URLs.

## Relay API

All requests and answers are JSON. Bodies are limited to 16 KiB.

### `GET /health`

Returns `{"status":"ok","apns":true|false,"fcm":true|false}`.

### `POST /seal`

Request: `{"platform":"apns"|"apns-sandbox"|"fcm","token":"…"}`.

| Answer | When |
|---|---|
| `200 {"sealed":"…"}` | Sealed. |
| `400 {"status":"bad_request"}` | The body is not valid JSON. |
| `400 {"status":"bad_platform"}` | Unknown platform. |
| `400 {"status":"bad_token"}` | The token is malformed. APNs tokens are hex (16 to 200 characters); FCM tokens are at most 4 KiB of `[A-Za-z0-9:_-]`. |
| `429 {"status":"rate_limited"}` | Too many requests from this IP address. |

APNs tokens are lower-cased before sealing.

**Sealing.** The sealed token is `base64url(0x01 ‖ nonce[12] ‖
AES-256-GCM(key, nonce, aad = "resonator/sealed/1", {"p":platform,"t":token,"i":issued_at}))`,
with a random nonce. Sealing the same token twice gives different blobs, so
a sealed token reveals nothing and cannot be linked to another one.

- The 32-byte key is stored as 64 hex characters in the `sealing_key` file.
  The relay creates the file with mode 600 the first time it starts.
- Losing the key, or using a different one, makes every sealed token
  unreadable. Devices then reseal only when their token or relay changes,
  so keep the file.

### `POST /wake`

Request: `{"sealed":"…","sender":"<key>","ts":<unix seconds>,"sig":"<base64url>"}`.

- `sender` is the calling node's Ed25519 federation key, as 52
  z-base-32 characters (iroh's `to_z32`) or 64 hex characters.
- `sig` is the Ed25519 signature of `"resonator/wake/1\0" ‖ sealed (UTF-8
  bytes) ‖ ts (i64 big-endian)`.

The relay checks the request in this order:

| # | Check | Answer if it fails |
|---|---|---|
| 1 | Per-IP rate limit | `429 rate_limited` |
| 2 | Valid JSON | `400 bad_request` |
| 3 | `ts` within ±5 min of now | `401 stale` |
| 4 | `sender` parses as a key | `400 bad_sender` |
| 5 | Signature verifies | `401 bad_signature` |
| 6 | The same signature was not seen in the last 10 min | `409 replay` |
| 7 | Per-sender rate limit | `429 rate_limited` |
| 8 | The sealed token opens | `400 bad_blob` |
| 9 | Per-device rate limit (keyed by the token, so it holds across reseals) | `429 rate_limited` |

Then it pushes. The outcome is one of:

- `200 sent`.
- `410 gone`: the token is dead. This is APNs `410`, `400 BadDeviceToken`
  or `400 DeviceTokenNotForTopic`; or FCM `404` / `UNREGISTERED`, or `400`
  about the registration token.
- `502 failed`: any other refusal or network error.
- `503 unsupported`: the token's push service is not configured.

**Rate limits.** These are token buckets: a burst, then one request per
interval. All are configurable under `[limits]`.

| Limit | Burst | Then |
|---|---|---|
| `/seal` per IP | 20 | 1 per 10 s |
| `/wake` per IP | 120 | 1 per 0.5 s |
| `/wake` per sender | 20 | 1 per 10 s |
| `/wake` per device | 3 | 1 per 20 s |
| `/wake` per device, long term | 30 | 1 per 2 min (about 30 per hour) |

At most `max_keys` (100 000) keys are tracked per limit. Full buckets are
forgotten first. If the table is still full, new keys are refused.

**What goes to the push services.**

- **APNs:**
  - Sent over HTTP/2 as `POST {endpoint}/3/device/<token>`.
  - Headers: `authorization: bearer <ES256 JWT {iss: team, iat}, kid = key ID>`
    (the JWT is cached for 50 minutes and remade on
    `ExpiredProviderToken`), `apns-push-type: background`,
    `apns-priority: 5`, `apns-topic: <topic>`.
  - Body: `{"aps":{"content-available":1}}`.
  - `apns-sandbox` tokens go to the sandbox endpoint and `apns` tokens to
    production. The sealed platform decides which.
- **FCM:**
  - `POST {endpoint}/v1/projects/<project>/messages:send` with
    `{"message":{"token":…,"data":{"resonator":"wake"},"android":{"priority":"high","ttl":"300s"}}}`.
  - The OAuth access token comes from the service account: an RS256 JWT
    assertion to its `token_uri`, scope `firebase.messaging`. It is cached
    until 5 minutes before it expires.

## Node configuration

```toml
[push]
relay = "http://mothership:9900"   # unset or empty: push is off
other_relays = []                  # other relays this node may call to wake devices
```

You can also set the relay with the environment variable
`RESONATOR_PUSH_RELAY` or the flag `--push-relay`.

- **Push off (no relay).** Nothing is sealed, advertised, stored or woken.
  `push-targets` requests are refused, the token the app gives is only kept
  in `<data dir>/push-token`, and `resonator config` output is unchanged.
- **A host must also set a relay.** A host only wakes devices if it has
  `[push] relay` too, and the relay named in the device's target must be
  its own or listed in `other_relays`.

For the app, `setPushToken` and `clearPushToken` (FFI) are unchanged. With
a relay configured, the token is sealed when the node starts and whenever
it changes.

Tuning (`resonator_federation::Tuning`, defaults): `wake_interval` 120 s,
`wake_retry` 2 s, `wake_window` 30 s, `wake_wait` 20 s, `push_advertise`
60 s.

## Relay configuration

TOML file (`--config`, or `RESONATOR_PUSH_CONFIG`), every key optional:

```toml
listen = "0.0.0.0:9900"
sealing_key = "/home/rafael/.config/resonator-push/sealing.key"
trust_forwarded_for = false        # true only behind a reverse proxy

[apns]                             # off while key_path is empty
key_path = "/home/rafael/.config/resonator-push/AuthKey_ABC123DEFG.p8"
key_id = "ABC123DEFG"
team_id = "569TWLS7W8"             # example: Rafael's Apple team
topic = "am.resonator.client.ios"        # the default
endpoint = "https://api.push.apple.com"                  # default
sandbox_endpoint = "https://api.sandbox.push.apple.com"  # default

[fcm]                              # off while service_account_path is empty
service_account_path = "/home/rafael/.config/resonator-push/firebase-sa.json"
project_id = ""                    # default: the service account's

[limits]
wake_per_device = { burst = 3, every_ms = 20000 }
```

Environment overrides:

- General: `RESONATOR_PUSH_LISTEN`, `_SEALING_KEY`, `_TRUST_FORWARDED_FOR`.
- APNs: `RESONATOR_PUSH_APNS_KEY_PATH`, `_APNS_KEY_ID`, `_APNS_TEAM_ID`,
  `_APNS_TOPIC`, `_APNS_ENDPOINT`, `_APNS_SANDBOX_ENDPOINT`.
- FCM: `RESONATOR_PUSH_FCM_SERVICE_ACCOUNT`, `_FCM_PROJECT_ID`,
  `_FCM_ENDPOINT`, `_FCM_TOKEN_URI`.

Secrets are only ever read from files named by path. Do not put them in
the repository, and do not put a team ID or key ID in a committed config.

`resonator-push --check` loads the configuration and keys, prints what is
enabled and exits.

### APNs setup (Rafael)

1. In the Apple Developer account (team `569TWLS7W8`), go to **Certificates,
   Identifiers & Profiles → Keys** and create a key with **Apple Push
   Notifications service (APNs)** enabled.
2. Download `AuthKey_<KEYID>.p8`. It can be downloaded only once.
3. Put it on mothership, for example in `~/.config/resonator-push/`, and
   run `chmod 600` on it.
4. Note the **Key ID** (10 characters) and the **Team ID**.
5. Set the topic to the app's bundle ID, `am.resonator.client.ios` (the default).
6. The App ID needs the Push Notifications capability, and the app needs the
   background mode *Remote notifications*.

Development-signed builds (`aps-environment development`) get **sandbox**
tokens. The app reports them as `apns-sandbox`, and the relay sends them to
`api.sandbox.push.apple.com`. App Store and TestFlight builds get
production tokens, `apns`. One `.p8` works for both.

### FCM setup (later, Android)

1. Create a Firebase project.
2. Under **Project settings → Service accounts**, generate a private key
   (JSON).
3. Store it with mode 600 and set `fcm.service_account_path`. The project
   ID is read from it.

## Running on mothership

Build:

```sh
CARGO_TARGET_DIR=~/.cache/resonator-push-target cargo build --release -p resonator-push
```

Run:

```sh
mkdir -p ~/.config/resonator-push && chmod 700 ~/.config/resonator-push
RESONATOR_PUSH_SEALING_KEY=~/.config/resonator-push/sealing.key \
RESONATOR_PUSH_APNS_KEY_PATH=~/.config/resonator-push/AuthKey_<KEYID>.p8 \
RESONATOR_PUSH_APNS_KEY_ID=<KEYID> \
RESONATOR_PUSH_APNS_TEAM_ID=569TWLS7W8 \
~/.cache/resonator-push-target/release/resonator-push
```

The relay listens on `0.0.0.0:9900`, so it is reachable over the tailnet at
`http://mothership:9900` (fallback `http://100.127.138.26:9900`). Don't
open the port in ufw. Check it with `curl http://mothership:9900/health`,
then set `[push] relay = "http://mothership:9900"` on the nodes. It stops
on SIGINT or SIGTERM.

**Tailnet only for now.** A phone reaches the relay only while it is on the
tailnet, but sealing happens only when the token changes, and the callers
of `/wake` are the nodes, which are on the tailnet.

**For production**, the relay needs a public HTTPS name. Put it behind a
TLS-terminating reverse proxy (for example Caddy) with
`trust_forwarded_for = true`, and point `[push] relay` at that URL. The
relay itself speaks plain HTTP.

## Security notes

- **A stolen sealed token** lets its holder ask the relay to wake that
  device, at most within the per-device limits, and nothing else. Every
  wake must be signed by some federation key; the signature makes requests
  attributable and lets the relay rate-limit per sender. It is not an
  allowlist: any key can sign.
- **Peers cannot forge targets.** A node can't plant a target for another
  device. Targets are signed by the device key, and receivers accept them
  only from that device or from a device of the same person domain.
- **Dead tokens** are reported as `gone` and dropped everywhere they are
  used.
