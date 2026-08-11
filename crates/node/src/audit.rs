//! `_audit` appends. The chain decides; the node writes the ledger.

use rusqlite::Connection;
use tracing::warn;

use crate::clock::now_rfc3339;

/// Direct `_audit` append for outcomes decided by the pipeline itself
/// (peer gate, banned statements, help, projections) with no footprint.
pub fn audit_direct(
    conn: &Connection,
    peer: &str,
    id: &str,
    signal: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
) {
    audit_direct_dir(conn, "in", peer, id, signal, decision, decided_by, reason);
}

/// [`audit_direct`] with an explicit direction (`local` on the owner
/// channel, docs/owner-channel.md sec 6.1).
#[allow(clippy::too_many_arguments)]
pub fn audit_direct_dir(
    conn: &Connection,
    direction: &str,
    peer: &str,
    id: &str,
    signal: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
) {
    audit_full_dir(
        conn,
        direction,
        peer,
        id,
        signal,
        "{}",
        decision,
        decided_by,
        Some(reason),
        0,
    );
}

/// Full `_audit` append; returns the row id (0 on failure) so execution
/// can update `rows_out`/`bytes_out` afterwards.
#[allow(clippy::too_many_arguments)]
pub fn audit_full(
    conn: &Connection,
    peer: &str,
    id: &str,
    signal: &str,
    footprint_json: &str,
    decision: &str,
    decided_by: &str,
    reason: Option<&str>,
    duration_ms: u64,
) -> i64 {
    audit_full_dir(
        conn,
        "in",
        peer,
        id,
        signal,
        footprint_json,
        decision,
        decided_by,
        reason,
        duration_ms,
    )
}

/// [`audit_full`] with an explicit direction (`local` on the owner
/// channel).
#[allow(clippy::too_many_arguments)]
pub fn audit_full_dir(
    conn: &Connection,
    direction: &str,
    peer: &str,
    id: &str,
    signal: &str,
    footprint_json: &str,
    decision: &str,
    decided_by: &str,
    reason: Option<&str>,
    duration_ms: u64,
) -> i64 {
    let res = conn.execute(
        "INSERT INTO _audit \
           (at, direction, peer, request_id, signal, footprint, decision, \
            decided_by, reason, duration_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        (
            now_rfc3339(),
            direction,
            peer,
            id,
            signal,
            footprint_json,
            decision,
            decided_by,
            reason,
            duration_ms as i64,
        ),
    );
    match res {
        Ok(_) => conn.last_insert_rowid(),
        Err(e) => {
            warn!(error = %e, "failed to append _audit row");
            0
        }
    }
}

/// Updates the outcome columns of an earlier audit row.
pub fn audit_outcome(conn: &Connection, audit_id: i64, rows_out: i64, bytes_out: i64) {
    if audit_id == 0 {
        return;
    }
    if let Err(e) = conn.execute(
        "UPDATE _audit SET rows_out = ?1, bytes_out = ?2 WHERE id = ?3",
        (rows_out, bytes_out, audit_id),
    ) {
        warn!(error = %e, "failed to update _audit outcome");
    }
}
