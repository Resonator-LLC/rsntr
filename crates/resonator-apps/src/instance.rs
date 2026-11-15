//! One running app: a store, its bindings, and the calls into it.

use std::time::Duration;

use wasmtime::component::{HasSelf, Linker};
use wasmtime::{Store, Trap};

use crate::bindings::WaveApp;
use crate::engine::TICK;
use crate::host::{INCOMING_STREAM_BASE, State};
use crate::{
    AppEngine, AppError, Component, Effect, Manifest, Peer, Snapshot, StreamMode, UiEvent, UiNode,
    WaveMessage,
};

/// The most nodes one render may return.
const MAX_UI_NODES: usize = 10_000;

/// How long calls may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// `init` and every event.
    pub event: Duration,
    /// `render`.
    pub render: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            event: Duration::from_millis(200),
            render: Duration::from_millis(50),
        }
    }
}

/// What an instance starts with.
#[derive(Debug, Clone)]
pub struct InstanceConfig {
    /// The app id (`sha256:...`).
    pub app_id: String,
    pub manifest: Manifest,
    /// This node's own peer entry, passed to `init`.
    pub self_peer: Peer,
    pub snapshot: Snapshot,
    pub deadlines: Deadlines,
}

/// A running app. Calls are synchronous; run it on a thread of its own
/// ([`crate::spawn`]).
pub struct Instance {
    store: Store<State>,
    app: WaveApp,
    deadlines: Deadlines,
    failure: Option<AppError>,
}

impl Instance {
    /// Instantiates the component and calls `init`. Returns the instance
    /// and the effects of `init`.
    pub fn start(
        engine: &AppEngine,
        component: &Component,
        config: InstanceConfig,
    ) -> Result<(Self, Vec<Effect>), AppError> {
        let instantiate = |e: wasmtime::Error| AppError::Instantiate(format!("{e:#}"));
        let mut linker = Linker::<State>::new(engine.engine());
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(instantiate)?;
        WaveApp::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s).map_err(instantiate)?;
        let state = State::for_manifest(&config.manifest, config.snapshot);
        let mut store = Store::new(engine.engine(), state);
        store.limiter(|s| &mut s.limits);
        set_deadline(&mut store, config.deadlines.event);
        let app = WaveApp::instantiate(&mut store, component, &linker)
            .map_err(|e| classify(e, instantiate))?;
        let mut instance = Self {
            store,
            app,
            deadlines: config.deadlines,
            failure: None,
        };
        let app_id = config.app_id;
        let peer = config.self_peer;
        let effects = instance.call(instance.deadlines.event, |app, store| {
            app.call_init(store, &app_id, &peer)
        })?;
        Ok((instance, effects))
    }

    /// New messages of the app wavelet. Messages at or below the last known
    /// sequence number are dropped.
    pub fn on_messages(&mut self, messages: Vec<WaveMessage>) -> Result<Vec<Effect>, AppError> {
        let last = self
            .store
            .data()
            .snapshot
            .messages
            .last()
            .map_or(0, |m| m.seq);
        let fresh: Vec<WaveMessage> = messages.into_iter().filter(|m| m.seq > last).collect();
        if fresh.is_empty() {
            return Ok(Vec::new());
        }
        self.store
            .data_mut()
            .snapshot
            .messages
            .extend(fresh.iter().cloned());
        self.event(|app, store| app.call_on_messages(store, &fresh))
    }

    pub fn on_peers_changed(&mut self, peers: Vec<Peer>) -> Result<Vec<Effect>, AppError> {
        self.store.data_mut().snapshot.peers = peers.clone();
        self.event(|app, store| app.call_on_peers_changed(store, &peers))
    }

    pub fn on_peer_data(&mut self, from: &str, data: &[u8]) -> Result<Vec<Effect>, AppError> {
        self.event(|app, store| app.call_on_peer_data(store, from, data))
    }

    /// A peer opened a stream. Incoming stream ids must be at or above
    /// 2^63, so they never collide with ids the app opened.
    pub fn on_stream(
        &mut self,
        stream: u64,
        from: &str,
        mode: StreamMode,
    ) -> Result<Vec<Effect>, AppError> {
        if stream < INCOMING_STREAM_BASE {
            return Err(AppError::Failed(format!(
                "incoming stream id {stream} below 2^63"
            )));
        }
        self.store.data_mut().streams.insert(stream);
        self.event(|app, store| app.call_on_stream(store, stream, from, mode))
    }

    pub fn on_stream_data(&mut self, stream: u64, data: &[u8]) -> Result<Vec<Effect>, AppError> {
        self.event(|app, store| app.call_on_stream_data(store, stream, data))
    }

    pub fn on_stream_closed(&mut self, stream: u64) -> Result<Vec<Effect>, AppError> {
        self.store.data_mut().streams.remove(&stream);
        self.event(|app, store| app.call_on_stream_closed(store, stream))
    }

    pub fn on_ui(&mut self, view: u32, event: &UiEvent) -> Result<Vec<Effect>, AppError> {
        self.event(|app, store| app.call_on_ui(store, view, event))
    }

    /// Renders a view. Effects queued during `render` (an app should queue
    /// none) are dropped.
    pub fn render(&mut self, view: u32) -> Result<Vec<UiNode>, AppError> {
        let mut nodes = Vec::new();
        self.call(self.deadlines.render, |app, store| {
            nodes = app.call_render(store, view)?;
            Ok(())
        })?;
        check_tree(&nodes)?;
        Ok(nodes)
    }

    /// Why the instance failed, if it did. A failed instance takes no more
    /// calls.
    pub fn failure(&self) -> Option<&AppError> {
        self.failure.as_ref()
    }

    /// The snapshot as the app sees it now (with its own kv writes).
    pub fn snapshot(&self) -> &Snapshot {
        &self.store.data().snapshot
    }

    fn event(
        &mut self,
        f: impl FnOnce(&WaveApp, &mut Store<State>) -> wasmtime::Result<()>,
    ) -> Result<Vec<Effect>, AppError> {
        self.call(self.deadlines.event, f)
    }

    fn call(
        &mut self,
        budget: Duration,
        f: impl FnOnce(&WaveApp, &mut Store<State>) -> wasmtime::Result<()>,
    ) -> Result<Vec<Effect>, AppError> {
        if let Some(failure) = &self.failure {
            return Err(AppError::Failed(failure.to_string()));
        }
        set_deadline(&mut self.store, budget);
        let result = f(&self.app, &mut self.store);
        let effects = self.store.data_mut().take_effects();
        match result {
            Ok(()) => Ok(effects),
            Err(e) => {
                let error = classify(e, |e| AppError::Trap(format!("{e:#}")));
                self.failure = Some(error.clone());
                Err(error)
            }
        }
    }
}

fn set_deadline(store: &mut Store<State>, budget: Duration) {
    // One tick more, since the current tick may be nearly over.
    let ticks = budget.as_millis().div_ceil(TICK.as_millis()) as u64 + 1;
    store.set_epoch_deadline(ticks);
}

fn classify(e: wasmtime::Error, other: impl FnOnce(wasmtime::Error) -> AppError) -> AppError {
    if e.downcast_ref::<Trap>() == Some(&Trap::Interrupt) {
        AppError::Timeout
    } else {
        other(e)
    }
}

/// A render must be a forest in document order: each parent comes before
/// its children.
fn check_tree(nodes: &[UiNode]) -> Result<(), AppError> {
    if nodes.len() > MAX_UI_NODES {
        return Err(AppError::BadRender(format!(
            "more than {MAX_UI_NODES} nodes"
        )));
    }
    for (i, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.parent
            && parent as usize >= i
        {
            return Err(AppError::BadRender(format!(
                "node {i} has parent {parent}, not an earlier node"
            )));
        }
    }
    Ok(())
}
