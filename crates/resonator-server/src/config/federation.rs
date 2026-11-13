//! The `[federation]` section of the configuration: whether this server
//! talks to other wave servers, which ones, and how it reaches them. The
//! settings are declared here so that there is one `Config`; what they
//! mean is implemented by the `resonator-federation` crate.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use resonator_model::id::is_valid_domain;
use serde::{Deserialize, Serialize};

/// Which remote servers may federate with this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FederationMode {
    /// Only the domains (or key domains) listed in `allow`.
    Allowlist,
    /// Any server (the default): one whose key is bound to its domain,
    /// and, for a domain that publishes no key record, the first key that
    /// claims it.
    #[default]
    Open,
}

impl std::str::FromStr for FederationMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "allowlist" => Ok(FederationMode::Allowlist),
            "open" => Ok(FederationMode::Open),
            _ => Err(format!(
                "invalid federation mode '{s}' (expected allowlist or open)"
            )),
        }
    }
}

/// Which relay servers the federation endpoint uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaySetting {
    /// The public relays of the iroh project.
    Default,
    /// No relay: only direct connections.
    Disabled,
    /// A relay of one's own, by URL.
    Custom(String),
}

/// A remote server reachable at a fixed address (`[[federation.peers]]`),
/// for setups without discovery, such as two servers on one machine.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FederationPeer {
    /// The peer's wave domain. May be left out if it is the key domain of
    /// `key`.
    pub domain: String,
    /// The peer's server key in its 52-character z-base-32 form. May be
    /// left out if `domain` is a key domain. A DNS domain listed with a
    /// key is bound to that key as if pinned by an admin.
    pub key: String,
    /// The peer's federation endpoint: a UDP `host:port`.
    pub addr: String,
}

/// The `[federation]` settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FederationConfig {
    /// Whether this server federates. Off by default; when off, nothing of
    /// federation runs and no server key is created.
    pub enabled: bool,
    /// `open` (the default) or `allowlist`.
    pub mode: FederationMode,
    /// Domains, or 52-character key domains, that may federate in
    /// `allowlist` mode.
    pub allow: Vec<String>,
    /// `default` for the public iroh relays, `off` for none, or the URL of
    /// a relay of one's own.
    pub relay: String,
    /// `default` to publish this server's addresses and look up others' by
    /// key through iroh's DNS address lookup, `off` for static addresses
    /// only.
    pub discovery: String,
    /// UDP address (`host:port`) or port the federation endpoint binds;
    /// empty for any port on all interfaces.
    pub listen: String,
    /// Seconds to wait for the answer to a federation request.
    pub request_timeout_secs: u64,
    /// Megabytes of attachments of other servers kept here (fetched for
    /// local users), least recently used dropped first. Not counted against
    /// any user's quota: the origin keeps the master copy.
    pub attachment_cache_mb: u64,
    /// Person mode: copy attachments uploaded on one device of the person
    /// to its other devices right away (and to devices that were away
    /// when they meet again), so that they can be viewed while the device
    /// they were uploaded to sleeps. The copies count against
    /// `attachment_cache_mb`; a device whose cache has no room left skips
    /// them. Without person mode this has no effect.
    pub replicate_own_attachments: bool,
    /// Person mode: the largest attachment copied ahead in megabytes;
    /// larger ones are fetched when opened.
    pub replicate_max_mb: u64,
    /// Person mode, pairing by code: the pkarr relay where an inviting
    /// device publishes the rendezvous record of a code and a new device
    /// looks it up. `default` is n0's relay (`https://dns.iroh.link/pkarr`)
    /// when `discovery` is on and none when it is off; `off` for none (no
    /// codes, links only); or the http(s) URL of a pkarr relay.
    pub pairing_rendezvous: String,
    /// The device directory, on the pkarr relay of `pairing_rendezvous`
    /// (so only when that is on, by default when `discovery` is): admin
    /// devices of a person publish the person's device keys under the
    /// person key, and servers that know no device of a person domain
    /// look them up to reach it. On by default.
    pub device_directory: bool,
    /// Peers at fixed addresses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<FederationPeer>,
}

/// n0's public pkarr relay, which iroh's address lookup uses too.
pub const DEFAULT_PAIRING_RENDEZVOUS: &str = "https://dns.iroh.link/pkarr";

impl Default for FederationConfig {
    fn default() -> Self {
        FederationConfig {
            enabled: false,
            mode: FederationMode::Open,
            allow: Vec::new(),
            relay: "default".to_owned(),
            discovery: "default".to_owned(),
            listen: String::new(),
            request_timeout_secs: 30,
            attachment_cache_mb: 1024,
            replicate_own_attachments: true,
            replicate_max_mb: 25,
            pairing_rendezvous: "default".to_owned(),
            device_directory: true,
            peers: Vec::new(),
        }
    }
}

impl FederationConfig {
    /// The relay setting `relay` names.
    pub fn relay_setting(&self) -> Result<RelaySetting, String> {
        match self.relay.trim() {
            "" | "default" => Ok(RelaySetting::Default),
            "off" | "disabled" | "none" => Ok(RelaySetting::Disabled),
            url if url.starts_with("https://") || url.starts_with("http://") => {
                Ok(RelaySetting::Custom(url.to_owned()))
            }
            other => Err(format!(
                "federation.relay '{other}' is not default, off or an http(s) URL"
            )),
        }
    }

    /// The attachment cache budget in bytes.
    pub fn attachment_cache_bytes(&self) -> u64 {
        self.attachment_cache_mb.saturating_mul(1024 * 1024)
    }

    /// The largest attachment copied ahead between a person's devices, in
    /// bytes.
    pub fn replicate_max_bytes(&self) -> u64 {
        self.replicate_max_mb.saturating_mul(1024 * 1024)
    }

    /// Whether addresses are published and looked up through DNS.
    pub fn discovery_enabled(&self) -> Result<bool, String> {
        match self.discovery.trim() {
            "" | "default" => Ok(true),
            "off" | "disabled" | "none" => Ok(false),
            other => Err(format!(
                "federation.discovery '{other}' is not default or off"
            )),
        }
    }

    /// The pkarr relay URL of pairing by code, or `None` if pairing by
    /// code is off (see `pairing_rendezvous`).
    pub fn pairing_rendezvous_url(&self) -> Result<Option<String>, String> {
        match self.pairing_rendezvous.trim() {
            "" | "default" => Ok(self
                .discovery_enabled()?
                .then(|| DEFAULT_PAIRING_RENDEZVOUS.to_owned())),
            "off" | "disabled" | "none" => Ok(None),
            url if url.starts_with("https://") || url.starts_with("http://") => {
                Ok(Some(url.trim_end_matches('/').to_owned()))
            }
            other => Err(format!(
                "federation.pairing_rendezvous '{other}' is not default, off or an http(s) URL"
            )),
        }
    }

    /// The UDP addresses to bind; empty for the endpoint's defaults.
    pub fn listen_addrs(&self) -> Result<Vec<SocketAddr>, String> {
        let listen = self.listen.trim();
        if listen.is_empty() {
            return Ok(Vec::new());
        }
        if let Ok(port) = listen.parse::<u16>() {
            return Ok(vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
                SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
            ]);
        }
        listen
            .parse::<SocketAddr>()
            .map(|addr| vec![addr])
            .map_err(|_| format!("federation.listen '{listen}' is not a port or host:port"))
    }

    pub fn validate(&self) -> Result<(), String> {
        self.relay_setting()?;
        self.discovery_enabled()?;
        self.pairing_rendezvous_url()?;
        self.listen_addrs()?;
        for domain in &self.allow {
            if !is_valid_domain(0, domain) {
                return Err(format!(
                    "federation.allow: '{domain}' is not a valid domain name"
                ));
            }
        }
        if self.request_timeout_secs == 0 {
            return Err("federation.request_timeout_secs must be at least 1".to_owned());
        }
        for peer in &self.peers {
            if peer.domain.is_empty() && peer.key.is_empty() {
                return Err("federation.peers: a peer needs a domain or a key".to_owned());
            }
            for name in [&peer.domain, &peer.key] {
                if !name.is_empty() && !is_valid_domain(0, name) {
                    return Err(format!(
                        "federation.peers: '{name}' is not a valid domain or key"
                    ));
                }
            }
            if peer.addr.parse::<SocketAddr>().is_err() {
                return Err(format!(
                    "federation.peers: addr '{}' is not a host:port",
                    peer.addr
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_valid() {
        let config = FederationConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.mode, FederationMode::Open);
        assert_eq!(config.relay_setting(), Ok(RelaySetting::Default));
        assert_eq!(config.discovery_enabled(), Ok(true));
        assert_eq!(config.listen_addrs(), Ok(Vec::new()));
        config.validate().unwrap();
    }

    #[test]
    fn the_pairing_rendezvous_follows_discovery_by_default() {
        let config = FederationConfig::default();
        assert_eq!(
            config.pairing_rendezvous_url(),
            Ok(Some(DEFAULT_PAIRING_RENDEZVOUS.to_owned()))
        );
        let offline = FederationConfig {
            discovery: "off".to_owned(),
            ..FederationConfig::default()
        };
        assert_eq!(offline.pairing_rendezvous_url(), Ok(None));
        let own = FederationConfig {
            discovery: "off".to_owned(),
            pairing_rendezvous: "http://127.0.0.1:9/pkarr/".to_owned(),
            ..FederationConfig::default()
        };
        assert_eq!(
            own.pairing_rendezvous_url(),
            Ok(Some("http://127.0.0.1:9/pkarr".to_owned()))
        );
        let off = FederationConfig {
            pairing_rendezvous: "off".to_owned(),
            ..FederationConfig::default()
        };
        assert_eq!(off.pairing_rendezvous_url(), Ok(None));
        let bad = FederationConfig {
            pairing_rendezvous: "dht".to_owned(),
            ..FederationConfig::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn settings_parse_and_are_checked() {
        let config = FederationConfig {
            relay: "off".to_owned(),
            discovery: "off".to_owned(),
            listen: "7777".to_owned(),
            ..FederationConfig::default()
        };
        assert_eq!(config.relay_setting(), Ok(RelaySetting::Disabled));
        assert_eq!(config.discovery_enabled(), Ok(false));
        assert_eq!(config.listen_addrs().unwrap().len(), 2);
        let config = FederationConfig {
            relay: "https://relay.example.com".to_owned(),
            listen: "100.64.0.1:7777".to_owned(),
            ..FederationConfig::default()
        };
        assert!(matches!(
            config.relay_setting(),
            Ok(RelaySetting::Custom(_))
        ));
        assert_eq!(
            config.listen_addrs().unwrap(),
            ["100.64.0.1:7777".parse().unwrap()]
        );
        for bad in [
            FederationConfig {
                relay: "sometimes".to_owned(),
                ..FederationConfig::default()
            },
            FederationConfig {
                discovery: "dht".to_owned(),
                ..FederationConfig::default()
            },
            FederationConfig {
                listen: "nowhere".to_owned(),
                ..FederationConfig::default()
            },
            FederationConfig {
                allow: vec!["Not A Domain".to_owned()],
                ..FederationConfig::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let peer = FederationPeer {
            domain: "b.example".to_owned(),
            key: String::new(),
            addr: "127.0.0.1:7001".to_owned(),
        };
        let config = FederationConfig {
            peers: vec![peer.clone()],
            ..FederationConfig::default()
        };
        config.validate().unwrap();
        for bad in [
            FederationPeer {
                addr: "nowhere".to_owned(),
                ..peer.clone()
            },
            FederationPeer {
                domain: String::new(),
                ..peer.clone()
            },
            FederationPeer {
                key: "Not A Key".to_owned(),
                ..peer
            },
        ] {
            let config = FederationConfig {
                peers: vec![bad],
                ..FederationConfig::default()
            };
            assert!(config.validate().is_err(), "{config:?}");
        }
        assert_eq!("OPEN".parse::<FederationMode>(), Ok(FederationMode::Open));
        assert!("closed".parse::<FederationMode>().is_err());
    }
}
