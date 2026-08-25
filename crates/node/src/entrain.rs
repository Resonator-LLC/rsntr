//! Connection-scoped entrainment (projection protocol sec 5).
//!
//! The registry maps active entrainments to their signal channels. Each
//! entrainment holds a capacity-1 tokio channel used as a dirty flag:
//! vibrations carry no payload in v1, so a burst of signals against a busy
//! consumer coalesces into the one already-queued signal (`try_send` on a
//! full channel is the coalescing, not an error). Damping of a genuinely
//! slow consumer happens at the stream-send layer in the pipeline (send
//! timeout -> `rsntr:Error` `limit-exceeded`), not here.
//!
//! Signals come from two sources, both fed by the sqlite `update_hook` the
//! pipeline installs on the node's connection:
//!
//! - a change to any table signals the entrainments whose point's
//!   `resource` is that table;
//! - a change to `_projection` or `_policy` additionally signals the
//!   well-known `urn:rsntr:projection-changed` point.

use std::collections::HashMap;
use std::sync::Mutex;

use rusqlite::Connection;
use tokio::sync::mpsc;

use resonator_protocol::vocab::PROJECTION_CHANGED;

use crate::projection::visible;

/// Registry of active entrainments. Cheap to share behind an `Arc`.
#[derive(Default)]
pub struct Entrainments {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    subs: HashMap<u64, Sub>,
    next_id: u64,
}

struct Sub {
    point: String,
    /// The table whose changes vibrate this point (`_projection.resource`);
    /// `None` for points signalled by point IRI only (projection-changed).
    table: Option<String>,
    tx: mpsc::Sender<()>,
}

impl Entrainments {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an entrainment; the receiver yields one `()` per pending
    /// (possibly coalesced) signal.
    pub fn subscribe(&self, point: String, table: Option<String>) -> (u64, mpsc::Receiver<()>) {
        // Capacity 1: a full channel means a signal is already pending, so
        // further signals coalesce into it.
        let (tx, rx) = mpsc::channel(1);
        let mut inner = self.inner.lock().expect("entrainments lock");
        let id = inner.next_id;
        inner.next_id += 1;
        inner.subs.insert(id, Sub { point, table, tx });
        (id, rx)
    }

    /// Removes an entrainment; called when its stream ends for any reason.
    pub fn unsubscribe(&self, id: u64) {
        let mut inner = self.inner.lock().expect("entrainments lock");
        inner.subs.remove(&id);
    }

    /// Signals every entrainment listening to `table`. Safe to call from
    /// the db thread (the update hook).
    pub fn signal_table(&self, table: &str) {
        let inner = self.inner.lock().expect("entrainments lock");
        for sub in inner.subs.values() {
            if sub.table.as_deref() == Some(table) {
                let _ = sub.tx.try_send(());
            }
        }
    }

    /// Signals every entrainment on `point` by IRI.
    pub fn signal_point(&self, point: &str) {
        let inner = self.inner.lock().expect("entrainments lock");
        for sub in inner.subs.values() {
            if sub.point == point {
                let _ = sub.tx.try_send(());
            }
        }
    }

    /// Number of active entrainments (test observability).
    pub fn active(&self) -> usize {
        self.inner.lock().expect("entrainments lock").subs.len()
    }
}

/// What the entrain handler should answer (gate outcome).
pub enum EntrainGate {
    /// Not an admitted peer: entrainment holds server resources, so unlike
    /// the projection itself it is not a pre-admission affordance.
    UnknownPeer,
    /// No such Sympathetic point in this node: `point-unknown`.
    UnknownPoint { reason: String },
    /// Policy refuses the caller this point.
    Denied { reason: String },
    /// Entrained; subscribe with this table key.
    Allowed { table: Option<String> },
}

/// Runs on the db thread: peer gate, point lookup, policy check. The
/// well-known projection-changed point is entrainable by every admitted
/// peer without a `_projection` row.
/// Owner-channel gate (docs/owner-channel.md sec 4): no peer gate and no
/// policy filter; any registered Sympathetic point may be entrained.
pub fn gate_entrain_owner(conn: &Connection, point: &str) -> EntrainGate {
    if point == PROJECTION_CHANGED {
        return EntrainGate::Allowed { table: None };
    }
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT kind, resource FROM _projection WHERE point_iri = ?1",
            [point],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    match row {
        None => EntrainGate::UnknownPoint {
            reason: format!("no point <{point}> in this projection"),
        },
        Some((kind, _)) if kind != "sympathetic" => EntrainGate::UnknownPoint {
            reason: format!("<{point}> is not a Sympathetic point"),
        },
        Some((_, resource)) => EntrainGate::Allowed { table: resource },
    }
}

pub fn gate_entrain(conn: &Connection, peer: &str, point: &str) -> EntrainGate {
    // peer_known also admits the node's own endpoint id (self-dial,
    // chat protocol sec 4.3), which is what lets `rsntr chat watch`
    // entrain the local inbox point.
    if !crate::pipeline::peer_known(conn, peer) {
        return EntrainGate::UnknownPeer;
    }

    if point == PROJECTION_CHANGED {
        return EntrainGate::Allowed { table: None };
    }

    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT kind, modulation, resource FROM _projection WHERE point_iri = ?1",
            [point],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((kind, modulation, resource)) = row else {
        return EntrainGate::UnknownPoint {
            reason: format!("no point <{point}> in this projection"),
        };
    };
    if kind != "sympathetic" {
        // A non-Sympathetic point is outside the entrainable projection.
        return EntrainGate::UnknownPoint {
            reason: format!("<{point}> is not a Sympathetic point"),
        };
    }
    match visible(
        conn,
        peer,
        &kind,
        modulation.as_deref(),
        resource.as_deref(),
    ) {
        Ok(true) => EntrainGate::Allowed { table: resource },
        Ok(false) => EntrainGate::Denied {
            reason: format!("no entrain policy for <{point}>"),
        },
        Err(e) => EntrainGate::Denied {
            reason: format!("policy check failed: {e}"),
        },
    }
}
