# Resonator v3

Resonator is a peer-to-peer data network in which every node is a SQLite
database with an identity. Apps and agents keep their information in a
node, exchange it as RDF over encrypted QUIC (iroh), and grant each other
exactly what a policy row says. This workspace produces:

- `resonator` core crates: SQLite + iroh p2p node + SPARQL-over-SQLite,
  embeddable in iOS, Android, and desktop apps (wasm intended later).
- `rsntr`: the multi-platform console tool exposing all functions,
  comfortable for LLM agents and shell pipelines, and serving the web
  interface.
- `resonator` Python package (pyo3/maturin), Jupyter/Colab usable.
- The resonator network definition: RDF/SPARQL lingua franca over the RDF
  envelope protocol, iroh as the current base transport, future
  bluetooth/radio behind a transport trait.

## What you can do with it

A node is a database with an identity and a dialable address, so the same
machinery carries rows, files, and live byte streams. Everything below is denied
until a policy row says otherwise, and revoked by deleting that row.

**Pair two devices.** One prints a ticket, the other admits it. No account, no
broker, no port forwarding.

    rsntr serve ~/node                      # prints a live ticket
    rsntr peer add station <ticket>         # on the other device
    rsntr peer status station --probe       # reachable?

**Move data.** Blobs travel by BLAKE3 hash over iroh-blobs as verified
streaming, so a truncated or altered byte is caught rather than trusted.

    rsntr fetch station blake3:<64hex> -o recording.mp4

Structured data moves as itself: `rsntr sparql --peer station '<query>'` runs
against the peer, and `turtle`, `sql` and `csv` load and dump the same rows
locally or across the wire.

**Forward a camera.** Register any command whose stdout is a stream, allow a
peer, and watch it from anywhere the two nodes can reach each other.

    # on the machine with the camera
    rsntr media add ~/node nvr/39 'ffmpeg -i rtsp://cam/1 -f mpegts -' \
        --content-type video/mp2t
    rsntr media allow phone nvr/39

    # anywhere else
    rsntr watch station nvr/39 | ffplay -f mpegts -

**Talk to a device.** A media source that also declares what its stdin accepts
becomes audio-duplex — two-way voice over the same encrypted link.

    rsntr media add ~/node door-talk 'aplay -f S16_LE -r 8000' \
        --accepts 'audio/L16;rate=8000;channels=1' \
        --content-type 'audio/L16;rate=8000;channels=1'

    sox -d -t raw -r 8000 -e signed -b 16 -c 1 - | rsntr talk station door-talk

**Anything else, as bytes.** Named pipes are duplex byte streams between nodes
with a program on each end — a serial port, a log tail, a game, a protocol
nobody has written down.

    rsntr pipe add sensors 'python3 read_sensors.py'   # register an endpoint
    rsntr pipe open station sensors                    # dial it from a peer
    rsntr pipe accept scratch                          # or bridge one terminal, ad hoc

**Build a small network for your own things.** Because a node is embeddable and
the CLI is scriptable, a handful of devices — a laptop, a phone, a Pi in a shed,
a notebook on Colab — become one addressable set that keeps its own data and
answers each other's queries. No server in the middle, and nothing to keep
paying for.

## One node per device

Every app and tool on a device shares one resonator node: one identity,
one database, one set of peers. Apps keep the user's information on the
user's device, in that node, instead of in their own silos, and other
apps on the device and remote peers reach it only through resonator's
verbs and policy.

- At most one process serves the node at a time, under a lease the
  kernel releases on any exit; whoever is awake takes over, and every
  other process reaches it over a local owner channel.
- A write from any process is live for all of them: vibrations, hooks,
  `rsntr await`, and message delivery wake within a quarter second,
  whichever app wrote.
- Apps register what they keep there (`rsntr apps declare`) and find
  each other, and every table's purpose, through the node's own graph
  (`rsntr system graph`).
- On iOS the node is a folder in the Files app that any other app joins
  with one pick; on Android it is shared by the apps of one signing key;
  on desktops `rsntr` uses a per-user node whenever `-d` is omitted.
- Peers see nothing until a `_policy` row says otherwise: keep the data
  on the device, invite the peer through resonator, revoke by deleting a
  row.

The full model, the platform rules, and the lifecycle contract for apps
are in [docs/shared-node.md](docs/shared-node.md).

## For agents

`rsntr` is the agent interface: stable `--json` output, documented exit
codes, and a daemonized node per agent. The whole loop — bootstrap,
pairing, chat with wake-up hooks, SPARQL/Turtle exchange, named binary
pipes — is in [docs/agents.md](docs/agents.md).

    rsntr serve ~/agent-node        # daemonize (auto-inits; idempotent)
    rsntr status ~/agent-node --json
    rsntr hook add message 'my-wake-script'
    rsntr chat wait --timeout 60 --json
    rsntr stop ~/agent-node

Note: since 0.2, `rsntr serve` detaches by default; use
`rsntr serve --foreground` in terminals and systemd units that expect
the old attached behavior.

## Build

    cargo build

## Test

    cargo test --workspace

Format and lint before pushing:

    cargo fmt
    cargo clippy --workspace -- -D warnings

## Licensing

Everything here is dual licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. That covers the whole workspace: the node, `rsntr`, the
Python package, the mod PDK, and the example mods. Nothing in resonator
requires you to open source what you build on it, run it as a service, or
embed it in a closed product.

The permissive choice is deliberate: resonator is a network, and a network is
worth what its reach is worth. Third-party nodes, clients, mods and embedders
are the point.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions. No CLA is required; see CONTRIBUTING.md.
