//! The `[webrtc]` section: wave app media between a browser and this node
//! over WebRTC (one UDP port), instead of the client socket. Off by
//! default; a browser that cannot reach the port uses the socket.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebRtcConfig {
    /// Whether app media may go over WebRTC.
    pub enabled: bool,
    /// The UDP address the node listens on.
    pub listen: SocketAddr,
    /// The address browsers send to: this host's public (or tailnet)
    /// address and the port, as they reach it.
    pub advertise: Option<SocketAddr>,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: SocketAddr::from(([0, 0, 0, 0], 7779)),
            advertise: None,
        }
    }
}

impl WebRtcConfig {
    pub fn is_default(&self) -> bool {
        *self == WebRtcConfig::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        match self.advertise {
            None => Err("webrtc.advertise is needed when webrtc is enabled".to_owned()),
            Some(addr) if addr.ip().is_unspecified() || addr.port() == 0 => {
                Err("webrtc.advertise must be an address browsers can reach".to_owned())
            }
            Some(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_needs_a_reachable_address() {
        let mut config = WebRtcConfig {
            enabled: true,
            ..WebRtcConfig::default()
        };
        assert!(config.validate().is_err());
        config.advertise = Some("0.0.0.0:7779".parse().unwrap());
        assert!(config.validate().is_err());
        config.advertise = Some("100.127.138.26:7779".parse().unwrap());
        assert_eq!(config.validate(), Ok(()));
        assert_eq!(WebRtcConfig::default().validate(), Ok(()));
    }

    #[test]
    fn parses_from_toml() {
        let config: WebRtcConfig = toml::from_str(
            "enabled = true\nlisten = \"0.0.0.0:7780\"\nadvertise = \"104.248.47.120:7780\"\n",
        )
        .unwrap();
        assert!(config.enabled);
        assert_eq!(config.listen.port(), 7780);
        assert_eq!(
            config.advertise,
            Some("104.248.47.120:7780".parse().unwrap())
        );
    }
}
