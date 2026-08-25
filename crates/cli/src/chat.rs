//! `rsntr chat`: the reference customer of the public primitives
//! (docs/chat-protocol.md).
//!
//! Everything here is the four verbs: write own tables (`chat_messages`,
//! `chat_rooms`, `chat_members`), INSERT into `_outbox` (every send,
//! online or offline), entrain a Sympathetic point (`chat watch`), and
//! re-read a Radiant (`chat log`). No chat daemon exists.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use ulid::Ulid;

use resonator_node::chat::{
    BODY_MAX_BYTES, BlobAttachment, CHAT_INBOX_POINT, CHAT_TABLES_DDL, ChatMessage,
    DIRECT_RESOURCE, message_to_turtle,
};
use resonator_node::clock::now_rfc3339;
use resonator_protocol::{EnvelopeObject, Request, RequestKind, Value};
use resonator_transport::{IrohConfig, IrohTransport, PeerId, RequestStream, Transport};

use crate::channel::{self, OwnerChannel, Prefer};
use crate::client::QueryOutcome;
use crate::store;

/// The room IRI scheme (chat protocol sec 11).
pub const ROOM_IRI_PREFIX: &str = "urn:rsntr:room:";

fn chat_ready(conn: &Connection) -> Result<()> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'chat_messages'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        bail!("chat is not scaffolded here; run `rsntr chat init <dir>` first");
    }
    Ok(())
}

/// The idempotent `_policy` allow SQL of the chat scaffold.
const POLICY_UPSERT_SQL: &str = "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
     SELECT ?1, ?2, ?3, 'allow', ?4 \
     WHERE NOT EXISTS (SELECT 1 FROM _policy \
        WHERE peer_or_group = ?1 AND table_name = ?2 AND action = ?3 AND effect = 'allow')";

/// Inserts a `_policy` allow row unless an identical one already exists.
/// Direct-connection variant, used by `chat watch`'s ephemeral admission.
fn ensure_policy(
    conn: &Connection,
    peer: &str,
    table: &str,
    action: &str,
    note: &str,
) -> Result<()> {
    conn.execute(POLICY_UPSERT_SQL, (peer, table, action, note))?;
    Ok(())
}

/// [`ensure_policy`] as an owner-channel Execute (`rsntr chat init`).
async fn ensure_policy_owner(
    ch: &OwnerChannel,
    peer: &str,
    table: &str,
    action: &str,
    note: &str,
) -> Result<()> {
    channel::execute(
        ch,
        POLICY_UPSERT_SQL,
        vec![
            Value::Text(peer.to_string()),
            Value::Text(table.to_string()),
            Value::Text(action.to_string()),
            Value::Text(note.to_string()),
        ],
    )
    .await?;
    Ok(())
}

/// The chat-scaffold projection rows (chat protocol sec 7.2), one
/// statement each so they ride the owner channel verbatim.
const CHAT_PROJECTION_SQL: [&str; 3] = [
    "INSERT OR IGNORE INTO _projection \
       (point_iri, kind, label, icon, ord, modulation, signal_template, fields, resource, note) \
     VALUES ('urn:rsntr:chat:send', 'excitable', 'send me a message', 'chat', 100, 'chat', \
       '[] a rsntr:Message ; rsntr:body \"{body}\" .', \
       '[{\"name\":\"body\",\"datatype\":\"http://www.w3.org/2001/XMLSchema#string\",\
          \"required\":true,\"hint\":\"message text\"}]', \
       'chat:direct', 'rsntr chat init')",
    "INSERT OR IGNORE INTO _projection (point_iri, kind, label, ord, resource, note) \
     VALUES ('urn:rsntr:chat-inbox', 'sympathetic', 'a message arrived', 101, \
       'chat_messages', 'rsntr chat init')",
    "INSERT OR IGNORE INTO _projection \
       (point_iri, kind, label, ord, modulation, signal, params_order, fields, resource, note) \
     VALUES ('urn:rsntr:chat:history', 'radiant', 'chat history', 102, 'sql-sqlite', \
       'SELECT id, scope, sender, at, body, blob_hash, blob_name \
          FROM chat_messages \
         WHERE (?1 IS NULL OR scope = ?1) \
         ORDER BY received_at DESC, id DESC LIMIT ?2', \
       '[\"scope\",\"limit\"]', \
       '[{\"name\":\"scope\",\"datatype\":\"http://www.w3.org/2001/XMLSchema#string\",\
          \"required\":false,\"hint\":\"peer endpoint or room IRI; empty = all\"},\
         {\"name\":\"limit\",\"datatype\":\"http://www.w3.org/2001/XMLSchema#integer\",\
          \"required\":false,\"default\":\"50\"}]', \
       'chat_messages', 'rsntr chat init')",
];

/// `rsntr chat init <dir>` (chat protocol sec 7): tables, projection
/// points, policy rows, mods entry, all as owner Executes (DDL is
/// permitted on the owner channel; `_outbox`/`_results` exist since
/// `rsntr init`). Idempotent.
pub fn chat_init(dir: &Path) -> Result<PeerId> {
    chat_init_with(dir, Prefer::Auto)
}

/// [`chat_init`] with an explicit transport preference.
pub fn chat_init_with(dir: &Path, prefer: Prefer) -> Result<PeerId> {
    let self_id = store::node_id(dir)?;
    let self_hex = self_id.to_string();

    // Local read: the advertised modulations (write rides the channel).
    let mods_update = {
        let conn = store::open_db(dir)?;
        let raw =
            resonator_node::get_rsntr(&conn, "modulations").unwrap_or_else(|| "[]".to_string());
        let mut mods: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        if mods.iter().any(|m| m == "chat") {
            None
        } else {
            mods.push("chat".to_string());
            Some(serde_json::to_string(&mods)?)
        }
    };

    channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        // One Execute per DDL statement (the request is the statement).
        for stmt in CHAT_TABLES_DDL.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            channel::execute(&ch, stmt, vec![])
                .await
                .context("creating the chat tables")?;
        }
        for sql in CHAT_PROJECTION_SQL {
            channel::execute(&ch, sql, vec![]).await?;
        }

        // Table descriptions (the console mandates them; scaffolds
        // therefore ship them).
        for (table, comment) in [
            ("chat_messages", "chat history: every message sent or received, by scope (peer or room)"),
            ("chat_rooms", "chat rooms this node knows: room IRI, display name, hosting peer"),
            ("chat_members", "room membership as known locally: who is in which room"),
        ] {
            channel::execute(
                &ch,
                "INSERT OR IGNORE INTO _comments (name, comment) VALUES (?1, ?2)",
                vec![Value::Text(table.to_string()), Value::Text(comment.to_string())],
            )
            .await?;
        }

        // Policy (sec 7.3): DMs open to admitted peers; the node's own
        // surfaces may read and entrain its chat.
        ensure_policy_owner(
            &ch,
            "*",
            DIRECT_RESOURCE,
            "chat",
            "rsntr chat init: DMs open to admitted peers",
        )
        .await?;
        ensure_policy_owner(
            &ch,
            &self_hex,
            "chat_messages",
            "read",
            "rsntr chat init: own history",
        )
        .await?;
        ensure_policy_owner(
            &ch,
            &self_hex,
            "chat_messages",
            "entrain",
            "rsntr chat init: own inbox",
        )
        .await?;

        // Advertise the modulation (sec 3): append `chat` to
        // `_rsntr.modulations` when an older init predates the builtin.
        if let Some(json) = mods_update {
            channel::execute(
                &ch,
                "INSERT INTO _rsntr (key, value) VALUES ('modulations', ?1) \
                 ON CONFLICT(key) DO UPDATE SET value = ?1",
                vec![Value::Text(json)],
            )
            .await
            .context("writing _rsntr.modulations")?;
        }
        Ok::<(), anyhow::Error>(())
    })??;

    Ok(self_id)
}

/// A resolved send/log target (chat protocol sec 8): rooms first, then
/// peers.
#[derive(Debug, Clone)]
pub enum Target {
    Peer {
        id: PeerId,
    },
    Room {
        room_id: String,
        name: String,
        host: String,
    },
}

impl Target {
    /// The `chat_messages.scope` value of this target.
    pub fn scope(&self) -> String {
        match self {
            Target::Peer { id } => id.to_string(),
            Target::Room { room_id, .. } => room_id.clone(),
        }
    }
}

/// Resolves `target`: a room IRI, a room name, a peer petname, or a
/// 64-hex endpoint id. Rooms are checked first; an ambiguous room name
/// requires the IRI.
pub fn resolve_target(conn: &Connection, target: &str) -> Result<Target> {
    let room_rows = |sql: &str| -> Result<Vec<(String, String, String)>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([target], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    };
    let mut rooms = room_rows("SELECT room_id, name, host FROM chat_rooms WHERE room_id = ?1")?;
    if rooms.is_empty() {
        rooms = room_rows("SELECT room_id, name, host FROM chat_rooms WHERE name = ?1")?;
    }
    match rooms.as_slice() {
        [] => {
            let (id, _addrs) = store::resolve_peer(conn, target)?;
            Ok(Target::Peer { id })
        }
        [(room_id, name, host)] => Ok(Target::Room {
            room_id: room_id.clone(),
            name: name.clone(),
            host: host.clone(),
        }),
        many => bail!(
            "room name {target:?} matches {} rooms; use the room IRI",
            many.len()
        ),
    }
}

/// Best-effort MIME type from the file extension; display metadata only.
fn guess_content_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("mp4") => "video/mp4",
        Some("ts") => "video/mp2t",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// What `rsntr chat send` did.
#[derive(Debug)]
pub struct SendReport {
    /// The message ULID (also the `_outbox.request_id` of the direct
    /// hop, so `chat log` joins delivery status).
    pub message_id: String,
    /// The stored scope: peer endpoint id or room IRI.
    pub scope: String,
    /// `(blake3:<hex>, bytes)` of the imported attachment, if any.
    pub blob: Option<(String, u64)>,
    /// Peers a delivery was enqueued to (fan-out when hosting a room).
    pub queued_to: Vec<String>,
}

/// Streaming BLAKE3 of a file, no blob store and no lock: the hash the
/// message's BlobRef carries when the import itself is delegated to the
/// serving process (docs/owner-channel.md sec 5.3).
fn hash_file_blake3(path: &Path) -> Result<(String, u64)> {
    let mut hasher = blake3::Hasher::new();
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let bytes = std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("hashing {}", path.display()))?;
    Ok((hasher.finalize().to_hex().to_string(), bytes))
}

/// `rsntr chat send <target> <text> [--file <path>]`: append locally and
/// enqueue in `_outbox` (chat protocol sec 4.1), all as owner-channel
/// Executes; touches no network. Over the control socket the append
/// commits on the serving connection, so the inbox point vibrates and
/// the outbox worker wakes.
pub async fn chat_send(
    dir: &Path,
    target: &str,
    text: &str,
    file: Option<&Path>,
) -> Result<SendReport> {
    chat_send_with(dir, Prefer::Auto, target, text, file).await
}

/// [`chat_send`] with an explicit transport preference.
pub async fn chat_send_with(
    dir: &Path,
    prefer: Prefer,
    target: &str,
    text: &str,
    file: Option<&Path>,
) -> Result<SendReport> {
    if text.len() > BODY_MAX_BYTES {
        bail!(
            "message body is {} bytes; the ceiling is {BODY_MAX_BYTES} \
             (anything bigger belongs in an attachment)",
            text.len()
        );
    }

    let self_id = store::node_id(dir)?.to_string();
    let target = {
        let conn = store::open_db(dir)?;
        chat_ready(&conn)?;
        resolve_target(&conn, target)?
    };

    let ch = OwnerChannel::open(dir, prefer).await?;

    // The attachment. Over the socket the serving process holds the blob
    // store's lock, so the CLI hashes natively and delegates the import
    // to the node (the owner-channel-only path parameter below); in
    // process the lock is free and the CLI imports directly, as before.
    let mut import_path: Option<std::path::PathBuf> = None;
    let attachment = match file {
        None => None,
        Some(path) if ch.is_socket() => {
            let (hash, bytes) = hash_file_blake3(path)?;
            import_path = Some(
                std::path::absolute(path)
                    .with_context(|| format!("resolving {}", path.display()))?,
            );
            Some(BlobAttachment {
                hash: format!("blake3:{hash}"),
                bytes: Some(bytes as i64),
                name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
                content_type: Some(guess_content_type(path)),
            })
        }
        Some(path) => {
            let (hash, bytes) = resonator_transport::blob_import(&dir.join("blobs"), path)
                .await
                .map_err(|e| anyhow::anyhow!("importing {}: {e}", path.display()))?;
            Some(BlobAttachment {
                hash: format!("blake3:{hash}"),
                bytes: Some(bytes as i64),
                name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
                content_type: Some(guess_content_type(path)),
            })
        }
    };

    let message_id = Ulid::new().to_string();
    let at = now_rfc3339();
    let scope = target.scope();

    // The local append (its commit is what vibrates the inbox point on
    // the socket transport).
    channel::execute(
        &ch,
        "INSERT INTO chat_messages \
           (id, scope, sender, at, body, blob_hash, blob_bytes, blob_name, blob_type, outgoing) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
        vec![
            Value::Text(message_id.clone()),
            Value::Text(scope.clone()),
            Value::Text(self_id.clone()),
            Value::Text(at.clone()),
            Value::Text(text.to_string()),
            attachment
                .as_ref()
                .map(|b| Value::Text(b.hash.clone()))
                .unwrap_or(Value::Null),
            attachment
                .as_ref()
                .and_then(|b| b.bytes)
                .map(Value::Integer)
                .unwrap_or(Value::Null),
            attachment
                .as_ref()
                .and_then(|b| b.name.clone())
                .map(Value::Text)
                .unwrap_or(Value::Null),
            attachment
                .as_ref()
                .and_then(|b| b.content_type.clone())
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ],
    )
    .await
    .context("appending the message")?;

    // Socket + --file: one chat Execute to self whose single parameter
    // is the source path (owner-channel-only affordance, doc sec 5.3).
    // The serving process, which holds the redb lock, imports the file
    // into its blob store; the append itself is a no-op because the
    // message id is already recorded (message-level idempotency).
    if let Some(path) = import_path {
        let msg = ChatMessage {
            id: Some(message_id.clone()),
            at: Some(at.clone()),
            body: text.to_string(),
            attachment: attachment.clone(),
            ..ChatMessage::default()
        };
        let mut request = Request::new(RequestKind::Execute, "chat", message_to_turtle(&msg));
        request.params = vec![Value::Text(path.to_string_lossy().into_owned())];
        let id = request.id_string();
        match ch.send(&request.to_envelope(), &id).await? {
            QueryOutcome::Rows { .. } => {}
            QueryOutcome::Denied(d) => bail!(
                "attachment import denied: {}",
                d.reason.as_deref().unwrap_or("(no reason given)")
            ),
            QueryOutcome::Failed(e) => bail!(
                "attachment import failed [{}]: {}",
                e.code,
                e.reason.unwrap_or_default()
            ),
            other => bail!("unexpected attachment import response: {other:?}"),
        }
    }

    // The delivery enqueue: one `_outbox` Execute per recipient.
    let outbox_sql =
        "INSERT INTO _outbox (request_id, peer, mod, signal) VALUES (?1, ?2, 'chat', ?3)";
    let mut queued_to = Vec::new();
    match &target {
        Target::Peer { id } => {
            // Direct hop: the message ULID doubles as the request id.
            let msg = ChatMessage {
                id: Some(message_id.clone()),
                at: Some(at.clone()),
                body: text.to_string(),
                attachment: attachment.clone(),
                ..ChatMessage::default()
            };
            channel::execute(
                &ch,
                outbox_sql,
                vec![
                    Value::Text(message_id.clone()),
                    Value::Text(id.to_string()),
                    Value::Text(message_to_turtle(&msg)),
                ],
            )
            .await
            .context("enqueueing the delivery")?;
            queued_to.push(id.to_string());
        }
        Target::Room {
            room_id,
            name,
            host,
        } => {
            if host == &self_id {
                // Hosting: local append is authoritative; fan out to
                // every member but the author (sec 5.3).
                let msg = ChatMessage {
                    id: Some(message_id.clone()),
                    from: Some(self_id.clone()),
                    room: Some(room_id.clone()),
                    at: Some(at.clone()),
                    body: text.to_string(),
                    attachment: attachment.clone(),
                    room_name: Some(name.clone()),
                };
                let signal = message_to_turtle(&msg);
                let members: Vec<String> = {
                    let conn = store::open_db(dir)?;
                    let mut stmt = conn.prepare(
                        "SELECT member FROM chat_members WHERE room_id = ?1 AND member != ?2",
                    )?;
                    let rows = stmt.query_map((room_id, &self_id), |r| r.get::<_, String>(0))?;
                    rows.filter_map(std::result::Result::ok).collect()
                };
                for member in members {
                    channel::execute(
                        &ch,
                        outbox_sql,
                        vec![
                            Value::Text(Ulid::new().to_string()),
                            Value::Text(member.clone()),
                            Value::Text(signal.clone()),
                        ],
                    )
                    .await
                    .context("enqueueing the fan-out")?;
                    queued_to.push(member);
                }
            } else {
                // Member: one hop to the host, rsntr:room set (sec 5.2).
                let msg = ChatMessage {
                    id: Some(message_id.clone()),
                    room: Some(room_id.clone()),
                    at: Some(at.clone()),
                    body: text.to_string(),
                    attachment: attachment.clone(),
                    ..ChatMessage::default()
                };
                channel::execute(
                    &ch,
                    outbox_sql,
                    vec![
                        Value::Text(message_id.clone()),
                        Value::Text(host.clone()),
                        Value::Text(message_to_turtle(&msg)),
                    ],
                )
                .await
                .context("enqueueing the delivery")?;
                queued_to.push(host.clone());
            }
        }
    }

    Ok(SendReport {
        message_id,
        scope,
        blob: attachment.map(|b| (b.hash, b.bytes.unwrap_or(0) as u64)),
        queued_to,
    })
}

/// One `chat log` line.
#[derive(Debug)]
pub struct LogEntry {
    pub id: String,
    pub scope: String,
    pub sender: String,
    pub at: String,
    pub received_at: String,
    pub body: String,
    pub blob_hash: Option<String>,
    pub blob_name: Option<String>,
    pub outgoing: bool,
    /// `_outbox.status` of own outgoing messages
    /// (queued/sent/done/denied/error/expired), when joinable.
    pub status: Option<String>,
}

/// `rsntr chat log <target>`: a local read of `chat_messages` for that
/// scope, newest first, joined with delivery status.
pub fn chat_log(dir: &Path, target: &str, limit: i64) -> Result<Vec<LogEntry>> {
    let conn = store::open_db(dir)?;
    chat_ready(&conn)?;
    let scope = resolve_target(&conn, target)?.scope();
    let mut stmt = conn.prepare(
        "SELECT m.id, m.scope, m.sender, m.at, m.received_at, m.body, \
                m.blob_hash, m.blob_name, m.outgoing, o.status \
         FROM chat_messages m LEFT JOIN _outbox o ON o.request_id = m.id \
         WHERE m.scope = ?1 \
         ORDER BY m.received_at DESC, m.id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map((scope, limit), |r| {
            Ok(LogEntry {
                id: r.get(0)?,
                scope: r.get(1)?,
                sender: r.get(2)?,
                at: r.get(3)?,
                received_at: r.get(4)?,
                body: r.get(5)?,
                blob_hash: r.get(6)?,
                blob_name: r.get(7)?,
                outgoing: r.get::<_, i64>(8)? != 0,
                status: r.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One event from `rsntr chat watch`.
#[derive(Debug)]
pub enum WatchEvent {
    /// Entrained to the local inbox point.
    Entrained,
    /// A new message in the watched scope.
    Message(LogEntry),
    /// The damp was confirmed; the watch is over.
    Damped,
    Denied(String),
    Failed(String),
}

/// The `_policy.note` marking the ephemeral watch rows for cleanup.
const WATCH_NOTE: &str = "rsntr chat watch (ephemeral surface)";

/// `rsntr chat watch <target>`: dial the local serving node, entrain
/// `<urn:rsntr:chat-inbox>`, and on each vibration re-read history from
/// the cursor, emitting new messages for that scope.
///
/// The chat protocol's self-dial (sec 4.3, the node's own key) is not
/// possible over iroh, which refuses connecting to its own endpoint id.
/// Instead the watch mints an ephemeral identity and, holding the same
/// database the serving node reads policy from, admits it for exactly
/// `read`/`entrain` on `chat_messages` for the watch's lifetime; the
/// rows are removed on exit.
pub async fn chat_watch(
    dir: std::path::PathBuf,
    target: String,
    tx: tokio::sync::mpsc::Sender<WatchEvent>,
    mut damp: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    let conn = store::open_db(&dir)?;
    chat_ready(&conn)?;
    let scope = resolve_target(&conn, &target)?.scope();
    let self_id = store::node_id(&dir)?;
    let addrs: Vec<std::net::SocketAddr> = resonator_node::get_rsntr(&conn, "serving_addrs")
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .map(|texts| texts.iter().filter_map(|t| t.parse().ok()).collect())
        .unwrap_or_default();
    if addrs.is_empty() {
        bail!(
            "no serving node found for {}; start one with `rsntr serve {}`",
            dir.display(),
            dir.display()
        );
    }
    let mut cursor: i64 = conn
        .query_row(
            "SELECT COALESCE(max(rowid), 0) FROM chat_messages",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let hello = store::hello_from_db(&conn);

    // Loopback dial: the serving endpoint's stored listen addrs are
    // local by construction, so the offline preset is always right here.
    let config = IrohConfig {
        hello,
        secret_key: None,
        offline: true,
        relays: Vec::new(),
        gossip: false,
        blobs: None,
    };
    let (transport, _incoming) = IrohTransport::bind(config)
        .await
        .map_err(|e| anyhow::anyhow!("binding the watch endpoint: {e}"))?;
    transport.add_peer_addrs(self_id, addrs);

    // Admit the ephemeral watch identity for this watch only.
    let watch_hex = transport.peer_id().to_string();
    conn.execute(
        "INSERT OR IGNORE INTO _peers (endpoint_id, name, added_at, notes) \
         VALUES (?1, '_watch', datetime('now'), ?2)",
        (&watch_hex, WATCH_NOTE),
    )?;
    ensure_policy(&conn, &watch_hex, "chat_messages", "read", WATCH_NOTE)?;
    ensure_policy(&conn, &watch_hex, "chat_messages", "entrain", WATCH_NOTE)?;
    // A sqlite Connection is not Sync, so it cannot live inside this
    // Send future; each catch-up read opens its own (WAL, cheap).
    drop(conn);

    let entrain_id = Ulid::new().to_string();
    let result = async {
        let (mut stream, _hello) = transport.open(self_id).await.map_err(|e| {
            anyhow::anyhow!(
                "no node is serving {} (self-dial failed: {e})",
                dir.display()
            )
        })?;
        stream
            .send(&EnvelopeObject::Entrain(resonator_protocol::Entrain {
                id: entrain_id.clone(),
                point: CHAT_INBOX_POINT.to_string(),
            }))
            .await
            .map_err(|e| anyhow::anyhow!("sending the entrain request: {e}"))?;

        let mut reader = resonator_protocol::EntrainmentReader::new(&entrain_id, CHAT_INBOX_POINT);
        let mut damp_armed = true;
        loop {
            tokio::select! {
                frame = stream.recv() => {
                    let Some(frame) = frame
                        .map_err(|e| anyhow::anyhow!("receiving from the watch stream: {e}"))?
                    else {
                        break;
                    };
                    use resonator_protocol::EntrainmentEvent as Ev;
                    match reader.accept(frame).context("entrainment choreography")? {
                        Ev::Entrained => {
                            if tx.send(WatchEvent::Entrained).await.is_err() {
                                break;
                            }
                        }
                        Ev::Vibration(_) => {
                            let (new_cursor, entries) = read_since(&dir, cursor, &scope)?;
                            cursor = new_cursor;
                            for entry in entries {
                                if tx.send(WatchEvent::Message(entry)).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        Ev::Damped => {
                            let _ = tx.send(WatchEvent::Damped).await;
                            break;
                        }
                        Ev::Denied(d) => {
                            let _ = tx
                                .send(WatchEvent::Denied(
                                    d.reason.unwrap_or_else(|| "denied".to_string()),
                                ))
                                .await;
                            break;
                        }
                        Ev::Error(e) => {
                            let _ = tx
                                .send(WatchEvent::Failed(format!(
                                    "{}: {}",
                                    e.code,
                                    e.reason.unwrap_or_default()
                                )))
                                .await;
                            break;
                        }
                    }
                }
                fired = &mut damp, if damp_armed => {
                    damp_armed = false;
                    if fired.is_ok() {
                        stream
                            .send(&EnvelopeObject::Damp(resonator_protocol::Damp {
                                id: Some(entrain_id.clone()),
                                point: CHAT_INBOX_POINT.to_string(),
                            }))
                            .await
                            .map_err(|e| anyhow::anyhow!("sending the damp: {e}"))?;
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    transport.shutdown().await;
    // Best-effort cleanup of the ephemeral admission.
    if let Ok(conn) = store::open_db(&dir) {
        let _ = conn.execute("DELETE FROM _peers WHERE endpoint_id = ?1", [&watch_hex]);
        let _ = conn.execute(
            "DELETE FROM _policy WHERE peer_or_group = ?1 AND note = ?2",
            (&watch_hex, WATCH_NOTE),
        );
    }
    result
}

/// Rows past `cursor` (by rowid) for `scope`; returns the advanced
/// cursor over all rows seen, matching or not.
fn read_since(dir: &Path, cursor: i64, scope: &str) -> Result<(i64, Vec<LogEntry>)> {
    let conn = store::open_db(dir)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, id, scope, sender, at, received_at, body, blob_hash, blob_name, outgoing \
         FROM chat_messages WHERE rowid > ?1 ORDER BY rowid",
    )?;
    let mut new_cursor = cursor;
    let mut out = Vec::new();
    let rows = stmt.query_map([cursor], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            LogEntry {
                id: r.get(1)?,
                scope: r.get(2)?,
                sender: r.get(3)?,
                at: r.get(4)?,
                received_at: r.get(5)?,
                body: r.get(6)?,
                blob_hash: r.get(7)?,
                blob_name: r.get(8)?,
                outgoing: r.get::<_, i64>(9)? != 0,
                status: None,
            },
        ))
    })?;
    for row in rows {
        let (rowid, entry) = row?;
        new_cursor = new_cursor.max(rowid);
        if entry.scope == scope {
            out.push(entry);
        }
    }
    Ok((new_cursor, out))
}

/// `rsntr chat room create <name>`: mint the room IRI; this node hosts.
pub fn room_create(dir: &Path, name: &str) -> Result<String> {
    room_create_with(dir, Prefer::Auto, name)
}

/// [`room_create`] with an explicit transport preference.
pub fn room_create_with(dir: &Path, prefer: Prefer, name: &str) -> Result<String> {
    let self_id = store::node_id(dir)?.to_string();
    {
        let conn = store::open_db(dir)?;
        chat_ready(&conn)?;
    }
    let room_id = format!("{ROOM_IRI_PREFIX}{}", Ulid::new());
    let room = room_id.clone();
    channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::execute(
            &ch,
            "INSERT INTO chat_rooms (room_id, name, host) VALUES (?1, ?2, ?3)",
            vec![
                Value::Text(room.clone()),
                Value::Text(name.to_string()),
                Value::Text(self_id.clone()),
            ],
        )
        .await?;
        channel::execute(
            &ch,
            "INSERT INTO chat_members (room_id, member) VALUES (?1, ?2)",
            vec![Value::Text(room), Value::Text(self_id)],
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    })??;
    Ok(room_id)
}

/// `rsntr chat room add <room> <peer>` (host only): one roster row plus
/// one membership policy row (chat protocol sec 5.1). The peer must
/// already be admitted.
pub fn room_add(dir: &Path, room: &str, peer: &str) -> Result<(String, String)> {
    room_add_with(dir, Prefer::Auto, room, peer)
}

/// [`room_add`] with an explicit transport preference.
pub fn room_add_with(
    dir: &Path,
    prefer: Prefer,
    room: &str,
    peer: &str,
) -> Result<(String, String)> {
    let self_id = store::node_id(dir)?.to_string();
    let conn = store::open_db(dir)?;
    chat_ready(&conn)?;
    let Target::Room { room_id, host, .. } = resolve_target(&conn, room)? else {
        bail!("no room named {room:?}");
    };
    if host != self_id {
        bail!("this node does not host {room_id}; only the host manages the roster");
    }
    let (peer_id, _addrs) = store::resolve_peer(&conn, peer)?;
    let peer_hex = peer_id.to_string();
    let admitted: i64 = conn.query_row(
        "SELECT count(*) FROM _peers WHERE endpoint_id = ?1",
        [&peer_hex],
        |r| r.get(0),
    )?;
    if admitted == 0 {
        bail!("{peer:?} is not an admitted peer; `rsntr peer add` it first");
    }
    drop(conn);
    {
        let room = room_id.clone();
        let member = peer_hex.clone();
        channel::block_on(async move {
            let ch = OwnerChannel::open(dir, prefer).await?;
            channel::execute(
                &ch,
                "INSERT OR IGNORE INTO chat_members (room_id, member) VALUES (?1, ?2)",
                vec![Value::Text(room.clone()), Value::Text(member.clone())],
            )
            .await?;
            ensure_policy_owner(
                &ch,
                &member,
                &room,
                "chat",
                "chat room member (rsntr chat room add)",
            )
            .await?;
            Ok::<(), anyhow::Error>(())
        })??;
    }
    Ok((room_id, peer_hex))
}

/// `rsntr chat room join <peer> <room-iri>` (member side): record the
/// host binding locally (chat protocol sec 5.5). Purely local.
pub fn room_join(dir: &Path, peer: &str, room_iri: &str, name: Option<&str>) -> Result<String> {
    room_join_with(dir, Prefer::Auto, peer, room_iri, name)
}

/// [`room_join`] with an explicit transport preference.
pub fn room_join_with(
    dir: &Path,
    prefer: Prefer,
    peer: &str,
    room_iri: &str,
    name: Option<&str>,
) -> Result<String> {
    let host = {
        let conn = store::open_db(dir)?;
        chat_ready(&conn)?;
        store::resolve_peer(&conn, peer)?.0
    };
    let host_hex = host.to_string();
    {
        let host_param = host_hex.clone();
        channel::block_on(async move {
            let ch = OwnerChannel::open(dir, prefer).await?;
            channel::execute(
                &ch,
                "INSERT INTO chat_rooms (room_id, name, host) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(room_id) DO UPDATE SET name = ?2, host = ?3",
                vec![
                    Value::Text(room_iri.to_string()),
                    Value::Text(name.unwrap_or(room_iri).to_string()),
                    Value::Text(host_param),
                ],
            )
            .await
        })??;
    }
    Ok(host_hex)
}
