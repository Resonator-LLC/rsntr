//! resonator-authenticator: the decider chain.
//!
//! Runs on the serving node only, colocated with the data it guards.
//! The [`Chain`] runs the tiers named by `_rsntr` key `auth_chain` in
//! order, under the per-tier authority masks in `_rsntr` key
//! `auth_masks`; the first non-Escalate answer wins and the tail
//! default is Deny. A tier that fails (error, panic, missing
//! registration) counts as Escalate: the chain never fails open.
//!
//! Actions are plain strings ("read", "write", "knock", "media",
//! "entrain", and later "mod:<name>") so new modulations can be gated
//! by policy rows without any enum change here.
//!
//! This crate is synchronous by design; deciders that need async
//! (ai/human tiers) bridge internally.

pub mod cache;
pub mod chain;
pub mod fingerprint;
pub mod human;
pub mod policy;
pub mod script;
pub mod types;

pub use cache::{
    CacheTier, DECISIONS_DDL, ensure_decisions_table, invalidate_decisions, record_decision,
};
pub use chain::{Chain, Decided, Tier};
pub use fingerprint::{normalize_statement, statement_fingerprint};
pub use human::{AnswerError, HumanTier, InboxAnswer, PENDING_REASON, ParkedParams, answer_inbox};
pub use policy::{POLICY_DDL, PolicyTier, ensure_policy_table};
// The `_scripts` DDL is unconditional so the schema is identical either
// way; only the tier that evaluates the scripts is behind the feature.
pub use script::{SCRIPTS_DDL, ensure_scripts_table};
#[cfg(feature = "script")]
pub use script::ScriptTier;
pub use types::{ActionKind, Decision, Footprint};

/// Creates every table this crate owns (`_policy`, `_scripts`,
/// `_decisions`). Idempotent.
pub fn ensure_auth_tables(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    ensure_policy_table(conn)?;
    ensure_scripts_table(conn)?;
    ensure_decisions_table(conn)
}
