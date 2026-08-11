//! `rsntr csv export` / `rsntr csv import`: the CSV codec shared with
//! the web interface (resonator_web::csv), driven over the owner
//! channel (docs/owner-channel.md sec 5.1).
//!
//! Every row read and insert is an ordinary `sql-sqlite` envelope, so an
//! import is as audited as a remote write; with `--create` the CREATE
//! TABLE rides a preceding DDL Execute, which the owner channel permits
//! (the old gated local pipeline and its `ensure_owner_admitted` seeding
//! are superseded). Inserts are chunked into multi-row INSERTs bounded
//! by sqlite's bind-parameter ceiling and, for the socket transport, a
//! parameter payload target comfortably inside the 256 KiB frame budget.

use std::path::Path;

use anyhow::Result;

use resonator_protocol::{RequestKind, Value};
use resonator_web::csv::{ImportReport, encode_field, field_value, parse_csv};

use crate::channel::{self, OwnerChannel, Prefer};
use crate::client::QueryOutcome;

/// A pipeline outcome the caller maps to an exit code (denied = 2,
/// timeout = 3, everything else 1).
pub use resonator_web::ApiFailure as Failure;
use resonator_web::ApiFailure;

/// Double-quote identifier quoting (sqlite).
fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Rows x columns ceiling per multi-row INSERT (well under sqlite's
/// default bind limit of 32766).
const CHUNK_PARAM_CEILING: usize = 20_000;
/// Rows per chunk, at most.
const CHUNK_ROW_CEILING: usize = 500;
/// Parameter payload target per chunk (doc sec 5.1: the framed envelope
/// must stay inside the 256 KiB budget).
const CHUNK_BYTE_TARGET: usize = 64 * 1024;

/// Maps a terminal outcome that is not plain rows into an [`ApiFailure`].
fn failure_of(outcome: &QueryOutcome) -> Option<ApiFailure> {
    match outcome {
        QueryOutcome::Denied(d) => Some(ApiFailure::Denied {
            reason: d.reason.clone(),
        }),
        QueryOutcome::Failed(e) => Some(ApiFailure::Error {
            code: e.code.clone(),
            reason: e.reason.clone().unwrap_or_default(),
        }),
        _ => None,
    }
}

/// The table's column names in declaration order, or `None` when the
/// table does not exist. Read over the owner channel (pragmas are
/// permitted there).
async fn table_columns(
    ch: &OwnerChannel,
    table: &str,
) -> Result<std::result::Result<Option<Vec<String>>, ApiFailure>> {
    let outcome = channel::run(
        ch,
        RequestKind::Query,
        "sql-sqlite",
        "SELECT name FROM pragma_table_info(?1) ORDER BY cid",
        vec![Value::Text(table.to_string())],
    )
    .await?;
    if let Some(f) = failure_of(&outcome) {
        return Ok(Err(f));
    }
    let QueryOutcome::Rows { rows, .. } = outcome else {
        return Ok(Err(ApiFailure::internal("unexpected response shape")));
    };
    if rows.is_empty() {
        return Ok(Ok(None));
    }
    Ok(Ok(Some(
        rows.iter()
            .map(|r| channel::cell_text(r, "name").unwrap_or_default())
            .collect(),
    )))
}

/// Exports `table` as an RFC 4180 document. The inner `Err` carries a
/// pipeline outcome (denied, unknown table, ...) for exit-code mapping.
pub async fn csv_export(dir: &Path, table: &str) -> Result<Result<String, ApiFailure>> {
    csv_export_with(dir, Prefer::Auto, table).await
}

/// [`csv_export`] with an explicit transport preference.
pub async fn csv_export_with(
    dir: &Path,
    prefer: Prefer,
    table: &str,
) -> Result<Result<String, ApiFailure>> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    let cols = match table_columns(&ch, table).await? {
        Ok(Some(cols)) => cols,
        Ok(None) => {
            return Ok(Err(ApiFailure::not_found(format!(
                "no such table {table:?}"
            ))));
        }
        Err(f) => return Ok(Err(f)),
    };
    let col_list = cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {col_list} FROM {}", quote_ident(table));
    let outcome = channel::run(&ch, RequestKind::Query, "sql-sqlite", &sql, vec![]).await?;
    if let Some(f) = failure_of(&outcome) {
        return Ok(Err(f));
    }
    let QueryOutcome::Rows { rows, done, .. } = outcome else {
        return Ok(Err(ApiFailure::internal("unexpected response shape")));
    };
    if done.truncated {
        return Ok(Err(ApiFailure::error(
            "limit-exceeded",
            format!("table {table:?} exceeds the node's row cap; export would be incomplete"),
        )));
    }

    let mut out = String::new();
    out.push_str(
        &cols
            .iter()
            .map(|c| encode_field(&Value::Text(c.clone())))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");
    for row in &rows {
        let line = cols
            .iter()
            .map(|name| encode_field(channel::cell(row, name)))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push_str("\r\n");
    }
    Ok(Ok(out))
}

/// Imports an RFC 4180 document into `table`; `create` allows creating
/// a missing table with one TEXT column per header field.
pub async fn csv_import(
    dir: &Path,
    table: &str,
    text: &str,
    create: bool,
) -> Result<Result<ImportReport, ApiFailure>> {
    csv_import_with(dir, Prefer::Auto, table, text, create).await
}

/// [`csv_import`] with an explicit transport preference.
pub async fn csv_import_with(
    dir: &Path,
    prefer: Prefer,
    table: &str,
    text: &str,
    create: bool,
) -> Result<Result<ImportReport, ApiFailure>> {
    let mut parsed = match parse_csv(text) {
        Ok(rows) => rows,
        Err(e) => return Ok(Err(ApiFailure::bad_request(format!("csv: {e}")))),
    };
    if parsed.is_empty() {
        return Ok(Err(ApiFailure::bad_request(
            "csv: a header line is required",
        )));
    }
    let header: Vec<String> = parsed.remove(0).into_iter().map(|(f, _)| f).collect();
    if header.iter().any(|h| h.is_empty()) {
        return Ok(Err(ApiFailure::bad_request("csv: empty header field")));
    }
    {
        let mut seen: Vec<String> = header.iter().map(|h| h.to_lowercase()).collect();
        seen.sort();
        seen.dedup();
        if seen.len() != header.len() {
            return Ok(Err(ApiFailure::bad_request("csv: duplicate header field")));
        }
    }
    for (i, row) in parsed.iter().enumerate() {
        if row.len() != header.len() {
            return Ok(Err(ApiFailure::bad_request(format!(
                "csv: row {} has {} fields, header has {}",
                i + 1,
                row.len(),
                header.len()
            ))));
        }
    }

    let ch = OwnerChannel::open(dir, prefer).await?;
    let created = match table_columns(&ch, table).await? {
        Err(f) => return Ok(Err(f)),
        Ok(Some(cols)) => {
            let mut have: Vec<String> = cols.iter().map(|c| c.to_lowercase()).collect();
            let mut want: Vec<String> = header.iter().map(|h| h.to_lowercase()).collect();
            have.sort();
            want.sort();
            if have != want {
                return Ok(Err(ApiFailure::bad_request(format!(
                    "csv header does not match the columns of {table:?}"
                ))));
            }
            false
        }
        Ok(None) => {
            if !create {
                return Ok(Err(ApiFailure::not_found(format!(
                    "no such table {table:?}"
                ))));
            }
            // The owner channel permits DDL; the create rides an
            // ordinary Execute and lands on the ledger like any other.
            let create_sql = format!(
                "CREATE TABLE {} ({})",
                quote_ident(table),
                header
                    .iter()
                    .map(|h| format!("{} TEXT", quote_ident(h)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let outcome =
                channel::run(&ch, RequestKind::Execute, "sql-sqlite", &create_sql, vec![]).await?;
            if let Some(f) = failure_of(&outcome) {
                return Ok(Err(f));
            }
            true
        }
    };

    // Multi-row INSERT chunks: one Execute (one ULID, one audit row, one
    // `_applied` row) per chunk, so a retried import resumes chunk by
    // chunk.
    let cols = header.len().max(1);
    let max_chunk_rows = (CHUNK_PARAM_CEILING / cols).clamp(1, CHUNK_ROW_CEILING);
    let col_list = header
        .iter()
        .map(|h| quote_ident(h))
        .collect::<Vec<_>>()
        .join(", ");
    let row_tuple = format!("({})", vec!["?"; header.len()].join(", "));

    let mut inserted: i64 = 0;
    let mut chunk: Vec<Value> = Vec::new();
    let mut chunk_rows = 0usize;
    let mut chunk_bytes = 0usize;
    let mut it = parsed.into_iter().peekable();
    while let Some(row) = it.next() {
        for (f, q) in &row {
            chunk_bytes += f.len() + 8;
            chunk.push(field_value(f, *q));
        }
        chunk_rows += 1;
        let flush =
            chunk_rows >= max_chunk_rows || chunk_bytes >= CHUNK_BYTE_TARGET || it.peek().is_none();
        if !flush {
            continue;
        }
        let sql = format!(
            "INSERT INTO {} ({col_list}) VALUES {}",
            quote_ident(table),
            vec![row_tuple.as_str(); chunk_rows].join(", ")
        );
        let outcome = channel::run(
            &ch,
            RequestKind::Execute,
            "sql-sqlite",
            &sql,
            std::mem::take(&mut chunk),
        )
        .await?;
        if let Some(f) = failure_of(&outcome) {
            return Ok(Err(f));
        }
        if let QueryOutcome::Rows { done, .. } = outcome {
            inserted += done.affected_rows.unwrap_or(chunk_rows as i64);
        }
        chunk_rows = 0;
        chunk_bytes = 0;
    }

    Ok(Ok(ImportReport {
        table: table.to_string(),
        created,
        rows_inserted: inserted,
    }))
}
