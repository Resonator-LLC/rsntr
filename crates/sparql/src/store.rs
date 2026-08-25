//! Native engine: schema, Turtle loader, query/update execution over a
//! rusqlite Connection, and the typed Store API.

use rusqlite::Connection;
use rusqlite::types::ValueRef;

use crate::ast::{Form, Parsed, Query, Slot, TriplePattern, Update, UpdateKind};
use crate::compile::compile_query;
use crate::parser::parse;
use crate::serialize;
use crate::term::{K_IRI, K_LIT, Term};

/// Engine error: a plain message, also used as the SQLite error text when
/// surfaced through the SQL functions.
#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<crate::parser::ParseError> for Error {
    fn from(e: crate::parser::ParseError) -> Self {
        Error(e.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS rdf_terms(\
  id INTEGER PRIMARY KEY,\
  kind INTEGER NOT NULL,\
  lex TEXT NOT NULL,\
  dtype TEXT NOT NULL DEFAULT '',\
  lang TEXT NOT NULL DEFAULT '',\
  UNIQUE(kind,lex,dtype,lang)\
);\
CREATE TABLE IF NOT EXISTS rdf_triples(\
  g INTEGER NOT NULL DEFAULT 0,\
  s INTEGER NOT NULL, p INTEGER NOT NULL, o INTEGER NOT NULL,\
  PRIMARY KEY(g,s,p,o)\
) WITHOUT ROWID;\
CREATE INDEX IF NOT EXISTS rdf_triples_gpos ON rdf_triples(g,p,o,s);\
CREATE INDEX IF NOT EXISTS rdf_triples_gosp ON rdf_triples(g,o,s,p);";

/// Create the storage schema (idempotent).
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}

/// Look up a ground term's id; 0 if the term isn't in the dictionary.
pub fn lookup_term_id(conn: &Connection, t: &Term) -> i64 {
    conn.prepare_cached(
        "SELECT id FROM rdf_terms WHERE kind=?1 AND lex=?2 AND dtype=?3 AND lang=?4",
    )
    .and_then(|mut st| st.query_row((t.kind, &t.lex, &t.dtype, &t.lang), |r| r.get::<_, i64>(0)))
    .unwrap_or(0)
}

fn intern_term(conn: &Connection, t: &Term) -> Result<i64> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO rdf_terms(kind,lex,dtype,lang) VALUES(?1,?2,?3,?4)",
    )?
    .execute((t.kind, &t.lex, &t.dtype, &t.lang))?;
    let id = lookup_term_id(conn, t);
    if id == 0 {
        return Err(Error("term lookup failed after insert".to_string()));
    }
    Ok(id)
}

fn insert_triple(conn: &Connection, s: i64, p: i64, o: i64) -> Result<i64> {
    let n = conn
        .prepare_cached("INSERT OR IGNORE INTO rdf_triples(g,s,p,o) VALUES(0,?1,?2,?3)")?
        .execute((s, p, o))?;
    Ok(n as i64)
}

/// Per-load blank node freshener: labels are rewritten to "g<stamp>n<serial>"
/// so re-loading the same document re-inserts bnode-scoped triples under new
/// labels. This is documented behavior, not an accident.
struct Freshener {
    stamp: i64,
    serial: i64,
    map: std::collections::HashMap<String, String>,
}

impl Freshener {
    fn new(conn: &Connection) -> Result<Self> {
        let stamp: i64 = conn.query_row("SELECT COALESCE(MAX(id),0) FROM rdf_terms", [], |r| {
            r.get(0)
        })?;
        Ok(Freshener {
            stamp,
            serial: 0,
            map: std::collections::HashMap::new(),
        })
    }

    fn fresh(&mut self) -> String {
        let label = format!("g{}n{}", self.stamp, self.serial);
        self.serial += 1;
        label
    }

    fn label(&mut self, source: &str) -> String {
        if let Some(l) = self.map.get(source) {
            return l.clone();
        }
        let l = self.fresh();
        self.map.insert(source.to_string(), l.clone());
        l
    }
}

fn conv_literal(l: oxrdf::Literal) -> Term {
    let (lex, dtype, lang) = l.destruct();
    if let Some(lang) = lang {
        Term::lit_lang(lex, lang)
    } else if let Some(dt) = dtype {
        Term::lit_dt(lex, dt.into_string())
    } else {
        Term::lit(lex)
    }
}

/// Load Turtle text into the default graph; returns the number of triples
/// actually inserted (INSERT OR IGNORE semantics).
pub fn load_turtle(conn: &Connection, text: &str, base: Option<&str>) -> Result<i64> {
    ensure_schema(conn)?;
    let mut parser = oxttl::TurtleParser::new();
    if let Some(base) = base {
        parser = parser
            .with_base_iri(base)
            .map_err(|e| Error(format!("invalid base IRI: {e}")))?;
    }
    let mut fresh = Freshener::new(conn)?;
    let mut inserted = 0i64;
    for item in parser.for_slice(text.as_bytes()) {
        let triple = item.map_err(|e| {
            let loc = e.location();
            Error(format!(
                "{} (line {}, col {})",
                e.message(),
                loc.start.line + 1,
                loc.start.column + 1
            ))
        })?;
        let s = match triple.subject {
            oxrdf::NamedOrBlankNode::NamedNode(n) => Term::iri(n.into_string()),
            oxrdf::NamedOrBlankNode::BlankNode(b) => Term::bnode(fresh.label(b.as_str())),
        };
        let p = Term::iri(triple.predicate.into_string());
        let o = match triple.object {
            oxrdf::Term::NamedNode(n) => Term::iri(n.into_string()),
            oxrdf::Term::BlankNode(b) => Term::bnode(fresh.label(b.as_str())),
            oxrdf::Term::Literal(l) => conv_literal(l),
        };
        let si = intern_term(conn, &s)?;
        let pi = intern_term(conn, &p)?;
        let oi = intern_term(conn, &o)?;
        inserted += insert_triple(conn, si, pi, oi)?;
    }
    Ok(inserted)
}

/// Load a Turtle file from disk.
pub fn load_turtle_file(conn: &Connection, path: &str, base: Option<&str>) -> Result<i64> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| Error(format!("rdf_load_turtle_file: cannot open '{path}'")))?;
    load_turtle(conn, &text, base)
}

fn value_to_text(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(r) => {
            // match SQLite's text conversion closely enough: keep a ".0"
            // on integral floats
            if r.fract() == 0.0 && r.abs() < 1e15 {
                format!("{r:.1}")
            } else {
                r.to_string()
            }
        }
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

/// One solution row: one Option<Term> per projected variable.
pub type SolutionRow = Vec<Option<Term>>;

/// Run the layered SELECT and read back one Option<Term> per projected
/// variable per row.
pub fn exec_select(conn: &Connection, q: &Query) -> Result<(Vec<String>, Vec<SolutionRow>)> {
    let mut lookup = |t: &Term| lookup_term_id(conn, t);
    let (sql, vars) = compile_query(q, &mut lookup).map_err(Error)?;
    let mut st = conn.prepare(&sql)?;
    let ncols = vars.len();
    let mut rows = Vec::new();
    let mut raw = st.query([])?;
    while let Some(row) = raw.next()? {
        let mut out: Vec<Option<Term>> = Vec::with_capacity(ncols);
        for j in 0..ncols {
            let base = j * 5;
            if row.get_ref(base)? == ValueRef::Null {
                out.push(None);
                continue;
            }
            let kind: i64 = row.get(base + 1)?;
            let lex = value_to_text(row.get_ref(base + 2)?);
            let dtype = value_to_text(row.get_ref(base + 3)?);
            let lang = value_to_text(row.get_ref(base + 4)?);
            out.push(Some(Term {
                kind,
                lex,
                dtype,
                lang,
            }));
        }
        rows.push(out);
    }
    Ok((vars, rows))
}

/// Run an ASK query.
pub fn exec_ask(conn: &Connection, q: &Query) -> Result<bool> {
    let mut lookup = |t: &Term| lookup_term_id(conn, t);
    let (sql, _) = compile_query(q, &mut lookup).map_err(Error)?;
    let mut st = conn.prepare(&sql)?;
    let mut raw = st.query([])?;
    Ok(raw.next()?.is_some())
}

/// Run a CONSTRUCT query: instantiate the template per solution, drop
/// invalid triples (literal subject, non-IRI predicate, unbound variable),
/// dedupe.
pub fn exec_construct(conn: &Connection, q: &Query) -> Result<Vec<[Term; 3]>> {
    let (vars, rows) = exec_select(conn, q)?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in &rows {
        for tp in &q.template {
            let mut slots: Vec<Term> = Vec::with_capacity(3);
            let mut ok = true;
            for slot in tp.slots() {
                match slot {
                    Slot::Var(v) => {
                        let idx = vars.iter().position(|x| x == v);
                        match idx.and_then(|i| row[i].clone()) {
                            Some(t) => slots.push(t),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    Slot::Ground(t) => slots.push(t.clone()),
                }
            }
            if !ok {
                continue;
            }
            let [s, p, o]: [Term; 3] = slots.try_into().unwrap();
            if s.kind == K_LIT || p.kind != K_IRI {
                continue;
            }
            let key = [s, p, o];
            if seen.insert(key.clone()) {
                out.push(key);
            }
        }
    }
    Ok(out)
}

/// rdf_query semantics: SELECT/ASK -> SPARQL 1.1 JSON, CONSTRUCT -> Turtle
/// (default) or N-Triples.
pub fn run_query(conn: &Connection, sparql: &str, format: Option<&str>) -> Result<String> {
    ensure_schema(conn)?;
    let parsed = parse(sparql)?;
    let q = match parsed {
        Parsed::Query(q) => q,
        Parsed::Update(_) => {
            return Err(Error(
                "rdf_query runs queries only; use rdf_update() for SPARQL Update".to_string(),
            ));
        }
    };
    match q.form {
        Form::Select => {
            if let Some(f) = format
                && !f.eq_ignore_ascii_case("json")
            {
                return Err(Error(
                    "rdf_query: SELECT results are variable bindings, not a graph; \
                     only 'json' output is available (use CONSTRUCT for Turtle)"
                        .to_string(),
                ));
            }
            let (vars, rows) = exec_select(conn, &q)?;
            Ok(serialize::select_results_json(&vars, &rows))
        }
        Form::Ask => {
            if let Some(f) = format
                && !f.eq_ignore_ascii_case("json")
            {
                return Err(Error(
                    "rdf_query: ASK results are boolean; only 'json' output is available"
                        .to_string(),
                ));
            }
            Ok(serialize::ask_json(exec_ask(conn, &q)?))
        }
        Form::Construct => {
            let ntriples = match format {
                None => false,
                Some(f) if f.eq_ignore_ascii_case("turtle") || f.eq_ignore_ascii_case("ttl") => {
                    false
                }
                Some(f) if f.eq_ignore_ascii_case("ntriples") || f.eq_ignore_ascii_case("nt") => {
                    true
                }
                Some(_) => {
                    return Err(Error(
                        "rdf_query: CONSTRUCT formats are 'turtle' (default) or 'ntriples'"
                            .to_string(),
                    ));
                }
            };
            let mut triples = exec_construct(conn, &q)?;
            if ntriples {
                Ok(serialize::serialize_ntriples(&mut triples))
            } else {
                Ok(serialize::serialize_turtle(&mut triples, &q.prefixes))
            }
        }
    }
}

/// Serialize the entire store (all graphs) as Turtle.
pub fn dump_turtle(conn: &Connection) -> Result<String> {
    ensure_schema(conn)?;
    let mut st = conn.prepare(
        "SELECT st.kind,st.lex,st.dtype,st.lang, \
                pt.kind,pt.lex,pt.dtype,pt.lang, \
                ot.kind,ot.lex,ot.dtype,ot.lang \
         FROM rdf_triples tr \
         JOIN rdf_terms st ON st.id=tr.s \
         JOIN rdf_terms pt ON pt.id=tr.p \
         JOIN rdf_terms ot ON ot.id=tr.o",
    )?;
    let mut triples = Vec::new();
    let mut raw = st.query([])?;
    while let Some(row) = raw.next()? {
        let mut t: Vec<Term> = Vec::with_capacity(3);
        for k in 0..3 {
            t.push(Term {
                kind: row.get(k * 4)?,
                lex: value_to_text(row.get_ref(k * 4 + 1)?),
                dtype: value_to_text(row.get_ref(k * 4 + 2)?),
                lang: value_to_text(row.get_ref(k * 4 + 3)?),
            });
        }
        let arr: [Term; 3] = t.try_into().unwrap();
        triples.push(arr);
    }
    Ok(serialize::serialize_turtle(&mut triples, &[]))
}

fn ground(tp: &TriplePattern, fresh: &mut Freshener) -> Result<[Term; 3]> {
    let mut out: Vec<Term> = Vec::with_capacity(3);
    for slot in tp.slots() {
        match slot {
            Slot::Var(v) => {
                if let Some(label) = v.strip_prefix("~bn~") {
                    out.push(Term::bnode(fresh.label(label)));
                } else {
                    return Err(Error(
                        "variables are not allowed in ground data".to_string(),
                    ));
                }
            }
            Slot::Ground(t) => out.push(t.clone()),
        }
    }
    Ok(out.try_into().unwrap())
}

/// Execute a SPARQL Update (INSERT DATA / DELETE DATA / DELETE WHERE);
/// returns the number of triples inserted or deleted.
pub fn run_update(conn: &Connection, u: &Update) -> Result<i64> {
    ensure_schema(conn)?;
    let mut affected = 0i64;
    match u.kind {
        UpdateKind::InsertData => {
            let mut fresh = Freshener::new(conn)?;
            for tp in &u.triples {
                let [s, p, o] = ground(tp, &mut fresh)?;
                let si = intern_term(conn, &s)?;
                let pi = intern_term(conn, &p)?;
                let oi = intern_term(conn, &o)?;
                affected += insert_triple(conn, si, pi, oi)?;
            }
        }
        UpdateKind::DeleteData => {
            let mut fresh = Freshener::new(conn)?;
            for tp in &u.triples {
                let [s, p, o] = ground(tp, &mut fresh)?;
                let ids = [
                    lookup_term_id(conn, &s),
                    lookup_term_id(conn, &p),
                    lookup_term_id(conn, &o),
                ];
                if ids.contains(&0) {
                    continue;
                }
                affected += conn
                    .prepare_cached("DELETE FROM rdf_triples WHERE g=0 AND s=?1 AND p=?2 AND o=?3")?
                    .execute((ids[0], ids[1], ids[2]))? as i64;
            }
        }
        UpdateKind::DeleteWhere => {
            // compile the pattern as a SELECT over its variables, then delete
            // each matched instantiation
            let mut vars: Vec<String> = Vec::new();
            for tp in &u.triples {
                for slot in tp.slots() {
                    if let Some(v) = slot.var()
                        && !vars.iter().any(|x| x == v)
                    {
                        vars.push(v.to_string());
                    }
                }
            }
            let q = Query {
                form: Form::Select,
                distinct: false,
                star: false,
                sel: vars
                    .iter()
                    .map(|v| crate::ast::SelItem {
                        agg: crate::ast::Agg::None,
                        distinct: false,
                        star: false,
                        var: Some(v.clone()),
                        alias: v.clone(),
                    })
                    .collect(),
                group_by: Vec::new(),
                template: Vec::new(),
                pattern: crate::ast::Group {
                    triples: u.triples.clone(),
                    filters: Vec::new(),
                    optionals: Vec::new(),
                    unions: Vec::new(),
                },
                order: Vec::new(),
                limit: -1,
                offset: 0,
                prefixes: Vec::new(),
                base: None,
            };
            let (pvars, rows) = exec_select(conn, &q)?;
            for row in &rows {
                for tp in &u.triples {
                    let mut ids: Vec<i64> = Vec::with_capacity(3);
                    let mut ok = true;
                    for slot in tp.slots() {
                        let id = match slot {
                            Slot::Var(v) => pvars
                                .iter()
                                .position(|x| x == v)
                                .and_then(|i| row[i].as_ref())
                                .map(|t| lookup_term_id(conn, t))
                                .unwrap_or(0),
                            Slot::Ground(t) => lookup_term_id(conn, t),
                        };
                        if id == 0 {
                            ok = false;
                            break;
                        }
                        ids.push(id);
                    }
                    if !ok {
                        continue;
                    }
                    affected += conn
                        .prepare_cached(
                            "DELETE FROM rdf_triples WHERE g=0 AND s=?1 AND p=?2 AND o=?3",
                        )?
                        .execute((ids[0], ids[1], ids[2]))? as i64;
                }
            }
        }
    }
    Ok(affected)
}

/// Parse and execute a SPARQL Update string.
pub fn update(conn: &Connection, sparql: &str) -> Result<i64> {
    match parse(sparql)? {
        Parsed::Update(u) => run_update(conn, &u),
        Parsed::Query(_) => Err(Error(
            "rdf_update runs INSERT DATA / DELETE DATA / DELETE WHERE; use rdf_query() for queries"
                .to_string(),
        )),
    }
}

/// Typed query results.
#[derive(Debug)]
pub enum QueryResults {
    /// SELECT: variable names and one Option<Term> per variable per row.
    Solutions {
        vars: Vec<String>,
        rows: Vec<Vec<Option<Term>>>,
    },
    /// ASK.
    Boolean(bool),
    /// CONSTRUCT.
    Graph(Vec<[Term; 3]>),
}

/// An RDF store over a rusqlite Connection with the SQL surface registered.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (or create) a store at `path`.
    pub fn open(path: &str) -> Result<Store> {
        Self::from_connection(Connection::open(path)?)
    }

    /// Open an in-memory store.
    pub fn open_in_memory() -> Result<Store> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Wrap an existing connection: creates the schema and registers the SQL
    /// functions and the sparql() table function.
    pub fn from_connection(conn: Connection) -> Result<Store> {
        ensure_schema(&conn)?;
        crate::functions::register(&conn)?;
        Ok(Store { conn })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn into_connection(self) -> Connection {
        self.conn
    }

    /// Load Turtle text; returns the number of triples inserted.
    pub fn load_turtle(&self, text: &str, base: Option<&str>) -> Result<i64> {
        load_turtle(&self.conn, text, base)
    }

    /// Load a Turtle file; returns the number of triples inserted.
    pub fn load_turtle_file(&self, path: &str, base: Option<&str>) -> Result<i64> {
        load_turtle_file(&self.conn, path, base)
    }

    /// Run a SPARQL query, returning typed results.
    pub fn query(&self, sparql: &str) -> Result<QueryResults> {
        ensure_schema(&self.conn)?;
        let q = match parse(sparql)? {
            Parsed::Query(q) => q,
            Parsed::Update(_) => {
                return Err(Error("use Store::update for SPARQL Update".to_string()));
            }
        };
        match q.form {
            Form::Select => {
                let (vars, rows) = exec_select(&self.conn, &q)?;
                Ok(QueryResults::Solutions { vars, rows })
            }
            Form::Ask => Ok(QueryResults::Boolean(exec_ask(&self.conn, &q)?)),
            Form::Construct => Ok(QueryResults::Graph(exec_construct(&self.conn, &q)?)),
        }
    }

    /// Run a SPARQL query, returning the serialized form rdf_query() returns.
    pub fn query_serialized(&self, sparql: &str, format: Option<&str>) -> Result<String> {
        run_query(&self.conn, sparql, format)
    }

    /// Execute a SPARQL Update; returns the number of triples affected.
    pub fn update(&self, sparql: &str) -> Result<i64> {
        update(&self.conn, sparql)
    }

    /// Serialize the whole store as Turtle.
    pub fn dump_turtle(&self) -> Result<String> {
        dump_turtle(&self.conn)
    }
}
