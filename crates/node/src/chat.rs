//! The builtin `chat` modulation (docs/chat-protocol.md).
//!
//! A chat send is an `rsntr:Execute` with mod `chat` whose `rsntr:signal`
//! is a self-contained Turtle document containing exactly one node typed
//! `rsntr:Message`. The handler parses the message, gates it with action
//! `chat` against the resource (`chat:direct` for direct messages and
//! fan-out receipts, the room IRI for member-to-host room sends), appends
//! idempotently to the user-space `chat_messages` table, and answers a
//! Result/Done pair like every other write. When this node hosts the
//! room, the append is followed by one `_outbox` row per member (the
//! host's fan-out, chat protocol sec 5.3).

use oxttl::TurtleParser;
use rusqlite::{Connection, OptionalExtension};
use tracing::warn;

use oxrdf::{Literal, NamedOrBlankNode, Term, Triple, vocab::rdf, vocab::xsd};
use resonator_authenticator::{ActionKind, Chain, Decision, Footprint};
use resonator_protocol::Request;
use resonator_protocol::vocab::{IMPLIED_PREFIXES, cls, prop};

use crate::audit::{audit_full, audit_full_dir};
use crate::clock::now_rfc3339;
use crate::ddl::get_rsntr;
use crate::footprint::Lane;
use crate::pipeline::peer_known;

/// `rsntr:body` ceiling in UTF-8 bytes (chat protocol sec 2).
pub const BODY_MAX_BYTES: usize = 65536;

/// The policy resource name of direct messages. A colon keeps it out of
/// any plausible SQL table namespace.
pub const DIRECT_RESOURCE: &str = "chat:direct";

/// The well-known chat point IRIs (chat protocol sec 7.2).
pub const CHAT_INBOX_POINT: &str = "urn:rsntr:chat-inbox";
pub const CHAT_SEND_POINT: &str = "urn:rsntr:chat:send";
pub const CHAT_HISTORY_POINT: &str = "urn:rsntr:chat:history";

/// The user-space chat tables (chat protocol sec 7.1). Owned by
/// `rsntr chat init`; the handler ensures them too so a node advertising
/// the builtin `chat` mod never fails on a missing table.
pub const CHAT_TABLES_DDL: &str = "\
CREATE TABLE IF NOT EXISTS chat_messages (
  id          TEXT PRIMARY KEY,
  scope       TEXT NOT NULL,
  sender      TEXT NOT NULL,
  at          TEXT NOT NULL,
  received_at TEXT NOT NULL DEFAULT (datetime('now')),
  body        TEXT NOT NULL,
  blob_hash   TEXT,
  blob_bytes  INTEGER,
  blob_name   TEXT,
  blob_type   TEXT,
  outgoing    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS chat_messages_scope
  ON chat_messages (scope, received_at);

CREATE TABLE IF NOT EXISTS chat_rooms (
  room_id    TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  host       TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS chat_members (
  room_id  TEXT NOT NULL REFERENCES chat_rooms(room_id),
  member   TEXT NOT NULL,
  added_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (room_id, member)
);
";

/// A `rsntr:BlobRef` attachment node's metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlobAttachment {
    /// `"blake3:"` + 64-hex.
    pub hash: String,
    pub bytes: Option<i64>,
    pub name: Option<String>,
    pub content_type: Option<String>,
}

/// One parsed `rsntr:Message` (chat protocol sec 2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatMessage {
    /// Message ULID; absent defaults to the wire request id.
    pub id: Option<String>,
    /// Author endpoint id claim; trust rules in sec 5.4.
    pub from: Option<String>,
    /// Room IRI; absent means a direct message.
    pub room: Option<String>,
    /// Author-claimed time; absent defaults to the receiver clock.
    pub at: Option<String>,
    pub body: String,
    pub attachment: Option<BlobAttachment>,
    /// The `<room> rsntr:name` label riding along, if any.
    pub room_name: Option<String>,
}

/// Why a signal failed to parse as a chat message.
#[derive(Debug)]
pub enum ChatParseError {
    /// Malformed payload: `protocol-error`.
    Protocol(String),
    /// Body over the ceiling: `limit-exceeded`.
    Limit(String),
}

fn term_text(t: &Term) -> Option<String> {
    match t {
        Term::Literal(lit) => Some(lit.value().to_string()),
        _ => None,
    }
}

/// Parses a chat signal: one self-contained Turtle document (implied
/// prefix block registered) containing exactly one `rsntr:Message` node.
pub fn parse_message(signal: &str) -> Result<ChatMessage, ChatParseError> {
    let mut parser = TurtleParser::new();
    for (name, iri) in IMPLIED_PREFIXES {
        parser = parser
            .with_prefix(name, iri)
            .expect("implied prefix IRIs are static and valid");
    }
    let mut triples: Vec<Triple> = Vec::new();
    for item in parser.for_slice(signal.as_bytes()) {
        match item {
            Ok(t) => triples.push(t),
            Err(e) => {
                return Err(ChatParseError::Protocol(format!(
                    "chat signal is not valid Turtle: {e}"
                )));
            }
        }
    }

    let subjects: Vec<&NamedOrBlankNode> = triples
        .iter()
        .filter(|t| {
            t.predicate.as_ref() == rdf::TYPE
                && matches!(&t.object, Term::NamedNode(n) if n.as_ref() == cls::MESSAGE)
        })
        .map(|t| &t.subject)
        .collect();
    let subject = match subjects.as_slice() {
        [one] => (*one).clone(),
        [] => {
            return Err(ChatParseError::Protocol(
                "chat signal carries no rsntr:Message node".into(),
            ));
        }
        _ => {
            return Err(ChatParseError::Protocol(
                "chat signal carries more than one rsntr:Message node".into(),
            ));
        }
    };

    let pairs = |at: &NamedOrBlankNode| -> Vec<(String, Term)> {
        triples
            .iter()
            .filter(|t| &t.subject == at)
            .map(|t| (t.predicate.as_str().to_string(), t.object.clone()))
            .collect()
    };

    let mut msg = ChatMessage::default();
    let mut body: Option<String> = None;
    let mut attachment_node: Option<NamedOrBlankNode> = None;
    for (pred, obj) in pairs(&subject) {
        if pred == prop::ID.as_str() {
            msg.id = term_text(&obj);
        } else if pred == prop::FROM.as_str() {
            msg.from = term_text(&obj);
        } else if pred == prop::AT.as_str() {
            msg.at = term_text(&obj);
        } else if pred == prop::BODY.as_str() {
            body = term_text(&obj);
        } else if pred == prop::ROOM.as_str() {
            match obj {
                Term::NamedNode(n) => msg.room = Some(n.as_str().to_string()),
                other => {
                    return Err(ChatParseError::Protocol(format!(
                        "rsntr:room must be an IRI, got {other}"
                    )));
                }
            }
        } else if pred == prop::ATTACHMENT.as_str() {
            match obj {
                Term::NamedNode(n) => attachment_node = Some(NamedOrBlankNode::NamedNode(n)),
                Term::BlankNode(b) => attachment_node = Some(NamedOrBlankNode::BlankNode(b)),
                other => {
                    return Err(ChatParseError::Protocol(format!(
                        "rsntr:attachment must be a node, got {other}"
                    )));
                }
            }
        }
        // Unknown predicates are ignored (the open-world rule).
    }

    let Some(body) = body else {
        return Err(ChatParseError::Protocol(
            "rsntr:Message is missing its rsntr:body".into(),
        ));
    };
    if body.len() > BODY_MAX_BYTES {
        return Err(ChatParseError::Limit(format!(
            "rsntr:body is {} bytes; the ceiling is {BODY_MAX_BYTES}; \
             anything bigger belongs in an attachment",
            body.len()
        )));
    }
    msg.body = body;

    // Overlong dedup keys cannot become primary keys.
    if let Some(id) = &msg.id
        && (id.is_empty() || id.len() > 256)
    {
        return Err(ChatParseError::Protocol(
            "rsntr:id must be 1..=256 characters".into(),
        ));
    }

    if let Some(node) = attachment_node {
        let mut blob = BlobAttachment::default();
        for (pred, obj) in pairs(&node) {
            if pred == prop::HASH.as_str() {
                blob.hash = term_text(&obj).unwrap_or_default();
            } else if pred == prop::BYTES.as_str() {
                blob.bytes = term_text(&obj).and_then(|s| s.parse().ok());
            } else if pred == prop::NAME.as_str() {
                blob.name = term_text(&obj);
            } else if pred == prop::CONTENT_TYPE.as_str() {
                blob.content_type = term_text(&obj);
            }
        }
        if blob.hash.is_empty() {
            return Err(ChatParseError::Protocol(
                "rsntr:attachment BlobRef is missing its rsntr:hash".into(),
            ));
        }
        msg.attachment = Some(blob);
    }

    // The room's display label, when the frame carries one.
    if let Some(room) = &msg.room
        && let Ok(room_node) = oxrdf::NamedNode::new(room.clone())
    {
        let room_subject = NamedOrBlankNode::NamedNode(room_node);
        msg.room_name = pairs(&room_subject)
            .into_iter()
            .find(|(p, _)| p == prop::NAME.as_str())
            .and_then(|(_, o)| term_text(&o));
    }

    Ok(msg)
}

/// Serializes a message as a self-contained Turtle document fit for a
/// chat signal. Uses N-Triples-shaped lines (full IRIs, writer-escaped
/// literals), which every Turtle parser accepts; nothing is ever
/// hand-escaped.
pub fn message_to_turtle(msg: &ChatMessage) -> String {
    // NamedNodeRef Display already wraps the IRI in angle brackets.
    let mut out = String::new();
    let lit = |s: &str| Literal::new_simple_literal(s).to_string();
    out.push_str(&format!("_:m {} {} .\n", rdf::TYPE, cls::MESSAGE));
    if let Some(id) = &msg.id {
        out.push_str(&format!("_:m {} {} .\n", prop::ID, lit(id)));
    }
    if let Some(from) = &msg.from {
        out.push_str(&format!("_:m {} {} .\n", prop::FROM, lit(from)));
    }
    if let Some(room) = &msg.room {
        out.push_str(&format!("_:m {} <{room}> .\n", prop::ROOM));
    }
    if let Some(at) = &msg.at {
        let typed = Literal::new_typed_literal(at.clone(), xsd::DATE_TIME);
        out.push_str(&format!("_:m {} {typed} .\n", prop::AT));
    }
    out.push_str(&format!("_:m {} {} .\n", prop::BODY, lit(&msg.body)));
    if let Some(blob) = &msg.attachment {
        out.push_str(&format!("_:m {} _:b .\n", prop::ATTACHMENT));
        out.push_str(&format!("_:b {} {} .\n", rdf::TYPE, cls::BLOB_REF));
        out.push_str(&format!("_:b {} {} .\n", prop::HASH, lit(&blob.hash)));
        if let Some(bytes) = blob.bytes {
            let typed = Literal::new_typed_literal(bytes.to_string(), xsd::INTEGER);
            out.push_str(&format!("_:b {} {typed} .\n", prop::BYTES));
        }
        if let Some(name) = &blob.name {
            out.push_str(&format!("_:b {} {} .\n", prop::NAME, lit(name)));
        }
        if let Some(ct) = &blob.content_type {
            out.push_str(&format!("_:b {} {} .\n", prop::CONTENT_TYPE, lit(ct)));
        }
    }
    if let (Some(room), Some(name)) = (&msg.room, &msg.room_name) {
        out.push_str(&format!("<{room}> {} {} .\n", prop::NAME, lit(name)));
    }
    out
}

/// What the chat handler answers.
#[derive(Debug)]
pub enum ChatOutcome {
    UnknownPeer,
    /// The scaffold never ran: `mod-unsupported`, before any policy.
    ModUnsupported,
    Denied {
        reason: String,
    },
    /// Malformed signal: `protocol-error`.
    BadRequest {
        message: String,
    },
    /// Body/frame ceiling: `limit-exceeded`.
    LimitExceeded {
        message: String,
    },
    EngineError {
        message: String,
    },
    /// Appended (`affected` 1) or an already-known message id (`affected`
    /// 0); either way the send succeeded.
    Applied {
        affected: i64,
    },
}

/// Runs one chat request on the db thread: peer gate, mod gate, parse,
/// chain decide on action `chat`, `_applied` idempotency, idempotent
/// append, host fan-out, audit. On the owner lane
/// (docs/owner-channel.md sec 4) the peer gate and the chain are skipped
/// and the audit row is `allow`/`owner`/`local`; everything else is
/// identical.
pub fn handle_chat(
    conn: &mut Connection,
    peer: &str,
    req: &Request,
    chain: &Chain,
    lane: Lane,
) -> ChatOutcome {
    let id = req.id_string();

    if lane == Lane::Remote && !peer_known(conn, peer) {
        crate::audit::audit_direct(
            conn,
            peer,
            &id,
            &req.signal,
            "deny",
            "peer-gate",
            "unknown peer",
        );
        return ChatOutcome::UnknownPeer;
    }
    let served = crate::ddl::served_modulations(conn);
    if !served
        .iter()
        .any(|m| resonator_protocol::mod_matches("chat", m))
    {
        return ChatOutcome::ModUnsupported;
    }
    if let Err(e) = conn.execute_batch(CHAT_TABLES_DDL) {
        return ChatOutcome::EngineError {
            message: format!("chat tables unavailable: {e}"),
        };
    }

    let direction = if lane == Lane::Owner { "local" } else { "in" };
    let msg = match parse_message(&req.signal) {
        Ok(m) => m,
        Err(ChatParseError::Protocol(message)) => {
            crate::audit::audit_direct_dir(
                conn,
                direction,
                peer,
                &id,
                &req.signal,
                "deny",
                "node",
                &message,
            );
            return ChatOutcome::BadRequest { message };
        }
        Err(ChatParseError::Limit(message)) => {
            crate::audit::audit_direct_dir(
                conn,
                direction,
                peer,
                &id,
                &req.signal,
                "deny",
                "node",
                &message,
            );
            return ChatOutcome::LimitExceeded { message };
        }
    };

    let self_id = get_rsntr(conn, "endpoint_id").unwrap_or_default();

    // Role resolution (chat protocol sec 5.4): who is trusted about what.
    struct Plan {
        resource: String,
        sender: String,
        scope: String,
        /// TOFU: create this chat_rooms binding on allow;
        /// `(room_id, name, host)`, host = the proven sender.
        bind_room: Option<(String, String, String)>,
        /// Fan out to the room's members after the append.
        fan_out_room: Option<String>,
    }
    let plan = match &msg.room {
        None => Plan {
            resource: DIRECT_RESOURCE.to_string(),
            sender: peer.to_string(),
            scope: peer.to_string(),
            bind_room: None,
            fan_out_room: None,
        },
        Some(room) => {
            let hosted: Option<String> = conn
                .query_row(
                    "SELECT host FROM chat_rooms WHERE room_id = ?1",
                    [room],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            match hosted {
                // This node hosts the room: a member-to-host send, gated
                // on the room IRI; membership policy is what admits it.
                Some(host) if host == self_id => Plan {
                    resource: room.clone(),
                    sender: peer.to_string(),
                    scope: room.clone(),
                    bind_room: None,
                    fan_out_room: Some(room.clone()),
                },
                // Known room hosted elsewhere: only the recorded host may
                // deliver into it.
                Some(host) => {
                    if host != peer {
                        let reason = format!(
                            "room <{room}> is bound to host {host}; \
                             fan-out from {peer} refused"
                        );
                        crate::audit::audit_direct_dir(
                            conn,
                            direction,
                            peer,
                            &id,
                            &req.signal,
                            "deny",
                            "node",
                            &reason,
                        );
                        return ChatOutcome::Denied { reason };
                    }
                    Plan {
                        resource: DIRECT_RESOURCE.to_string(),
                        sender: msg.from.clone().unwrap_or_else(|| peer.to_string()),
                        scope: room.clone(),
                        bind_room: None,
                        fan_out_room: None,
                    }
                }
                // Unknown room: trust-on-first-use, the proven sender
                // becomes the recorded host.
                None => Plan {
                    resource: DIRECT_RESOURCE.to_string(),
                    sender: msg.from.clone().unwrap_or_else(|| peer.to_string()),
                    scope: room.clone(),
                    bind_room: Some((
                        room.clone(),
                        msg.room_name.clone().unwrap_or_else(|| room.clone()),
                        peer.to_string(),
                    )),
                    fan_out_room: None,
                },
            }
        }
    };

    // Chain decide with the resource as the footprint's one table, so the
    // policy tier resolves (peer, resource, 'chat') most-specific-first.
    // The owner lane is constitutionally allowed: no chain, ledger only.
    let footprint = Footprint::from_tables(
        ActionKind::Write,
        [(plan.resource.as_str(), Vec::<String>::new())],
    );
    if lane == Lane::Owner {
        audit_full_dir(
            conn,
            "local",
            peer,
            &id,
            &req.signal,
            &footprint.to_json(),
            "allow",
            "owner",
            None,
            0,
        );
    } else {
        let decided = chain.decide(conn, peer, "chat", &footprint, &req.signal);
        let (decision_str, deny_reason) = match &decided.decision {
            Decision::Allow | Decision::AllowNarrowed { .. } => ("allow", None),
            Decision::Deny { reason } => ("deny", Some(reason.clone())),
            Decision::Escalate => (
                "deny",
                Some("authenticator chain did not decide".to_string()),
            ),
        };
        audit_full(
            conn,
            peer,
            &id,
            &req.signal,
            &footprint.to_json(),
            decision_str,
            &decided.decided_by,
            deny_reason.as_deref(),
            0,
        );
        if let Some(reason) = deny_reason {
            return ChatOutcome::Denied { reason };
        }
    }

    match apply_message(
        conn,
        &id,
        &msg,
        &plan.sender,
        &plan.scope,
        &plan.bind_room,
        plan.fan_out_room.as_deref(),
        &self_id,
    ) {
        Ok(affected) => ChatOutcome::Applied { affected },
        Err(e) => ChatOutcome::EngineError {
            message: e.to_string(),
        },
    }
}

/// The transactional tail of [`handle_chat`]: `_applied` replay, append,
/// TOFU room binding, fan-out enqueue, `_applied` record.
#[allow(clippy::too_many_arguments)]
fn apply_message(
    conn: &mut Connection,
    request_id: &str,
    msg: &ChatMessage,
    sender: &str,
    scope: &str,
    bind_room: &Option<(String, String, String)>,
    fan_out_room: Option<&str>,
    self_id: &str,
) -> Result<i64, rusqlite::Error> {
    let tx = conn.transaction()?;

    // Wire-level idempotency: a retransmitted request is answered from
    // its recorded outcome.
    let recorded: Option<String> = tx
        .query_row(
            "SELECT outcome FROM _applied WHERE request_id = ?1",
            [request_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(json) = recorded {
        let affected = serde_json::from_str::<serde_json::Value>(&json)
            .ok()
            .and_then(|v| v.get("affected_rows").and_then(|a| a.as_i64()))
            .unwrap_or(0);
        tx.commit()?;
        return Ok(affected);
    }

    let message_id = msg.id.clone().unwrap_or_else(|| request_id.to_string());
    let at = msg.at.clone().unwrap_or_else(now_rfc3339);
    let blob = msg.attachment.as_ref();

    if let Some((room_id, name, host)) = bind_room {
        tx.execute(
            "INSERT OR IGNORE INTO chat_rooms (room_id, name, host) VALUES (?1, ?2, ?3)",
            (room_id, name, host),
        )?;
    }

    // Message-level idempotency: the same message under a second request
    // id (a second delivery path) lands exactly once.
    let affected = tx.execute(
        "INSERT OR IGNORE INTO chat_messages \
           (id, scope, sender, at, body, blob_hash, blob_bytes, blob_name, blob_type, outgoing) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
        rusqlite::params![
            message_id,
            scope,
            sender,
            at,
            msg.body,
            blob.map(|b| b.hash.clone()),
            blob.and_then(|b| b.bytes),
            blob.and_then(|b| b.name.clone()),
            blob.and_then(|b| b.content_type.clone()),
        ],
    )? as i64;

    // Host fan-out (chat protocol sec 5.3): one _outbox row per member
    // other than the author and the host itself, fresh request ids,
    // original message id and authorship preserved.
    if let Some(room) = fan_out_room
        && affected == 1
    {
        let room_name: Option<String> = tx
            .query_row(
                "SELECT name FROM chat_rooms WHERE room_id = ?1",
                [room],
                |r| r.get(0),
            )
            .optional()?;
        let out = ChatMessage {
            id: Some(message_id.clone()),
            from: Some(sender.to_string()),
            room: Some(room.to_string()),
            at: Some(at.clone()),
            body: msg.body.clone(),
            attachment: msg.attachment.clone(),
            room_name,
        };
        let signal = message_to_turtle(&out);
        let members: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT member FROM chat_members WHERE room_id = ?1 \
                   AND member != ?2 AND member != ?3",
            )?;
            let rows = stmt.query_map((room, sender, self_id), |r| r.get::<_, String>(0))?;
            rows.filter_map(Result::ok).collect()
        };
        for member in members {
            let res = tx.execute(
                "INSERT INTO _outbox (request_id, peer, mod, signal) VALUES (?1, ?2, 'chat', ?3)",
                (ulid::Ulid::new().to_string(), &member, &signal),
            );
            if let Err(e) = res {
                // The message is stored; a missing _outbox only loses
                // fan-out, and the owner's scaffold is what fixes it.
                warn!(error = %e, member, "chat fan-out enqueue failed");
            }
        }
    }

    tx.execute(
        "INSERT INTO _applied (request_id, at, outcome) VALUES (?1, ?2, ?3)",
        (
            request_id,
            now_rfc3339(),
            serde_json::json!({ "affected_rows": affected }).to_string(),
        ),
    )?;
    tx.commit()?;
    Ok(affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_message() {
        let msg = parse_message(
            "[] a rsntr:Message ; rsntr:id \"01K1CH4T0001XMPL0000000001\" ; \
             rsntr:body \"lunch at noon?\" .",
        )
        .expect("parse");
        assert_eq!(msg.id.as_deref(), Some("01K1CH4T0001XMPL0000000001"));
        assert_eq!(msg.body, "lunch at noon?");
        assert!(msg.room.is_none());
        assert!(msg.attachment.is_none());
    }

    #[test]
    fn parse_room_message_with_attachment_round_trips() {
        let src = ChatMessage {
            id: Some("01K1CH4T0002XMPL0000000002".into()),
            from: Some("e0".repeat(32)),
            room: Some("urn:rsntr:room:01K1R00M0001XMPL000000000A".into()),
            at: Some("2026-07-29T12:03:00".into()),
            body: "the \"seedlings\" came up\nnew line \\ backslash".into(),
            attachment: Some(BlobAttachment {
                hash: format!("blake3:{}", "af".repeat(32)),
                bytes: Some(4194304),
                name: Some("seedlings.jpg".into()),
                content_type: Some("image/jpeg".into()),
            }),
            room_name: Some("gardening".into()),
        };
        let turtle = message_to_turtle(&src);
        let back = parse_message(&turtle).expect("reparse");
        assert_eq!(back, src);
    }

    #[test]
    fn parse_rejects_malformed() {
        // No Message node at all.
        assert!(matches!(
            parse_message("[] a rsntr:Query ."),
            Err(ChatParseError::Protocol(_))
        ));
        // Two Message nodes.
        assert!(matches!(
            parse_message(
                "[] a rsntr:Message ; rsntr:body \"a\" . [] a rsntr:Message ; rsntr:body \"b\" ."
            ),
            Err(ChatParseError::Protocol(_))
        ));
        // Missing body.
        assert!(matches!(
            parse_message("[] a rsntr:Message ; rsntr:id \"x\" ."),
            Err(ChatParseError::Protocol(_))
        ));
        // Not Turtle.
        assert!(matches!(
            parse_message("this is not turtle"),
            Err(ChatParseError::Protocol(_))
        ));
        // Room as a literal.
        assert!(matches!(
            parse_message("[] a rsntr:Message ; rsntr:body \"x\" ; rsntr:room \"not-an-iri\" ."),
            Err(ChatParseError::Protocol(_))
        ));
    }

    #[test]
    fn parse_enforces_body_ceiling() {
        let big = "x".repeat(BODY_MAX_BYTES + 1);
        let signal = message_to_turtle(&ChatMessage {
            body: big,
            ..ChatMessage::default()
        });
        assert!(matches!(
            parse_message(&signal),
            Err(ChatParseError::Limit(_))
        ));
    }
}
