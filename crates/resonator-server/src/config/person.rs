//! The `[person]` section of the configuration: whether this node acts as
//! one device of a *person* (a federation domain whose authority is a
//! person key) instead of a server with its own domain. What it means is
//! implemented by the `resonator-federation` crate (`person` module).

use resonator_model::id::is_valid_domain;
use serde::{Deserialize, Serialize};

/// The `[person]` settings. Person mode needs `[federation] enabled`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PersonConfig {
    /// Whether this node is a device of a person. Off by default.
    pub enabled: bool,
    /// The file holding the person key on admin devices; empty for
    /// `person.key` in the data directory.
    pub key_path: String,
    /// A name for this device in its certificate.
    pub device_name: String,
    /// Whether this device holds the person key (`key_path`) and may
    /// certify and revoke devices. An admin device certifies itself.
    pub admin: bool,
    /// The person domain: empty for the person key's 52-character
    /// z-base-32 form, or a DNS domain whose `_resonator` TXT record lists
    /// the person key (`k=<person key>`).
    pub domain: String,
    /// Deprecated since host leases: the device key (52-character
    /// z-base-32) of the device to send submissions to before any host
    /// lease of the person is known. Which device hosts the person's
    /// wavelets is decided by the host lease.
    pub holder: String,
    /// Whether this device is an anchor: an always-on device preferred as
    /// the holder of the person's host lease. An anchor claims an expired
    /// lease at once and other holders hand the lease over to it.
    pub anchor: bool,
    /// How often an anchor holding the lease renews it, in seconds (0:
    /// 600). A lease lasts three renewal intervals.
    pub lease_renew_anchor_secs: u64,
    /// How often another device holding the lease renews it, in seconds
    /// (0: 120).
    pub lease_renew_secs: u64,
    /// This device's certificate, as printed by `resonator person cert`
    /// (hex); empty to read `device.cert` in the data directory.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cert: String,
}

impl PersonConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.domain.is_empty() && !is_valid_domain(0, &self.domain) {
            return Err(format!(
                "person.domain: '{}' is not a valid domain",
                self.domain
            ));
        }
        if !self.holder.is_empty() && !is_valid_domain(0, &self.holder) {
            return Err(format!(
                "person.holder: '{}' is not a device key",
                self.holder
            ));
        }
        if !self.cert.is_empty() && !self.cert.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("person.cert is not hex".to_owned());
        }
        Ok(())
    }
}
