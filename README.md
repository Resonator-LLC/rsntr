# Resonator v3

A Rust workspace that produces:

- `resonator` core crates: SQLite + iroh p2p node + SPARQL-over-SQLite,
  usable embedded (iOS/Android/desktop apps, ideally wasm later).
- `rsntr`: the multi-platform console tool exposing all functions,
  comfortable for LLM agents and shell pipelines, and serving the web
  interface.
- `resonator` Python package (pyo3/maturin), Jupyter/Colab usable.
- The resonator network definition: RDF/SPARQL lingua franca over the RDF
  envelope protocol, iroh as the current base transport, future
  bluetooth/radio behind a transport trait.

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
