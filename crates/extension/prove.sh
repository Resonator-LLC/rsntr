#!/bin/sh
# Proof for the loadable extension: build it, load it into a stock sqlite3
# (CLI .load first, python3 sqlite3 fallback), load the sample fixture and
# check the known 22-triple count plus one SELECT result.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
fixture="$root/tests/fixtures/sparql/sample.ttl"

cargo build --release --manifest-path "$here/Cargo.toml"

case "$(uname -s)" in
    Darwin) lib="$here/target/release/libresonator_extension.dylib" ;;
    *)      lib="$here/target/release/libresonator_extension.so" ;;
esac
[ -f "$lib" ] || { echo "FAIL: $lib was not built" >&2; exit 1; }

query="PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT ?name WHERE { <http://example.org/alice> foaf:name ?name }"

run_cli() {
    sqlite3 :memory: \
        ".load $lib" \
        "SELECT rdf_load_turtle_file('$fixture');" \
        "SELECT rdf_query('$query');" \
        2>/dev/null
}

run_py() {
    python3 - "$lib" "$fixture" "$query" <<'EOF'
import sqlite3, sys
lib, fixture, query = sys.argv[1], sys.argv[2], sys.argv[3]
con = sqlite3.connect(":memory:")
con.enable_load_extension(True)
con.load_extension(lib)
print(con.execute("SELECT rdf_load_turtle_file(?)", (fixture,)).fetchone()[0])
print(con.execute("SELECT rdf_query(?)", (query,)).fetchone()[0])
EOF
}

out=""
loader=""
if command -v sqlite3 >/dev/null 2>&1; then
    out="$(run_cli || true)"
    loader="sqlite3 CLI $(sqlite3 --version | cut -d' ' -f1)"
fi
if [ -z "$out" ] && command -v python3 >/dev/null 2>&1; then
    out="$(run_py)"
    loader="python3 sqlite3"
fi
if [ -z "$out" ]; then
    echo "FAIL: no loader available (need a sqlite3 CLI with .load enabled" \
         "or a python3 whose sqlite3 module has enable_load_extension)" >&2
    exit 1
fi

count="$(printf '%s\n' "$out" | sed -n 1p)"
result="$(printf '%s\n' "$out" | sed -n 2p)"
if [ "$count" != "22" ]; then
    echo "FAIL: expected 22 triples from sample.ttl, got: $count" >&2
    exit 1
fi
case "$result" in
    *'"name":{"type":"literal","value":"Alice"}'*) ;;
    *) echo "FAIL: unexpected SELECT result: $result" >&2; exit 1 ;;
esac

echo "PASS: loaded $lib into $loader; 22 triples, SELECT returned Alice"
