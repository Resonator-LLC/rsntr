//! SPARQL-over-SQLite engine for Resonator v3.
//!
//! An RDF triple store on plain SQLite tables (rdf_terms dictionary +
//! rdf_triples with a graph column) and a SPARQL subset compiled into a
//! single SQL SELECT per query. Turtle goes in; SPARQL 1.1 JSON results
//! (SELECT/ASK) or Turtle/N-Triples (CONSTRUCT, dumps) come out.
//!
//! Behavioral oracle: the rdfsparql.c C engine from the research corpus;
//! its test suite is ported as this crate's conformance suite.
//!
//! # Supported subset
//!
//! - Query forms: SELECT (`*`/list/DISTINCT), CONSTRUCT, ASK (compiled as a
//!   LIMIT 1 existence check).
//! - Patterns: basic graph patterns (`;`/`,` shorthand), OPTIONAL, FILTER,
//!   UNION chains.
//! - FILTER: `=`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `||`, `!`, bound(),
//!   regex() (Rust regex crate, 'i' flag), str(), lang(), datatype().
//! - GROUP BY with COUNT(*)/COUNT(?x)/COUNT(DISTINCT ?x)/SUM/MIN/MAX/AVG;
//!   ORDER BY ASC/DESC; LIMIT/OFFSET; PREFIX/BASE.
//! - SPARQL Update: INSERT DATA / DELETE DATA / DELETE WHERE.
//! - GRAPH/MINUS/BIND/VALUES/SERVICE/HAVING/DESCRIBE, property paths and
//!   subqueries report clean errors.
//!
//! # Documented simplifications (kept from the C engine)
//!
//! - FILTER comparisons are pragmatic, not fully spec-typed: comparisons
//!   against a numeric literal are numeric (CAST to REAL); everything else
//!   compares lexical forms; `=`/`!=` between two bare variables compares
//!   whole RDF terms by dictionary id.
//! - A filter referencing a variable unbound in its branch evaluates to
//!   false (bound() false, !bound() true), approximating SPARQL's
//!   error-eliminates-row semantics via SQL NULL propagation.
//! - Multiple OPTIONAL groups are independent left joins; variables shared
//!   only between two OPTIONAL groups are not joined across them.
//! - SUM/MIN/MAX/AVG are numeric (values cast to REAL) and carry
//!   xsd:decimal; COUNT carries xsd:integer.
//! - UNION is compiled by distributing it over the enclosing group into a
//!   UNION ALL of conjunctive queries, capped at 128 alternatives.
//! - Pattern matching against a numeric literal matches its lexical form
//!   (30 matches "30", not "30.0").
//! - Blank node labels are freshened per load, so re-loading the same
//!   document re-inserts bnode-scoped triples under new labels.
//! - SPARQL-side IRI resolution skips dot-segment normalization (Turtle
//!   loads go through oxttl, which resolves fully).
//! - The g (graph) column exists from day 1 but everything reads and writes
//!   the default graph g = 0 until GRAPH support lands.

pub mod ast;
pub mod compile;
pub mod parser;
pub mod serialize;
pub mod term;

#[cfg(feature = "native")]
pub mod functions;
#[cfg(feature = "native")]
pub mod store;
#[cfg(feature = "native")]
pub mod vtab;

pub use ast::{Form, Parsed, Query, Update};
pub use parser::{ParseError, parse};
pub use term::Term;

#[cfg(feature = "native")]
pub use functions::register;
#[cfg(feature = "native")]
pub use store::{Error, QueryResults, Store};
