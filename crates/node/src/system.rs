//! The builtin `system` modulation: the node's own state as RDF.
//!
//! Signal `graph` answers `rsntr:Graph` frames of triples generated on
//! read from the system tables — `_peers` → `rsntr:Peer`, `_policy` →
//! `rsntr:PolicyRule`, `_modulations` → `rsntr:Modulation`, `chat_rooms`
//! → `rsntr:Room` — plus the node itself (`rsntr:Node`, what it serves)
//! and the caller's projection points (via [`build_projection`], so the
//! visibility rules are shared). Storage and enforcement stay
//! table-shaped by design (footprint policy, table-granular vibrations);
//! this is the complete meaning layer over them, not a second store.
//!
//! Filtering per caller: each table's triples require an allow row for
//! `read` on that table (or `*`), the same allow-row-exists check the
//! projection uses; the owner lane is unfiltered. Nothing is written.

use oxrdf::{Literal, NamedNode, Term, Triple};
use rusqlite::{Connection, OptionalExtension};

use resonator_protocol::vocab::{IMPLIED_PREFIXES, RDF_NS, RDFS_NS, RSNTR_NS, XSD_NS};

use crate::ddl::{get_rsntr, served_modulations};
use crate::projection::{ProjectionOutcome, build_projection};

/// The modulation name.
pub const SYSTEM_MOD: &str = "system";
/// The one signal of v1.
pub const GRAPH_SIGNAL: &str = "graph";

fn rsntr(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{RSNTR_NS}{local}"))
}

fn rdf_type() -> NamedNode {
    NamedNode::new_unchecked(format!("{RDF_NS}type"))
}

fn text(s: &str) -> Term {
    Term::Literal(Literal::new_simple_literal(s))
}

fn typed(s: &str, xsd_local: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(
        s,
        NamedNode::new_unchecked(format!("{XSD_NS}{xsd_local}")),
    ))
}

fn boolean(b: bool) -> Term {
    typed(if b { "true" } else { "false" }, "boolean")
}

/// The IRI of a peer in the system graph.
pub fn peer_iri(hex: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:rsntr:peer:{hex}"))
}

/// The IRI of this node in its own system graph.
pub fn node_iri(hex: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:rsntr:node:{hex}"))
}

/// Whether `peer` holds a `read` allow row on `table` (or the wildcards):
/// the projection's allow-row-exists approximation, applied per table.
pub fn readable(conn: &Connection, peer: &str, table: &str) -> Result<bool, rusqlite::Error> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM _policy \
         WHERE effect = 'allow' AND action = 'read' \
           AND (table_name = ?1 OR table_name = '*') \
           AND (peer_or_group = ?2 OR peer_or_group = '*')",
        (table, peer),
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Which sections a fetch emitted (for the audit reason and tests).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphSections {
    pub peers: bool,
    pub policy: bool,
    pub modulations: bool,
    pub rooms: bool,
    pub points: usize,
    pub apps: bool,
}

/// Builds the caller's view of the system graph. `unfiltered` is the
/// owner lane.
pub fn build_system_graph(
    conn: &Connection,
    peer: &str,
    unfiltered: bool,
) -> Result<(Vec<Triple>, GraphSections), rusqlite::Error> {
    let mut out: Vec<Triple> = Vec::new();
    let mut sections = GraphSections::default();
    let self_hex = get_rsntr(conn, "endpoint_id").unwrap_or_default();
    let node = node_iri(&self_hex);
    let a = rdf_type();

    // The node itself: public, like help.
    out.push(Triple::new(node.clone(), a.clone(), rsntr("Node")));
    out.push(Triple::new(
        node.clone(),
        rsntr("endpointId"),
        text(&self_hex),
    ));
    for m in served_modulations(conn) {
        out.push(Triple::new(node.clone(), rsntr("serves"), text(&m)));
    }

    // _peers → rsntr:Peer
    if unfiltered || readable(conn, peer, "_peers")? {
        sections.peers = true;
        let mut stmt = conn.prepare(
            "SELECT endpoint_id, name, relay_url, addrs, added_at, last_seen, notes \
             FROM _peers ORDER BY endpoint_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (hex, name, relay, addrs, added_at, last_seen, notes) = row?;
            let s = peer_iri(&hex);
            out.push(Triple::new(s.clone(), a.clone(), rsntr("Peer")));
            out.push(Triple::new(s.clone(), rsntr("endpointId"), text(&hex)));
            out.push(Triple::new(node.clone(), rsntr("knows"), s.clone()));
            if let Some(name) = name {
                out.push(Triple::new(s.clone(), rsntr("name"), text(&name)));
            }
            if let Some(relay) = relay {
                out.push(Triple::new(s.clone(), rsntr("relay"), text(&relay)));
            }
            for addr in addrs
                .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
                .unwrap_or_default()
            {
                out.push(Triple::new(s.clone(), rsntr("addr"), text(&addr)));
            }
            out.push(Triple::new(
                s.clone(),
                rsntr("addedAt"),
                typed(&added_at, "dateTime"),
            ));
            if let Some(seen) = last_seen {
                out.push(Triple::new(
                    s.clone(),
                    rsntr("lastSeen"),
                    typed(&seen, "dateTime"),
                ));
            }
            if let Some(notes) = notes {
                out.push(Triple::new(s, rsntr("note"), text(&notes)));
            }
        }
    }

    // _policy → rsntr:PolicyRule
    if unfiltered || readable(conn, peer, "_policy")? {
        sections.policy = true;
        let mut stmt = conn.prepare(
            "SELECT rowid, peer_or_group, table_name, action, effect, note \
             FROM _policy ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (rowid, who, table, action, effect, note) = row?;
            let s = NamedNode::new_unchecked(format!("urn:rsntr:policy:{rowid}"));
            out.push(Triple::new(s.clone(), a.clone(), rsntr("PolicyRule")));
            out.push(Triple::new(node.clone(), rsntr("rule"), s.clone()));
            out.push(Triple::new(s.clone(), rsntr("peerOrGroup"), text(&who)));
            out.push(Triple::new(s.clone(), rsntr("resource"), text(&table)));
            out.push(Triple::new(s.clone(), rsntr("action"), text(&action)));
            out.push(Triple::new(s.clone(), rsntr("effect"), text(&effect)));
            if let Some(note) = note {
                out.push(Triple::new(s, rsntr("note"), text(&note)));
            }
        }
    }

    // _modulations → rsntr:Modulation
    if unfiltered || readable(conn, peer, "_modulations")? {
        sections.modulations = true;
        let mut stmt = conn
            .prepare("SELECT name, enabled, caps, sha256, note FROM _modulations ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (name, enabled, caps, sha, note) = row?;
            let s = NamedNode::new_unchecked(format!("urn:rsntr:mod:{name}"));
            out.push(Triple::new(s.clone(), a.clone(), rsntr("Modulation")));
            out.push(Triple::new(node.clone(), rsntr("installed"), s.clone()));
            out.push(Triple::new(s.clone(), rsntr("name"), text(&name)));
            out.push(Triple::new(
                s.clone(),
                rsntr("enabled"),
                boolean(enabled != 0),
            ));
            out.push(Triple::new(s.clone(), rsntr("hash"), text(&sha)));
            for cap in serde_json::from_str::<Vec<String>>(&caps).unwrap_or_default() {
                out.push(Triple::new(s.clone(), rsntr("cap"), text(&cap)));
            }
            if let Some(note) = note {
                out.push(Triple::new(s, rsntr("note"), text(&note)));
            }
        }
    }

    // _apps → rsntr:App (+ rsntr:Table for the tables the caller may read)
    if unfiltered || readable(conn, peer, "_apps")? {
        sections.apps = true;
        for app in crate::apps::list_apps(conn).map_err(|e| match e {
            crate::error::NodeError::Db(e) => e,
            other => rusqlite::Error::ModuleError(other.to_string()),
        })? {
            let s = NamedNode::new_unchecked(format!("urn:rsntr:app:{}", app.app_id));
            out.push(Triple::new(s.clone(), a.clone(), rsntr("App")));
            out.push(Triple::new(node.clone(), rsntr("hosts"), s.clone()));
            out.push(Triple::new(s.clone(), rsntr("name"), text(&app.name)));
            if let Some(v) = &app.version {
                out.push(Triple::new(s.clone(), rsntr("version"), text(v)));
            }
            out.push(Triple::new(
                s.clone(),
                rsntr("libVersion"),
                text(&app.lib_version),
            ));
            out.push(Triple::new(
                s.clone(),
                rsntr("platform"),
                text(&app.platform),
            ));
            out.push(Triple::new(
                s.clone(),
                rsntr("firstSeen"),
                typed(&app.first_seen, "dateTime"),
            ));
            out.push(Triple::new(
                s.clone(),
                rsntr("lastSeen"),
                typed(&app.last_seen, "dateTime"),
            ));
            if let Some(note) = &app.note {
                out.push(Triple::new(s.clone(), rsntr("note"), text(note)));
            }
            for table in &app.tables {
                // A table is named only to a caller who may read it: the
                // projection-honesty rule applied to tables.
                if !(unfiltered || readable(conn, peer, table)?) {
                    continue;
                }
                let t = NamedNode::new_unchecked(format!("urn:rsntr:table:{table}"));
                out.push(Triple::new(s.clone(), rsntr("declares"), t.clone()));
                out.push(Triple::new(t.clone(), a.clone(), rsntr("Table")));
                out.push(Triple::new(t.clone(), rsntr("name"), text(table)));
                let comment: Option<String> = conn
                    .query_row(
                        "SELECT comment FROM _comments WHERE name = ?1",
                        [table],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(c) = comment {
                    out.push(Triple::new(
                        t,
                        NamedNode::new_unchecked(format!("{RDFS_NS}comment")),
                        text(&c),
                    ));
                }
            }
        }
    }

    // chat_rooms (+ chat_members) → rsntr:Room
    if table_exists(conn, "chat_rooms")? && (unfiltered || readable(conn, peer, "chat_rooms")?) {
        sections.rooms = true;
        let mut stmt =
            conn.prepare("SELECT room_id, name, host FROM chat_rooms ORDER BY room_id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let rooms: Vec<(String, String, String)> = rows.collect::<Result<Vec<_>, _>>()?;
        let members_ok = table_exists(conn, "chat_members")?;
        for (room_id, name, host) in rooms {
            let Ok(s) = NamedNode::new(room_id.clone()) else {
                continue;
            };
            out.push(Triple::new(s.clone(), a.clone(), rsntr("Room")));
            out.push(Triple::new(node.clone(), rsntr("room"), s.clone()));
            out.push(Triple::new(s.clone(), rsntr("name"), text(&name)));
            out.push(Triple::new(s.clone(), rsntr("host"), peer_iri(&host)));
            if members_ok {
                let mut stmt = conn.prepare(
                    "SELECT member FROM chat_members WHERE room_id = ?1 ORDER BY member",
                )?;
                let members = stmt.query_map([&room_id], |r| r.get::<_, String>(0))?;
                for m in members {
                    out.push(Triple::new(s.clone(), rsntr("member"), peer_iri(&m?)));
                }
            }
        }
    }

    // The caller's projection points, through the shared builder: the
    // Projection frame's triples minus its header, with a flat
    // `rsntr:offers` from the node instead of the frame's rdf:List.
    if let ProjectionOutcome::Built(p) = build_projection(conn, peer, "", "", unfiltered)? {
        let offered: Vec<String> = p.offers.iter().map(|pt| pt.iri.clone()).collect();
        let doc = resonator_protocol::EnvelopeObject::Projection(p)
            .to_turtle()
            .unwrap_or_default();
        let mut parser = oxttl::TurtleParser::new();
        for (name, iri) in IMPLIED_PREFIXES {
            if let Ok(p) = parser.clone().with_prefix(name, iri) {
                parser = p;
            }
        }
        let rdf_type_iri = format!("{RDF_NS}type");
        let rdf_rest = format!("{RDF_NS}rest");
        let offers_pred = format!("{RSNTR_NS}offers");
        let projection_cls = format!("{RSNTR_NS}Projection");
        let parsed: Vec<Triple> = parser.for_slice(doc.as_bytes()).flatten().collect();
        // The frame header: the blank node typed rsntr:Projection. Its
        // own triples and the cells of its rdf:List of offers are the
        // frame's scaffolding, not node state; the points' coupling
        // lists (also rdf:Lists) stay intact.
        let header = parsed.iter().find_map(|t| {
            (t.predicate.as_str() == rdf_type_iri
                && matches!(&t.object, Term::NamedNode(n) if n.as_str() == projection_cls))
            .then(|| t.subject.clone())
        });
        let mut drop: std::collections::HashSet<oxrdf::NamedOrBlankNode> =
            std::collections::HashSet::new();
        if let Some(h) = header {
            let mut cell = parsed.iter().find_map(|t| {
                (t.subject == h && t.predicate.as_str() == offers_pred).then(|| match &t.object {
                    Term::BlankNode(b) => Some(oxrdf::NamedOrBlankNode::BlankNode(b.clone())),
                    Term::NamedNode(n) => Some(oxrdf::NamedOrBlankNode::NamedNode(n.clone())),
                    _ => None,
                })
            });
            drop.insert(h);
            while let Some(Some(c)) = cell {
                if !drop.insert(c.clone()) {
                    break;
                }
                cell = parsed.iter().find_map(|t| {
                    (t.subject == c && t.predicate.as_str() == rdf_rest).then(|| match &t.object {
                        Term::BlankNode(b) => Some(oxrdf::NamedOrBlankNode::BlankNode(b.clone())),
                        Term::NamedNode(n) => Some(oxrdf::NamedOrBlankNode::NamedNode(n.clone())),
                        _ => None,
                    })
                });
            }
        }
        for t in parsed {
            if !drop.contains(&t.subject) {
                out.push(t);
            }
        }
        for iri in offered {
            if let Ok(p) = NamedNode::new(iri) {
                out.push(Triple::new(node.clone(), rsntr("offers"), p));
                sections.points += 1;
            }
        }
    }

    Ok((out, sections))
}

/// Peers' rows exist only for the owner-facing count in audit reasons.
pub fn peer_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM _peers", [], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::ensure_node_tables;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        ensure_node_tables(&conn).expect("tables");
        crate::ddl::seed_rsntr_defaults(&conn).expect("defaults");
        conn.execute(
            "INSERT OR REPLACE INTO _rsntr (key, value) VALUES ('endpoint_id', ?1)",
            ["ab".repeat(32)],
        )
        .expect("id");
        conn.execute(
            "INSERT INTO _peers (endpoint_id, name, addrs, added_at) \
             VALUES (?1, 'bob', '[\"127.0.0.1:4433\"]', '2026-01-01T00:00:00Z')",
            ["cd".repeat(32)],
        )
        .expect("peer");
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
             VALUES (?1, '_peers', 'read', 'allow', 'test')",
            ["cd".repeat(32)],
        )
        .expect("policy");
        conn
    }

    fn has(triples: &[Triple], pred_local: &str) -> usize {
        let p = format!("{RSNTR_NS}{pred_local}");
        triples.iter().filter(|t| t.predicate.as_str() == p).count()
    }

    fn typed_as(triples: &[Triple], class_local: &str) -> usize {
        let c = format!("{RSNTR_NS}{class_local}");
        triples
            .iter()
            .filter(|t| {
                t.predicate.as_str() == format!("{RDF_NS}type")
                    && matches!(&t.object, Term::NamedNode(n) if n.as_str() == c)
            })
            .count()
    }

    #[test]
    fn owner_sees_everything_peer_sees_its_grants() {
        let conn = db();
        let (all, sections) = build_system_graph(&conn, "owner", true).expect("owner");
        assert!(sections.peers && sections.policy && sections.modulations);
        assert_eq!(typed_as(&all, "Node"), 1);
        assert_eq!(typed_as(&all, "Peer"), 1);
        assert_eq!(typed_as(&all, "PolicyRule"), 1);
        assert!(has(&all, "serves") >= 3, "served modulations listed");
        assert_eq!(has(&all, "addr"), 1);

        let bob = "cd".repeat(32);
        let (mine, sections) = build_system_graph(&conn, &bob, false).expect("bob");
        assert!(sections.peers && !sections.policy && !sections.modulations);
        assert_eq!(typed_as(&mine, "Peer"), 1);
        assert_eq!(typed_as(&mine, "PolicyRule"), 0);

        // Apps: the owner sees the app and its table with the comment; a
        // peer with read on _apps but not on the table sees the app only.
        conn.execute_batch("CREATE TABLE notebook_pages (id INTEGER PRIMARY KEY)")
            .unwrap();
        let app = crate::apps::AppInfo {
            id: "org.example.notebook".into(),
            name: Some("Notebook".into()),
            version: None,
        };
        crate::apps::register_app(&conn, &app).unwrap();
        for (sql, params) in crate::apps::declare_statements(
            &conn,
            &app.id,
            &[crate::apps::TableDecl {
                name: "notebook_pages".into(),
                comment: "pages of the notebook".into(),
            }],
        )
        .unwrap()
        {
            conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
                .unwrap();
        }
        let (all, sections) = build_system_graph(&conn, "owner", true).expect("owner");
        assert!(sections.apps);
        assert_eq!(typed_as(&all, "App"), 1);
        assert_eq!(typed_as(&all, "Table"), 1);
        assert_eq!(has(&all, "declares"), 1);
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
             VALUES (?1, '_apps', 'read', 'allow')",
            ["cd".repeat(32)],
        )
        .unwrap();
        let (mine, _) = build_system_graph(&conn, &bob, false).expect("bob");
        assert_eq!(typed_as(&mine, "App"), 1);
        assert_eq!(
            has(&mine, "declares"),
            0,
            "the table itself is not readable by bob"
        );

        let (none, sections) =
            build_system_graph(&conn, &"ef".repeat(32), false).expect("stranger");
        assert!(
            !(sections.peers
                || sections.policy
                || sections.modulations
                || sections.rooms
                || sections.apps),
            "{sections:?}"
        );
        assert_eq!(typed_as(&none, "Node"), 1, "the node itself is public");
        assert_eq!(typed_as(&none, "Peer"), 0);
    }
}
