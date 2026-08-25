//! Decision and footprint types shared by the tiers and the node.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// What kind of access a statement's footprint represents, from the
/// sqlite authorizer pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionKind {
    #[default]
    Read,
    Write,
    /// DDL, pragma, transaction control, and anything else that is
    /// neither a plain read nor a plain write.
    Other,
}

impl ActionKind {
    /// Canonical lowercase name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Other => "other",
        }
    }

    /// The default policy action string for this kind: reads are
    /// "read", everything that can change state is "write".
    pub fn default_action(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write | Self::Other => "write",
        }
    }
}

/// Ground-truth statement footprint from the sqlite authorizer pass.
///
/// Owned by this crate so the node can depend on it without a cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Footprint {
    /// What kind of access the statement performs overall.
    pub kind: ActionKind,
    /// table name -> column names touched (empty set: table-level
    /// access with no per-column information, e.g. DELETE).
    pub tables: BTreeMap<String, BTreeSet<String>>,
    /// Constructs the authorizer refused during the collect pass
    /// (e.g. "attach", "detach", "pragma", "load_extension", "ddl").
    /// Non-empty means the statement must not run as-is.
    pub denied: BTreeSet<String>,
}

impl Footprint {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// True when the authorizer flagged at least one denied construct.
    pub fn has_denied(&self) -> bool {
        !self.denied.is_empty()
    }

    /// Records an authorizer-denied construct.
    pub fn deny_construct(&mut self, construct: impl Into<String>) {
        self.denied.insert(construct.into());
    }

    /// Builds a footprint from `(table, [columns])` pairs; convenient
    /// in tests and simple callers.
    pub fn from_tables<I, S, C>(kind: ActionKind, items: I) -> Self
    where
        I: IntoIterator<Item = (S, C)>,
        S: Into<String>,
        C: IntoIterator,
        C::Item: Into<String>,
    {
        let tables = items
            .into_iter()
            .map(|(t, cols)| (t.into(), cols.into_iter().map(Into::into).collect()))
            .collect();
        Self {
            kind,
            tables,
            denied: BTreeSet::new(),
        }
    }

    /// JSON form, for the node's `_audit.footprint` column.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// A decider's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Yes, but through this rewritten statement (e.g. a per-peer view).
    AllowNarrowed {
        rewrite: String,
    },
    Deny {
        reason: String,
    },
    /// This decider abstains; ask the next one.
    Escalate,
}

impl Decision {
    /// The audit ledger value; `None` for Escalate, which is never a
    /// final decision.
    pub fn audit_str(&self) -> Option<&'static str> {
        match self {
            Self::Allow => Some("allow"),
            Self::AllowNarrowed { .. } => Some("allow_narrowed"),
            Self::Deny { .. } => Some("deny"),
            Self::Escalate => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_json_round_trip() {
        let mut fp = Footprint::from_tables(ActionKind::Write, [("notes", vec!["id", "body"])]);
        fp.deny_construct("attach");
        let json = fp.to_json();
        let back: Footprint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, fp);
        assert!(back.has_denied());
        assert!(json.contains("\"write\""));
    }

    #[test]
    fn action_kind_defaults() {
        assert_eq!(ActionKind::Read.default_action(), "read");
        assert_eq!(ActionKind::Write.default_action(), "write");
        assert_eq!(ActionKind::Other.default_action(), "write");
    }
}
