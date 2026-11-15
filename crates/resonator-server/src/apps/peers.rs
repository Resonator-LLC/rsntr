//! The peers of a wave app: the same app (same app id) for the same gadget,
//! run by another participant of the wave, on this node or on another
//! device. Instances on this node reach each other directly; other devices
//! are reached through a [`PeerChannel`] (the federation endpoint's
//! `resonator/app/1`), which calls back into the manager for what comes in.
//!
//! A peer's id is `<participant>` for an instance on this node and
//! `<participant>/<device key>` for one on another device.
//!
//! An app with the `media` permission also has the peer `viewer` (marked
//! `local`): the person in front of this node. Their microphone comes as
//! a datagram stream from it (one Opus frame per datagram), and what the
//! app writes on a stream it opens to it goes to the viewer's speaker
//! (a channel byte, then an Opus frame).

use async_trait::async_trait;
use resonator_apps::StreamMode;
use resonator_model::id::{ParticipantId, WaveId};

/// An instance of the same app on another device.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemotePeer {
    /// The device's key (z-base-32).
    pub device: String,
    pub participant: ParticipantId,
}

impl RemotePeer {
    pub fn id(&self) -> String {
        format!("{}/{}", self.participant.address(), self.device)
    }
}

/// What a message between two instances is about: the gadget, the app both
/// run, and the two accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub wave: WaveId,
    pub gadget: String,
    pub app: String,
    /// The account on this node.
    pub local: ParticipantId,
}

/// The sending half of a stream between two instances.
pub trait StreamTx: Send + Sync {
    /// Queues data. A reliable stream refuses data while its queue is
    /// full; a datagram stream drops it.
    fn write(&self, data: Vec<u8>) -> Result<(), String>;
    /// Ends the stream.
    fn close(&self);
}

/// Carries app traffic to other devices.
#[async_trait]
pub trait PeerChannel: Send + Sync {
    /// The instances of `route`'s app for `participants` on other
    /// devices (each told of `route.local` in turn).
    async fn discover(&self, route: &Route, participants: &[ParticipantId]) -> Vec<RemotePeer>;
    /// Tells `peers` that the instance of `route` stopped.
    async fn leave(&self, route: &Route, peers: &[RemotePeer]);
    async fn send(&self, route: &Route, to: &RemotePeer, data: Vec<u8>) -> Result<(), String>;
    /// Opens a stream to `to`; what comes back on it is handed to the
    /// manager's `stream_data` and `stream_closed` under `stream`.
    async fn open(
        &self,
        route: &Route,
        to: &RemotePeer,
        mode: StreamMode,
        stream: u64,
    ) -> Result<Box<dyn StreamTx>, String>;
}

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use resonator_apps::{Effect, Event, Peer};

use super::{AppKey, AppManager, Inner, Running, lock};

/// Where a peer runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PeerTarget {
    /// Another account's instance on this node.
    Local(ParticipantId),
    Remote(RemotePeer),
}

impl PeerTarget {
    fn id(&self) -> String {
        match self {
            PeerTarget::Local(participant) => participant.address().to_owned(),
            PeerTarget::Remote(peer) => peer.id(),
        }
    }

    fn peer(&self) -> Peer {
        let (participant, device) = match self {
            PeerTarget::Local(participant) => (participant.address().to_owned(), String::new()),
            PeerTarget::Remote(peer) => {
                (peer.participant.address().to_owned(), peer.device.clone())
            }
        };
        Peer {
            id: self.id(),
            participant,
            device,
            local: false,
        }
    }

    fn participant(&self) -> &ParticipantId {
        match self {
            PeerTarget::Local(participant) => participant,
            PeerTarget::Remote(peer) => &peer.participant,
        }
    }
}

/// The peers and streams of one instance.
#[derive(Default)]
pub(super) struct PeerState {
    pub(super) peers: HashMap<String, PeerTarget>,
    pub(super) streams: HashMap<u64, Arc<dyn StreamTx>>,
    /// The `viewer` peer, for an app with the `media` permission.
    viewer: Option<Peer>,
    /// The stream of the viewer's microphone.
    mic: Option<u64>,
}

/// The id of the peer in front of this node.
pub const VIEWER: &str = "viewer";

/// What the app writes to the viewer: frames for its speaker.
struct ViewerTx {
    signals: tokio::sync::broadcast::Sender<super::AppSignal>,
}

impl StreamTx for ViewerTx {
    fn write(&self, data: Vec<u8>) -> Result<(), String> {
        if data.len() < 2 || data.len() > 4096 {
            return Err("a speaker frame is a channel byte and up to 4095 bytes".to_owned());
        }
        // Nobody listening is no error.
        let _ = self.signals.send(super::AppSignal::Media(data));
        Ok(())
    }

    fn close(&self) {}
}

fn grants(running: &Running, permission: resonator_apps::Permission) -> bool {
    running.manifest.permissions.contains(&permission)
}

/// A stream between two instances on this node: what one writes is the
/// other's data.
struct LocalTx {
    manager: Weak<Inner>,
    to: AppKey,
    stream: u64,
}

impl StreamTx for LocalTx {
    fn write(&self, data: Vec<u8>) -> Result<(), String> {
        let manager = AppManager(self.manager.upgrade().ok_or("stopped")?);
        manager.stream_data(&self.to, self.stream, data);
        Ok(())
    }

    fn close(&self) {
        if let Some(inner) = self.manager.upgrade() {
            AppManager(inner).stream_closed(&self.to, self.stream);
        }
    }
}

/// The manager without keeping it alive, for the peer channel.
#[derive(Clone)]
pub struct WeakAppManager(Weak<Inner>);

impl WeakAppManager {
    pub fn upgrade(&self) -> Option<AppManager> {
        self.0.upgrade().map(AppManager)
    }
}

impl AppManager {
    pub fn downgrade(&self) -> WeakAppManager {
        WeakAppManager(Arc::downgrade(&self.0))
    }

    /// Installs the channel to other devices. Without one, an app's peers
    /// are the instances on this node.
    pub fn set_channel(&self, channel: Arc<dyn PeerChannel>) {
        *lock(&self.0.channel) = Some(channel);
    }

    fn channel(&self) -> Option<Arc<dyn PeerChannel>> {
        lock(&self.0.channel).clone()
    }

    fn route(key: &AppKey, app: &str) -> Route {
        Route {
            wave: key.wave.clone(),
            gadget: key.gadget.clone(),
            app: app.to_owned(),
            local: key.account.clone(),
        }
    }

    /// The running instance of `key` if it runs `app`.
    fn running_app(&self, key: &AppKey, app: &str) -> Option<Arc<Running>> {
        lock(&self.0.instances)
            .get(key)
            .filter(|running| running.app_id == app && !running.handle.is_stopped())
            .cloned()
    }

    /// The other instances of the same app for the same gadget on this node.
    fn local_twins(&self, key: &AppKey, app: &str) -> Vec<AppKey> {
        lock(&self.0.instances)
            .iter()
            .filter(|(other, running)| {
                other.wave == key.wave
                    && other.gadget == key.gadget
                    && other.account != key.account
                    && running.app_id == app
            })
            .map(|(other, _)| other.clone())
            .collect()
    }

    /// Tells the instance its peers, if they changed.
    fn update_peers(
        &self,
        running: &Running,
        change: impl FnOnce(&mut HashMap<String, PeerTarget>) -> bool,
    ) {
        let peers = {
            let mut state = lock(&running.peers);
            if !change(&mut state.peers) {
                return;
            }
            let mut peers: Vec<Peer> = state.peers.values().map(PeerTarget::peer).collect();
            peers.sort_by(|a, b| a.id.cmp(&b.id));
            peers.extend(state.viewer.clone());
            peers
        };
        if running.handle.try_send(Event::PeersChanged(peers)).is_err() {
            tracing::debug!("could not tell an app of its peers");
        }
    }

    fn add_peer(&self, running: &Running, target: PeerTarget) {
        self.update_peers(running, |peers| {
            let id = target.id();
            if peers.get(&id) == Some(&target) {
                return false;
            }
            peers.insert(id, target);
            true
        });
    }

    fn remove_peers(&self, running: &Running, gone: impl Fn(&PeerTarget) -> bool) {
        self.update_peers(running, |peers| {
            let before = peers.len();
            peers.retain(|_, target| !gone(target));
            peers.len() != before
        });
    }

    /// Finds the peers of the instance of `key`: the instances on this node
    /// and, through the channel, on other devices. Peers who left the
    /// conversation are dropped.
    pub(super) async fn discover(&self, key: &AppKey) {
        let Some(running) = lock(&self.0.instances).get(key).cloned() else {
            return;
        };
        if !running
            .manifest
            .permissions
            .iter()
            .any(|p| p.as_str() == "peers")
        {
            return;
        }
        let Some(root) = self.root_participants(&key.wave).await else {
            return;
        };
        self.remove_peers(&running, |target| !root.contains(target.participant()));
        for twin in self.local_twins(key, &running.app_id) {
            if !root.contains(&twin.account) {
                continue;
            }
            let Some(other) = self.running_app(&twin, &running.app_id) else {
                continue;
            };
            self.add_peer(&running, PeerTarget::Local(twin.account.clone()));
            self.add_peer(&other, PeerTarget::Local(key.account.clone()));
        }
        if let Some(channel) = self.channel() {
            let route = Self::route(key, &running.app_id);
            for peer in channel.discover(&route, &root).await {
                if root.contains(&peer.participant) {
                    self.add_peer(&running, PeerTarget::Remote(peer));
                }
            }
        }
    }

    /// Gives an app with the `media` permission its `viewer` peer.
    pub(super) fn announce_viewer(&self, key: &AppKey) {
        let Some(running) = lock(&self.0.instances).get(key).cloned() else {
            return;
        };
        if !grants(&running, resonator_apps::Permission::Media) {
            return;
        }
        let viewer = Peer {
            id: VIEWER.to_owned(),
            participant: key.account.address().to_owned(),
            device: String::new(),
            local: true,
        };
        lock(&running.peers).viewer = Some(viewer);
        self.update_peers(&running, |_| true);
    }

    /// A frame of the viewer's microphone, for the instance of `key`.
    pub(super) fn viewer_media(
        &self,
        key: &AppKey,
        running: &Arc<Running>,
        frame: Vec<u8>,
    ) -> Result<(), String> {
        if !self.is_current(key, running) {
            return Err("the app stopped".to_owned());
        }
        if !grants(running, resonator_apps::Permission::Media) {
            return Err("the app takes no media".to_owned());
        }
        let opened = {
            let mut state = lock(&running.peers);
            match state.mic {
                Some(stream) => (stream, false),
                None => {
                    let stream = running.next_incoming.fetch_add(1, Ordering::AcqRel);
                    // What the app writes back goes to the speaker.
                    let silent: Arc<dyn StreamTx> = Arc::new(ViewerTx {
                        signals: running.signals.clone(),
                    });
                    state.streams.insert(stream, silent);
                    state.mic = Some(stream);
                    (stream, true)
                }
            }
        };
        let (stream, fresh) = opened;
        if fresh {
            running
                .handle
                .try_send(Event::Stream {
                    stream,
                    from: VIEWER.to_owned(),
                    mode: StreamMode::Datagram,
                })
                .map_err(|e| e.to_string())?;
        }
        // A full mailbox drops the frame, as a datagram would be.
        let _ = running.handle.try_send(Event::StreamData {
            stream,
            data: frame,
        });
        Ok(())
    }

    /// Takes a stopped instance out of its peers' lists and ends its
    /// streams.
    pub(super) fn retire(&self, key: &AppKey, running: &Running) {
        let (peers, streams) = {
            let mut state = lock(&running.peers);
            (
                std::mem::take(&mut state.peers),
                std::mem::take(&mut state.streams),
            )
        };
        for stream in streams.into_values() {
            stream.close();
        }
        for twin in self.local_twins(key, &running.app_id) {
            if let Some(other) = lock(&self.0.instances).get(&twin).cloned() {
                self.remove_peers(&other, |t| *t == PeerTarget::Local(key.account.clone()));
            }
        }
        let remote: Vec<RemotePeer> = peers
            .into_values()
            .filter_map(|target| match target {
                PeerTarget::Remote(peer) => Some(peer),
                PeerTarget::Local(_) => None,
            })
            .collect();
        if let (false, Some(channel)) = (remote.is_empty(), self.channel()) {
            let route = Self::route(key, &running.app_id);
            tokio::spawn(async move { channel.leave(&route, &remote).await });
        }
    }

    // ---- what comes in from other devices ----

    /// Another device's instance of `app` says hello: it becomes a peer of
    /// the instances here, which are returned. Its participant must be in
    /// the conversation.
    pub async fn hello(
        &self,
        from: RemotePeer,
        wave: &WaveId,
        gadget: &str,
        app: &str,
    ) -> Result<Vec<ParticipantId>, String> {
        let root = self.root_participants(wave).await.ok_or("no such wave")?;
        if !root.contains(&from.participant) {
            return Err("not a participant of the wave".to_owned());
        }
        let here: Vec<(AppKey, Arc<Running>)> = lock(&self.0.instances)
            .iter()
            .filter(|(key, running)| {
                key.wave == *wave
                    && key.gadget == gadget
                    && running.app_id == app
                    && root.contains(&key.account)
                    && running
                        .manifest
                        .permissions
                        .iter()
                        .any(|p| p.as_str() == "peers")
            })
            .map(|(key, running)| (key.clone(), Arc::clone(running)))
            .collect();
        for (_, running) in &here {
            self.add_peer(running, PeerTarget::Remote(from.clone()));
        }
        Ok(here.into_iter().map(|(key, _)| key.account).collect())
    }

    /// Another device's instance stopped.
    pub fn bye(&self, from: &RemotePeer, wave: &WaveId, gadget: &str) {
        let here: Vec<Arc<Running>> = lock(&self.0.instances)
            .iter()
            .filter(|(key, _)| key.wave == *wave && key.gadget == gadget)
            .map(|(_, running)| Arc::clone(running))
            .collect();
        for running in here {
            self.remove_peers(&running, |t| *t == PeerTarget::Remote(from.clone()));
        }
    }

    /// A device can no longer be reached: its instances are no peers.
    pub fn device_gone(&self, device: &str) {
        let all: Vec<Arc<Running>> = lock(&self.0.instances).values().cloned().collect();
        for running in all {
            self.remove_peers(
                &running,
                |t| matches!(t, PeerTarget::Remote(p) if p.device == device),
            );
        }
    }

    /// The instance of `to` here for a peer `from`, if `from` is its peer.
    fn peer_instance(
        &self,
        from: &RemotePeer,
        wave: &WaveId,
        gadget: &str,
        app: &str,
        to: &ParticipantId,
    ) -> Option<(AppKey, Arc<Running>)> {
        let key = AppKey {
            account: to.clone(),
            wave: wave.clone(),
            gadget: gadget.to_owned(),
        };
        let running = self.running_app(&key, app)?;
        let known = lock(&running.peers).peers.contains_key(&from.id());
        known.then_some((key, running))
    }

    /// Data a peer sent to the instance of `to`.
    pub fn peer_data(
        &self,
        from: &RemotePeer,
        wave: &WaveId,
        gadget: &str,
        app: &str,
        to: &ParticipantId,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let (_, running) = self
            .peer_instance(from, wave, gadget, app, to)
            .ok_or("no such peer")?;
        running
            .handle
            .try_send(Event::PeerData {
                from: from.id(),
                data,
            })
            .map_err(|e| e.to_string())
    }

    /// A peer opened a stream to the instance of `to`; returns the
    /// instance and the stream's id there, for `stream_data`.
    #[allow(clippy::too_many_arguments)]
    pub fn stream_opened(
        &self,
        from: &RemotePeer,
        wave: &WaveId,
        gadget: &str,
        app: &str,
        to: &ParticipantId,
        mode: StreamMode,
        tx: Arc<dyn StreamTx>,
    ) -> Result<(AppKey, u64), String> {
        let (key, running) = self
            .peer_instance(from, wave, gadget, app, to)
            .ok_or("no such peer")?;
        if !running
            .manifest
            .permissions
            .iter()
            .any(|p| p.as_str() == "streams")
        {
            return Err("the app takes no streams".to_owned());
        }
        let stream = Self::incoming(&running, tx);
        running
            .handle
            .try_send(Event::Stream {
                stream,
                from: from.id(),
                mode,
            })
            .map_err(|e| e.to_string())?;
        Ok((key, stream))
    }

    fn incoming(running: &Running, tx: Arc<dyn StreamTx>) -> u64 {
        let stream = running.next_incoming.fetch_add(1, Ordering::AcqRel);
        lock(&running.peers).streams.insert(stream, tx);
        stream
    }

    /// Data on a stream of the instance of `key`.
    pub fn stream_data(&self, key: &AppKey, stream: u64, data: Vec<u8>) {
        let Some(running) = lock(&self.0.instances).get(key).cloned() else {
            return;
        };
        if lock(&running.peers).streams.contains_key(&stream)
            && running
                .handle
                .try_send(Event::StreamData { stream, data })
                .is_err()
        {
            tracing::debug!("an app's stream data was dropped");
        }
    }

    /// The other end closed a stream of the instance of `key`.
    pub fn stream_closed(&self, key: &AppKey, stream: u64) {
        let Some(running) = lock(&self.0.instances).get(key).cloned() else {
            return;
        };
        if lock(&running.peers).streams.remove(&stream).is_some() {
            let _ = running.handle.try_send(Event::StreamClosed { stream });
        }
    }

    // ---- what an instance asks for ----

    /// Carries out a peer effect of the instance of `key`.
    pub(super) async fn peer_effect(&self, key: &AppKey, app: &str, effect: Effect) {
        let Some(running) = lock(&self.0.instances).get(key).cloned() else {
            return;
        };
        let target = |to: &str| lock(&running.peers).peers.get(to).cloned();
        let has_viewer = lock(&running.peers).viewer.is_some();
        match effect {
            Effect::Open { to, stream, .. } if to == VIEWER && has_viewer => {
                let tx: Arc<dyn StreamTx> = Arc::new(ViewerTx {
                    signals: running.signals.clone(),
                });
                lock(&running.peers).streams.insert(stream, tx);
            }
            Effect::Send { to, data } if to == VIEWER && has_viewer => {
                let _ = ViewerTx {
                    signals: running.signals.clone(),
                }
                .write(data);
            }
            Effect::Send { to, data } => match target(&to) {
                Some(PeerTarget::Local(account)) => {
                    let twin = AppKey {
                        account,
                        ..key.clone()
                    };
                    if let Some(other) = self.running_app(&twin, app) {
                        let _ = other.handle.try_send(Event::PeerData {
                            from: key.account.address().to_owned(),
                            data,
                        });
                    }
                }
                Some(PeerTarget::Remote(peer)) => {
                    if let Some(channel) = self.channel()
                        && let Err(error) = channel.send(&Self::route(key, app), &peer, data).await
                    {
                        tracing::debug!(%error, "could not send to an app's peer");
                    }
                }
                None => tracing::debug!("an app sent to a peer that is gone"),
            },
            Effect::Open { to, mode, stream } => {
                let tx: Result<Arc<dyn StreamTx>, String> = match target(&to) {
                    Some(PeerTarget::Local(account)) => {
                        let twin = AppKey {
                            account,
                            ..key.clone()
                        };
                        match self.running_app(&twin, app) {
                            Some(other) => {
                                let back = Arc::new(LocalTx {
                                    manager: Arc::downgrade(&self.0),
                                    to: key.clone(),
                                    stream,
                                });
                                let theirs = Self::incoming(&other, back);
                                let _ = other.handle.try_send(Event::Stream {
                                    stream: theirs,
                                    from: key.account.address().to_owned(),
                                    mode,
                                });
                                Ok(Arc::new(LocalTx {
                                    manager: Arc::downgrade(&self.0),
                                    to: twin,
                                    stream: theirs,
                                }))
                            }
                            None => Err("the peer stopped".to_owned()),
                        }
                    }
                    Some(PeerTarget::Remote(peer)) => match self.channel() {
                        Some(channel) => channel
                            .open(&Self::route(key, app), &peer, mode, stream)
                            .await
                            .map(Arc::from),
                        None => Err("no peer channel".to_owned()),
                    },
                    None => Err("no such peer".to_owned()),
                };
                match tx {
                    Ok(tx) => {
                        lock(&running.peers).streams.insert(stream, tx);
                    }
                    Err(error) => {
                        tracing::debug!(%error, "could not open an app's stream");
                        let _ = running.handle.try_send(Event::StreamClosed { stream });
                    }
                }
            }
            Effect::Write { stream, data } => {
                let tx = lock(&running.peers).streams.get(&stream).cloned();
                if let Some(tx) = tx
                    && let Err(error) = tx.write(data)
                {
                    tracing::debug!(%error, "could not write to an app's stream");
                }
            }
            Effect::Close { stream } => {
                let tx = lock(&running.peers).streams.remove(&stream);
                if let Some(tx) = tx {
                    tx.close();
                }
            }
            _ => {}
        }
    }
}
