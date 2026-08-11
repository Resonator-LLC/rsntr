//! The JSON convenience API under `/api` (web-api.md section 8): thin
//! translations onto envelope requests submitted through the same local
//! pipeline as everything else. JSON shapes match the CLI's `--json`
//! output field-for-field.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use resonator_authenticator::{ActionKind, Chain, Decision};
use resonator_node::audit::audit_full;
use resonator_node::sparql_mod::sparql_footprint;
use resonator_protocol::{Request, RequestKind, Row, Value};

use crate::error::ApiFailure;
use crate::local::{Collected, run_collect};
use crate::{WebState, quote_ident, read_table_meta};

const MAX_JSON_BODY: usize = 8 * 1024 * 1024;
const MAX_UPLOAD_BODY: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------
// JSON plumbing
// ---------------------------------------------------------------------

/// One engine value as JSON, matching the CLI's `value_to_json`.
pub(crate) fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Integer(i) => json!(i),
        Value::Real(f) => match serde_json::Number::from_f64(*f) {
            Some(n) => Json::Number(n),
            None => json!(f.to_string()),
        },
        Value::Text(t) => json!(t),
        Value::Blob(b) => {
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            json!({ "blob_hex": hex })
        }
        Value::BlobRef { hash, bytes } => json!({ "blobref": { "hash": hash, "bytes": bytes } }),
    }
}

/// One JSON value as an engine value (request direction).
fn json_to_value(j: &Json) -> Result<Value, ApiFailure> {
    match j {
        Json::Null => Ok(Value::Null),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Real(f))
            } else {
                Err(ApiFailure::bad_request(format!("unbindable number {n}")))
            }
        }
        Json::String(s) => Ok(Value::Text(s.clone())),
        Json::Object(o) => {
            if let Some(Json::String(hex)) = o.get("blob_hex") {
                let mut bytes = Vec::with_capacity(hex.len() / 2);
                if hex.len() % 2 != 0 {
                    return Err(ApiFailure::bad_request("blob_hex has odd length"));
                }
                for i in (0..hex.len()).step_by(2) {
                    bytes.push(
                        u8::from_str_radix(&hex[i..i + 2], 16)
                            .map_err(|_| ApiFailure::bad_request("blob_hex is not hex"))?,
                    );
                }
                Ok(Value::Blob(bytes))
            } else {
                Err(ApiFailure::bad_request(
                    "objects bind only as {\"blob_hex\": \"...\"}",
                ))
            }
        }
        other => Err(ApiFailure::bad_request(format!(
            "JSON value {other} cannot bind as an engine value"
        ))),
    }
}

/// One row as a JSON array in header column order, NULLs restored.
fn row_to_json(columns: &[String], row: &Row) -> Json {
    Json::Array(
        columns
            .iter()
            .map(|name| {
                row.cells
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| value_to_json(v))
                    .unwrap_or(Json::Null)
            })
            .collect(),
    )
}

fn json_response(status: StatusCode, body: Json) -> Response {
    (
        status,
        [
            ("Content-Type", "application/json"),
            ("Cache-Control", "no-store"),
        ],
        body.to_string(),
    )
        .into_response()
}

fn ok_json(body: Json) -> Response {
    json_response(StatusCode::OK, body)
}

/// The JSON error body for one failure, with the request id when known.
pub(crate) fn failure_response(f: &ApiFailure, id: Option<&str>) -> Response {
    let mut body = match f {
        ApiFailure::Denied { reason } => json!({ "ok": false, "denied": reason }),
        ApiFailure::Error { code, reason } => {
            json!({ "ok": false, "error": { "code": code, "reason": reason } })
        }
    };
    if let Some(id) = id {
        body["id"] = json!(id);
    }
    json_response(f.status(), body)
}

async fn read_json_body(headers: &HeaderMap, body: Body) -> Result<Json, Response> {
    let is_json = headers
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|v| v == "application/json");
    if !is_json {
        return Err(failure_response(
            &ApiFailure::error("bad-request", "expected Content-Type application/json"),
            None,
        )
        .into_status(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    }
    let bytes = axum::body::to_bytes(body, MAX_JSON_BODY).await.map_err(|_| {
        json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({ "ok": false, "error": { "code": "limit-exceeded", "reason": "body too large" } }),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|e| {
        failure_response(
            &ApiFailure::bad_request(format!("invalid JSON body: {e}")),
            None,
        )
    })
}

/// Rewrites a response's status (for 415 reuse of the JSON error body).
trait IntoStatus {
    fn into_status(self, status: StatusCode) -> Response;
}
impl IntoStatus for Response {
    fn into_status(mut self, status: StatusCode) -> Response {
        *self.status_mut() = status;
        self
    }
}

/// Runs one statement through the local pipeline as the owner.
async fn run_owner(
    state: &WebState,
    kind: RequestKind,
    modulation: &str,
    signal: String,
    params: Vec<Value>,
) -> (String, Result<Collected, ApiFailure>) {
    let mut request = Request::new(kind, modulation, signal);
    request.params = params;
    let id = request.id_string();
    let collected = run_collect(
        state.node.clone(),
        state.owner,
        state.hello.clone(),
        request.to_envelope(),
    )
    .await;
    (id, collected)
}

/// The CLI's query report shape.
fn report_json(
    id: &str,
    columns: &[String],
    rows: &[Row],
    done: &resonator_protocol::Done,
) -> Json {
    json!({
        "ok": true,
        "id": id,
        "columns": columns,
        "rows": rows.iter().map(|r| row_to_json(columns, r)).collect::<Vec<_>>(),
        "row_count": done.row_count,
        "affected_rows": done.affected_rows,
        "last_insert_rowid": done.last_insert_rowid,
        "truncated": done.truncated,
    })
}

// ---------------------------------------------------------------------
// GET /api/meta
// ---------------------------------------------------------------------

pub(crate) async fn meta(State(state): State<Arc<WebState>>) -> Response {
    let hello = match state.node.hello().await {
        Ok(h) => h,
        Err(e) => return failure_response(&ApiFailure::internal(e), None),
    };
    let (id, collected) = run_owner(
        &state,
        RequestKind::Query,
        "sql-sqlite",
        "SELECT m.name, c.comment FROM sqlite_master m \
         LEFT JOIN _comments c ON c.name = m.name \
         WHERE m.type = 'table' ORDER BY m.name"
            .to_string(),
        Vec::new(),
    )
    .await;
    let tables = match collected {
        Ok(Collected::Rows { rows, .. }) => rows
            .iter()
            .filter_map(|r| {
                // NULL cells are omitted from Row frames: look up by
                // column name, not position.
                let name = r.cells.iter().find_map(|(n, v)| match v {
                    Value::Text(t) if n == "name" => Some(t.as_str()),
                    _ => None,
                })?;
                let comment = r.cells.iter().find_map(|(n, v)| match v {
                    Value::Text(t) if n == "comment" => Some(t.as_str()),
                    _ => None,
                });
                Some(json!({
                    "name": name,
                    "reserved": name.starts_with('_'),
                    "comment": comment,
                }))
            })
            .collect::<Vec<_>>(),
        Ok(_) => return failure_response(&ApiFailure::internal("unexpected shape"), Some(&id)),
        Err(f) => return failure_response(&f, Some(&id)),
    };
    ok_json(json!({
        "ok": true,
        "node_id": state.node_id,
        "ver": hello.envelope_version,
        "mods": hello.mods,
        "tables": tables,
    }))
}

// ---------------------------------------------------------------------
// GET /api/peer/{id}
// ---------------------------------------------------------------------

/// How long a live hello probe may take. This is also the bound on how
/// long the transport's connection-pool mutex is held for us:
/// `IrohTransport::open` keeps the pool locked across the dial, so
/// cancelling the future on timeout releases it for relay traffic.
const PEER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

#[derive(Deserialize)]
pub(crate) struct PeerGetParams {
    /// `?local=1` skips the live probe (list rendering needs rows only).
    local: Option<String>,
}

/// One admitted peer: the local `_peers` row plus, unless `?local=1`, a
/// live probe that surfaces the peer's wire hello (mods, encodings,
/// hint) - received on every dial and otherwise discarded.
///
/// Caveats, honestly: a pooled connection to a peer that just died looks
/// alive for up to the transport's 10s idle timeout, so `reachable:
/// true` is optimistic in that window (the detail view's projection
/// fetch is the real probe); and the hello is cached from dial time.
pub(crate) async fn peer_get(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
    Query(params): Query<PeerGetParams>,
) -> Response {
    let peer: resonator_transport::PeerId = match id.parse() {
        Ok(p) => p,
        Err(_) => {
            return failure_response(
                &ApiFailure::bad_request("peer must be a 64-hex endpoint id"),
                None,
            );
        }
    };
    let (rid, collected) = run_owner(
        &state,
        RequestKind::Query,
        "sql-sqlite",
        "SELECT name, addrs, added_at, last_seen, notes FROM _peers \
         WHERE endpoint_id = ?1"
            .to_string(),
        vec![Value::Text(peer.to_string())],
    )
    .await;
    let row = match collected {
        Ok(Collected::Rows { columns, rows, .. }) => match rows.first() {
            Some(r) => {
                let cell = |name: &str| {
                    columns
                        .iter()
                        .position(|c| c == name)
                        .and_then(|_| r.cells.iter().find(|(n, _)| n == name))
                        .map(|(_, v)| value_to_json(v))
                        .unwrap_or(Json::Null)
                };
                let addrs = match cell("addrs") {
                    Json::String(raw) => {
                        serde_json::from_str::<Vec<String>>(&raw).map(Json::from).unwrap_or(Json::Null)
                    }
                    _ => Json::Null,
                };
                json!({
                    "endpoint_id": peer.to_string(),
                    "name": cell("name"),
                    "addrs": addrs,
                    "added_at": cell("added_at"),
                    "last_seen": cell("last_seen"),
                    "notes": cell("notes"),
                })
            }
            None => {
                return failure_response(
                    &ApiFailure::not_found(format!("{peer} is not in _peers")),
                    Some(&rid),
                );
            }
        },
        Ok(_) => return failure_response(&ApiFailure::internal("unexpected shape"), Some(&rid)),
        Err(f) => return failure_response(&f, Some(&rid)),
    };

    let skip_probe = params.local.as_deref() == Some("1");
    let live = match (&state.transport, skip_probe) {
        (Some(transport), false) => {
            use resonator_transport::{RequestStream, Transport};
            match tokio::time::timeout(PEER_PROBE_TIMEOUT, transport.open(peer)).await {
                Ok(Ok((mut stream, hello))) => {
                    // FIN the empty stream: the acceptor's route_request
                    // treats a zero-frame stream as a clean no-op.
                    let _ = stream.finish().await;
                    json!({
                        "reachable": true,
                        "ver": hello.envelope_version,
                        "encodings": hello.encodings,
                        "mods": hello.mods,
                        "hint": hello.hint,
                    })
                }
                Ok(Err(e)) => json!({ "reachable": false, "error": e.to_string() }),
                Err(_) => json!({
                    "reachable": false,
                    "error": format!("probe timed out after {}s", PEER_PROBE_TIMEOUT.as_secs()),
                }),
            }
        }
        _ => Json::Null,
    };

    ok_json(json!({ "ok": true, "peer": row, "live": live }))
}

// ---------------------------------------------------------------------
// GET /api/table/{name}
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct PageParams {
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(crate) async fn table_get(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(page): Query<PageParams>,
) -> Response {
    let limit = page.limit.unwrap_or(100).clamp(0, 1000);
    let offset = page.offset.unwrap_or(0).max(0);

    let meta = match read_table_meta(&state.node, &name).await {
        Ok(Some(m)) => m,
        Ok(None) => {
            return failure_response(
                &ApiFailure::not_found(format!("no such table {name:?}")),
                None,
            );
        }
        Err(f) => return failure_response(&f, None),
    };

    // Total, through the pipeline.
    let (id, counted) = run_owner(
        &state,
        RequestKind::Query,
        "sql-sqlite",
        format!("SELECT count(*) AS n FROM {}", quote_ident(&name)),
        Vec::new(),
    )
    .await;
    let total = match counted {
        Ok(Collected::Rows { rows, .. }) => rows
            .first()
            .and_then(|r| r.cells.first())
            .and_then(|(_, v)| match v {
                Value::Integer(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0),
        Ok(_) => return failure_response(&ApiFailure::internal("unexpected shape"), Some(&id)),
        Err(f) => return failure_response(&f, Some(&id)),
    };

    // The page, through the pipeline. rowid is selected under an alias
    // that cannot collide with a declared column; a WITHOUT ROWID table
    // (or one declaring its own `rowid` column) pages without rowids.
    let has_rowids = !meta.without_rowid
        && !meta
            .columns
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case("rowid"));
    let col_list = meta
        .columns
        .iter()
        .map(|c| quote_ident(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = if has_rowids {
        format!(
            "SELECT rowid AS __rsntr_rowid, {col_list} FROM {} LIMIT ?1 OFFSET ?2",
            quote_ident(&name)
        )
    } else {
        format!(
            "SELECT {col_list} FROM {} LIMIT ?1 OFFSET ?2",
            quote_ident(&name)
        )
    };
    let (id, collected) = run_owner(
        &state,
        RequestKind::Query,
        "sql-sqlite",
        sql,
        vec![Value::Integer(limit), Value::Integer(offset)],
    )
    .await;
    let rows = match collected {
        Ok(Collected::Rows { rows, .. }) => rows,
        Ok(_) => return failure_response(&ApiFailure::internal("unexpected shape"), Some(&id)),
        Err(f) => return failure_response(&f, Some(&id)),
    };

    let data_cols: Vec<String> = meta.columns.iter().map(|c| c.name.clone()).collect();
    let mut out_rows = Vec::with_capacity(rows.len());
    let mut rowids = Vec::with_capacity(rows.len());
    for row in &rows {
        out_rows.push(row_to_json(&data_cols, row));
        rowids.push(if has_rowids {
            row.cells
                .iter()
                .find(|(n, _)| n == "__rsntr_rowid")
                .map(|(_, v)| value_to_json(v))
                .unwrap_or(Json::Null)
        } else {
            Json::Null
        });
    }

    ok_json(json!({
        "ok": true,
        "table": name,
        "columns": meta.columns.iter().map(|c| json!({
            "name": c.name,
            "decltype": c.decltype,
            "pk": c.pk,
            "notnull": c.notnull,
        })).collect::<Vec<_>>(),
        "rows": out_rows,
        "rowids": rowids,
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
}

// ---------------------------------------------------------------------
// Row editing
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct ValuesBody {
    values: serde_json::Map<String, Json>,
}

fn parse_values(body: Json) -> Result<Vec<(String, Value)>, ApiFailure> {
    let parsed: ValuesBody = serde_json::from_value(body)
        .map_err(|e| ApiFailure::bad_request(format!("expected {{\"values\": {{...}}}}: {e}")))?;
    let mut out = Vec::with_capacity(parsed.values.len());
    for (name, j) in parsed.values {
        out.push((name, json_to_value(&j)?));
    }
    Ok(out)
}

/// Confirms the table exists (404 otherwise) before building SQL on it.
async fn require_table(state: &WebState, name: &str) -> Result<(), Response> {
    match read_table_meta(&state.node, name).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(failure_response(
            &ApiFailure::not_found(format!("no such table {name:?}")),
            None,
        )),
        Err(f) => Err(failure_response(&f, None)),
    }
}

pub(crate) async fn row_insert(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = require_table(&state, &name).await {
        return resp;
    }
    let json_body = match read_json_body(&headers, body).await {
        Ok(j) => j,
        Err(resp) => return resp,
    };
    let values = match parse_values(json_body) {
        Ok(v) => v,
        Err(f) => return failure_response(&f, None),
    };
    let sql = if values.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES", quote_ident(&name))
    } else {
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            quote_ident(&name),
            values
                .iter()
                .map(|(n, _)| quote_ident(n))
                .collect::<Vec<_>>()
                .join(", "),
            (1..=values.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let params: Vec<Value> = values.into_iter().map(|(_, v)| v).collect();
    let (id, collected) = run_owner(&state, RequestKind::Execute, "sql-sqlite", sql, params).await;
    match collected {
        Ok(Collected::Rows { done, .. }) => json_response(
            StatusCode::CREATED,
            json!({ "ok": true, "last_insert_rowid": done.last_insert_rowid }),
        ),
        Ok(_) => failure_response(&ApiFailure::internal("unexpected shape"), Some(&id)),
        Err(f) => failure_response(&f, Some(&id)),
    }
}

pub(crate) async fn row_update(
    State(state): State<Arc<WebState>>,
    Path((name, rowid)): Path<(String, i64)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(resp) = require_table(&state, &name).await {
        return resp;
    }
    let json_body = match read_json_body(&headers, body).await {
        Ok(j) => j,
        Err(resp) => return resp,
    };
    let values = match parse_values(json_body) {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => return failure_response(&ApiFailure::bad_request("no values to set"), None),
        Err(f) => return failure_response(&f, None),
    };
    let sets = values
        .iter()
        .enumerate()
        .map(|(i, (n, _))| format!("{} = ?{}", quote_ident(n), i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE {} SET {sets} WHERE rowid = ?{}",
        quote_ident(&name),
        values.len() + 1
    );
    let mut params: Vec<Value> = values.into_iter().map(|(_, v)| v).collect();
    params.push(Value::Integer(rowid));
    let (id, collected) = run_owner(&state, RequestKind::Execute, "sql-sqlite", sql, params).await;
    match collected {
        Ok(Collected::Rows { done, .. }) => {
            if done.affected_rows == Some(0) {
                failure_response(
                    &ApiFailure::not_found(format!("no row {rowid} in {name:?}")),
                    Some(&id),
                )
            } else {
                ok_json(json!({ "ok": true, "affected_rows": done.affected_rows }))
            }
        }
        Ok(_) => failure_response(&ApiFailure::internal("unexpected shape"), Some(&id)),
        Err(f) => failure_response(&f, Some(&id)),
    }
}

pub(crate) async fn row_delete(
    State(state): State<Arc<WebState>>,
    Path((name, rowid)): Path<(String, i64)>,
) -> Response {
    if let Err(resp) = require_table(&state, &name).await {
        return resp;
    }
    let sql = format!("DELETE FROM {} WHERE rowid = ?1", quote_ident(&name));
    let (id, collected) = run_owner(
        &state,
        RequestKind::Execute,
        "sql-sqlite",
        sql,
        vec![Value::Integer(rowid)],
    )
    .await;
    match collected {
        Ok(Collected::Rows { done, .. }) => {
            if done.affected_rows == Some(0) {
                failure_response(
                    &ApiFailure::not_found(format!("no row {rowid} in {name:?}")),
                    Some(&id),
                )
            } else {
                ok_json(json!({ "ok": true, "affected_rows": done.affected_rows }))
            }
        }
        Ok(_) => failure_response(&ApiFailure::internal("unexpected shape"), Some(&id)),
        Err(f) => failure_response(&f, Some(&id)),
    }
}

// ---------------------------------------------------------------------
// POST /api/sql and /api/sparql
// ---------------------------------------------------------------------

/// Read shapes ride `rsntr:Query`, everything else `rsntr:Execute`;
/// the serving side derives the real kind from its own footprint.
pub(crate) fn classify_sql(sql: &str) -> RequestKind {
    let first = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match first.as_str() {
        "select" | "with" | "values" | "explain" => RequestKind::Query,
        _ => RequestKind::Execute,
    }
}

/// SELECT/ASK/CONSTRUCT/DESCRIBE are queries, update forms executes.
pub(crate) fn classify_sparql(text: &str) -> RequestKind {
    for word in text.split_whitespace() {
        match word.to_ascii_lowercase().as_str() {
            "select" | "ask" | "construct" | "describe" => return RequestKind::Query,
            "insert" | "delete" | "load" | "clear" | "create" | "drop" | "with" | "move"
            | "copy" | "add" => return RequestKind::Execute,
            _ => {}
        }
    }
    RequestKind::Query
}

#[derive(Deserialize)]
struct SqlBody {
    sql: String,
    #[serde(default)]
    params: Vec<Json>,
}

pub(crate) async fn sql(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let json_body = match read_json_body(&headers, body).await {
        Ok(j) => j,
        Err(resp) => return resp,
    };
    let parsed: SqlBody = match serde_json::from_value(json_body) {
        Ok(p) => p,
        Err(e) => {
            return failure_response(
                &ApiFailure::bad_request(format!("expected {{\"sql\", \"params\"?}}: {e}")),
                None,
            );
        }
    };
    let mut params = Vec::with_capacity(parsed.params.len());
    for j in &parsed.params {
        match json_to_value(j) {
            Ok(v) => params.push(v),
            Err(f) => return failure_response(&f, None),
        }
    }
    let kind = classify_sql(&parsed.sql);
    let (id, collected) = run_owner(&state, kind, "sql-sqlite", parsed.sql, params).await;
    match collected {
        Ok(Collected::Rows {
            columns,
            rows,
            done,
            ..
        }) => ok_json(report_json(&id, &columns, &rows, &done)),
        Ok(_) => failure_response(&ApiFailure::internal("unexpected shape"), Some(&id)),
        Err(f) => failure_response(&f, Some(&id)),
    }
}

#[derive(Deserialize)]
struct SparqlBody {
    query: String,
}

pub(crate) async fn sparql(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let json_body = match read_json_body(&headers, body).await {
        Ok(j) => j,
        Err(resp) => return resp,
    };
    let parsed: SparqlBody = match serde_json::from_value(json_body) {
        Ok(p) => p,
        Err(e) => {
            return failure_response(
                &ApiFailure::bad_request(format!("expected {{\"query\"}}: {e}")),
                None,
            );
        }
    };
    let kind = classify_sparql(&parsed.query);
    let (id, collected) = run_owner(&state, kind, "sparql", parsed.query, Vec::new()).await;
    match (kind, collected) {
        (RequestKind::Execute, Ok(Collected::Rows { done, .. })) => ok_json(json!({
            "ok": true,
            "id": id,
            "affected_rows": done.affected_rows,
        })),
        (
            _,
            Ok(Collected::Rows {
                columns,
                rows,
                done,
                ..
            }),
        ) => ok_json(report_json(&id, &columns, &rows, &done)),
        (_, Ok(Collected::Graph { triples, done })) => ok_json(json!({
            "ok": true,
            "id": id,
            "triples": triples.iter().map(|t| format!("{t} .")).collect::<Vec<_>>(),
            "triple_count": done.row_count,
            "truncated": done.truncated,
        })),
        (_, Err(f)) => failure_response(&f, Some(&id)),
    }
}

// ---------------------------------------------------------------------
// POST /api/turtle
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct TurtleParams {
    base: Option<String>,
}

/// Loads Turtle into the store. The spec's letter routes this through a
/// `sql-sqlite` execute of `rdf_load_turtle`, but the pipeline's
/// enforce-mode authorizer cannot approve the function's inner writes;
/// the store write is gated here with exactly the sparql modulation's
/// write path (same chain, same `rdf_*` footprint, same `_audit` row)
/// and then applied on the db thread.
pub(crate) async fn turtle(
    State(state): State<Arc<WebState>>,
    Query(params): Query<TurtleParams>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let is_turtle = headers
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|v| v == "text/turtle");
    if !is_turtle {
        return failure_response(
            &ApiFailure::error("bad-request", "expected Content-Type text/turtle"),
            None,
        )
        .into_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let bytes = match axum::body::to_bytes(body, MAX_UPLOAD_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({ "ok": false, "error": { "code": "limit-exceeded", "reason": "body too large" } }),
            );
        }
    };
    let text = match String::from_utf8(bytes.to_vec()) {
        Ok(t) => t,
        Err(_) => {
            return failure_response(&ApiFailure::bad_request("body is not UTF-8"), None);
        }
    };

    let peer = state.node_id.clone();
    let id = ulid::Ulid::new().to_string();
    let audit_id = id.clone();
    let base = params.base;
    let result = state
        .node
        .db()
        .call(move |conn| {
            let chain = Chain::with_builtin_tiers();
            let footprint = sparql_footprint(ActionKind::Write);
            let start = std::time::Instant::now();
            let decided = chain.decide(conn, &peer, "write", &footprint, "rdf_load_turtle");
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
                &peer,
                &audit_id,
                "rdf_load_turtle",
                &footprint.to_json(),
                decision_str,
                &decided.decided_by,
                reason.as_deref(),
                duration,
            );
            match decided.decision {
                Decision::Allow | Decision::AllowNarrowed { .. } => {
                    resonator_sparql::store::load_turtle(conn, &text, base.as_deref())
                        .map_err(|e| ApiFailure::error("engine-error", e.0))
                }
                Decision::Deny { reason } => Err(ApiFailure::Denied {
                    reason: Some(reason),
                }),
                Decision::Escalate => Err(ApiFailure::Denied {
                    reason: Some("authenticator chain did not decide".to_string()),
                }),
            }
        })
        .await;
    match result {
        Ok(Ok(count)) => ok_json(json!({ "ok": true, "triple_count": count })),
        Ok(Err(f)) => failure_response(&f, Some(&id)),
        Err(e) => failure_response(&ApiFailure::internal(e), Some(&id)),
    }
}

// ---------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------

pub(crate) async fn csv_get(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
) -> Response {
    match crate::csv::export_csv(&state.node, &state.node_id, &name).await {
        Ok(doc) => (
            StatusCode::OK,
            [
                ("Content-Type", "text/csv; charset=utf-8".to_string()),
                (
                    "Content-Disposition",
                    format!("attachment; filename=\"{}.csv\"", name.replace('"', "")),
                ),
                ("Cache-Control", "no-store".to_string()),
            ],
            doc,
        )
            .into_response(),
        Err(f) => failure_response(&f, None),
    }
}

pub(crate) async fn csv_post(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let is_csv = headers
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|v| v == "text/csv");
    if !is_csv {
        return failure_response(
            &ApiFailure::error("bad-request", "expected Content-Type text/csv"),
            None,
        )
        .into_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let bytes = match axum::body::to_bytes(body, MAX_UPLOAD_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({ "ok": false, "error": { "code": "limit-exceeded", "reason": "body too large" } }),
            );
        }
    };
    let text = match String::from_utf8(bytes.to_vec()) {
        Ok(t) => t,
        Err(_) => return failure_response(&ApiFailure::bad_request("body is not UTF-8"), None),
    };
    match crate::csv::import_csv(&state.node, &state.node_id, &name, &text, true).await {
        Ok(report) => ok_json(json!({
            "ok": true,
            "table": report.table,
            "created": report.created,
            "rows_inserted": report.rows_inserted,
        })),
        Err(f) => failure_response(&f, None),
    }
}
