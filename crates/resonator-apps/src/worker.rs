//! Runs an [`Instance`] on a thread of its own, fed by a mailbox.

use std::thread;

use tokio::sync::{mpsc, oneshot};

use crate::{AppError, Effect, Instance, Peer, StreamMode, UiEvent, UiNode, WaveMessage};

/// Events a mailbox holds before senders wait.
const MAILBOX: usize = 256;

/// Something for the app to handle.
#[derive(Debug)]
pub enum Event {
    Messages(Vec<WaveMessage>),
    PeersChanged(Vec<Peer>),
    PeerData {
        from: String,
        data: Vec<u8>,
    },
    Stream {
        stream: u64,
        from: String,
        mode: StreamMode,
    },
    StreamData {
        stream: u64,
        data: Vec<u8>,
    },
    StreamClosed {
        stream: u64,
    },
    Ui {
        view: u32,
        event: UiEvent,
    },
    Render {
        view: u32,
        reply: oneshot::Sender<Result<Vec<UiNode>, AppError>>,
    },
}

/// What one event produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub effects: Vec<Effect>,
    /// Set when the event failed the instance; no more reports follow.
    pub failure: Option<AppError>,
}

/// Sends events to a running instance. Dropping every handle stops it.
#[derive(Debug, Clone)]
pub struct InstanceHandle {
    mailbox: mpsc::Sender<Event>,
}

impl InstanceHandle {
    /// Queues an event, waiting while the mailbox is full.
    pub async fn send(&self, event: Event) -> Result<(), AppError> {
        self.mailbox
            .send(event)
            .await
            .map_err(|_| AppError::Stopped)
    }

    /// Queues an event if there is room (for datagrams: a full mailbox
    /// drops them).
    pub fn try_send(&self, event: Event) -> Result<(), AppError> {
        self.mailbox.try_send(event).map_err(|_| AppError::Stopped)
    }

    pub async fn render(&self, view: u32) -> Result<Vec<UiNode>, AppError> {
        let (reply, answer) = oneshot::channel();
        self.send(Event::Render { view, reply }).await?;
        answer.await.map_err(|_| AppError::Stopped)?
    }

    pub fn is_stopped(&self) -> bool {
        self.mailbox.is_closed()
    }
}

/// Runs the instance on its own thread. Reports of every event that
/// produced effects or failed come out of the returned receiver; the
/// thread ends when every handle is dropped or the instance fails.
pub fn spawn(
    name: &str,
    mut instance: Instance,
) -> Result<(InstanceHandle, mpsc::UnboundedReceiver<Report>), AppError> {
    let (mailbox, mut events) = mpsc::channel(MAILBOX);
    let (reports, out) = mpsc::unbounded_channel();
    thread::Builder::new()
        .name(format!("app {name}"))
        .spawn(move || {
            while let Some(event) = events.blocking_recv() {
                let result = match event {
                    Event::Render { view, reply } => {
                        let rendered = instance.render(view);
                        let failed = instance.failure().cloned();
                        let _ = reply.send(rendered);
                        match failed {
                            Some(f) => Err(f),
                            None => Ok(Vec::new()),
                        }
                    }
                    Event::Messages(m) => instance.on_messages(m),
                    Event::PeersChanged(p) => instance.on_peers_changed(p),
                    Event::PeerData { from, data } => instance.on_peer_data(&from, &data),
                    Event::Stream { stream, from, mode } => instance.on_stream(stream, &from, mode),
                    Event::StreamData { stream, data } => instance.on_stream_data(stream, &data),
                    Event::StreamClosed { stream } => instance.on_stream_closed(stream),
                    Event::Ui { view, event } => instance.on_ui(view, &event),
                };
                match result {
                    Ok(effects) if effects.is_empty() => {}
                    Ok(effects) => {
                        let _ = reports.send(Report {
                            effects,
                            failure: None,
                        });
                    }
                    Err(AppError::BadRender(_)) => {}
                    Err(failure) => {
                        let _ = reports.send(Report {
                            effects: Vec::new(),
                            failure: Some(failure),
                        });
                        break;
                    }
                }
            }
        })
        .map_err(|e| AppError::Engine(format!("app thread: {e}")))?;
    Ok((InstanceHandle { mailbox }, out))
}
