//! Device mode (`mode = "device"`) and its `[device]` section: the node
//! is one phone or computer of a person, used by that person alone,
//! usually from a WebView of an app on the same device.
//!
//! In device mode:
//! - there is exactly one account, `<device.account>@<domain>` (the domain
//!   is the person domain in person mode), created at start without a
//!   password;
//! - registration and the server directory are off; contacts replace the
//!   directory (`/contacts`);
//! - a random token is made at every start. `GET /?token=<token>` from a
//!   loopback peer signs that account in with an ordinary session cookie;
//! - requests from anywhere but loopback are refused unless
//!   `allow_remote` is set (an anchor or a desktop reached from other
//!   machines), where the usual sign-in applies;
//! - the origins in `origins` (the app's WebView origins) may make
//!   state-changing requests and open the WebSocket.

use serde::{Deserialize, Serialize};

use crate::auth::registration;

/// Whether the node is a multi-user server or one device of a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Accounts registered by many people on one domain (the default).
    #[default]
    Server,
    /// One person's device: one account, signed in by the loopback token.
    Device,
}

impl Mode {
    pub fn is_server(&self) -> bool {
        *self == Mode::Server
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "server" => Ok(Mode::Server),
            "device" => Ok(Mode::Device),
            _ => Err(format!("invalid mode '{s}' (expected server or device)")),
        }
    }
}

/// The `[device]` settings; used only with `mode = "device"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceConfig {
    /// The name part of the one account; its domain is the node's
    /// (person) domain. The same on every device of the person.
    pub account: String,
    /// Whether requests from other machines are served (with the usual
    /// sign-in; the token works from loopback only). Off: only loopback
    /// peers are served.
    pub allow_remote: bool,
    /// Origins (`scheme://host[:port]`) of the app wrapping the node, such
    /// as `resonator://app` (iOS `WKURLSchemeHandler`) or
    /// `https://appassets.androidplatform.net` (Android
    /// `WebViewAssetLoader`), allowed to make state-changing requests and
    /// open the WebSocket like the node's own pages.
    pub origins: Vec<String>,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        DeviceConfig {
            account: "me".to_owned(),
            allow_remote: false,
            origins: vec![
                "resonator://app".to_owned(),
                "https://appassets.androidplatform.net".to_owned(),
            ],
        }
    }
}

impl DeviceConfig {
    pub fn is_default(&self) -> bool {
        *self == DeviceConfig::default()
    }

    pub fn validate(&self, domain: &str) -> Result<(), String> {
        if registration::check_new_username(domain, Some(&self.account)).is_err()
            || self.account.contains('@')
        {
            return Err(format!(
                "device.account: '{}' is not a valid account name",
                self.account
            ));
        }
        for origin in &self.origins {
            let valid = origin.split_once("://").is_some_and(|(scheme, authority)| {
                !scheme.is_empty()
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                    && !authority.is_empty()
                    && !authority.contains('/')
            });
            if !valid {
                return Err(format!(
                    "device.origins: '{origin}' is not of the form scheme://host[:port]"
                ));
            }
        }
        Ok(())
    }
}
