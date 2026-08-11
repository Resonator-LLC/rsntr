#!/usr/bin/env bash
# Build (if needed) and run the rdfsparql test suite.
# Prefers the sqlite3 CLI for a smoke test when present; the full assertions
# live in test_rdfsparql.py (stdlib-only Python).
set -euo pipefail
cd "$(dirname "$0")/.."

make >/dev/null

if command -v sqlite3 >/dev/null 2>&1; then
  echo "== sqlite3 CLI smoke test =="
  sqlite3 :memory: <<'SQL'
.load ./rdfsparql
.bail on
SELECT 'loaded: ' || rdf_load_turtle_file('examples/sample.ttl');
SELECT rdf_query('PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT ?name WHERE { ?s foaf:name ?name } ORDER BY ?name LIMIT 2');
SELECT json_extract(binding,'$.name') FROM sparql('PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT ?name WHERE { ?s foaf:name ?name }');
SELECT rdf_query('PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX ex: <http://example.org/> CONSTRUCT { ?s ex:hasName ?n } WHERE { ?s foaf:name ?n }');
SQL
  echo "== CLI smoke test OK =="
fi

echo "== Python test suite =="
python3 tests/test_rdfsparql.py
