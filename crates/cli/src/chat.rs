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
    BODY_MAX_BYTES, BlobAttachment, CHAT_INBOX_POINT, CHAT_TABLES_DDL, ChatMessage, ChatParseError,
    DIRECT_RESOURCE, ensure_chat_columns, message_to_turtle, tags_from_json, tags_to_json,
    validate_thread_fields,
};
use resonator_node::clock::now_rfc3339;
use resonator_protocol::{EnvelopeObject, Request, RequestKind, Value};
use resonator_transport::{PeerId, RequestStream};

use crate::channel::{self, OwnerChannel, Prefer};
use crate::client::QueryOutcome;
use crate::store;

/// The room IRI scheme (chat protocol sec 11).
pub const ROOM_IRI_PREFIX: &str = "urn:rsntr:room:";

/// The attachment name marking a body auto-spilled by `chat send` (the
/// wire body is a preview; the full text is the blob). Readers
/// ([`inline_spilled`]) fetch and inline it transparently.
pub const SPILL_NAME: &str = "rsntr-spilled-body.txt";

/// The preview kept in the wire body of a spilled message.
const SPILL_PREVIEW_BYTES: usize = 1024;

/// [`inline_spilled`] fetches at most this many bytes per message;
/// bigger spills stay a preview plus BlobRef.
pub const INLINE_MAX_BYTES: i64 = 4 * 1024 * 1024;

fn chat_ready(conn: &Connection) -> Result<()> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'chat_messages'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        bail!("chat is not scaffolded here; run `rsntr chat init <dir>` first");
    }
    // A scaffold from before threading gains re_id/tags on first use.
    ensure_chat_columns(conn).context("migrating chat_messages")?;
    Ok(())
}

/// The idempotent `_policy` allow SQL of the chat scaffold.
const POLICY_UPSERT_SQL: &str = "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
     SELECT ?1, ?2, ?3, 'allow', ?4 \
     WHERE NOT EXISTS (SELECT 1 FROM _policy \
        WHERE peer_or_group = ?1 AND table_name = ?2 AND action = ?3 AND effect = 'allow')";

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

/// The SQL-side chat log view; owned by the node crate's chat migration
/// so every opener of an older database gets it, not only `chat init`.
pub use resonator_node::chat::CHAT_LOG_VIEW_DDL;

/// `rsntr chat init <dir>` (chat protocol sec 7): tables, projection
/// points, policy rows, mods entry, all as owner Executes (DDL is
/// permitted on the owner channel; `_outbox`/`_results` exist since
/// `rsntr init`). Idempotent; re-running it on an older scaffold is
/// the migration path (threading columns, the `chat_log_v` view).
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
        // Older scaffolds gain the threading columns before the view
        // (whose `m.*` must see them). Local connection: ALTER TABLE is
        // idempotent and schema changes are visible to the daemon.
        {
            let conn = store::open_db(dir)?;
            ensure_chat_columns(&conn).context("migrating chat_messages")?;
        }
        channel::execute(&ch, CHAT_LOG_VIEW_DDL, vec![])
            .await
            .context("creating chat_log_v")?;
        for sql in CHAT_PROJECTION_SQL {
            channel::execute(&ch, sql, vec![]).await?;
        }

        // Table descriptions (the console mandates them; scaffolds
        // therefore ship them).
        for (table, comment) in [
            (
                "chat_messages",
                "chat history: every message sent or received, by scope (peer or room)",
            ),
            (
                "chat_rooms",
                "chat rooms this node knows: room IRI, display name, hosting peer",
            ),
            (
                "chat_members",
                "room membership as known locally: who is in which room",
            ),
            (
                "chat_log_v",
                "view: chat_messages joined with the sender's petname and delivery status",
            ),
        ] {
            channel::execute(
                &ch,
                "INSERT OR IGNORE INTO _comments (name, comment) VALUES (?1, ?2)",
                vec![
                    Value::Text(table.to_string()),
                    Value::Text(comment.to_string()),
                ],
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
    /// The body exceeded [`BODY_MAX_BYTES`] and went out as a text-blob
    /// attachment with a preview body; local history keeps the full
    /// text.
    pub spilled: bool,
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

/// The staging directory for attachments handed to another process's
/// blob store (chat protocol sec 6; docs/shared-node.md).
pub const BLOBS_INBOX_DIR: &str = "blobs-inbox";

/// Copies `src` to `<dir>/blobs-inbox/<hash>` (tmp+rename) and returns
/// the absolute staged path.
fn stage_attachment(dir: &Path, src: &Path, hash: &str) -> Result<std::path::PathBuf> {
    let inbox = dir.join(BLOBS_INBOX_DIR);
    std::fs::create_dir_all(&inbox).with_context(|| format!("creating {}", inbox.display()))?;
    let staged = inbox.join(hash);
    if !staged.exists() {
        let tmp = inbox.join(format!("{hash}.tmp-{}", std::process::id()));
        std::fs::copy(src, &tmp).with_context(|| format!("staging {}", src.display()))?;
        std::fs::rename(&tmp, &staged).with_context(|| format!("staging {}", staged.display()))?;
    }
    std::path::absolute(&staged).with_context(|| format!("resolving {}", staged.display()))
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
    chat_send_opts(dir, prefer, target, text, file, &SendOptions::default()).await
}

/// Threading options of a send (`--re`, `--tag`).
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    /// The id of the message this one answers (`rsntr:re`).
    pub re: Option<String>,
    /// Tags (`rsntr:tag`); normalized (sorted, deduplicated) on send.
    pub tags: Vec<String>,
}

/// [`chat_send_with`] plus threading fields.
pub async fn chat_send_opts(
    dir: &Path,
    prefer: Prefer,
    target: &str,
    text: &str,
    file: Option<&Path>,
    opts: &SendOptions,
) -> Result<SendReport> {
    let self_id = store::node_id(dir)?.to_string();
    let target = {
        let conn = store::open_db(dir)?;
        chat_ready(&conn)?;
        resolve_target(&conn, target)?
    };
    let mut tags = opts.tags.clone();
    validate_thread_fields(opts.re.as_deref(), &mut tags).map_err(|e| match e {
        ChatParseError::Protocol(m) | ChatParseError::Limit(m) => anyhow::anyhow!(m),
    })?;
    let re = opts.re.clone();

    // Auto-spill: a body over the wire ceiling goes out as a text-blob
    // attachment with a preview body; local history keeps the full text.
    // Removed on every exit, so a failed send leaves no spill behind.
    let mut spill_tmp = RemoveOnDrop(None);
    let (wire_body, attach_path, attach_name, attach_type) = if text.len() > BODY_MAX_BYTES {
        if file.is_some() {
            bail!(
                "the body is {} bytes (ceiling {BODY_MAX_BYTES}) and an attachment is \
                 already given; put the big text in the --file instead",
                text.len()
            );
        }
        let tmp = dir.join(format!(".rsntr-spill-{}.txt", Ulid::new()));
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        spill_tmp.0 = Some(tmp.clone());
        (
            spill_preview(text),
            Some(tmp),
            Some(SPILL_NAME.to_string()),
            Some("text/plain".to_string()),
        )
    } else {
        (
            text.to_string(),
            file.map(Path::to_path_buf),
            file.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            file.map(guess_content_type),
        )
    };

    let ch = OwnerChannel::open(dir, prefer).await?;

    // The attachment. Over the socket the serving process holds the blob
    // store's lock, so the CLI hashes natively and delegates the import
    // to the node (the owner-channel-only path parameter below); in
    // process the lock is free and the CLI imports directly, as before.
    let mut import_path: Option<std::path::PathBuf> = None;
    let attachment = match &attach_path {
        None => None,
        Some(path) if ch.is_socket() => {
            let (hash, bytes) = hash_file_blake3(path)?;
            // Staged inside the node directory: the serving process may
            // be another app that cannot read this one's files (an iOS
            // container), but every app can read the shared directory.
            import_path = Some(stage_attachment(dir, path, &hash)?);
            Some(BlobAttachment {
                hash: format!("blake3:{hash}"),
                bytes: Some(bytes as i64),
                name: attach_name.clone(),
                content_type: attach_type.clone(),
            })
        }
        Some(path) => {
            let (hash, bytes) = resonator_transport::blob_import(&dir.join("blobs"), path)
                .await
                .map_err(|e| anyhow::anyhow!("importing {}: {e}", path.display()))?;
            Some(BlobAttachment {
                hash: format!("blake3:{hash}"),
                bytes: Some(bytes as i64),
                name: attach_name.clone(),
                content_type: attach_type.clone(),
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
           (id, scope, sender, at, body, blob_hash, blob_bytes, blob_name, blob_type, outgoing, \
            re_id, tags) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11)",
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
            re.clone().map(Value::Text).unwrap_or(Value::Null),
            tags_to_json(&tags).map(Value::Text).unwrap_or(Value::Null),
        ],
    )
    .await
    .context("appending the message")?;

    // Socket + --file: one chat Execute to self whose single parameter
    // is the source path (owner-channel-only affordance, doc sec 5.3).
    // The serving process, which holds the redb lock, imports the file
    // into its blob store; the append itself is a no-op because the
    // message id is already recorded (message-level idempotency).
    if let Some(path) = &import_path {
        let msg = ChatMessage {
            id: Some(message_id.clone()),
            at: Some(at.clone()),
            body: wire_body.clone(),
            attachment: attachment.clone(),
            re: re.clone(),
            tags: tags.clone(),
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
    if let Some(staged) = &import_path {
        // The store copied it; the staged file has served its purpose.
        let _ = std::fs::remove_file(staged);
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
                body: wire_body.clone(),
                attachment: attachment.clone(),
                re: re.clone(),
                tags: tags.clone(),
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
                    body: wire_body.clone(),
                    attachment: attachment.clone(),
                    room_name: Some(name.clone()),
                    re: re.clone(),
                    tags: tags.clone(),
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
                    body: wire_body.clone(),
                    attachment: attachment.clone(),
                    re: re.clone(),
                    tags: tags.clone(),
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

    let spilled = spill_tmp.0.is_some();
    Ok(SendReport {
        message_id,
        scope,
        blob: attachment.map(|b| (b.hash, b.bytes.unwrap_or(0) as u64)),
        queued_to,
        spilled,
    })
}

/// A temporary file deleted when the guard drops (success or error).
struct RemoveOnDrop(Option<std::path::PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The preview body of a spilled message: a prefix on a char boundary
/// plus a marker (readers with the blob inline the full text back).
fn spill_preview(text: &str) -> String {
    let mut end = SPILL_PREVIEW_BYTES.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} … [{} bytes; full text in the attachment]",
        &text[..end],
        text.len()
    )
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
    pub blob_bytes: Option<i64>,
    pub blob_name: Option<String>,
    pub outgoing: bool,
    /// `_outbox.status` of own outgoing messages
    /// (queued/sent/done/denied/error/expired), when joinable.
    pub status: Option<String>,
    /// When the peer applied an own message (`_outbox.completed_at` of a
    /// `done` row); None for incoming or undelivered messages.
    pub delivered_at: Option<String>,
    /// Threading: the message this one answers (`rsntr:re`).
    pub re: Option<String>,
    /// Tags (`rsntr:tag`), sorted.
    pub tags: Vec<String>,
}

/// `rsntr chat log` filters (`--re`, `--tag`).
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    /// Only messages answering this id.
    pub re: Option<String>,
    /// Only messages carrying this tag.
    pub tag: Option<String>,
}

/// `rsntr chat log <target>`: a local read of `chat_messages` for that
/// scope, newest first, joined with delivery status. `since` (a message
/// ULID; they are totally ordered) restricts to strictly newer messages
/// — the agent's cursor.
pub fn chat_log(
    dir: &Path,
    target: &str,
    limit: i64,
    since: Option<&str>,
) -> Result<Vec<LogEntry>> {
    chat_log_filtered(dir, target, limit, since, &LogFilter::default())
}

/// [`chat_log`] narrowed by thread (`re`) and/or tag.
pub fn chat_log_filtered(
    dir: &Path,
    target: &str,
    limit: i64,
    since: Option<&str>,
    filter: &LogFilter,
) -> Result<Vec<LogEntry>> {
    let conn = store::open_db(dir)?;
    chat_ready(&conn)?;
    let scope = resolve_target(&conn, target)?.scope();
    let mut stmt = conn.prepare(
        "SELECT m.id, m.scope, m.sender, m.at, m.received_at, m.body, \
                m.blob_hash, m.blob_bytes, m.blob_name, m.outgoing, o.status, \
                CASE WHEN o.status = 'done' THEN o.completed_at END, \
                m.re_id, m.tags \
         FROM chat_messages m LEFT JOIN _outbox o ON o.request_id = m.id \
         WHERE m.scope = ?1 AND (?3 IS NULL OR m.id > ?3) \
           AND (?4 IS NULL OR m.re_id = ?4) \
           AND (?5 IS NULL OR EXISTS (SELECT 1 FROM json_each(m.tags) t WHERE t.value = ?5)) \
         ORDER BY m.received_at DESC, m.id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map((scope, limit, since, &filter.re, &filter.tag), |r| {
            Ok(LogEntry {
                id: r.get(0)?,
                scope: r.get(1)?,
                sender: r.get(2)?,
                at: r.get(3)?,
                received_at: r.get(4)?,
                body: r.get(5)?,
                blob_hash: r.get(6)?,
                blob_bytes: r.get(7)?,
                blob_name: r.get(8)?,
                outgoing: r.get::<_, i64>(9)? != 0,
                status: r.get(10)?,
                delivered_at: r.get(11)?,
                re: r.get(12)?,
                tags: tags_from_json(r.get::<_, Option<String>>(13)?.as_deref()),
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
    /// A new message in the watched scope (boxed: a LogEntry dwarfs the
    /// signalling variants).
    Message(Box<LogEntry>),
    /// The damp was confirmed; the watch is over.
    Damped,
    Denied(String),
    Failed(String),
}

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
    let (scope, mut cursor) = {
        let conn = store::open_db(&dir)?;
        chat_ready(&conn)?;
        let scope = resolve_target(&conn, &target)?.scope();
        let cursor: i64 = conn
            .query_row(
                "SELECT COALESCE(max(rowid), 0) FROM chat_messages",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        (scope, cursor)
    };
    // The owner channel: no self-dial, no ephemeral identity, no policy
    // rows; the serving process pushes vibrations on this connection.
    let (mut stream, mut reader, entrain_id) =
        crate::owner::entrain(&dir, CHAT_INBOX_POINT).await?;
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
                        let (new_cursor, entries) = read_since(&dir, cursor, Some(&scope))?;
                        cursor = new_cursor;
                        for entry in entries {
                            if tx.send(WatchEvent::Message(Box::new(entry))).await.is_err() {
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

/// Rows past `cursor` (by rowid), filtered to `scope` when given;
/// returns the advanced cursor over all rows seen, matching or not.
fn read_since(dir: &Path, cursor: i64, scope: Option<&str>) -> Result<(i64, Vec<LogEntry>)> {
    let conn = store::open_db(dir)?;
    ensure_chat_columns(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, id, scope, sender, at, received_at, body, \
                blob_hash, blob_bytes, blob_name, outgoing, re_id, tags \
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
                blob_bytes: r.get(8)?,
                blob_name: r.get(9)?,
                outgoing: r.get::<_, i64>(10)? != 0,
                status: None,
                delivered_at: None,
                re: r.get(11)?,
                tags: tags_from_json(r.get::<_, Option<String>>(12)?.as_deref()),
            },
        ))
    })?;
    for row in rows {
        let (rowid, entry) = row?;
        new_cursor = new_cursor.max(rowid);
        if scope.is_none_or(|s| entry.scope == s) {
            out.push(entry);
        }
    }
    Ok((new_cursor, out))
}

/// What `rsntr chat wait` returned.
#[derive(Debug)]
pub struct WaitReport {
    /// The window elapsed with nothing arriving.
    pub timed_out: bool,
    /// The incoming messages that ended the wait (oldest first).
    pub messages: Vec<LogEntry>,
    /// The highest message ULID seen — the `--since` cursor for the
    /// next read.
    pub next_since: Option<String>,
}

/// `rsntr chat wait [<target>] --timeout <secs>`: block until an
/// incoming message arrives (in `target`'s scope, or any scope when
/// omitted) or the window elapses. The agent's bounded-wait primitive:
/// one JSON object out, exit 0 on message, 3 on timeout.
///
/// Rides the same loopback entrainment as [`chat_watch`], so it needs a
/// serving daemon. Own sends never end the wait.
pub async fn chat_wait(
    dir: &Path,
    target: Option<&str>,
    timeout: std::time::Duration,
) -> Result<WaitReport> {
    let (scope, mut cursor) = {
        let conn = store::open_db(dir)?;
        chat_ready(&conn)?;
        let scope = match target {
            Some(t) => Some(resolve_target(&conn, t)?.scope()),
            None => None,
        };
        let cursor: i64 = conn
            .query_row(
                "SELECT COALESCE(max(rowid), 0) FROM chat_messages",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        (scope, cursor)
    };
    let (mut stream, mut reader, _entrain_id) =
        crate::owner::entrain(dir, CHAT_INBOX_POINT).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    // Reads rows past the cursor and keeps the incoming ones; the
    // catch-up after Entrained covers what landed during setup.
    let take_new = |cursor: &mut i64| -> Result<Vec<LogEntry>> {
        let (new_cursor, entries) = read_since(dir, *cursor, scope.as_deref())?;
        *cursor = new_cursor;
        Ok(entries.into_iter().filter(|e| !e.outgoing).collect())
    };
    loop {
        tokio::select! {
            frame = stream.recv() => {
                let Some(frame) = frame
                    .map_err(|e| anyhow::anyhow!("receiving from the wait stream: {e}"))?
                else {
                    bail!("the serving node closed the wait stream");
                };
                use resonator_protocol::EntrainmentEvent as Ev;
                match reader.accept(frame).context("entrainment choreography")? {
                    Ev::Entrained | Ev::Vibration(_) => {
                        let messages = take_new(&mut cursor)?;
                        if !messages.is_empty() {
                            let next_since = messages.iter().map(|e| e.id.clone()).max();
                            // Dropping the connection damps the
                            // entrainment; no in-band Damp needed.
                            return Ok(WaitReport {
                                timed_out: false,
                                messages,
                                next_since,
                            });
                        }
                    }
                    Ev::Damped => bail!("the serving node damped the wait"),
                    Ev::Denied(d) => bail!(
                        "denied: {}",
                        d.reason.unwrap_or_else(|| "(no reason given)".into())
                    ),
                    Ev::Error(e) => bail!("{}: {}", e.code, e.reason.unwrap_or_default()),
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(WaitReport {
                    timed_out: true,
                    messages: Vec::new(),
                    next_since: None,
                });
            }
        }
    }
}

/// True when this entry is an incoming auto-spilled body still waiting
/// to be inlined.
pub fn is_spilled(entry: &LogEntry) -> bool {
    !entry.outgoing && entry.blob_hash.is_some() && entry.blob_name.as_deref() == Some(SPILL_NAME)
}

/// Fetches and inlines spilled bodies from their senders, persisting the
/// full text into `chat_messages` (so each spill fetches once; the
/// cleared `blob_name` marks it inlined). Failures and bodies over
/// [`INLINE_MAX_BYTES`] keep their preview and are reported by message
/// id in the returned list.
pub async fn inline_spilled(dir: &Path, entries: &mut [LogEntry]) -> Vec<String> {
    let mut skipped = Vec::new();
    for entry in entries.iter_mut() {
        if !is_spilled(entry) {
            continue;
        }
        if entry.blob_bytes.is_some_and(|b| b > INLINE_MAX_BYTES) {
            skipped.push(entry.id.clone());
            continue;
        }
        let Some(hash) = entry.blob_hash.clone() else {
            continue;
        };
        // Stored dial hints first (cheap, LAN/offline friendly), then
        // the discovery/relay path (which still layers the stored hints
        // under it); one more round after a pause covers transient
        // holepunching state.
        let mut fetched = Err(anyhow::anyhow!("unfetched"));
        'rounds: for round in 0..2 {
            if round > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            for (offline, secs) in [(true, 10), (false, 15)] {
                fetched = crate::client::run_fetch(
                    dir,
                    &entry.sender,
                    &hash,
                    None,
                    offline,
                    std::time::Duration::from_secs(secs),
                )
                .await;
                if matches!(fetched, Ok(crate::client::FetchOutcome::Bytes(_))) {
                    break 'rounds;
                }
            }
        }
        match fetched {
            Ok(crate::client::FetchOutcome::Bytes(bytes)) => match String::from_utf8(bytes) {
                Ok(text) => {
                    if let Ok(conn) = store::open_db(dir) {
                        let _ = conn.execute(
                            "UPDATE chat_messages SET body = ?1, blob_name = NULL WHERE id = ?2",
                            (&text, &entry.id),
                        );
                    }
                    entry.body = text;
                    entry.blob_name = None;
                }
                Err(_) => {
                    tracing::warn!(id = %entry.id, "spilled body is not utf-8; keeping the preview");
                    skipped.push(entry.id.clone());
                }
            },
            Ok(_) | Err(_) => {
                tracing::warn!(id = %entry.id, "fetching the spilled body failed; keeping the preview");
                skipped.push(entry.id.clone());
            }
        }
    }
    skipped
}

/// One room as `rsntr chat room list` reports it.
#[derive(Debug, Clone)]
pub struct RoomInfo {
    pub room_id: String,
    pub name: String,
    /// The hosting peer's endpoint id.
    pub host: String,
    /// This node hosts the room (manages its roster, fans out).
    pub hosting: bool,
    /// Local roster size; a member side keeps no roster (0), only the
    /// host knows who is in the room.
    pub member_count: i64,
}

/// One roster row of `rsntr chat room members`.
#[derive(Debug, Clone)]
pub struct RoomMember {
    /// The member's endpoint id (64-hex).
    pub member: String,
    /// Its `_peers` petname, when admitted under one.
    pub name: Option<String>,
    pub added_at: String,
}

/// `rsntr chat room list`: every room this node knows, hosted or joined.
pub fn room_list(dir: &Path) -> Result<Vec<RoomInfo>> {
    let self_id = store::node_id(dir)?.to_string();
    let conn = store::open_db(dir)?;
    chat_ready(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT r.room_id, r.name, r.host, \
                (SELECT count(*) FROM chat_members m WHERE m.room_id = r.room_id) \
         FROM chat_rooms r ORDER BY r.created_at, r.room_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let host: String = r.get(2)?;
            Ok(RoomInfo {
                room_id: r.get(0)?,
                name: r.get(1)?,
                hosting: host == self_id,
                host,
                member_count: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `rsntr chat room members <room>`: the room plus its local roster
/// (host side: everyone admitted; member side: empty, the host keeps it).
pub fn room_members(dir: &Path, room: &str) -> Result<(RoomInfo, Vec<RoomMember>)> {
    let self_id = store::node_id(dir)?.to_string();
    let conn = store::open_db(dir)?;
    chat_ready(&conn)?;
    let Target::Room {
        room_id,
        name,
        host,
    } = resolve_target(&conn, room)?
    else {
        bail!("no room named {room:?}");
    };
    let mut stmt = conn.prepare(
        "SELECT m.member, p.name, m.added_at FROM chat_members m \
         LEFT JOIN _peers p ON p.endpoint_id = m.member \
         WHERE m.room_id = ?1 ORDER BY m.added_at, m.member",
    )?;
    let members = stmt
        .query_map([&room_id], |r| {
            Ok(RoomMember {
                member: r.get(0)?,
                name: r.get(1)?,
                added_at: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let info = RoomInfo {
        room_id,
        name,
        hosting: host == self_id,
        host,
        member_count: members.len() as i64,
    };
    Ok((info, members))
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
