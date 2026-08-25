# time mod

The M6 validation plugin: a Resonator mod that answers any request with one
row holding the node's current UTC time (column `now`, ISO-8601 with
nanoseconds), derived from the `now_ns` host function. Declares the `clock`
capability in `describe()`.

Standalone cargo project (not a workspace member); uses `resonator-mod-pdk`
via a path dependency.

## Build

    cd examples/time-mod
    cargo build --target wasm32-unknown-unknown --release

Output:

    target/wasm32-unknown-unknown/release/time_mod.wasm
