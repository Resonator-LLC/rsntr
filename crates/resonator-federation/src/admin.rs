//! What `resonator federation peers` does: lists the keys this server has
//! seen for other domains, and lets an admin approve or deny them.
//!
//! It works on the database directly, so it may run while the server runs
//! (SQLite in WAL mode lets both write; the writes are short). The server
//! checks keys whenever a connection is set up, so a decision applies to
//! the next connection; one already open is kept until it closes (at the
//! latest when the server restarts).

use iroh::PublicKey;
use resonator_server::storage::{FederationStore, PeerRecord, PeerStatus, StorageError};
use thiserror::Error;

use crate::key::{key_domain, parse_key_domain};

/// Failure of an admin command.
#[derive(Debug, Error)]
pub enum AdminError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("{0}")]
    Usage(String),
}

/// The decision of `peers approve` or `peers deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Deny,
}

/// One line of `peers`: domain, key, status, first seen, last verified.
pub fn format_peers(peers: &[PeerRecord]) -> String {
    if peers.is_empty() {
        return "no peers seen yet\n".to_owned();
    }
    let width = peers.iter().map(|p| p.domain.len()).max().unwrap_or(0);
    let mut out = format!(
        "{:width$}  {:52}  {:11}  {:20}  {}\n",
        "DOMAIN", "KEY", "STATUS", "FIRST SEEN", "LAST VERIFIED"
    );
    for peer in peers {
        out.push_str(&format!(
            "{:width$}  {:52}  {:11}  {:20}  {}\n",
            peer.domain,
            key_text(&peer.key),
            peer.status.as_str(),
            utc(peer.first_seen),
            utc(peer.last_verified),
        ));
    }
    out
}

fn key_text(key: &[u8]) -> String {
    <&[u8; 32]>::try_from(key)
        .ok()
        .and_then(|bytes| PublicKey::from_bytes(bytes).ok())
        .map_or_else(|| "(invalid key)".to_owned(), |key| key_domain(&key))
}

/// Milliseconds since the epoch as `YYYY-MM-DD hh:mm:ssZ`.
fn utc(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Approves or denies keys, named by `target`: a key (its 52-character
/// key domain) or a DNS domain.
///
/// - `approve <key>` makes the key trusted for every domain it was seen
///   for (retiring a domain's previous key), or for its own key domain if
///   it was seen for none.
/// - `approve <domain>` trusts the domain's quarantined key; there must be
///   exactly one.
/// - `deny <key>` denies the key for every domain it was seen for, or for
///   its own key domain.
/// - `deny <domain>` denies every key of the domain that is trusted or
///   quarantined.
///
/// Returns the rows changed, as they are now.
pub async fn decide(
    store: &FederationStore,
    target: &str,
    decision: Decision,
    now: i64,
) -> Result<Vec<PeerRecord>, AdminError> {
    let target = target.trim().trim_end_matches('.').to_ascii_lowercase();
    let status = match decision {
        Decision::Approve => PeerStatus::Trusted,
        Decision::Deny => PeerStatus::Denied,
    };
    let peers = store.peers().await?;
    let mut chosen: Vec<(String, Vec<u8>)> = Vec::new();
    if let Ok(Some(key)) = parse_key_domain(&target) {
        chosen.extend(
            peers
                .iter()
                .filter(|p| p.key == key.as_bytes())
                .map(|p| (p.domain.clone(), p.key.clone())),
        );
        if chosen.is_empty() {
            chosen.push((target.clone(), key.as_bytes().to_vec()));
        }
    } else {
        let of_domain: Vec<&PeerRecord> = peers.iter().filter(|p| p.domain == target).collect();
        if of_domain.is_empty() {
            return Err(AdminError::Usage(format!(
                "no key seen for {target}; name the key instead"
            )));
        }
        match decision {
            Decision::Approve => {
                let quarantined: Vec<&&PeerRecord> = of_domain
                    .iter()
                    .filter(|p| p.status == PeerStatus::Quarantined)
                    .collect();
                match quarantined.as_slice() {
                    [one] => chosen.push((one.domain.clone(), one.key.clone())),
                    [] => {
                        return Err(AdminError::Usage(format!(
                            "{target} has no quarantined key to approve; name the key instead"
                        )));
                    }
                    _ => {
                        return Err(AdminError::Usage(format!(
                            "{target} has several quarantined keys; name the one to approve"
                        )));
                    }
                }
            }
            Decision::Deny => chosen.extend(
                of_domain
                    .iter()
                    .filter(|p| matches!(p.status, PeerStatus::Trusted | PeerStatus::Quarantined))
                    .map(|p| (p.domain.clone(), p.key.clone())),
            ),
        }
        if chosen.is_empty() {
            return Err(AdminError::Usage(format!(
                "{target} has no trusted or quarantined key to deny"
            )));
        }
    }
    for (domain, key) in &chosen {
        store.set_peer(domain, key, status, now).await?;
    }
    let after = store.peers().await?;
    Ok(after
        .into_iter()
        .filter(|p| chosen.iter().any(|(d, k)| *d == p.domain && *k == p.key))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00:00Z");
        assert_eq!(utc(1_793_664_000_000 + 3_723_000), "2026-11-03 01:02:03Z");
    }
}
