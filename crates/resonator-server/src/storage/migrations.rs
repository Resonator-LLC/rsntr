//! Embedded schema migrations. The schema version is SQLite's
//! `user_version`; migration `n` (1-based) moves the schema from version
//! `n - 1` to `n`. Later steps (attachments, sessions, tokens, search)
//! append entries here; existing entries must never change.

use rusqlite::Connection;

use super::StorageError;

pub(crate) struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

pub(crate) const MIGRATIONS: &[Migration] = &[
    INITIAL,
    Migration {
        name: "sessions and personal access tokens",
        sql: r#"
-- Browser sessions. The cookie holds a random 256-bit id; only its SHA-256
-- is stored, so a copy of the database does not give access. No foreign key
-- to accounts: storing an account replaces its row.
CREATE TABLE sessions (
    id_hash             BLOB PRIMARY KEY,
    address             TEXT NOT NULL,
    -- SHA-256 of the token that authenticates a WebSocket for this session.
    socket_token_hash   BLOB NOT NULL UNIQUE,
    created_at          INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL,
    -- Idle expiry, pushed forward on use; never beyond absolute_expires_at.
    expires_at          INTEGER NOT NULL,
    absolute_expires_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX sessions_address ON sessions(address);
CREATE INDEX sessions_expires_at ON sessions(expires_at);

-- Personal access tokens, presented as "Authorization: Bearer". Only the
-- SHA-256 of a token is stored.
CREATE TABLE access_tokens (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    token_hash   BLOB NOT NULL UNIQUE,
    address      TEXT NOT NULL,
    name         TEXT NOT NULL,
    -- Space-separated scope names.
    scopes       TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    -- NULL: never expires.
    expires_at   INTEGER,
    last_used_at INTEGER
);
CREATE INDEX access_tokens_address ON access_tokens(address);

-- Robots and agents authenticate with the token whose hash their account
-- holds (lower-case hex SHA-256).
CREATE INDEX accounts_access_token_hash ON accounts(access_token_hash)
    WHERE access_token_hash IS NOT NULL;
"#,
    },
    Migration {
        name: "search index: per-user wave views and user state",
        sql: r#"
-- The search index. It is derived from the wavelets and can be rebuilt
-- from them at any time; `version` says how far a row has got. The
-- full-text table (search_text, FTS5, rowid = search_wavelets.id) is not
-- created here but by the indexer, because FTS5 may be missing from the
-- SQLite build.
CREATE TABLE search_wavelets (
    id               INTEGER PRIMARY KEY,
    wave_domain      TEXT NOT NULL,
    wave_id          TEXT NOT NULL,
    wavelet_domain   TEXT NOT NULL,
    wavelet_id       TEXT NOT NULL,
    -- The wavelet version this row, its participants and its user state
    -- reflect.
    version          INTEGER NOT NULL,
    -- The wavelet version whose text is in the full-text index; -1 if none.
    text_version     INTEGER NOT NULL DEFAULT -1,
    creator          TEXT NOT NULL,
    created          INTEGER NOT NULL,
    last_modified    INTEGER NOT NULL,
    -- 0 other, 1 conversational (conv+...), 2 user data (user+...)
    kind             INTEGER NOT NULL,
    is_root          INTEGER NOT NULL,
    -- Whether the wavelet has a conversation manifest.
    has_conversation INTEGER NOT NULL,
    -- User-data wavelets: the explicit follow state (1 followed, 0 muted),
    -- NULL if there is none.
    followed         INTEGER,
    UNIQUE (wave_domain, wave_id, wavelet_domain, wavelet_id)
);

-- Who is a participant of which wavelet: the per-user wave view. The
-- shared domain participant (@domain) is a participant like any other.
CREATE TABLE search_participants (
    participant TEXT NOT NULL,
    wavelet     INTEGER NOT NULL REFERENCES search_wavelets(id) ON DELETE CASCADE,
    PRIMARY KEY (participant, wavelet)
) WITHOUT ROWID;
CREATE INDEX search_participants_wavelet ON search_participants(wavelet);

-- The archive state in a user-data wavelet: the version at which its
-- owner archived each conversational wavelet of the wave.
CREATE TABLE search_archive (
    udw            INTEGER NOT NULL REFERENCES search_wavelets(id) ON DELETE CASCADE,
    wavelet_domain TEXT NOT NULL,
    wavelet_id     TEXT NOT NULL,
    version        INTEGER NOT NULL,
    PRIMARY KEY (udw, wavelet_domain, wavelet_id)
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "attachments",
        sql: r#"
-- Attachment metadata (AttachmentMetadata of attachment.proto); the bytes
-- are files in the attachments directory, named by the SHA-256 of the id.
-- A row is written only after its file is durable.
CREATE TABLE attachments (
    -- The serialised attachment id: "<domain>/<id>" or "<id>".
    id               TEXT PRIMARY KEY,
    -- The wavelet the attachment belongs to, as a wave reference path;
    -- whoever may read that wavelet may read the attachment.
    wave_ref         TEXT NOT NULL,
    file_name        TEXT NOT NULL,
    mime_type        TEXT NOT NULL,
    size             INTEGER NOT NULL,
    -- Address of the uploader; empty if unknown.
    creator          TEXT NOT NULL,
    -- Set if the attachment is an image that has a stored thumbnail.
    image_width      INTEGER,
    image_height     INTEGER,
    thumbnail_width  INTEGER,
    thumbnail_height INTEGER,
    -- NULL: not scanned.
    malware          INTEGER,
    sha256           BLOB NOT NULL,
    created_at       INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX attachments_wave_ref ON attachments(wave_ref);
"#,
    },
    Migration {
        name: "robot registrations, agent triggers and usage",
        sql: r#"
-- Who registered an external robot, and the key of the HMAC that signs
-- the server's calls to it. A table of its own because storing an account
-- replaces its row. Removed with the robot.
CREATE TABLE robot_registrations (
    address        TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    webhook_secret TEXT NOT NULL,
    created_at     INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX robot_registrations_owner ON robot_registrations(owner);

-- What triggered a built-in agent and how far it has got with it, so that
-- a reply is answered exactly once across restarts. blip_id is the
-- replying blip, or empty for "added to the wavelet".
CREATE TABLE agent_triggers (
    agent            TEXT NOT NULL,
    wave_ref         TEXT NOT NULL,
    blip_id          TEXT NOT NULL,
    -- 'pending', 'answering' or 'answered'
    state            TEXT NOT NULL,
    response_blip_id TEXT,
    updated_at       INTEGER NOT NULL,
    PRIMARY KEY (agent, wave_ref, blip_id)
) WITHOUT ROWID;
CREATE INDEX agent_triggers_unfinished ON agent_triggers(agent) WHERE state != 'answered';

-- Model requests per day (days since the epoch) and subject (an address,
-- or empty for the whole server), for the daily budgets.
CREATE TABLE agent_usage (
    day               INTEGER NOT NULL,
    subject           TEXT NOT NULL,
    requests          INTEGER NOT NULL DEFAULT 0,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, subject)
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "agent triggers: answered-content fingerprint and redo count",
        sql: r#"
-- What an agent answered: a SHA-256 (hex) of the normalised text it
-- responded to (the triggering reply, or for 'added to the wavelet' the
-- human-written text of the wave), so that an edit changing that text
-- makes it redo its answer. NULL for triggers answered before this
-- migration. reanswers counts the redone answers, for ai.max_reanswers.
ALTER TABLE agent_triggers ADD COLUMN fingerprint TEXT;
ALTER TABLE agent_triggers ADD COLUMN reanswers INTEGER NOT NULL DEFAULT 0;
"#,
    },
    Migration {
        name: "federation: peers, signer info, outbox, applied requests, replica marker",
        sql: r#"
-- Server-to-server federation. Every table stays empty, and hosted_by NULL,
-- on a server with federation disabled.

-- Which server key a remote DNS domain is bound to (trust on first use).
-- key is the raw 32-byte Ed25519 key. status: 'trusted' (the domain's
-- current key), 'retired' (an earlier key, replaced through a signed
-- rotation; its old signatures still verify), 'quarantined' (a key change
-- without a valid rotation statement, waiting for an admin) or 'denied'.
-- Key domains (user@<z32 key>) need no row: the domain is the key.
CREATE TABLE federation_peers (
    domain        TEXT NOT NULL,
    key           BLOB NOT NULL,
    first_seen    INTEGER NOT NULL,
    last_verified INTEGER NOT NULL,
    status        TEXT NOT NULL
        CHECK (status IN ('trusted', 'retired', 'quarantined', 'denied')),
    PRIMARY KEY (domain, key)
) WITHOUT ROWID;
CREATE UNIQUE INDEX federation_peers_trusted ON federation_peers(domain)
    WHERE status = 'trusted';

-- Signer info by signer id (SHA-256 of the key), local and remote, so that
-- signatures of old deltas can be checked and served after a key rotates.
-- info is the serialised ProtocolSignerInfo.
CREATE TABLE signer_info (
    signer_id BLOB PRIMARY KEY,
    domain    TEXT NOT NULL,
    key       BLOB NOT NULL,
    info      BLOB NOT NULL
) WITHOUT ROWID;

-- Notifications (applied deltas and commits) a host still has to deliver
-- to a remote domain: one row per domain and wavelet, coalesced, from
-- from_version on. request_id is the ULID of the current attempt.
-- status: 'pending', 'delivered' or 'expired'.
CREATE TABLE federation_outbox (
    domain       TEXT NOT NULL,
    wavelet      TEXT NOT NULL,
    from_version INTEGER NOT NULL,
    request_id   TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    next_at      INTEGER NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending',
    PRIMARY KEY (domain, wavelet)
) WITHOUT ROWID;
CREATE INDEX federation_outbox_due ON federation_outbox(next_at) WHERE status = 'pending';

-- Request ids (ULIDs) of federation requests already applied, so that a
-- retry is answered without applying it twice. Pruned after 30 days.
CREATE TABLE federation_applied (
    request_id TEXT PRIMARY KEY,
    domain     TEXT NOT NULL,
    applied_at INTEGER NOT NULL,
    -- The serialised response sent the first time.
    response   BLOB
) WITHOUT ROWID;
CREATE INDEX federation_applied_at ON federation_applied(applied_at);

-- The remote domain hosting a wavelet held here as a read-only replica;
-- NULL for wavelets this server hosts.
ALTER TABLE wavelets ADD COLUMN hosted_by TEXT;
"#,
    },
    Migration {
        name: "federation: outbox expiry",
        sql: r#"
-- When the oldest notification of an outbox row that is still undelivered
-- was queued, in milliseconds since the epoch; a row expires some days
-- after. NULL once delivered.
ALTER TABLE federation_outbox ADD COLUMN pending_since INTEGER;
"#,
    },
    Migration {
        name: "federation: cached attachments of other servers",
        sql: r#"
-- Attachments fetched from the server that holds them (the domain of the
-- id), kept here as a cache: origin is that domain, NULL for attachments
-- uploaded here. last_used orders the cache for eviction (milliseconds
-- since the epoch). An evicted copy keeps its row, whose sha256 stays
-- pinned (attachments never change), but not its files.
ALTER TABLE attachments ADD COLUMN origin TEXT;
ALTER TABLE attachments ADD COLUMN last_used INTEGER;
ALTER TABLE attachments ADD COLUMN evicted INTEGER NOT NULL DEFAULT 0;
CREATE INDEX attachments_cache ON attachments(last_used)
    WHERE origin IS NOT NULL AND evicted = 0;
"#,
    },
    Migration {
        name: "federation: wavelet epochs and orphaned deltas",
        sql: r#"
-- Who holds the host role of a wavelet from which version on. Epoch 0,
-- the wavelet domain's own server from version 0, is implicit and has no
-- row. statement is the signed EpochStart (federation_rpc.SignedEpochStart)
-- as received, so it can be passed on.
CREATE TABLE wavelet_epochs (
    wavelet       TEXT NOT NULL,
    epoch         INTEGER NOT NULL,
    start_version INTEGER NOT NULL,
    start_hash    BLOB NOT NULL,
    holder        BLOB NOT NULL,
    holder_domain TEXT NOT NULL,
    statement     BLOB NOT NULL,
    PRIMARY KEY (wavelet, epoch)
) WITHOUT ROWID;

-- Applied deltas taken out of a wavelet's history by a rewind to the
-- start of a newer epoch: kept, signed and unchanged, until the current
-- host has them. seq orders them within one fork. status: 'pending' or
-- 'done'.
CREATE TABLE orphan_deltas (
    wavelet      TEXT NOT NULL,
    sha256       BLOB NOT NULL,
    fork_version INTEGER NOT NULL,
    fork_hash    BLOB NOT NULL,
    seq          INTEGER NOT NULL,
    applied      BLOB NOT NULL,
    created_at   INTEGER NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending',
    PRIMARY KEY (wavelet, sha256)
) WITHOUT ROWID;
CREATE INDEX orphan_deltas_pending ON orphan_deltas(wavelet, fork_version, seq)
    WHERE status = 'pending';
"#,
    },
    Migration {
        name: "person identity: device certificates and revocation lists",
        sql: r#"
-- Certificates by which a person key lets a device key act for it
-- (federation_rpc.SignedDeviceCert, kept as signed), learned from Hellos,
-- signer infos and the `person cert` command. Person mode only; server
-- mode pins stay in federation_peers.
CREATE TABLE device_certs (
    device_key BLOB NOT NULL,
    person     BLOB NOT NULL,
    name       TEXT NOT NULL,
    admin      INTEGER NOT NULL,
    issued_at  INTEGER NOT NULL,
    cert       BLOB NOT NULL,
    learned_at INTEGER NOT NULL,
    PRIMARY KEY (device_key, person)
) WITHOUT ROWID;
CREATE INDEX device_certs_person ON device_certs(person);

-- The newest revocation list of each person
-- (federation_rpc.SignedRevocationList, kept as signed). seq only grows.
CREATE TABLE revocations (
    person     BLOB PRIMARY KEY,
    seq        INTEGER NOT NULL,
    list       BLOB NOT NULL,
    learned_at INTEGER NOT NULL
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "pending submissions and host leases",
        sql: r#"
-- Deltas submitted here to wavelets hosted elsewhere whose host could not
-- be reached, kept as signed (a serialised ProtocolSignedDelta) until the
-- host applies them. Sent in seq order per wavelet, each with its
-- request_id, so that the host recognises a retry.
CREATE TABLE pending_submits (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The wavelet's wave:// URI.
    wavelet        TEXT NOT NULL,
    author         TEXT NOT NULL,
    request_id     TEXT NOT NULL UNIQUE,
    signed_delta   BLOB NOT NULL,
    -- The version the delta was made against.
    target_version INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    attempts       INTEGER NOT NULL DEFAULT 0,
    last_error     TEXT
);
CREATE INDEX pending_submits_wavelet ON pending_submits(wavelet, seq);

-- Person mode: the newest host lease known of each person
-- (federation_rpc.SignedHostLease, kept as signed).
CREATE TABLE host_leases (
    person     BLOB PRIMARY KEY,
    lease      BLOB NOT NULL,
    learned_at INTEGER NOT NULL
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "contacts",
        sql: r#"
-- Device mode: addresses an account added to its contacts by hand (by
-- address or contact link). Everyone the account shares a wave with is a
-- contact too, derived from the search index rather than stored here.
CREATE TABLE contacts (
    owner    TEXT NOT NULL,
    address  TEXT NOT NULL,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (owner, address)
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "pairing, device addresses and co-hosts",
        sql: r#"
-- Person mode, admin devices: pairing invitations made here. The secret
-- is needed to check the joiner's MAC, so it is kept until the invitation
-- is used, expires or is cancelled.
CREATE TABLE pair_invitations (
    id          TEXT PRIMARY KEY,
    secret      BLOB NOT NULL,
    admin       INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    -- 'open', 'requested' (an admin pairing waits for confirmation),
    -- 'confirmed', 'used'
    state       TEXT NOT NULL,
    joiner      BLOB,
    joiner_name TEXT,
    failures    INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

-- Person mode: where devices of the person were last reachable
-- (host:port), learned by pairing; this device's own row too.
CREATE TABLE device_addrs (
    device     BLOB NOT NULL,
    addr       TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (device, addr)
) WITHOUT ROWID;

-- Co-host grants (federation_rpc.SignedCoHost, kept as signed): the
-- newest of each person for each delegate domain.
CREATE TABLE co_hosts (
    person          BLOB NOT NULL,
    delegate_domain TEXT NOT NULL,
    issued_at       INTEGER NOT NULL,
    until           INTEGER NOT NULL,
    grant_bytes     BLOB NOT NULL,
    learned_at      INTEGER NOT NULL,
    PRIMARY KEY (person, delegate_domain)
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "pairing by code",
        sql: r#"
-- Person mode, admin devices: an invitation may also be taken by a short
-- code (`<nameplate>-<word>-<word>`). The nameplate finds the inviting
-- device through a rendezvous record and is not secret; `code` holds the
-- words (the SPAKE2 password) as long as the secret is kept. Wrong codes
-- are counted apart from wrong link MACs, with a lower limit.
ALTER TABLE pair_invitations ADD COLUMN nameplate TEXT;
ALTER TABLE pair_invitations ADD COLUMN code BLOB;
ALTER TABLE pair_invitations ADD COLUMN code_failures INTEGER NOT NULL DEFAULT 0;
CREATE INDEX pair_invitations_nameplate ON pair_invitations (nameplate)
    WHERE nameplate IS NOT NULL;
"#,
    },
    Migration {
        name: "push targets",
        sql: r#"
-- How devices are woken through a push relay (federation_rpc.
-- SignedPushTarget, kept as signed): the newest of each device. It holds
-- the token sealed by the relay, never the token. This device's own row
-- also has the SHA-256 of the token it sealed (`token_hash`), to notice
-- a new token.
CREATE TABLE push_targets (
    device     BLOB PRIMARY KEY,
    issued_at  INTEGER NOT NULL,
    relay      TEXT NOT NULL,
    signed     BLOB NOT NULL,
    token_hash BLOB,
    learned_at INTEGER NOT NULL
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "profiles",
        sql: r#"
-- The name and avatar people set for themselves. `own` rows belong to
-- accounts of this domain (set here, or by another device of the same
-- person); the others are copies fetched from the domain of the address
-- (`fetched_at`, for refreshing). `avatar` is a 256x256 PNG made here
-- from the upload or checked when fetched, `avatar_sha256` its hash.
-- `updated_at` (ms, set by the owner) decides which version wins.
CREATE TABLE profiles (
    address       TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    avatar        BLOB,
    avatar_sha256 BLOB,
    updated_at    INTEGER NOT NULL,
    own           INTEGER NOT NULL,
    fetched_at    INTEGER NOT NULL
) WITHOUT ROWID;
"#,
    },
    Migration {
        name: "app kv",
        sql: r#"
-- The key/value store of wave apps (`kv` in resonator-apps): one per
-- account, wave and gadget, local to this node and never federated.
CREATE TABLE app_kv (
    account TEXT NOT NULL,
    wave    TEXT NOT NULL,
    gadget  TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   BLOB NOT NULL,
    PRIMARY KEY (account, wave, gadget, key)
) WITHOUT ROWID;
"#,
    },
];

const INITIAL: Migration = Migration {
    name: "initial schema: meta, wavelets, deltas, snapshots, accounts",
    sql: r#"
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- One row per wavelet that has at least one delta.
CREATE TABLE wavelets (
    id             INTEGER PRIMARY KEY,
    wave_domain    TEXT NOT NULL,
    wave_id        TEXT NOT NULL,
    wavelet_domain TEXT NOT NULL,
    wavelet_id     TEXT NOT NULL,
    UNIQUE (wave_domain, wave_id, wavelet_domain, wavelet_id)
);

-- The delta history. `applied` holds the serialised
-- ProtocolAppliedWaveletDelta exactly as hashed; `transformed` the
-- serialised ProtoTransformedWaveletDelta. Deltas of a wavelet are
-- contiguous, so the delta ending at a version is the one with the greatest
-- applied_at_version below it.
CREATE TABLE deltas (
    wavelet            INTEGER NOT NULL REFERENCES wavelets(id) ON DELETE CASCADE,
    applied_at_version INTEGER NOT NULL,
    resulting_version  INTEGER NOT NULL,
    resulting_hash     BLOB NOT NULL,
    applied            BLOB NOT NULL,
    transformed        BLOB NOT NULL,
    applied_timestamp  INTEGER NOT NULL,
    author             TEXT NOT NULL,
    PRIMARY KEY (wavelet, applied_at_version),
    CHECK (resulting_version > applied_at_version AND applied_at_version >= 0)
) WITHOUT ROWID;

-- Periodic wavelet snapshots: a serialised WaveletSnapshot at a delta
-- boundary, so that loading need not replay the whole history.
CREATE TABLE snapshots (
    wavelet  INTEGER NOT NULL REFERENCES wavelets(id) ON DELETE CASCADE,
    version  INTEGER NOT NULL,
    hash     BLOB NOT NULL,
    snapshot BLOB NOT NULL,
    PRIMARY KEY (wavelet, version)
) WITHOUT ROWID;

CREATE TABLE accounts (
    address               TEXT PRIMARY KEY,
    -- 'human', 'robot' or 'agent'
    kind                  TEXT NOT NULL CHECK (kind IN ('human', 'robot', 'agent')),
    -- Humans: argon2id PHC string, NULL if password login is disabled.
    password_hash         TEXT,
    locale                TEXT,
    -- Robots and agents.
    robot_url             TEXT,
    access_token_hash     TEXT,
    is_verified           INTEGER NOT NULL DEFAULT 0,
    capabilities_hash     TEXT,
    capabilities_version  TEXT
) WITHOUT ROWID;

CREATE TABLE robot_capabilities (
    address    TEXT NOT NULL REFERENCES accounts(address) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    -- Comma-separated context names.
    contexts   TEXT NOT NULL,
    filter     TEXT NOT NULL,
    PRIMARY KEY (address, event_type)
) WITHOUT ROWID;
"#,
};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub(crate) fn schema_version(conn: &Connection) -> Result<u32, StorageError> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Brings the schema up to [`SCHEMA_VERSION`], one transaction per
/// migration. Fails on a database written by a newer build.
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), StorageError> {
    let found = schema_version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(StorageError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let version = index as u32 + 1;
        tracing::info!(version, name = migration.name, "applying schema migration");
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}
