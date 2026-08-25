# SPARQL conformance fixtures

The behavioral oracle for `crates/sparql`: the Rust SPARQL-over-SQLite engine
must reproduce the results these fixtures pin (same inputs, same expected
JSON/Turtle shapes). The assertions themselves live in
`crates/sparql/tests/conformance.rs`.

Files:

- `sample.ttl`: example Turtle data, 22 triples.
