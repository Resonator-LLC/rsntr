//! What the server records for federation: which server key each remote
//! DNS domain is bound to (`federation_peers`), the signer info of delta
//! signers (`signer_info`), the notifications a host still owes remote
//! servers (`federation_outbox`) and the requests already applied
//! (`federation_applied`). The tables are created by migrations 7 and 8
//! and stay empty while federation is disabled.
//!
//! The rules for when a binding changes (trust on first use, signed key
//! rotation, quarantine) live in the `resonator-federation` crate; this
//! store only reads and writes rows.

use rusqlite::{OptionalExtension, params};

use super::{Database, StorageError};

/// How far a domain's key is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerStatus {
    /// The domain's current key.
    Trusted,
    /// An earlier key of the domain, replaced through a signed rotation.
    /// Signatures it made remain valid.
    Retired,
    /// A key that replaced the trusted one without a valid rotation
    /// statement; waits for an admin.
    Quarantined,
    /// Refused by an admin.
    Denied,
}

impl PeerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PeerStatus::Trusted => "trusted",
            PeerStatus::Retired => "retired",
            PeerStatus::Quarantined => "quarantined",
            PeerStatus::Denied => "denied",
        }
    }

    pub fn parse(text: &str) -> Option<PeerStatus> {
        match text {
            "trusted" => Some(PeerStatus::Trusted),
            "retired" => Some(PeerStatus::Retired),
            "quarantined" => Some(PeerStatus::Quarantined),
            "denied" => Some(PeerStatus::Denied),
            _ => None,
        }
    }
}

/// A row of `federation_peers`: one key of one domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRecord {
    pub domain: String,
    /// The raw 32-byte Ed25519 key.
    pub key: Vec<u8>,
    /// Milliseconds since the epoch.
    pub first_seen: i64,
    pub last_verified: i64,
    pub status: PeerStatus,
}

/// A row of `signer_info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerInfoRecord {
    /// SHA-256 of `key`.
    pub signer_id: Vec<u8>,
    pub domain: String,
    pub key: Vec<u8>,
    /// The serialised `ProtocolSignerInfo`.
    pub info: Vec<u8>,
}

/// Delivery state of an outbox row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutboxStatus {
    Pending,
    Delivered,
    Expired,
}

impl OutboxStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxStatus::Pending => "pending",
            OutboxStatus::Delivered => "delivered",
            OutboxStatus::Expired => "expired",
        }
    }

    pub fn parse(text: &str) -> Option<OutboxStatus> {
        match text {
            "pending" => Some(OutboxStatus::Pending),
            "delivered" => Some(OutboxStatus::Delivered),
            "expired" => Some(OutboxStatus::Expired),
            _ => None,
        }
    }
}

/// A row of `federation_outbox`: what a remote domain still has to be sent
/// of one wavelet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub domain: String,
    /// The wavelet's `wave://` URI.
    pub wavelet: String,
    /// Deltas from this version on are to be sent.
    pub from_version: u64,
    /// The ULID of the current attempt.
    pub request_id: String,
    pub attempts: u32,
    /// When the next attempt is due, in milliseconds since the epoch.
    pub next_at: i64,
    pub status: OutboxStatus,
    /// When the oldest undelivered notification was queued.
    pub pending_since: Option<i64>,
}

fn outbox_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxEntry> {
    let status: String = row.get(6)?;
    Ok(OutboxEntry {
        domain: row.get(0)?,
        wavelet: row.get(1)?,
        from_version: row.get::<_, i64>(2)?.max(0) as u64,
        request_id: row.get(3)?,
        attempts: row.get::<_, i64>(4)?.clamp(0, i64::from(u32::MAX)) as u32,
        next_at: row.get(5)?,
        status: OutboxStatus::parse(&status).unwrap_or(OutboxStatus::Expired),
        pending_since: row.get(7)?,
    })
}

/// A row of `federation_applied`: a request applied, with the answer it
/// got. Written in the same transaction as what the request changed where
/// that is a delta (see `DeltasAccess::enqueue_append`), so that a crash
/// cannot separate the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedRequest {
    pub request_id: String,
    /// The domain the request came from.
    pub domain: String,
    /// Milliseconds since the epoch.
    pub applied_at: i64,
    /// The serialised response sent.
    pub response: Option<Vec<u8>>,
}

/// Records an applied request; a request id already recorded keeps its
/// first response.
pub(crate) fn insert_applied(
    conn: &rusqlite::Connection,
    row: &AppliedRequest,
) -> Result<(), StorageError> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO federation_applied (request_id, domain, applied_at, \
         response) VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![
        row.request_id,
        row.domain,
        row.applied_at,
        row.response
    ])?;
    Ok(())
}

const OUTBOX_COLUMNS: &str =
    "domain, wavelet, from_version, request_id, attempts, next_at, status, pending_since";

fn sql_version(version: u64) -> Result<i64, StorageError> {
    i64::try_from(version)
        .map_err(|_| StorageError::Invalid(format!("version {version} exceeds the range of int64")))
}

/// A stored device certificate (`device_certs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCertRecord {
    pub device_key: Vec<u8>,
    pub person: Vec<u8>,
    pub name: String,
    pub admin: bool,
    pub issued_at: i64,
    /// The serialised signed certificate.
    pub cert: Vec<u8>,
    pub learned_at: i64,
}

/// A stored revocation list (`revocations`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationRecord {
    pub person: Vec<u8>,
    pub seq: u64,
    /// The serialised signed list.
    pub list: Vec<u8>,
    pub learned_at: i64,
}

/// The federation tables; cheap to clone.
#[derive(Debug, Clone)]
pub struct FederationStore {
    db: Database,
}

fn peer(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeerRecord> {
    let status: String = row.get(4)?;
    Ok(PeerRecord {
        domain: row.get(0)?,
        key: row.get(1)?,
        first_seen: row.get(2)?,
        last_verified: row.get(3)?,
        // The CHECK constraint admits only known values.
        status: PeerStatus::parse(&status).unwrap_or(PeerStatus::Denied),
    })
}

const PEER_COLUMNS: &str = "domain, key, first_seen, last_verified, status";

impl FederationStore {
    pub(crate) fn new(db: Database) -> Self {
        FederationStore { db }
    }

    // ---- Peers ----

    /// Every key recorded for `domain`, trusted first, then by first sight.
    pub async fn peer_keys(&self, domain: &str) -> Result<Vec<PeerRecord>, StorageError> {
        let domain = domain.to_owned();
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {PEER_COLUMNS} FROM federation_peers WHERE domain = ?1 \
                     ORDER BY status != 'trusted', first_seen"
                ))?;
                let rows = statement.query_map([domain], peer)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// The trusted key of `domain`, if one is pinned.
    pub async fn trusted_key(&self, domain: &str) -> Result<Option<PeerRecord>, StorageError> {
        let domain = domain.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "SELECT {PEER_COLUMNS} FROM federation_peers \
                         WHERE domain = ?1 AND status = 'trusted'"
                    ))?
                    .query_row([domain], peer)
                    .optional()?)
            })
            .await
    }

    /// Every recorded peer key, by domain.
    pub async fn peers(&self) -> Result<Vec<PeerRecord>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {PEER_COLUMNS} FROM federation_peers ORDER BY domain, first_seen"
                ))?;
                let rows = statement.query_map([], peer)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Records `key` for `domain` with `status` at time `now`, keeping
    /// `first_seen` of an existing row. Making a key trusted retires the
    /// domain's previously trusted key, in the same transaction.
    pub async fn set_peer(
        &self,
        domain: &str,
        key: &[u8],
        status: PeerStatus,
        now: i64,
    ) -> Result<(), StorageError> {
        let domain = domain.to_owned();
        let key = key.to_vec();
        self.db
            .write(move |conn| {
                if status == PeerStatus::Trusted {
                    conn.prepare_cached(
                        "UPDATE federation_peers SET status = 'retired' \
                         WHERE domain = ?1 AND status = 'trusted' AND key != ?2",
                    )?
                    .execute(params![domain, key])?;
                }
                conn.prepare_cached(
                    "INSERT INTO federation_peers (domain, key, first_seen, last_verified, status) \
                     VALUES (?1, ?2, ?3, ?3, ?4) ON CONFLICT (domain, key) DO UPDATE SET \
                     last_verified = excluded.last_verified, status = excluded.status",
                )?
                .execute(params![domain, key, now, status.as_str()])?;
                Ok(())
            })
            .await
    }

    /// Notes that `key` was seen bound to `domain` at `now`.
    pub async fn touch_peer(&self, domain: &str, key: &[u8], now: i64) -> Result<(), StorageError> {
        let domain = domain.to_owned();
        let key = key.to_vec();
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE federation_peers SET last_verified = ?3 WHERE domain = ?1 AND key = ?2",
                )?
                .execute(params![domain, key, now])?;
                Ok(())
            })
            .await
    }

    // ---- Signer info ----

    /// Stores (or replaces) a signer's info.
    pub async fn put_signer_info(&self, record: SignerInfoRecord) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO signer_info (signer_id, domain, key, info) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT (signer_id) DO UPDATE SET domain = excluded.domain, \
                     key = excluded.key, info = excluded.info",
                )?
                .execute(params![
                    record.signer_id,
                    record.domain,
                    record.key,
                    record.info
                ])?;
                Ok(())
            })
            .await
    }

    pub async fn signer_info(
        &self,
        signer_id: &[u8],
    ) -> Result<Option<SignerInfoRecord>, StorageError> {
        let signer_id = signer_id.to_vec();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT signer_id, domain, key, info FROM signer_info WHERE signer_id = ?1",
                    )?
                    .query_row([signer_id], |row| {
                        Ok(SignerInfoRecord {
                            signer_id: row.get(0)?,
                            domain: row.get(1)?,
                            key: row.get(2)?,
                            info: row.get(3)?,
                        })
                    })
                    .optional()?)
            })
            .await
    }

    // ---- Outbox ----

    /// Notes that `domain` has to be sent the deltas of `wavelet` from
    /// `from_version` on. Coalesces with a pending row (the earlier version
    /// wins, the backoff stays); a delivered or expired row starts over.
    /// `request_id` becomes the ULID of the next attempt.
    pub async fn outbox_enqueue(
        &self,
        domain: &str,
        wavelet: &str,
        from_version: u64,
        request_id: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        let domain = domain.to_owned();
        let wavelet = wavelet.to_owned();
        let request_id = request_id.to_owned();
        let from_version = sql_version(from_version)?;
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO federation_outbox (domain, wavelet, from_version, request_id, \
                     attempts, next_at, status, pending_since) \
                     VALUES (?1, ?2, ?3, ?4, 0, ?5, 'pending', ?5) \
                     ON CONFLICT (domain, wavelet) DO UPDATE SET \
                     from_version = CASE WHEN status = 'pending' \
                         THEN min(from_version, excluded.from_version) \
                         ELSE excluded.from_version END, \
                     request_id = excluded.request_id, \
                     attempts = CASE WHEN status = 'pending' THEN attempts ELSE 0 END, \
                     next_at = CASE WHEN status = 'pending' THEN next_at ELSE excluded.next_at END, \
                     pending_since = CASE WHEN status = 'pending' \
                         THEN coalesce(pending_since, excluded.pending_since) \
                         ELSE excluded.pending_since END, \
                     status = 'pending'",
                )?
                .execute(params![domain, wavelet, from_version, request_id, now])?;
                Ok(())
            })
            .await
    }

    /// Pending rows due at `now`, the longest waiting first; at most
    /// `limit`.
    pub async fn outbox_due(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<OutboxEntry>, StorageError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {OUTBOX_COLUMNS} FROM federation_outbox \
                     WHERE status = 'pending' AND next_at <= ?1 ORDER BY next_at LIMIT ?2"
                ))?;
                let rows = statement.query_map(params![now, limit], outbox_entry)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// When the next pending row is due, if there is one.
    pub async fn outbox_next_at(&self) -> Result<Option<i64>, StorageError> {
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT min(next_at) FROM federation_outbox WHERE status = 'pending'",
                    )?
                    .query_row([], |row| row.get(0))?)
            })
            .await
    }

    /// Every outbox row, by domain and wavelet.
    pub async fn outbox_entries(&self) -> Result<Vec<OutboxEntry>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {OUTBOX_COLUMNS} FROM federation_outbox ORDER BY domain, wavelet"
                ))?;
                let rows = statement.query_map([], outbox_entry)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// The attempt `request_id` delivered everything up to `sent_to`. The
    /// row is done unless a notification came in meanwhile (it then has
    /// another request id), in which case it stays pending, due at `now`.
    pub async fn outbox_delivered(
        &self,
        domain: &str,
        wavelet: &str,
        request_id: &str,
        sent_to: u64,
        now: i64,
    ) -> Result<(), StorageError> {
        let domain = domain.to_owned();
        let wavelet = wavelet.to_owned();
        let request_id = request_id.to_owned();
        let sent_to = sql_version(sent_to)?;
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE federation_outbox SET from_version = max(from_version, ?4), \
                     attempts = 0, \
                     next_at = CASE WHEN request_id = ?3 THEN next_at ELSE ?5 END, \
                     pending_since = CASE WHEN request_id = ?3 THEN NULL ELSE pending_since END, \
                     status = CASE WHEN request_id = ?3 THEN 'delivered' ELSE status END \
                     WHERE domain = ?1 AND wavelet = ?2",
                )?
                .execute(params![domain, wavelet, request_id, sent_to, now])?;
                Ok(())
            })
            .await
    }

    /// An attempt failed after delivering everything up to `sent_to`; the
    /// next is due at `next_at` with the id `request_id`.
    pub async fn outbox_retry(
        &self,
        domain: &str,
        wavelet: &str,
        sent_to: u64,
        request_id: &str,
        next_at: i64,
    ) -> Result<(), StorageError> {
        let domain = domain.to_owned();
        let wavelet = wavelet.to_owned();
        let request_id = request_id.to_owned();
        let sent_to = sql_version(sent_to)?;
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE federation_outbox SET from_version = max(from_version, ?3), \
                     request_id = ?4, attempts = attempts + 1, next_at = ?5 \
                     WHERE domain = ?1 AND wavelet = ?2 AND status = 'pending'",
                )?
                .execute(params![domain, wavelet, sent_to, request_id, next_at])?;
                Ok(())
            })
            .await
    }

    /// Gives up on a row.
    pub async fn outbox_expire(&self, domain: &str, wavelet: &str) -> Result<(), StorageError> {
        let domain = domain.to_owned();
        let wavelet = wavelet.to_owned();
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE federation_outbox SET status = 'expired' \
                     WHERE domain = ?1 AND wavelet = ?2",
                )?
                .execute(params![domain, wavelet])?;
                Ok(())
            })
            .await
    }

    // ---- Applied requests ----

    /// The request with this ULID if it was applied before: the domain it
    /// came from and the response sent.
    pub async fn applied_request(
        &self,
        request_id: &str,
    ) -> Result<Option<(String, Option<Vec<u8>>)>, StorageError> {
        let request_id = request_id.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT domain, response FROM federation_applied WHERE request_id = ?1",
                    )?
                    .query_row([request_id], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()?)
            })
            .await
    }

    /// Records an applied request with its response; a request id already
    /// recorded keeps its first response.
    pub async fn record_applied(
        &self,
        request_id: &str,
        domain: &str,
        now: i64,
        response: Option<Vec<u8>>,
    ) -> Result<(), StorageError> {
        let row = AppliedRequest {
            request_id: request_id.to_owned(),
            domain: domain.to_owned(),
            applied_at: now,
            response,
        };
        self.db.write(move |conn| insert_applied(conn, &row)).await
    }

    /// Forgets requests applied before `before`; returns how many.
    pub async fn prune_applied(&self, before: i64) -> Result<usize, StorageError> {
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached("DELETE FROM federation_applied WHERE applied_at < ?1")?
                    .execute([before])?)
            })
            .await
    }

    // ---- Person identity ----

    /// Stores a device certificate (replacing an older one of the same
    /// device and person; a newer stored one is kept).
    pub async fn put_device_cert(&self, record: DeviceCertRecord) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO device_certs (device_key, person, name, admin, issued_at, cert, \
                     learned_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                     ON CONFLICT (device_key, person) DO UPDATE SET name = excluded.name, \
                     admin = excluded.admin, issued_at = excluded.issued_at, cert = excluded.cert, \
                     learned_at = excluded.learned_at \
                     WHERE excluded.issued_at >= device_certs.issued_at",
                )?
                .execute(params![
                    record.device_key,
                    record.person,
                    record.name,
                    record.admin,
                    record.issued_at,
                    record.cert,
                    record.learned_at
                ])?;
                Ok(())
            })
            .await
    }

    /// Every stored device certificate, by person, then issue time.
    pub async fn device_certs(&self) -> Result<Vec<DeviceCertRecord>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT device_key, person, name, admin, issued_at, cert, learned_at \
                     FROM device_certs ORDER BY person, issued_at",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok(DeviceCertRecord {
                        device_key: row.get(0)?,
                        person: row.get(1)?,
                        name: row.get(2)?,
                        admin: row.get(3)?,
                        issued_at: row.get(4)?,
                        cert: row.get(5)?,
                        learned_at: row.get(6)?,
                    })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Stores a person's revocation list if its `seq` is higher than that
    /// of the stored one; returns whether it was stored.
    pub async fn put_revocations(&self, record: RevocationRecord) -> Result<bool, StorageError> {
        let seq = sql_version(record.seq)?;
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "INSERT INTO revocations (person, seq, list, learned_at) \
                         VALUES (?1, ?2, ?3, ?4) ON CONFLICT (person) DO UPDATE SET \
                         seq = excluded.seq, list = excluded.list, learned_at = excluded.learned_at \
                         WHERE excluded.seq > revocations.seq",
                    )?
                    .execute(params![record.person, seq, record.list, record.learned_at])?
                    > 0)
            })
            .await
    }

    /// The newest revocation list of every person known.
    pub async fn revocations(&self) -> Result<Vec<RevocationRecord>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT person, seq, list, learned_at FROM revocations ORDER BY person",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok(RevocationRecord {
                        person: row.get(0)?,
                        seq: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                        list: row.get(2)?,
                        learned_at: row.get(3)?,
                    })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    // ---- Host leases ----

    /// Records the newest host lease of `person` (the caller has checked
    /// that it is newer).
    pub async fn put_host_lease(
        &self,
        person: Vec<u8>,
        lease: Vec<u8>,
        learned_at: i64,
    ) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO host_leases (person, lease, learned_at) VALUES (?1, ?2, ?3) \
                     ON CONFLICT (person) DO UPDATE SET lease = excluded.lease, \
                     learned_at = excluded.learned_at",
                )?
                .execute(params![person, lease, learned_at])?;
                Ok(())
            })
            .await
    }

    /// The stored host leases: (person, signed lease).
    pub async fn host_leases(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement =
                    conn.prepare_cached("SELECT person, lease FROM host_leases ORDER BY person")?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    // ---- Pairing ----

    /// Records a new pairing invitation.
    pub async fn put_pair_invitation(
        &self,
        record: PairInvitationRecord,
    ) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO pair_invitations (id, secret, admin, created_at, expires_at, \
                     state, joiner, joiner_name, failures, nameplate, code, code_failures) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )?
                .execute(params![
                    record.id,
                    record.secret,
                    record.admin,
                    record.created_at,
                    record.expires_at,
                    record.state,
                    record.joiner,
                    record.joiner_name,
                    record.failures,
                    record.nameplate,
                    record.code,
                    record.code_failures
                ])?;
                Ok(())
            })
            .await
    }

    /// The invitation not used yet whose code has `nameplate`.
    pub async fn pair_invitation_by_nameplate(
        &self,
        nameplate: &str,
    ) -> Result<Option<PairInvitationRecord>, StorageError> {
        let nameplate = nameplate.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "{PAIR_SELECT} WHERE nameplate = ?1 AND state <> 'used' \
                         ORDER BY created_at DESC LIMIT 1"
                    ))?
                    .query_row(params![nameplate], pair_row)
                    .optional()?)
            })
            .await
    }

    /// Counts a wrong code tried at an invitation; returns the wrong codes
    /// so far.
    pub async fn pair_code_failed(&self, id: &str) -> Result<u32, StorageError> {
        let id = id.to_owned();
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE pair_invitations SET code_failures = code_failures + 1 WHERE id = ?1",
                )?
                .execute(params![id])?;
                Ok(conn
                    .prepare_cached("SELECT code_failures FROM pair_invitations WHERE id = ?1")?
                    .query_row(params![id], |row| row.get(0))
                    .optional()?
                    .unwrap_or(0))
            })
            .await
    }

    /// A pairing invitation by id.
    pub async fn pair_invitation(
        &self,
        id: &str,
    ) -> Result<Option<PairInvitationRecord>, StorageError> {
        let id = id.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!("{PAIR_SELECT} WHERE id = ?1"))?
                    .query_row(params![id], pair_row)
                    .optional()?)
            })
            .await
    }

    /// The pairing invitations not used yet, newest first.
    pub async fn pair_invitations(&self) -> Result<Vec<PairInvitationRecord>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "{PAIR_SELECT} WHERE state <> 'used' ORDER BY created_at DESC"
                ))?;
                let rows = statement.query_map([], pair_row)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Moves an invitation from state `from` to `to` (with the joiner);
    /// returns whether it was in state `from`.
    pub async fn update_pair_invitation(
        &self,
        id: &str,
        from: &str,
        to: &str,
        joiner: Option<Vec<u8>>,
        joiner_name: Option<String>,
    ) -> Result<bool, StorageError> {
        let (id, from, to) = (id.to_owned(), from.to_owned(), to.to_owned());
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "UPDATE pair_invitations SET state = ?3, \
                         joiner = COALESCE(?4, joiner), joiner_name = COALESCE(?5, joiner_name) \
                         WHERE id = ?1 AND state = ?2",
                    )?
                    .execute(params![id, from, to, joiner, joiner_name])?
                    > 0)
            })
            .await
    }

    /// Counts a failed attempt at an invitation; returns the failures so
    /// far.
    pub async fn pair_invitation_failed(&self, id: &str) -> Result<u32, StorageError> {
        let id = id.to_owned();
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE pair_invitations SET failures = failures + 1 WHERE id = ?1",
                )?
                .execute(params![id])?;
                Ok(conn
                    .prepare_cached("SELECT failures FROM pair_invitations WHERE id = ?1")?
                    .query_row(params![id], |row| row.get(0))
                    .optional()?
                    .unwrap_or(0))
            })
            .await
    }

    /// Removes an invitation; returns whether there was one.
    pub async fn delete_pair_invitation(&self, id: &str) -> Result<bool, StorageError> {
        let id = id.to_owned();
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached("DELETE FROM pair_invitations WHERE id = ?1")?
                    .execute(params![id])?
                    > 0)
            })
            .await
    }

    /// Forgets the secrets of invitations used or expired before `now`.
    pub async fn prune_pair_invitations(&self, now: i64) -> Result<usize, StorageError> {
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "DELETE FROM pair_invitations WHERE expires_at < ?1 OR state = 'used'",
                    )?
                    .execute(params![now])?)
            })
            .await
    }

    // ---- Device addresses ----

    /// Records where a device was reachable.
    pub async fn put_device_addr(
        &self,
        device: Vec<u8>,
        addr: String,
        at: i64,
    ) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO device_addrs (device, addr, updated_at) VALUES (?1, ?2, ?3) \
                     ON CONFLICT (device, addr) DO UPDATE SET updated_at = excluded.updated_at",
                )?
                .execute(params![device, addr, at])?;
                Ok(())
            })
            .await
    }

    /// Every recorded device address: (device, addr, updated_at).
    pub async fn device_addrs(&self) -> Result<Vec<(Vec<u8>, String, i64)>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT device, addr, updated_at FROM device_addrs ORDER BY device, updated_at DESC",
                )?;
                let rows =
                    statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    // ---- Co-hosts ----

    /// Stores a co-host grant if it is newer than the stored one of the
    /// same person and delegate domain; returns whether it was stored.
    pub async fn put_co_host(&self, record: CoHostRecord) -> Result<bool, StorageError> {
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "INSERT INTO co_hosts (person, delegate_domain, issued_at, until, \
                         grant_bytes, learned_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                         ON CONFLICT (person, delegate_domain) DO UPDATE SET \
                         issued_at = excluded.issued_at, until = excluded.until, \
                         grant_bytes = excluded.grant_bytes, learned_at = excluded.learned_at \
                         WHERE excluded.issued_at > co_hosts.issued_at",
                    )?
                    .execute(params![
                        record.person,
                        record.delegate_domain,
                        record.issued_at,
                        record.until,
                        record.grant,
                        record.learned_at
                    ])?
                    > 0)
            })
            .await
    }

    /// Every stored co-host grant.
    pub async fn co_hosts(&self) -> Result<Vec<CoHostRecord>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT person, delegate_domain, issued_at, until, grant_bytes, learned_at \
                     FROM co_hosts ORDER BY person, delegate_domain",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok(CoHostRecord {
                        person: row.get(0)?,
                        delegate_domain: row.get(1)?,
                        issued_at: row.get(2)?,
                        until: row.get(3)?,
                        grant: row.get(4)?,
                        learned_at: row.get(5)?,
                    })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    // ---- Push targets ----

    /// Stores how a device is woken if it is newer (by `issued_at`) than
    /// the stored one; returns whether it was stored.
    pub async fn put_push_target(&self, record: PushTargetRecord) -> Result<bool, StorageError> {
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "INSERT INTO push_targets (device, issued_at, relay, signed, token_hash, \
                         learned_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                         ON CONFLICT (device) DO UPDATE SET issued_at = excluded.issued_at, \
                         relay = excluded.relay, signed = excluded.signed, \
                         token_hash = excluded.token_hash, learned_at = excluded.learned_at \
                         WHERE excluded.issued_at > push_targets.issued_at",
                    )?
                    .execute(params![
                        record.device,
                        record.issued_at,
                        record.relay,
                        record.signed,
                        record.token_hash,
                        record.learned_at
                    ])?
                    > 0)
            })
            .await
    }

    /// Forgets how `device` is woken, unless what is stored was issued
    /// after `issued_at`.
    pub async fn delete_push_target(
        &self,
        device: Vec<u8>,
        issued_at: i64,
    ) -> Result<bool, StorageError> {
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "DELETE FROM push_targets WHERE device = ?1 AND issued_at <= ?2",
                    )?
                    .execute(params![device, issued_at])?
                    > 0)
            })
            .await
    }

    /// Every stored push target.
    pub async fn push_targets(&self) -> Result<Vec<PushTargetRecord>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT device, issued_at, relay, signed, token_hash, learned_at \
                     FROM push_targets ORDER BY device",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok(PushTargetRecord {
                        device: row.get(0)?,
                        issued_at: row.get(1)?,
                        relay: row.get(2)?,
                        signed: row.get(3)?,
                        token_hash: row.get(4)?,
                        learned_at: row.get(5)?,
                    })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }
}

/// How a device is woken (`push_targets`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTargetRecord {
    pub device: Vec<u8>,
    pub issued_at: i64,
    pub relay: String,
    /// The serialised `SignedPushTarget` (holds the sealed token, never
    /// the token itself).
    pub signed: Vec<u8>,
    /// This device's own row only: SHA-256 of the push token it sealed,
    /// to notice a new token or relay.
    pub token_hash: Option<Vec<u8>>,
    pub learned_at: i64,
}

const PAIR_SELECT: &str = "SELECT id, secret, admin, created_at, expires_at, state, joiner, \
                           joiner_name, failures, nameplate, code, code_failures \
                           FROM pair_invitations";

fn pair_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PairInvitationRecord> {
    Ok(PairInvitationRecord {
        id: row.get(0)?,
        secret: row.get(1)?,
        admin: row.get(2)?,
        created_at: row.get(3)?,
        expires_at: row.get(4)?,
        state: row.get(5)?,
        joiner: row.get(6)?,
        joiner_name: row.get(7)?,
        failures: row.get(8)?,
        nameplate: row.get(9)?,
        code: row.get(10)?,
        code_failures: row.get(11)?,
    })
}

/// A pairing invitation (`pair_invitations`). Its `Debug` form leaves
/// the secret out.
#[derive(Clone, PartialEq, Eq)]
pub struct PairInvitationRecord {
    pub id: String,
    pub secret: Vec<u8>,
    pub admin: bool,
    pub created_at: i64,
    pub expires_at: i64,
    /// `open`, `requested`, `confirmed` or `used`.
    pub state: String,
    pub joiner: Option<Vec<u8>>,
    pub joiner_name: Option<String>,
    pub failures: u32,
    /// Pairing by code: the code's nameplate (not secret), if it has a
    /// code.
    pub nameplate: Option<String>,
    /// Pairing by code: the words (the SPAKE2 password). Never log them.
    pub code: Option<Vec<u8>>,
    /// Wrong codes tried so far.
    pub code_failures: u32,
}

impl std::fmt::Debug for PairInvitationRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairInvitationRecord")
            .field("id", &self.id)
            .field("admin", &self.admin)
            .field("expires_at", &self.expires_at)
            .field("state", &self.state)
            .field("nameplate", &self.nameplate)
            .finish_non_exhaustive()
    }
}

/// A co-host grant (`co_hosts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoHostRecord {
    pub person: Vec<u8>,
    pub delegate_domain: String,
    pub issued_at: i64,
    pub until: i64,
    /// The serialised `SignedCoHost`.
    pub grant: Vec<u8>,
    pub learned_at: i64,
}
