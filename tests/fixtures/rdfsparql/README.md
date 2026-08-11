# rdfsparql conformance fixtures

Provenance: extracted from the research artifact
`vecsrchrdf-claude-rdf-sqlite-sparql-pi0ama.zip` (rdfsparql/tests and
rdfsparql/examples members), the test suite of the zero-dependency C SQLite
extension `rdfsparql.c`.

These files are the conformance oracle for `crates/sparql`: the Rust
SPARQL-over-SQLite engine must reproduce the behavior these tests assert
(same inputs, same expected JSON/Turtle shapes).

Files:

- `test_rdfsparql.py`: 37 behavioral assertions against the C engine.
- `run_tests.sh`: original test runner for the C extension.
- `sample.ttl`: example Turtle data.
