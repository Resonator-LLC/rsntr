#!/usr/bin/env python3
"""Tests for the rdfsparql SQLite extension.

Uses only the standard library.  Skips (exit 0 with a notice) if this Python
build cannot load SQLite extensions.
"""
import json
import os
import sqlite3
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
EXT = os.path.join(ROOT, "rdfsparql")
SAMPLE = os.path.join(ROOT, "examples", "sample.ttl")

# 22 triples: alice 6, bob 4, carol 4, dave 2, collection 5, bnode label 1
SAMPLE_TRIPLES = 22

FOAF = "http://xmlns.com/foaf/0.1/"
EX = "http://example.org/"


def open_store():
    conn = sqlite3.connect(":memory:")
    conn.enable_load_extension(True)
    conn.load_extension(EXT)
    conn.enable_load_extension(False)
    return conn


class RdfSparqlTests(unittest.TestCase):
    def setUp(self):
        self.db = open_store()
        (self.loaded,) = self.db.execute(
            "SELECT rdf_load_turtle_file(?)", (SAMPLE,)
        ).fetchone()

    def tearDown(self):
        self.db.close()

    def q(self, sparql, fmt=None):
        if fmt is None:
            (out,) = self.db.execute("SELECT rdf_query(?)", (sparql,)).fetchone()
        else:
            (out,) = self.db.execute(
                "SELECT rdf_query(?,?)", (sparql, fmt)
            ).fetchone()
        return out

    def rows(self, sparql):
        doc = json.loads(self.q(sparql))
        return doc["results"]["bindings"]

    # ------------------------------------------------------------- loading --

    def test_load_counts_triples(self):
        self.assertEqual(self.loaded, SAMPLE_TRIPLES)

    def test_reload_is_idempotent_for_ground_triples(self):
        # Blank nodes get fresh labels per load, so only the 6 bnode-involving
        # triples (5 collection + 1 label) are re-inserted.
        (again,) = self.db.execute(
            "SELECT rdf_load_turtle_file(?)", (SAMPLE,)
        ).fetchone()
        self.assertEqual(again, 6)

    def test_load_turtle_text_with_base(self):
        db = open_store()
        (n,) = db.execute(
            "SELECT rdf_load_turtle(?, ?)",
            ("<s> <p> <o> .", "http://base.example/dir/"),
        ).fetchone()
        self.assertEqual(n, 1)
        rows = json.loads(
            db.execute(
                "SELECT rdf_query('SELECT ?s WHERE { ?s ?p ?o }')"
            ).fetchone()[0]
        )["results"]["bindings"]
        self.assertEqual(rows[0]["s"]["value"], "http://base.example/dir/s")
        db.close()

    def test_parse_error_reports_position(self):
        with self.assertRaises(sqlite3.OperationalError) as cm:
            self.db.execute("SELECT rdf_load_turtle('<a> <b> .')")
        self.assertIn("line", str(cm.exception))

    # ------------------------------------------------------------- SELECT --

    def test_basic_select(self):
        rows = self.rows(
            "PREFIX foaf: <%s> SELECT ?name WHERE { ?s foaf:name ?name } "
            "ORDER BY ?name" % FOAF
        )
        names = [r["name"]["value"] for r in rows]
        self.assertEqual(names, ["Alice", "Bob", "Carol", "Dave"])

    def test_join_on_shared_variable(self):
        rows = self.rows(
            "PREFIX foaf: <%s> PREFIX ex: <%s> "
            "SELECT ?name WHERE { ex:alice foaf:knows ?p . ?p foaf:name ?name } "
            "ORDER BY ?name" % (FOAF, EX)
        )
        self.assertEqual([r["name"]["value"] for r in rows], ["Bob", "Carol"])

    def test_filter_numeric_comparison(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?name ?age WHERE { ?s foaf:name ?name . ?s foaf:age ?age . "
            "FILTER(?age > 26) } ORDER BY ?age" % FOAF
        )
        self.assertEqual(
            [(r["name"]["value"], r["age"]["value"]) for r in rows],
            [("Alice", "30"), ("Carol", "41")],
        )
        self.assertEqual(
            rows[0]["age"]["datatype"],
            "http://www.w3.org/2001/XMLSchema#integer",
        )

    def test_filter_boolean_connectives_and_regex(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            'SELECT ?name WHERE { ?s foaf:name ?name . '
            'FILTER(regex(?name, "^[AC]", "i") && ?name != "Carol") }' % FOAF
        )
        self.assertEqual([r["name"]["value"] for r in rows], ["Alice"])

    def test_optional_present_and_absent(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?name ?age WHERE { ?s foaf:name ?name . "
            "OPTIONAL { ?s foaf:age ?age } } ORDER BY ?name" % FOAF
        )
        got = {r["name"]["value"]: r.get("age", {}).get("value") for r in rows}
        self.assertEqual(
            got, {"Alice": "30", "Bob": "25", "Carol": "41", "Dave": None}
        )

    def test_bound_filter_over_optional(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?name WHERE { ?s foaf:name ?name . "
            "OPTIONAL { ?s foaf:age ?age } FILTER(!bound(?age)) }" % FOAF
        )
        self.assertEqual([r["name"]["value"] for r in rows], ["Dave"])

    def test_distinct_limit_offset(self):
        rows = self.rows(
            "PREFIX foaf: <%s> SELECT DISTINCT ?t WHERE { ?s a ?t }" % FOAF
        )
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["t"]["value"], FOAF + "Person")
        rows = self.rows(
            "PREFIX foaf: <%s> SELECT ?name WHERE { ?s foaf:name ?name } "
            "ORDER BY ?name LIMIT 2 OFFSET 1" % FOAF
        )
        self.assertEqual([r["name"]["value"] for r in rows], ["Bob", "Carol"])

    def test_order_by_desc_numeric(self):
        rows = self.rows(
            "PREFIX foaf: <%s> SELECT ?age WHERE { ?s foaf:age ?age } "
            "ORDER BY DESC(?age)" % FOAF
        )
        self.assertEqual([r["age"]["value"] for r in rows], ["41", "30", "25"])

    def test_language_tag_in_results(self):
        rows = self.rows(
            "PREFIX foaf: <%s> PREFIX ex: <%s> "
            "SELECT ?name WHERE { ex:bob foaf:name ?name }" % (FOAF, EX)
        )
        self.assertEqual(rows[0]["name"]["xml:lang"], "en")

    def test_select_star(self):
        rows = self.rows(
            "PREFIX ex: <%s> SELECT * WHERE { ex:group ?p ?list }" % EX
        )
        self.assertEqual(len(rows), 1)
        self.assertEqual(set(rows[0].keys()), {"p", "list"})

    def test_lang_builtin(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            'SELECT ?name WHERE { ?s foaf:name ?name . FILTER(lang(?name) = "EN") }'
            % FOAF
        )
        self.assertEqual([r["name"]["value"] for r in rows], ["Bob"])

    def test_unsupported_feature_errors_cleanly(self):
        with self.assertRaises(sqlite3.OperationalError) as cm:
            self.q("SELECT ?s WHERE { ?s ?p ?o MINUS { ?s a ?t } }")
        self.assertIn("MINUS", str(cm.exception))
        with self.assertRaises(sqlite3.OperationalError) as cm:
            self.q(
                "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s ?p ?o } "
                "GROUP BY ?s HAVING(?n > 1)"
            )
        self.assertIn("HAVING", str(cm.exception))

    # ------------------------------------------------------------- UNION --

    def test_union_basic(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?v WHERE { { ?s foaf:mbox ?v } UNION { ?s foaf:nick ?v } }"
            % FOAF
        )
        got = sorted(r["v"]["value"] for r in rows)
        self.assertEqual(got, ["cc", "mailto:alice@example.org"])

    def test_union_joined_with_shared_pattern(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?name ?v WHERE { ?s a foaf:Person . ?s foaf:name ?name . "
            "{ ?s foaf:age ?v } UNION { ?s foaf:nick ?v } } ORDER BY ?name"
            % FOAF
        )
        got = [(r["name"]["value"], r["v"]["value"]) for r in rows]
        self.assertEqual(
            got,
            [("Alice", "30"), ("Bob", "25"), ("Carol", "41"), ("Carol", "cc")],
        )

    def test_union_distinct_dedupes_across_branches(self):
        rows = self.rows(
            "PREFIX foaf: <%s> SELECT DISTINCT ?s WHERE "
            "{ { ?s a foaf:Person } UNION { ?s a foaf:Person } }" % FOAF
        )
        self.assertEqual(len(rows), 4)

    def test_union_var_bound_in_one_branch_only(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?v ?nick WHERE "
            "{ { ?s foaf:mbox ?v } UNION { ?s foaf:nick ?nick } }" % FOAF
        )
        self.assertEqual(len(rows), 2)
        # each row binds exactly one of the two variables
        for r in rows:
            self.assertEqual(len(r), 1)

    def test_union_three_branches(self):
        rows = self.rows(
            "PREFIX foaf: <%s> SELECT ?v WHERE {"
            " { ?s foaf:mbox ?v } UNION { ?s foaf:nick ?v } "
            " UNION { ?s foaf:label ?v } }" % FOAF
        )
        self.assertEqual(len(rows), 2)

    def test_nested_plain_group_is_a_join(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?name WHERE { { ?s foaf:name ?name . ?s foaf:age ?a } }"
            % FOAF
        )
        self.assertEqual(len(rows), 3)

    # -------------------------------------------------------- aggregates --

    def test_count_star(self):
        rows = self.rows("SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }")
        self.assertEqual(rows[0]["n"]["value"], str(SAMPLE_TRIPLES))
        self.assertEqual(
            rows[0]["n"]["datatype"], "http://www.w3.org/2001/XMLSchema#integer"
        )

    def test_count_group_by(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s foaf:knows ?o } "
            "GROUP BY ?s ORDER BY DESC(?n)" % FOAF
        )
        got = [(r["s"]["value"], r["n"]["value"]) for r in rows]
        self.assertEqual(
            got, [(EX + "alice", "2"), (EX + "bob", "1")]
        )

    def test_count_var_skips_unbound(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT (COUNT(?age) AS ?bound) (COUNT(*) AS ?all) WHERE "
            "{ ?s foaf:name ?name . OPTIONAL { ?s foaf:age ?age } }" % FOAF
        )
        self.assertEqual(rows[0]["bound"]["value"], "3")
        self.assertEqual(rows[0]["all"]["value"], "4")

    def test_count_distinct(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE { ?s a ?t }" % FOAF
        )
        self.assertEqual(rows[0]["n"]["value"], "1")

    def test_sum_min_max_avg(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT (SUM(?a) AS ?sum) (MIN(?a) AS ?min) (MAX(?a) AS ?max) "
            "(AVG(?a) AS ?avg) WHERE { ?s foaf:age ?a }" % FOAF
        )
        r = rows[0]
        self.assertEqual(float(r["sum"]["value"]), 96.0)
        self.assertEqual(float(r["min"]["value"]), 25.0)
        self.assertEqual(float(r["max"]["value"]), 41.0)
        self.assertEqual(float(r["avg"]["value"]), 32.0)

    def test_aggregate_plain_var_requires_group_by(self):
        with self.assertRaises(sqlite3.OperationalError) as cm:
            self.q("SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s ?p ?o }")
        self.assertIn("GROUP BY", str(cm.exception))

    def test_aggregate_over_union(self):
        rows = self.rows(
            "PREFIX foaf: <%s> "
            "SELECT (COUNT(?v) AS ?n) WHERE "
            "{ { ?s foaf:mbox ?v } UNION { ?s foaf:nick ?v } }" % FOAF
        )
        self.assertEqual(rows[0]["n"]["value"], "2")

    def test_group_by_via_table_function(self):
        rows = self.db.execute(
            "SELECT json_extract(binding,'$.s'), json_extract(binding,'$.n') "
            "FROM sparql(?) ORDER BY 2 DESC",
            (
                "PREFIX foaf: <%s> SELECT ?s (COUNT(?o) AS ?n) "
                "WHERE { ?s foaf:knows ?o } GROUP BY ?s" % FOAF,
            ),
        ).fetchall()
        self.assertEqual(
            rows, [(EX + "alice", "2"), (EX + "bob", "1")]
        )

    # --------------------------------------------------- table function ----

    def test_sparql_table_function(self):
        rows = self.db.execute(
            "SELECT json_extract(binding, '$.name') FROM sparql(?) ",
            (
                "PREFIX foaf: <%s> SELECT ?name WHERE { ?s foaf:name ?name } "
                "ORDER BY ?name" % FOAF,
            ),
        ).fetchall()
        self.assertEqual([r[0] for r in rows], ["Alice", "Bob", "Carol", "Dave"])

    def test_sparql_table_function_join_with_sql(self):
        (n,) = self.db.execute(
            "SELECT count(*) FROM sparql('SELECT ?s WHERE { ?s ?p ?o }')"
        ).fetchone()
        self.assertEqual(n, SAMPLE_TRIPLES)

    # ------------------------------------------------------ Turtle output --

    def test_construct_returns_turtle_by_default(self):
        out = self.q(
            "PREFIX foaf: <%s> PREFIX ex: <%s> "
            "CONSTRUCT { ?s ex:hasName ?name } WHERE { ?s foaf:name ?name }"
            % (FOAF, EX)
        )
        self.assertIn("@prefix ex: <%s> ." % EX, out)
        self.assertIn('ex:hasName "Alice"', out)
        # must be loadable Turtle (round-trip)
        db2 = open_store()
        (n,) = db2.execute("SELECT rdf_load_turtle(?)", (out,)).fetchone()
        self.assertEqual(n, 4)
        db2.close()

    def test_construct_ntriples_format(self):
        out = self.q(
            "PREFIX foaf: <%s> PREFIX ex: <%s> "
            "CONSTRUCT { ?s ex:hasName ?name } WHERE { ?s foaf:name ?name }"
            % (FOAF, EX),
            "ntriples",
        )
        self.assertIn(
            "<%salice> <%shasName> \"Alice\" ." % (EX, EX), out
        )

    def test_construct_json_rejected_with_hint(self):
        with self.assertRaises(sqlite3.OperationalError):
            self.q("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }", "json")

    def test_select_turtle_rejected_with_hint(self):
        with self.assertRaises(sqlite3.OperationalError) as cm:
            self.q("SELECT ?s WHERE { ?s ?p ?o }", "turtle")
        self.assertIn("CONSTRUCT", str(cm.exception))

    def test_dump_turtle_roundtrip(self):
        dump = self.db.execute("SELECT rdf_dump_turtle()").fetchone()[0]
        db2 = open_store()
        (n,) = db2.execute("SELECT rdf_load_turtle(?)", (dump,)).fetchone()
        self.assertEqual(n, SAMPLE_TRIPLES)
        # same answers on the round-tripped store
        q = (
            "PREFIX foaf: <%s> SELECT ?name WHERE { ?s foaf:name ?name } "
            "ORDER BY ?name" % FOAF
        )
        a = json.loads(self.q(q))
        b = json.loads(db2.execute("SELECT rdf_query(?)", (q,)).fetchone()[0])
        self.assertEqual(a, b)
        db2.close()

    def test_datatype_and_lang_survive_roundtrip(self):
        dump = self.db.execute("SELECT rdf_dump_turtle()").fetchone()[0]
        db2 = open_store()
        db2.execute("SELECT rdf_load_turtle(?)", (dump,))
        rows = json.loads(
            db2.execute(
                "SELECT rdf_query(?)",
                (
                    "PREFIX foaf: <%s> PREFIX ex: <%s> "
                    "SELECT ?n WHERE { ex:bob foaf:name ?n }" % (FOAF, EX),
                ),
            ).fetchone()[0]
        )["results"]["bindings"]
        self.assertEqual(rows[0]["n"]["xml:lang"], "en")
        db2.close()

    # ----------------------------------------------------------- misc ------

    def test_persistence_on_disk(self):
        path = os.path.join(HERE, "_t.db")
        try:
            db = sqlite3.connect(path)
            db.enable_load_extension(True)
            db.load_extension(EXT)
            db.execute("SELECT rdf_load_turtle('<a:s> <a:p> <a:o> .')")
            db.commit()
            db.close()
            db = sqlite3.connect(path)
            db.enable_load_extension(True)
            db.load_extension(EXT)
            (n,) = db.execute(
                "SELECT count(*) FROM sparql('SELECT ?s WHERE { ?s ?p ?o }')"
            ).fetchone()
            self.assertEqual(n, 1)
            db.close()
        finally:
            if os.path.exists(path):
                os.remove(path)

    def test_rdf_regexp_function(self):
        (r,) = self.db.execute("SELECT rdf_regexp('^ab+c$','abbbc')").fetchone()
        self.assertEqual(r, 1)
        (r,) = self.db.execute("SELECT rdf_regexp('^ab+c$','ABC')").fetchone()
        self.assertEqual(r, 0)  # case-sensitive without the 'i' flag
        (r,) = self.db.execute("SELECT rdf_regexp('^ab+c$','ABC','i')").fetchone()
        self.assertEqual(r, 1)


def main():
    probe = sqlite3.connect(":memory:")
    if not hasattr(probe, "enable_load_extension"):
        print("SKIP: this Python build cannot load SQLite extensions")
        return 0
    try:
        probe.enable_load_extension(True)
    except sqlite3.OperationalError:
        print("SKIP: extension loading disabled in this Python build")
        return 0
    finally:
        probe.close()
    if not (os.path.exists(EXT + ".so") or os.path.exists(EXT + ".dylib")):
        print("Extension not built; run `make` in rdfsparql/ first")
        return 1
    unittest.main(argv=[sys.argv[0], "-v"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
