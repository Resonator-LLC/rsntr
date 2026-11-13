//! The resonator node for apps: a UniFFI interface over `resonator-node`
//! (proc-macro mode, no UDL), from which `uniffi-bindgen` generates Swift
//! and Kotlin (see `README.md` and `bindings/`).
//!
//! An app starts a [`ResonatorNode`] in device mode on a directory of its
//! own, loads [`ResonatorNode::signed_in_url`] in a WebView, and calls
//! [`ResonatorNode::enter_background`] / [`ResonatorNode::enter_foreground`]
//! from its lifecycle. Every call blocks: call them off the main thread.
//! The node runs on a tokio runtime of its own.

#[cfg(target_os = "android")]
mod android;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use resonator_node::{BackgroundReport, ForegroundReport, Handle, LeaseState, NodeError};

uniffi::setup_scaffolding!();

/// What can go wrong.
/// (The fields are named `detail`, not `message`: Kotlin exceptions
/// already have a `message`.)
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ResonatorError {
    /// The configuration is not valid.
    #[error("invalid configuration: {detail}")]
    Config { detail: String },
    /// The node could not start (or do what was asked).
    #[error("{detail}")]
    Node { detail: String },
    /// The node has been stopped.
    #[error("the node is stopped")]
    Stopped,
}

impl From<NodeError> for ResonatorError {
    fn from(error: NodeError) -> Self {
        match error {
            NodeError::Stopped => ResonatorError::Stopped,
            NodeError::Config(_) | NodeError::Invalid(_) => ResonatorError::Config {
                detail: error.to_string(),
            },
            error => ResonatorError::Node {
                detail: error.to_string(),
            },
        }
    }
}

/// The push service of a device's push token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PushPlatform {
    /// APNs, production (App Store and TestFlight builds).
    Apns,
    /// APNs sandbox (development builds from Xcode).
    ApnsSandbox,
    /// Firebase Cloud Messaging (Android).
    Fcm,
}

impl From<PushPlatform> for resonator_node::push::PushPlatform {
    fn from(platform: PushPlatform) -> Self {
        match platform {
            PushPlatform::Apns => Self::Apns,
            PushPlatform::ApnsSandbox => Self::ApnsSandbox,
            PushPlatform::Fcm => Self::Fcm,
        }
    }
}

/// How to start a node. The defaults make a device of a person of its
/// own (the person key is made on the first start and kept in
/// `data_dir/person.key`).
#[derive(Debug, Clone, uniffi::Record)]
pub struct NodeConfig {
    /// The app's directory for the node's files (database, keys).
    pub data_dir: String,
    /// The account's name before the `@` (default `me`).
    #[uniffi(default = None)]
    pub account: Option<String>,
    /// A name for this device, shown to its person.
    #[uniffi(default = None)]
    pub device_name: Option<String>,
    /// Serve other machines too (they sign in with a password; the token
    /// works from loopback only).
    #[uniffi(default = false)]
    pub allow_remote: bool,
    /// More settings, as TOML laid over these (`[federation]`,
    /// `[person]`, `[ai]`, …).
    #[uniffi(default = None)]
    pub extra_toml: Option<String>,
}

/// The host lease of the device's person, as the device sees it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LeaseInfo {
    /// The holder (`<z32 device key>`), if a lease is known.
    pub holder: Option<String>,
    pub term: u64,
    /// Milliseconds since the epoch.
    pub expires_at_ms: i64,
    /// This device holds the lease.
    pub held_here: bool,
    /// This device is an anchor.
    pub anchor: bool,
    /// This device reached a relay or a peer at its last look.
    pub online: bool,
}

impl From<LeaseState> for LeaseInfo {
    fn from(lease: LeaseState) -> Self {
        LeaseInfo {
            holder: lease.holder.map(|key| key.to_z32()),
            term: lease.term,
            expires_at_ms: lease.expires_at,
            held_here: lease.held_here,
            anchor: lease.anchor,
            online: lease.online,
        }
    }
}

/// What [`ResonatorNode::pair_join`] did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PairJoinResult {
    /// The person domain this device belongs to now.
    pub person: String,
    /// This device holds the person key too.
    pub admin: bool,
    /// The inviting device (`<z32 key>`).
    pub inviter: String,
}

/// A pairing invitation from [`ResonatorNode::pair_invite`]. The code and
/// the link each let a device in once: show them only to the person,
/// never log them.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct PairInvite {
    /// The invitation id (for confirming an admin pairing).
    pub id: String,
    /// The `resonator:pair/...` link (for a QR code).
    pub link: String,
    /// The short code to type on the new device (`482913-ocean-tiger`);
    /// none if the pairing rendezvous is off or cannot be reached.
    pub code: Option<String>,
    /// When the invitation expires (milliseconds since the epoch).
    pub expires_at_ms: i64,
    /// The new device gets the person key too.
    pub admin: bool,
}

impl std::fmt::Debug for PairInvite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairInvite")
            .field("id", &self.id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("admin", &self.admin)
            .finish_non_exhaustive()
    }
}

/// What [`ResonatorNode::enter_background`] did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BackgroundResult {
    /// The device the host lease was handed to.
    pub handed_over_to: Option<String>,
    /// That device took the lease within the budget.
    pub handover_complete: bool,
    pub pending_sent: u64,
    /// Edits still queued for their host (sent at the next foreground).
    pub pending_left: u64,
}

impl From<BackgroundReport> for BackgroundResult {
    fn from(report: BackgroundReport) -> Self {
        BackgroundResult {
            handed_over_to: report.handed_over_to,
            handover_complete: report.handover_complete,
            pending_sent: report.pending_sent as u64,
            pending_left: report.pending_left as u64,
        }
    }
}

/// What [`ResonatorNode::enter_foreground`] did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ForegroundResult {
    pub devices_synced: u64,
    pub wavelets_updated: u64,
    pub pending_sent: u64,
    pub pending_left: u64,
    pub lease: Option<LeaseInfo>,
}

impl From<ForegroundReport> for ForegroundResult {
    fn from(report: ForegroundReport) -> Self {
        ForegroundResult {
            devices_synced: report.devices_synced as u64,
            wavelets_updated: report.wavelets_updated as u64,
            pending_sent: report.pending_sent as u64,
            pending_left: report.pending_left as u64,
            lease: report.lease.map(LeaseInfo::from),
        }
    }
}

/// The TOML overrides of `config` (strings quoted by the TOML writer).
fn overrides(config: &NodeConfig) -> Result<String, ResonatorError> {
    let invalid = |message: String| ResonatorError::Config { detail: message };
    let mut table: toml::Table = match &config.extra_toml {
        Some(text) => toml::from_str(text).map_err(|e| invalid(e.to_string()))?,
        None => toml::Table::new(),
    };
    let section = |table: &mut toml::Table, name: &str| -> Result<toml::Table, ResonatorError> {
        match table.remove(name) {
            None => Ok(toml::Table::new()),
            Some(toml::Value::Table(section)) => Ok(section),
            Some(_) => Err(invalid(format!("{name} must be a table"))),
        }
    };
    let mut device = section(&mut table, "device")?;
    if let Some(account) = &config.account {
        device.insert("account".into(), account.clone().into());
    }
    if config.allow_remote {
        device.insert("allow_remote".into(), true.into());
    }
    table.insert("device".into(), device.into());
    if let Some(name) = &config.device_name {
        let mut person = section(&mut table, "person")?;
        person.insert("device_name".into(), name.clone().into());
        table.insert("person".into(), person.into());
    }
    toml::to_string(&table).map_err(|e| invalid(e.to_string()))
}

/// A running node.
#[derive(Debug, uniffi::Object)]
pub struct ResonatorNode {
    handle: Handle,
}

#[uniffi::export]
impl ResonatorNode {
    /// Starts a node in device mode; blocks until it serves (on
    /// `127.0.0.1`, a free port).
    #[uniffi::constructor]
    pub fn start(config: NodeConfig) -> Result<Arc<ResonatorNode>, ResonatorError> {
        let overrides = overrides(&config)?;
        let config = resonator_node::device_config(Path::new(&config.data_dir), &overrides)?;
        let handle = resonator_node::start(config)?;
        Ok(Arc::new(ResonatorNode { handle }))
    }

    /// `http://127.0.0.1:<port>`.
    pub fn local_url(&self) -> String {
        self.handle.local_url().to_owned()
    }

    /// The loopback sign-in token, new at every start. Never log it.
    pub fn token(&self) -> Option<String> {
        self.handle.token().map(str::to_owned)
    }

    /// `<local_url>/?token=<token>`: load it in the WebView; the node
    /// answers with the session cookie and the client. Never log it.
    pub fn signed_in_url(&self) -> Option<String> {
        self.handle.signed_in_url()
    }

    /// The host lease of the device's person.
    pub fn lease_state(&self) -> Option<LeaseInfo> {
        self.handle.lease_state().map(LeaseInfo::from)
    }

    /// The app goes to the background: hands the host lease to another
    /// device of the person (an anchor first) if this one holds it, sends
    /// queued edits and flushes, within `budget_ms`.
    pub fn enter_background(&self, budget_ms: u64) -> Result<BackgroundResult, ResonatorError> {
        Ok(self
            .handle
            .enter_background(Duration::from_millis(budget_ms))?
            .into())
    }

    /// The app is back: syncs with the person's other devices, sends
    /// queued edits and looks at the lease again, within `budget_ms`.
    pub fn enter_foreground(&self, budget_ms: u64) -> Result<ForegroundResult, ResonatorError> {
        Ok(self
            .handle
            .enter_foreground(Duration::from_millis(budget_ms))?
            .into())
    }

    /// Pairs this (fresh) device by a pairing code (`482913-ocean-tiger`;
    /// case and hyphens or spaces do not matter) or a `resonator:pair/...`
    /// link made on an admin device of a person (its settings, or
    /// `pair_invite`), then restarts the node as a device of that person,
    /// at the same URL and token. For an admin pairing it waits up to
    /// `wait_ms` for the inviting device to confirm.
    pub fn pair_join(
        &self,
        code_or_link: String,
        device_name: Option<String>,
        wait_ms: u64,
    ) -> Result<PairJoinResult, ResonatorError> {
        let summary = self.handle.pair_join(
            &code_or_link,
            device_name.as_deref().unwrap_or_default(),
            Duration::from_millis(wait_ms),
        )?;
        Ok(PairJoinResult {
            person: summary.person,
            admin: summary.admin,
            inviter: summary.inviter,
        })
    }

    /// On an admin device: a one-time pairing invitation (valid for 10
    /// minutes) for a new device: a short code to type and a link (for a
    /// QR code); `admin` gives it the person key too, after confirmation
    /// in the settings here. Show them only to the person; never log
    /// them.
    pub fn pair_invite(&self, admin: bool) -> Result<PairInvite, ResonatorError> {
        let invitation = self.handle.pair_invite(admin)?;
        Ok(PairInvite {
            id: invitation.pending.id,
            link: invitation.link,
            code: invitation.code,
            expires_at_ms: invitation.pending.expires_at,
            admin: invitation.pending.admin,
        })
    }

    /// The push token the platform gave this device (iOS
    /// `didRegisterForRemoteNotificationsWithDeviceToken`, as hex; Android
    /// FCM `onNewToken`). Call it after `start` and whenever the platform
    /// hands out a new token; the node keeps it and offers it to the nodes
    /// that may need to wake this device. Never log the token.
    pub fn set_push_token(
        &self,
        platform: PushPlatform,
        token: String,
    ) -> Result<(), ResonatorError> {
        let token = resonator_node::push::PushToken::new(platform.into(), &token)
            .map_err(|message| ResonatorError::Config { detail: message })?;
        Ok(self.handle.set_push_token(Some(&token))?)
    }

    /// Forgets this device's push token (notifications turned off).
    pub fn clear_push_token(&self) -> Result<(), ResonatorError> {
        Ok(self.handle.set_push_token(None)?)
    }

    /// Whether the node runs.
    pub fn is_running(&self) -> bool {
        self.handle.is_running()
    }

    /// Stops the node; the database is flushed and closed. Later calls do
    /// nothing; other calls then fail with `Stopped`.
    pub fn stop(&self) {
        self.handle.stop();
    }
}

/// The link that adds `address` as a contact (`resonator:contact/…`),
/// for sharing (a QR code, a message).
#[uniffi::export]
pub fn contact_link(address: String) -> String {
    format!(
        "{}{}",
        resonator_server::http::CONTACT_LINK_PREFIX,
        address.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_starts_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let node = ResonatorNode::start(NodeConfig {
            data_dir: dir.path().display().to_string(),
            account: Some("ada".into()),
            device_name: Some("phone".into()),
            allow_remote: false,
            extra_toml: Some(
                "[federation]\nrelay = \"off\"\ndiscovery = \"off\"\nlisten = \"127.0.0.1:0\"\n"
                    .into(),
            ),
        })
        .unwrap();
        assert!(node.local_url().starts_with("http://127.0.0.1:"));
        let url = node.signed_in_url().unwrap();
        assert!(url.ends_with(&node.token().unwrap()));
        assert!(node.lease_state().is_some());
        let background = node.enter_background(500).unwrap();
        assert_eq!(background.pending_left, 0);
        node.stop();
        assert!(!node.is_running());
        assert!(matches!(
            node.enter_foreground(500),
            Err(ResonatorError::Stopped)
        ));
    }

    #[test]
    fn bad_settings_are_a_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = ResonatorNode::start(NodeConfig {
            data_dir: dir.path().display().to_string(),
            account: Some("not valid!".into()),
            device_name: None,
            allow_remote: false,
            extra_toml: None,
        })
        .unwrap_err();
        assert!(matches!(error, ResonatorError::Config { .. }), "{error}");
    }

    #[test]
    fn contact_links() {
        assert_eq!(
            contact_link(" ada@example.com ".into()),
            "resonator:contact/ada@example.com"
        );
    }
}
