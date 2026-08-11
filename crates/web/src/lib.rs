//! resonator-web: the rsntr web interface server (docs/web-api.md).
//!
//! An axum server inside the node process. The browser speaks RDF
//! envelope frames over plain HTTP (`/request`, `/entrain`,
//! `/stream/{id}`), listens on an SSE control feed (`/feed`), and drives
//! the day-1 tools through the JSON `/api` family. Every data access is
//! translated into envelope requests and submitted into the node's own
//! serving pipeline with the node's EndpointId as the peer: the local
//! surface acts as the owner, and nothing bypasses the peer gate, the
//! footprint authorizer, the authenticator chain, limits, or `_audit`.
//!
//! Auth: one capability token (minted here when the caller does not
//! provide a persisted one), accepted as `Authorization: Bearer` or the
//! `rsntr_token` cookie; only `GET /` and the static PWA assets
//! (manifest, service worker, icons) are served without it. The cookie
//! is issued exclusively in exchange for proof of the token: an
//! authenticated `POST /api/session`, or refreshed on a `GET /` that
//! already presents it. The CLI prints the entry URL with the token in
//! the fragment.

pub mod csv;
pub mod error;

mod api;
mod duplex;
mod feed;
mod frames;
mod local;
mod streams;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request as AxumRequest, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use resonator_node::{Node, NodeError};
use resonator_protocol::vocab::PROJECTION_CHANGED;
use resonator_protocol::{EnvelopeObject, Hello, Vibration};
use resonator_transport::{IrohTransport, PeerId};

pub use error::ApiFailure;
pub use feed::FeedEvent;
pub use streams::{ServerStream, StreamRegistry};

/// The default web bind address (web-api.md).
pub const DEFAULT_ADDR: &str = "127.0.0.1:2718";

/// Configuration of one web server.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Bind address; port 0 binds an ephemeral port.
    pub addr: SocketAddr,
    /// Capability token; minted (32 random bytes, base64url) when None.
    pub token: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            addr: DEFAULT_ADDR.parse().expect("static addr"),
            token: None,
        }
    }
}

/// Errors starting the web server.
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("binding {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("owner id {0:?} is not a 64-hex endpoint id")]
    BadOwner(String),
    #[error("node error: {0}")]
    Node(#[from] NodeError),
}

/// Shared state of the running server.
pub struct WebState {
    pub(crate) node: Arc<Node>,
    pub(crate) owner: PeerId,
    pub(crate) node_id: String,
    pub(crate) hello: Hello,
    pub(crate) token: String,
    pub(crate) transport: Option<Arc<IrohTransport>>,
    pub(crate) feed: broadcast::Sender<FeedEvent>,
    pub(crate) streams: StreamRegistry,
    /// Open audio-duplex exchanges awaiting upstream bytes.
    pub(crate) duplex: duplex::DuplexRegistry,
}

impl WebState {
    /// Publishes one event to every open control feed.
    pub fn publish(&self, ev: FeedEvent) {
        let _ = self.feed.send(ev);
    }

    /// Publishes a session-level envelope object on the feed.
    pub fn publish_envelope(&self, obj: &EnvelopeObject) {
        if let Ok(turtle) = obj.to_turtle() {
            self.publish(FeedEvent::Envelope { turtle });
        }
    }
}

/// A running web server.
pub struct WebServer {
    addr: SocketAddr,
    state: Arc<WebState>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    feed_task: JoinHandle<()>,
    feed_sub: u64,
}

impl WebServer {
    /// The bound address (with the real port when 0 was requested).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The capability token of this serve run.
    pub fn token(&self) -> &str {
        &self.state.token
    }

    /// The entry URL with the token in the fragment (never a query
    /// string): what the CLI prints and the browser opens.
    pub fn url(&self) -> String {
        format!("http://{}/#{}", self.addr, self.state.token)
    }

    /// The shared state (feed publishing, server-initiated streams).
    pub fn state(&self) -> &Arc<WebState> {
        &self.state
    }

    /// Stops accepting and drops all connections.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.feed_task.abort();
        self.state.node.entrainments().unsubscribe(self.feed_sub);
        // Graceful shutdown waits for open SSE feeds forever; abort.
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Admits the node's own EndpointId as a peer and seeds wildcard owner
/// policy (read/write/entrain allow) unless the owner already has rows
/// for those actions. A `_policy` row that denies the owner still wins
/// at equal or higher specificity, so denying the browser stays
/// possible. Idempotent; shared by the web server and the CLI's local
/// csv surface.
pub async fn ensure_owner_admitted(node: &Node, owner: &str) -> Result<(), NodeError> {
    let owner = owner.to_string();
    node.db()
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at, notes) \
                 VALUES (?1, datetime('now'), 'this node itself (local owner surface)')",
                [&owner],
            )?;
            for action in ["read", "write", "entrain"] {
                let have: i64 = conn.query_row(
                    "SELECT count(*) FROM _policy \
                     WHERE peer_or_group = ?1 AND table_name = '*' AND action = ?2",
                    (&owner, action),
                    |r| r.get(0),
                )?;
                if have == 0 {
                    conn.execute(
                        "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
                         VALUES (?1, '*', ?2, 'allow', 'local owner surface default')",
                        (&owner, action),
                    )?;
                }
            }
            Ok::<_, rusqlite::Error>(())
        })
        .await?
        .map_err(NodeError::Db)
}

/// Starts the web server for `node`, serving as `owner` (the node's own
/// 64-hex EndpointId). `transport` enables `?peer=` relaying; without it
/// only the local node is reachable.
pub async fn serve_web(
    node: Arc<Node>,
    owner: &str,
    transport: Option<Arc<IrohTransport>>,
    config: WebConfig,
) -> Result<WebServer, WebError> {
    let owner_id: PeerId = owner
        .parse()
        .map_err(|_| WebError::BadOwner(owner.to_string()))?;

    ensure_owner_admitted(&node, owner).await?;
    // Idempotent: `Node::run` installs the same hook; here it covers
    // web-only setups (and tests) that never start the wire loop.
    node.install_vibration_hook().await?;

    let token = match &config.token {
        Some(t) => t.clone(),
        None => mint_token(),
    };
    let (feed_tx, _) = broadcast::channel(256);
    let state = Arc::new(WebState {
        node: node.clone(),
        owner: owner_id,
        node_id: owner.to_string(),
        hello: resonator_transport::basic_hello(&[], Some("rsntr web surface")),
        token,
        transport,
        feed: feed_tx,
        streams: StreamRegistry::default(),
        duplex: duplex::DuplexRegistry::default(),
    });

    // Session-level freshness on the feed: each projection-changed
    // signal (a `_projection` or `_policy` commit) publishes one
    // `envelope` event carrying a Vibration of the well-known point.
    let (feed_sub, feed_task) = {
        let (sub_id, mut rx) = node
            .entrainments()
            .subscribe(PROJECTION_CHANGED.to_string(), None);
        let st = state.clone();
        let id = ulid::Ulid::new().to_string();
        let task = tokio::spawn(async move {
            let mut seq: i64 = 0;
            while rx.recv().await.is_some() {
                st.publish_envelope(&EnvelopeObject::Vibration(Vibration {
                    id: id.clone(),
                    point: PROJECTION_CHANGED.to_string(),
                    seq,
                    at: None,
                    payload: Vec::new(),
                }));
                seq += 1;
            }
        });
        (sub_id, task)
    };

    let app = router(state.clone());
    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .map_err(|source| WebError::Bind {
            addr: config.addr,
            source,
        })?;
    let addr = listener.local_addr().map_err(|source| WebError::Bind {
        addr: config.addr,
        source,
    })?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        });
        if let Err(e) = serve.await {
            tracing::warn!(error = %e, "web server exited with an error");
        }
    });

    Ok(WebServer {
        addr,
        state,
        shutdown: Some(shutdown_tx),
        task,
        feed_task,
        feed_sub,
    })
}

/// Mints a capability token: 32 bytes from the same CSPRNG that mints
/// node identities, encoded base64url (43 chars, no padding). Public so
/// embedders that persist a token across runs mint it the same way.
pub fn mint_token() -> String {
    URL_SAFE_NO_PAD.encode(iroh::SecretKey::generate().to_bytes())
}

/// The `Set-Cookie` value installing the persistent auth cookie. No
/// `Secure`: the origin is plain http on loopback (Safari refuses Secure
/// cookies there) or sits behind a TLS proxy; `HttpOnly` keeps the token
/// out of script storage.
fn token_cookie(token: &str) -> String {
    format!("rsntr_token={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000")
}

/// Paths served without a token: the UI shell and the static PWA assets
/// (browsers fetch the manifest, service worker, and icons without any
/// credentials). All of them are compiled in and carry no data.
fn public_path(path: &str) -> bool {
    // Deep links (/o/<peer>/proj[/<path>], /o/<peer>/holo/<mod>) serve
    // the same data-free shell as `/`: the client router reads the
    // path after auth. GET/HEAD-gated like every exemption here.
    if path.starts_with("/o/") {
        return true;
    }
    matches!(
        path,
        "/" | "/manifest.webmanifest"
            | "/sw.js"
            | "/icon-192.png"
            | "/icon-512.png"
            | "/icon-maskable-512.png"
            | "/apple-touch-icon.png"
            | "/favicon.ico"
    )
}

fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(ui))
        .route("/o/{*rest}", get(ui))
        .route("/manifest.webmanifest", get(manifest))
        .route("/sw.js", get(sw_js))
        .route("/icon-192.png", get(icon_192))
        .route("/icon-512.png", get(icon_512))
        .route("/icon-maskable-512.png", get(icon_maskable_512))
        .route("/apple-touch-icon.png", get(apple_touch_icon))
        // Browsers request /favicon.ico on their own; answer instead of
        // letting the request 401 into the fallback.
        .route("/favicon.ico", get(icon_192))
        .route("/api/session", post(session))
        .route("/feed", get(feed::feed))
        .route("/request", post(frames::request))
        .route("/entrain", post(frames::entrain))
        .route("/duplex/{id}", post(duplex::duplex_post))
        .route(
            "/stream/{id}",
            get(streams::stream_get).post(streams::stream_post),
        )
        .route("/api/meta", get(api::meta))
        .route("/api/peer/{id}", get(api::peer_get))
        .route("/api/table/{name}", get(api::table_get))
        .route("/api/table/{name}/rows", post(api::row_insert))
        .route(
            "/api/table/{name}/rows/{rowid}",
            axum::routing::patch(api::row_update).delete(api::row_delete),
        )
        .route("/api/sql", post(api::sql))
        .route("/api/sparql", post(api::sparql))
        .route("/api/turtle", post(api::turtle))
        .route("/api/csv/{table}", get(api::csv_get).post(api::csv_post))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        // Per-endpoint reads enforce their own tighter caps.
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024 + 1024))
        .with_state(state)
}

/// The UI shell: one self-contained HTML file, embedded at compile time,
/// served without a token (it contains no data). When the request
/// already presents the valid token (Bearer or cookie), the persistent
/// cookie is refreshed; a bare request never receives one.
async fn ui(State(state): State<Arc<WebState>>, req: AxumRequest) -> Response {
    let authed = request_token(&req).is_some_and(|t| ct_eq(t.as_bytes(), state.token.as_bytes()));
    let mut resp = (
        [
            ("Content-Type", "text/html; charset=utf-8"),
            ("Cache-Control", "no-store"),
        ],
        Html(include_str!("../../../web-ui/index.html")),
    )
        .into_response();
    if authed && let Ok(cookie) = token_cookie(&state.token).parse() {
        resp.headers_mut()
            .insert(axum::http::header::SET_COOKIE, cookie);
    }
    resp
}

/// `POST /api/session`: issues the persistent auth cookie. Sits behind
/// the auth middleware, so reaching it is the proof of token.
async fn session(State(state): State<Arc<WebState>>) -> Response {
    let cookie = token_cookie(&state.token);
    (
        StatusCode::NO_CONTENT,
        [
            ("Set-Cookie", cookie.as_str()),
            ("Cache-Control", "no-store"),
        ],
    )
        .into_response()
}

/// The web app manifest (PWA install metadata).
async fn manifest() -> Response {
    (
        [
            ("Content-Type", "application/manifest+json"),
            ("Cache-Control", "public, max-age=3600"),
        ],
        include_str!("../../../web-ui/manifest.webmanifest"),
    )
        .into_response()
}

/// The install-only service worker (no caching; see web-ui/sw.js).
/// no-cache so browsers revalidate the worker script promptly.
async fn sw_js() -> Response {
    (
        [
            ("Content-Type", "text/javascript; charset=utf-8"),
            ("Cache-Control", "no-cache"),
        ],
        include_str!("../../../web-ui/sw.js"),
    )
        .into_response()
}

async fn icon_192() -> Response {
    (
        [
            ("Content-Type", "image/png"),
            ("Cache-Control", "public, max-age=86400"),
        ],
        include_bytes!("../../../web-ui/icon-192.png").as_slice(),
    )
        .into_response()
}

async fn icon_512() -> Response {
    (
        [
            ("Content-Type", "image/png"),
            ("Cache-Control", "public, max-age=86400"),
        ],
        include_bytes!("../../../web-ui/icon-512.png").as_slice(),
    )
        .into_response()
}

async fn icon_maskable_512() -> Response {
    (
        [
            ("Content-Type", "image/png"),
            ("Cache-Control", "public, max-age=86400"),
        ],
        include_bytes!("../../../web-ui/icon-maskable-512.png").as_slice(),
    )
        .into_response()
}

async fn apple_touch_icon() -> Response {
    (
        [
            ("Content-Type", "image/png"),
            ("Cache-Control", "public, max-age=86400"),
        ],
        include_bytes!("../../../web-ui/apple-touch-icon.png").as_slice(),
    )
        .into_response()
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [
            ("Content-Type", "application/json"),
            ("Cache-Control", "no-store"),
        ],
        r#"{"ok":false,"error":{"code":"not-found","reason":"unknown route"}}"#,
    )
        .into_response()
}

/// Constant-time byte comparison (the token is fixed-length, so the
/// length check leaks nothing secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn request_token(req: &AxumRequest) -> Option<String> {
    if let Some(auth) = req.headers().get("Authorization")
        && let Ok(auth) = auth.to_str()
        && let Some(token) = auth.strip_prefix("Bearer ")
    {
        return Some(token.trim().to_string());
    }
    let cookies = req.headers().get("Cookie")?.to_str().ok()?;
    for pair in cookies.split(';') {
        let pair = pair.trim();
        if let Some(v) = pair.strip_prefix("rsntr_token=") {
            return Some(v.trim().to_string());
        }
    }
    None
}

async fn auth(State(state): State<Arc<WebState>>, req: AxumRequest, next: Next) -> Response {
    // Unauthenticated: only GET/HEAD of the UI shell and the static PWA
    // assets (public_path); everything else requires the token.
    let exempt = public_path(req.uri().path())
        && (req.method() == Method::GET || req.method() == Method::HEAD);
    if !exempt {
        let ok = request_token(&req).is_some_and(|t| ct_eq(t.as_bytes(), state.token.as_bytes()));
        if !ok {
            return (
                StatusCode::UNAUTHORIZED,
                [
                    ("Content-Type", "application/json"),
                    ("Cache-Control", "no-store"),
                ],
                Body::from(
                    r#"{"ok":false,"error":{"code":"unauthorized","reason":"missing or invalid token"}}"#,
                ),
            )
                .into_response();
        }
    }
    next.run(req).await
}

// ---------------------------------------------------------------------
// Shared table introspection
// ---------------------------------------------------------------------

/// One column of a table, from `pragma_table_info`.
#[derive(Debug, Clone)]
pub(crate) struct ColumnMeta {
    pub name: String,
    pub decltype: Option<String>,
    pub pk: bool,
    pub notnull: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TableMeta {
    pub columns: Vec<ColumnMeta>,
    pub without_rowid: bool,
}

/// Schema introspection (not row data) straight off the db thread; row
/// reads and writes go through the pipeline.
pub(crate) async fn read_table_meta(
    node: &Arc<Node>,
    table: &str,
) -> Result<Option<TableMeta>, ApiFailure> {
    let table = table.to_string();
    node.db()
        .call(move |conn| {
            let sql: Option<Option<String>> = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [&table],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            let Some(create_sql) = sql else {
                return Ok::<_, rusqlite::Error>(None);
            };
            let without_rowid = create_sql
                .as_deref()
                .is_some_and(|s| s.to_ascii_uppercase().contains("WITHOUT ROWID"));
            let mut stmt = conn.prepare(
                "SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid",
            )?;
            let columns = stmt
                .query_map([&table], |r| {
                    let decltype: String = r.get(1)?;
                    Ok(ColumnMeta {
                        name: r.get(0)?,
                        decltype: if decltype.is_empty() {
                            None
                        } else {
                            Some(decltype)
                        },
                        notnull: r.get::<_, i64>(2)? != 0,
                        pk: r.get::<_, i64>(3)? != 0,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(TableMeta {
                columns,
                without_rowid,
            }))
        })
        .await
        .map_err(ApiFailure::internal)?
        .map_err(ApiFailure::internal)
}

/// Quotes one SQL identifier; embedded quotes are doubled, so any table
/// or column name is safe to splice.
pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Parses the owner's 64-hex EndpointId.
pub(crate) fn owner_peer_id(owner: &str) -> Result<PeerId, ApiFailure> {
    owner
        .parse()
        .map_err(|_| ApiFailure::internal(format!("bad owner id {owner:?}")))
}
