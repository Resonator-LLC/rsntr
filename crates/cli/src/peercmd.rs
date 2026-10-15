//! `rsntr peer remove`: forgets a peer, and everything that would keep
//! reaching it or quietly re-admit it.
//!
//! Deleting the `_peers` row alone revokes admission (the peer gate reads
//! `_peers` on every request) but leaks three ways:
//!
//! - undelivered `_outbox` rows addressed by endpoint id keep dialling
//!   the peer: the worker resolves a bare hex id without a `_peers` row
//!   and retries for up to thirty days;
//! - room fan-out reads `chat_members` without looking at `_peers`, so
//!   every room message is still queued to it;
//! - its allow and price rules in `_policy` and its cached allows in
//!   `_decisions` stay put, so re-admitting it later silently restores
//!   every permission it once had.
//!
//! So removal deletes the row, those rules, the cached decisions and the
//! room memberships, and moves undelivered messages to the terminal
//! `error` status. Deny rules are kept: removing a peer must never
//! unblock one. History is kept too: chat messages, the audit log,
//! results, receipts, and decided inbox rows. A pending inbox knock is
//! left alone, since answering it `allow` is an explicit re-admission.
//!
//! Writes ride the owner channel (docs/owner-channel.md sec 5), so a
//! serving node sees them on its live connection. Not covered: streams
//! the peer already holds open (an entrainment, a media feed) are gated
//! once when they open and run until the peer disconnects, and presence
//! gossip keeps it on the topic until the daemon restarts. Any new
//! request from it is refused at once.

use std::path::Path;

use anyhow::{Context, Result, bail};
use resonator_protocol::Value;
use resonator_transport::PeerId;

use crate::channel::{self, OwnerChannel, Prefer};
use crate::store;

/// What one removal cleared, per table, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRemoval {
    pub endpoint_id: String,
    /// The petname the peer was stored under, when it had a row with one.
    pub name: Option<String>,
    /// `_peers` rows deleted (0 when an earlier removal already did it).
    pub peer: i64,
    /// Allow and price rules deleted; deny rules are never counted here.
    pub policy: i64,
    pub decisions: i64,
    pub room_memberships: i64,
    /// Undelivered `_outbox` rows moved to `error`.
    pub outbox_cancelled: i64,
}

impl PeerRemoval {
    fn cleared_anything(&self) -> bool {
        self.peer + self.policy + self.decisions + self.room_memberships + self.outbox_cancelled > 0
    }
}

/// [`peer_remove_with`] over the default transport preference.
pub fn peer_remove(dir: &Path, target: &str) -> Result<PeerRemoval> {
    peer_remove_with(dir, Prefer::Auto, target)
}

/// Removes the peer named by `target`, a petname or a 64-hex endpoint id.
///
/// A petname must match exactly one row: `_peers.name` is not unique, and
/// picking one of several at random would remove the wrong peer without
/// a word. An endpoint id is taken as given even with no row left, so a
/// removal interrupted after its first step can be finished by re-running
/// it with the id. Fails when there was nothing at all to clear.
pub fn peer_remove_with(dir: &Path, prefer: Prefer, target: &str) -> Result<PeerRemoval> {
    let Resolved {
        endpoint_id,
        name,
        has_rooms,
    } = resolve(dir, target)?;
    let id = endpoint_id.clone();
    let pet = name.clone();
    let counts = channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        let mut n = [0i64; 5];
        // The row goes first: if anything after it fails, the peer is
        // already refused, and a re-run by id finishes the rest.
        let steps: [(&str, Vec<Value>); 5] = [
            (
                "DELETE FROM _peers WHERE endpoint_id = ?1",
                vec![Value::Text(id.clone())],
            ),
            (
                "DELETE FROM _policy WHERE peer_or_group = ?1 AND effect != 'deny'",
                vec![Value::Text(id.clone())],
            ),
            (
                "DELETE FROM _decisions WHERE peer = ?1",
                vec![Value::Text(id.clone())],
            ),
            (
                "DELETE FROM chat_members WHERE member = ?1",
                vec![Value::Text(id.clone())],
            ),
            (
                // Rows may name the peer by id or by petname.
                "UPDATE _outbox SET status = 'error', error = 'peer removed', \
                 completed_at = datetime('now') \
                 WHERE status IN ('queued', 'sent') \
                 AND (peer = ?1 OR (?2 IS NOT NULL AND peer = ?2))",
                vec![
                    Value::Text(id.clone()),
                    pet.map(Value::Text).unwrap_or(Value::Null),
                ],
            ),
        ];
        for (i, (slot, (sql, params))) in n.iter_mut().zip(steps).enumerate() {
            // The chat tables exist only once chat has been used; a node
            // that never had a room has no memberships to clear.
            if i == 3 && !has_rooms {
                continue;
            }
            let done = channel::execute(&ch, sql, params).await?;
            *slot = done.affected_rows.unwrap_or(0);
        }
        anyhow::Ok(n)
    })??;
    let removal = PeerRemoval {
        endpoint_id,
        name,
        peer: counts[0],
        policy: counts[1],
        decisions: counts[2],
        room_memberships: counts[3],
        outbox_cancelled: counts[4],
    };
    if !removal.cleared_anything() {
        bail!("unknown peer {target:?}: nothing to remove");
    }
    Ok(removal)
}

struct Resolved {
    endpoint_id: String,
    name: Option<String>,
    /// Whether this node has the chat tables at all.
    has_rooms: bool,
}

/// The endpoint id and stored petname for `target`.
fn resolve(dir: &Path, target: &str) -> Result<Resolved> {
    let conn = store::open_db(dir)?;
    let has_rooms = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'chat_members'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .context("reading the schema")?
        > 0;
    if let Ok(id) = target.parse::<PeerId>() {
        let id = id.to_string();
        let name = conn
            .query_row(
                "SELECT name FROM _peers WHERE endpoint_id = ?1",
                [&id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        return Ok(Resolved {
            endpoint_id: id,
            name,
            has_rooms,
        });
    }
    let mut stmt = conn.prepare("SELECT endpoint_id FROM _peers WHERE name = ?1")?;
    let ids = stmt
        .query_map([target], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()
        .context("reading _peers")?;
    match ids.as_slice() {
        [] => bail!("unknown peer {target:?}: not an endpoint id and not a name in _peers"),
        [id] => Ok(Resolved {
            endpoint_id: id.clone(),
            name: Some(target.to_string()),
            has_rooms,
        }),
        many => bail!(
            "{} peers are named {target:?} ({}); remove one by its endpoint id",
            many.len(),
            many.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const ALICE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const BOB: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn seed(dir: &Path) {
        let conn = store::open_db(dir).unwrap();
        conn.execute_batch(resonator_node::chat::CHAT_TABLES_DDL)
            .unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO _peers (endpoint_id, name, added_at) VALUES
               ('{ALICE}', 'alice', datetime('now')),
               ('{BOB}', 'bob', datetime('now'));
             INSERT INTO _policy (peer_or_group, table_name, action, effect) VALUES
               ('{ALICE}', 'notes', 'read', 'allow'),
               ('{ALICE}', 'notes', 'write', 'price'),
               ('{ALICE}', '*', 'knock', 'deny'),
               ('{BOB}', 'notes', 'read', 'allow');
             INSERT INTO _decisions (peer, fingerprint, decision, decided_by, created_at) VALUES
               ('{ALICE}', 'fp', 'allow', 'human', datetime('now')),
               ('{BOB}', 'fp', 'allow', 'human', datetime('now'));
             INSERT INTO chat_rooms (room_id, name, host) VALUES ('r1', 'room', '{BOB}');
             INSERT INTO chat_members (room_id, member) VALUES ('r1', '{ALICE}'), ('r1', '{BOB}');
             INSERT INTO _outbox (request_id, peer, signal, status) VALUES
               ('q-hex', '{ALICE}', 'x', 'queued'),
               ('s-name', 'alice', 'x', 'sent'),
               ('d-hex', '{ALICE}', 'x', 'done'),
               ('q-bob', '{BOB}', 'x', 'queued');
             INSERT INTO chat_messages (id, scope, sender, at, body) VALUES
               ('m1', '{ALICE}', '{ALICE}', datetime('now'), 'hello');"
        ))
        .unwrap();
    }

    fn count(dir: &Path, sql: &str) -> i64 {
        store::open_db(dir)
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn removal_clears_what_would_reach_or_readmit_the_peer_and_keeps_history() {
        let t = TempDir::new("peer-remove");
        store::init_dir(t.path()).unwrap();
        seed(t.path());

        let r = peer_remove_with(t.path(), Prefer::Local, "alice").unwrap();
        assert_eq!(r.endpoint_id, ALICE);
        assert_eq!(r.name.as_deref(), Some("alice"));
        assert_eq!(
            (
                r.peer,
                r.policy,
                r.decisions,
                r.room_memberships,
                r.outbox_cancelled
            ),
            (1, 2, 1, 1, 2)
        );

        let p = t.path();
        let alice = |sql: &str| count(p, &sql.replace("?", &format!("'{ALICE}'")));
        assert_eq!(
            alice("SELECT count(*) FROM _peers WHERE endpoint_id = ?"),
            0
        );
        assert_eq!(
            alice("SELECT count(*) FROM _policy WHERE peer_or_group = ? AND effect = 'deny'"),
            1,
            "a deny rule survives: removing a peer must not unblock it"
        );
        assert_eq!(
            alice("SELECT count(*) FROM _policy WHERE peer_or_group = ?"),
            1
        );
        assert_eq!(alice("SELECT count(*) FROM _decisions WHERE peer = ?"), 0);
        assert_eq!(
            alice("SELECT count(*) FROM chat_members WHERE member = ?"),
            0
        );
        assert_eq!(
            count(
                p,
                "SELECT count(*) FROM _outbox WHERE request_id IN ('q-hex','s-name') \
                 AND status = 'error' AND error = 'peer removed'"
            ),
            2,
            "undelivered messages stop, whether addressed by id or petname"
        );
        assert_eq!(
            count(
                p,
                "SELECT count(*) FROM _outbox WHERE request_id = 'd-hex' AND status = 'done'"
            ),
            1,
            "delivered history is untouched"
        );
        assert_eq!(
            count(p, "SELECT count(*) FROM chat_messages WHERE id = 'm1'"),
            1
        );

        // Nothing of Bob's moved.
        let bob = |sql: &str| count(p, &sql.replace("?", &format!("'{BOB}'")));
        assert_eq!(bob("SELECT count(*) FROM _peers WHERE endpoint_id = ?"), 1);
        assert_eq!(
            bob("SELECT count(*) FROM _policy WHERE peer_or_group = ?"),
            1
        );
        assert_eq!(bob("SELECT count(*) FROM _decisions WHERE peer = ?"), 1);
        assert_eq!(bob("SELECT count(*) FROM chat_members WHERE member = ?"), 1);
        assert_eq!(
            count(
                p,
                "SELECT count(*) FROM _outbox WHERE request_id = 'q-bob' AND status = 'queued'"
            ),
            1
        );
    }

    /// Chat tables appear only once chat is used, so a node that never
    /// had a room must still be able to remove a peer. The first version
    /// of this command failed outright there.
    #[test]
    fn a_node_that_never_used_chat_can_still_remove_a_peer() {
        let t = TempDir::new("peer-remove-nochat");
        store::init_dir(t.path()).unwrap();
        store::open_db(t.path())
            .unwrap()
            .execute_batch(&format!(
                "INSERT INTO _peers (endpoint_id, name, added_at)
                   VALUES ('{ALICE}', 'alice', datetime('now'));
                 INSERT INTO _policy (peer_or_group, table_name, action, effect)
                   VALUES ('{ALICE}', 'notes', 'read', 'allow');"
            ))
            .unwrap();
        assert_eq!(
            count(
                t.path(),
                "SELECT count(*) FROM sqlite_master WHERE name = 'chat_members'"
            ),
            0,
            "precondition: no chat tables on this node"
        );

        let r = peer_remove_with(t.path(), Prefer::Local, "alice").unwrap();
        assert_eq!((r.peer, r.policy, r.room_memberships), (1, 1, 0));
    }

    #[test]
    fn an_ambiguous_petname_removes_nothing_and_names_the_ids() {
        let t = TempDir::new("peer-remove-dup");
        store::init_dir(t.path()).unwrap();
        seed(t.path());
        store::open_db(t.path())
            .unwrap()
            .execute(
                "UPDATE _peers SET name = 'alice' WHERE endpoint_id = ?1",
                [BOB],
            )
            .unwrap();

        let err = peer_remove_with(t.path(), Prefer::Local, "alice")
            .unwrap_err()
            .to_string();
        assert!(err.contains(ALICE) && err.contains(BOB), "{err}");
        assert_eq!(count(t.path(), "SELECT count(*) FROM _peers"), 2);
    }

    #[test]
    fn unknown_targets_fail_and_a_second_removal_by_id_has_nothing_left() {
        let t = TempDir::new("peer-remove-missing");
        store::init_dir(t.path()).unwrap();
        seed(t.path());

        assert!(peer_remove_with(t.path(), Prefer::Local, "carol").is_err());
        peer_remove_with(t.path(), Prefer::Local, ALICE).unwrap();
        let again = peer_remove_with(t.path(), Prefer::Local, ALICE).unwrap_err();
        assert!(again.to_string().contains("nothing to remove"), "{again}");
    }

    #[test]
    fn an_interrupted_removal_is_finished_by_rerunning_with_the_id() {
        let t = TempDir::new("peer-remove-rerun");
        store::init_dir(t.path()).unwrap();
        seed(t.path());
        // What a run that died after its first step leaves behind.
        store::open_db(t.path())
            .unwrap()
            .execute("DELETE FROM _peers WHERE endpoint_id = ?1", [ALICE])
            .unwrap();

        let r = peer_remove_with(t.path(), Prefer::Local, ALICE).unwrap();
        assert_eq!(r.peer, 0);
        assert_eq!(r.name, None);
        assert_eq!((r.policy, r.decisions, r.room_memberships), (2, 1, 1));
        // Without the petname, only id-addressed messages can be matched.
        assert_eq!(r.outbox_cancelled, 1);
    }
}
