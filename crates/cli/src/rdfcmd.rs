//! `rsntr sparql` and `rsntr turtle`: first-class RDF commands, local
//! and remote with one shape.
//!
//! Without `--peer` the statement rides the owner channel; with it, the
//! transport — the sparql modulation is served identically on both lanes
//! (`stream_sparql_outcome` in the node crate), so these are thin
//! wrappers over the same envelopes `rsntr query --mod sparql` sends.
//!
//! `turtle load` parses the document client-side and applies it as
//! `INSERT DATA` chunks sized to the frame budget. RDF stores are sets,
//! so re-running a load is a semantic no-op (except blank nodes, which
//! mint fresh identity per run — prefer IRIs in loaded documents).

use std::path::Path;

use anyhow::{Context, Result};

use resonator_protocol::{Request, RequestKind};

use crate::channel::{OwnerChannel, Prefer};
use crate::client::{self, QueryOutcome, QueryReport, classify_sparql};

/// Default `INSERT DATA` chunk budget for `turtle load`, in bytes of
/// triple text — comfortably inside the 256 KiB frame with envelope
/// overhead.
pub const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;

/// One SPARQL text, anywhere: the owner channel without `peer`, the
/// transport with it. The kind (Query vs Execute) follows
/// [`classify_sparql`], matching the remote path.
pub async fn sparql_report(
    dir: &Path,
    prefer: Prefer,
    peer: Option<&str>,
    text: &str,
    offline: bool,
    timeout_ms: Option<i64>,
) -> Result<QueryReport> {
    match peer {
        Some(peer) => client::run_query(dir, peer, "sparql", text, &[], offline, timeout_ms).await,
        None => {
            let ch = OwnerChannel::open(dir, prefer).await?;
            let mut request = Request::new(classify_sparql(text), "sparql", text);
            request.options.timeout_ms = timeout_ms;
            let id = request.id_string();
            let outcome = ch.send(&request.to_envelope(), &id).await?;
            Ok(QueryReport { id, outcome })
        }
    }
}

/// What `rsntr turtle load` did.
#[derive(Debug)]
pub enum LoadOutcome {
    /// Every chunk applied.
    Loaded { triples: usize, chunks: usize },
    /// A chunk was denied or failed; earlier chunks are applied (safe to
    /// re-run: INSERT DATA is set-semantics idempotent).
    Refused(QueryReport),
}

/// Parses `text` as Turtle and applies it as chunked `INSERT DATA`
/// executes — locally over the owner channel, or to `peer` (which needs
/// `write` policy on the RDF store's backing tables).
pub async fn turtle_load(
    dir: &Path,
    prefer: Prefer,
    peer: Option<&str>,
    text: &str,
    offline: bool,
    chunk_bytes: usize,
) -> Result<LoadOutcome> {
    let mut lines = Vec::new();
    for triple in oxttl::TurtleParser::new().for_slice(text.as_bytes()) {
        let triple = triple.context("parsing the turtle document")?;
        lines.push(format!("{triple} ."));
    }
    let triples = lines.len();

    // Greedy pack into INSERT DATA chunks under the byte budget.
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in lines {
        if !current.is_empty() && current.len() + line.len() + 1 > chunk_bytes {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(&line);
        current.push('\n');
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let chunk_count = chunks.len();

    let local = match peer {
        Some(_) => None,
        None => Some(OwnerChannel::open(dir, prefer).await?),
    };
    for chunk in chunks {
        let sql = format!("INSERT DATA {{\n{chunk}}}");
        let report = match (peer, &local) {
            (Some(peer), _) => {
                client::run_statement(
                    dir,
                    peer,
                    RequestKind::Execute,
                    "sparql",
                    &sql,
                    vec![],
                    offline,
                    None,
                )
                .await?
            }
            (None, Some(ch)) => {
                let request = Request::new(RequestKind::Execute, "sparql", &sql);
                let id = request.id_string();
                let outcome = ch.send(&request.to_envelope(), &id).await?;
                QueryReport { id, outcome }
            }
            (None, None) => unreachable!("local channel opened above"),
        };
        match &report.outcome {
            QueryOutcome::Rows { .. } | QueryOutcome::Graph { .. } => {}
            QueryOutcome::Denied(_) | QueryOutcome::Failed(_) => {
                return Ok(LoadOutcome::Refused(report));
            }
            QueryOutcome::Help { .. } => {
                anyhow::bail!("unexpected help response to an INSERT DATA")
            }
        }
    }
    Ok(LoadOutcome::Loaded {
        triples,
        chunks: chunk_count,
    })
}

/// `rsntr turtle dump`: the whole graph as triples — sugar for a
/// catch-all CONSTRUCT (budgeted; `truncated` is surfaced in the
/// report).
pub async fn turtle_dump(
    dir: &Path,
    prefer: Prefer,
    peer: Option<&str>,
    offline: bool,
) -> Result<QueryReport> {
    sparql_report(
        dir,
        prefer,
        peer,
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
        offline,
        None,
    )
    .await
}
