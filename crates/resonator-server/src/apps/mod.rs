//! Wave apps on this node: finds a gadget's app, checks the grant and the
//! bundle, runs one instance per account, wave and gadget (with the
//! `resonator-apps` runtime) and carries out what it asks for.
//!
//! An instance starts when someone opens the app ([`AppManager::open`])
//! and stops `[apps] idle_secs` after its last viewer left, when the
//! gadget, the grant or the account's place in the wave goes away, or when
//! the node goes to the background. Messages it missed meanwhile stay in
//! the app wavelet.

mod peers;
mod wavelet;

pub use peers::{PeerChannel, RemotePeer, Route, StreamTx, WeakAppManager};
pub use resonator_apps::StreamMode;
pub use wavelet::{app_wavelet, gadget_of, is_app_wavelet};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use resonator_apps::{
    AppEngine, AppError, Bundle, Component, Deadlines, Effect, EngineOptions, Event, Instance,
    InstanceConfig, InstanceHandle, LogLevel, Manifest, Peer, Report, Snapshot, UiEvent, UiNode,
};
use resonator_model::data::WaveletData;
use resonator_model::doc::DocView;
use resonator_model::id::constants::CONVERSATION_ROOT_WAVELET;
use resonator_model::id::util::build_user_data_wavelet_id;
use resonator_model::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::supplement::{ReadablePrimitiveSupplement, UserDataSupplement};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};

use crate::attachment::{AttachmentId, AttachmentService};
use crate::config::AppsConfig;
use crate::storage::{AppKvScope, AppKvStore};
use crate::waveserver::{WaveBusEvent, WaveServer, WaveletProvider};

/// The `m/gadgets` key whose value allows a gadget's app: the app id
/// (`sha256:<hex>`) the user allowed.
pub const GRANT_KEY: &str = "allowed";

/// The tag and attributes of a gadget with an app.
const GADGET_TAG: &str = "gadget";
const APP_ATTR: &str = "app";
const BUNDLE_ATTR: &str = "bundle";
const ID_ATTR: &str = "id";

/// Signals sent to the viewers of an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppSignal {
    /// The app asked for a new render of a view.
    Invalidate(u32),
    /// The instance failed; it takes no more events.
    Failed(String),
    /// The instance was stopped (gadget, grant or participant gone).
    Stopped,
    /// A frame the app sent to the viewer (a channel byte, then Opus).
    Media(Vec<u8>),
}

/// Why an app does not run.
#[derive(Debug, Clone, Error)]
pub enum OpenError {
    #[error("no such wave or gadget")]
    NotFound,
    #[error("the gadget has no app")]
    NotAnApp,
    #[error("not a participant of the wave")]
    NotParticipant,
    /// The user has not allowed this app; the manifest says what it asks
    /// for.
    #[error("the app is not allowed")]
    NeedsGrant {
        app: String,
        manifest: Box<Manifest>,
    },
    #[error("the bundle is not available: {0}")]
    BundleMissing(String),
    #[error("the bundle does not match the gadget's app")]
    HashMismatch,
    #[error("too many apps are running")]
    TooMany,
    #[error("the app failed: {0}")]
    Failed(String),
}

/// One running instance's key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppKey {
    pub account: ParticipantId,
    pub wave: WaveId,
    pub gadget: String,
}

struct Running {
    app_id: String,
    manifest: Manifest,
    handle: InstanceHandle,
    signals: broadcast::Sender<AppSignal>,
    viewers: AtomicUsize,
    /// The sequence number of the last message of the app wavelet handed
    /// to the instance.
    delivered: AtomicU64,
    /// When the last viewer left; `None` while someone views it.
    idle_since: Mutex<Option<Instant>>,
    /// Its peers and open streams.
    peers: Mutex<peers::PeerState>,
    /// The id of the next stream a peer opens to it (2^63 and up).
    next_incoming: AtomicU64,
    /// The task applying the instance's effects.
    effects: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.effects.abort();
    }
}

struct Inner {
    engine: Arc<AppEngine>,
    server: WaveServer,
    attachments: AttachmentService,
    kv: AppKvStore,
    config: AppsConfig,
    instances: Mutex<HashMap<AppKey, Arc<Running>>>,
    /// Compiled components by app id.
    components: Mutex<HashMap<String, Component>>,
    /// Starts one at a time, so two viewers do not start two instances.
    starting: tokio::sync::Mutex<()>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Reaches the same apps on other devices.
    channel: Mutex<Option<Arc<dyn PeerChannel>>>,
}

/// Runs the wave apps of this node; cheap to clone.
#[derive(Clone)]
pub struct AppManager(Arc<Inner>);

impl std::fmt::Debug for AppManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppManager")
            .field("running", &self.running())
            .finish()
    }
}

/// What a viewer of an open app holds. Dropping it leaves the app.
pub struct AppSession {
    manager: AppManager,
    key: AppKey,
    running: Arc<Running>,
}

impl std::fmt::Debug for AppSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppSession")
            .field("key", &self.key)
            .finish()
    }
}

impl AppSession {
    pub fn app_id(&self) -> &str {
        &self.running.app_id
    }

    pub fn manifest(&self) -> &Manifest {
        &self.running.manifest
    }

    /// Signals from now on.
    pub fn signals(&self) -> broadcast::Receiver<AppSignal> {
        self.running.signals.subscribe()
    }

    pub async fn render(&self, view: u32) -> Result<Vec<UiNode>, AppError> {
        self.running.handle.render(view).await
    }

    pub async fn event(&self, view: u32, event: UiEvent) -> Result<(), AppError> {
        self.running.handle.send(Event::Ui { view, event }).await
    }

    /// Whether the instance still runs.
    /// Hands a frame of the viewer's microphone to the app (it needs the
    /// `media` permission).
    pub fn media(&self, frame: Vec<u8>) -> Result<(), String> {
        self.manager.viewer_media(&self.key, &self.running, frame)
    }

    pub fn is_running(&self) -> bool {
        !self.running.handle.is_stopped() && self.manager.is_current(&self.key, &self.running)
    }
}

impl Drop for AppSession {
    fn drop(&mut self) {
        if self.running.viewers.fetch_sub(1, Ordering::AcqRel) == 1 {
            *lock(&self.running.idle_since) = Some(Instant::now());
        }
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A gadget with an app, as found in a conversation's documents.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GadgetApp {
    app: String,
    bundle: String,
}

/// Finds the gadget `gadget` in the documents of `wavelet`.
fn find_gadget(wavelet: &WaveletData, gadget: &str) -> Option<Option<GadgetApp>> {
    for document in wavelet.documents() {
        let view = DocView::new(document.content());
        for element in view.descendant_elements(view.root()) {
            if !view.has_tag(element, GADGET_TAG)
                || view.attribute(element, ID_ATTR) != Some(gadget)
            {
                continue;
            }
            let app = view.attribute(element, APP_ATTR).map(str::to_owned);
            let bundle = view.attribute(element, BUNDLE_ATTR).map(str::to_owned);
            return Some(match (app, bundle) {
                (Some(app), Some(bundle)) => Some(GadgetApp { app, bundle }),
                _ => None,
            });
        }
    }
    None
}

impl AppManager {
    /// Builds the manager and starts its background tasks. Must be called
    /// within a tokio runtime.
    pub fn new(
        config: AppsConfig,
        cache_dir: Option<PathBuf>,
        server: WaveServer,
        attachments: AttachmentService,
        kv: AppKvStore,
    ) -> Result<Self, AppError> {
        let engine = AppEngine::new(EngineOptions {
            cache_dir,
            pulley: false,
        })?;
        let manager = AppManager(Arc::new(Inner {
            engine: Arc::new(engine),
            server,
            attachments,
            kv,
            config,
            instances: Mutex::new(HashMap::new()),
            components: Mutex::new(HashMap::new()),
            starting: tokio::sync::Mutex::new(()),
            tasks: Mutex::new(Vec::new()),
            channel: Mutex::new(None),
        }));
        let watcher = tokio::spawn(manager.clone().watch());
        let reaper = tokio::spawn(manager.clone().reap());
        lock(&manager.0.tasks).extend([watcher, reaper]);
        Ok(manager)
    }

    /// Instances running now.
    pub fn running(&self) -> usize {
        lock(&self.0.instances).len()
    }

    /// Whether an instance runs for `key`.
    pub fn is_running(&self, key: &AppKey) -> bool {
        lock(&self.0.instances).contains_key(key)
    }

    fn is_current(&self, key: &AppKey, running: &Arc<Running>) -> bool {
        lock(&self.0.instances)
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, running))
    }

    /// Opens the app of `gadget` in `wave` for `account`, starting it if it
    /// does not run.
    pub async fn open(
        &self,
        account: &ParticipantId,
        wave: &WaveId,
        gadget: &str,
    ) -> Result<AppSession, OpenError> {
        let key = AppKey {
            account: account.clone(),
            wave: wave.clone(),
            gadget: gadget.to_owned(),
        };
        let _starting = self.0.starting.lock().await;
        let running = match self.current(&key).await {
            Some(running) => running,
            None => self.start(&key).await?,
        };
        running.viewers.fetch_add(1, Ordering::AcqRel);
        *lock(&running.idle_since) = None;
        Ok(AppSession {
            manager: self.clone(),
            key,
            running,
        })
    }

    /// The running instance of `key`, if it is still allowed to run.
    async fn current(&self, key: &AppKey) -> Option<Arc<Running>> {
        let running = lock(&self.0.instances).get(key).cloned()?;
        if running.handle.is_stopped() || self.allowed(key).await.as_ref() != Some(&running.app_id)
        {
            self.stop(key, AppSignal::Stopped);
            return None;
        }
        Some(running)
    }

    /// The app id `key` may run: the gadget exists with an app, and the
    /// account is a participant and allowed it.
    async fn allowed(&self, key: &AppKey) -> Option<String> {
        let gadget = self.gadget(key).await.ok()?;
        (self.grant(key).await? == gadget.app).then_some(gadget.app)
    }

    fn root(wave: &WaveId) -> Result<WaveletName, OpenError> {
        let root = WaveletId::of(wave.domain(), CONVERSATION_ROOT_WAVELET)
            .map_err(|_| OpenError::NotFound)?;
        Ok(WaveletName::of(wave.clone(), root))
    }

    /// The gadget's app, if the account is a participant of the wave.
    async fn gadget(&self, key: &AppKey) -> Result<GadgetApp, OpenError> {
        let found = self
            .0
            .server
            .with_wavelet(&Self::root(&key.wave)?, |data| {
                if !data.participants().contains(&key.account) {
                    return Err(OpenError::NotParticipant);
                }
                match find_gadget(data, &key.gadget) {
                    None => Err(OpenError::NotFound),
                    Some(None) => Err(OpenError::NotAnApp),
                    Some(Some(app)) => Ok(app),
                }
            })
            .await
            .map_err(|_| OpenError::NotFound)?;
        found.unwrap_or(Err(OpenError::NotFound))
    }

    /// The app id the account allowed for the gadget.
    async fn grant(&self, key: &AppKey) -> Option<String> {
        let user_data = build_user_data_wavelet_id(&key.account).ok()?;
        let name = WaveletName::of(key.wave.clone(), user_data);
        self.0
            .server
            .with_wavelet(&name, |data| {
                UserDataSupplement::new(data)
                    .gadget_state(&key.gadget)
                    .remove(GRANT_KEY)
            })
            .await
            .ok()
            .flatten()
            .flatten()
    }

    /// The bundle of a gadget, from the attachments here or fetched from
    /// the wave's other servers; checked against the gadget's app id.
    async fn bundle(&self, key: &AppKey, gadget: &GadgetApp) -> Result<Bundle, OpenError> {
        let missing = |e: String| OpenError::BundleMissing(e);
        let id = AttachmentId::deserialise(&gadget.bundle).map_err(|e| missing(e.to_string()))?;
        let attachments = &self.0.attachments;
        let mut bytes = attachments
            .read(&id)
            .await
            .map_err(|e| missing(e.to_string()))?;
        if bytes.is_none() && attachments.is_fetchable(&id) {
            attachments
                .fetch_remote(&id, &Self::root(&key.wave)?)
                .await
                .map_err(|e| missing(e.to_string()))?;
            bytes = attachments
                .read(&id)
                .await
                .map_err(|e| missing(e.to_string()))?;
        }
        let bytes = bytes.ok_or_else(|| missing("not found".to_owned()))?;
        let bundle = Bundle::from_zip(&bytes).map_err(|e| missing(e.to_string()))?;
        if bundle.id() != gadget.app {
            return Err(OpenError::HashMismatch);
        }
        Ok(bundle)
    }

    async fn start(&self, key: &AppKey) -> Result<Arc<Running>, OpenError> {
        let gadget = self.gadget(key).await?;
        let bundle = self.bundle(key, &gadget).await?;
        if self.grant(key).await.as_deref() != Some(gadget.app.as_str()) {
            return Err(OpenError::NeedsGrant {
                app: gadget.app,
                manifest: Box::new(bundle.manifest),
            });
        }
        if self.running() >= self.0.config.max_instances {
            return Err(OpenError::TooMany);
        }
        let failed = |e: AppError| OpenError::Failed(e.to_string());
        let scope = AppKvScope {
            account: key.account.address().to_owned(),
            wave: key.wave.to_string(),
            gadget: key.gadget.clone(),
        };
        let kv = self
            .0
            .kv
            .load(&scope)
            .await
            .map_err(|e| OpenError::Failed(e.to_string()))?;
        let messages = self.messages(key, 0).await;
        let delivered = messages.last().map_or(0, |m| m.seq);
        let component = self
            .component(&gadget.app, bundle.wasm)
            .await
            .map_err(failed)?;
        let manifest = bundle.manifest;
        let config = InstanceConfig {
            app_id: gadget.app.clone(),
            manifest: manifest.clone(),
            self_peer: Peer {
                id: "local".to_owned(),
                participant: key.account.address().to_owned(),
                device: String::new(),
                local: true,
            },
            snapshot: Snapshot {
                kv,
                messages,
                ..Snapshot::default()
            },
            deadlines: Deadlines::default(),
        };
        let engine = Arc::clone(&self.0.engine);
        let name = manifest.name.clone();
        let (instance, effects) =
            tokio::task::spawn_blocking(move || Instance::start(&engine, &component, config))
                .await
                .map_err(|e| OpenError::Failed(e.to_string()))?
                .map_err(failed)?;
        let (handle, reports) = resonator_apps::spawn(&name, instance).map_err(failed)?;
        let (signals, _) = broadcast::channel(256);
        let applier = Applier {
            manager: self.clone(),
            key: key.clone(),
            scope,
            app: gadget.app.clone(),
            name: name.clone(),
            signals: signals.clone(),
        };
        let effects_task = tokio::spawn(applier.run(effects, reports));
        let running = Arc::new(Running {
            app_id: gadget.app,
            manifest,
            handle,
            signals,
            viewers: AtomicUsize::new(0),
            delivered: AtomicU64::new(delivered),
            idle_since: Mutex::new(Some(Instant::now())),
            peers: Mutex::new(peers::PeerState::default()),
            next_incoming: AtomicU64::new(1 << 63),
            effects: effects_task,
        });
        lock(&self.0.instances).insert(key.clone(), Arc::clone(&running));
        tracing::info!(app = %name, wave = %key.wave, gadget = %key.gadget, "started app");
        // Messages that came while it started, and the participants of the
        // app wavelet as they should be.
        let manager = self.clone();
        let started = key.clone();
        tokio::spawn(async move {
            manager.deliver(&started).await;
            manager.mirror(&started).await;
            manager.announce_viewer(&started);
            manager.discover(&started).await;
        });
        Ok(running)
    }

    /// The compiled component of an app, compiled once per node run (and
    /// cached on disk by the engine).
    async fn component(&self, app: &str, wasm: Vec<u8>) -> Result<Component, AppError> {
        if let Some(component) = lock(&self.0.components).get(app) {
            return Ok(component.clone());
        }
        let engine = Arc::clone(&self.0.engine);
        let component = tokio::task::spawn_blocking(move || engine.compile(&wasm))
            .await
            .map_err(|e| AppError::Compile(e.to_string()))??;
        lock(&self.0.components).insert(app.to_owned(), component.clone());
        Ok(component)
    }

    /// The messages of the app wavelet of `key` after `since`, if the
    /// account may read it.
    async fn messages(&self, key: &AppKey, since: u64) -> Vec<resonator_apps::WaveMessage> {
        let Some(name) = app_wavelet(&key.wave, &key.gadget) else {
            return Vec::new();
        };
        self.0
            .server
            .with_wavelet(&name, |data| {
                if data.participants().contains(&key.account) {
                    wavelet::messages(data, since)
                } else {
                    Vec::new()
                }
            })
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Hands the instance of `key` the messages it has not seen.
    async fn deliver(&self, key: &AppKey) {
        let Some(running) = lock(&self.0.instances).get(key).cloned() else {
            return;
        };
        let since = running.delivered.load(Ordering::Acquire);
        let messages = self.messages(key, since).await;
        let Some(last) = messages.last().map(|m| m.seq) else {
            return;
        };
        // Two deliveries may race; the instance drops what it has seen.
        running.delivered.fetch_max(last, Ordering::AcqRel);
        if let Err(error) = running.handle.send(Event::Messages(messages)).await {
            tracing::debug!(wave = %key.wave, gadget = %key.gadget, %error, "could not hand messages to an app");
        }
    }

    /// The participants of the conversation of `key`.
    async fn root_participants(&self, wave: &WaveId) -> Option<Vec<ParticipantId>> {
        self.0
            .server
            .with_wavelet(&Self::root(wave).ok()?, |data| data.participants().to_vec())
            .await
            .ok()
            .flatten()
    }

    /// Posts a message of the app of `key` to its app wavelet, creating it
    /// with the conversation's participants if need be.
    async fn post(&self, key: &AppKey, kind: &str, body: &str) -> Result<(), String> {
        let name = app_wavelet(&key.wave, &key.gadget)
            .ok_or_else(|| "the gadget id cannot name an app wavelet".to_owned())?;
        let root = self
            .root_participants(&key.wave)
            .await
            .ok_or_else(|| "the wave is gone".to_owned())?;
        if !root.contains(&key.account) {
            return Err("not a participant of the wave".to_owned());
        }
        // A wavelet created meanwhile by another node: the second try sees it.
        let mut last_error = String::new();
        for _ in 0..2 {
            let current = self
                .0
                .server
                .get_snapshot(&name)
                .await
                .map_err(|e| e.to_string())?
                .map(|committed| committed.snapshot);
            let delta = wavelet::post_delta(
                &name,
                current,
                &root,
                &key.account,
                kind,
                body,
                now_millis(),
            )?;
            match self.0.server.submit_delta(&name, &delta).await {
                Ok(_) => return Ok(()),
                Err(error) => last_error = error.to_string(),
            }
        }
        Err(last_error)
    }

    /// Brings the participants of the app wavelet of `key` up to date with
    /// the conversation's, as the instance's account if it is one of them.
    async fn mirror(&self, key: &AppKey) {
        let Some(name) = app_wavelet(&key.wave, &key.gadget) else {
            return;
        };
        let Some(root) = self.root_participants(&key.wave).await else {
            return;
        };
        let Ok(Some(current)) = self.0.server.get_snapshot(&name).await else {
            return;
        };
        if let Some(delta) =
            wavelet::mirror_delta(current.snapshot, &root, &key.account, now_millis())
            && let Err(error) = self.0.server.submit_delta(&name, &delta).await
        {
            tracing::debug!(wave = %key.wave, gadget = %key.gadget, %error, "could not update the app wavelet's participants");
        }
    }

    /// Stops the instance of `key`, telling its viewers why.
    fn stop(&self, key: &AppKey, signal: AppSignal) {
        let removed = lock(&self.0.instances).remove(key);
        if let Some(running) = removed {
            tracing::info!(wave = %key.wave, gadget = %key.gadget, "stopped app");
            self.retire(key, &running);
            let _ = running.signals.send(signal);
        }
    }

    /// Stops every instance (for the node going to the background); they
    /// start again when opened.
    pub fn stop_all(&self) {
        let keys: Vec<AppKey> = lock(&self.0.instances).keys().cloned().collect();
        for key in keys {
            self.stop(&key, AppSignal::Stopped);
        }
    }

    /// Stops every instance and the background tasks.
    pub fn shutdown(&self) {
        for task in lock(&self.0.tasks).drain(..) {
            task.abort();
        }
        self.stop_all();
    }

    /// Stops instances whose gadget, grant or participant went away.
    async fn watch(self) {
        let (id, mut events) = self.0.server.bus().subscribe_channel();
        while let Some(event) = events.recv().await {
            let WaveBusEvent::Update { wavelet_name, .. } = event else {
                continue;
            };
            if let Some(gadget) = gadget_of(&wavelet_name.wavelet_id) {
                let listening: Vec<AppKey> = lock(&self.0.instances)
                    .keys()
                    .filter(|key| key.wave == wavelet_name.wave_id && key.gadget == gadget)
                    .cloned()
                    .collect();
                for key in listening {
                    self.deliver(&key).await;
                }
                continue;
            }
            let root_changed =
                Self::root(&wavelet_name.wave_id).is_ok_and(|root| root == wavelet_name);
            let affected: Vec<AppKey> = lock(&self.0.instances)
                .keys()
                .filter(|key| key.wave == wavelet_name.wave_id)
                .cloned()
                .collect();
            for key in affected {
                let app = lock(&self.0.instances).get(&key).map(|r| r.app_id.clone());
                if let Some(app) = app
                    && self.allowed(&key).await != Some(app)
                {
                    self.stop(&key, AppSignal::Stopped);
                } else if root_changed {
                    // Off the watcher: the submit may wait for another node.
                    let manager = self.clone();
                    tokio::spawn(async move {
                        manager.mirror(&key).await;
                        manager.discover(&key).await;
                    });
                }
            }
        }
        self.0.server.bus().unsubscribe(id);
    }

    /// Stops instances nobody viewed for `idle_secs`.
    async fn reap(self) {
        let idle = Duration::from_secs(self.0.config.idle_secs);
        let every = (idle / 2).clamp(Duration::from_millis(250), Duration::from_secs(10));
        let mut ticker = tokio::time::interval(every);
        loop {
            ticker.tick().await;
            let expired: Vec<AppKey> = lock(&self.0.instances)
                .iter()
                .filter(|(_, running)| {
                    lock(&running.idle_since).is_some_and(|since| since.elapsed() >= idle)
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in expired {
                self.stop(&key, AppSignal::Stopped);
            }
        }
    }
}

/// Carries out the effects of one instance, in order.
struct Applier {
    manager: AppManager,
    key: AppKey,
    scope: AppKvScope,
    app: String,
    name: String,
    signals: broadcast::Sender<AppSignal>,
}

impl Applier {
    async fn run(self, first: Vec<Effect>, mut reports: mpsc::UnboundedReceiver<Report>) {
        self.apply(first).await;
        while let Some(report) = reports.recv().await {
            self.apply(report.effects).await;
            if let Some(failure) = report.failure {
                tracing::warn!(app = %self.name, %failure, "app failed");
                let _ = self.signals.send(AppSignal::Failed(failure.to_string()));
                let removed = {
                    let mut instances = lock(&self.manager.0.instances);
                    if instances
                        .get(&self.key)
                        .is_some_and(|running| running.signals.same_channel(&self.signals))
                    {
                        instances.remove(&self.key)
                    } else {
                        None
                    }
                };
                if let Some(running) = removed {
                    self.manager.retire(&self.key, &running);
                }
                return;
            }
        }
    }

    async fn apply(&self, effects: Vec<Effect>) {
        let mut kv: Vec<(String, Option<Vec<u8>>)> = Vec::new();
        for effect in effects {
            match effect {
                Effect::KvSet { key, value } => kv.push((key, Some(value))),
                Effect::KvDelete { key } => kv.push((key, None)),
                Effect::Invalidate { view } => {
                    let _ = self.signals.send(AppSignal::Invalidate(view));
                }
                Effect::Log { level, message } => match level {
                    LogLevel::Debug | LogLevel::Info => {
                        tracing::debug!(app = %self.name, "{message}")
                    }
                    LogLevel::Warn | LogLevel::Error => {
                        tracing::info!(app = %self.name, "{message}")
                    }
                },
                Effect::Post { kind, body } => {
                    if let Err(error) = self.manager.post(&self.key, &kind, &body).await {
                        tracing::info!(app = %self.name, %error, "could not post the app's message");
                    }
                }
                effect @ (Effect::Send { .. }
                | Effect::Open { .. }
                | Effect::Write { .. }
                | Effect::Close { .. }) => {
                    self.manager.peer_effect(&self.key, &self.app, effect).await;
                }
            }
        }
        if let Err(error) = self.manager.0.kv.apply(&self.scope, kv).await {
            tracing::warn!(app = %self.name, %error, "could not store the app's kv");
        }
    }
}
