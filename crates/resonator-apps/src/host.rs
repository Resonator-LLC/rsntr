//! What the host gives an app: the imports of `wave-app`, served from a
//! [`Snapshot`], with every write queued as an [`Effect`].

use std::collections::{BTreeMap, HashSet};

use wasmtime::component::ResourceTable;
use wasmtime::{StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::bindings::resonator::app::{kv, log, peers, types, ui, wave};
use crate::{Manifest, Peer, Permission, StreamMode, WaveMessage};

pub use crate::bindings::resonator::app::log::Level as LogLevel;

const DENIED: &str = "permission denied";
/// Effects one call may queue.
const MAX_EFFECTS: usize = 4096;
/// Bytes one call may queue in effects (posts, kv values, peer data).
const MAX_EFFECT_BYTES: usize = 16 << 20;
const MAX_KEY_BYTES: usize = 256;
const MAX_VALUE_BYTES: usize = 1 << 20;
/// All kv values of one instance together.
const MAX_KV_BYTES: usize = 16 << 20;
const MAX_POST_BYTES: usize = 64 << 10;
const MAX_LOG_CHARS: usize = 2000;

/// What the app can read: kept up to date by the node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The app wavelet's messages, in order.
    pub messages: Vec<WaveMessage>,
    /// This instance's kv store.
    pub kv: BTreeMap<String, Vec<u8>>,
    /// Peers running the same app for the same gadget.
    pub peers: Vec<Peer>,
}

/// Something the app asked for; the node carries it out after the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Post {
        kind: String,
        body: String,
    },
    KvSet {
        key: String,
        value: Vec<u8>,
    },
    KvDelete {
        key: String,
    },
    Send {
        to: String,
        data: Vec<u8>,
    },
    Open {
        to: String,
        mode: StreamMode,
        stream: u64,
    },
    Write {
        stream: u64,
        data: Vec<u8>,
    },
    Close {
        stream: u64,
    },
    Invalidate {
        view: u32,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}

/// The store data of one instance.
pub(crate) struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    pub(crate) limits: StoreLimits,
    grants: HashSet<Permission>,
    pub(crate) snapshot: Snapshot,
    effects: Vec<Effect>,
    effect_bytes: usize,
    next_stream: u64,
    pub(crate) streams: HashSet<u64>,
}

impl State {
    pub(crate) fn new(manifest_grants: &[Permission], memory_mb: u32, snapshot: Snapshot) -> Self {
        let mut wasi = WasiCtx::builder();
        wasi.allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .socket_addr_check(|_, _| Box::pin(async { false }));
        Self {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size((memory_mb as usize) << 20)
                .instances(256)
                .tables(256)
                .memories(32)
                .table_elements(1 << 20)
                .build(),
            grants: manifest_grants.iter().copied().collect(),
            snapshot,
            effects: Vec::new(),
            effect_bytes: 0,
            next_stream: 1,
            streams: HashSet::new(),
        }
    }

    pub(crate) fn for_manifest(manifest: &Manifest, snapshot: Snapshot) -> Self {
        Self::new(&manifest.permissions, manifest.memory_mb(), snapshot)
    }

    pub(crate) fn take_effects(&mut self) -> Vec<Effect> {
        self.effect_bytes = 0;
        std::mem::take(&mut self.effects)
    }

    fn grants(&self, permission: Permission) -> bool {
        self.grants.contains(&permission)
    }

    fn check(&self, permission: Permission) -> Result<(), String> {
        if self.grants(permission) {
            Ok(())
        } else {
            Err(DENIED.into())
        }
    }

    /// Queues an effect, or refuses it when this call queued too much.
    fn queue(&mut self, effect: Effect, bytes: usize) -> Result<(), String> {
        if self.effects.len() >= MAX_EFFECTS || self.effect_bytes + bytes > MAX_EFFECT_BYTES {
            return Err("too many effects in one call".into());
        }
        self.effect_bytes += bytes;
        self.effects.push(effect);
        Ok(())
    }

    fn kv_bytes(&self) -> usize {
        self.snapshot
            .kv
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum()
    }

    fn is_peer(&self, id: &str) -> bool {
        self.snapshot.peers.iter().any(|p| p.id == id)
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl types::Host for State {}

impl wave::Host for State {
    fn messages(&mut self, since: u64) -> Vec<WaveMessage> {
        if !self.grants(Permission::Wave) {
            return Vec::new();
        }
        self.snapshot
            .messages
            .iter()
            .filter(|m| m.seq > since)
            .cloned()
            .collect()
    }

    fn post(&mut self, kind: String, body: String) -> Result<(), String> {
        self.check(Permission::Wave)?;
        if kind.is_empty() || kind.len() > 64 {
            return Err("kind must be 1 to 64 bytes".into());
        }
        if body.len() > MAX_POST_BYTES {
            return Err(format!("body is larger than {} KiB", MAX_POST_BYTES >> 10));
        }
        let bytes = kind.len() + body.len();
        self.queue(Effect::Post { kind, body }, bytes)
    }
}

impl kv::Host for State {
    fn get(&mut self, key: String) -> Option<Vec<u8>> {
        if !self.grants(Permission::Kv) {
            return None;
        }
        self.snapshot.kv.get(&key).cloned()
    }

    fn set(&mut self, key: String, value: Vec<u8>) -> Result<(), String> {
        self.check(Permission::Kv)?;
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return Err(format!("key must be 1 to {MAX_KEY_BYTES} bytes"));
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!(
                "value is larger than {} MiB",
                MAX_VALUE_BYTES >> 20
            ));
        }
        let old = self
            .snapshot
            .kv
            .get(&key)
            .map_or(0, |v| key.len() + v.len());
        if self.kv_bytes() - old + key.len() + value.len() > MAX_KV_BYTES {
            return Err("kv store is full".into());
        }
        let bytes = key.len() + value.len();
        self.queue(
            Effect::KvSet {
                key: key.clone(),
                value: value.clone(),
            },
            bytes,
        )?;
        // Reads in the same call see the write.
        self.snapshot.kv.insert(key, value);
        Ok(())
    }

    fn delete(&mut self, key: String) -> Result<(), String> {
        self.check(Permission::Kv)?;
        let bytes = key.len();
        self.queue(Effect::KvDelete { key: key.clone() }, bytes)?;
        self.snapshot.kv.remove(&key);
        Ok(())
    }
}

impl peers::Host for State {
    fn list(&mut self) -> Vec<Peer> {
        if !self.grants(Permission::Peers) {
            return Vec::new();
        }
        self.snapshot.peers.clone()
    }

    fn send(&mut self, to: String, data: Vec<u8>) -> Result<(), String> {
        self.check(Permission::Peers)?;
        if !self.is_peer(&to) {
            return Err("no such peer".into());
        }
        let bytes = data.len();
        self.queue(Effect::Send { to, data }, bytes)
    }

    fn open(&mut self, to: String, mode: StreamMode) -> Result<u64, String> {
        self.check(Permission::Streams)?;
        if !self.is_peer(&to) {
            return Err("no such peer".into());
        }
        let stream = self.next_stream;
        self.queue(Effect::Open { to, mode, stream }, 0)?;
        self.next_stream += 1;
        self.streams.insert(stream);
        Ok(stream)
    }

    fn write(&mut self, stream: u64, data: Vec<u8>) -> Result<(), String> {
        self.check(Permission::Streams)?;
        if !self.streams.contains(&stream) {
            return Err("no such stream".into());
        }
        let bytes = data.len();
        self.queue(Effect::Write { stream, data }, bytes)
    }

    fn close(&mut self, stream: u64) {
        if self.grants(Permission::Streams) && self.streams.remove(&stream) {
            let _ = self.queue(Effect::Close { stream }, 0);
        }
    }
}

impl ui::Host for State {
    fn invalidate(&mut self, view: u32) {
        let already = self
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Invalidate { view: v } if *v == view));
        if !already {
            let _ = self.queue(Effect::Invalidate { view }, 0);
        }
    }
}

impl log::Host for State {
    fn log(&mut self, level: LogLevel, message: String) {
        let message: String = message.chars().take(MAX_LOG_CHARS).collect();
        let bytes = message.len();
        let _ = self.queue(Effect::Log { level, message }, bytes);
    }
}

/// Stream ids opened by the other side live in a separate range so they
/// never collide with ours.
pub(crate) const INCOMING_STREAM_BASE: u64 = 1 << 63;
