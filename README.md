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

## Build

    cargo build

## Test

    cargo test --workspace

Format and lint before pushing:

    cargo fmt
    cargo clippy --workspace -- -D warnings

## Licensing

- The workspace (the node and everything that runs it) is licensed under
  AGPL-3.0-only. See LICENSE.
- Commercial licenses are available separately for organizations that cannot
  comply with the AGPL's network copyleft.
- The Python package (`crates/python`) and the future mod PDK crate are MIT
  licensed, so writing clients and mods never requires AGPL compliance.
- External contributions require signing a CLA so the single-copyright-holder
  position (and thus dual licensing) is preserved. See CONTRIBUTING.md.
