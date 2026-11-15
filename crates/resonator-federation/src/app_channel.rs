//! Wave apps across devices: connections on ALPN `resonator/app/1`
//! between instances of the same app (same app id) for the same gadget.
//!
//! Every stream starts with a header frame (a 4-byte big-endian length,
//! then JSON) naming the wave, the gadget, the app and the two accounts.
//! The connection's TLS proves the remote device key; the header's `from`
//! account must be of a domain that key speaks for (a server's key, or a
//! certified device of a person). The manager then checks that `from` is
//! a participant of the wave and that `to` runs the same app.
//!
//! - `hello` (bi): the answer lists the accounts running the app there;
//!   both ends then know each other as peers.
//! - `bye` (uni): the instance stopped.
//! - `send` (uni): one data frame.
//! - `open` (bi): a stream. Reliable: data frames both ways, the end of
//!   the stream closes it. Datagram: QUIC datagrams on the same
//!   connection, each an 8-byte big-endian tag then the data; the opener
//!   numbers the stream `n` (in the header) and sends with tag `2n`, the
//!   other end with `2n + 1`. The other end answers one byte on the bi
//!   stream once it takes the datagrams; the opener sends none before.
//!   Otherwise the bi stream only marks the datagram stream's lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use iroh::PublicKey;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use resonator_model::id::{IdSerialiser, ModernIdSerialiser, ParticipantId, WaveId};
use resonator_server::apps::{
    AppKey, PeerChannel, RemotePeer, Route, StreamMode, StreamTx, WeakAppManager,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::endpoint::{AppService, FederationEndpoint, PeerAuthority};
use crate::trust::Trust;

const MAX_HEADER: usize = 16 * 1024;
/// Most bytes of one `send` or one stream frame.
const MAX_DATA: usize = 1 << 20;
/// Frames queued on one reliable stream.
const STREAM_QUEUE: usize = 64;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const TAG_BYTES: usize = 8;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Op {
    Hello,
    Bye,
    Send,
    Open,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Reliable,
    Datagram,
}

#[derive(Serialize, Deserialize, Debug)]
struct Header {
    op: Op,
    wave: String,
    gadget: String,
    app: String,
    from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<Mode>,
    /// A datagram stream's number on this connection, chosen by its opener.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    datagram: Option<u64>,
}

#[derive(Serialize, Deserialize, Default)]
struct HelloReply {
    #[serde(default)]
    participants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<(), String> {
    let length = u32::try_from(bytes.len()).map_err(|_| "frame too large".to_owned())?;
    send.write_all(&length.to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    send.write_all(bytes).await.map_err(|e| e.to_string())
}

/// Reads one frame; `None` at the end of the stream.
async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Option<Vec<u8>>, String> {
    let mut prefix = [0u8; 4];
    match recv.read_exact(&mut prefix).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let length = u32::from_be_bytes(prefix) as usize;
    if length > max {
        return Err(format!("a frame of {length} bytes"));
    }
    let mut bytes = vec![0u8; length];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(bytes))
}

fn mode_of(mode: StreamMode) -> Mode {
    match mode {
        StreamMode::Reliable => Mode::Reliable,
        StreamMode::Datagram => Mode::Datagram,
    }
}

/// The sending half of a reliable stream: a queue and the task writing it.
struct ReliableTx {
    queue: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
}

impl ReliableTx {
    fn start(mut send: SendStream) -> Self {
        let (queue, mut frames) = mpsc::channel::<Vec<u8>>(STREAM_QUEUE);
        tokio::spawn(async move {
            while let Some(data) = frames.recv().await {
                if write_frame(&mut send, &data).await.is_err() {
                    return;
                }
            }
            let _ = send.finish();
        });
        ReliableTx {
            queue: Mutex::new(Some(queue)),
        }
    }
}

impl StreamTx for ReliableTx {
    fn write(&self, data: Vec<u8>) -> Result<(), String> {
        if data.len() > MAX_DATA {
            return Err("too large".to_owned());
        }
        match &*lock(&self.queue) {
            Some(queue) => queue.try_send(data).map_err(|e| e.to_string()),
            None => Err("closed".to_owned()),
        }
    }

    fn close(&self) {
        lock(&self.queue).take();
    }
}

/// The sending half of a datagram stream. QUIC drops the oldest datagrams
/// when they cannot be sent fast enough.
struct DatagramTx {
    connection: Connection,
    tag: u64,
    closed: std::sync::atomic::AtomicBool,
    lifetime: Mutex<Option<SendStream>>,
}

impl DatagramTx {
    fn new(connection: Connection, tag: u64, lifetime: Option<SendStream>) -> Self {
        DatagramTx {
            connection,
            tag,
            closed: std::sync::atomic::AtomicBool::new(false),
            lifetime: Mutex::new(lifetime),
        }
    }
}

impl StreamTx for DatagramTx {
    fn write(&self, data: Vec<u8>) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("closed".to_owned());
        }
        let max = self
            .connection
            .max_datagram_size()
            .ok_or_else(|| "the peer takes no datagrams".to_owned())?;
        if data.len() + TAG_BYTES > max {
            return Err(format!("a datagram over {max} bytes"));
        }
        let mut bytes = Vec::with_capacity(TAG_BYTES + data.len());
        bytes.extend_from_slice(&self.tag.to_be_bytes());
        bytes.extend_from_slice(&data);
        self.connection
            .send_datagram(bytes.into())
            .map_err(|e| e.to_string())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(mut send) = lock(&self.lifetime).take() {
            let _ = send.finish();
        }
    }
}

struct Inner {
    endpoint: FederationEndpoint,
    trust: Trust,
    apps: WeakAppManager,
    own: PublicKey,
    /// The connections this node opened, by key.
    outbound: tokio::sync::Mutex<HashMap<PublicKey, Connection>>,
    /// Every open app connection, by stable id.
    live: Mutex<HashMap<usize, PublicKey>>,
    /// Keys met, by their z-base-32 form (a peer's device).
    keys: Mutex<HashMap<String, PublicKey>>,
    /// (key, domain) pairs found authorised.
    authorised: Mutex<std::collections::HashSet<(PublicKey, String)>>,
    /// Incoming datagram tags by (connection, tag): the instance and its
    /// stream.
    datagrams: Mutex<HashMap<(usize, u64), (AppKey, u64)>>,
    next_datagram: AtomicU64,
}

/// The wave app channel of a node; cheap to clone.
#[derive(Clone)]
pub struct AppChannel(Arc<Inner>);

impl AppChannel {
    pub fn new(endpoint: FederationEndpoint, trust: Trust, apps: WeakAppManager) -> Self {
        AppChannel(Arc::new(Inner {
            own: endpoint.id(),
            endpoint,
            trust,
            apps,
            outbound: tokio::sync::Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashMap::new()),
            authorised: Mutex::new(std::collections::HashSet::new()),
            datagrams: Mutex::new(HashMap::new()),
            next_datagram: AtomicU64::new(1),
        }))
    }
}

/// What an incoming header says, checked.
struct Admitted {
    header: Header,
    wave: WaveId,
    from: RemotePeer,
    to: Option<ParticipantId>,
}

impl Inner {
    fn device(&self, key: PublicKey) -> String {
        let device = key.to_z32();
        lock(&self.keys).insert(device.clone(), key);
        device
    }

    fn key_of(&self, peer: &RemotePeer) -> Result<PublicKey, String> {
        lock(&self.keys)
            .get(&peer.device)
            .copied()
            .ok_or_else(|| "unknown device".to_owned())
    }

    /// Whether `key` speaks for `domain`.
    async fn speaks_for(&self, key: PublicKey, domain: &str) -> bool {
        if self.trust.is_revoked(&key) {
            return false;
        }
        let pair = (key, domain.to_owned());
        if lock(&self.authorised).contains(&pair) {
            return true;
        }
        let ok = PeerAuthority::authorise(&self.trust, &key, domain, &[])
            .await
            .is_authorised();
        if ok {
            lock(&self.authorised).insert(pair);
        }
        ok
    }

    async fn admit(&self, key: PublicKey, bytes: &[u8]) -> Result<Admitted, String> {
        let header: Header = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let wave = ModernIdSerialiser
            .deserialise_wave_id(&header.wave)
            .map_err(|e| e.to_string())?;
        let from = ParticipantId::of(&header.from).map_err(|e| e.to_string())?;
        if !self.speaks_for(key, from.domain()).await {
            return Err(format!("the device does not speak for {}", from.domain()));
        }
        let to = header
            .to
            .as_deref()
            .map(ParticipantId::of)
            .transpose()
            .map_err(|e| e.to_string())?;
        Ok(Admitted {
            header,
            wave,
            from: RemotePeer {
                device: self.device(key),
                participant: from,
            },
            to,
        })
    }

    fn header(route: &Route, op: Op) -> Result<Header, String> {
        Ok(Header {
            op,
            wave: ModernIdSerialiser
                .serialise_wave_id(&route.wave)
                .map_err(|e| e.to_string())?,
            gadget: route.gadget.clone(),
            app: route.app.clone(),
            from: route.local.address().to_owned(),
            to: None,
            mode: None,
            datagram: None,
        })
    }

    /// Serves an app connection, whichever end opened it, until it closes.
    async fn run(self: Arc<Self>, connection: Connection) {
        let key = connection.remote_id();
        let id = connection.stable_id();
        lock(&self.live).insert(id, key);
        loop {
            tokio::select! {
                bi = connection.accept_bi() => match bi {
                    Ok((send, recv)) => {
                        let inner = Arc::clone(&self);
                        let connection = connection.clone();
                        tokio::spawn(async move {
                            if let Err(error) = inner.incoming_bi(key, connection, send, recv).await {
                                tracing::debug!(%error, "refused an app stream");
                            }
                        });
                    }
                    Err(_) => break,
                },
                uni = connection.accept_uni() => match uni {
                    Ok(recv) => {
                        let inner = Arc::clone(&self);
                        tokio::spawn(async move {
                            if let Err(error) = inner.incoming_uni(key, recv).await {
                                tracing::debug!(%error, "refused an app message");
                            }
                        });
                    }
                    Err(_) => break,
                },
                datagram = connection.read_datagram() => match datagram {
                    Ok(bytes) => self.datagram(id, &bytes),
                    Err(_) => break,
                },
            }
        }
        lock(&self.live).remove(&id);
        lock(&self.datagrams).retain(|(connection, _), _| *connection != id);
        let still = lock(&self.live).values().any(|k| *k == key);
        if !still && let Some(apps) = self.apps.upgrade() {
            apps.device_gone(&key.to_z32());
        }
    }

    fn datagram(&self, connection: usize, bytes: &[u8]) {
        if bytes.len() < TAG_BYTES {
            return;
        }
        let (tag, data) = bytes.split_at(TAG_BYTES);
        let tag = u64::from_be_bytes(tag.try_into().unwrap_or_default());
        let target = lock(&self.datagrams).get(&(connection, tag)).cloned();
        if let (Some((key, stream)), Some(apps)) = (target, self.apps.upgrade()) {
            apps.stream_data(&key, stream, data.to_vec());
        }
    }

    async fn incoming_uni(&self, key: PublicKey, mut recv: RecvStream) -> Result<(), String> {
        let header = read_frame(&mut recv, MAX_HEADER)
            .await?
            .ok_or("no header")?;
        let Admitted {
            header,
            wave,
            from,
            to,
        } = self.admit(key, &header).await?;
        let apps = self.apps.upgrade().ok_or("apps stopped")?;
        match header.op {
            Op::Bye => apps.bye(&from, &wave, &header.gadget),
            Op::Send => {
                let to = to.ok_or("no addressee")?;
                let data = read_frame(&mut recv, MAX_DATA).await?.unwrap_or_default();
                apps.peer_data(&from, &wave, &header.gadget, &header.app, &to, data)?;
            }
            Op::Hello | Op::Open => return Err("not on a uni stream".to_owned()),
        }
        Ok(())
    }

    async fn incoming_bi(
        &self,
        key: PublicKey,
        connection: Connection,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Result<(), String> {
        let header = read_frame(&mut recv, MAX_HEADER)
            .await?
            .ok_or("no header")?;
        let Admitted {
            header,
            wave,
            from,
            to,
        } = self.admit(key, &header).await?;
        let apps = self.apps.upgrade().ok_or("apps stopped")?;
        match header.op {
            Op::Hello => {
                let reply = match apps.hello(from, &wave, &header.gadget, &header.app).await {
                    Ok(here) => HelloReply {
                        participants: here.iter().map(|p| p.address().to_owned()).collect(),
                        error: None,
                    },
                    Err(error) => HelloReply {
                        participants: Vec::new(),
                        error: Some(error),
                    },
                };
                let bytes = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
                write_frame(&mut send, &bytes).await?;
                let _ = send.finish();
                Ok(())
            }
            Op::Open => {
                let to = to.ok_or("no addressee")?;
                let mut datagram_tx = None;
                let mut ack = None;
                let (mode, tx): (StreamMode, Arc<dyn StreamTx>) = match header.mode {
                    Some(Mode::Datagram) => {
                        let n = header.datagram.ok_or("no datagram number")?;
                        let tx = Arc::new(DatagramTx::new(
                            connection.clone(),
                            n.wrapping_mul(2).wrapping_add(1),
                            None,
                        ));
                        datagram_tx = Some(Arc::clone(&tx));
                        ack = Some(send);
                        (StreamMode::Datagram, tx)
                    }
                    _ => (StreamMode::Reliable, Arc::new(ReliableTx::start(send))),
                };
                let (instance, stream) =
                    apps.stream_opened(&from, &wave, &header.gadget, &header.app, &to, mode, tx)?;
                if let Some(n) = header.datagram.filter(|_| mode == StreamMode::Datagram) {
                    lock(&self.datagrams).insert(
                        (connection.stable_id(), n.wrapping_mul(2)),
                        (instance.clone(), stream),
                    );
                }
                if let (Some(tx), Some(mut send)) = (datagram_tx, ack) {
                    // The datagrams are taken now.
                    send.write_all(&[1]).await.map_err(|e| e.to_string())?;
                    *lock(&tx.lifetime) = Some(send);
                }
                drop(apps);
                self.pump(recv, mode, connection.stable_id(), instance, stream)
                    .await;
                Ok(())
            }
            Op::Bye | Op::Send => Err("not on a bi stream".to_owned()),
        }
    }

    /// Hands what comes on a stream to the instance until the stream ends.
    async fn pump(
        &self,
        mut recv: RecvStream,
        mode: StreamMode,
        connection: usize,
        instance: AppKey,
        stream: u64,
    ) {
        match mode {
            StreamMode::Reliable => {
                while let Ok(Some(data)) = read_frame(&mut recv, MAX_DATA).await {
                    match self.apps.upgrade() {
                        Some(apps) => apps.stream_data(&instance, stream, data),
                        None => return,
                    }
                }
            }
            StreamMode::Datagram => {
                let _ = recv.read_to_end(0).await;
                lock(&self.datagrams)
                    .retain(|(c, _), (k, s)| !(*c == connection && *k == instance && *s == stream));
            }
        }
        if let Some(apps) = self.apps.upgrade() {
            apps.stream_closed(&instance, stream);
        }
    }

    /// The connection to `key` this node opened, opening it if need be.
    async fn connection(self: &Arc<Self>, key: PublicKey) -> Result<Connection, String> {
        let mut outbound = self.outbound.lock().await;
        if let Some(connection) = outbound.get(&key)
            && connection.close_reason().is_none()
        {
            return Ok(connection.clone());
        }
        // The federation Hello first: it hands the other end this device's
        // certificate, by which it knows whom the key speaks for.
        if let Err(error) = self.endpoint.connect(key, None).await {
            tracing::debug!(%error, "no federation hello before an app connection");
        }
        let connection = self
            .endpoint
            .connect_app(key)
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(Arc::clone(self).run(connection.clone()));
        outbound.insert(key, connection.clone());
        Ok(connection)
    }

    /// Says hello to the instances on `key`; returns the accounts there.
    async fn hello(self: &Arc<Self>, route: &Route, key: PublicKey) -> Result<Vec<String>, String> {
        let connection = self.connection(key).await?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
        let header =
            serde_json::to_vec(&Self::header(route, Op::Hello)?).map_err(|e| e.to_string())?;
        write_frame(&mut send, &header).await?;
        let _ = send.finish();
        let reply = read_frame(&mut recv, MAX_HEADER)
            .await?
            .ok_or("no answer")?;
        let reply: HelloReply = serde_json::from_slice(&reply).map_err(|e| e.to_string())?;
        match reply.error {
            Some(error) => Err(error),
            None => Ok(reply.participants),
        }
    }

    async fn uni(
        self: &Arc<Self>,
        key: PublicKey,
        header: Header,
        data: Option<&[u8]>,
    ) -> Result<(), String> {
        let connection = self.connection(key).await?;
        let mut send = connection.open_uni().await.map_err(|e| e.to_string())?;
        let header = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
        write_frame(&mut send, &header).await?;
        if let Some(data) = data {
            write_frame(&mut send, data).await?;
        }
        send.finish().map_err(|e| e.to_string())
    }
}

#[async_trait]
impl AppService for AppChannel {
    async fn serve(&self, connection: Connection) {
        Arc::clone(&self.0).run(connection).await;
    }
}

#[async_trait]
impl PeerChannel for AppChannel {
    async fn discover(&self, route: &Route, participants: &[ParticipantId]) -> Vec<RemotePeer> {
        let inner = &self.0;
        // The devices that may run the app for each participant: a
        // person's devices, a server's key.
        let mut wanted: HashMap<PublicKey, Vec<ParticipantId>> = HashMap::new();
        for participant in participants {
            let domain = participant.domain();
            let mut keys = inner.trust.devices_of(domain);
            if let Ok(Ok(Some(key))) =
                tokio::time::timeout(HELLO_TIMEOUT, inner.trust.resolve(domain)).await
            {
                keys.push(key);
            }
            for key in keys {
                if key != inner.own {
                    let list = wanted.entry(key).or_default();
                    if !list.contains(participant) {
                        list.push(participant.clone());
                    }
                }
            }
        }
        let hellos = wanted.into_iter().map(|(key, wanted)| async move {
            let answered = match tokio::time::timeout(HELLO_TIMEOUT, inner.hello(route, key)).await
            {
                Ok(Ok(answered)) => answered,
                Ok(Err(error)) => {
                    tracing::debug!(%error, "no app hello");
                    return Vec::new();
                }
                Err(_) => return Vec::new(),
            };
            let device = inner.device(key);
            let mut peers = Vec::new();
            for participant in wanted {
                if answered.iter().any(|a| a == participant.address())
                    && inner.speaks_for(key, participant.domain()).await
                {
                    peers.push(RemotePeer {
                        device: device.clone(),
                        participant,
                    });
                }
            }
            peers
        });
        futures_util::future::join_all(hellos)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    async fn leave(&self, route: &Route, peers: &[RemotePeer]) {
        let mut keys: Vec<PublicKey> = peers.iter().filter_map(|p| self.0.key_of(p).ok()).collect();
        keys.sort_by_key(|k| *k.as_bytes());
        keys.dedup();
        for key in keys {
            let Ok(header) = Inner::header(route, Op::Bye) else {
                return;
            };
            let _ = tokio::time::timeout(HELLO_TIMEOUT, self.0.uni(key, header, None)).await;
        }
    }

    async fn send(&self, route: &Route, to: &RemotePeer, data: Vec<u8>) -> Result<(), String> {
        if data.len() > MAX_DATA {
            return Err("too large".to_owned());
        }
        let key = self.0.key_of(to)?;
        let mut header = Inner::header(route, Op::Send)?;
        header.to = Some(to.participant.address().to_owned());
        self.0.uni(key, header, Some(&data)).await
    }

    async fn open(
        &self,
        route: &Route,
        to: &RemotePeer,
        mode: StreamMode,
        stream: u64,
    ) -> Result<Box<dyn StreamTx>, String> {
        let inner = &self.0;
        let key = inner.key_of(to)?;
        let connection = inner.connection(key).await?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
        let mut header = Inner::header(route, Op::Open)?;
        header.to = Some(to.participant.address().to_owned());
        header.mode = Some(mode_of(mode));
        let n = inner.next_datagram.fetch_add(1, Ordering::Relaxed);
        if mode == StreamMode::Datagram {
            header.datagram = Some(n);
        }
        let bytes = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
        write_frame(&mut send, &bytes).await?;
        let instance = AppKey {
            account: route.local.clone(),
            wave: route.wave.clone(),
            gadget: route.gadget.clone(),
        };
        let tx: Box<dyn StreamTx> = match mode {
            StreamMode::Datagram => {
                lock(&inner.datagrams).insert(
                    (connection.stable_id(), n.wrapping_mul(2).wrapping_add(1)),
                    (instance.clone(), stream),
                );
                // Wait until the other end takes the datagrams.
                let mut ack = [0u8; 1];
                let acked = tokio::time::timeout(HELLO_TIMEOUT, recv.read_exact(&mut ack)).await;
                if !matches!(acked, Ok(Ok(()))) {
                    lock(&inner.datagrams).retain(|_, (k, s)| !(*k == instance && *s == stream));
                    return Err("the peer did not take the stream".to_owned());
                }
                Box::new(DatagramTx::new(
                    connection.clone(),
                    n.wrapping_mul(2),
                    Some(send),
                ))
            }
            StreamMode::Reliable => Box::new(ReliableTx::start(send)),
        };
        let pump = Arc::clone(inner);
        let id = connection.stable_id();
        tokio::spawn(async move { pump.pump(recv, mode, id, instance, stream).await });
        Ok(tx)
    }
}
