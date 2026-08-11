//! Conformance suite ported from the C engine's Python tests
//! (tests/fixtures/rdfsparql/test_rdfsparql.py). Every behavioral assertion
//! there has a counterpart here, plus v3 additions (ASK, SPARQL Update).

use rusqlite::Connection;
use serde_json::Value;

// 22 triples: alice 6, bob 4, carol 4, dave 2, collection 5, bnode label 1
const SAMPLE_TRIPLES: i64 = 22;

const FOAF: &str = "http://xmlns.com/foaf/0.1/";
const EX: &str = "http://example.org/";

fn sample_path() -> String {
    format!(
        "{}/../../tests/fixtures/rdfsparql/sample.ttl",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn open_store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    resonator_sparql::register(&conn).unwrap();
    conn
}

fn setup() -> (Connection, i64) {
    let db = open_store();
    let loaded: i64 = db
        .query_row("SELECT rdf_load_turtle_file(?1)", [sample_path()], |r| {
            r.get(0)
        })
        .unwrap();
    (db, loaded)
}

fn q(db: &Connection, sparql: &str) -> Result<String, rusqlite::Error> {
    db.query_row("SELECT rdf_query(?1)", [sparql], |r| r.get(0))
}

fn qf(db: &Connection, sparql: &str, fmt: &str) -> Result<String, rusqlite::Error> {
    db.query_row("SELECT rdf_query(?1,?2)", [sparql, fmt], |r| r.get(0))
}

fn rows(db: &Connection, sparql: &str) -> Vec<Value> {
    let doc: Value = serde_json::from_str(&q(db, sparql).unwrap()).unwrap();
    doc["results"]["bindings"].as_array().unwrap().clone()
}

fn val<'a>(row: &'a Value, var: &str) -> &'a str {
    row[var]["value"].as_str().unwrap()
}

// ------------------------------------------------------------- loading --

#[test]
fn load_counts_triples() {
    let (_db, loaded) = setup();
    assert_eq!(loaded, SAMPLE_TRIPLES);
}

#[test]
fn reload_is_idempotent_for_ground_triples() {
    // Blank nodes get fresh labels per load, so only the 6 bnode-involving
    // triples (5 collection + 1 label) are re-inserted.
    let (db, _) = setup();
    let again: i64 = db
        .query_row("SELECT rdf_load_turtle_file(?1)", [sample_path()], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(again, 6);
}

#[test]
fn load_turtle_text_with_base() {
    let db = open_store();
    let n: i64 = db
        .query_row(
            "SELECT rdf_load_turtle(?1, ?2)",
            ["<s> <p> <o> .", "http://base.example/dir/"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    let rows = rows(&db, "SELECT ?s WHERE { ?s ?p ?o }");
    assert_eq!(val(&rows[0], "s"), "http://base.example/dir/s");
}

#[test]
fn parse_error_reports_position() {
    let (db, _) = setup();
    let err = db
        .query_row("SELECT rdf_load_turtle('<a> <b> .')", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_err();
    assert!(err.to_string().contains("line"), "got: {err}");
}

// ------------------------------------------------------------- SELECT --

#[test]
fn basic_select() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name }} ORDER BY ?name"
        ),
    );
    let names: Vec<&str> = rows.iter().map(|r| val(r, "name")).collect();
    assert_eq!(names, ["Alice", "Bob", "Carol", "Dave"]);
}

#[test]
fn join_on_shared_variable() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> PREFIX ex: <{EX}> \
             SELECT ?name WHERE {{ ex:alice foaf:knows ?p . ?p foaf:name ?name }} ORDER BY ?name"
        ),
    );
    let names: Vec<&str> = rows.iter().map(|r| val(r, "name")).collect();
    assert_eq!(names, ["Bob", "Carol"]);
}

#[test]
fn filter_numeric_comparison() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> \
             SELECT ?name ?age WHERE {{ ?s foaf:name ?name . ?s foaf:age ?age . \
             FILTER(?age > 26) }} ORDER BY ?age"
        ),
    );
    let got: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (val(r, "name"), val(r, "age")))
        .collect();
    assert_eq!(got, [("Alice", "30"), ("Carol", "41")]);
    assert_eq!(
        rows[0]["age"]["datatype"].as_str().unwrap(),
        "http://www.w3.org/2001/XMLSchema#integer"
    );
}

#[test]
fn filter_boolean_connectives_and_regex() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name . \
             FILTER(regex(?name, \"^[AC]\", \"i\") && ?name != \"Carol\") }}"
        ),
    );
    let names: Vec<&str> = rows.iter().map(|r| val(r, "name")).collect();
    assert_eq!(names, ["Alice"]);
}

#[test]
fn optional_present_and_absent() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name ?age WHERE {{ ?s foaf:name ?name . \
             OPTIONAL {{ ?s foaf:age ?age }} }} ORDER BY ?name"
        ),
    );
    let got: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|r| (val(r, "name"), r["age"]["value"].as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("Alice", Some("30")),
            ("Bob", Some("25")),
            ("Carol", Some("41")),
            ("Dave", None)
        ]
    );
}

#[test]
fn bound_filter_over_optional() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name . \
             OPTIONAL {{ ?s foaf:age ?age }} FILTER(!bound(?age)) }}"
        ),
    );
    let names: Vec<&str> = rows.iter().map(|r| val(r, "name")).collect();
    assert_eq!(names, ["Dave"]);
}

#[test]
fn distinct_limit_offset() {
    let (db, _) = setup();
    let r1 = rows(
        &db,
        &format!("PREFIX foaf: <{FOAF}> SELECT DISTINCT ?t WHERE {{ ?s a ?t }}"),
    );
    assert_eq!(r1.len(), 1);
    assert_eq!(val(&r1[0], "t"), format!("{FOAF}Person"));
    let r2 = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name }} \
             ORDER BY ?name LIMIT 2 OFFSET 1"
        ),
    );
    let names: Vec<&str> = r2.iter().map(|r| val(r, "name")).collect();
    assert_eq!(names, ["Bob", "Carol"]);
}

#[test]
fn order_by_desc_numeric() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?age WHERE {{ ?s foaf:age ?age }} ORDER BY DESC(?age)"
        ),
    );
    let ages: Vec<&str> = rows.iter().map(|r| val(r, "age")).collect();
    assert_eq!(ages, ["41", "30", "25"]);
}

#[test]
fn language_tag_in_results() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> PREFIX ex: <{EX}> SELECT ?name WHERE {{ ex:bob foaf:name ?name }}"
        ),
    );
    assert_eq!(rows[0]["name"]["xml:lang"].as_str().unwrap(), "en");
}

#[test]
fn select_star() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!("PREFIX ex: <{EX}> SELECT * WHERE {{ ex:group ?p ?list }}"),
    );
    assert_eq!(rows.len(), 1);
    let keys: std::collections::BTreeSet<&str> = rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(keys, ["list", "p"].into_iter().collect());
}

#[test]
fn lang_builtin() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name . \
             FILTER(lang(?name) = \"EN\") }}"
        ),
    );
    let names: Vec<&str> = rows.iter().map(|r| val(r, "name")).collect();
    assert_eq!(names, ["Bob"]);
}

#[test]
fn unsupported_feature_errors_cleanly() {
    let (db, _) = setup();
    let err = q(&db, "SELECT ?s WHERE { ?s ?p ?o MINUS { ?s a ?t } }").unwrap_err();
    assert!(err.to_string().contains("MINUS"), "got: {err}");
    let err = q(
        &db,
        "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?s HAVING(?n > 1)",
    )
    .unwrap_err();
    assert!(err.to_string().contains("HAVING"), "got: {err}");
}

// ------------------------------------------------------------- UNION --

#[test]
fn union_basic() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?v WHERE {{ {{ ?s foaf:mbox ?v }} UNION {{ ?s foaf:nick ?v }} }}"
        ),
    );
    let mut got: Vec<&str> = rows.iter().map(|r| val(r, "v")).collect();
    got.sort();
    assert_eq!(got, ["cc", "mailto:alice@example.org"]);
}

#[test]
fn union_joined_with_shared_pattern() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> \
             SELECT ?name ?v WHERE {{ ?s a foaf:Person . ?s foaf:name ?name . \
             {{ ?s foaf:age ?v }} UNION {{ ?s foaf:nick ?v }} }} ORDER BY ?name"
        ),
    );
    let got: Vec<(&str, &str)> = rows.iter().map(|r| (val(r, "name"), val(r, "v"))).collect();
    assert_eq!(
        got,
        [
            ("Alice", "30"),
            ("Bob", "25"),
            ("Carol", "41"),
            ("Carol", "cc")
        ]
    );
}

#[test]
fn union_distinct_dedupes_across_branches() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT DISTINCT ?s WHERE \
             {{ {{ ?s a foaf:Person }} UNION {{ ?s a foaf:Person }} }}"
        ),
    );
    assert_eq!(rows.len(), 4);
}

#[test]
fn union_var_bound_in_one_branch_only() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?v ?nick WHERE \
             {{ {{ ?s foaf:mbox ?v }} UNION {{ ?s foaf:nick ?nick }} }}"
        ),
    );
    assert_eq!(rows.len(), 2);
    // each row binds exactly one of the two variables
    for r in &rows {
        assert_eq!(r.as_object().unwrap().len(), 1);
    }
}

#[test]
fn union_three_branches() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?v WHERE {{ \
             {{ ?s foaf:mbox ?v }} UNION {{ ?s foaf:nick ?v }} \
             UNION {{ ?s foaf:label ?v }} }}"
        ),
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn nested_plain_group_is_a_join() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ {{ ?s foaf:name ?name . ?s foaf:age ?a }} }}"
        ),
    );
    assert_eq!(rows.len(), 3);
}

// -------------------------------------------------------- aggregates --

#[test]
fn count_star() {
    let (db, _) = setup();
    let rows = rows(&db, "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }");
    assert_eq!(val(&rows[0], "n"), SAMPLE_TRIPLES.to_string());
    assert_eq!(
        rows[0]["n"]["datatype"].as_str().unwrap(),
        "http://www.w3.org/2001/XMLSchema#integer"
    );
}

#[test]
fn count_group_by() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT ?s (COUNT(?o) AS ?n) WHERE {{ ?s foaf:knows ?o }} \
             GROUP BY ?s ORDER BY DESC(?n)"
        ),
    );
    let got: Vec<(&str, &str)> = rows.iter().map(|r| (val(r, "s"), val(r, "n"))).collect();
    assert_eq!(
        got,
        [
            (format!("{EX}alice").as_str(), "2"),
            (format!("{EX}bob").as_str(), "1")
        ]
    );
}

#[test]
fn count_var_skips_unbound() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> \
             SELECT (COUNT(?age) AS ?bound) (COUNT(*) AS ?all) WHERE \
             {{ ?s foaf:name ?name . OPTIONAL {{ ?s foaf:age ?age }} }}"
        ),
    );
    assert_eq!(val(&rows[0], "bound"), "3");
    assert_eq!(val(&rows[0], "all"), "4");
}

#[test]
fn count_distinct() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!("PREFIX foaf: <{FOAF}> SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE {{ ?s a ?t }}"),
    );
    assert_eq!(val(&rows[0], "n"), "1");
}

#[test]
fn sum_min_max_avg() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> \
             SELECT (SUM(?a) AS ?sum) (MIN(?a) AS ?min) (MAX(?a) AS ?max) \
             (AVG(?a) AS ?avg) WHERE {{ ?s foaf:age ?a }}"
        ),
    );
    let r = &rows[0];
    assert_eq!(val(r, "sum").parse::<f64>().unwrap(), 96.0);
    assert_eq!(val(r, "min").parse::<f64>().unwrap(), 25.0);
    assert_eq!(val(r, "max").parse::<f64>().unwrap(), 41.0);
    assert_eq!(val(r, "avg").parse::<f64>().unwrap(), 32.0);
}

#[test]
fn aggregate_plain_var_requires_group_by() {
    let (db, _) = setup();
    let err = q(&db, "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s ?p ?o }").unwrap_err();
    assert!(err.to_string().contains("GROUP BY"), "got: {err}");
}

#[test]
fn aggregate_over_union() {
    let (db, _) = setup();
    let rows = rows(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> SELECT (COUNT(?v) AS ?n) WHERE \
             {{ {{ ?s foaf:mbox ?v }} UNION {{ ?s foaf:nick ?v }} }}"
        ),
    );
    assert_eq!(val(&rows[0], "n"), "2");
}

#[test]
fn group_by_via_table_function() {
    let (db, _) = setup();
    let query = format!(
        "PREFIX foaf: <{FOAF}> SELECT ?s (COUNT(?o) AS ?n) WHERE {{ ?s foaf:knows ?o }} GROUP BY ?s"
    );
    let mut st = db
        .prepare(
            "SELECT json_extract(binding,'$.s'), json_extract(binding,'$.n') \
             FROM sparql(?1) ORDER BY 2 DESC",
        )
        .unwrap();
    let got: Vec<(String, String)> = st
        .query_map([query], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        got,
        [
            (format!("{EX}alice"), "2".to_string()),
            (format!("{EX}bob"), "1".to_string())
        ]
    );
}

// --------------------------------------------------- table function ----

#[test]
fn sparql_table_function() {
    let (db, _) = setup();
    let query =
        format!("PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name }} ORDER BY ?name");
    let mut st = db
        .prepare("SELECT json_extract(binding, '$.name') FROM sparql(?1)")
        .unwrap();
    let names: Vec<String> = st
        .query_map([query], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(names, ["Alice", "Bob", "Carol", "Dave"]);
}

#[test]
fn sparql_table_function_join_with_sql() {
    let (db, _) = setup();
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM sparql('SELECT ?s WHERE { ?s ?p ?o }')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, SAMPLE_TRIPLES);
}

// ------------------------------------------------------ Turtle output --

#[test]
fn construct_returns_turtle_by_default() {
    let (db, _) = setup();
    let out = q(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> PREFIX ex: <{EX}> \
             CONSTRUCT {{ ?s ex:hasName ?name }} WHERE {{ ?s foaf:name ?name }}"
        ),
    )
    .unwrap();
    assert!(out.contains(&format!("@prefix ex: <{EX}> .")), "got: {out}");
    assert!(out.contains("ex:hasName \"Alice\""), "got: {out}");
    // must be loadable Turtle (round-trip)
    let db2 = open_store();
    let n: i64 = db2
        .query_row("SELECT rdf_load_turtle(?1)", [out], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4);
}

#[test]
fn construct_ntriples_format() {
    let (db, _) = setup();
    let out = qf(
        &db,
        &format!(
            "PREFIX foaf: <{FOAF}> PREFIX ex: <{EX}> \
             CONSTRUCT {{ ?s ex:hasName ?name }} WHERE {{ ?s foaf:name ?name }}"
        ),
        "ntriples",
    )
    .unwrap();
    assert!(
        out.contains(&format!("<{EX}alice> <{EX}hasName> \"Alice\" .")),
        "got: {out}"
    );
}

#[test]
fn construct_json_rejected_with_hint() {
    let (db, _) = setup();
    assert!(qf(&db, "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }", "json").is_err());
}

#[test]
fn select_turtle_rejected_with_hint() {
    let (db, _) = setup();
    let err = qf(&db, "SELECT ?s WHERE { ?s ?p ?o }", "turtle").unwrap_err();
    assert!(err.to_string().contains("CONSTRUCT"), "got: {err}");
}

#[test]
fn dump_turtle_roundtrip() {
    let (db, _) = setup();
    let dump: String = db
        .query_row("SELECT rdf_dump_turtle()", [], |r| r.get(0))
        .unwrap();
    let db2 = open_store();
    let n: i64 = db2
        .query_row("SELECT rdf_load_turtle(?1)", [dump], |r| r.get(0))
        .unwrap();
    assert_eq!(n, SAMPLE_TRIPLES);
    // same answers on the round-tripped store
    let query =
        format!("PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name }} ORDER BY ?name");
    let a: Value = serde_json::from_str(&q(&db, &query).unwrap()).unwrap();
    let b: Value = serde_json::from_str(&q(&db2, &query).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn datatype_and_lang_survive_roundtrip() {
    let (db, _) = setup();
    let dump: String = db
        .query_row("SELECT rdf_dump_turtle()", [], |r| r.get(0))
        .unwrap();
    let db2 = open_store();
    let _: i64 = db2
        .query_row("SELECT rdf_load_turtle(?1)", [dump], |r| r.get(0))
        .unwrap();
    let rows = {
        let doc: Value = serde_json::from_str(
            &db2.query_row(
                "SELECT rdf_query(?1)",
                [format!(
                    "PREFIX foaf: <{FOAF}> PREFIX ex: <{EX}> SELECT ?n WHERE {{ ex:bob foaf:name ?n }}"
                )],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        doc["results"]["bindings"].as_array().unwrap().clone()
    };
    assert_eq!(rows[0]["n"]["xml:lang"].as_str().unwrap(), "en");
}

// ----------------------------------------------------------- misc ------

#[test]
fn persistence_on_disk() {
    let path = std::env::temp_dir().join(format!("resonator_sparql_t_{}.db", std::process::id()));
    let path_str = path.to_str().unwrap().to_string();
    let _ = std::fs::remove_file(&path);
    {
        let db = Connection::open(&path_str).unwrap();
        resonator_sparql::register(&db).unwrap();
        let _: i64 = db
            .query_row("SELECT rdf_load_turtle('<a:s> <a:p> <a:o> .')", [], |r| {
                r.get(0)
            })
            .unwrap();
    }
    {
        let db = Connection::open(&path_str).unwrap();
        resonator_sparql::register(&db).unwrap();
        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM sparql('SELECT ?s WHERE { ?s ?p ?o }')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rdf_regexp_function() {
    let (db, _) = setup();
    let r: i64 = db
        .query_row("SELECT rdf_regexp('^ab+c$','abbbc')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(r, 1);
    // case-sensitive without the 'i' flag
    let r: i64 = db
        .query_row("SELECT rdf_regexp('^ab+c$','ABC')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(r, 0);
    let r: i64 = db
        .query_row("SELECT rdf_regexp('^ab+c$','ABC','i')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(r, 1);
}

// --------------------------------------------------- v3 additions ------

#[test]
fn ask_queries() {
    let (db, _) = setup();
    let out = q(
        &db,
        &format!("PREFIX foaf: <{FOAF}> ASK {{ ?s foaf:name \"Alice\" }}"),
    )
    .unwrap();
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["boolean"], Value::Bool(true));
    let out = q(
        &db,
        &format!("PREFIX foaf: <{FOAF}> ASK {{ ?s foaf:name \"Zed\" }}"),
    )
    .unwrap();
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["boolean"], Value::Bool(false));
    // json format override is accepted, others are not
    assert!(qf(&db, "ASK { ?s ?p ?o }", "json").is_ok());
    assert!(qf(&db, "ASK { ?s ?p ?o }", "turtle").is_err());
}

#[test]
fn update_insert_delete_data() {
    let db = open_store();
    let n: i64 = db
        .query_row(
            "SELECT rdf_update('INSERT DATA { <a:s> <a:p> <a:o> . <a:s> <a:p> <a:o2> }')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2);
    // idempotent re-insert
    let n: i64 = db
        .query_row(
            "SELECT rdf_update('INSERT DATA { <a:s> <a:p> <a:o> }')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
    let n: i64 = db
        .query_row(
            "SELECT rdf_update('DELETE DATA { <a:s> <a:p> <a:o> }')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM sparql('SELECT ?s WHERE { ?s ?p ?o }')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn update_delete_where() {
    let (db, _) = setup();
    let n: i64 = db
        .query_row(
            "SELECT rdf_update(?1)",
            [format!(
                "PREFIX foaf: <{FOAF}> DELETE WHERE {{ ?s foaf:knows ?o }}"
            )],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 3);
    let rows = rows(
        &db,
        &format!("PREFIX foaf: <{FOAF}> SELECT ?o WHERE {{ ?s foaf:knows ?o }}"),
    );
    assert!(rows.is_empty());
}

#[test]
fn store_typed_api() {
    use resonator_sparql::{QueryResults, Store};
    let store = Store::open_in_memory().unwrap();
    let n = store.load_turtle_file(&sample_path(), None).unwrap();
    assert_eq!(n, SAMPLE_TRIPLES);
    match store
        .query(&format!(
            "PREFIX foaf: <{FOAF}> SELECT ?name WHERE {{ ?s foaf:name ?name }} ORDER BY ?name"
        ))
        .unwrap()
    {
        QueryResults::Solutions { vars, rows } => {
            assert_eq!(vars, ["name"]);
            let names: Vec<&str> = rows
                .iter()
                .map(|r| r[0].as_ref().unwrap().lex.as_str())
                .collect();
            assert_eq!(names, ["Alice", "Bob", "Carol", "Dave"]);
        }
        other => panic!("expected solutions, got {other:?}"),
    }
    match store.query("ASK { ?s ?p ?o }").unwrap() {
        QueryResults::Boolean(b) => assert!(b),
        other => panic!("expected boolean, got {other:?}"),
    }
    match store
        .query(&format!(
            "PREFIX foaf: <{FOAF}> PREFIX ex: <{EX}> \
             CONSTRUCT {{ ?s ex:hasName ?n }} WHERE {{ ?s foaf:name ?n }}"
        ))
        .unwrap()
    {
        QueryResults::Graph(triples) => assert_eq!(triples.len(), 4),
        other => panic!("expected graph, got {other:?}"),
    }
    // dump round-trips through a second store
    let dump = store.dump_turtle().unwrap();
    let store2 = Store::open_in_memory().unwrap();
    assert_eq!(store2.load_turtle(&dump, None).unwrap(), SAMPLE_TRIPLES);
}

#[test]
fn rdf_init_function() {
    let db = Connection::open_in_memory().unwrap();
    resonator_sparql::register(&db).unwrap();
    let ok: String = db.query_row("SELECT rdf_init()", [], |r| r.get(0)).unwrap();
    assert_eq!(ok, "ok");
    // schema exists and is idempotent
    let ok: String = db.query_row("SELECT rdf_init()", [], |r| r.get(0)).unwrap();
    assert_eq!(ok, "ok");
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'rdf_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(n >= 2);
}
