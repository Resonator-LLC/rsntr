//! The framed endpoints: `POST /request` (web-api.md section 6) and
//! `POST /entrain` (section 7), plus the shared frame plumbing.
//!
//! Bodies in both directions are `application/rsntr-frames`: u32-LE
//! length prefix + one Turtle document per record, byte-identical to the
//! iroh wire. The status code is mapped from the first response frame
//! (it is known before the body starts); later failures arrive in-band.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::BytesMut;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use resonator_protocol::{
    EnvelopeObject, ErrorEnvelope, MAX_FRAME_LEN, decode_frame_eof, encode_envelope,
};
use resonator_transport::{PeerId, RequestStream, Transport};

use crate::WebState;
use crate::duplex::UpMsg;
use crate::error::error_status;
use crate::local::{Piece, spawn_local, spawn_local_duplex};

const FRAMES_CONTENT_TYPE: &str = "application/rsntr-frames";

/// Builds an error response on a framed endpoint: mapped status, body =
/// one `rsntr:Error` frame (so a frame-only client never needs the
/// status).
pub(crate) fn frames_error(
    status: StatusCode,
    code: impl Into<String>,
    reason: impl Into<String>,
    id: Option<String>,
) -> Response {
    let code = code.into();
    // HTTP-native conditions have no envelope code; the closest wire
    // code still names the failure in-band.
    let wire_code = match code.as_str() {
        "not-found" | "conflict" | "bad-request" | "unauthorized" => "protocol-error".to_string(),
        _ => code,
    };
    let frame = EnvelopeObject::Error(ErrorEnvelope {
        id,
        code: wire_code,
        reason: Some(reason.into()),
    });
    let mut buf = BytesMut::new();
    let body = match encode_envelope(&frame, &mut buf) {
        Ok(()) => Body::from(buf.freeze()),
        Err(_) => Body::empty(),
    };
    (
        status,
        [
            ("Content-Type", FRAMES_CONTENT_TYPE),
            ("Cache-Control", "no-store"),
        ],
        body,
    )
        .into_response()
}

fn is_frames_content_type(headers: &HeaderMap) -> bool {
    headers
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim())
        .is_some_and(|v| v.eq_ignore_ascii_case(FRAMES_CONTENT_TYPE))
}

/// Reads a whole `application/rsntr-frames` body into its frames.
/// Answers 415 on the wrong content type, 413 over `max_bytes` or on an
/// oversized frame, 400 on truncated or unparseable frames.
pub(crate) async fn decode_frames_body(
    headers: &HeaderMap,
    body: Body,
    max_bytes: usize,
) -> Result<Vec<EnvelopeObject>, Response> {
    if !is_frames_content_type(headers) {
        return Err(frames_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "protocol-error",
            format!("expected Content-Type {FRAMES_CONTENT_TYPE}"),
            None,
        ));
    }
    let bytes = axum::body::to_bytes(body, max_bytes).await.map_err(|_| {
        frames_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "limit-exceeded",
            "request body exceeds the budget",
            None,
        )
    })?;
    let mut buf = BytesMut::from(&bytes[..]);
    let mut frames = Vec::new();
    loop {
        match decode_frame_eof(&mut buf) {
            Ok(None) => break,
            Ok(Some(doc)) => match EnvelopeObject::from_turtle(&doc) {
                Ok(obj) => frames.push(obj),
                Err(e) => {
                    return Err(frames_error(
                        StatusCode::BAD_REQUEST,
                        "protocol-error",
                        format!("frame is not a valid envelope: {e}"),
                        None,
                    ));
                }
            },
            Err(resonator_protocol::ProtocolError::FrameTooLarge { len }) => {
                return Err(frames_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "limit-exceeded",
                    format!("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte budget"),
                    None,
                ));
            }
            Err(e) => {
                return Err(frames_error(
                    StatusCode::BAD_REQUEST,
                    "protocol-error",
                    format!("malformed frame stream: {e}"),
                    None,
                ));
            }
        }
    }
    Ok(frames)
}

/// Reads exactly one framed envelope from the body.
async fn read_one_frame(headers: &HeaderMap, body: Body) -> Result<EnvelopeObject, Response> {
    let mut frames = decode_frames_body(headers, body, 4 + MAX_FRAME_LEN).await?;
    match (frames.len(), frames.pop()) {
        (1, Some(obj)) => Ok(obj),
        _ => Err(frames_error(
            StatusCode::BAD_REQUEST,
            "protocol-error",
            "the body must carry exactly one framed request",
            None,
        )),
    }
}

/// The status implied by the first response frame (before anything has
/// been streamed, per web-api.md section 10).
fn first_frame_status(piece: &Piece) -> StatusCode {
    match piece {
        Piece::Frame(EnvelopeObject::Denied(_)) => StatusCode::FORBIDDEN,
        Piece::Frame(EnvelopeObject::Error(e)) => error_status(&e.code),
        _ => StatusCode::OK,
    }
}

/// Streams response pieces into a frames body: frames are re-encoded
/// with the shared codec, raw pieces (media) pass through verbatim.
fn body_from_pieces(first: Option<Piece>, mut rx: mpsc::Receiver<Piece>) -> Body {
    let (btx, brx) = mpsc::channel::<Result<bytes::Bytes, std::convert::Infallible>>(8);
    tokio::spawn(async move {
        let encode = |piece: &Piece| -> Option<bytes::Bytes> {
            match piece {
                Piece::Frame(obj) => {
                    let mut buf = BytesMut::new();
                    encode_envelope(obj, &mut buf).ok()?;
                    Some(buf.freeze())
                }
                Piece::Raw(bytes) => Some(bytes::Bytes::from(bytes.clone())),
            }
        };
        if let Some(p) = first {
            match encode(&p) {
                Some(b) => {
                    if btx.send(Ok(b)).await.is_err() {
                        return;
                    }
                }
                None => return,
            }
        }
        loop {
            // Watch the body itself, not just the next send: a duplex
            // talk sink produces no downstream pieces, so waiting for a
            // send to fail would never reveal that the client left, and
            // the request would run on with nobody listening.
            let piece = tokio::select! {
                _ = btx.closed() => return,
                p = rx.recv() => p,
            };
            let Some(p) = piece else { return };
            match encode(&p) {
                Some(b) => {
                    // A dropped body (client abort) ends this task; the
                    // pipeline's next send then fails and execution
                    // stops, exactly like a reset stream.
                    if btx.send(Ok(b)).await.is_err() {
                        return;
                    }
                }
                None => return,
            }
        }
    });
    Body::from_stream(ReceiverStream::new(brx))
}

/// Common tail of both framed endpoints: peek the first piece, map the
/// status, stream the rest.
async fn respond_with_pieces(mut rx: mpsc::Receiver<Piece>) -> Response {
    let first = rx.recv().await;
    let status = first.as_ref().map_or(StatusCode::OK, first_frame_status);
    (
        status,
        [
            ("Content-Type", FRAMES_CONTENT_TYPE),
            ("Cache-Control", "no-store"),
            ("X-Accel-Buffering", "no"),
        ],
        body_from_pieces(first, rx),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(crate) struct PeerParam {
    peer: Option<String>,
}

/// Resolves the `?peer=` parameter: `Ok(None)` = serve locally.
fn parse_peer(peer: &Option<String>) -> Result<Option<PeerId>, Box<Response>> {
    match peer {
        None => Ok(None),
        Some(s) => match s.parse::<PeerId>() {
            Ok(id) => Ok(Some(id)),
            Err(e) => Err(Box::new(frames_error(
                StatusCode::BAD_REQUEST,
                "protocol-error",
                format!("invalid peer parameter: {e}"),
                None,
            ))),
        },
    }
}

/// Relays one remote exchange: forwards response frames verbatim, and
/// after an `rsntr:Media` header switches to the raw byte feed.
async fn relay_remote(
    mut stream: resonator_transport::IrohRequestStream,
    first: Option<EnvelopeObject>,
    tx: mpsc::Sender<Piece>,
) {
    let mut media = false;
    if let Some(frame) = first {
        media = matches!(frame, EnvelopeObject::Media(_));
        if tx.send(Piece::Frame(frame)).await.is_err() {
            return;
        }
    }
    loop {
        if media {
            match stream.recv_raw().await {
                Ok(Some(chunk)) => {
                    if tx.send(Piece::Raw(chunk)).await.is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => return,
            }
        } else {
            match stream.recv().await {
                Ok(Some(frame)) => {
                    media = matches!(frame, EnvelopeObject::Media(_));
                    if tx.send(Piece::Frame(frame)).await.is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => return,
            }
        }
    }
}

/// Dials `peer`, sends `envelope`, and returns the response pieces.
/// `finish_send` half-closes after the request (`/request`); `/entrain`
/// keeps the write half open so dropping the stream is the damp.
async fn spawn_remote(
    state: &WebState,
    peer: PeerId,
    envelope: EnvelopeObject,
    finish_send: bool,
) -> Result<mpsc::Receiver<Piece>, Response> {
    let Some(transport) = &state.transport else {
        return Err(frames_error(
            StatusCode::BAD_GATEWAY,
            "protocol-error",
            "this server has no network transport; only the local node is reachable",
            None,
        ));
    };
    // One retry after evicting the pooled connection: a peer that
    // restarted leaves a zombie connection that accepts the request but
    // never answers; the transport's idle timeout surfaces that as a
    // stream error within seconds, and a fresh dial then succeeds.
    // Requests are idempotent across the resend (request id + _applied),
    // and a slow-but-alive peer (a parked knock, a long query) never
    // errors, so it is never resent.
    let mut attempt = 0;
    let (stream, first) = loop {
        let step: Result<_, String> = async {
            let (mut stream, _hello) = transport
                .open(peer)
                .await
                .map_err(|e| format!("dialing {peer} failed: {e}"))?;
            stream
                .send(&envelope)
                .await
                .map_err(|e| format!("sending to {peer} failed: {e}"))?;
            if finish_send {
                stream
                    .finish()
                    .await
                    .map_err(|e| format!("half-closing to {peer} failed: {e}"))?;
            }
            let first = stream
                .recv()
                .await
                .map_err(|e| format!("no response from {peer}: {e}"))?;
            Ok((stream, first))
        }
        .await;
        match step {
            Ok(pair) => break pair,
            Err(e) if attempt == 0 => {
                attempt += 1;
                tracing::debug!(peer = %peer, error = %e, "relay attempt failed; evicting and redialing");
                transport.evict(peer).await;
            }
            Err(e) => {
                return Err(frames_error(
                    StatusCode::BAD_GATEWAY,
                    "protocol-error",
                    e,
                    None,
                ));
            }
        }
    };
    let (tx, rx) = mpsc::channel::<Piece>(8);
    tokio::spawn(relay_remote(stream, first, tx));
    Ok(rx)
}

/// Relays one remote audio-duplex exchange: downstream bytes to the
/// response body, upstream `/duplex/{id}` bytes to the wire. One task
/// owns the whole stream; the handlers write while the arms read (the
/// entrain-lane select pattern). On Fin the wire's write half closes and
/// the registry entry is dropped, so late POSTs answer 404.
async fn relay_duplex(
    mut stream: resonator_transport::IrohRequestStream,
    first: Option<EnvelopeObject>,
    tx: mpsc::Sender<Piece>,
    mut up_rx: mpsc::Receiver<UpMsg>,
    state: Arc<WebState>,
    id: String,
) {
    let is_duplex = matches!(first, Some(EnvelopeObject::AudioDuplex(_)));
    if let Some(frame) = first
        && tx.send(Piece::Frame(frame)).await.is_err()
    {
        let _ = stream.finish().await;
        state.duplex.remove(&id);
        return;
    }
    if !is_duplex {
        // Denied or Error: no upstream will flow; close our write half
        // and relay the remainder as ordinary frames.
        let _ = stream.finish().await;
        state.duplex.remove(&id);
        while let Ok(Some(frame)) = stream.recv().await {
            if tx.send(Piece::Frame(frame)).await.is_err() {
                return;
            }
        }
        return;
    }
    let mut finished = false;
    loop {
        tokio::select! {
            // The browser dropping the response body is the hangup. A
            // talk sink emits nothing downstream, so without this arm a
            // departed caller would never be noticed (the recv_raw arm
            // would wait forever for bytes that never come) and the
            // source would outlive the call.
            _ = tx.closed() => break,
            chunk = stream.recv_raw() => match chunk {
                Ok(Some(c)) => {
                    if tx.send(Piece::Raw(c)).await.is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            },
            up = up_rx.recv(), if !finished => match up {
                Some(UpMsg::Data(b)) => {
                    if stream.send_raw(&b).await.is_err() {
                        break;
                    }
                }
                Some(UpMsg::Fin) | None => {
                    let _ = stream.finish().await;
                    finished = true;
                    // Late POSTs should 404, not queue into nowhere.
                    state.duplex.remove(&id);
                }
            },
        }
    }
    state.duplex.remove(&id);
}

/// Dials `peer` for an audio-duplex exchange: like [`spawn_remote`] with
/// the write half kept open, relayed by [`relay_duplex`].
async fn spawn_remote_duplex(
    state: &Arc<WebState>,
    peer: PeerId,
    envelope: EnvelopeObject,
    up_rx: mpsc::Receiver<UpMsg>,
    id: String,
) -> Result<mpsc::Receiver<Piece>, Response> {
    let Some(transport) = &state.transport else {
        return Err(frames_error(
            StatusCode::BAD_GATEWAY,
            "protocol-error",
            "this server has no network transport; only the local node is reachable",
            None,
        ));
    };
    // One retry after evicting the pooled connection, exactly as the
    // ordinary relay does: a peer that restarted leaves a zombie
    // connection whose dial or first read fails within seconds, and a
    // fresh dial then succeeds. Opening a duplex twice is harmless -
    // the first attempt never reached the source.
    let mut attempt = 0;
    let (stream, first) = loop {
        let step: Result<_, String> = async {
            let (mut stream, _hello) = transport
                .open(peer)
                .await
                .map_err(|e| format!("dialing {peer} failed: {e}"))?;
            stream
                .send(&envelope)
                .await
                .map_err(|e| format!("sending to {peer} failed: {e}"))?;
            let first = stream
                .recv()
                .await
                .map_err(|e| format!("no response from {peer}: {e}"))?;
            Ok((stream, first))
        }
        .await;
        match step {
            Ok(pair) => break pair,
            Err(e) if attempt == 0 => {
                attempt += 1;
                tracing::debug!(peer = %peer, error = %e, "duplex dial failed; evicting and redialing");
                transport.evict(peer).await;
            }
            Err(e) => {
                return Err(frames_error(
                    StatusCode::BAD_GATEWAY,
                    "protocol-error",
                    e,
                    None,
                ));
            }
        }
    };
    let (tx, rx) = mpsc::channel::<Piece>(8);
    tokio::spawn(relay_duplex(stream, first, tx, up_rx, state.clone(), id));
    Ok(rx)
}

/// Honors a well-formed client ULID and mints one otherwise, exactly as
/// the wire pipeline does for knocks.
fn normalize_statement_id(id: &str) -> String {
    ulid::Ulid::from_string(id)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| ulid::Ulid::new().to_string())
}

/// `POST /request`: one `rsntr:Query`/`rsntr:Execute` in, the response
/// frames streamed out.
pub(crate) async fn request(
    State(state): State<Arc<WebState>>,
    Query(params): Query<PeerParam>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let peer = match parse_peer(&params.peer) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    let envelope = match read_one_frame(&headers, body).await {
        Ok(obj) => obj,
        Err(resp) => return resp,
    };
    let envelope = match envelope {
        EnvelopeObject::Query(mut st) => {
            st.id = normalize_statement_id(&st.id);
            EnvelopeObject::Query(st)
        }
        EnvelopeObject::Execute(mut st) => {
            st.id = normalize_statement_id(&st.id);
            EnvelopeObject::Execute(st)
        }
        other => {
            return frames_error(
                StatusCode::BAD_REQUEST,
                "protocol-error",
                format!("/request carries rsntr:Query or rsntr:Execute only, got {other:?}"),
                None,
            );
        }
    };

    // An audio-duplex query registers its upstream channel BEFORE the
    // response starts, so the id in the AudioDuplex header is always
    // POSTable by the time the browser reads it.
    let duplex_id = match &envelope {
        EnvelopeObject::Query(st)
            if resonator_protocol::mod_matches("audio-duplex", &st.modulation) =>
        {
            Some(st.id.clone())
        }
        _ => None,
    };

    let rx = match (peer, duplex_id) {
        (None, None) => spawn_local(
            state.node.clone(),
            state.owner,
            state.hello.clone(),
            envelope,
        ),
        (None, Some(id)) => {
            let (up_tx, up_rx) = mpsc::channel::<UpMsg>(32);
            state.duplex.insert(&id, up_tx);
            spawn_local_duplex(state.clone(), envelope, up_rx, id)
        }
        (Some(peer), None) => match spawn_remote(&state, peer, envelope, true).await {
            Ok(rx) => rx,
            Err(resp) => return resp,
        },
        (Some(peer), Some(id)) => {
            let (up_tx, up_rx) = mpsc::channel::<UpMsg>(32);
            state.duplex.insert(&id, up_tx);
            match spawn_remote_duplex(&state, peer, envelope, up_rx, id.clone()).await {
                Ok(rx) => rx,
                Err(resp) => {
                    state.duplex.remove(&id);
                    return resp;
                }
            }
        }
    };
    respond_with_pieces(rx).await
}

/// `POST /entrain`: one `rsntr:Entrain` in; Done then Vibrations out.
/// Aborting the response is the damp.
pub(crate) async fn entrain(
    State(state): State<Arc<WebState>>,
    Query(params): Query<PeerParam>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let peer = match parse_peer(&params.peer) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    let envelope = match read_one_frame(&headers, body).await {
        Ok(obj) => obj,
        Err(resp) => return resp,
    };
    let EnvelopeObject::Entrain(_) = &envelope else {
        return frames_error(
            StatusCode::BAD_REQUEST,
            "protocol-error",
            "/entrain carries one rsntr:Entrain frame",
            None,
        );
    };

    let rx = match peer {
        None => spawn_local(
            state.node.clone(),
            state.owner,
            state.hello.clone(),
            envelope,
        ),
        Some(peer) => match spawn_remote(&state, peer, envelope, false).await {
            Ok(rx) => rx,
            Err(resp) => return resp,
        },
    };
    respond_with_pieces(rx).await
}
