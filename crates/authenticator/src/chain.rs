//! The decider chain: ordered tiers from `_rsntr.auth_chain`, per-tier
//! authority masks from `_rsntr.auth_masks`, first non-Escalate answer
//! wins, tail default Deny.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};

use rusqlite::Connection;
use serde::Deserialize;
use tracing::warn;

use crate::types::{Decision, Footprint};

/// A chain tier. Sees the node's database connection because the
/// tiers read `_policy`, `_scripts`, `_decisions`, `_inbox`, and audit
/// history from it.
pub trait Tier: Send + Sync {
    /// The name this tier is addressed by in `auth_chain`, and reported
    /// as `Decided::decided_by`.
    fn name(&self) -> &str;

    /// Decides on one request. `action` is a policy action string
    /// ("read", "write", "knock", "media", "entrain", "mod:<name>");
    /// `statement` is the SQL/signal text, or `""` where the request
    /// carries none. On a `knock` it is the knock's own message, so a
    /// tier can answer on an invite code or a pairing token rather than
    /// parking every stranger for a human.
    ///
    /// `statement` is remote input in every case, and on a knock it has
    /// passed no parser at all: treat it as data, match it, never
    /// interpolate it into a query. Tiers must never fail open: internal
    /// errors are returned as [`Decision::Escalate`].
    fn decide(
        &self,
        conn: &Connection,
        peer: &str,
        action: &str,
        footprint: &Footprint,
        statement: &str,
    ) -> Decision;
}

/// One tier's authority mask, as declared in `_rsntr.auth_masks`:
///
/// ```json
/// { "ai": { "allow": ["read"], "deny": ["read", "write"] } }
/// ```
///
/// For each decision kind (`allow`, `allow_narrowed`, `deny`) the mask
/// lists the actions the tier may emit it for (`"*"` for all). A kind
/// absent from a tier's mask is forbidden. A tier absent from
/// `auth_masks` has full authority. Escalate is always permitted: it
/// is abstention, not authority. A masked-out decision is downgraded
/// to Escalate.
#[derive(Debug, Default, Deserialize)]
struct TierMask {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    allow_narrowed: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

impl TierMask {
    fn permits(&self, decision: &Decision, action: &str) -> bool {
        let list = match decision {
            Decision::Allow => &self.allow,
            Decision::AllowNarrowed { .. } => &self.allow_narrowed,
            Decision::Deny { .. } => &self.deny,
            Decision::Escalate => return true,
        };
        list.iter().any(|a| a == "*" || a == action)
    }
}

/// The chain's final answer: never Escalate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub decision: Decision,
    /// The tier that answered, or `"default"` for the implicit tail
    /// deny.
    pub decided_by: String,
}

const DEFAULT_DENY_REASON: &str = "no decider in the chain allowed the request";

/// The chain combinator. Holds a registry of available tiers by name;
/// which of them run, in what order, and with what authority is
/// configuration read from `_rsntr` on every decide, so an UPDATE
/// takes effect on the next request.
#[derive(Default)]
pub struct Chain {
    tiers: HashMap<String, Box<dyn Tier>>,
}

impl Chain {
    /// An empty registry. With no tiers registered (or an empty
    /// `auth_chain`) every request is denied by the tail.
    pub fn new() -> Self {
        Self {
            tiers: HashMap::new(),
        }
    }

    /// A registry with the built-in tiers: `"cache"`, `"policy"`,
    /// `"script"`, and `"human"`. Which of them run (and in what order)
    /// stays `_rsntr.auth_chain` configuration.
    ///
    /// `"script"` is present only when the crate's `script` feature is
    /// on. Without it, an `auth_chain` naming `"script"` finds no tier
    /// registered under that name, which the chain already treats as an
    /// abstention — the same as a tier that failed — and the tail default
    /// is still Deny.
    pub fn with_builtin_tiers() -> Self {
        let mut chain = Self::new();
        chain.register(Box::new(crate::cache::CacheTier));
        chain.register(Box::new(crate::policy::PolicyTier));
        #[cfg(feature = "script")]
        chain.register(Box::new(crate::script::ScriptTier::default()));
        chain.register(Box::new(crate::human::HumanTier));
        chain
    }

    /// Registers a tier under its own name; replaces any previous tier
    /// with that name.
    pub fn register(&mut self, tier: Box<dyn Tier>) {
        self.tiers.insert(tier.name().to_string(), tier);
    }

    /// Runs the configured chain. Never returns Escalate: the tail
    /// default is Deny.
    pub fn decide(
        &self,
        conn: &Connection,
        peer: &str,
        action: &str,
        footprint: &Footprint,
        statement: &str,
    ) -> Decided {
        let (chain_names, masks) = read_config(conn);

        for name in &chain_names {
            let Some(tier) = self.tiers.get(name) else {
                warn!(tier = %name, "auth_chain names an unregistered tier; skipping");
                continue;
            };
            // A panicking tier is a failed tier, and a failed tier
            // escalates; the chain must never fail open.
            let decision = panic::catch_unwind(AssertUnwindSafe(|| {
                tier.decide(conn, peer, action, footprint, statement)
            }))
            .unwrap_or_else(|_| {
                warn!(tier = %name, "tier panicked; treating as Escalate");
                Decision::Escalate
            });
            if matches!(decision, Decision::Escalate) {
                continue;
            }
            let permitted = masks.get(name).is_none_or(|m| m.permits(&decision, action));
            if !permitted {
                warn!(
                    tier = %name,
                    action = %action,
                    "decision exceeds tier authority mask; downgraded to Escalate"
                );
                continue;
            }
            return Decided {
                decision,
                decided_by: name.clone(),
            };
        }

        Decided {
            decision: Decision::Deny {
                reason: DEFAULT_DENY_REASON.to_string(),
            },
            decided_by: "default".to_string(),
        }
    }
}

/// One `_rsntr` value; `None` when the key or the table is absent.
fn read_rsntr(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM _rsntr WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .ok()
}

/// Reads `auth_chain` and `auth_masks`. Fail closed: a missing or
/// unparseable `auth_chain` is an empty chain (deny all); an
/// unparseable `auth_masks` also empties the chain, because running
/// tiers with unknown authority would fail open.
fn read_config(conn: &Connection) -> (Vec<String>, HashMap<String, TierMask>) {
    let chain = match read_rsntr(conn, "auth_chain") {
        Some(raw) => match serde_json::from_str::<Vec<String>>(&raw) {
            Ok(names) => names,
            Err(err) => {
                warn!(error = %err, "unparseable _rsntr auth_chain; denying all");
                return (Vec::new(), HashMap::new());
            }
        },
        None => Vec::new(),
    };
    let masks = match read_rsntr(conn, "auth_masks") {
        Some(raw) => match serde_json::from_str::<HashMap<String, TierMask>>(&raw) {
            Ok(masks) => masks,
            Err(err) => {
                warn!(error = %err, "unparseable _rsntr auth_masks; denying all");
                return (Vec::new(), HashMap::new());
            }
        },
        None => HashMap::new(),
    };
    (chain, masks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ActionKind;

    /// A stub tier that always answers the same decision.
    struct Fixed {
        name: &'static str,
        decision: Decision,
    }

    impl Tier for Fixed {
        fn name(&self) -> &str {
            self.name
        }
        fn decide(
            &self,
            _conn: &Connection,
            _peer: &str,
            _action: &str,
            _footprint: &Footprint,
            _statement: &str,
        ) -> Decision {
            self.decision.clone()
        }
    }

    fn conn_with_config(chain: &str, masks: Option<&str>) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE _rsntr (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        conn.execute(
            "INSERT INTO _rsntr (key, value) VALUES ('auth_chain', ?1)",
            [chain],
        )
        .unwrap();
        if let Some(m) = masks {
            conn.execute(
                "INSERT INTO _rsntr (key, value) VALUES ('auth_masks', ?1)",
                [m],
            )
            .unwrap();
        }
        conn
    }

    fn read_fp() -> Footprint {
        Footprint::from_tables(ActionKind::Read, [("notes", vec!["id"])])
    }

    #[test]
    fn tail_deny_on_empty_chain() {
        let conn = conn_with_config("[]", None);
        let chain = Chain::new();
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "default");
        assert!(matches!(d.decision, Decision::Deny { .. }));
    }

    #[test]
    fn tail_deny_when_rsntr_missing() {
        // No _rsntr table at all: fail closed.
        let conn = Connection::open_in_memory().unwrap();
        let chain = Chain::with_builtin_tiers();
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "default");
        assert!(matches!(d.decision, Decision::Deny { .. }));
    }

    #[test]
    fn chain_ordering_first_non_escalate_wins() {
        let conn = conn_with_config(r#"["a", "b"]"#, None);
        let mut chain = Chain::new();
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Escalate,
        }));
        chain.register(Box::new(Fixed {
            name: "b",
            decision: Decision::Allow,
        }));
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "b");
        assert_eq!(d.decision, Decision::Allow);

        // Same registry, reversed order, and a deciding first tier.
        let conn = conn_with_config(r#"["b", "a"]"#, None);
        let mut chain = Chain::new();
        chain.register(Box::new(Fixed {
            name: "b",
            decision: Decision::Deny {
                reason: "b says no".into(),
            },
        }));
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Allow,
        }));
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "b");
        assert!(matches!(d.decision, Decision::Deny { .. }));
    }

    #[test]
    fn mask_clamps_decision_to_escalate() {
        // "a" may only allow reads; on a write its Allow is clamped and
        // the tail denies.
        let conn = conn_with_config(r#"["a"]"#, Some(r#"{ "a": { "allow": ["read"] } }"#));
        let mut chain = Chain::new();
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Allow,
        }));

        let d = chain.decide(&conn, "alice", "write", &read_fp(), "INSERT ...");
        assert_eq!(d.decided_by, "default");
        assert!(matches!(d.decision, Decision::Deny { .. }));

        // The same tier's Allow passes for the permitted action.
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "a");
        assert_eq!(d.decision, Decision::Allow);
    }

    #[test]
    fn mask_clamp_falls_through_to_next_tier() {
        let conn = conn_with_config(r#"["a", "b"]"#, Some(r#"{ "a": { "deny": ["*"] } }"#));
        let mut chain = Chain::new();
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Allow,
        }));
        chain.register(Box::new(Fixed {
            name: "b",
            decision: Decision::Allow,
        }));
        // "a" wanted Allow but may only deny; "b" answers instead.
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "b");
        assert_eq!(d.decision, Decision::Allow);
    }

    #[test]
    fn mask_wildcard_action() {
        let conn = conn_with_config(r#"["a"]"#, Some(r#"{ "a": { "allow": ["*"] } }"#));
        let mut chain = Chain::new();
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Allow,
        }));
        let d = chain.decide(&conn, "alice", "mod:time", &read_fp(), "");
        assert_eq!(d.decided_by, "a");
        assert_eq!(d.decision, Decision::Allow);
    }

    #[test]
    fn unparseable_config_denies_all() {
        let conn = conn_with_config("not json", None);
        let mut chain = Chain::new();
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Allow,
        }));
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "default");
        assert!(matches!(d.decision, Decision::Deny { .. }));

        let conn = conn_with_config(r#"["a"]"#, Some("not json"));
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "default");
        assert!(matches!(d.decision, Decision::Deny { .. }));
    }

    #[test]
    fn unregistered_tier_is_skipped() {
        let conn = conn_with_config(r#"["ghost", "a"]"#, None);
        let mut chain = Chain::new();
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Allow,
        }));
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "a");
    }

    #[test]
    fn panicking_tier_escalates() {
        struct Panics;
        impl Tier for Panics {
            fn name(&self) -> &str {
                "panics"
            }
            fn decide(
                &self,
                _conn: &Connection,
                _peer: &str,
                _action: &str,
                _footprint: &Footprint,
                _statement: &str,
            ) -> Decision {
                panic!("boom");
            }
        }
        let conn = conn_with_config(r#"["panics", "a"]"#, None);
        let mut chain = Chain::new();
        chain.register(Box::new(Panics));
        chain.register(Box::new(Fixed {
            name: "a",
            decision: Decision::Allow,
        }));
        let d = chain.decide(&conn, "alice", "read", &read_fp(), "SELECT 1");
        assert_eq!(d.decided_by, "a");
        assert_eq!(d.decision, Decision::Allow);
    }
}
