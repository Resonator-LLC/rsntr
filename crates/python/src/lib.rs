//! Python bindings for the resonator node (`import resonator`): a native
//! extension wrapping the `rsntr` CLI library behind a blocking,
//! notebook-friendly surface. One module-owned multi-thread tokio runtime
//! backs every async call; each method releases the GIL while it blocks,
//! so Jupyter's own asyncio loop is never touched.
//!
//! Surface:
//! - `Node(dir, offline=False)`: create (init) or reopen a node directory.
//! - `serve(web=False)` / `stop()`, `ticket()`, `add_peer()`, `knock()`,
//!   `endpoint_id`, `db_path`; context-manager support (`with` stops).
//! - remote: `query()` / `execute()` -> `QueryResult` (or Turtle text for
//!   CONSTRUCT), `help()` -> str.
//! - local (the owner channel, ungated): `local()`, `sparql()`,
//!   `load_turtle()`.
//! - chat: `chat_init()`, `chat_send()`, `chat_log()`.
//! - mods: `register_mod(name, handler)` serves a Python callable as a
//!   modulation, gated like wasm mods (register before `serve()`).
//! - media: `stream_media()` -> `MediaStream` (iterate `bytes`).
//! - errors: `resonator.Denied`, `resonator.QueryError` ("[code] reason").

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use pyo3::IntoPyObjectExt;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};

use resonator_authenticator::{Decision as AuthDecision, Footprint};
use resonator_node::mod_handler::{ModHandler, ModHandlerFuture};
use resonator_node::{audit, peer_known};
use resonator_protocol::{
    Denied as DeniedFrame, Done, EnvelopeObject, ErrorEnvelope, Request, RequestKind, ResultHeader,
    Row, Value, mod_matches,
};
use resonator_transport::parse_ticket;
use rsntr::channel::{self, OwnerChannel, Prefer};
use rsntr::chat;
use rsntr::client::{self, MediaChunk, QueryOutcome};
use rsntr::serve::{self, RunningNode};
use rsntr::store;

/// The one tokio runtime the module owns; built on first use.
static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn rt() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("building the resonator tokio runtime")
    })
}

create_exception!(resonator, Denied, PyException);
create_exception!(resonator, QueryError, PyException);

fn anyhow_err(e: anyhow::Error) -> PyErr {
    PyRuntimeError::new_err(format!("{e:#}"))
}

fn denied_err(reason: Option<String>) -> PyErr {
    Denied::new_err(reason.unwrap_or_else(|| "denied".to_string()))
}

fn failed_err(e: &ErrorEnvelope) -> PyErr {
    QueryError::new_err(format!(
        "[{}] {}",
        e.code,
        e.reason.as_deref().unwrap_or("(no reason given)")
    ))
}

// ---------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------

/// One envelope value -> a native Python object.
fn value_to_py(py: Python<'_>, v: &Value) -> PyResult<Py<PyAny>> {
    match v {
        Value::Null => Ok(py.None()),
        Value::Integer(i) => i.into_py_any(py),
        Value::Real(f) => f.into_py_any(py),
        Value::Text(s) => s.into_py_any(py),
        Value::Blob(b) => PyBytes::new(py, b).into_py_any(py),
        Value::BlobRef { hash, bytes } => {
            let d = PyDict::new(py);
            d.set_item("hash", hash)?;
            d.set_item("bytes", *bytes)?;
            d.into_py_any(py)
        }
    }
}

/// One Python parameter -> an envelope value. `bool` is checked before
/// `int` (a Python bool is an int subtype).
fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if obj.is_none() {
        return Ok(Value::Null);
    }
    if let Ok(b) = obj.downcast::<pyo3::types::PyBool>() {
        return Ok(Value::Integer(b.is_true() as i64));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(Value::Integer(i));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(Value::Real(f));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(Value::Text(s));
    }
    if let Ok(b) = obj.extract::<Vec<u8>>() {
        return Ok(Value::Blob(b));
    }
    Err(PyValueError::new_err(format!(
        "unsupported parameter type {}; use None/bool/int/float/str/bytes",
        obj.get_type().name()?
    )))
}

fn py_params(params: Option<Vec<Bound<'_, PyAny>>>) -> PyResult<Vec<Value>> {
    params.unwrap_or_default().iter().map(py_to_value).collect()
}

/// One result row -> a Python list aligned to `columns`, NULLs as `None`
/// (a decoded `Row` carries only non-NULL cells).
fn row_to_py(py: Python<'_>, columns: &[String], row: &Row) -> PyResult<Py<PyAny>> {
    let mut out: Vec<Py<PyAny>> = Vec::with_capacity(columns.len());
    for col in columns {
        let cell = row
            .cells
            .iter()
            .find(|(name, _)| name == col)
            .map(|(_, v)| v);
        match cell {
            Some(v) => out.push(value_to_py(py, v)?),
            None => out.push(py.None()),
        }
    }
    out.into_py_any(py)
}

// ---------------------------------------------------------------------
// Pure helpers (unit-tested without Python)
// ---------------------------------------------------------------------

/// The request kind implied by a statement text under a modulation,
/// mirroring the CLI: SPARQL forms for the sparql mod, SQL first-word
/// classification otherwise. A labeling heuristic; the serving side
/// derives the real kind from its own footprint.
fn classify_kind(modulation: &str, signal: &str) -> RequestKind {
    if mod_matches("sparql", modulation) {
        client::classify_sparql(signal)
    } else {
        client::classify_sql(signal)
    }
}

/// Parses Turtle and rebuilds it as one `INSERT DATA` update (N-Triples
/// body), so a Turtle load rides the owner channel's sparql modulation:
/// audited, idempotent by request id, and vibrating like any other
/// write. Returns `None` for a triple-less document.
fn turtle_to_insert_data(
    text: &str,
    base: Option<&str>,
) -> anyhow::Result<Option<(String, usize)>> {
    let mut parser = oxttl::TurtleParser::new();
    if let Some(base) = base {
        parser = parser
            .with_base_iri(base)
            .map_err(|e| anyhow::anyhow!("invalid base IRI: {e}"))?;
    }
    let mut triples: Vec<oxrdf::Triple> = Vec::new();
    for item in parser.for_slice(text.as_bytes()) {
        triples.push(item.map_err(|e| anyhow::anyhow!("turtle parse error: {e}"))?);
    }
    if triples.is_empty() {
        return Ok(None);
    }
    let mut update = String::from("INSERT DATA {\n");
    for t in &triples {
        update.push_str(&format!("{t} .\n"));
    }
    update.push('}');
    Ok(Some((update, triples.len())))
}

/// Ensures `dir` is an initialized node directory, initializing when the
/// key is missing.
fn ensure_node_dir(dir: &Path) -> anyhow::Result<()> {
    if store::node_id(dir).is_err() {
        store::init_dir(dir)?;
    }
    Ok(())
}

/// One chat history row; kept out of the pyclass so the Rust test suite
/// covers the read.
struct ChatRow {
    id: String,
    scope: String,
    sender: String,
    at: String,
    received_at: String,
    body: String,
    blob_hash: Option<String>,
    blob_name: Option<String>,
    outgoing: bool,
    status: Option<String>,
}

/// Reads chat history newest first; `scope=None` reads every scope.
fn read_chat_log(dir: &Path, scope: Option<&str>, limit: i64) -> anyhow::Result<Vec<ChatRow>> {
    let conn = store::open_db(dir)?;
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'chat_messages'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        anyhow::bail!("chat is not scaffolded here; call chat_init() first");
    }
    // A given scope may be a petname, a room name/IRI, or a 64-hex id.
    let resolved = match scope {
        None => None,
        Some(s) => Some(chat::resolve_target(&conn, s)?.scope()),
    };
    let mut stmt = conn.prepare(
        "SELECT m.id, m.scope, m.sender, m.at, m.received_at, m.body, \
                m.blob_hash, m.blob_name, m.outgoing, o.status \
         FROM chat_messages m LEFT JOIN _outbox o ON o.request_id = m.id \
         WHERE (?1 IS NULL OR m.scope = ?1) \
         ORDER BY m.received_at DESC, m.id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map((resolved, limit), |r| {
            Ok(ChatRow {
                id: r.get(0)?,
                scope: r.get(1)?,
                sender: r.get(2)?,
                at: r.get(3)?,
                received_at: r.get(4)?,
                body: r.get(5)?,
                blob_hash: r.get(6)?,
                blob_name: r.get(7)?,
                outgoing: r.get::<_, i64>(8)? != 0,
                status: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------
// QueryResult
// ---------------------------------------------------------------------

/// The result of a successful query/execute: a table of rows plus the
/// `rsntr:Done` trailer facts.
#[pyclass]
struct QueryResult {
    #[pyo3(get)]
    columns: Vec<String>,
    /// A Python list of row lists, built once at construction.
    rows_obj: Py<PyAny>,
    #[pyo3(get)]
    row_count: Option<i64>,
    #[pyo3(get)]
    affected_rows: Option<i64>,
    #[pyo3(get)]
    last_insert_rowid: Option<i64>,
    #[pyo3(get)]
    truncated: bool,
    nrows: usize,
}

impl QueryResult {
    fn build(py: Python<'_>, columns: Vec<String>, rows: Vec<Row>, done: Done) -> PyResult<Self> {
        let mut py_rows: Vec<Py<PyAny>> = Vec::with_capacity(rows.len());
        for row in &rows {
            py_rows.push(row_to_py(py, &columns, row)?);
        }
        let nrows = py_rows.len();
        Ok(QueryResult {
            columns,
            rows_obj: py_rows.into_py_any(py)?,
            row_count: done.row_count,
            affected_rows: done.affected_rows,
            last_insert_rowid: done.last_insert_rowid,
            truncated: done.truncated,
            nrows,
        })
    }
}

#[pymethods]
impl QueryResult {
    /// The rows as a list of lists (NULLs as `None`).
    #[getter]
    fn rows(&self, py: Python<'_>) -> Py<PyAny> {
        self.rows_obj.clone_ref(py)
    }

    /// Rows as a list of `{column: value}` dicts.
    fn to_dicts(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rows = self.rows_obj.bind(py);
        let out = PyList::empty(py);
        for row in rows.try_iter()? {
            let row = row?;
            let d = PyDict::new(py);
            for (col, val) in self.columns.iter().zip(row.try_iter()?) {
                d.set_item(col, val?)?;
            }
            out.append(d)?;
        }
        out.into_py_any(py)
    }

    /// A pandas DataFrame (requires pandas: `pip install pandas`).
    fn to_pandas(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let pandas = py.import("pandas").map_err(|_| {
            PyRuntimeError::new_err("pandas is not installed; `pip install pandas`")
        })?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("columns", self.columns.clone())?;
        let df = pandas
            .getattr("DataFrame")?
            .call((self.rows_obj.bind(py),), Some(&kwargs))?;
        df.into_py_any(py)
    }

    fn __len__(&self) -> usize {
        self.nrows
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(self.rows_obj.bind(py).try_iter()?.into_any().unbind())
    }

    fn __repr__(&self) -> String {
        format!(
            "QueryResult(columns={:?}, rows={}{})",
            self.columns,
            self.nrows,
            if self.truncated { ", truncated" } else { "" }
        )
    }
}

/// Terminal outcome -> Python: rows become a `QueryResult`, a graph
/// becomes Turtle text, help becomes text; refusals and errors raise.
fn outcome_to_py(py: Python<'_>, outcome: QueryOutcome) -> PyResult<Py<PyAny>> {
    match outcome {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => Py::new(py, QueryResult::build(py, columns, rows, done)?)?.into_py_any(py),
        QueryOutcome::Graph { triples, done: _ } => {
            let mut text = String::new();
            for t in &triples {
                text.push_str(&format!("{t} .\n"));
            }
            text.into_py_any(py)
        }
        QueryOutcome::Help { text, .. } => text.into_py_any(py),
        QueryOutcome::Denied(d) => Err(denied_err(d.reason)),
        QueryOutcome::Failed(e) => Err(failed_err(&e)),
    }
}

// ---------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------

/// A resonator node: one directory holding the sqlite database and the
/// ed25519 key. Constructing initializes the directory when needed.
#[pyclass]
struct Node {
    dir: PathBuf,
    offline: bool,
    running: Mutex<Option<RunningNode>>,
    web: Mutex<Option<resonator_web::WebServer>>,
    /// Python-defined mods by modulation name; shared with the
    /// [`PyModsHandler`] installed at serve time.
    pymods: Arc<Mutex<HashMap<String, Py<PyAny>>>>,
}

#[pymethods]
impl Node {
    #[new]
    #[pyo3(signature = (dir, offline=false))]
    fn new(dir: PathBuf, offline: bool) -> PyResult<Self> {
        ensure_node_dir(&dir).map_err(anyhow_err)?;
        Ok(Node {
            dir,
            offline,
            running: Mutex::new(None),
            web: Mutex::new(None),
            pymods: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// This node's 64-hex endpoint id.
    #[getter]
    fn endpoint_id(&self) -> PyResult<String> {
        Ok(store::node_id(&self.dir).map_err(anyhow_err)?.to_string())
    }

    /// Path to the node's sqlite database (readable with sqlite3/pandas
    /// even while serving; the database is WAL).
    #[getter]
    fn db_path(&self) -> String {
        store::db_path(&self.dir).to_string_lossy().into_owned()
    }

    /// The node directory.
    #[getter]
    fn dir(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }

    /// Whether this node is currently serving.
    #[getter]
    fn is_serving(&self) -> bool {
        self.running.lock().expect("running lock").is_some()
    }

    /// Starts serving in the background (returns the direct socket
    /// addresses). `web=True` also starts the web interface; its entry
    /// URL is `web_url` afterwards.
    #[pyo3(signature = (web=false, web_addr=None))]
    fn serve(&self, py: Python<'_>, web: bool, web_addr: Option<String>) -> PyResult<Vec<String>> {
        if self.is_serving() {
            return Err(PyRuntimeError::new_err("node is already serving"));
        }
        let dir = self.dir.clone();
        let offline = self.offline;
        let running = py
            .allow_threads(|| rt().block_on(serve::start_node(&dir, offline)))
            .map_err(anyhow_err)?;
        if !self.pymods.lock().expect("pymods lock").is_empty() {
            running.node().set_mod_handler(Box::new(PyModsHandler {
                mods: self.pymods.clone(),
                node: running.node().clone(),
            }));
        }
        let web_server = if web {
            let addr: SocketAddr = web_addr
                .as_deref()
                .unwrap_or(serve::DEFAULT_WEB_ADDR)
                .parse()
                .map_err(|e| PyValueError::new_err(format!("bad web_addr: {e}")))?;
            match py.allow_threads(|| rt().block_on(serve::start_web(&running, &dir, addr, false)))
            {
                Ok(ws) => Some(ws),
                Err(e) => {
                    py.allow_threads(|| rt().block_on(running.shutdown()));
                    return Err(anyhow_err(e));
                }
            }
        } else {
            None
        };
        let addrs = running
            .direct_addrs()
            .iter()
            .map(|a| a.to_string())
            .collect();
        *self.running.lock().expect("running lock") = Some(running);
        *self.web.lock().expect("web lock") = web_server;
        Ok(addrs)
    }

    /// The web interface entry URL (token in the fragment), when serving
    /// with `web=True`.
    #[getter]
    fn web_url(&self) -> Option<String> {
        self.web.lock().expect("web lock").as_ref().map(|w| w.url())
    }

    /// Stops serving (a no-op when not serving).
    fn stop(&self, py: Python<'_>) -> PyResult<()> {
        let web = self.web.lock().expect("web lock").take();
        if let Some(w) = web {
            py.allow_threads(|| rt().block_on(w.shutdown()));
        }
        let running = self.running.lock().expect("running lock").take();
        if let Some(r) = running {
            py.allow_threads(|| rt().block_on(r.shutdown()));
        }
        Ok(())
    }

    /// A shareable dialing ticket for the live serving endpoint, waiting
    /// up to `wait_secs` for a direct address so the ticket is dialable
    /// immediately. Requires `serve()` first.
    #[pyo3(signature = (wait_secs=3.0))]
    fn ticket(&self, py: Python<'_>, wait_secs: f64) -> PyResult<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(wait_secs.max(0.0));
        loop {
            let t = {
                let guard = self.running.lock().expect("running lock");
                match guard.as_ref() {
                    Some(r) => r.ticket(),
                    None => {
                        return Err(PyRuntimeError::new_err(
                            "node is not serving; call serve() first \
                             (a ticket names the live endpoint)",
                        ));
                    }
                }
            };
            let dialable = matches!(parse_ticket(&t), Ok((_, addrs)) if !addrs.is_empty());
            if dialable || std::time::Instant::now() >= deadline {
                return Ok(t);
            }
            py.allow_threads(|| std::thread::sleep(Duration::from_millis(100)));
        }
    }

    /// Registers a peer under `name`. The target may be a dialing ticket
    /// or a 64-hex endpoint id; `addrs` adds "ip:port" dial hints.
    /// Returns the peer's endpoint id.
    #[pyo3(signature = (name, ticket_or_id, addrs=None))]
    fn add_peer(
        &self,
        py: Python<'_>,
        name: &str,
        ticket_or_id: &str,
        addrs: Option<Vec<String>>,
    ) -> PyResult<String> {
        let socket_addrs = addrs
            .unwrap_or_default()
            .iter()
            .map(|s| {
                s.parse::<SocketAddr>()
                    .map_err(|e| PyValueError::new_err(format!("bad address {s:?}: {e}")))
            })
            .collect::<PyResult<Vec<_>>>()?;
        let dir = self.dir.clone();
        let name = name.to_string();
        let target = ticket_or_id.to_string();
        let (id, _stored) = py
            .allow_threads(|| store::peer_add(&dir, &name, &target, &socket_addrs))
            .map_err(anyhow_err)?;
        Ok(id.to_string())
    }

    /// Registers a Python-defined mod: `handler` serves every request
    /// whose modulation matches `name`. Called as `handler(req)` with
    /// `req = {"peer": 64-hex caller, "kind": "query"|"execute",
    /// "signal": str, "params": list}`; return `None` (bare Done),
    /// `(columns, rows)`, `{"columns": [...], "rows": [...]}`, or a
    /// list of dicts; raise `resonator.Denied` to refuse. Callers are
    /// gated exactly like wasm mods: admitted peers only, then `_policy`
    /// decides action `mod:<name>` (seed an allow row or every call is
    /// denied). Unlike sandboxed wasm mods the handler runs IN PROCESS
    /// with owner powers - the gate covers who may call, not what the
    /// handler does. Register before `serve()`: the hello advertises
    /// modulations at bind, so later registrations are unreachable.
    fn register_mod(&self, name: &str, handler: Py<PyAny>) -> PyResult<()> {
        if self.is_serving() {
            return Err(PyRuntimeError::new_err(
                "register mods before serve(): the hello advertises \
                 modulations when the endpoint binds",
            ));
        }
        const BUILTINS: &[&str] = &[
            "help", "projection", "media", "audio-duplex", "chat", "sparql", "sql-sqlite",
        ];
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(PyValueError::new_err(
                "mod name must be non-empty ascii alphanumeric/dash/underscore",
            ));
        }
        if BUILTINS.contains(&name) {
            return Err(PyValueError::new_err(format!(
                "{name:?} is a builtin modulation"
            )));
        }
        // Advertise in the hello (`_rsntr.modulations`), idempotently.
        let conn = store::open_db(&self.dir).map_err(anyhow_err)?;
        let raw = resonator_node::get_rsntr(&conn, "modulations").unwrap_or_else(|| "[]".into());
        let mut mods: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        if !mods.iter().any(|m| m == name) {
            mods.push(name.to_string());
            let json = serde_json::to_string(&mods)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            resonator_node::set_rsntr(&conn, "modulations", &json)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        }
        self.pymods
            .lock()
            .expect("pymods lock")
            .insert(name.to_string(), handler);
        Ok(())
    }

    /// Knocks on `peer` (petname, ticket, or 64-hex id) with a reason
    /// message: the one frame an unadmitted key may send
    /// (connection-protocol.md section 4). Returns a dict
    /// `{id, decision, decided_by, reason}`; `decision` is `"allow"`
    /// (admitted, talk normally now), `"pending"` (parked for the
    /// owner), `"deny"`, or `None` when the peer answered nothing at
    /// all (knocks are rate-limited and dropped in silence).
    #[pyo3(signature = (peer, message))]
    fn knock(&self, py: Python<'_>, peer: &str, message: &str) -> PyResult<Py<PyAny>> {
        let dir = self.dir.clone();
        let offline = self.offline;
        let peer_s = peer.to_string();
        let message_s = message.to_string();
        let report = py
            .allow_threads(|| rt().block_on(client::knock(&dir, &peer_s, &message_s, offline)))
            .map_err(anyhow_err)?;
        let d = PyDict::new(py);
        d.set_item("id", &report.id)?;
        match &report.decision {
            Some(dec) => {
                d.set_item("decision", &dec.decision)?;
                d.set_item("decided_by", &dec.decided_by)?;
                d.set_item("reason", dec.reason.as_deref())?;
            }
            None => {
                d.set_item("decision", py.None())?;
                d.set_item("decided_by", py.None())?;
                d.set_item("reason", py.None())?;
            }
        }
        d.into_py_any(py)
    }

    /// Sends `signal` to `peer` (petname or 64-hex id) in `mod`
    /// (default sql-sqlite), classified as Query or Execute from the
    /// text. Rows return a `QueryResult`; a sparql CONSTRUCT returns
    /// Turtle text. Raises `Denied` / `QueryError`.
    #[pyo3(signature = (peer, signal, r#mod="sql-sqlite", params=None, timeout_ms=None))]
    fn query(
        &self,
        py: Python<'_>,
        peer: &str,
        signal: &str,
        r#mod: &str,
        params: Option<Vec<Bound<'_, PyAny>>>,
        timeout_ms: Option<i64>,
    ) -> PyResult<Py<PyAny>> {
        let kind = classify_kind(r#mod, signal);
        self.remote_statement(py, peer, kind, r#mod, signal, params, timeout_ms)
    }

    /// [`query`](Node::query) forced to a write (`rsntr:Execute`).
    #[pyo3(signature = (peer, signal, r#mod="sql-sqlite", params=None, timeout_ms=None))]
    fn execute(
        &self,
        py: Python<'_>,
        peer: &str,
        signal: &str,
        r#mod: &str,
        params: Option<Vec<Bound<'_, PyAny>>>,
        timeout_ms: Option<i64>,
    ) -> PyResult<Py<PyAny>> {
        self.remote_statement(
            py,
            peer,
            RequestKind::Execute,
            r#mod,
            signal,
            params,
            timeout_ms,
        )
    }

    /// Runs `signal` on this node over the owner channel: no peer gate,
    /// no authenticator chain, footprint-collected and audited
    /// (`decided_by 'owner'`), DDL permitted. Same return shapes as
    /// `query`.
    #[pyo3(signature = (signal, r#mod="sql-sqlite", params=None))]
    fn local(
        &self,
        py: Python<'_>,
        signal: &str,
        r#mod: &str,
        params: Option<Vec<Bound<'_, PyAny>>>,
    ) -> PyResult<Py<PyAny>> {
        let kind = classify_kind(r#mod, signal);
        let values = py_params(params)?;
        let dir = self.dir.clone();
        let modulation = r#mod.to_string();
        let signal = signal.to_string();
        let outcome = py
            .allow_threads(|| {
                rt().block_on(async move {
                    let ch = OwnerChannel::open(&dir, Prefer::Auto).await?;
                    channel::run(&ch, kind, &modulation, &signal, values).await
                })
            })
            .map_err(anyhow_err)?;
        outcome_to_py(py, outcome)
    }

    /// Runs a SPARQL text against this node's own store (owner channel).
    /// SELECT/ASK return a `QueryResult`, CONSTRUCT returns Turtle text,
    /// updates return a `QueryResult` with `affected_rows`.
    #[pyo3(signature = (query))]
    fn sparql(&self, py: Python<'_>, query: &str) -> PyResult<Py<PyAny>> {
        self.local(py, query, "sparql", None)
    }

    /// Loads Turtle into this node's RDF store (owner channel, as one
    /// idempotent INSERT DATA). Returns the number of triples asserted.
    #[pyo3(signature = (text, base=None))]
    fn load_turtle(&self, py: Python<'_>, text: &str, base: Option<String>) -> PyResult<usize> {
        let Some((update, count)) =
            turtle_to_insert_data(text, base.as_deref()).map_err(anyhow_err)?
        else {
            return Ok(0);
        };
        let dir = self.dir.clone();
        let outcome = py
            .allow_threads(|| {
                rt().block_on(async move {
                    let ch = OwnerChannel::open(&dir, Prefer::Auto).await?;
                    channel::run(&ch, RequestKind::Execute, "sparql", &update, vec![]).await
                })
            })
            .map_err(anyhow_err)?;
        match outcome {
            QueryOutcome::Rows { .. } => Ok(count),
            QueryOutcome::Denied(d) => Err(denied_err(d.reason)),
            QueryOutcome::Failed(e) => Err(failed_err(&e)),
            other => Err(PyRuntimeError::new_err(format!(
                "unexpected load response: {other:?}"
            ))),
        }
    }

    /// Asks `peer` for usage guidance (the `help` modulation).
    #[pyo3(signature = (peer, topic=None))]
    fn help(&self, py: Python<'_>, peer: &str, topic: Option<String>) -> PyResult<String> {
        let dir = self.dir.clone();
        let offline = self.offline;
        let peer_s = peer.to_string();
        let report = py
            .allow_threads(|| rt().block_on(client::run_help(&dir, &peer_s, topic, offline)))
            .map_err(anyhow_err)?;
        match report.outcome {
            QueryOutcome::Help { text, .. } => Ok(text),
            QueryOutcome::Denied(d) => Err(denied_err(d.reason)),
            QueryOutcome::Failed(e) => Err(failed_err(&e)),
            other => Err(PyRuntimeError::new_err(format!(
                "unexpected help response: {other:?}"
            ))),
        }
    }

    // --- chat ---

    /// Scaffolds chat on this node (tables, projection points, policy);
    /// idempotent.
    fn chat_init(&self, py: Python<'_>) -> PyResult<()> {
        let dir = self.dir.clone();
        py.allow_threads(|| chat::chat_init(&dir))
            .map_err(anyhow_err)?;
        Ok(())
    }

    /// Sends a chat message to `target` (peer petname, 64-hex id, room
    /// name, or room IRI), optionally attaching `file`. The send is an
    /// `_outbox` enqueue; delivery happens while this node serves.
    /// Returns `{message_id, scope, queued_to, blob}`.
    #[pyo3(signature = (target, text, file=None))]
    fn chat_send(
        &self,
        py: Python<'_>,
        target: &str,
        text: &str,
        file: Option<PathBuf>,
    ) -> PyResult<Py<PyAny>> {
        let dir = self.dir.clone();
        let target_s = target.to_string();
        let text_s = text.to_string();
        let report = py
            .allow_threads(|| {
                rt().block_on(chat::chat_send(&dir, &target_s, &text_s, file.as_deref()))
            })
            .map_err(anyhow_err)?;
        let d = PyDict::new(py);
        d.set_item("message_id", &report.message_id)?;
        d.set_item("scope", &report.scope)?;
        d.set_item("queued_to", &report.queued_to)?;
        match &report.blob {
            Some((hash, bytes)) => {
                let b = PyDict::new(py);
                b.set_item("hash", hash)?;
                b.set_item("bytes", *bytes)?;
                d.set_item("blob", b)?;
            }
            None => d.set_item("blob", py.None())?,
        }
        d.into_py_any(py)
    }

    /// Reads chat history, newest first, as a list of dicts. `scope`
    /// filters to one conversation (peer or room); `None` reads all.
    /// Outgoing messages carry their `_outbox` delivery `status`.
    #[pyo3(signature = (scope=None, limit=200))]
    fn chat_log(&self, py: Python<'_>, scope: Option<String>, limit: i64) -> PyResult<Py<PyAny>> {
        let dir = self.dir.clone();
        let rows = py
            .allow_threads(|| read_chat_log(&dir, scope.as_deref(), limit))
            .map_err(anyhow_err)?;
        let out = PyList::empty(py);
        for row in rows {
            let d = PyDict::new(py);
            d.set_item("id", row.id)?;
            d.set_item("scope", row.scope)?;
            d.set_item("sender", row.sender)?;
            d.set_item("at", row.at)?;
            d.set_item("received_at", row.received_at)?;
            d.set_item("body", row.body)?;
            d.set_item("blob_hash", row.blob_hash)?;
            d.set_item("blob_name", row.blob_name)?;
            d.set_item("outgoing", row.outgoing)?;
            d.set_item("status", row.status)?;
            out.append(d)?;
        }
        out.into_py_any(py)
    }

    /// Nudges the outbox worker to scan `_outbox` now (deliveries are
    /// otherwise picked up on the poll cadence). No-op when not serving.
    fn wake_outbox(&self) {
        if let Some(r) = self.running.lock().expect("running lock").as_ref() {
            r.wake_outbox();
        }
    }

    /// Opens the media `source` on `peer` (petname or 64-hex id) and
    /// returns a `MediaStream`: `.content_type` is set from the peer's
    /// header, then iterate the stream to pull the raw feed as `bytes`.
    /// Raises `Denied` if the peer's policy refuses, `QueryError` on a
    /// protocol/engine error or if the peer is unreachable.
    #[pyo3(signature = (peer, source))]
    fn stream_media(&self, py: Python<'_>, peer: &str, source: &str) -> PyResult<MediaStream> {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<MediaChunk>(16);
        let dir = self.dir.clone();
        let offline = self.offline;
        let peer_s = peer.to_string();
        let source_s = source.to_string();
        // The feed runs as its own task; the header (or a refusal) arrives
        // first, then Data chunks, all drained by MediaStream.
        let handle = rt().spawn(async move {
            client::run_media_channel(&dir, &peer_s, &source_s, offline, tx).await
        });
        let first = py.allow_threads(|| rt().block_on(rx.recv()));
        match first {
            Some(MediaChunk::Header { content_type }) => Ok(MediaStream {
                content_type,
                rx: Mutex::new(Some(rx)),
            }),
            Some(MediaChunk::Denied(d)) => Err(denied_err(d.reason)),
            Some(MediaChunk::Failed(e)) => Err(failed_err(&e)),
            Some(MediaChunk::Data(_)) => {
                Err(QueryError::new_err("media feed sent data before a header"))
            }
            // No item: the feed task failed before sending a header (e.g. the
            // peer was unreachable). Surface that task's error if there is one.
            None => {
                let joined = py.allow_threads(|| rt().block_on(handle));
                match joined {
                    Ok(Err(e)) => Err(anyhow_err(e)),
                    _ => Err(QueryError::new_err("media stream ended before the header")),
                }
            }
        }
    }

    // --- context manager ---

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc=None, _tb=None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        _exc_type: Option<Py<PyAny>>,
        _exc: Option<Py<PyAny>>,
        _tb: Option<Py<PyAny>>,
    ) -> PyResult<bool> {
        self.stop(py)?;
        Ok(false)
    }

    fn __repr__(&self) -> String {
        format!(
            "Node(dir={:?}, offline={}, serving={})",
            self.dir.display(),
            self.offline,
            self.is_serving()
        )
    }
}

impl Node {
    /// The shared remote round trip behind `query` and `execute`.
    #[allow(clippy::too_many_arguments)]
    fn remote_statement(
        &self,
        py: Python<'_>,
        peer: &str,
        kind: RequestKind,
        modulation: &str,
        signal: &str,
        params: Option<Vec<Bound<'_, PyAny>>>,
        timeout_ms: Option<i64>,
    ) -> PyResult<Py<PyAny>> {
        let values = py_params(params)?;
        let dir = self.dir.clone();
        let offline = self.offline;
        let peer_s = peer.to_string();
        let mod_s = modulation.to_string();
        let signal_s = signal.to_string();
        let report = py
            .allow_threads(|| {
                rt().block_on(client::run_statement(
                    &dir, &peer_s, kind, &mod_s, &signal_s, values, offline, timeout_ms,
                ))
            })
            .map_err(anyhow_err)?;
        outcome_to_py(py, report.outcome)
    }
}

// ---------------------------------------------------------------------
// MediaStream: a live media feed pulled from a peer, iterated as bytes
// ---------------------------------------------------------------------

/// An open media feed. `content_type` is known before the first chunk;
/// iterating yields the raw feed as `bytes` (chunked in arrival order)
/// until the source ends. A slow reader applies backpressure to the peer;
/// closing (or dropping) the stream ends the feed. Feed the bytes to a
/// decoder that accepts a byte stream (PyAV, an ffmpeg subprocess, ...).
#[pyclass]
struct MediaStream {
    #[pyo3(get)]
    content_type: String,
    rx: Mutex<Option<tokio::sync::mpsc::Receiver<MediaChunk>>>,
}

#[pymethods]
impl MediaStream {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Py<PyBytes>>> {
        let mut guard = self.rx.lock().expect("media rx lock");
        let recv = {
            let Some(rx) = guard.as_mut() else {
                return Ok(None);
            };
            py.allow_threads(|| rt().block_on(rx.recv()))
        };
        match recv {
            Some(MediaChunk::Data(bytes)) => Ok(Some(PyBytes::new(py, &bytes).unbind())),
            Some(MediaChunk::Denied(d)) => {
                *guard = None;
                Err(denied_err(d.reason))
            }
            Some(MediaChunk::Failed(e)) => {
                *guard = None;
                Err(failed_err(&e))
            }
            // A second header mid-feed is a protocol confusion; end the feed.
            Some(MediaChunk::Header { .. }) | None => {
                *guard = None;
                Ok(None)
            }
        }
    }

    /// Stop pulling and release the feed.
    fn close(&self) {
        *self.rx.lock().expect("media rx lock") = None;
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc=None, _tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<Py<PyAny>>,
        _exc: Option<Py<PyAny>>,
        _tb: Option<Py<PyAny>>,
    ) -> bool {
        self.close();
        false
    }

    fn __repr__(&self) -> String {
        format!("MediaStream(content_type={:?})", self.content_type)
    }
}

// ---------------------------------------------------------------------
// Python-defined mods: an in-process ModHandler dispatching to callables
// ---------------------------------------------------------------------

/// The converted outcome of one Python mod callback.
enum PyModOutcome {
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    Done,
    Denied(String),
    Error(String),
}

/// The gate verdict for one mod request (mirrors the wasm host's).
enum ModGate {
    Unknown,
    Denied(String),
    Allowed(i64),
}

/// Serves registered Python callables as modulations. Installed by
/// `Node.serve()` when any mod is registered; the map is shared with the
/// pyclass so `mods()` always reflects the registrations.
struct PyModsHandler {
    mods: Arc<Mutex<HashMap<String, Py<PyAny>>>>,
    node: Arc<resonator_node::Node>,
}

impl ModHandler for PyModsHandler {
    fn mods(&self) -> Vec<String> {
        self.mods.lock().expect("pymods lock").keys().cloned().collect()
    }

    fn handle(
        &self,
        peer: String,
        request: Request,
        frames: tokio::sync::mpsc::Sender<EnvelopeObject>,
    ) -> ModHandlerFuture<'_> {
        Box::pin(async move {
            let id = request.id_string();
            let callback = {
                let guard = self.mods.lock().expect("pymods lock");
                guard
                    .iter()
                    .find(|(name, _)| mod_matches(name, &request.modulation))
                    .map(|(_, cb)| Python::with_gil(|py| cb.clone_ref(py)))
            };
            let Some(callback) = callback else {
                let _ = frames
                    .send(EnvelopeObject::Error(ErrorEnvelope {
                        id: Some(id),
                        code: "mod-unsupported".to_string(),
                        reason: Some(format!("no mod serves {:?}", request.modulation)),
                    }))
                    .await;
                return;
            };

            // The same gate the wasm mods host applies (the pipeline
            // gates nothing on the mod path): admitted peers only, then
            // the chain decides action `mod:<name>` with an empty
            // footprint, audited either way.
            let chain = self.node.chain().clone();
            let (peer_g, id_g, mod_g, signal_g) = (
                peer.clone(),
                id.clone(),
                request.modulation.clone(),
                request.signal.clone(),
            );
            let gate = self
                .node
                .db()
                .call(move |conn| {
                    if !peer_known(conn, &peer_g) {
                        audit::audit_direct(
                            conn, &peer_g, &id_g, &signal_g, "deny", "peer-gate", "unknown peer",
                        );
                        return ModGate::Unknown;
                    }
                    let action = format!("mod:{mod_g}");
                    let start = std::time::Instant::now();
                    let decided =
                        chain.decide(conn, &peer_g, &action, &Footprint::default(), &signal_g);
                    let ms = start.elapsed().as_millis() as u64;
                    match decided.decision {
                        AuthDecision::Allow | AuthDecision::AllowNarrowed { .. } => {
                            let audit_id = audit::audit_full(
                                conn,
                                &peer_g,
                                &id_g,
                                &signal_g,
                                "{}",
                                "allow",
                                &decided.decided_by,
                                Some("python mod"),
                                ms,
                            );
                            ModGate::Allowed(audit_id)
                        }
                        AuthDecision::Deny { reason } => {
                            audit::audit_full(
                                conn,
                                &peer_g,
                                &id_g,
                                &signal_g,
                                "{}",
                                "deny",
                                &decided.decided_by,
                                Some(&reason),
                                ms,
                            );
                            ModGate::Denied(reason)
                        }
                        AuthDecision::Escalate => {
                            audit::audit_full(
                                conn,
                                &peer_g,
                                &id_g,
                                &signal_g,
                                "{}",
                                "deny",
                                &decided.decided_by,
                                Some("escalation is not supported for mods"),
                                ms,
                            );
                            ModGate::Denied("no decider in the chain allowed the request".into())
                        }
                    }
                })
                .await;
            let audit_id = match gate {
                Err(e) => {
                    let _ = frames
                        .send(EnvelopeObject::Error(ErrorEnvelope {
                            id: Some(id),
                            code: "engine-error".to_string(),
                            reason: Some(e.to_string()),
                        }))
                        .await;
                    return;
                }
                Ok(ModGate::Unknown) => {
                    let _ = frames
                        .send(EnvelopeObject::Denied(DeniedFrame {
                            id: Some(id),
                            reason: Some("unknown peer: only rsntr:Knock is accepted".into()),
                        }))
                        .await;
                    return;
                }
                Ok(ModGate::Denied(reason)) => {
                    let _ = frames
                        .send(EnvelopeObject::Denied(DeniedFrame {
                            id: Some(id),
                            reason: Some(reason),
                        }))
                        .await;
                    return;
                }
                Ok(ModGate::Allowed(a)) => a,
            };

            let kind = match request.kind {
                RequestKind::Query => "query",
                RequestKind::Execute => "execute",
            };
            let signal = request.signal.clone();
            let params = request.params.clone();
            let peer_cb = peer.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| call_py_mod(py, &callback, &peer_cb, kind, &signal, &params))
            })
            .await
            .unwrap_or_else(|e| PyModOutcome::Error(format!("python mod panicked: {e}")));

            let rows_out = send_mod_outcome(&frames, &id, outcome).await;
            let _ = self
                .node
                .db()
                .call(move |conn| audit::audit_outcome(conn, audit_id, rows_out, 0))
                .await;
        })
    }
}

/// Builds the request dict, calls the handler under the GIL, and
/// converts the return value (or exception) into a [`PyModOutcome`].
fn call_py_mod(
    py: Python<'_>,
    callback: &Py<PyAny>,
    peer: &str,
    kind: &str,
    signal: &str,
    params: &[Value],
) -> PyModOutcome {
    let called = (|| -> PyResult<Py<PyAny>> {
        let req = PyDict::new(py);
        req.set_item("peer", peer)?;
        req.set_item("kind", kind)?;
        req.set_item("signal", signal)?;
        let ps = PyList::empty(py);
        for v in params {
            ps.append(value_to_py(py, v)?)?;
        }
        req.set_item("params", ps)?;
        Ok(callback.bind(py).call1((req,))?.unbind())
    })();
    match called {
        Ok(ret) => convert_mod_return(ret.bind(py)),
        Err(err) if err.is_instance_of::<Denied>(py) => {
            PyModOutcome::Denied(err.value(py).to_string())
        }
        Err(err) => PyModOutcome::Error(err.to_string()),
    }
}

fn convert_mod_return(ret: &Bound<'_, PyAny>) -> PyModOutcome {
    let converted = (|| -> PyResult<PyModOutcome> {
        if ret.is_none() {
            return Ok(PyModOutcome::Done);
        }
        if let Ok(d) = ret.downcast::<PyDict>() {
            let (Some(cols), Some(rows)) = (d.get_item("columns")?, d.get_item("rows")?) else {
                return Err(PyValueError::new_err(
                    "mod return dict needs 'columns' and 'rows'",
                ));
            };
            return rows_outcome(cols.extract()?, &rows);
        }
        if let Ok(t) = ret.downcast::<PyTuple>() {
            if t.len() == 2 {
                return rows_outcome(t.get_item(0)?.extract()?, &t.get_item(1)?);
            }
            return Err(PyValueError::new_err("mod return tuple must be (columns, rows)"));
        }
        if let Ok(l) = ret.downcast::<PyList>() {
            if l.is_empty() {
                return Ok(PyModOutcome::Done);
            }
            let first = l.get_item(0)?;
            let fd = first.downcast::<PyDict>().map_err(|_| {
                PyValueError::new_err("a mod return list must contain dicts")
            })?;
            let mut columns: Vec<String> = Vec::new();
            for k in fd.keys() {
                columns.push(k.extract()?);
            }
            let mut rows = Vec::with_capacity(l.len());
            for item in l.iter() {
                let d = item.downcast::<PyDict>().map_err(|_| {
                    PyValueError::new_err("a mod return list must contain dicts")
                })?;
                let mut row = Vec::with_capacity(columns.len());
                for c in &columns {
                    row.push(match d.get_item(c)? {
                        Some(v) => py_to_value(&v)?,
                        None => Value::Null,
                    });
                }
                rows.push(row);
            }
            return Ok(PyModOutcome::Rows { columns, rows });
        }
        Err(PyValueError::new_err(
            "mod must return None, (columns, rows), {'columns','rows'}, or a list of dicts",
        ))
    })();
    converted.unwrap_or_else(|e| PyModOutcome::Error(e.to_string()))
}

fn rows_outcome(columns: Vec<String>, rows_obj: &Bound<'_, PyAny>) -> PyResult<PyModOutcome> {
    let mut rows = Vec::new();
    for row in rows_obj.try_iter()? {
        let mut cells = Vec::with_capacity(columns.len());
        for cell in row?.try_iter()? {
            cells.push(py_to_value(&cell?)?);
        }
        if cells.len() != columns.len() {
            return Err(PyValueError::new_err(format!(
                "row has {} cells, expected {}",
                cells.len(),
                columns.len()
            )));
        }
        rows.push(cells);
    }
    Ok(PyModOutcome::Rows { columns, rows })
}

/// Emits the frames for one outcome (ids all carry the request id, as
/// the wasm host does) and returns the row count for the audit trailer.
async fn send_mod_outcome(
    frames: &tokio::sync::mpsc::Sender<EnvelopeObject>,
    id: &str,
    outcome: PyModOutcome,
) -> i64 {
    let done = |row_count: i64| Done {
        id: id.to_string(),
        row_count: Some(row_count),
        affected_rows: None,
        last_insert_rowid: None,
        truncated: false,
    };
    match outcome {
        PyModOutcome::Done => {
            // The choreography requires a response frame before Done.
            let _ = frames
                .send(EnvelopeObject::Result(ResultHeader {
                    id: id.to_string(),
                    columns: vec![],
                    decl_types: vec![],
                }))
                .await;
            let _ = frames.send(EnvelopeObject::Done(done(0))).await;
            0
        }
        PyModOutcome::Rows { columns, rows } => {
            let n = rows.len() as i64;
            let _ = frames
                .send(EnvelopeObject::Result(ResultHeader {
                    id: id.to_string(),
                    columns: columns.clone(),
                    decl_types: vec![],
                }))
                .await;
            let batch: Vec<Row> = rows
                .into_iter()
                .enumerate()
                .map(|(i, cells)| Row {
                    seq: i as i64,
                    cells: columns.iter().cloned().zip(cells).collect(),
                })
                .collect();
            if !batch.is_empty() {
                let _ = frames.send(EnvelopeObject::Row(batch)).await;
            }
            let _ = frames.send(EnvelopeObject::Done(done(n))).await;
            n
        }
        PyModOutcome::Denied(reason) => {
            let _ = frames
                .send(EnvelopeObject::Denied(DeniedFrame {
                    id: Some(id.to_string()),
                    reason: Some(reason),
                }))
                .await;
            0
        }
        PyModOutcome::Error(reason) => {
            let _ = frames
                .send(EnvelopeObject::Error(ErrorEnvelope {
                    id: Some(id.to_string()),
                    code: "mod-error".to_string(),
                    reason: Some(reason),
                }))
                .await;
            0
        }
    }
}

#[pymodule]
fn resonator(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Node>()?;
    m.add_class::<QueryResult>()?;
    m.add_class::<MediaStream>()?;
    m.add("Denied", m.py().get_type::<Denied>())?;
    m.add("QueryError", m.py().get_type::<QueryError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsntr::testutil::TempDir;

    #[test]
    fn ensure_node_dir_inits_once() {
        let tmp = TempDir::new("py-init");
        ensure_node_dir(tmp.path()).expect("first init");
        let id = store::node_id(tmp.path()).expect("id");
        // Reopening must not re-init (init_dir refuses; ensure skips it).
        ensure_node_dir(tmp.path()).expect("reopen");
        assert_eq!(store::node_id(tmp.path()).expect("id again"), id);
    }

    #[test]
    fn classify_kind_per_mod() {
        assert_eq!(classify_kind("sql-sqlite", "SELECT 1"), RequestKind::Query);
        assert_eq!(
            classify_kind("sql-sqlite", "INSERT INTO t VALUES (1)"),
            RequestKind::Execute
        );
        assert_eq!(
            classify_kind("sparql", "PREFIX ex: <http://ex/> ASK { ex:a ?p ?o }"),
            RequestKind::Query
        );
        assert_eq!(
            classify_kind("sparql", "INSERT DATA { <a:s> <a:p> <a:o> }"),
            RequestKind::Execute
        );
    }

    #[test]
    fn turtle_converts_to_insert_data() {
        let (update, count) = turtle_to_insert_data(
            "@prefix ex: <http://example.org/> .\n\
             ex:s ex:p \"hallo\"@de, 5 .",
            None,
        )
        .expect("convert")
        .expect("non-empty");
        assert_eq!(count, 2);
        assert!(update.starts_with("INSERT DATA {"));
        assert!(update.contains("<http://example.org/s>"));
        assert!(update.contains("\"hallo\"@de"));
        // A triple-less document loads zero.
        assert!(
            turtle_to_insert_data("@prefix ex: <http://example.org/> .", None)
                .expect("convert")
                .is_none()
        );
        // Garbage errors.
        assert!(turtle_to_insert_data("not turtle", None).is_err());
    }

    /// The full local loop the Python `load_turtle` + `sparql` methods
    /// ride: init a directory, load Turtle over the owner channel as
    /// INSERT DATA, then SELECT it back through the sparql modulation.
    #[test]
    fn owner_channel_turtle_round_trip() {
        let tmp = TempDir::new("py-owner");
        ensure_node_dir(tmp.path()).expect("init");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async {
            let ch = OwnerChannel::open(tmp.path(), Prefer::Local)
                .await
                .expect("open channel");
            let (update, count) = turtle_to_insert_data(
                "@prefix ex: <http://example.org/> .\n\
                 ex:alice ex:knows ex:bob .\n\
                 ex:alice ex:name \"Alice\" .",
                None,
            )
            .expect("convert")
            .expect("non-empty");
            assert_eq!(count, 2);
            let outcome = channel::run(&ch, RequestKind::Execute, "sparql", &update, vec![])
                .await
                .expect("insert data");
            match outcome {
                QueryOutcome::Rows { done, .. } => {
                    assert_eq!(done.affected_rows, Some(2));
                }
                other => panic!("unexpected insert outcome: {other:?}"),
            }
            let outcome = channel::run(
                &ch,
                RequestKind::Query,
                "sparql",
                "SELECT ?o WHERE { <http://example.org/alice> <http://example.org/name> ?o }",
                vec![],
            )
            .await
            .expect("select");
            match outcome {
                QueryOutcome::Rows { rows, .. } => {
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0].cells[0].1, Value::Text("\"Alice\"".to_string()));
                }
                other => panic!("unexpected select outcome: {other:?}"),
            }
        });
    }
}
