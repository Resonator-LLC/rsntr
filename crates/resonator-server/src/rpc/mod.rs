//! The client wave protocol over a WebSocket: the `ProtocolWaveClientRpc`
//! service (Open, Submit, Authenticate) in the JSON envelope of the
//! original.
//!
//! Derived from `ServerRpcProvider`, `ServerRpcControllerImpl`,
//! `WebSocketChannel` and `WaveClientRpcImpl` of
//! `org.waveprotocol.box.server` in Apache Wave, licensed under the Apache
//! License, Version 2.0.
//!
//! Every text frame is one envelope
//! `{"messageType": NAME, "sequenceNumber": N, "message": BODY}` with the
//! body encoded as in [`resonator_model::proto::json`]. The client numbers its
//! requests; every response carries the number of its request:
//!
//! - `ProtocolAuthenticate` is answered by `ProtocolAuthenticationResult`.
//! - `ProtocolSubmitRequest` is answered by one `ProtocolSubmitResponse`.
//! - `ProtocolOpenRequest` starts a stream of `ProtocolWaveletUpdate`s
//!   that lasts until the client sends `CancelRpc` with the same number
//!   (answered by `RpcFinished` with `failed` false) or the stream fails
//!   (`RpcFinished` with `failed` true and the error text).
//! - A request that cannot be served is answered by `RpcFinished` with
//!   `failed` true.
//!
//! This module only translates between frames and the front end
//! ([`crate::frontend`]); what a view contains is decided there.
//!
//! Each connection has a bounded queue of outgoing messages. A client that
//! does not read fast enough for the queue to drain is disconnected rather
//! than buffered for.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::SinkExt;
use futures_util::stream::{SplitSink, StreamExt};
use resonator_model::id::{IdFilter, IdSerialiser, ModernIdSerialiser};
use resonator_model::proto::json::{self, Envelope, JsonMessage};
use resonator_model::proto::rpc::RpcFinished;
use resonator_model::proto::waveclient_rpc::{
    ProtocolAuthenticate, ProtocolAuthenticationResult, ProtocolOpenRequest, ProtocolSubmitRequest,
    ProtocolSubmitResponse,
};
use resonator_model::proto::{deserialize_hashed_version, serialize_hashed_version};
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio::time::Instant;

use crate::auth::{AuthService, Identity};
use crate::config::Config;
use crate::frontend::{
    ClientFrontend, KnownWavelet, SubscriptionHandle, UpdateSink, WaveViewUpdate,
};
use crate::metrics::Metrics;
use crate::waveserver::{SubmitError, SubmitResult};

/// WebSocket close codes used by the server.
#[cfg(feature = "apps")]
mod apps;

pub mod close_code {
    /// The server is shutting down.
    pub const GOING_AWAY: u16 = 1001;
    /// A sequence number was reused for a second open stream.
    pub const PROTOCOL_ERROR: u16 = 1002;
    /// A binary frame; the protocol is text only.
    pub const UNSUPPORTED_DATA: u16 = 1003;
    /// A frame that is not a JSON envelope.
    pub const INVALID_PAYLOAD: u16 = 1007;
    /// The client's credential ended, it was idle too long, or it did not
    /// keep up with its updates.
    pub const POLICY_VIOLATION: u16 = 1008;
    /// A message larger than the configured maximum.
    pub const MESSAGE_TOO_BIG: u16 = 1009;
}

/// Error texts sent to clients.
pub mod error_text {
    pub const NOT_LOGGED_IN: &str = "Not logged in";
    pub const AUTH_TOKEN_INVALID: &str = "Auth token invalid";
    pub const DIFFERENT_USER: &str = "Session already authenticated as a different user";
    pub const NO_READ_SCOPE: &str = "Token does not have the read scope";
    pub const NO_WRITE_SCOPE: &str = "Token does not have the write scope";
    pub const TOO_MANY_VIEWS: &str = "Too many open wave views on this connection";
}

/// How long a single frame may take to be written before the connection
/// is given up.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a connection may stay without an authenticated user.
const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(60);

/// Limits of a WebSocket connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketSettings {
    /// Largest message accepted, in bytes.
    pub max_message_size: usize,
    /// Messages that may wait to be written.
    pub send_queue: usize,
    /// Bytes (estimated) that may wait to be written.
    pub send_queue_bytes: usize,
    pub ping_interval: Duration,
    /// Client silence after which the connection is closed; `None` for
    /// never.
    pub idle_timeout: Option<Duration>,
    /// Wave views open at a time.
    pub max_subscriptions: usize,
}

impl SocketSettings {
    pub fn from_config(config: &Config) -> Self {
        SocketSettings {
            max_message_size: config.websocket_max_message_size,
            send_queue: config.websocket_send_queue,
            send_queue_bytes: config.websocket_send_queue_bytes,
            ping_interval: Duration::from_secs(config.websocket_ping_interval_secs.max(1)),
            idle_timeout: Some(config.websocket_idle_timeout_secs)
                .filter(|secs| *secs > 0)
                .map(Duration::from_secs),
            max_subscriptions: config.websocket_max_subscriptions,
        }
    }
}

/// What every connection shares.
#[derive(Debug, Clone)]
pub struct SocketContext {
    pub frontend: ClientFrontend,
    pub auth: AuthService,
    pub metrics: Arc<Metrics>,
    pub settings: SocketSettings,
    /// Becomes true when the server shuts down.
    pub shutdown: watch::Receiver<bool>,
    /// Wave apps, if they run here.
    #[cfg(feature = "apps")]
    pub apps: Option<crate::apps::AppManager>,
}

enum Outbound {
    /// Deltas or a notice of a wave view, serialised by the writer.
    Update {
        seq: i32,
        update: Box<WaveViewUpdate>,
    },
    /// A ready envelope.
    Text(String),
    Ping,
    Close {
        code: u16,
        reason: &'static str,
    },
}

/// A queued message and the bytes it counts for against the queue's limit.
struct Queued {
    message: Outbound,
    cost: usize,
}

/// The sending side of a connection's outgoing queue, which is bounded
/// both in messages and in (estimated) bytes. What a connection can hold
/// in memory is the byte limit plus the excess of at most two messages
/// that are larger than half of it (snapshots of large wavelets).
struct Outbox {
    queue: mpsc::Sender<Queued>,
    queued_bytes: AtomicUsize,
    max_bytes: usize,
    /// Signalled whenever the writer takes a message off the queue.
    drained: Notify,
    overflowed: AtomicBool,
    overflow: Notify,
    metrics: Arc<Metrics>,
}

impl Outbox {
    fn overflowed(&self) {
        if !self.overflowed.swap(true, Ordering::SeqCst) {
            self.overflow.notify_one();
        }
    }

    /// Queues a message. A full queue means the client is not keeping up:
    /// the connection is then closed. No single message counts for more
    /// than half the byte limit, so that one large snapshot does not by
    /// itself condemn the connection (or the small messages behind it).
    fn push(&self, message: Outbound, cost: usize) -> bool {
        let cost = cost.min(self.max_bytes / 2);
        let waiting = self.queued_bytes.fetch_add(cost, Ordering::SeqCst);
        if waiting.saturating_add(cost) > self.max_bytes {
            self.queued_bytes.fetch_sub(cost, Ordering::SeqCst);
            self.overflowed();
            return false;
        }
        match self.queue.try_send(Queued { message, cost }) {
            Ok(()) => true,
            Err(error) => {
                self.queued_bytes.fetch_sub(cost, Ordering::SeqCst);
                if matches!(error, mpsc::error::TrySendError::Full(_)) {
                    self.overflowed();
                }
                false
            }
        }
    }

    fn push_text(&self, text: String) -> bool {
        Metrics::inc(&self.metrics.websocket_messages_sent_total);
        let cost = text.len();
        self.push(Outbound::Text(text), cost)
    }

    fn send<M: JsonMessage>(&self, seq: i32, message: &M) -> bool {
        match json::envelope(seq, message) {
            Ok(text) => self.push_text(text),
            Err(error) => {
                tracing::error!(%error, message = M::NAME, "Failed to JSONify proto message");
                false
            }
        }
    }

    /// Ends an RPC with an error.
    fn fail(&self, seq: i32, error: impl Into<String>) -> bool {
        self.send(seq, &RpcFinished::failure(error))
    }
}

/// Delivers a wave view to the connection under the open request's
/// sequence number.
struct SocketSink {
    outbox: Arc<Outbox>,
    seq: i32,
}

/// Roughly the bytes a delta takes in the queue and on the wire: twice its
/// protobuf size, since the JSON form is wordier.
fn delta_cost(delta: &crate::storage::WaveletDeltaRecord) -> usize {
    delta
        .applied
        .as_ref()
        .map_or(256, |applied| applied.bytes().len())
        .saturating_mul(2)
}

impl UpdateSink for SocketSink {
    fn update(&self, update: WaveViewUpdate) -> bool {
        if update.snapshot.is_some() {
            // A snapshot's size is only known once serialised, and it is
            // not shared with other connections anyway.
            return match update
                .to_proto()
                .and_then(|message| json::envelope(self.seq, &message))
            {
                Ok(text) => self.outbox.push_text(text),
                Err(error) => {
                    tracing::error!(%error, "Failed to JSONify wavelet snapshot");
                    true
                }
            };
        }
        Metrics::inc(&self.outbox.metrics.websocket_messages_sent_total);
        let cost = update
            .deltas
            .iter()
            .map(|delta| delta_cost(delta))
            .fold(256usize, usize::saturating_add);
        self.outbox.push(
            Outbound::Update {
                seq: self.seq,
                update: Box::new(update),
            },
            cost,
        )
    }

    fn failed(&self, error: &str) {
        self.outbox.fail(self.seq, error);
    }

    fn accepts_resets(&self) -> bool {
        true
    }

    fn ready(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let outbox = &self.outbox;
            let deadline = Instant::now() + WRITE_TIMEOUT;
            loop {
                let drained = outbox.drained.notified();
                let busy = outbox.queued_bytes.load(Ordering::SeqCst) > outbox.max_bytes / 2;
                if !busy || outbox.queue.is_closed() || outbox.overflowed.load(Ordering::SeqCst) {
                    return;
                }
                if tokio::time::timeout_at(deadline, drained).await.is_err() {
                    return;
                }
            }
        })
    }
}

async fn write_frames(
    mut sink: SplitSink<WebSocket, Message>,
    mut queue: mpsc::Receiver<Queued>,
    outbox: std::sync::Weak<Outbox>,
) {
    while let Some(Queued { message, cost }) = queue.recv().await {
        if let Some(outbox) = outbox.upgrade() {
            outbox.queued_bytes.fetch_sub(cost, Ordering::SeqCst);
            outbox.drained.notify_waiters();
        }
        let frame = match message {
            Outbound::Update { seq, update } => {
                let text = update
                    .to_proto()
                    .and_then(|message| json::envelope(seq, &message));
                match text {
                    Ok(text) => Message::Text(text.into()),
                    Err(error) => {
                        tracing::error!(%error, "Failed to JSONify wavelet update");
                        continue;
                    }
                }
            }
            Outbound::Text(text) => Message::Text(text.into()),
            Outbound::Ping => Message::Ping(Default::default()),
            Outbound::Close { code, reason } => {
                let frame = Message::Close(Some(CloseFrame {
                    code,
                    reason: reason.into(),
                }));
                let _ = tokio::time::timeout(WRITE_TIMEOUT, sink.send(frame)).await;
                break;
            }
        };
        match tokio::time::timeout(WRITE_TIMEOUT, sink.send(frame)).await {
            Ok(Ok(())) => {}
            _ => break,
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
}

/// Finds the envelope's sequence number in text that could not be parsed,
/// so that the request can still be answered with an error.
fn salvage_sequence_number(text: &str) -> Option<i32> {
    const KEY: &str = "\"sequenceNumber\"";
    let rest = text[text.find(KEY)? + KEY.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let end = rest
        .char_indices()
        .find(|(index, c)| !(c.is_ascii_digit() || (*index == 0 && *c == '-')))
        .map_or(rest.len(), |(index, _)| index);
    rest[..end].parse().ok()
}

struct Connection {
    context: Arc<SocketContext>,
    outbox: Arc<Outbox>,
    identity: Option<Identity>,
    /// Open wave views by the sequence number of their open request
    /// (`Connection.activeRpcs`).
    views: HashMap<i32, SubscriptionHandle>,
    /// Open app views by the sequence number of their open request.
    #[cfg(feature = "apps")]
    apps: HashMap<i32, apps::AppView>,
}

type Flow = ControlFlow<(u16, &'static str)>;

impl Connection {
    fn close(code: u16, reason: &'static str) -> Flow {
        ControlFlow::Break((code, reason))
    }

    /// Parses a request body; answers the request with an error if that
    /// fails.
    fn body<M: JsonMessage>(&self, envelope: &Envelope) -> Option<M> {
        match envelope.body::<M>() {
            Ok(message) => Some(message),
            Err(error) => {
                tracing::debug!(%error, message = M::NAME, "message handling error");
                self.outbox.fail(
                    envelope.sequence_number,
                    format!("Invalid message: {error}"),
                );
                None
            }
        }
    }

    async fn handle_text(&mut self, text: &str) -> Flow {
        Metrics::inc(&self.context.metrics.websocket_messages_received_total);
        let envelope = match Envelope::parse(text) {
            Ok(envelope) => envelope,
            Err(error) => {
                tracing::debug!(%error, "unparseable frame");
                return match salvage_sequence_number(text) {
                    Some(seq) => {
                        self.outbox.fail(seq, format!("Invalid message: {error}"));
                        ControlFlow::Continue(())
                    }
                    None => Self::close(close_code::INVALID_PAYLOAD, "not a JSON envelope"),
                };
            }
        };
        let seq = envelope.sequence_number;
        match envelope.message_type.as_str() {
            ProtocolAuthenticate::NAME => self.authenticate(&envelope).await,
            ProtocolOpenRequest::NAME => return self.open(&envelope).await,
            ProtocolSubmitRequest::NAME => self.submit(&envelope).await,
            #[cfg(feature = "apps")]
            resonator_model::proto::app::AppOpenRequest::NAME => self.open_app(&envelope),
            #[cfg(feature = "apps")]
            resonator_model::proto::app::AppEventRequest::NAME => self.app_event(&envelope).await,
            #[cfg(feature = "apps")]
            resonator_model::proto::app::AppMediaRequest::NAME => self.app_media(&envelope),
            "CancelRpc" => {
                // The stream ends in the standard manner.
                #[cfg(feature = "apps")]
                if let Some(view) = self.apps.remove(&seq) {
                    let finished = view.is_closed();
                    drop(view);
                    if !finished {
                        self.outbox.send(seq, &RpcFinished::success());
                    }
                }
                if let Some(view) = self.views.remove(&seq) {
                    tracing::debug!(seq, "Cancelling open RPC");
                    let failed = view.is_closed();
                    drop(view);
                    if !failed {
                        self.outbox.send(seq, &RpcFinished::success());
                    }
                }
            }
            other => {
                self.outbox
                    .fail(seq, format!("Unknown proto class: {other}"));
            }
        }
        ControlFlow::Continue(())
    }

    #[cfg(feature = "apps")]
    fn open_app(&mut self, envelope: &Envelope) {
        let seq = envelope.sequence_number;
        let Some(request) = self.body::<resonator_model::proto::app::AppOpenRequest>(envelope)
        else {
            return;
        };
        let Some(identity) = &self.identity else {
            self.outbox.fail(seq, error_text::NOT_LOGGED_IN);
            return;
        };
        if !identity.can_write() {
            self.outbox.fail(seq, error_text::NO_WRITE_SCOPE);
            return;
        }
        apps::open(
            self.context.apps.as_ref(),
            &mut self.apps,
            &self.outbox,
            seq,
            &identity.participant,
            request,
        );
    }

    #[cfg(feature = "apps")]
    async fn app_event(&mut self, envelope: &Envelope) {
        let seq = envelope.sequence_number;
        let Some(request) = self.body::<resonator_model::proto::app::AppEventRequest>(envelope)
        else {
            return;
        };
        if self.identity.is_none() {
            self.outbox.fail(seq, error_text::NOT_LOGGED_IN);
            return;
        }
        apps::event(&self.apps, &self.outbox, seq, request).await;
    }

    #[cfg(feature = "apps")]
    fn app_media(&mut self, envelope: &Envelope) {
        let seq = envelope.sequence_number;
        let Some(request) = self.body::<resonator_model::proto::app::AppMediaRequest>(envelope)
        else {
            return;
        };
        if self.identity.is_none() {
            self.outbox.fail(seq, error_text::NOT_LOGGED_IN);
            return;
        }
        apps::media(&self.apps, &self.outbox, seq, request);
    }

    async fn authenticate(&mut self, envelope: &Envelope) {
        let seq = envelope.sequence_number;
        let Some(request) = self.body::<ProtocolAuthenticate>(envelope) else {
            return;
        };
        let token = request.token.unwrap_or_default();
        let authenticated = match self.context.auth.authenticate_socket_token(&token).await {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                self.outbox.fail(seq, error_text::AUTH_TOKEN_INVALID);
                return;
            }
            Err(error) => {
                tracing::error!(%error, "authentication lookup failed");
                self.outbox.fail(seq, "Internal error");
                return;
            }
        };
        match &self.identity {
            Some(current) if current.participant != authenticated.participant => {
                self.outbox.fail(seq, error_text::DIFFERENT_USER);
                return;
            }
            // Keep the credential that allows more.
            Some(current) if !authenticated.scopes.contains(current.scopes) => {}
            _ => {
                tracing::debug!(participant = %authenticated.participant, "Session authenticated");
                self.identity = Some(authenticated);
            }
        }
        self.outbox.send(seq, &ProtocolAuthenticationResult {});
    }

    async fn open(&mut self, envelope: &Envelope) -> Flow {
        let seq = envelope.sequence_number;
        let Some(request) = self.body::<ProtocolOpenRequest>(envelope) else {
            return ControlFlow::Continue(());
        };
        self.views.retain(|_, view| !view.is_closed());
        if self.views.contains_key(&seq) {
            return Self::close(
                close_code::PROTOCOL_ERROR,
                "sequence number of an open RPC reused",
            );
        }
        let outbox = &self.outbox;
        let Some(identity) = &self.identity else {
            outbox.fail(seq, error_text::NOT_LOGGED_IN);
            return ControlFlow::Continue(());
        };
        if !identity.can_read() {
            outbox.fail(seq, error_text::NO_READ_SCOPE);
            return ControlFlow::Continue(());
        }
        let wave_id = match ModernIdSerialiser
            .deserialise_wave_id(request.wave_id.as_deref().unwrap_or(""))
        {
            Ok(wave_id) => wave_id,
            Err(error) => {
                outbox.fail(seq, error.to_string());
                return ControlFlow::Continue(());
            }
        };
        let mut known_wavelets = Vec::with_capacity(request.known_wavelet.len());
        for known in &request.known_wavelet {
            let wavelet_id = ModernIdSerialiser
                .deserialise_wavelet_id(known.wavelet_id.as_deref().unwrap_or(""))
                .map_err(|error| error.to_string());
            let version = known
                .hashed_version
                .as_ref()
                .ok_or_else(|| "Missing known version".to_owned())
                .and_then(|version| {
                    deserialize_hashed_version(version).map_err(|error| error.to_string())
                });
            match (wavelet_id, version) {
                (Ok(wavelet_id), Ok(version)) => known_wavelets.push(KnownWavelet {
                    wavelet_id,
                    version,
                }),
                (Err(error), _) | (_, Err(error)) => {
                    outbox.fail(seq, error);
                    return ControlFlow::Continue(());
                }
            }
        }
        if self.views.len() >= self.context.settings.max_subscriptions {
            outbox.fail(seq, error_text::TOO_MANY_VIEWS);
            return ControlFlow::Continue(());
        }
        let filter = IdFilter::of([], request.wavelet_id_prefix);
        let sink = Box::new(SocketSink {
            outbox: Arc::clone(outbox),
            seq,
        });
        match self
            .context
            .frontend
            .open(
                &identity.participant,
                &wave_id,
                filter,
                &known_wavelets,
                sink,
            )
            .await
        {
            Ok(view) => {
                self.views.insert(seq, view);
            }
            Err(error) => {
                tracing::warn!(%error, "openRequest failure");
                outbox.fail(seq, error.to_string());
            }
        }
        ControlFlow::Continue(())
    }

    async fn submit(&mut self, envelope: &Envelope) {
        let seq = envelope.sequence_number;
        let Some(request) = self.body::<ProtocolSubmitRequest>(envelope) else {
            return;
        };
        let outbox = &self.outbox;
        let wavelet_name = match ModernIdSerialiser
            .deserialise_wavelet_name(request.wavelet_name.as_deref().unwrap_or(""))
        {
            Ok(wavelet_name) => wavelet_name,
            Err(error) => {
                outbox.fail(seq, error.to_string());
                return;
            }
        };
        // Called once with the answer, or twice for a submission that waits
        // for its host: first with the interim `pending` answer, then with
        // the final one.
        let respond = {
            let outbox = Arc::clone(&self.outbox);
            move |result: &Result<SubmitResult, String>| {
                let response = match result {
                    Ok(result) => ProtocolSubmitResponse {
                        operations_applied: Some(
                            i32::try_from(result.operations_applied).unwrap_or(i32::MAX),
                        ),
                        error_message: None,
                        hashed_version_after_application: serialize_hashed_version(
                            &result.hashed_version_after_application,
                        )
                        .ok(),
                        pending: result.pending.map(|_| true),
                    },
                    Err(error) => ProtocolSubmitResponse {
                        operations_applied: Some(0),
                        error_message: Some(error.clone()),
                        hashed_version_after_application: None,
                        pending: None,
                    },
                };
                outbox.send(seq, &response);
            }
        };
        let identity = match &self.identity {
            // As in the original, where no user never matches the author.
            None => {
                respond(&Err(SubmitError::AuthorMismatch.to_string()));
                return;
            }
            Some(identity) if !identity.can_write() => {
                respond(&Err(error_text::NO_WRITE_SCOPE.to_owned()));
                return;
            }
            Some(identity) => identity,
        };
        let Some(delta) = request.delta else {
            respond(&Err("Missing delta".to_owned()));
            return;
        };
        let _ = self
            .context
            .frontend
            .submit_request(
                &identity.participant,
                &wavelet_name,
                delta,
                request.channel_id.as_deref(),
                respond,
            )
            .await;
    }

    /// Whether the connection's credential is still good.
    async fn still_authenticated(&self) -> bool {
        match &self.identity {
            None => true,
            Some(identity) => self
                .context
                .auth
                .validate(identity)
                .await
                // Do not drop connections over a storage hiccup.
                .unwrap_or(true),
        }
    }
}

/// Serves the wave protocol on an accepted WebSocket until it closes.
/// `identity` is who the upgrade request was authenticated as, if anyone;
/// the client may also authenticate with `ProtocolAuthenticate`.
pub async fn serve_socket(
    socket: WebSocket,
    context: Arc<SocketContext>,
    identity: Option<Identity>,
) {
    let metrics = Arc::clone(&context.metrics);
    Metrics::inc(&metrics.websocket_connections_total);
    metrics
        .websocket_connections
        .fetch_add(1, Ordering::Relaxed);
    tracing::debug!(
        participant = identity.as_ref().map(|i| i.participant.address()),
        "New websocket connection"
    );

    let (sink, mut frames) = socket.split();
    let (queue, queued) = mpsc::channel(context.settings.send_queue.max(1));
    let outbox = Arc::new(Outbox {
        queue,
        queued_bytes: AtomicUsize::new(0),
        max_bytes: context.settings.send_queue_bytes.max(1),
        drained: Notify::new(),
        overflowed: AtomicBool::new(false),
        overflow: Notify::new(),
        metrics: Arc::clone(&metrics),
    });
    let mut writer = tokio::spawn(write_frames(sink, queued, Arc::downgrade(&outbox)));

    let settings = context.settings.clone();
    let mut revocations = context.auth.revocations();
    let mut shutdown = context.shutdown.clone();
    let mut connection = Connection {
        context,
        outbox: Arc::clone(&outbox),
        identity,
        views: HashMap::new(),
        #[cfg(feature = "apps")]
        apps: HashMap::new(),
    };
    let mut ping = tokio::time::interval_at(
        Instant::now() + settings.ping_interval,
        settings.ping_interval,
    );
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let connected = Instant::now();
    let mut last_heard = connected;
    let mut writer_done = false;

    let close = loop {
        let flow: Flow = tokio::select! {
            frame = frames.next() => match frame {
                None | Some(Ok(Message::Close(_))) => break None,
                Some(Err(error)) => {
                    // Most likely a message over the size limit.
                    tracing::debug!(%error, "websocket receive failed");
                    let text = error.to_string().to_ascii_lowercase();
                    let too_big = ["too long", "too large", "too big", "capacity"]
                        .iter()
                        .any(|hint| text.contains(hint));
                    break too_big.then_some((close_code::MESSAGE_TOO_BIG, "message too big"));
                }
                Some(Ok(Message::Text(text))) => {
                    last_heard = Instant::now();
                    connection.handle_text(text.as_str()).await
                }
                Some(Ok(Message::Binary(_))) => {
                    Connection::close(close_code::UNSUPPORTED_DATA, "text frames only")
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                    last_heard = Instant::now();
                    ControlFlow::Continue(())
                }
            },
            _ = outbox.overflow.notified() => {
                Metrics::inc(&metrics.websocket_slow_consumers_total);
                tracing::info!(
                    participant = connection.identity.as_ref().map(|i| i.participant.address()),
                    "closing websocket of a client that does not keep up"
                );
                break Some((close_code::POLICY_VIOLATION, "client too slow"));
            }
            _ = ping.tick() => {
                if settings.idle_timeout.is_some_and(|limit| last_heard.elapsed() > limit) {
                    Connection::close(close_code::POLICY_VIOLATION, "idle timeout")
                } else if connection.identity.is_none() && connected.elapsed() > AUTHENTICATION_TIMEOUT {
                    Connection::close(close_code::POLICY_VIOLATION, "authentication timeout")
                } else if !connection.still_authenticated().await {
                    Connection::close(close_code::POLICY_VIOLATION, "signed out")
                } else {
                    outbox.push(Outbound::Ping, 0);
                    ControlFlow::Continue(())
                }
            }
            revocation = revocations.recv() => {
                let revoked = match revocation {
                    Ok(revocation) => connection
                        .identity
                        .as_ref()
                        .is_some_and(|identity| revocation.applies_to(identity)),
                    // Missed some: ask.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        !connection.still_authenticated().await
                    }
                    Err(broadcast::error::RecvError::Closed) => true,
                };
                if revoked {
                    Connection::close(close_code::POLICY_VIOLATION, "signed out")
                } else {
                    ControlFlow::Continue(())
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    Connection::close(close_code::GOING_AWAY, "server shutting down")
                } else {
                    ControlFlow::Continue(())
                }
            }
            _ = &mut writer => {
                writer_done = true;
                break None;
            }
        };
        if let ControlFlow::Break(close) = flow {
            break Some(close);
        }
        if outbox.overflowed.load(Ordering::SeqCst) {
            Metrics::inc(&metrics.websocket_slow_consumers_total);
            break Some((close_code::POLICY_VIOLATION, "client too slow"));
        }
    };

    // End the views first so that nothing more is queued.
    connection.views.clear();
    if let Some((code, reason)) = close {
        // Not through `push`: a full queue is no news here.
        let _ = outbox.queue.try_send(Queued {
            message: Outbound::Close { code, reason },
            cost: 0,
        });
    }
    drop(connection);
    drop(outbox);
    // The writer ends once the queue is drained and every sender is gone;
    // a client that does not read does not get to hold it up.
    if !writer_done
        && tokio::time::timeout(Duration::from_secs(2), &mut writer)
            .await
            .is_err()
    {
        writer.abort();
    }
    metrics
        .websocket_connections
        .fetch_sub(1, Ordering::Relaxed);
    tracing::debug!("websocket disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_numbers_are_salvaged_from_broken_frames() {
        let salvage = salvage_sequence_number;
        assert_eq!(
            salvage(r#"{"messageType":"X","sequenceNumber":17,"message":{"1":"\ud800"}}"#),
            Some(17)
        );
        assert_eq!(salvage(r#"{"sequenceNumber" : -3 ,"message":"#), Some(-3));
        assert_eq!(salvage(r#"{"sequenceNumber":"7"}"#), None);
        assert_eq!(salvage(r#"{"sequenceNumber":}"#), None);
        assert_eq!(salvage(r#"{"sequenceNumber":99999999999}"#), None);
        assert_eq!(salvage("garbage"), None);
        // An escaped key inside a string does not count.
        assert_eq!(salvage(r#"{"x":"\"sequenceNumber\":5"}"#), None);
    }
}
