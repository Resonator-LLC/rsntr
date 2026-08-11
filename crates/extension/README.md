# resonator-extension

A loadable SQLite extension that brings the resonator-sparql surface to any
stock sqlite3. Load the library into a plain sqlite3 CLI, a Python sqlite3
connection, or any host using sqlite3_load_extension(), and the calling
connection gains:

- `rdf_init()` - create the triple-store schema (rdf_terms, rdf_triples)
- `rdf_load_turtle(text [, base])` - load a Turtle document, returns triples added
- `rdf_load_turtle_file(path [, base])` - same, from a file
- `rdf_query(sparql [, format])` - SELECT/ASK as SPARQL 1.1 JSON, CONSTRUCT as Turtle
- `rdf_update(sparql)` - INSERT DATA / DELETE DATA / DELETE WHERE
- `rdf_dump_turtle()` - dump the store as Turtle
- `rdf_regexp(pattern, text [, flags])` - the engine's regex helper
- `sparql('SELECT ...')` - table-valued function, one row per solution,
  column `binding` is a JSON object

All sqlite calls are routed through the host's sqlite3_api_routines
(rusqlite's `loadable_extension` mode); the library links and bundles no
sqlite of its own. Minimum host SQLite: 3.34. The entry point is the
default `sqlite3_extension_init`, and it returns SQLITE_OK_LOAD_PERMANENTLY
so the registered callbacks stay valid for the life of the process.

## Build

This crate is intentionally excluded from the v3 workspace: rusqlite's
`loadable_extension` feature must never unify with the `bundled` feature the
rest of the workspace links (a `--workspace` build would otherwise route
every crate's sqlite calls through a never-initialized api pointer). Build
it standalone:

    cargo build --release --manifest-path crates/extension/Cargo.toml

or from this directory simply `cargo build --release`. The artifact lands in
this crate's own target dir:

- macOS: `crates/extension/target/release/libresonator_extension.dylib`
- Linux: `crates/extension/target/release/libresonator_extension.so`
- Windows: `crates/extension/target/release/resonator_extension.dll`

For the same reason the crate depends on resonator-sparql with
`default-features = false`: with `bundled` in the graph, libsqlite3-sys
would emit bindings pinned to the bundled sqlite's version and the runtime
version check would refuse any older host. Without it the prebuilt 3.34.1
extension bindings apply.

## Load

sqlite3 CLI (macOS):

    sqlite3
    sqlite> .load crates/extension/target/release/libresonator_extension.dylib
    sqlite> SELECT rdf_load_turtle('@prefix ex: <http://example.org/> . ex:a ex:b ex:c .');
    sqlite> SELECT rdf_query('SELECT ?s WHERE { ?s ?p ?o }');

sqlite3 CLI (Linux): same, with `.load .../libresonator_extension.so`.
The `.dylib`/`.so` suffix may be omitted; sqlite tries the platform suffix.

Python:

    import sqlite3
    con = sqlite3.connect("data.db")
    con.enable_load_extension(True)
    con.load_extension("crates/extension/target/release/libresonator_extension.dylib")
    con.execute("SELECT rdf_init()")

Note: some sqlite3 builds ship with extension loading disabled (older macOS
system CLIs, some python.org Pythons). Use a Homebrew/conda sqlite3 or a
Python whose sqlite3 module has `enable_load_extension` in that case.

## Prove

    crates/extension/prove.sh

builds the release dylib, loads it into the system sqlite3 CLI (falling back
to python3), loads `tests/fixtures/rdfsparql/sample.ttl` and asserts the
known 22-triple count and one SELECT result.
