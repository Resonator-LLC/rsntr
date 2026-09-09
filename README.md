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
