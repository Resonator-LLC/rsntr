//! The owner channel, client side (docs/owner-channel.md).
//!
//! Every local `rsntr` command is a generator of ordinary RDF envelope
//! objects (`rsntr:Query` / `rsntr:Execute`) delivered to the node over
//! a privileged path that skips the peer gate and the authenticator
//! chain while staying footprint-collected and audited. This module is
//! that path's client: it builds the transport, sends one envelope, and
//! collects the response frames into the same terminal shapes the
//! remote client uses, so command code is transport-blind.
//!
//! Two transports (doc sec 3):
//!
//! - the control socket `<dir>/rsntr.sock` while a node serves: framed
//!   envelopes execute on the serving process's live connection, so its
//!   update hook fires and Sympathetic points vibrate;
//! - in-process: the same pipeline `rsntr serve` builds, minus the
//!   transport, driven directly with decoded envelopes (always works,
//!   WAL beside a serving node, but the serving process's hooks do not
//!   fire).
//!
//! Selection (doc sec 3.3): try the socket, fall back to in-process;
//! `--socket` / `--local` force one or the other.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig};
use resonator_protocol::{
    Done, EnvelopeObject, Request, RequestKind, ResponseEvent, ResponseReader, Row, Value,
};
use resonator_transport::RequestStream;

use crate::client::QueryOutcome;
use crate::store;

/// Transport preference for one command invocation (doc sec 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prefer {
    /// Socket when a node serves the directory, in-process otherwise.
    #[default]
    Auto,
    /// The control socket only; fail when no node is serving.
    Socket,
    /// In-process only, even beside a serving node (writes then do not
    /// vibrate the serving process; debugging and bulk work).
    Local,
}

/// One opened owner channel; send as many requests as the command needs.
pub struct OwnerChannel {
    inner: Inner,
}

enum Inner {
    /// Dials `<dir>/rsntr.sock` freshly per request (the socket protocol
    /// is one request per connection).
    #[cfg(unix)]
    Socket { dir: PathBuf },
    /// The in-process pipeline: same construction as `rsntr serve`,
    /// minus the transport.
    Local { node: Arc<Node> },
}

impl OwnerChannel {
    /// Opens the owner channel for a node directory per `prefer`.
    pub async fn open(dir: &Path, prefer: Prefer) -> Result<Self> {
        match prefer {
            Prefer::Socket => {
                #[cfg(unix)]
                {
                    // Probe dial; the server tolerates a frameless close.
                    match crate::owner_socket::connect(dir).await {
                        Ok(_probe) => Ok(Self {
                            inner: Inner::Socket {
                                dir: dir.to_path_buf(),
                            },
                        }),
                        Err(e) => bail!(
                            "no node is serving {} (control socket: {e}); \
                             start `rsntr serve` or drop --socket",
                            dir.display()
                        ),
                    }
                }
                #[cfg(not(unix))]
                bail!("the control socket is not available on this platform; use --local")
            }
            Prefer::Local => Self::local(dir),
            Prefer::Auto => {
                #[cfg(unix)]
                if let Ok(_probe) = crate::owner_socket::connect(dir).await {
                    return Ok(Self {
                        inner: Inner::Socket {
                            dir: dir.to_path_buf(),
                        },
                    });
                }
                Self::local(dir)
            }
        }
    }

    /// Wraps an already-built pipeline (a serving node's own `Node`, or
    /// one kept alive by an embedding host) as an owner channel, so
    /// requests execute on that pipeline's live connection and its
    /// update hooks fire.
    pub fn from_node(node: Arc<Node>) -> Self {
        Self {
            inner: Inner::Local { node },
        }
    }

    /// Builds the in-process transport: the node database opened with
    /// the serving connection's surface (rdf_* SQL functions) behind a
    /// dedicated db thread, and the owner dispatch entry on top.
    pub fn local(dir: &Path) -> Result<Self> {
        let dpath = store::db_path(dir);
        if !dpath.exists() {
            bail!(
                "no node database at {}; run `rsntr init {}` first",
                dpath.display(),
                dir.display()
            );
        }
        let conn = resonator_node::open_node_db(&dpath)
            .map_err(|e| anyhow::anyhow!("opening {}: {e}", dpath.display()))?;
        store::configure(&conn)?;
        let node = Arc::new(Node::new(
            DbHandle::spawn(conn),
            Chain::with_builtin_tiers(),
            NodeConfig::default(),
        ));
        Ok(Self {
            inner: Inner::Local { node },
        })
    }

    /// The in-process pipeline, when this channel is local. Embedding
    /// hosts (the mobile ffi) use it for owner surfaces that need a
    /// streaming dispatch, e.g. entrainment callbacks.
    pub fn node(&self) -> Option<&Arc<Node>> {
        match &self.inner {
            #[cfg(unix)]
            Inner::Socket { .. } => None,
            Inner::Local { node } => Some(node),
        }
    }

    /// True when requests ride the control socket (and therefore execute
    /// on the serving process's live connection).
    pub fn is_socket(&self) -> bool {
        match &self.inner {
            #[cfg(unix)]
            Inner::Socket { .. } => true,
            Inner::Local { .. } => false,
        }
    }

    /// Sends one request envelope and collects the streamed response
    /// into its terminal shape. `id` is the request's ULID string (for
    /// response correlation).
    pub async fn send(&self, envelope: &EnvelopeObject, id: &str) -> Result<QueryOutcome> {
        match &self.inner {
            #[cfg(unix)]
            Inner::Socket { dir } => {
                let conn = crate::owner_socket::connect(dir)
                    .await
                    .with_context(|| format!("dialing the control socket in {}", dir.display()))?;
                let mut stream = crate::owner_socket::client_stream(conn);
                stream
                    .send(envelope)
                    .await
                    .map_err(|e| anyhow::anyhow!("sending the owner request: {e}"))?;
                let mut fold = FrameFold::new(id);
                while let Some(frame) = stream
                    .recv()
                    .await
                    .map_err(|e| anyhow::anyhow!("receiving the owner response: {e}"))?
                {
                    fold.accept(frame)?;
                }
                fold.finish()
            }
            Inner::Local { node } => {
                let mut collector = Collector::default();
                node.handle_owner(envelope.clone(), &mut collector)
                    .await
                    .map_err(|e| anyhow::anyhow!("owner dispatch failed: {e}"))?;
                let mut fold = FrameFold::new(id);
                for frame in collector.frames {
                    fold.accept(frame)?;
                }
                fold.finish()
            }
        }
    }
}

/// Runs one statement over an open channel and returns its outcome.
pub async fn run(
    channel: &OwnerChannel,
    kind: RequestKind,
    modulation: &str,
    signal: &str,
    params: Vec<Value>,
) -> Result<QueryOutcome> {
    let mut request = Request::new(kind, modulation, signal);
    request.params = params;
    let id = request.id_string();
    channel.send(&request.to_envelope(), &id).await
}

/// An `sql-sqlite` Execute expected to succeed: returns its `Done`;
/// `Denied` and error frames become errors.
pub async fn execute(channel: &OwnerChannel, sql: &str, params: Vec<Value>) -> Result<Done> {
    match run(channel, RequestKind::Execute, "sql-sqlite", sql, params).await? {
        QueryOutcome::Rows { done, .. } => Ok(done),
        QueryOutcome::Denied(d) => bail!(
            "denied: {}",
            d.reason.as_deref().unwrap_or("(no reason given)")
        ),
        QueryOutcome::Failed(e) => bail!("[{}] {}", e.code, e.reason.unwrap_or_default()),
        other => bail!("unexpected response shape for an execute: {other:?}"),
    }
}

/// An `sql-sqlite` Query expected to succeed: returns `(columns, rows,
/// done)`.
pub async fn query_rows(
    channel: &OwnerChannel,
    sql: &str,
    params: Vec<Value>,
) -> Result<(Vec<String>, Vec<Row>, Done)> {
    match run(channel, RequestKind::Query, "sql-sqlite", sql, params).await? {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => Ok((columns, rows, done)),
        QueryOutcome::Denied(d) => bail!(
            "denied: {}",
            d.reason.as_deref().unwrap_or("(no reason given)")
        ),
        QueryOutcome::Failed(e) => bail!("[{}] {}", e.code, e.reason.unwrap_or_default()),
        other => bail!("unexpected response shape for a query: {other:?}"),
    }
}

/// Runs an owner-channel future to completion from synchronous command
/// code. A dedicated thread with its own single-thread runtime, so it is
/// safe both outside any runtime (the CLI main path) and inside one (a
/// test's multi-thread runtime); inside a current-thread runtime that is
/// also serving the same directory it would deadlock, which no caller
/// does.
pub(crate) fn block_on<T: Send>(fut: impl std::future::Future<Output = T> + Send) -> Result<T> {
    std::thread::scope(|s| {
        let joined = s
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                Ok::<T, std::io::Error>(rt.block_on(fut))
            })
            .join();
        match joined {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow::anyhow!("building the owner-channel runtime: {e}")),
            Err(_) => Err(anyhow::anyhow!("the owner-channel worker panicked")),
        }
    })
}

/// The cell of `row` under `column`, or NULL (omitted columns are NULL
/// on the wire).
pub(crate) fn cell<'r>(row: &'r Row, column: &str) -> &'r Value {
    row.cells
        .iter()
        .find(|(n, _)| n == column)
        .map(|(_, v)| v)
        .unwrap_or(&Value::Null)
}

/// The cell as text, when present and textual.
pub(crate) fn cell_text(row: &Row, column: &str) -> Option<String> {
    match cell(row, column) {
        Value::Text(t) => Some(t.clone()),
        Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Response folding
// ---------------------------------------------------------------------

/// Folds response frames through the protocol reader into the terminal
/// [`QueryOutcome`], mirroring the remote client's collection loop.
struct FrameFold {
    reader: ResponseReader,
    columns: Vec<String>,
    rows: Vec<Row>,
    triples: Vec<oxrdf::Triple>,
    graphs_seen: bool,
    terminal: Option<QueryOutcome>,
}

impl FrameFold {
    fn new(id: &str) -> Self {
        Self {
            reader: ResponseReader::new(id),
            columns: Vec::new(),
            rows: Vec::new(),
            triples: Vec::new(),
            graphs_seen: false,
            terminal: None,
        }
    }

    fn accept(&mut self, frame: EnvelopeObject) -> Result<()> {
        match self.reader.accept(frame).context("response choreography")? {
            ResponseEvent::Header(h) => self.columns = h.columns,
            ResponseEvent::Rows(batch) => self.rows.extend(batch),
            ResponseEvent::Graph(g) => {
                self.graphs_seen = true;
                self.triples.extend(g.payload);
            }
            ResponseEvent::Generic(_) => {}
            ResponseEvent::Done(done) => {
                self.terminal = Some(if self.graphs_seen {
                    QueryOutcome::Graph {
                        triples: std::mem::take(&mut self.triples),
                        done,
                    }
                } else {
                    QueryOutcome::Rows {
                        columns: std::mem::take(&mut self.columns),
                        rows: std::mem::take(&mut self.rows),
                        done,
                    }
                });
            }
            ResponseEvent::Denied(d) => self.terminal = Some(QueryOutcome::Denied(d)),
            ResponseEvent::Error(e) => self.terminal = Some(QueryOutcome::Failed(e)),
            ResponseEvent::Help { signal, topics } => {
                self.terminal = Some(QueryOutcome::Help {
                    text: signal,
                    topics,
                });
            }
            ResponseEvent::Media(_) => {
                bail!("the owner channel does not carry media feeds");
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<QueryOutcome> {
        self.reader.finish().context("response stream truncated")?;
        self.terminal
            .context("response ended without a terminal frame")
    }
}

/// The in-process transport's stream: collects the pipeline's response
/// frames in memory (bounded by the node's response limits). `recv`
/// reports a closed read half, which is only reached by handlers that
/// listen for in-band frames (entrainment; not an owner-channel use).
#[derive(Default)]
struct Collector {
    frames: Vec<EnvelopeObject>,
}

impl RequestStream for Collector {
    async fn send(
        &mut self,
        obj: &EnvelopeObject,
    ) -> std::result::Result<(), resonator_transport::TransportError> {
        self.frames.push(obj.clone());
        Ok(())
    }

    async fn recv(
        &mut self,
    ) -> std::result::Result<Option<EnvelopeObject>, resonator_transport::TransportError> {
        Ok(None)
    }

    async fn send_raw(
        &mut self,
        _bytes: &[u8],
    ) -> std::result::Result<(), resonator_transport::TransportError> {
        Err(resonator_transport::TransportError::Stream(
            "raw byte streaming is not served on the owner channel".into(),
        ))
    }

    async fn finish(&mut self) -> std::result::Result<(), resonator_transport::TransportError> {
        Ok(())
    }
}
