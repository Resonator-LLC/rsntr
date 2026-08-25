//! The local surface: a [`RequestStream`] adapter that feeds the node's
//! own serving pipeline, so every HTTP request runs the exact same path
//! (peer gate, footprint, authenticator chain, limits, audit) as an iroh
//! request, with the node's own EndpointId as the peer.

use std::sync::Arc;

use tokio::sync::mpsc;

use resonator_node::Node;
use resonator_protocol::{Denied, Done, EnvelopeObject, ErrorEnvelope, Hello, ResultHeader, Row};
use resonator_transport::{IncomingRequest, PeerId, RequestStream, TransportError};

use crate::WebState;
use crate::duplex::UpMsg;
use crate::error::ApiFailure;

/// One piece of a response as it leaves the pipeline: a typed frame, or
/// raw bytes (the media modulation's byte feed).
#[derive(Debug)]
pub(crate) enum Piece {
    Frame(EnvelopeObject),
    Raw(Vec<u8>),
}

/// The pipeline's view of one HTTP exchange. `send` hands pieces to the
/// consumer over a bounded channel (the response body's backpressure);
/// a dropped consumer fails the send, which cancels the running request
/// exactly like a reset iroh stream. `recv` never yields: the HTTP
/// response has no write half, so an entrainment ends only by damping
/// (client abort) or a failed vibration send.
pub(crate) struct LocalStream {
    out: mpsc::Sender<Piece>,
    /// Held so `inbound` never closes: `recv` must stay pending rather
    /// than report a clean half-close the client never performed.
    _hold: mpsc::Sender<EnvelopeObject>,
    inbound: mpsc::Receiver<EnvelopeObject>,
    /// Upstream raw bytes from `/duplex/{id}` POSTs; `None` for the
    /// ordinary one-shot exchanges.
    upstream: Option<mpsc::Receiver<UpMsg>>,
    up_eof: bool,
}

impl RequestStream for LocalStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        self.out
            .send(Piece::Frame(obj.clone()))
            .await
            .map_err(|_| TransportError::Stream("web client went away".into()))
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        Ok(self.inbound.recv().await)
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.out
            .send(Piece::Raw(bytes.to_vec()))
            .await
            .map_err(|_| TransportError::Stream("web client went away".into()))
    }

    async fn recv_raw(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.up_eof {
            return Ok(None);
        }
        // Disjoint field borrows: the upstream receiver and the response
        // sender are watched together on purpose. A caller that simply
        // walks away never sends Fin, and a talk sink never writes
        // downstream for a failing send to reveal the departure, so
        // without this the handler would wait forever on both arms and
        // the source process would outlive the call.
        let out = &self.out;
        let Some(rx) = self.upstream.as_mut() else {
            return Ok(None);
        };
        let ended = tokio::select! {
            msg = rx.recv() => match msg {
                Some(UpMsg::Data(b)) => return Ok(Some(b.to_vec())),
                Some(UpMsg::Fin) | None => true,
            },
            _ = out.closed() => true,
        };
        if ended {
            self.up_eof = true;
        }
        Ok(None)
    }

    /// Resolves when the browser drops the response body: without this,
    /// a silent duplex source outlives the client's departure after Fin
    /// (the trait's default never resolves).
    async fn closed(&mut self) {
        self.out.closed().await
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

/// Submits one request envelope into the pipeline as the given peer and
/// returns the receiver of its response pieces.
pub(crate) fn spawn_local(
    node: Arc<Node>,
    peer: PeerId,
    hello: Hello,
    first: EnvelopeObject,
) -> mpsc::Receiver<Piece> {
    let (out, rx) = mpsc::channel::<Piece>(8);
    let (hold, inbound) = mpsc::channel::<EnvelopeObject>(1);
    let stream = LocalStream {
        out,
        _hold: hold,
        inbound,
        upstream: None,
        up_eof: false,
    };
    let inc = IncomingRequest {
        peer,
        peer_hello: hello,
        first,
        stream,
    };
    tokio::spawn(async move {
        if let Err(e) = node.handle(inc).await {
            tracing::debug!(error = %e, "local web request failed");
        }
    });
    rx
}

/// [`spawn_local`] for an audio-duplex exchange: the stream reads its
/// upstream bytes from the `/duplex/{id}` channel, and the registry
/// entry is reaped when the exchange ends.
pub(crate) fn spawn_local_duplex(
    state: Arc<WebState>,
    first: EnvelopeObject,
    upstream: mpsc::Receiver<UpMsg>,
    id: String,
) -> mpsc::Receiver<Piece> {
    let (out, rx) = mpsc::channel::<Piece>(8);
    let (hold, inbound) = mpsc::channel::<EnvelopeObject>(1);
    let stream = LocalStream {
        out,
        _hold: hold,
        inbound,
        upstream: Some(upstream),
        up_eof: false,
    };
    let inc = IncomingRequest {
        peer: state.owner,
        peer_hello: state.hello.clone(),
        first,
        stream,
    };
    tokio::spawn(async move {
        if let Err(e) = state.node.handle(inc).await {
            tracing::debug!(error = %e, "local audio-duplex exchange failed");
        }
        state.duplex.remove(&id);
    });
    rx
}

/// A fully collected non-streaming response, for the JSON `/api` family.
#[derive(Debug)]
pub(crate) enum Collected {
    Rows {
        columns: Vec<String>,
        rows: Vec<Row>,
        done: Done,
    },
    Graph {
        triples: Vec<oxrdf::Triple>,
        done: Done,
    },
}

/// Runs one envelope through the local pipeline and collects the frames
/// into a terminal shape; `Denied`/`Error` frames become failures.
pub(crate) async fn run_collect(
    node: Arc<Node>,
    peer: PeerId,
    hello: Hello,
    envelope: EnvelopeObject,
) -> Result<Collected, ApiFailure> {
    let mut rx = spawn_local(node, peer, hello, envelope);
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut triples: Vec<oxrdf::Triple> = Vec::new();
    let mut graphs_seen = false;
    while let Some(piece) = rx.recv().await {
        let frame = match piece {
            Piece::Frame(f) => f,
            Piece::Raw(_) => {
                return Err(ApiFailure::error(
                    "protocol-error",
                    "raw media bytes on a collected response",
                ));
            }
        };
        match frame {
            EnvelopeObject::Result(ResultHeader { columns: c, .. }) => {
                columns = c;
            }
            EnvelopeObject::Row(batch) => rows.extend(batch),
            EnvelopeObject::Graph(g) => {
                graphs_seen = true;
                triples.extend(g.payload);
            }
            EnvelopeObject::Done(done) => {
                return Ok(if graphs_seen {
                    Collected::Graph { triples, done }
                } else {
                    Collected::Rows {
                        columns,
                        rows,
                        done,
                    }
                });
            }
            EnvelopeObject::Denied(Denied { reason, .. }) => {
                return Err(ApiFailure::Denied { reason });
            }
            EnvelopeObject::Error(ErrorEnvelope { code, reason, .. }) => {
                return Err(ApiFailure::Error {
                    code,
                    reason: reason.unwrap_or_default(),
                });
            }
            // Passthrough frames from mods this surface has no view of.
            EnvelopeObject::Generic(_) => {}
            other => {
                return Err(ApiFailure::error(
                    "protocol-error",
                    format!("unexpected response frame: {other:?}"),
                ));
            }
        }
    }
    Err(ApiFailure::internal(
        "response ended without a terminal frame",
    ))
}
