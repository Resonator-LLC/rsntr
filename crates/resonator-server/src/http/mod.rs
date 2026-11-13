//! The HTTP server: authentication endpoints, the WebSocket upgrade and
//! the read-only wave endpoints, on axum.
//!
//! Paths follow the servlets `ServerMain` registers in Apache Wave
//! (`/auth/signin`, `/auth/signout`, `/auth/register`, `/socket`,
//! `/fetch/*`, `/waveref/*`, `/profile/*`, `/iniavatars/*`, `/locale/*`,
//! `/search/*`, `/notification/*`, `/attachment/*`, `/thumbnail/*`,
//! `/attachmentsInfo`); `/session`, `/auth/tokens`, `/search/changes`,
//! `/healthz` and `/metrics` are new. Handlers
//! are thin: they resolve the caller to an [`Identity`] (see
//! [`RequestContext`]) and call the library layers ([`crate::auth`],
//! [`crate::frontend`]).
//!
//! Every request passes [`context_layer`], which authenticates it (bearer
//! token or session cookie), rejects cross-site state-changing requests,
//! and sets the security headers.

mod attachment_handlers;
mod auth_handlers;
mod device_handlers;
mod person_handlers;
mod probe_handlers;
mod search_handlers;
mod wave_handlers;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Extension, Router};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::attachment::{AttachmentService, AttachmentSettings};
use crate::auth::{AuthService, AuthSettings, Identity, SESSION_COOKIE};
use crate::config::{Config, CookieSecure, IpNet};
use crate::frontend::ClientFrontend;
use crate::metrics::{Metrics, MetricsSnapshot};
use crate::rpc::{SocketContext, SocketSettings};
use crate::search::{SearchService, SearchSettings};
use crate::waveserver::WaveServer;
use crate::welcome::WelcomeService;

pub use device_handlers::{CONTACT_LINK_PREFIX, DeviceAccess};
pub use person_handlers::{ControlError, HostStatus, PersonControl};
pub use probe_handlers::{PROBES_PER_MINUTE, ProbeOutcome, RemoteProbe};

/// What the handlers share.
pub struct AppInner {
    pub config: Config,
    pub server: WaveServer,
    pub auth: AuthService,
    pub frontend: ClientFrontend,
    pub search: SearchService,
    pub attachments: AttachmentService,
    /// Set if a welcome wave is configured.
    pub welcome: Option<WelcomeService>,
    pub metrics: Arc<Metrics>,
    pub socket: Arc<SocketContext>,
    trusted_proxies: Vec<IpNet>,
    shutdown: watch::Sender<bool>,
    /// Set by federation: answers `/federation/probe`.
    remote_probe: std::sync::OnceLock<Arc<dyn RemoteProbe>>,
    probe_limiter: std::sync::Mutex<crate::auth::rate_limit::WindowLimiter>,
    /// Device mode: the one account and the loopback token.
    device: Option<DeviceAccess>,
    /// Set by federation in person mode: the person settings and
    /// pairing (`/person/...`), and whether a wave's host is frozen.
    person_control: std::sync::OnceLock<Arc<dyn PersonControl>>,
    /// Wakes whoever restarts the node (after pairing or recovery).
    restart: Arc<tokio::sync::Notify>,
    restart_asked: std::sync::atomic::AtomicBool,
}

/// The application state; cheap to clone.
#[derive(Clone)]
pub struct AppState(Arc<AppInner>);

impl std::ops::Deref for AppState {
    type Target = AppInner;

    fn deref(&self) -> &AppInner {
        &self.0
    }
}

impl AppState {
    /// Builds the authentication service, the client front end, the
    /// search service and the attachment store on `server`. Fails if the
    /// attachments directory cannot be used. Must be called within a tokio
    /// runtime.
    pub fn new(config: Config, server: WaveServer) -> std::io::Result<Self> {
        let auth = AuthService::new(server.database(), AuthSettings::from_config(&config));
        let frontend = ClientFrontend::new(server.clone());
        let search = SearchService::with_policy(
            server.clone(),
            Arc::clone(frontend.policy()),
            SearchSettings::from_config(&config),
        );
        let attachments = match AttachmentService::open(
            server.database(),
            AttachmentSettings::from_config(&config),
        ) {
            Ok(attachments) => attachments,
            Err(error) => {
                search.shutdown();
                frontend.shutdown();
                return Err(error);
            }
        };
        Ok(Self::with_parts(
            config,
            server,
            auth,
            frontend,
            search,
            attachments,
        ))
    }

    /// [`AppState::new`] with prepared services.
    pub fn with_parts(
        config: Config,
        server: WaveServer,
        auth: AuthService,
        frontend: ClientFrontend,
        search: SearchService,
        attachments: AttachmentService,
    ) -> Self {
        let welcome = config.welcome_wave().and_then(|template| {
            WelcomeService::new(
                server.clone(),
                auth.accounts().clone(),
                Arc::clone(auth.clock()),
                template,
            )
        });
        if !config.welcome_wave_id.is_empty() && welcome.is_none() {
            tracing::warn!(
                welcome_wave_id = %config.welcome_wave_id,
                "the configured welcome wave id is not a valid wave id; no welcome waves"
            );
        }
        if let Some(welcome) = &welcome {
            // Greeting happens on a task of its own: registration neither
            // waits for it nor fails with it.
            let greeter = welcome.clone();
            auth.on_registration(move |participant| {
                let (greeter, participant) = (greeter.clone(), participant.clone());
                tokio::spawn(async move {
                    if let Err(error) = greeter.greet(&participant).await {
                        tracing::warn!(%participant, %error, "could not create the welcome wave");
                    }
                });
            });
        }
        let metrics = Arc::new(Metrics::default());
        let kept = config.device_token.as_ref().map(|token| token.0.clone());
        let device = config
            .device_address()
            .map(|participant| DeviceAccess::new(participant, kept.as_deref()));
        let (shutdown, shutdown_signal) = watch::channel(false);
        let socket = Arc::new(SocketContext {
            frontend: frontend.clone(),
            auth: auth.clone(),
            metrics: Arc::clone(&metrics),
            settings: SocketSettings::from_config(&config),
            shutdown: shutdown_signal,
        });
        AppState(Arc::new(AppInner {
            // Validated with the config; an invalid entry trusts nobody.
            trusted_proxies: config.trusted_proxy_nets().unwrap_or_default(),
            config,
            server,
            auth,
            frontend,
            search,
            attachments,
            welcome,
            metrics,
            socket,
            shutdown,
            remote_probe: std::sync::OnceLock::new(),
            probe_limiter: std::sync::Mutex::new(crate::auth::rate_limit::WindowLimiter::new(
                probe_handlers::PROBES_PER_MINUTE,
                60_000,
            )),
            device,
            person_control: std::sync::OnceLock::new(),
            restart: Arc::new(tokio::sync::Notify::new()),
            restart_asked: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// Asks whoever runs the node to restart it (same configuration,
    /// address and device token): the node became a device of another
    /// person by pairing or recovery.
    pub fn request_restart(&self) {
        self.restart_asked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.restart.notify_waiters();
        self.restart.notify_one();
    }

    /// Resolves once a restart is asked for.
    pub async fn restart_requested(&self) {
        loop {
            let notified = self.restart.notified();
            if self.restart_asked.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    /// Tells every WebSocket connection to close.
    pub fn begin_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Resolves once shutdown has begun.
    pub(crate) async fn shutting_down(&self) {
        let mut shutdown = self.shutdown.subscribe();
        let _ = shutdown.wait_for(|stop| *stop).await;
    }

    fn is_trusted_proxy(&self, address: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|net| net.contains(address))
    }

    /// Everything `/metrics` reports.
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        let (submits, submit_errors) = self.frontend.submit_counts();
        MetricsSnapshot {
            open_subscriptions: self.frontend.subscription_count() as u64,
            submits_total: submits,
            submit_errors_total: submit_errors,
            db_writer_queue_depth: self.server.database().write_queue_depth() as u64,
            loaded_waves: self.server.loaded_wave_count() as u64,
            ..self.metrics.snapshot()
        }
    }
}

/// Who is making a request and from where; set by [`context_layer`] and
/// read by handlers as `Extension<RequestContext>`.
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// The authenticated caller, if any.
    pub identity: Option<Identity>,
    /// The client's address: the peer, or what a trusted proxy reports.
    pub client: IpAddr,
    /// Whether the client reached the server over HTTPS (as reported by a
    /// trusted proxy).
    pub https: bool,
    /// The session id, if the request was authenticated by cookie.
    pub session_id: Option<String>,
    /// Whether the peer is a trusted proxy, whose forwarding headers count.
    pub trusted_peer: bool,
}

impl RequestContext {
    pub fn participant(&self) -> Option<&resonator_model::id::ParticipantId> {
        self.identity.as_ref().map(|identity| &identity.participant)
    }
}

// ---- Small helpers ----

/// A JSON response.
pub(crate) fn json(status: StatusCode, value: serde_json::Value) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        value.to_string(),
    )
        .into_response()
}

/// A failure in the shape of the authentication endpoints.
pub(crate) fn failure(status: StatusCode, message: impl std::fmt::Display) -> Response {
    json(
        status,
        serde_json::json!({"status": "FAILED", "message": message.to_string()}),
    )
}

pub(crate) fn internal_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "request failed");
    failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

fn header_str(headers: &HeaderMap, name: impl header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The value of a request cookie.
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = header_str(headers, header::AUTHORIZATION)?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

/// The `Set-Cookie` value for the session cookie; `max_age_secs` 0 clears
/// it.
pub(crate) fn session_cookie(session_id: &str, max_age_secs: u64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

impl AppState {
    /// Whether the session cookie gets the `Secure` attribute.
    pub(crate) fn cookie_secure(&self, context: &RequestContext) -> bool {
        match self.config.cookie_secure {
            CookieSecure::Always => true,
            CookieSecure::Never => false,
            CookieSecure::Auto => context.https,
        }
    }

    /// The client address: the peer unless it is a trusted proxy, in which
    /// case the last address in `X-Forwarded-For` that is not one.
    fn client_address(&self, headers: &HeaderMap, peer: IpAddr) -> IpAddr {
        if !self.is_trusted_proxy(peer) {
            return peer;
        }
        headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .filter_map(|hop| hop.trim().parse::<IpAddr>().ok())
            .rfind(|hop| !self.is_trusted_proxy(*hop))
            .unwrap_or(peer)
    }

    /// Whether a request may change state or open the WebSocket as far as
    /// its origin goes: it names no origin (not a browser), or its origin
    /// is the server itself or configured as allowed.
    pub fn origin_allowed(&self, headers: &HeaderMap, trusted_peer: bool) -> bool {
        let referer_origin = || {
            let referer = header_str(headers, header::REFERER)?;
            let scheme_end = referer.find("://")? + 3;
            let end = referer[scheme_end..]
                .find('/')
                .map_or(referer.len(), |slash| scheme_end + slash);
            Some(&referer[..end])
        };
        let Some(origin) = header_str(headers, header::ORIGIN).or_else(referer_origin) else {
            // No origin at all: not a browser, unless it says otherwise.
            return header_str(headers, "sec-fetch-site")
                .is_none_or(|site| site == "same-origin" || site == "none");
        };
        if self
            .config
            .allowed_origins
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(origin))
        {
            return true;
        }
        // Device mode: the origins of the app wrapping the node.
        if self.device.is_some()
            && self
                .config
                .device
                .origins
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(origin))
        {
            return true;
        }
        let Some(authority) = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
        else {
            // "null" and other opaque origins.
            return false;
        };
        let forwarded_host = || {
            trusted_peer
                .then(|| header_str(headers, "x-forwarded-host"))
                .flatten()
        };
        forwarded_host()
            .or_else(|| header_str(headers, header::HOST))
            .is_some_and(|host| host.eq_ignore_ascii_case(authority))
    }
}

const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "same-origin"),
    ("cross-origin-opener-policy", "same-origin"),
    (
        "content-security-policy",
        "default-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
    ),
];

/// Authenticates the request, rejects cross-site state changes, and sets
/// the security headers on the response.
pub async fn context_layer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    Metrics::inc(&state.metrics.http_requests_total);
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());
    let trusted_peer = state.is_trusted_proxy(peer);
    let headers = request.headers();
    let mut context = RequestContext {
        identity: None,
        client: state.client_address(headers, peer),
        https: trusted_peer
            && header_str(headers, "x-forwarded-proto")
                .is_some_and(|proto| proto.eq_ignore_ascii_case("https")),
        session_id: None,
        trusted_peer,
    };
    let mut renewed_cookie = None;
    // Device mode: other machines are kept out, and the loopback token
    // signs in.
    if state.device.is_some()
        && let Some(response) =
            device_handlers::gate(&state, &context, request.method(), request.uri()).await
    {
        return with_security_headers(response, &context, None, &state);
    }
    // `/metrics` has a bearer token of its own.
    let bearer = bearer_token(headers).filter(|_| request.uri().path() != "/metrics");
    let response = 'respond: {
        if let Some(token) = bearer {
            match state.auth.authenticate_bearer(token).await {
                Ok(Some(identity)) => context.identity = Some(identity),
                Ok(None) => {
                    let mut response =
                        failure(StatusCode::UNAUTHORIZED, "Invalid or expired access token");
                    response.headers_mut().insert(
                        header::WWW_AUTHENTICATE,
                        HeaderValue::from_static("Bearer error=\"invalid_token\""),
                    );
                    break 'respond response;
                }
                Err(error) => break 'respond internal_error(error),
            }
        } else if let Some(session_id) = cookie(headers, SESSION_COOKIE) {
            match state.auth.authenticate_session(session_id).await {
                Ok(Some(session)) => {
                    if session.renewed {
                        renewed_cookie = Some((session_id.to_owned(), session.max_age_secs));
                    }
                    context.identity = Some(session.identity);
                    context.session_id = Some(session_id.to_owned());
                }
                Ok(None) => {}
                Err(error) => break 'respond internal_error(error),
            }
        }
        // Cross-site request forgery: a state-changing request that rides
        // on cookies (or could set them, as sign-in does) must come from
        // the server's own pages. Bearer requests carry their credential
        // explicitly and are exempt.
        let safe = matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        );
        if !safe && bearer.is_none() && !state.origin_allowed(headers, trusted_peer) {
            Metrics::inc(&state.metrics.csrf_rejections_total);
            break 'respond failure(StatusCode::FORBIDDEN, "Cross-site request rejected");
        }
        request.extensions_mut().insert(context.clone());
        next.run(request).await
    };
    with_security_headers(response, &context, renewed_cookie, &state)
}

/// Sets the security headers (and a renewed session cookie) on a
/// response of [`context_layer`].
fn with_security_headers(
    mut response: Response,
    context: &RequestContext,
    renewed_cookie: Option<(String, u64)>,
    state: &AppState,
) -> Response {
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        let name = HeaderName::from_static(name);
        // A handler may set a stricter content security policy of its own
        // (attachments are served sandboxed).
        if name == header::CONTENT_SECURITY_POLICY && headers.contains_key(&name) {
            continue;
        }
        headers.insert(name, HeaderValue::from_static(value));
    }
    if context.https {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if let Some((session_id, max_age_secs)) = renewed_cookie
        && !headers.contains_key(header::SET_COOKIE)
    {
        headers.insert(
            header::SET_COOKIE,
            session_cookie(&session_id, max_age_secs, state.cookie_secure(context)),
        );
    }
    response
}

/// The routes of the server.
pub fn router(state: AppState) -> Router {
    router_with(state, placeholder_routes())
}

/// The page served at `/` when no client is mounted there. A binary that
/// embeds the web client passes its own `/` route to [`router_with`]
/// instead.
pub fn placeholder_routes() -> Router<AppState> {
    Router::new().route("/", get(wave_handlers::index))
}

/// The routes of the server plus `extra` ones (the robot, agent and tool
/// endpoints, which live in another crate). The extra routes sit behind
/// the same layers as the built-in ones: their handlers find the caller in
/// the [`RequestContext`] extension, cross-site state changes by cookie
/// are rejected before they run, and the body limit applies.
pub fn router_with(state: AppState, extra: Router<AppState>) -> Router {
    use attachment_handlers as attachment;
    use auth_handlers as auth;
    use search_handlers as search;
    use wave_handlers as wave;
    // Uploads have a body limit of their own: the largest attachment plus
    // room for the other parts of the form.
    let upload_limit = usize::try_from(state.config.attachment_max_size)
        .unwrap_or(usize::MAX)
        .saturating_add(64 * 1024);
    let upload =
        || axum::routing::post(attachment::upload).layer(DefaultBodyLimit::max(upload_limit));
    Router::new()
        .route("/healthz", get(wave::healthz))
        .route("/metrics", get(wave::metrics))
        .route("/session", get(wave::session))
        .route("/socket", get(wave::socket))
        .route("/auth/signin", get(auth::signin_page).post(auth::signin))
        .route("/auth/signout", get(auth::signout_page).post(auth::signout))
        .route(
            "/auth/register",
            get(auth::register_page).post(auth::register),
        )
        .route(
            "/auth/tokens",
            get(auth::list_tokens).post(auth::create_token),
        )
        .route("/auth/tokens/{id}", delete(auth::revoke_token))
        .route("/fetch/{*path}", get(wave::fetch))
        .route("/waveref/{*path}", get(wave::waveref))
        .route("/directory", get(wave::directory))
        .route(
            "/contacts",
            get(device_handlers::list).post(device_handlers::add),
        )
        .route("/contacts/{address}", delete(device_handlers::remove))
        .route("/profile", get(wave::profiles))
        .route("/profile/", get(wave::profiles))
        .route("/profile/{*rest}", get(wave::profiles))
        .route("/iniavatars/{*path}", get(wave::initials_avatar))
        .route("/locale", get(wave::get_locale).post(wave::set_locale))
        .route("/locale/", get(wave::get_locale).post(wave::set_locale))
        .route(
            "/locale/{*rest}",
            get(wave::get_locale).post(wave::set_locale),
        )
        .route("/search", get(search::search))
        .route("/search/", get(search::search))
        .route("/search/changes", get(search::changes))
        .route("/notification", get(search::notification))
        .route("/notification/", get(search::notification))
        .route("/attachment", upload())
        .route("/attachment/", upload())
        .route(
            "/attachment/{*id}",
            get(attachment::download).merge(upload()),
        )
        .route("/thumbnail/{*id}", get(attachment::thumbnail))
        .route("/attachmentsInfo", get(attachment::info))
        .route("/attachmentsInfo/", get(attachment::info))
        .route("/federation/probe", get(probe_handlers::probe))
        .route("/person", get(person_handlers::overview))
        .route(
            "/person/revoke",
            axum::routing::post(person_handlers::revoke),
        )
        .route(
            "/person/anchor",
            axum::routing::post(person_handlers::anchor),
        )
        .route(
            "/person/phrase",
            axum::routing::post(person_handlers::phrase),
        )
        .route(
            "/person/recover",
            axum::routing::post(person_handlers::recover),
        )
        .route(
            "/person/cohost",
            axum::routing::post(person_handlers::co_host),
        )
        .route(
            "/person/pair",
            get(person_handlers::invitations).post(person_handlers::invite),
        )
        .route(
            "/person/pair/join",
            get(person_handlers::join_progress).post(person_handlers::join),
        )
        .route(
            "/person/pair/{id}/confirm",
            axum::routing::post(person_handlers::confirm),
        )
        .route("/person/pair/{id}", delete(person_handlers::cancel))
        .route("/waves/host-status", get(person_handlers::host_status))
        .route(
            "/waves/continue",
            axum::routing::post(person_handlers::continue_wave),
        )
        .merge(extra)
        .layer(DefaultBodyLimit::max(state.config.http_max_body_size))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            context_layer,
        ))
        .with_state(state)
}

/// The running HTTP server.
pub struct HttpServer {
    state: AppState,
    addresses: Vec<SocketAddr>,
    tasks: Vec<JoinHandle<()>>,
    stop: watch::Sender<bool>,
}

impl std::fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServer")
            .field("addresses", &self.addresses)
            .finish()
    }
}

impl HttpServer {
    /// Binds every configured listen address and starts serving.
    ///
    /// TLS SEAM: connections are plain TCP. Native TLS would wrap the
    /// accepted streams here (a rustls acceptor in front of the same
    /// router) and mark requests as HTTPS in [`context_layer`].
    pub async fn bind(state: AppState) -> std::io::Result<Self> {
        Self::bind_with(state, placeholder_routes()).await
    }

    /// [`HttpServer::bind`] with extra routes, see [`router_with`].
    pub async fn bind_with(state: AppState, extra: Router<AppState>) -> std::io::Result<Self> {
        let mut listeners = Vec::new();
        for address in &state.config.listen {
            let listener = TcpListener::bind(address.as_str()).await.map_err(|error| {
                std::io::Error::new(error.kind(), format!("cannot listen on {address}: {error}"))
            })?;
            listeners.push(listener);
        }
        Self::serve_with(state, listeners, extra)
    }

    /// Serves on listeners that are already bound.
    pub fn serve(state: AppState, listeners: Vec<TcpListener>) -> std::io::Result<Self> {
        Self::serve_with(state, listeners, placeholder_routes())
    }

    /// [`HttpServer::serve`] with extra routes, see [`router_with`].
    pub fn serve_with(
        state: AppState,
        listeners: Vec<TcpListener>,
        extra: Router<AppState>,
    ) -> std::io::Result<Self> {
        let (stop, _) = watch::channel(false);
        let mut addresses = Vec::new();
        let mut tasks = Vec::new();
        for listener in listeners {
            addresses.push(listener.local_addr()?);
            let app = router_with(state.clone(), extra.clone())
                .into_make_service_with_connect_info::<SocketAddr>();
            let mut stopped = stop.subscribe();
            tasks.push(tokio::spawn(async move {
                let served = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = stopped.wait_for(|stop| *stop).await;
                    })
                    .await;
                if let Err(error) = served {
                    tracing::error!(%error, "http server failed");
                }
            }));
        }
        Ok(HttpServer {
            state,
            addresses,
            tasks,
            stop,
        })
    }

    /// The addresses actually bound (with the real port where 0 was
    /// configured).
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.addresses
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Stops accepting connections, closes the WebSockets and waits (not
    /// for ever) for requests in flight.
    pub async fn shutdown(self) {
        self.state.begin_shutdown();
        let _ = self.stop.send(true);
        for mut task in self.tasks {
            if tokio::time::timeout(Duration::from_secs(10), &mut task)
                .await
                .is_err()
            {
                tracing::warn!("http connections still open after 10s; dropping them");
                task.abort();
            }
        }
        // The WebSocket tasks outlive their HTTP connections; give them a
        // moment to send their close frames.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self
            .state
            .metrics
            .websocket_connections
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.state.frontend.shutdown();
        self.state.search.shutdown();
    }
}

/// The request context, for handler signatures.
pub(crate) type Context = Extension<RequestContext>;

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn cookies_are_parsed() {
        let map = headers(&[("cookie", "a=1; WSESSIONID=abc; b=2"), ("cookie", "c=3")]);
        assert_eq!(cookie(&map, "WSESSIONID"), Some("abc"));
        assert_eq!(cookie(&map, "c"), Some("3"));
        assert_eq!(cookie(&map, "SESSIONID"), None);
        assert_eq!(cookie(&headers(&[]), "a"), None);
    }

    #[test]
    fn bearer_tokens_are_parsed() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer wn_pat_x")])),
            Some("wn_pat_x")
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "bearer  wn_pat_x ")])),
            Some("wn_pat_x")
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Basic abc")])),
            None
        );
        assert_eq!(bearer_token(&headers(&[])), None);
    }

    #[test]
    fn session_cookie_attributes() {
        assert_eq!(
            session_cookie("abc", 60, false),
            "WSESSIONID=abc; Path=/; HttpOnly; SameSite=Lax; Max-Age=60"
        );
        assert_eq!(
            session_cookie("abc", 0, true),
            "WSESSIONID=abc; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Secure"
        );
    }
}
