//! SQL virtual-table surfaces over the wire protocol: interactive remote
//! reads and writes as plain SQL (the synchronous sibling of the outbox).
//!
//! Two vtabs, both registered by [`register_remote_vtabs`]:
//!
//! - `remote_query(peer, signal, p1..p8)`: eponymous table-valued
//!   function running one statement against a peer, one output row per
//!   result row. sqlite's vtab schema is declared once at connect time,
//!   before any query text exists, so per-call typed columns are
//!   impossible with the rusqlite vtab API; the pragmatic choice is JSON
//!   output columns (`row` a JSON array aligned to `columns`, `cells` a
//!   JSON object keyed by column name), typed back with sqlite's own
//!   `json_extract`/`json_each`.
//! - `CREATE VIRTUAL TABLE x USING iroh_remote(peer=..., table=...)`:
//!   the schema-mirroring form with typed columns. Simple predicates
//!   (`=`, `<`, `<=`, `>`, `>=`, `<>`, `LIKE`) push down through
//!   `xBestIndex` into the remote SELECT; INSERT/UPDATE/DELETE map to
//!   remote Execute statements, each carrying a fresh ULID request id so
//!   the serving side's `_applied` idempotency holds on any resend. The
//!   remote table must be a rowid table (`rowid` is the row identity the
//!   write path addresses). Column names come from a `columns=a,b,c` arg
//!   when given, else from a `SELECT * FROM t LIMIT 0` probe against the
//!   peer at CREATE/connect time (which needs the peer reachable and the
//!   read allowed).
//!
//! sqlite's vtab callbacks are synchronous and the transport is async:
//! each remote call is spawned onto the owning tokio runtime and the
//! calling thread blocks on a channel with a deadline
//! ([`RemoteContext::run_blocking`]). That is acceptable for interactive
//! use and wrong for bulk work; bulk work belongs to the outbox. Never
//! call through these vtabs from a task on the same runtime thread pool
//! that has nothing else to run the network work on; the node's
//! dedicated db thread (where owner-lane statements execute) is the
//! intended caller.
//!
//! On the serving side these are ordinary requests: the peer's gate,
//! footprint, authenticator chain, limits, and audit all apply. A denial
//! surfaces as a SQL error naming the reason.

use std::borrow::Cow;
use std::ffi::{CStr, CString, c_int};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::types::ValueRef;
use rusqlite::vtab::{
    Context, CreateVTab, Filters, IndexConstraintOp, IndexInfo, Inserts, Module, UpdateVTab,
    Updates, VTab, VTabConnection, VTabCursor, VTabKind,
};
use rusqlite::{Connection, Error, ffi};

use resonator_protocol::{Done, Request, RequestKind, ResponseEvent, ResponseReader, Row, Value};
use resonator_transport::{PeerId, RequestStream, Transport};

use crate::outbox::{row_to_json, value_to_json};

/// Default per-call deadline when the context sets none.
pub const DEFAULT_REMOTE_TIMEOUT: Duration = Duration::from_secs(15);

/// How one remote call failed.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// The peer name resolved to nothing (not 64-hex, resolver had no row).
    #[error("unknown peer {0:?}")]
    UnknownPeer(String),
    /// The serving side's authenticator said no.
    #[error("denied by the peer: {0}")]
    Denied(String),
    /// An `rsntr:Error` frame: `code: reason`.
    #[error("remote error: {0}")]
    Failed(String),
    /// Dial/stream trouble before or during the exchange.
    #[error("transport: {0}")]
    Transport(String),
    /// The deadline elapsed without a terminal frame.
    #[error("timed out: no answer within {}ms", .0.as_millis())]
    Timeout(Duration),
    /// The response broke choreography.
    #[error("protocol: {0}")]
    Protocol(String),
}

/// The terminal shape of one remote statement: header columns, rows, and
/// the closing `rsntr:Done`.
#[derive(Debug)]
pub struct RemoteReply {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub done: Done,
}

/// Sends one request over `transport` and drains the response: the
/// library seam under the vtabs (the CLI's client machinery reshaped to a
/// [`Transport`] handle). sparql CONSTRUCT graph frames come back as one
/// row per triple under a single `triple` column.
pub async fn run_remote<T: Transport>(
    transport: &T,
    peer: PeerId,
    request: &Request,
) -> Result<RemoteReply, RemoteError> {
    let id = request.id_string();
    let envelope = request.to_envelope();

    let (mut stream, _peer_hello) = transport
        .open(peer)
        .await
        .map_err(|e| RemoteError::Transport(format!("opening a stream to {peer}: {e}")))?;
    stream
        .send(&envelope)
        .await
        .map_err(|e| RemoteError::Transport(format!("sending the request: {e}")))?;
    stream
        .finish()
        .await
        .map_err(|e| RemoteError::Transport(format!("half-closing the request stream: {e}")))?;

    let mut reader = ResponseReader::new(&id);
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    while let Some(frame) = stream
        .recv()
        .await
        .map_err(|e| RemoteError::Transport(format!("receiving the response: {e}")))?
    {
        match reader
            .accept(frame)
            .map_err(|e| RemoteError::Protocol(e.to_string()))?
        {
            ResponseEvent::Header(h) => columns = h.columns,
            ResponseEvent::Rows(batch) => rows.extend(batch),
            ResponseEvent::Graph(g) => {
                columns = vec!["triple".to_string()];
                for t in g.payload {
                    rows.push(Row {
                        seq: rows.len() as i64,
                        cells: vec![("triple".to_string(), Value::Text(format!("{t} .")))],
                    });
                }
            }
            // Passthrough frames from mods this surface has no view of.
            ResponseEvent::Generic(_) => {}
            ResponseEvent::Done(done) => {
                return Ok(RemoteReply {
                    columns,
                    rows,
                    done,
                });
            }
            ResponseEvent::Denied(d) => {
                return Err(RemoteError::Denied(
                    d.reason.unwrap_or_else(|| "request denied".to_string()),
                ));
            }
            ResponseEvent::Error(e) => {
                return Err(RemoteError::Failed(format!(
                    "{}: {}",
                    e.code,
                    e.reason.unwrap_or_default()
                )));
            }
            ResponseEvent::Help { .. } | ResponseEvent::Media(_) => {
                return Err(RemoteError::Protocol(
                    "unexpected help/media response to a SQL request".to_string(),
                ));
            }
        }
    }
    reader
        .finish()
        .map_err(|e| RemoteError::Protocol(e.to_string()))?;
    Err(RemoteError::Protocol(
        "response ended without a terminal frame".to_string(),
    ))
}

/// Object-safe face of [`run_remote`] so [`RemoteContext`] can hold any
/// [`Transport`] without a generic parameter (the vtab aux type must be
/// concrete).
trait RemoteCaller: Send + Sync {
    fn call(
        &self,
        peer: PeerId,
        request: Request,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RemoteReply, RemoteError>> + Send + 'static>,
    >;
}

struct TransportCaller<T: Transport>(Arc<T>);

impl<T: Transport> RemoteCaller for TransportCaller<T> {
    fn call(
        &self,
        peer: PeerId,
        request: Request,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RemoteReply, RemoteError>> + Send + 'static>,
    > {
        let transport = self.0.clone();
        Box::pin(async move { run_remote(transport.as_ref(), peer, &request).await })
    }
}

/// Resolves a peer argument (petname or 64-hex endpoint id) to a
/// [`PeerId`]. Must NOT go through the node's [`resonator_node::DbHandle`]:
/// the vtab blocks that very thread while a call is in flight. Open your
/// own read-only connection instead (see the serve wiring).
pub type PeerResolver = Arc<dyn Fn(&str) -> Option<PeerId> + Send + Sync>;

/// Everything a registered vtab needs to reach the network: the tokio
/// runtime that drives the transport, the transport itself, the peer
/// resolver, and the per-call deadline.
pub struct RemoteContext {
    runtime: tokio::runtime::Handle,
    caller: Arc<dyn RemoteCaller>,
    resolver: Option<PeerResolver>,
    timeout: Duration,
}

impl RemoteContext {
    /// A context over `transport`, dispatching its async work on
    /// `runtime`. Without a resolver only 64-hex endpoint ids name peers.
    pub fn new<T: Transport>(transport: Arc<T>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            runtime,
            caller: Arc::new(TransportCaller(transport)),
            resolver: None,
            timeout: DEFAULT_REMOTE_TIMEOUT,
        }
    }

    /// Adds petname resolution.
    pub fn with_resolver(mut self, resolver: PeerResolver) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Overrides the default per-call deadline.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn resolve(&self, peer: &str) -> Result<PeerId, RemoteError> {
        if let Some(resolver) = &self.resolver
            && let Some(id) = resolver(peer)
        {
            return Ok(id);
        }
        peer.parse()
            .map_err(|_| RemoteError::UnknownPeer(peer.to_string()))
    }

    /// Runs one request to `peer` and blocks the calling thread until the
    /// reply, an error, or the deadline (`timeout`, defaulting to the
    /// context's). The network work runs on the context's runtime; the
    /// caller must not be a thread that runtime needs.
    pub fn run_blocking(
        &self,
        peer: &str,
        request: Request,
        timeout: Option<Duration>,
    ) -> Result<RemoteReply, RemoteError> {
        let timeout = timeout.unwrap_or(self.timeout);
        let peer_id = self.resolve(peer)?;
        let fut = self.caller.call(peer_id, request);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.runtime.spawn(async move {
            let out = match tokio::time::timeout(timeout, fut).await {
                Ok(r) => r,
                Err(_elapsed) => Err(RemoteError::Timeout(timeout)),
            };
            let _ = tx.send(out);
        });
        // The inner tokio timeout is authoritative; the grace only covers
        // a wedged runtime.
        match rx.recv_timeout(timeout + Duration::from_secs(2)) {
            Ok(out) => out,
            Err(_) => Err(RemoteError::Timeout(timeout)),
        }
    }
}

/// Registers `remote_query` and `iroh_remote` on `conn`. Call on the
/// node's serving connection once a transport exists; without a serving
/// transport (plain `rsntr` in-process runs) the vtabs are simply absent.
pub fn register_remote_vtabs(conn: &Connection, ctx: Arc<RemoteContext>) -> rusqlite::Result<()> {
    const REMOTE_QUERY: Module<RemoteQueryTab> = Module::eponymous_only_module();
    const IROH_REMOTE: Module<IrohRemoteTab> = Module::update_module();
    conn.create_module(c"remote_query", &REMOTE_QUERY, Some(ctx.clone()))?;
    conn.create_module(c"iroh_remote", &IROH_REMOTE, Some(ctx))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Read shapes go out as Query, everything else as Execute; a labeling
/// heuristic, the serving side derives the real kind from its own
/// footprint.
fn classify_sql(sql: &str) -> RequestKind {
    let first = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match first.as_str() {
        "select" | "with" | "values" | "explain" | "pragma" => RequestKind::Query,
        _ => RequestKind::Execute,
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn value_from_ref(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Integer(i),
        ValueRef::Real(f) => Value::Real(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    }
}

fn set_value(ctx: &mut Context, v: &Value) -> rusqlite::Result<()> {
    match v {
        Value::Null => ctx.set_result(&rusqlite::types::Null),
        Value::Integer(i) => ctx.set_result(i),
        Value::Real(f) => ctx.set_result(f),
        Value::Text(s) => ctx.set_result(s),
        Value::Blob(b) => ctx.set_result(b),
        Value::BlobRef { hash, .. } => ctx.set_result(hash),
    }
}

fn module_err(surface: &str, e: RemoteError) -> Error {
    Error::ModuleError(format!("{surface}: {e}"))
}

/// A request with the deadline mirrored into the wire options, so the
/// serving side can cap itself too.
fn make_request(kind: RequestKind, signal: &str, params: Vec<Value>, timeout: Duration) -> Request {
    let mut request = Request::new(kind, "sql-sqlite", signal);
    request.params = params;
    request.options.timeout_ms = Some(timeout.as_millis() as i64);
    request
}

// ---------------------------------------------------------------------------
// remote_query: eponymous table-valued function
// ---------------------------------------------------------------------------

const RQ_COL_ROW: c_int = 0;
const RQ_COL_CELLS: c_int = 1;
const RQ_COL_COLUMNS: c_int = 2;
const RQ_COL_PEER: c_int = 3;
const RQ_COL_SIGNAL: c_int = 4;
const RQ_MAX_PARAMS: c_int = 8;
const RQ_USAGE: &str =
    "remote_query: usage SELECT row FROM remote_query('peer', 'SELECT ...' [, p1..p8])";

#[repr(C)]
pub struct RemoteQueryTab {
    base: ffi::sqlite3_vtab,
    ctx: Arc<RemoteContext>,
}

unsafe impl<'vtab> VTab<'vtab> for RemoteQueryTab {
    type Aux = Arc<RemoteContext>;
    type Cursor = RemoteQueryCursor;

    fn connect(
        _db: &mut VTabConnection,
        aux: Option<&Self::Aux>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        _args: &[&[u8]],
    ) -> rusqlite::Result<(Cow<'static, CStr>, Self)> {
        let ctx = aux
            .ok_or_else(|| Error::ModuleError("remote_query: missing context".to_string()))?
            .clone();
        Ok((
            Cow::Borrowed(
                c"CREATE TABLE x(row TEXT, cells TEXT, columns TEXT, \
                  peer TEXT HIDDEN, signal TEXT HIDDEN, \
                  p1 HIDDEN, p2 HIDDEN, p3 HIDDEN, p4 HIDDEN, \
                  p5 HIDDEN, p6 HIDDEN, p7 HIDDEN, p8 HIDDEN)",
            ),
            RemoteQueryTab {
                base: unsafe { std::mem::zeroed() },
                ctx,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> rusqlite::Result<bool> {
        // (input column, constraint index) for every usable equality on a
        // hidden column; the bitmask over input columns is the idx_num,
        // argv order is ascending column order.
        let mut present: Vec<(c_int, usize)> = Vec::new();
        let mut required_unusable = false;
        for (i, constraint) in info.constraints().enumerate() {
            let col = constraint.column();
            if !(RQ_COL_PEER..RQ_COL_PEER + 2 + RQ_MAX_PARAMS).contains(&col) {
                continue;
            }
            if !constraint.is_usable() {
                if col == RQ_COL_PEER || col == RQ_COL_SIGNAL {
                    required_unusable = true;
                }
                continue;
            }
            if matches!(
                constraint.operator(),
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ
            ) && !present.iter().any(|(c, _)| *c == col)
            {
                present.push((col, i));
            }
        }
        let have = |col: c_int| present.iter().any(|(c, _)| *c == col);
        if required_unusable && (!have(RQ_COL_PEER) || !have(RQ_COL_SIGNAL)) {
            // This plan cannot deliver the required arguments; tell
            // sqlite to find another.
            return Ok(false);
        }
        present.sort_unstable();
        let mut mask: c_int = 0;
        for (n, (col, ci)) in present.iter().enumerate() {
            let mut usage = info.constraint_usage(*ci);
            usage.set_argv_index((n + 1) as c_int);
            usage.set_omit(true);
            mask |= 1 << (col - RQ_COL_PEER);
        }
        info.set_idx_num(mask);
        info.set_estimated_cost(1_000_000.0);
        Ok(true)
    }

    fn open(&'_ mut self) -> rusqlite::Result<RemoteQueryCursor> {
        Ok(RemoteQueryCursor {
            base: unsafe { std::mem::zeroed() },
            ctx: self.ctx.clone(),
            peer: String::new(),
            signal: String::new(),
            params: Vec::new(),
            columns_json: String::new(),
            rows: Vec::new(),
            pos: 0,
        })
    }
}

#[repr(C)]
pub struct RemoteQueryCursor {
    base: ffi::sqlite3_vtab_cursor,
    ctx: Arc<RemoteContext>,
    peer: String,
    signal: String,
    params: Vec<Value>,
    columns_json: String,
    /// Per result row: (JSON array aligned to columns, JSON object).
    rows: Vec<(String, String)>,
    pos: usize,
}

unsafe impl VTabCursor for RemoteQueryCursor {
    fn filter(
        &mut self,
        idx_num: c_int,
        _idx_str: Option<&str>,
        args: &Filters<'_>,
    ) -> rusqlite::Result<()> {
        self.rows.clear();
        self.pos = 0;
        self.params.clear();
        let mut peer: Option<String> = None;
        let mut signal: Option<String> = None;
        let mut arg_iter = args.iter();
        for bit in 0..(2 + RQ_MAX_PARAMS) {
            if idx_num & (1 << bit) == 0 {
                continue;
            }
            let value = arg_iter
                .next()
                .ok_or_else(|| Error::ModuleError(RQ_USAGE.to_string()))?;
            match bit {
                0 | 1 => {
                    let Value::Text(text) = value_from_ref(value) else {
                        return Err(Error::ModuleError(RQ_USAGE.to_string()));
                    };
                    if bit == 0 {
                        peer = Some(text);
                    } else {
                        signal = Some(text);
                    }
                }
                _ => self.params.push(value_from_ref(value)),
            }
        }
        let (Some(peer), Some(signal)) = (peer, signal) else {
            return Err(Error::ModuleError(RQ_USAGE.to_string()));
        };
        self.peer = peer;
        self.signal = signal;

        let request = make_request(
            classify_sql(&self.signal),
            &self.signal,
            self.params.clone(),
            self.ctx.timeout,
        );
        let reply = self
            .ctx
            .run_blocking(&self.peer, request, None)
            .map_err(|e| module_err("remote_query", e))?;

        self.columns_json = serde_json::to_string(&reply.columns).unwrap_or_default();
        self.rows = reply
            .rows
            .iter()
            .map(|row| {
                let arr = row_to_json(&reply.columns, row);
                let obj: serde_json::Map<String, serde_json::Value> = row
                    .cells
                    .iter()
                    .map(|(name, v)| (name.clone(), value_to_json(v)))
                    .collect();
                (arr, serde_json::Value::Object(obj).to_string())
            })
            .collect();
        Ok(())
    }

    fn next(&mut self) -> rusqlite::Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, ctx: &mut Context, i: c_int) -> rusqlite::Result<()> {
        match i {
            RQ_COL_ROW => ctx.set_result(&self.rows[self.pos].0),
            RQ_COL_CELLS => ctx.set_result(&self.rows[self.pos].1),
            RQ_COL_COLUMNS => ctx.set_result(&self.columns_json),
            RQ_COL_PEER => ctx.set_result(&self.peer),
            RQ_COL_SIGNAL => ctx.set_result(&self.signal),
            _ => {
                let idx = (i - RQ_COL_SIGNAL - 1) as usize;
                match self.params.get(idx) {
                    Some(v) => set_value(ctx, v),
                    None => ctx.set_result(&rusqlite::types::Null),
                }
            }
        }
    }

    fn rowid(&self) -> rusqlite::Result<i64> {
        Ok(self.pos as i64 + 1)
    }
}

// ---------------------------------------------------------------------------
// iroh_remote: schema-mirroring virtual table
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct IrohRemoteTab {
    base: ffi::sqlite3_vtab,
    ctx: Arc<RemoteContext>,
    peer: String,
    table: String,
    cols: Vec<String>,
    timeout: Option<Duration>,
}

/// `key=value` args of the CREATE VIRTUAL TABLE statement, values with
/// optional single or double quotes.
fn parse_module_args(args: &[&[u8]]) -> Result<Vec<(String, String)>, Error> {
    let mut out = Vec::new();
    for raw in args {
        let text = std::str::from_utf8(raw)
            .map_err(|_| Error::ModuleError("iroh_remote: argument is not UTF-8".to_string()))?;
        let Some((key, value)) = text.split_once('=') else {
            return Err(Error::ModuleError(format!(
                "iroh_remote: argument {text:?} is not key=value"
            )));
        };
        let value = value.trim();
        let value = value
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .unwrap_or(value);
        out.push((key.trim().to_ascii_lowercase(), value.to_string()));
    }
    Ok(out)
}

impl IrohRemoteTab {
    fn exec(&self, sql: String, params: Vec<Value>) -> rusqlite::Result<RemoteReply> {
        let request = make_request(
            RequestKind::Execute,
            &sql,
            params,
            self.timeout.unwrap_or(self.ctx.timeout),
        );
        self.ctx
            .run_blocking(&self.peer, request, self.timeout)
            .map_err(|e| module_err("iroh_remote", e))
    }
}

unsafe impl<'vtab> VTab<'vtab> for IrohRemoteTab {
    type Aux = Arc<RemoteContext>;
    type Cursor = IrohRemoteCursor;

    fn connect(
        _db: &mut VTabConnection,
        aux: Option<&Self::Aux>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        args: &[&[u8]],
    ) -> rusqlite::Result<(Cow<'static, CStr>, Self)> {
        let ctx = aux
            .ok_or_else(|| Error::ModuleError("iroh_remote: missing context".to_string()))?
            .clone();
        let mut peer = None;
        let mut table = None;
        let mut cols: Vec<String> = Vec::new();
        let mut timeout = None;
        for (key, value) in parse_module_args(args)? {
            match key.as_str() {
                "peer" => peer = Some(value),
                "table" => table = Some(value),
                "columns" => {
                    cols = value
                        .split(',')
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect();
                }
                "timeout_ms" => {
                    let ms: u64 = value.parse().map_err(|_| {
                        Error::ModuleError(format!("iroh_remote: bad timeout_ms {value:?}"))
                    })?;
                    timeout = Some(Duration::from_millis(ms));
                }
                other => {
                    return Err(Error::ModuleError(format!(
                        "iroh_remote: unknown argument {other:?} \
                         (expected peer, table, columns, timeout_ms)"
                    )));
                }
            }
        }
        let peer = peer.ok_or_else(|| {
            Error::ModuleError("iroh_remote: peer=... argument is required".to_string())
        })?;
        let table = table.ok_or_else(|| {
            Error::ModuleError("iroh_remote: table=... argument is required".to_string())
        })?;

        if cols.is_empty() {
            // Schema probe against the peer; a denial or unreachable peer
            // fails the CREATE with its reason.
            let request = make_request(
                RequestKind::Query,
                &format!("SELECT * FROM {} LIMIT 0", quote_ident(&table)),
                Vec::new(),
                timeout.unwrap_or(ctx.timeout),
            );
            let reply = ctx
                .run_blocking(&peer, request, timeout)
                .map_err(|e| module_err("iroh_remote", e))?;
            cols = reply.columns;
        }
        if cols.is_empty() {
            return Err(Error::ModuleError(format!(
                "iroh_remote: no columns for remote table {table:?}"
            )));
        }

        let schema = format!(
            "CREATE TABLE x({})",
            cols.iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let schema = CString::new(schema)
            .map_err(|_| Error::ModuleError("iroh_remote: NUL in a column name".to_string()))?;
        Ok((
            Cow::Owned(schema),
            IrohRemoteTab {
                base: unsafe { std::mem::zeroed() },
                ctx,
                peer,
                table,
                cols,
                timeout,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> rusqlite::Result<bool> {
        // Deparse simple usable predicates into a remote WHERE clause
        // (the idx_str); each consumed constraint becomes ?N in argv
        // order and is omitted from local re-checking (the peer
        // evaluates it). Everything else stays a local filter over the
        // fetched rows.
        let mut picked: Vec<(usize, String)> = Vec::new();
        for (i, constraint) in info.constraints().enumerate() {
            if !constraint.is_usable() {
                continue;
            }
            let op = match constraint.operator() {
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ => "=",
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GT => ">",
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GE => ">=",
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LT => "<",
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LE => "<=",
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_NE => "<>",
                IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LIKE => "LIKE",
                _ => continue,
            };
            let col = constraint.column();
            let name = if col < 0 {
                "rowid".to_string()
            } else {
                match self.cols.get(col as usize) {
                    Some(c) => quote_ident(c),
                    None => continue,
                }
            };
            let n = picked.len() + 1;
            picked.push((i, format!("{name} {op} ?{n}")));
        }
        for (n, (ci, _)) in picked.iter().enumerate() {
            let mut usage = info.constraint_usage(*ci);
            usage.set_argv_index((n + 1) as c_int);
            usage.set_omit(true);
        }
        if !picked.is_empty() {
            let clause = picked
                .iter()
                .map(|(_, c)| c.as_str())
                .collect::<Vec<_>>()
                .join(" AND ");
            info.set_idx_str(&clause);
        }
        info.set_idx_num(picked.len() as c_int);
        info.set_estimated_cost(if picked.is_empty() {
            1_000_000.0
        } else {
            10_000.0
        });
        Ok(true)
    }

    fn open(&'_ mut self) -> rusqlite::Result<IrohRemoteCursor> {
        Ok(IrohRemoteCursor {
            base: unsafe { std::mem::zeroed() },
            ctx: self.ctx.clone(),
            peer: self.peer.clone(),
            table: self.table.clone(),
            cols: self.cols.clone(),
            timeout: self.timeout,
            rows: Vec::new(),
            pos: 0,
        })
    }
}

impl CreateVTab<'_> for IrohRemoteTab {
    const KIND: VTabKind = VTabKind::Default;
}

impl UpdateVTab<'_> for IrohRemoteTab {
    fn delete(&mut self, arg: ValueRef<'_>) -> rusqlite::Result<()> {
        let rowid = match arg {
            ValueRef::Integer(i) => i,
            other => {
                return Err(Error::ModuleError(format!(
                    "iroh_remote: non-integer rowid {other:?} in DELETE"
                )));
            }
        };
        self.exec(
            format!("DELETE FROM {} WHERE rowid = ?1", quote_ident(&self.table)),
            vec![Value::Integer(rowid)],
        )?;
        Ok(())
    }

    fn insert(&mut self, args: &Inserts<'_>) -> rusqlite::Result<i64> {
        let values: Vec<Value> = args.iter().skip(2).map(value_from_ref).collect();
        if values.len() != self.cols.len() {
            return Err(Error::ModuleError(format!(
                "iroh_remote: INSERT carries {} values for {} columns",
                values.len(),
                self.cols.len()
            )));
        }
        // args[1] is an explicit rowid (INSERT INTO t(rowid, ...)), NULL
        // otherwise.
        let explicit_rowid = match args.iter().nth(1) {
            Some(ValueRef::Integer(i)) => Some(i),
            _ => None,
        };
        let mut names: Vec<String> = Vec::new();
        let mut params: Vec<Value> = Vec::new();
        if let Some(rowid) = explicit_rowid {
            names.push("rowid".to_string());
            params.push(Value::Integer(rowid));
        }
        names.extend(self.cols.iter().map(|c| quote_ident(c)));
        params.extend(values);
        let placeholders = (1..=params.len())
            .map(|n| format!("?{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let reply = self.exec(
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                quote_ident(&self.table),
                names.join(", "),
                placeholders
            ),
            params,
        )?;
        Ok(explicit_rowid.or(reply.done.last_insert_rowid).unwrap_or(0))
    }

    fn update(&mut self, args: &Updates<'_>) -> rusqlite::Result<()> {
        let mut iter = args.iter();
        let old_rowid = match iter.next() {
            Some(ValueRef::Integer(i)) => i,
            other => {
                return Err(Error::ModuleError(format!(
                    "iroh_remote: non-integer rowid {other:?} in UPDATE"
                )));
            }
        };
        let new_rowid = match iter.next() {
            Some(ValueRef::Integer(i)) => Some(i),
            _ => None,
        };
        let values: Vec<Value> = iter.map(value_from_ref).collect();
        if values.len() != self.cols.len() {
            return Err(Error::ModuleError(format!(
                "iroh_remote: UPDATE carries {} values for {} columns",
                values.len(),
                self.cols.len()
            )));
        }
        let mut sets: Vec<String> = self
            .cols
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{} = ?{}", quote_ident(c), i + 1))
            .collect();
        let mut params = values;
        if let Some(new_rowid) = new_rowid
            && new_rowid != old_rowid
        {
            params.push(Value::Integer(new_rowid));
            sets.push(format!("rowid = ?{}", params.len()));
        }
        params.push(Value::Integer(old_rowid));
        self.exec(
            format!(
                "UPDATE {} SET {} WHERE rowid = ?{}",
                quote_ident(&self.table),
                sets.join(", "),
                params.len()
            ),
            params,
        )?;
        Ok(())
    }
}

#[repr(C)]
pub struct IrohRemoteCursor {
    base: ffi::sqlite3_vtab_cursor,
    ctx: Arc<RemoteContext>,
    peer: String,
    table: String,
    cols: Vec<String>,
    timeout: Option<Duration>,
    /// (rowid, values in declared column order).
    rows: Vec<(i64, Vec<Value>)>,
    pos: usize,
}

unsafe impl VTabCursor for IrohRemoteCursor {
    fn filter(
        &mut self,
        _idx_num: c_int,
        idx_str: Option<&str>,
        args: &Filters<'_>,
    ) -> rusqlite::Result<()> {
        self.rows.clear();
        self.pos = 0;
        // The alias is load-bearing: with an INTEGER PRIMARY KEY the peer
        // would otherwise name the rowid column after the alias column.
        let mut sql = format!(
            "SELECT rowid AS _rsntr_rowid_, {} FROM {}",
            self.cols
                .iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", "),
            quote_ident(&self.table)
        );
        if let Some(clause) = idx_str
            && !clause.is_empty()
        {
            sql.push_str(" WHERE ");
            sql.push_str(clause);
        }
        let params: Vec<Value> = args.iter().map(value_from_ref).collect();
        let request = make_request(
            RequestKind::Query,
            &sql,
            params,
            self.timeout.unwrap_or(self.ctx.timeout),
        );
        let reply = self
            .ctx
            .run_blocking(&self.peer, request, self.timeout)
            .map_err(|e| module_err("iroh_remote", e))?;

        // Cells arrive by name and NULLs are omitted on the wire, so
        // rebuild each row against the declared column order.
        self.rows = reply
            .rows
            .iter()
            .map(|row| {
                let get = |name: &str| {
                    row.cells
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, v)| v.clone())
                };
                let rowid = match get("_rsntr_rowid_") {
                    Some(Value::Integer(i)) => Ok(i),
                    _ => Err(Error::ModuleError(format!(
                        "iroh_remote: remote table {:?} returned no integer rowid \
                         (WITHOUT ROWID tables are not supported)",
                        self.table
                    ))),
                }?;
                let values: Vec<Value> = self
                    .cols
                    .iter()
                    .map(|c| get(c).unwrap_or(Value::Null))
                    .collect();
                Ok((rowid, values))
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(())
    }

    fn next(&mut self) -> rusqlite::Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, ctx: &mut Context, i: c_int) -> rusqlite::Result<()> {
        match self.rows[self.pos].1.get(i as usize) {
            Some(v) => set_value(ctx, v),
            None => ctx.set_result(&rusqlite::types::Null),
        }
    }

    fn rowid(&self) -> rusqlite::Result<i64> {
        Ok(self.rows[self.pos].0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_sql_kinds() {
        assert_eq!(classify_sql("SELECT 1"), RequestKind::Query);
        assert_eq!(
            classify_sql("  with x as (select 1) select * from x"),
            RequestKind::Query
        );
        assert_eq!(
            classify_sql("INSERT INTO t VALUES (1)"),
            RequestKind::Execute
        );
        assert_eq!(classify_sql(""), RequestKind::Execute);
    }

    #[test]
    fn ident_quoting() {
        assert_eq!(quote_ident("plain"), "\"plain\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn module_arg_parsing() {
        let args: Vec<&[u8]> = vec![
            b"peer = abc".as_slice(),
            b"table='notes'".as_slice(),
            b"columns=\"a, b\"".as_slice(),
        ];
        let parsed = parse_module_args(&args).unwrap();
        assert_eq!(
            parsed,
            vec![
                ("peer".to_string(), "abc".to_string()),
                ("table".to_string(), "notes".to_string()),
                ("columns".to_string(), "a, b".to_string()),
            ]
        );
        assert!(parse_module_args(&[b"nokey".as_slice()]).is_err());
    }
}
