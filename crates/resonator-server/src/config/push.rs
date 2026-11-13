//! The `[push]` section: waking sleeping devices through a push relay
//! (`resonator-push`, see `docs/push-relay.md`). Off while `relay` is
//! empty: nothing is sealed, advertised, learned or woken.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PushConfig {
    /// The base URL of the push relay this device seals its push token
    /// with, and through which it wakes other devices
    /// (`http://mothership:9900`, or an `https://` URL).
    pub relay: String,
    /// Other relays this node may call to wake devices that sealed their
    /// token there. Devices whose relay is neither `relay` nor listed
    /// here are not woken (a node only calls relays it was told to).
    pub other_relays: Vec<String>,
}

impl PushConfig {
    pub fn is_default(&self) -> bool {
        *self == PushConfig::default()
    }

    /// Whether push is on.
    pub fn enabled(&self) -> bool {
        !self.relay.is_empty()
    }

    /// Whether this node may call `relay` to wake a device.
    pub fn may_call(&self, relay: &str) -> bool {
        let relay = relay.trim_end_matches('/');
        self.enabled()
            && std::iter::once(&self.relay)
                .chain(&self.other_relays)
                .any(|known| known.trim_end_matches('/') == relay)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        for url in std::iter::once(&self.relay)
            .filter(|relay| !relay.is_empty())
            .chain(&self.other_relays)
        {
            let rest = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"));
            if !matches!(rest, Some(rest) if !rest.is_empty() && !rest.contains(['?', '#', ' '])) {
                return Err(format!("push relay '{url}' is not an http(s) URL"));
            }
        }
        if self.relay.is_empty() && !self.other_relays.is_empty() {
            return Err("push.other_relays needs push.relay".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relays_are_urls_and_only_known_ones_are_called() {
        let mut push = PushConfig::default();
        assert!(!push.enabled() && push.validate().is_ok() && !push.may_call(""));
        push.relay = "http://mothership:9900/".to_owned();
        push.other_relays = vec!["https://push.example".to_owned()];
        assert!(push.validate().is_ok());
        assert!(push.may_call("http://mothership:9900"));
        assert!(push.may_call("https://push.example/"));
        assert!(!push.may_call("http://evil.example"));
        push.relay = "mothership:9900".to_owned();
        assert!(push.validate().is_err());
        push.relay.clear();
        assert!(push.validate().is_err());
    }
}
