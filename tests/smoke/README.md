# Smoke-test templates

Provenance: copied from the research project `research/dumbpipe-client`
(talk.sh, stream.sh, media.sh, mux.sh).

These are templates for the v3 external smoke tests, not runnable as-is:
their proto codec must be rebuilt on the v3 protocol crate
(`crates/protocol`, namespace http://resonator.network/v3/rsntr#). The raw
dumbpipe byte pipe they drive works unchanged.
