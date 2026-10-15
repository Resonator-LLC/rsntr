# One identity, two endpoints: relay session displacement

**Status:** verified defect; structural fix implemented and verified, one gap left (blob fetch).
**Found:** 2026-09-10, while answering a media measurement question.

## The shape

Every `rsntr` client command binds its own iroh endpoint using the node's
own secret key (`client::client_config` passes `store::load_secret(dir)`).
While such a command runs on a machine whose daemon is serving that same
directory, the machine has **two live endpoints sharing one endpoint id**.

We already stop the client publishing its address record, so it cannot
displace the serving node's discovery record:

```rust
// crates/cli/src/client.rs
publish: false,   // the serving endpoint owns this key's record
```

That is a different hazard from this one. Under the default preset the
client still **joins a home relay**, and a relay keys its sessions by
endpoint id.

## What the relay does

From `iroh-relay-1.0.3/src/server/clients.rs`:

- `register` finds an existing entry for the endpoint id, replaces the
  active client with the newcomer, pushes the old one onto `inactive`,
  and sends it `Status::SameEndpointIdConnected`.
- `send_packet` forwards **only** to `state.active`. An inactive
  connection receives nothing.
- `unregister`, when the *active* client disconnects, pops the most
  recent inactive client, promotes it back to active, and sends it
  `Status::Healthy`.

And from `iroh-1.0.3/src/socket/transports/relay/actor.rs`, the client
side logs the status and takes no action:

```rust
RelayToClientMsg::Status(status) => match status {
    Status::Healthy => info!("Relay server reports: {status}"),
    _ => warn!("Relay server reports problem: {status}"),
},
```

## Consequence, measured

Reproduced on mothership with one daemon serving and one `rsntr watch`
run from the same directory:

| time | event |
| --- | --- |
| 15:46:07 | daemon binds, home relay `euc1-1` |
| 15:46:08 | daemon: "Another endpoint connected with the same endpoint id. No more messages will be received." |
| 15:46:33 | the watch is killed at its 25s timeout; daemon: "The connection is healthy and has recovered from previous problems" |

So the daemon goes deaf about a second into a client command and
recovers the moment that command's endpoint disconnects. **The state is
not sticky** and restarting the daemon is not the remedy.

Confirmed independently on another machine, with a positive control so
that a null result means something:

| command | deafness window |
| --- | --- |
| `await`, 20s | none: no displacement event |
| `knock` (short) | 0.99s |
| `watch`, 20s | 19.8s |

The window equals the command's lifetime, and recovery was automatic in
both positive cases. The `watch` row is the control: it proves the
detection works, so the `await` row means "nothing happened" rather than
"nothing was looked for".

The damage is proportional to duration:

- Short commands (query, knock, chat send) cost a gap of a second or
  two. QUIC keepalive is 5s under a 10s idle timeout, so live relayed
  connections normally survive.
- Long commands hold the daemon deaf for their whole run, far past the
  idle timeout. Every relayed connection the daemon holds dies,
  surfacing as `stream error: connection lost`. The long binding
  commands are `watch`, `talk`, `pipe open`, and `entrain <peer>`.

  `await`, `chat wait` and `chat watch` are **not** among them, though
  they are the longest-running commands we have. They entrain over the
  owner channel (`crate::owner::entrain`) and never bind an endpoint, so
  they displace nothing. That is not an accident of implementation: it
  is the fix below, already built for the local case.
- Two client commands at once on one identity displace **each other**:
  the earlier goes deaf mid-stream and dies.

Direct paths are unaffected throughout. This only touches traffic that
actually rides a relay.

## The 10 second threshold, flagged not actioned

What turns a deafness window into dead connections is the connection
idle timeout, and that one is ours:

| | |
| --- | --- |
| keepalive | 5s (iroh default, inherited) |
| per-path max idle | 15s (iroh default) |
| relay path max idle | 30s (iroh default) |
| **connection max idle** | **10s (`CONN_IDLE_TIMEOUT`, ours)** |

A relay path tolerates 30 seconds of silence at the path level. The
connection dies after 10 seconds of receiving nothing, so 10s is the
boundary between a window that costs a gap and one that kills every
relayed connection the daemon holds. It predicts the measurements above
rather than describing them: 0.99s harmless, 19.8s fatal.

It is the tightest number in the stack by a factor of three, and it is
not only fleet plumbing. An assistant running on a phone and driving a
screen over these links meets cell handovers, lifts, tunnels and
congested venues, which plausibly stall past 10 seconds and rarely past
30. So this is a product tolerance.

Not changed here, deliberately. The blast radius is every connection we
make, and moving a timeout in the same change as the endpoint model
would make a later regression un-bisectable. Whoever picks it up should
evaluate it against what the app needs and check what the app does on
reconnect before touching the constant.

## Seeing it

The daemon logs both events, but the default filter throws them away
(`crates/cli/src/main.rs` builds `EnvFilter::new("warn,iroh=off")`).
To watch a node while measuring:

```
RUST_LOG="warn,iroh::socket::transports::relay=info" rsntr serve <dir> --foreground
```

A deafness window that overlaps a failed open is a diagnosis; one that
does not eliminates this explanation.

## Mitigation today

Run long client commands from a **separate node directory** whose
identity is not also serving, admitted as a peer on the target. A
different key means no session to displace.

## The fix

When a daemon holds the serving lease for a directory, client commands
must not bind a second endpoint for that identity. They should ask the
daemon to make the outbound request over the owner channel, so one
identity means one endpoint, always.

`await` is the proof this works. It holds a subscription open for as
long as the caller wants, through the daemon, over the owner channel,
and binds nothing. What it does not do is reach a *peer*: it entrains a
local point. The missing piece is an outbound leg, not a new pattern.

The owner channel already carries framed envelopes over a byte stream,
and raw media bytes are positional on our streams (everything after the
framed part), so a proxy can be a **byte tunnel** after a one-frame
handshake: the client sends a request naming the peer, the daemon
answers with the peer's `Hello`, and thereafter copies bytes both ways
between the owner connection and the peer's QUIC bi-stream. The client
speaks the ordinary peer protocol over the tunnel, unchanged.

## What is built

Implemented in `crates/cli/src/{owner.rs, client.rs}`:

- The owner channel takes two reserved modulations. `peer-proxy` asks
  the serving process to open a stream to a peer: it answers with that
  peer's `Hello` and then copies bytes both ways until either end is
  done. `peer-path` asks it what path it is currently using, on its own
  short connection so a running session is not disturbed.
- `client::open_peer` returns a `PeerSession` whose stream is either
  `Direct` (this process dialled) or `Proxied` (the daemon did). It
  proxies whenever a lease is held on the directory, which is exactly
  the case where a second endpoint would displace the first.
  `RSNTR_NO_PEER_PROXY=1` forces the direct path.
- `PathProbe` is separable from the stream, so a running feed can be
  sampled while it runs, and for a proxied session it asks the daemon.

Verified with a positive control in the same run, one daemon with relay
logging:

| 20s watch from a serving directory | displacement events |
| --- | --- |
| forced onto the direct path (`RSNTR_NO_PEER_PROXY=1`) | 1 |
| default, through the daemon | 0 |

The control is not decoration. A first run of this showed 0 and 0, which
looks like success and is not: the grep pattern matched the enum name
while the log prints the human sentence, so nothing could ever be
counted. A null result without a control that fires proves only that
nothing was measured.

`run_statement`, `knock`, `run_help`, `run_projection`, `run_entrain`,
`run_media_channel`, `run_duplex` and `run_pages_fetch` all go through
it. Two sites still dial for themselves, deliberately:

- `refresh_peer`, whose whole purpose is to learn a peer's addresses
  from a dial *this* process makes.
- `run_fetch` and `fetch_blob_with_addrs`: blobs ride their own ALPN,
  which this tunnel does not carry. A large fetch is a long command, so
  this is the one long displacement window that remains.
