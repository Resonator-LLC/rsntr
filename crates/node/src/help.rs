//! The help modulation: owner text from `_rsntr.help_text` plus facts
//! generated from `_policy`, so help never overstates access. Served to
//! everyone, strangers included.

use rusqlite::Connection;

use crate::audit::audit_direct;
use crate::ddl::{get_rsntr, served_modulations};

/// The drill-down topics a help overview advertises; each is answerable
/// by a help query whose text is the topic name.
const HELP_TOPICS: [&str; 5] = ["modulations", "tables", "knock", "projection", "examples"];

/// Builds the help response text and topic list for `topic` (empty means
/// the overview), and audits the request. Runs on the db thread. Never
/// fails: help is always answered.
pub fn build_help(conn: &Connection, peer: &str, id: &str, topic: &str) -> (String, Vec<String>) {
    let topic = topic.trim();
    let text = match topic {
        "" => help_overview(conn, peer),
        "modulations" => help_modulations(conn),
        "tables" => help_tables(conn, peer),
        "knock" => help_knock(),
        "projection" => help_projection(),
        "examples" => help_examples(),
        // Unknown topic: fall back to the overview so help always answers.
        _ => help_overview(conn, peer),
    };
    let reason = if topic.is_empty() {
        "help overview".to_string()
    } else {
        format!("help topic {topic}")
    };
    audit_direct(conn, peer, id, "help", "allow", "help", &reason);
    (text, HELP_TOPICS.iter().map(|s| s.to_string()).collect())
}

/// Owner-authored overview from `_rsntr.help_text`; empty when unset.
fn owner_help_text(conn: &Connection) -> String {
    get_rsntr(conn, "help_text").unwrap_or_default()
}

/// Tables the asking peer may currently read: `_policy` rows with
/// `action='read'`, `effect='allow'`, and `peer_or_group` matching the
/// peer or `'*'`. A returned `'*'` means "all tables".
fn readable_tables(conn: &Connection, peer: &str) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT table_name FROM _policy \
         WHERE action = 'read' AND effect = 'allow' \
           AND (peer_or_group = ?1 OR peer_or_group = '*')",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map([peer], |r| r.get::<_, String>(0)) {
        Ok(iter) => iter.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

/// One line describing what the peer may read, from [`readable_tables`].
fn readable_tables_line(tables: &[String]) -> String {
    if tables.iter().any(|t| t == "*") {
        "Readable now: all tables.".to_string()
    } else if tables.is_empty() {
        "Readable now: nothing yet; you are not admitted to read any table.".to_string()
    } else {
        format!("Readable now: {}.", tables.join(", "))
    }
}

fn help_overview(conn: &Connection, peer: &str) -> String {
    let modulations = served_modulations(conn).join(", ");
    let tables = readable_tables(conn, peer);
    let owner = owner_help_text(conn);
    let mut out = String::new();
    if !owner.trim().is_empty() {
        out.push_str(owner.trim_end());
        out.push_str("\n\n");
    }
    out.push_str("This is a resonator node (rsntr, envelope 0.1).\n");
    out.push_str(&format!("I serve the modulations: {modulations}.\n"));
    out.push_str(&readable_tables_line(&tables));
    out.push('\n');
    out.push_str(
        "Not admitted yet? Introduce yourself and I may let you in:\n  \
         [] a rsntr:Knock ; rsntr:message \"who you are and what you want\" .\n",
    );
    out.push_str(
        "Ask for more: send a help query with text one of: \
         modulations, tables, knock, projection, examples.",
    );
    out
}

fn help_modulations(conn: &Connection) -> String {
    let modulations = served_modulations(conn).join(", ");
    format!(
        "I serve these modulations: {modulations}.\n\
         Send a rsntr:Query with rsntr:mod set to one of them. \
         The 'help' modulation returns this guidance; 'sql-sqlite' runs SQL; \
         'sparql' runs SPARQL over my RDF store."
    )
}

fn help_tables(conn: &Connection, peer: &str) -> String {
    let tables = readable_tables(conn, peer);
    let mut out = readable_tables_line(&tables);
    out.push('\n');
    out.push_str(
        "To run a read:\n  \
         [] a rsntr:Query ; rsntr:mod \"sql-sqlite\" ; rsntr:signal \"SELECT ...\" .",
    );
    out
}

fn help_knock() -> String {
    "Not admitted? Introduce yourself with a knock:\n  \
     [] a rsntr:Knock ; rsntr:message \"who you are and what you want\" .\n\
     The owner reviews knocks and may add you as a peer. Until then you \
     may only ask for help and knock."
        .to_string()
}

fn help_projection() -> String {
    "The projection is my capability surface as data: what you can read \
     (Radiant), do (Excitable), and watch (Sympathetic), with the input \
     each needs. You render it however you like.\n\
     Fetch it:   [] a rsntr:Query ; rsntr:mod \"projection\" ; rsntr:signal \"\" .\n\
     Drill in:   rsntr:signal is the path a point's rsntr:projects handed you.\n\
     Watch one:  [] a rsntr:Entrain ; rsntr:id \"<ULID>\" ; rsntr:point <iri> .\n\
     Stop:       [] a rsntr:Damp ; rsntr:point <iri> .  (or just close the stream)"
        .to_string()
}

fn help_examples() -> String {
    "Examples:\n\
     Overview:  [] a rsntr:Query ; rsntr:mod \"help\" ; rsntr:signal \"\" .\n\
     A read:    [] a rsntr:Query ; rsntr:mod \"sql-sqlite\" ; rsntr:signal \"SELECT 1\" .\n\
     SPARQL:    [] a rsntr:Query ; rsntr:mod \"sparql\" ; rsntr:signal \"SELECT * WHERE { ?s ?p ?o }\" .\n\
     A knock:   [] a rsntr:Knock ; rsntr:message \"who you are and what you want\" ."
        .to_string()
}
