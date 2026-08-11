//! CSV both ways (web-api.md sections `GET`/`POST /api/csv/{table}`),
//! shared by the web handlers and the `rsntr csv` subcommands.
//!
//! RFC 4180 with CRLF line ends. Cell encoding: NULL is an empty
//! unquoted field, the empty string is `""`, numbers are their SQL
//! lexical form, a blob is `x'<hex>'` unquoted, text is quoted when it
//! contains commas, quotes, or newlines (or would read back as NULL or
//! a blob). Decoding mirrors this exactly, so export and import
//! round-trip byte-for-byte.
//!
//! The row reads and inserts ride the pipeline as `sql-sqlite`
//! statements, so they are audited and policy-bound. One deviation from
//! the spec's letter: the CREATE TABLE of a create-or-append import
//! cannot ride the pipeline (its authorizer categorically bans DDL), so
//! the create runs directly on the owner surface with a direct `_audit`
//! record, and only then do the inserts go through the pipeline.

use std::sync::Arc;

use resonator_node::Node;
use resonator_node::audit::audit_direct;
use resonator_protocol::{Request, RequestKind, Value};

use crate::error::ApiFailure;
use crate::local::{Collected, run_collect};
use crate::{TableMeta, owner_peer_id, quote_ident};

/// Outcome of one import.
#[derive(Debug, Clone)]
pub struct ImportReport {
    pub table: String,
    pub created: bool,
    pub rows_inserted: i64,
}

// ---------------------------------------------------------------------
// Cell codec
// ---------------------------------------------------------------------

fn is_blob_lexical(field: &str) -> bool {
    let Some(hex) = field
        .strip_prefix("x'")
        .or_else(|| field.strip_prefix("X'"))
        .and_then(|rest| rest.strip_suffix('\''))
    else {
        return false;
    };
    hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Encodes one engine value as one CSV field.
pub fn encode_field(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Blob(b) => {
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            format!("x'{hex}'")
        }
        // Read-only in v1; exported as descriptive quoted text.
        Value::BlobRef { hash, bytes } => quote(&format!("blobref({hash}, {bytes} bytes)")),
        Value::Text(t) => encode_text(t),
    }
}

fn encode_text(t: &str) -> String {
    if t.is_empty() {
        // Distinguishes the empty string from NULL.
        return "\"\"".to_string();
    }
    // Quote when RFC 4180 requires it, or when the raw text would read
    // back as a blob literal.
    if t.contains(',')
        || t.contains('"')
        || t.contains('\n')
        || t.contains('\r')
        || is_blob_lexical(t)
    {
        quote(t)
    } else {
        t.to_string()
    }
}

fn quote(t: &str) -> String {
    let mut out = String::with_capacity(t.len() + 2);
    out.push('"');
    for c in t.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Decodes one CSV field (with its was-quoted flag) into a value.
pub fn field_value(field: &str, quoted: bool) -> Value {
    if !quoted {
        if field.is_empty() {
            return Value::Null;
        }
        if is_blob_lexical(field) {
            let hex = &field[2..field.len() - 1];
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("checked hex"))
                .collect();
            return Value::Blob(bytes);
        }
    }
    // Everything else binds as text; sqlite's type affinity converts
    // numeric strings on numeric columns.
    Value::Text(field.to_string())
}

/// Parses an RFC 4180 document into rows of `(field, was_quoted)`.
/// Accepts LF and CRLF; a trailing newline does not produce an empty
/// row. Bytes after a closing quote in the same field are an error.
pub fn parse_csv(text: &str) -> Result<Vec<Vec<(String, bool)>>, String> {
    let mut rows: Vec<Vec<(String, bool)>> = Vec::new();
    let mut row: Vec<(String, bool)> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut saw_any = false;

    while let Some(c) = chars.next() {
        saw_any = true;
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !quoted => {
                in_quotes = true;
                quoted = true;
            }
            '"' => return Err("quote inside an unquoted field".to_string()),
            ',' => {
                row.push((std::mem::take(&mut field), quoted));
                quoted = false;
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push((std::mem::take(&mut field), quoted));
                quoted = false;
                rows.push(std::mem::take(&mut row));
            }
            '\n' => {
                row.push((std::mem::take(&mut field), quoted));
                quoted = false;
                rows.push(std::mem::take(&mut row));
            }
            c if quoted && !c.is_whitespace() => {
                return Err("data after a closing quote".to_string());
            }
            c => field.push(c),
        }
    }
    if in_quotes {
        return Err("unterminated quoted field".to_string());
    }
    if !field.is_empty() || quoted || !row.is_empty() {
        row.push((field, quoted));
        rows.push(row);
    }
    if !saw_any {
        return Err("empty document".to_string());
    }
    Ok(rows)
}

// ---------------------------------------------------------------------
// Export / import over the pipeline
// ---------------------------------------------------------------------

async fn table_meta(node: &Arc<Node>, table: &str) -> Result<TableMeta, ApiFailure> {
    match crate::read_table_meta(node, table).await? {
        Some(meta) => Ok(meta),
        None => Err(ApiFailure::not_found(format!("no such table {table:?}"))),
    }
}

/// The whole table as an RFC 4180 document (header line first), read
/// through the pipeline as the owner. Truncation by the node's row cap
/// is reported as an error rather than a silently short file.
pub async fn export_csv(node: &Arc<Node>, owner: &str, table: &str) -> Result<String, ApiFailure> {
    let peer = owner_peer_id(owner)?;
    let meta = table_meta(node, table).await?;
    let cols: Vec<String> = meta.columns.iter().map(|c| c.name.clone()).collect();
    let col_list = cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {col_list} FROM {}", quote_ident(table));
    let request = Request::new(RequestKind::Query, "sql-sqlite", sql);
    let collected = run_collect(
        node.clone(),
        peer,
        resonator_transport::basic_hello(&[], None),
        request.to_envelope(),
    )
    .await?;
    let Collected::Rows { rows, done, .. } = collected else {
        return Err(ApiFailure::internal("unexpected response shape"));
    };
    if done.truncated {
        return Err(ApiFailure::error(
            "limit-exceeded",
            format!("table {table:?} exceeds the node's row cap; export would be incomplete"),
        ));
    }

    let mut out = String::new();
    out.push_str(
        &cols
            .iter()
            .map(|c| encode_text(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");
    for row in &rows {
        let line = cols
            .iter()
            .map(|name| {
                // NULL cells are omitted on the wire; restore them.
                row.cells
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| encode_field(v))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push_str("\r\n");
    }
    Ok(out)
}

/// Create-or-append import. When the table is missing and `create` is
/// set it is created with one TEXT column per header field; otherwise
/// the header must be exactly the table's column set (any order,
/// sqlite-style case-insensitive). Inserts ride the pipeline in
/// multi-row chunks.
pub async fn import_csv(
    node: &Arc<Node>,
    owner: &str,
    table: &str,
    text: &str,
    create: bool,
) -> Result<ImportReport, ApiFailure> {
    let peer = owner_peer_id(owner)?;
    let mut parsed = parse_csv(text).map_err(|e| ApiFailure::bad_request(format!("csv: {e}")))?;
    if parsed.is_empty() {
        return Err(ApiFailure::bad_request("csv: a header line is required"));
    }
    let header: Vec<String> = parsed.remove(0).into_iter().map(|(f, _)| f).collect();
    if header.iter().any(|h| h.is_empty()) {
        return Err(ApiFailure::bad_request("csv: empty header field"));
    }
    {
        let mut seen: Vec<String> = header.iter().map(|h| h.to_lowercase()).collect();
        seen.sort();
        seen.dedup();
        if seen.len() != header.len() {
            return Err(ApiFailure::bad_request("csv: duplicate header field"));
        }
    }
    for (i, row) in parsed.iter().enumerate() {
        if row.len() != header.len() {
            return Err(ApiFailure::bad_request(format!(
                "csv: row {} has {} fields, header has {}",
                i + 1,
                row.len(),
                header.len()
            )));
        }
    }

    let meta = crate::read_table_meta(node, table).await?;
    let created = match &meta {
        Some(meta) => {
            let mut have: Vec<String> =
                meta.columns.iter().map(|c| c.name.to_lowercase()).collect();
            let mut want: Vec<String> = header.iter().map(|h| h.to_lowercase()).collect();
            have.sort();
            want.sort();
            if have != want {
                return Err(ApiFailure::bad_request(format!(
                    "csv header does not match the columns of {table:?}"
                )));
            }
            false
        }
        None => {
            if !create {
                return Err(ApiFailure::not_found(format!("no such table {table:?}")));
            }
            let create_sql = format!(
                "CREATE TABLE {} ({})",
                quote_ident(table),
                header
                    .iter()
                    .map(|h| format!("{} TEXT", quote_ident(h)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            // DDL cannot ride the pipeline (categorically banned by the
            // authorizer); the owner surface creates directly and leaves
            // a direct audit record.
            let owner_str = owner.to_string();
            let sql = create_sql.clone();
            node.db()
                .call(move |conn| {
                    let res = conn.execute(&sql, []);
                    let (decision, reason) = match &res {
                        Ok(_) => ("allow", "csv import created the table".to_string()),
                        Err(e) => ("deny", format!("create failed: {e}")),
                    };
                    audit_direct(
                        conn,
                        &owner_str,
                        &ulid::Ulid::new().to_string(),
                        &sql,
                        decision,
                        "node",
                        &reason,
                    );
                    res.map(|_| ())
                })
                .await
                .map_err(ApiFailure::internal)?
                .map_err(|e| ApiFailure::internal(format!("creating {table:?}: {e}")))?;
            true
        }
    };

    // Multi-row INSERT chunks: bounded well under sqlite's bind limit,
    // one pipeline request (and one audit row) per chunk.
    let cols = header.len().max(1);
    let chunk_rows = (20_000 / cols).clamp(1, 500);
    let col_list = header
        .iter()
        .map(|h| quote_ident(h))
        .collect::<Vec<_>>()
        .join(", ");
    let row_tuple = format!("({})", vec!["?"; header.len()].join(", "));

    let mut inserted: i64 = 0;
    for chunk in parsed.chunks(chunk_rows) {
        let sql = format!(
            "INSERT INTO {} ({col_list}) VALUES {}",
            quote_ident(table),
            vec![row_tuple.as_str(); chunk.len()].join(", ")
        );
        let mut request = Request::new(RequestKind::Execute, "sql-sqlite", sql);
        request.params = chunk
            .iter()
            .flat_map(|row| row.iter().map(|(f, q)| field_value(f, *q)))
            .collect();
        let collected = run_collect(
            node.clone(),
            peer,
            resonator_transport::basic_hello(&[], None),
            request.to_envelope(),
        )
        .await?;
        if let Collected::Rows { done, .. } = collected {
            inserted += done.affected_rows.unwrap_or(chunk.len() as i64);
        }
    }

    Ok(ImportReport {
        table: table.to_string(),
        created,
        rows_inserted: inserted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_codec_round_trip() {
        // (value, its encoding, what the encoding decodes back to:
        // numbers come back as text and bind by sqlite affinity).
        let cases: Vec<(Value, &str, Value)> = vec![
            (Value::Null, "", Value::Null),
            (
                Value::Text(String::new()),
                "\"\"",
                Value::Text(String::new()),
            ),
            (
                Value::Text("plain".into()),
                "plain",
                Value::Text("plain".into()),
            ),
            (
                Value::Text("a,b".into()),
                "\"a,b\"",
                Value::Text("a,b".into()),
            ),
            (
                Value::Text("say \"hi\"".into()),
                "\"say \"\"hi\"\"\"",
                Value::Text("say \"hi\"".into()),
            ),
            (
                Value::Text("x'00'".into()),
                "\"x'00'\"",
                Value::Text("x'00'".into()),
            ),
            (Value::Integer(-7), "-7", Value::Text("-7".into())),
            (
                Value::Blob(vec![0xab, 0x01]),
                "x'ab01'",
                Value::Blob(vec![0xab, 0x01]),
            ),
        ];
        for (v, expect, back) in cases {
            let enc = encode_field(&v);
            assert_eq!(enc, expect);
            let doc = format!("h\r\n{enc}\r\n");
            let rows = parse_csv(&doc).unwrap();
            let (f, q) = &rows[1][0];
            assert_eq!(field_value(f, *q), back, "field {expect:?}");
        }
    }

    #[test]
    fn parse_handles_quoted_newlines_and_crlf() {
        let rows = parse_csv("a,b\r\n\"1\r\n2\",x\n,\"\"\r\n").unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1][0], ("1\r\n2".to_string(), true));
        assert_eq!(rows[2][0], (String::new(), false));
        assert_eq!(rows[2][1], (String::new(), true));
        assert!(parse_csv("\"open").is_err());
        assert!(parse_csv("a\"b").is_err());
    }
}
