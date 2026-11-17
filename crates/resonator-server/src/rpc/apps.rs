//! Wave apps on the client socket: `AppOpenRequest` streams `AppUpdate`s
//! (status, then the rendered view after every invalidation) until the
//! client cancels it or the app stops; `AppEventRequest` hands a UI event
//! to an app this connection has open.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use resonator_apps::{UiEvent, UiNode};
use resonator_model::id::{IdSerialiser, ModernIdSerialiser, ParticipantId, WaveId};
use resonator_model::proto::app::{
    AppEventRequest, AppEventResponse, AppMediaRequest, AppOpenRequest, AppPair, AppRtcRequest,
    AppRtcResponse, AppRtcSlot, AppUiNode, AppUpdate,
};
use resonator_model::proto::rpc::RpcFinished;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use super::Outbox;
use crate::apps::rtc::{RtcEvent, RtcHub, RtcLink};
use crate::apps::{AppManager, AppSession, AppSignal, OpenError};

/// Most app views open at a time on one connection.
const MAX_OPEN: usize = 32;

/// An open app view: the task streaming it, and its session once the app
/// runs. Dropping it ends the stream.
pub(super) struct AppView {
    wave: WaveId,
    gadget: String,
    session: Arc<Mutex<Option<Arc<AppSession>>>>,
    /// A WebRTC connection for the view's media, to its stream task.
    links: mpsc::Sender<RtcLink>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AppView {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl AppView {
    pub(super) fn is_closed(&self) -> bool {
        self.task.is_finished()
    }
}

fn status(status: &str) -> AppUpdate {
    AppUpdate {
        status: Some(status.to_owned()),
        ..AppUpdate::default()
    }
}

fn nodes(nodes: Vec<UiNode>) -> Vec<AppUiNode> {
    nodes
        .into_iter()
        .map(|node| AppUiNode {
            parent: node.parent.and_then(|p| i32::try_from(p).ok()),
            tag: Some(node.tag),
            text: node.text,
            attrs: node
                .attrs
                .into_iter()
                .map(|(name, value)| AppPair {
                    name: Some(name),
                    value: Some(value),
                })
                .collect(),
            on: node.on,
        })
        .collect()
}

fn permissions(manifest: &resonator_apps::Manifest) -> Vec<String> {
    manifest
        .permissions
        .iter()
        .map(|p| p.as_str().to_owned())
        .collect()
}

/// Opens the view; the reply goes out on `seq`. Returns the view to keep,
/// or `None` after failing the request.
pub(super) fn open(
    apps: Option<&AppManager>,
    open: &mut HashMap<i32, AppView>,
    outbox: &Arc<Outbox>,
    seq: i32,
    account: &ParticipantId,
    request: AppOpenRequest,
) {
    let Some(apps) = apps else {
        outbox.fail(seq, "Wave apps are off on this server");
        return;
    };
    open.retain(|_, view| !view.is_closed());
    if open.contains_key(&seq) {
        outbox.fail(seq, "sequence number of an open RPC reused");
        return;
    }
    if open.len() >= MAX_OPEN {
        outbox.fail(seq, "Too many open apps on this connection");
        return;
    }
    let wave =
        match ModernIdSerialiser.deserialise_wave_id(request.wave_id.as_deref().unwrap_or("")) {
            Ok(wave) => wave,
            Err(error) => {
                outbox.fail(seq, error.to_string());
                return;
            }
        };
    let gadget = request.gadget_id.unwrap_or_default();
    let view = u32::try_from(request.view.unwrap_or(0)).unwrap_or(0);
    let session = Arc::new(Mutex::new(None));
    let (links, link_receiver) = mpsc::channel(2);
    let task = tokio::spawn(stream(
        apps.clone(),
        Arc::clone(outbox),
        seq,
        account.clone(),
        wave.clone(),
        gadget.clone(),
        view,
        Arc::clone(&session),
        link_receiver,
    ));
    open.insert(
        seq,
        AppView {
            wave,
            gadget,
            session,
            links,
            task,
        },
    );
}

#[allow(clippy::too_many_arguments)]
async fn stream(
    apps: AppManager,
    outbox: Arc<Outbox>,
    seq: i32,
    account: ParticipantId,
    wave: WaveId,
    gadget: String,
    view: u32,
    slot: Arc<Mutex<Option<Arc<AppSession>>>>,
    mut links: mpsc::Receiver<RtcLink>,
) {
    outbox.send(seq, &status("loading"));
    let session = match apps.open(&account, &wave, &gadget).await {
        Ok(session) => Arc::new(session),
        Err(OpenError::NeedsGrant { app, manifest }) => {
            outbox.send(
                seq,
                &AppUpdate {
                    status: Some("needs-grant".to_owned()),
                    app_id: Some(app),
                    name: Some(manifest.name.clone()),
                    permissions: permissions(&manifest),
                    ..AppUpdate::default()
                },
            );
            outbox.send(seq, &RpcFinished::success());
            return;
        }
        Err(error) => {
            outbox.send(
                seq,
                &AppUpdate {
                    status: Some("failed".to_owned()),
                    reason: Some(error.to_string()),
                    ..AppUpdate::default()
                },
            );
            outbox.send(seq, &RpcFinished::success());
            return;
        }
    };
    let mut signals = session.signals();
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&session));
    let running = |nodes_: Vec<UiNode>| AppUpdate {
        status: Some("running".to_owned()),
        app_id: Some(session.app_id().to_owned()),
        name: Some(session.manifest().name.clone()),
        permissions: permissions(session.manifest()),
        nodes: nodes(nodes_),
        ..AppUpdate::default()
    };
    let end = |state: &str, reason: Option<String>| {
        outbox.send(
            seq,
            &AppUpdate {
                status: Some(state.to_owned()),
                reason,
                ..AppUpdate::default()
            },
        );
        outbox.send(seq, &RpcFinished::success());
    };
    let media = |frames: Vec<Vec<u8>>, video: Vec<Vec<u8>>| {
        if frames.is_empty() && video.is_empty() {
            return;
        }
        let engine = base64::engine::general_purpose::STANDARD;
        outbox.send(
            seq,
            &AppUpdate {
                status: Some("media".to_owned()),
                media: frames.iter().map(|f| engine.encode(f)).collect(),
                video: video.iter().map(|f| engine.encode(f)).collect(),
                ..AppUpdate::default()
            },
        );
    };
    // The view's WebRTC connection, and whether media flows on it.
    let mut link: Option<RtcLink> = None;
    let mut connected = false;
    let slots = |slots: Vec<(String, u8)>| {
        outbox.send(
            seq,
            &AppUpdate {
                status: Some("slots".to_owned()),
                slots: slots
                    .into_iter()
                    .map(|(mid, channel)| AppRtcSlot {
                        mid: Some(mid),
                        channel: Some(i32::from(channel)),
                    })
                    .collect(),
                ..AppUpdate::default()
            },
        );
    };
    let mut render = true;
    loop {
        if std::mem::take(&mut render) {
            match session.render(view).await {
                Ok(rendered) => {
                    outbox.send(seq, &running(rendered));
                }
                // A bad render keeps the last one; a dead instance ends
                // through its signal.
                Err(resonator_apps::AppError::BadRender(reason)) => {
                    tracing::debug!(%reason, "app rendered a bad tree");
                }
                Err(error) => return end("failed", Some(error.to_string())),
            }
        }
        let mut frames = Vec::new();
        let mut video: Vec<Vec<u8>> = Vec::new();
        let signal = tokio::select! {
            signal = signals.recv() => signal,
            new = links.recv() => {
                if let Some(new) = new {
                    link = Some(new);
                    connected = false;
                }
                continue;
            }
            event = next_event(&mut link) => {
                match event {
                    Some(RtcEvent::Connected) => connected = true,
                    Some(RtcEvent::Slots(list)) => slots(list),
                    None => {
                        link = None;
                        connected = false;
                    }
                }
                continue;
            }
        };
        match signal {
            Ok(AppSignal::Invalidate(v)) => render = v == view,
            Ok(AppSignal::Media(frame)) => frames.push(frame),
            Ok(AppSignal::Video(frame)) => video.push(frame),
            Err(RecvError::Lagged(_)) => render = true,
            Ok(AppSignal::Failed(reason)) => return end("failed", Some(reason)),
            Ok(AppSignal::Stopped) | Err(RecvError::Closed) => return end("stopped", None),
        }
        // Coalesce invalidations that came meanwhile (and media, up to a
        // message of reasonable size).
        let mut bytes = video.iter().map(Vec::len).sum::<usize>();
        while bytes < MAX_MEDIA_BATCH
            && let Ok(signal) = signals.try_recv()
        {
            match signal {
                AppSignal::Invalidate(v) => render |= v == view,
                AppSignal::Media(frame) => {
                    bytes += frame.len();
                    frames.push(frame);
                }
                AppSignal::Video(frame) => {
                    bytes += frame.len();
                    video.push(frame);
                }
                AppSignal::Failed(reason) => return end("failed", Some(reason)),
                AppSignal::Stopped => return end("stopped", None),
            }
        }
        match &link {
            Some(link) if connected => {
                for frame in frames {
                    link.send(frame, false);
                }
                for frame in video {
                    link.send(frame, true);
                }
            }
            _ => media(frames, video),
        }
    }
}

/// The next event of the view's WebRTC connection, if it has one.
async fn next_event(link: &mut Option<RtcLink>) -> Option<RtcEvent> {
    match link {
        Some(link) => link.event().await,
        None => std::future::pending().await,
    }
}

/// Answers the viewer's WebRTC offer for an app open on this connection
/// and hands the connection to the view's stream.
pub(super) async fn rtc(
    hub: Option<&RtcHub>,
    open: &HashMap<i32, AppView>,
    outbox: &Outbox,
    seq: i32,
    request: AppRtcRequest,
) {
    let answer = |answer: Option<String>, error: Option<String>| {
        outbox.send(
            seq,
            &AppRtcResponse {
                answer,
                error_message: error,
            },
        );
    };
    let Some(hub) = hub else {
        return answer(None, Some("WebRTC is off on this server".to_owned()));
    };
    let Ok(wave) = ModernIdSerialiser.deserialise_wave_id(request.wave_id.as_deref().unwrap_or(""))
    else {
        return answer(None, Some("invalid wave id".to_owned()));
    };
    let gadget = request.gadget_id.unwrap_or_default();
    let found = open
        .values()
        .filter(|view| view.wave == wave && view.gadget == gadget && !view.is_closed())
        .find_map(|view| {
            let session = view
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()?;
            Some((session, view.links.clone()))
        });
    let Some((session, links)) = found else {
        return answer(None, Some("the app is not open".to_owned()));
    };
    let permissions = &session.manifest().permissions;
    if !permissions.contains(&resonator_apps::Permission::Media)
        && !permissions.contains(&resonator_apps::Permission::Camera)
        && !permissions.contains(&resonator_apps::Permission::Video)
    {
        return answer(None, Some("the app has no media".to_owned()));
    }
    match hub
        .accept(request.offer.as_deref().unwrap_or(""), session)
        .await
    {
        Ok((sdp, link)) => {
            if links.send(link).await.is_err() {
                return answer(None, Some("the app is not open".to_owned()));
            }
            answer(Some(sdp), None);
        }
        Err(error) => answer(None, Some(error)),
    }
}

/// Most bytes of one microphone frame (Opus frames are far smaller).
const MAX_MEDIA_FRAME: usize = 4096;
/// Most bytes of media frames in one update, before base64.
const MAX_MEDIA_BATCH: usize = 512 * 1024;

/// Hands a frame of the viewer's microphone to an app open on this
/// connection.
pub(super) fn media(
    open: &HashMap<i32, AppView>,
    outbox: &Outbox,
    seq: i32,
    request: AppMediaRequest,
) {
    let answer = |error: Option<String>| {
        outbox.send(
            seq,
            &AppEventResponse {
                error_message: error,
            },
        );
    };
    let Ok(wave) = ModernIdSerialiser.deserialise_wave_id(request.wave_id.as_deref().unwrap_or(""))
    else {
        return answer(Some("invalid wave id".to_owned()));
    };
    let gadget = request.gadget_id.unwrap_or_default();
    let Ok(frame) =
        base64::engine::general_purpose::STANDARD.decode(request.data.as_deref().unwrap_or(""))
    else {
        return answer(Some("invalid frame".to_owned()));
    };
    let video = request.video.unwrap_or(false);
    let limit = if video {
        crate::apps::MAX_VIDEO_FRAME
    } else {
        MAX_MEDIA_FRAME
    };
    if frame.len() < 1 + usize::from(video) || frame.len() > limit {
        return answer(Some("invalid frame".to_owned()));
    }
    let session = open
        .values()
        .filter(|view| view.wave == wave && view.gadget == gadget)
        .find_map(|view| {
            view.session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
    match session {
        Some(session) => answer(session.media(frame, video).err()),
        None => answer(Some("the app is not open".to_owned())),
    }
}

/// Hands a UI event to an app open on this connection.
pub(super) async fn event(
    open: &HashMap<i32, AppView>,
    outbox: &Outbox,
    seq: i32,
    request: AppEventRequest,
) {
    let fail = |message: String| {
        outbox.send(
            seq,
            &AppEventResponse {
                error_message: Some(message),
            },
        );
    };
    let Ok(wave) = ModernIdSerialiser.deserialise_wave_id(request.wave_id.as_deref().unwrap_or(""))
    else {
        return fail("invalid wave id".to_owned());
    };
    let gadget = request.gadget_id.unwrap_or_default();
    let session = open
        .values()
        .filter(|view| view.wave == wave && view.gadget == gadget)
        .find_map(|view| {
            view.session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
    let Some(session) = session else {
        return fail("the app is not open".to_owned());
    };
    let event = UiEvent {
        node: u32::try_from(request.node.unwrap_or(0)).unwrap_or(0),
        name: request.name.unwrap_or_default(),
        value: request.value,
        fields: request
            .fields
            .into_iter()
            .map(|pair| {
                (
                    pair.name.unwrap_or_default(),
                    pair.value.unwrap_or_default(),
                )
            })
            .collect(),
    };
    let view = u32::try_from(request.view.unwrap_or(0)).unwrap_or(0);
    match session.event(view, event).await {
        Ok(()) => {
            outbox.send(
                seq,
                &AppEventResponse {
                    error_message: None,
                },
            );
        }
        Err(error) => fail(error.to_string()),
    }
}
