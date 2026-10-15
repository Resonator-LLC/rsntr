//! The `mod-install` modulation: a policy-gated remote peer installs,
//! updates, or removes a wasm mod (docs/mod-install.md).
//!
//! The request is an `rsntr:Execute` with mod `mod-install` whose
//! `rsntr:signal` is a Turtle document; the typed subject's class is the
//! verb. The wasm itself never rides the request (it dwarfs the frame
//! budget): the installer names a blake3 blob hash, and the serving node
//! fetches those bytes from the installer out of band over iroh-blobs.
//! This module owns the request parse, the gate, and the row writes; the
//! async orchestration (blob fetch + hot reload) lives in the pipeline.

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use resonator_authenticator::{ActionKind, Footprint, ParkedParams, statement_fingerprint};
use ulid::Ulid;

use oxrdf::{Term, Triple, vocab::rdf};
use oxttl::TurtleParser;

use resonator_protocol::vocab::{IMPLIED_PREFIXES, RSNTR_NS};

use crate::audit::audit_direct;
use crate::pipeline::peer_known;

/// The registry's own default memory ceiling when the installer names
/// none (mirrors `resonator_mods` `DEFAULT_MEMORY_MB`).
pub const DEFAULT_MOD_MEMORY_MB: u64 = 64;
/// Hard ceiling on an installed wasm blob (bytes).
pub const MAX_WASM_BYTES: usize = 4 * 1024 * 1024;

/// The verb carried by a mod-install signal.
#[derive(Debug, PartialEq, Eq)]
pub enum Verb {
    Install,
    Remove,
}

/// A parsed mod-install request.
#[derive(Debug)]
pub struct ModInstall {
    pub verb: Verb,
    pub name: String,
    /// The blob hash to fetch (blake3, `blake3:` prefix accepted); only
    /// meaningful for `Install`.
    pub hash: Option<String>,
    /// Optional declared byte size, cross-checked after fetch.
    pub bytes: Option<u64>,
    pub caps: Vec<String>,
    /// The installer's serving addresses, so the target reaches its
    /// blob-serving daemon and not a short-lived request endpoint.
    pub addrs: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub memory_mb: Option<u64>,
    pub note: Option<String>,
}

fn term_text(t: &Term) -> Option<String> {
    match t {
        Term::NamedNode(n) => Some(n.as_str().to_string()),
        Term::Literal(l) => Some(l.value().to_string()),
        Term::BlankNode(_) => None,
    }
}

fn rsntr(local: &str) -> String {
    format!("{RSNTR_NS}{local}")
}

/// Parses a mod-install signal. `Err` carries a human message for a
/// `protocol-error` frame.
pub fn parse(signal: &str) -> Result<ModInstall, String> {
    let mut parser = TurtleParser::new();
    for (name, iri) in IMPLIED_PREFIXES {
        parser = parser
            .with_prefix(name, iri)
            .expect("implied prefix IRIs are static and valid");
    }
    let mut triples: Vec<Triple> = Vec::new();
    for item in parser.for_slice(signal.as_bytes()) {
        triples.push(item.map_err(|e| format!("mod-install signal is not valid Turtle: {e}"))?);
    }

    let install_cls = rsntr("ModInstall");
    let remove_cls = rsntr("ModRemove");
    let mut verb = None;
    let mut subject = None;
    for t in &triples {
        if t.predicate.as_ref() == rdf::TYPE
            && let Term::NamedNode(n) = &t.object
        {
            let cls = n.as_str();
            if cls == install_cls {
                verb = Some(Verb::Install);
                subject = Some(t.subject.clone());
            } else if cls == remove_cls {
                verb = Some(Verb::Remove);
                subject = Some(t.subject.clone());
            }
        }
    }
    let (verb, subject) = match (verb, subject) {
        (Some(v), Some(s)) => (v, s),
        _ => {
            return Err(
                "mod-install signal carries no rsntr:ModInstall or rsntr:ModRemove node".into(),
            );
        }
    };

    let props: Vec<(&str, &Term)> = triples
        .iter()
        .filter(|t| t.subject == subject)
        .map(|t| (t.predicate.as_str(), &t.object))
        .collect();
    let one = |local: &str| -> Option<String> {
        let p = rsntr(local);
        props
            .iter()
            .find(|(pred, _)| *pred == p)
            .and_then(|(_, o)| term_text(o))
    };
    let all = |local: &str| -> Vec<String> {
        let p = rsntr(local);
        props
            .iter()
            .filter(|(pred, _)| *pred == p)
            .filter_map(|(_, o)| term_text(o))
            .collect()
    };

    let name = one("name").ok_or("mod-install signal has no rsntr:name")?;
    if !name_ok(&name) {
        return Err(format!(
            "mod name {name:?} must be ASCII alphanumeric plus - _ ."
        ));
    }
    Ok(ModInstall {
        verb,
        name,
        hash: one("hash"),
        bytes: one("bytes").and_then(|s| s.parse().ok()),
        caps: all("caps"),
        addrs: all("addr"),
        timeout_ms: one("timeoutMs").and_then(|s| s.parse().ok()),
        memory_mb: one("memoryMb").and_then(|s| s.parse().ok()),
        note: one("note"),
    })
}

/// Mod-name charset (mirrors `resonator_mods` / cli `check_name`).
pub fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// What the install gate decided.
pub enum Gate {
    UnknownPeer,
    Denied {
        reason: String,
    },
    /// No policy took a position: the request parked in `_inbox` for
    /// the owner (the cross-peer approval flow, docs/mod-install.md).
    Pending,
    Allowed,
}

/// True when a `_policy` row with `effect` matches `action` on `name`
/// for `peer` (or the `*` wildcards).
fn policy_matches(conn: &Connection, peer: &str, name: &str, action: &str, effect: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM _policy \
         WHERE action = ?4 AND effect = ?1 \
           AND (peer_or_group = ?2 OR peer_or_group = '*') \
           AND (table_name = ?3 OR table_name = '*') LIMIT 1",
        (effect, peer, name, action),
        |_| Ok(()),
    )
    .optional()
    .map(|o| o.is_some())
    .unwrap_or(false)
}

/// Parks an unmatched install in `_inbox` for the owner, deduplicating
/// on the statement fingerprint like the human tier: retries while the
/// owner thinks answer pending again without stacking rows. The parked
/// footprint names the mod as its one table with action `mod-install`,
/// which is exactly the shape `rsntr inbox answer --remember` turns
/// into the `(peer, <name>, 'mod-install', 'allow')` row this gate
/// checks; `--grant '<name>=mod-cap:<cap>'` covers the caps.
fn park_install(conn: &Connection, peer: &str, signal: &str, name: &str) -> rusqlite::Result<()> {
    let footprint = Footprint::from_tables(ActionKind::Other, [(name, Vec::<String>::new())]);
    let fp = statement_fingerprint("mod-install", signal, &footprint);
    let pending: Option<()> = conn
        .query_row(
            "SELECT 1 FROM _inbox WHERE peer = ?1 AND decision IS NULL \
               AND sql <> '' \
               AND json_extract(params, '$.fingerprint') = ?2",
            (peer, &fp),
            |_| Ok(()),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(());
    }
    let parked = ParkedParams {
        action: "mod-install".to_string(),
        fingerprint: fp,
        footprint,
    };
    conn.execute(
        "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        (
            Ulid::new().to_string(),
            peer,
            signal,
            serde_json::to_string(&parked).expect("parked params json"),
        ),
    )?;
    Ok(())
}

/// Peer gate, then the `mod-install` right on this name, then the caps
/// ceiling (`mod-cap:<cap>` per requested cap). An explicit deny row is
/// a hard deny; a granted install proceeds; an UNMATCHED install parks
/// in `_inbox` for the owner and answers Pending. Every outcome writes
/// one `_audit` row. `caps` is empty for a remove. `signal` is the
/// request's turtle, stored verbatim in the parked row.
pub fn gate(
    conn: &Connection,
    peer: &str,
    id: &str,
    name: &str,
    caps: &[String],
    signal: &str,
) -> Gate {
    let audit_sig = format!("mod-install {name}");
    if !peer_known(conn, peer) {
        audit_direct(
            conn,
            peer,
            id,
            &audit_sig,
            "deny",
            "peer-gate",
            "unknown peer",
        );
        return Gate::UnknownPeer;
    }
    if policy_matches(conn, peer, name, "mod-install", "deny") {
        let reason = format!("mod-install on {name:?} is denied by policy");
        audit_direct(conn, peer, id, &audit_sig, "deny", "policy", &reason);
        return Gate::Denied { reason };
    }
    if !policy_matches(conn, peer, name, "mod-install", "allow") {
        // No position taken: park for the owner (approval flow).
        match park_install(conn, peer, signal, name) {
            Ok(()) => {
                audit_direct(
                    conn,
                    peer,
                    id,
                    &audit_sig,
                    "deny",
                    "human",
                    resonator_authenticator::PENDING_REASON,
                );
                return Gate::Pending;
            }
            Err(e) => {
                let reason = format!("no policy grants you mod-install on {name:?}");
                audit_direct(
                    conn,
                    peer,
                    id,
                    &audit_sig,
                    "deny",
                    "policy",
                    &format!("{reason} (park failed: {e})"),
                );
                return Gate::Denied { reason };
            }
        }
    }
    let missing: Vec<&String> = caps
        .iter()
        .filter(|c| {
            let action = format!("mod-cap:{c}");
            policy_matches(conn, peer, name, &action, "deny")
                || !policy_matches(conn, peer, name, &action, "allow")
        })
        .collect();
    if !missing.is_empty() {
        let reason = format!(
            "the caps ceiling for {name:?} does not grant: {}",
            missing
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        audit_direct(conn, peer, id, &audit_sig, "deny", "policy", &reason);
        return Gate::Denied { reason };
    }
    audit_direct(conn, peer, id, &audit_sig, "allow", "policy", "mod-install");
    Gate::Allowed
}

/// Lowercase hex sha256 of the wasm (the `_modulations.sha256` the
/// registry verifies at load).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Upserts a `_modulations` row `enabled=1` with the caps and limits.
/// The grant is the approval, so a remote install is enabled at once
/// (unlike the owner-lane `mod add`, which starts disabled).
#[allow(clippy::too_many_arguments)]
pub fn upsert(
    conn: &Connection,
    name: &str,
    wasm: &[u8],
    sha256: &str,
    caps_json: &str,
    limits_json: &str,
    note: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _modulations (name, wasm, sha256, enabled, caps, limits, note) \
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6) \
         ON CONFLICT(name) DO UPDATE SET wasm = ?2, sha256 = ?3, enabled = 1, \
             caps = ?4, limits = ?5, note = ?6",
        rusqlite::params![name, wasm, sha256, caps_json, limits_json, note],
    )?;
    Ok(())
}

/// Sets a row's enabled bit (used to disable a mod refused at reload).
pub fn set_enabled(conn: &Connection, name: &str, enabled: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE _modulations SET enabled = ?2 WHERE name = ?1",
        rusqlite::params![name, i64::from(enabled)],
    )?;
    Ok(())
}

/// Deletes a mod row; true when it existed.
pub fn remove(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let n = conn.execute("DELETE FROM _modulations WHERE name = ?1", [name])?;
    Ok(n > 0)
}
