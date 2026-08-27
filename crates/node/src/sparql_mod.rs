//! The `sparql` wire modulation, backed by resonator-sparql.
//!
//! - `rsntr:Query` + mod `sparql` carries SELECT/ASK/CONSTRUCT text.
//!   SELECT and ASK answer as `rsntr:Result`/`rsntr:Row`/`rsntr:Done` like
//!   sql-sqlite; CONSTRUCT answers as `rsntr:Graph` frames plus `Done`.
//! - `rsntr:Execute` + mod `sparql` carries SPARQL Update text (INSERT
//!   DATA / DELETE DATA / DELETE WHERE), idempotent via `_applied`.
//!
//! Policy story (PLAN.md section 11 initial stance): the whole store is
//! gated by `_policy` rows on the `rdf_triples`/`rdf_terms` tables; a read
//! needs `read` on them, an update needs `write`. Graph-level policy
//! follows named-graph support.
//!
//! Result cells carry terms in N-Triples lexical form (`<iri>`, `_:b`,
//! `"lit"`, `"lit"@lang`, `"lit"^^<dtype>`) so IRIs and literals stay
//! unambiguous inside a plain-text cell; unbound variables are omitted
//! like SQL NULLs.

use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Triple};
use rusqlite::{Connection, OptionalExtension};

use resonator_authenticator::{ActionKind, Chain, Decision, Footprint};
use resonator_protocol::{Request, RequestKind, Value};
use resonator_sparql::term::{K_BNODE, K_IRI};
use resonator_sparql::{Form, Parsed, Term, parse, store};

use crate::audit::{audit_full, audit_full_dir};
use crate::clock::now_rfc3339;

/// The synthetic footprint the sparql modulation presents to the chain:
/// the triple store's backing tables.
pub fn sparql_footprint(kind: ActionKind) -> Footprint {
    Footprint::from_tables(
        kind,
        [
            ("rdf_triples", Vec::<String>::new()),
            ("rdf_terms", Vec::<String>::new()),
        ],
    )
}

/// What the db-thread job resolved; the async side turns this into frames.
pub enum SparqlOutcome {
    Denied {
        reason: String,
    },
    /// Bad SPARQL, or a query/update carried in the wrong envelope class.
    BadRequest {
        message: String,
    },
    EngineError {
        message: String,
    },
    Select {
        vars: Vec<String>,
        rows: Vec<Vec<Option<Term>>>,
        truncated: bool,
    },
    Ask(bool),
    Construct {
        triples: Vec<Triple>,
    },
    Updated {
        affected: i64,
    },
}

/// Runs on the db thread, after the peer gate: parse, chain decide, audit,
/// execute. `row_cap` truncates SELECT results.
pub fn gate_and_run_sparql(
    conn: &mut Connection,
    peer: &str,
    req: &Request,
    chain: &Chain,
    row_cap: i64,
) -> SparqlOutcome {
    let id = req.id_string();
    let parsed = match parse(&req.signal) {
        Ok(p) => p,
        Err(e) => {
            return SparqlOutcome::BadRequest {
                message: format!("sparql parse error: {e}"),
            };
        }
    };
    let (kind, action) = match (&parsed, req.kind) {
        (Parsed::Query(_), RequestKind::Query) => (ActionKind::Read, "read"),
        (Parsed::Update(_), RequestKind::Execute) => (ActionKind::Write, "write"),
        (Parsed::Query(_), RequestKind::Execute) => {
            return SparqlOutcome::BadRequest {
                message: "a SPARQL query rides a rsntr:Query, not rsntr:Execute".into(),
            };
        }
        (Parsed::Update(_), RequestKind::Query) => {
            return SparqlOutcome::BadRequest {
                message: "a SPARQL update rides a rsntr:Execute, not rsntr:Query".into(),
            };
        }
    };

    let footprint = sparql_footprint(kind);
    let start = std::time::Instant::now();
    let decided = chain.decide(conn, peer, action, &footprint, &req.signal);
    let duration = start.elapsed().as_millis() as u64;
    let (decision_str, reason) = match &decided.decision {
        Decision::Allow | Decision::AllowNarrowed { .. } => ("allow", None),
        Decision::Deny { reason } => ("deny", Some(reason.clone())),
        Decision::Escalate => (
            "deny",
            Some("authenticator chain did not decide".to_string()),
        ),
    };
    audit_full(
        conn,
        peer,
        &id,
        &req.signal,
        &footprint.to_json(),
        decision_str,
        &decided.decided_by,
        reason.as_deref(),
        duration,
    );
    match decided.decision {
        Decision::Allow => {}
        // A narrowed rewrite for sparql would be replacement SPARQL text;
        // no tier emits one yet, so refuse rather than half-support it.
        Decision::AllowNarrowed { .. } => {
            return SparqlOutcome::Denied {
                reason: "allow-narrowed is not supported for the sparql modulation".into(),
            };
        }
        Decision::Deny { reason } => return SparqlOutcome::Denied { reason },
        Decision::Escalate => {
            return SparqlOutcome::Denied {
                reason: "authenticator chain did not decide".into(),
            };
        }
    }

    execute_parsed(conn, &id, parsed, row_cap)
}

/// Owner-channel variant (docs/owner-channel.md sec 4): parse and class
/// checks as on the remote path, then no chain decide; the audit row is
/// `allow`/`owner`/`local` and execution runs under the same row cap.
pub fn run_sparql_owner(
    conn: &mut Connection,
    peer: &str,
    req: &Request,
    row_cap: i64,
) -> SparqlOutcome {
    let id = req.id_string();
    let parsed = match parse(&req.signal) {
        Ok(p) => p,
        Err(e) => {
            return SparqlOutcome::BadRequest {
                message: format!("sparql parse error: {e}"),
            };
        }
    };
    let kind = match (&parsed, req.kind) {
        (Parsed::Query(_), RequestKind::Query) => ActionKind::Read,
        (Parsed::Update(_), RequestKind::Execute) => ActionKind::Write,
        (Parsed::Query(_), RequestKind::Execute) => {
            return SparqlOutcome::BadRequest {
                message: "a SPARQL query rides a rsntr:Query, not rsntr:Execute".into(),
            };
        }
        (Parsed::Update(_), RequestKind::Query) => {
            return SparqlOutcome::BadRequest {
                message: "a SPARQL update rides a rsntr:Execute, not rsntr:Query".into(),
            };
        }
    };
    let footprint = sparql_footprint(kind);
    audit_full_dir(
        conn,
        "local",
        peer,
        &id,
        &req.signal,
        &footprint.to_json(),
        "allow",
        "owner",
        None,
        0,
    );
    execute_parsed(conn, &id, parsed, row_cap)
}

/// The shared execution tail: runs a parsed, already-decided query or
/// update.
fn execute_parsed(conn: &mut Connection, id: &str, parsed: Parsed, row_cap: i64) -> SparqlOutcome {
    match parsed {
        Parsed::Query(q) => match q.form {
            Form::Select => match store::exec_select(conn, &q) {
                Ok((vars, mut rows)) => {
                    let truncated = (rows.len() as i64) > row_cap;
                    if truncated {
                        rows.truncate(row_cap.max(0) as usize);
                    }
                    SparqlOutcome::Select {
                        vars,
                        rows,
                        truncated,
                    }
                }
                Err(e) => SparqlOutcome::EngineError { message: e.0 },
            },
            Form::Ask => match store::exec_ask(conn, &q) {
                Ok(b) => SparqlOutcome::Ask(b),
                Err(e) => SparqlOutcome::EngineError { message: e.0 },
            },
            Form::Construct => match store::exec_construct(conn, &q) {
                Ok(triples) => SparqlOutcome::Construct {
                    triples: triples.iter().filter_map(ox_triple).collect(),
                },
                Err(e) => SparqlOutcome::EngineError { message: e.0 },
            },
        },
        Parsed::Update(u) => run_update_idempotent(conn, id, &u),
    }
}

/// Applies a SPARQL update inside a transaction with `_applied`
/// idempotency: a retransmitted id is answered from the record.
fn run_update_idempotent(
    conn: &mut Connection,
    id: &str,
    u: &resonator_sparql::Update,
) -> SparqlOutcome {
    let recorded: Option<String> = conn
        .query_row(
            "SELECT outcome FROM _applied WHERE request_id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    if let Some(json) = recorded {
        let outcome: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
        let affected = outcome
            .get("affected_rows")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        return SparqlOutcome::Updated { affected };
    }
    let tx = match conn.transaction() {
        Ok(tx) => tx,
        Err(e) => {
            return SparqlOutcome::EngineError {
                message: e.to_string(),
            };
        }
    };
    let affected = match store::run_update(&tx, u) {
        Ok(n) => n,
        Err(e) => return SparqlOutcome::EngineError { message: e.0 },
    };
    let record = tx.execute(
        "INSERT INTO _applied (request_id, at, outcome) VALUES (?1, ?2, ?3)",
        (
            id,
            now_rfc3339(),
            serde_json::json!({ "affected_rows": affected }).to_string(),
        ),
    );
    if let Err(e) = record {
        return SparqlOutcome::EngineError {
            message: e.to_string(),
        };
    }
    match tx.commit() {
        Ok(()) => SparqlOutcome::Updated { affected },
        Err(e) => SparqlOutcome::EngineError {
            message: e.to_string(),
        },
    }
}

/// One solution cell as an envelope value: N-Triples lexical form.
pub fn term_value(t: &Term) -> Value {
    Value::Text(nt_term(t))
}

/// N-Triples serialization of one term.
pub fn nt_term(t: &Term) -> String {
    match t.kind {
        K_IRI => format!("<{}>", t.lex),
        K_BNODE => format!("_:{}", t.lex),
        _ => {
            let mut out = String::with_capacity(t.lex.len() + 8);
            out.push('"');
            for c in t.lex.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c => out.push(c),
                }
            }
            out.push('"');
            if !t.lang.is_empty() {
                out.push('@');
                out.push_str(&t.lang);
            } else if !t.dtype.is_empty() {
                out.push_str("^^<");
                out.push_str(&t.dtype);
                out.push('>');
            }
            out
        }
    }
}

/// Converts one engine triple into an oxrdf Triple for a Graph frame.
/// Invalid IRIs or labels drop the triple (defensive; the store only
/// holds what the loader accepted).
fn ox_triple(t: &[Term; 3]) -> Option<Triple> {
    let subject: NamedOrBlankNode = match t[0].kind {
        K_IRI => NamedNode::new(&t[0].lex).ok()?.into(),
        K_BNODE => BlankNode::new(t[0].lex.as_str()).ok()?.into(),
        _ => return None,
    };
    let predicate = NamedNode::new(&t[1].lex).ok()?;
    let object: oxrdf::Term = match t[2].kind {
        K_IRI => NamedNode::new(&t[2].lex).ok()?.into(),
        K_BNODE => BlankNode::new(t[2].lex.as_str()).ok()?.into(),
        _ => {
            if !t[2].lang.is_empty() {
                Literal::new_language_tagged_literal(&t[2].lex, &t[2].lang)
                    .ok()?
                    .into()
            } else if !t[2].dtype.is_empty() {
                Literal::new_typed_literal(&t[2].lex, NamedNode::new(&t[2].dtype).ok()?).into()
            } else {
                Literal::new_simple_literal(&t[2].lex).into()
            }
        }
    };
    Some(Triple::new(subject, predicate, object))
}

/// Coarse encoded-size estimate of one payload triple, for frame chunking.
pub fn triple_estimate(t: &Triple) -> usize {
    t.subject.to_string().len() + t.predicate.to_string().len() + t.object.to_string().len() + 16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nt_term_forms() {
        assert_eq!(nt_term(&Term::iri("http://ex.org/a")), "<http://ex.org/a>");
        assert_eq!(nt_term(&Term::bnode("b0")), "_:b0");
        assert_eq!(
            nt_term(&Term::lit("hi \"there\"\n")),
            "\"hi \\\"there\\\"\\n\""
        );
        assert_eq!(nt_term(&Term::lit_lang("hallo", "de")), "\"hallo\"@de");
        assert_eq!(
            nt_term(&Term::lit_dt(
                "5",
                "http://www.w3.org/2001/XMLSchema#integer"
            )),
            "\"5\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        );
    }

    #[test]
    fn ox_triple_conversion() {
        let t = [
            Term::iri("http://ex.org/s"),
            Term::iri("http://ex.org/p"),
            Term::lit_dt("5", "http://www.w3.org/2001/XMLSchema#integer"),
        ];
        let ox = ox_triple(&t).unwrap();
        assert_eq!(ox.predicate.as_str(), "http://ex.org/p");
        // Literal subjects are invalid and dropped.
        let bad = [Term::lit("x"), Term::iri("http://ex.org/p"), Term::lit("y")];
        assert!(ox_triple(&bad).is_none());
    }
}
