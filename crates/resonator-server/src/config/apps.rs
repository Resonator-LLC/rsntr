//! The `[apps]` section: wave apps (gadgets that run a wasm component on
//! this node; see the `resonator-apps` crate). Off by default; device
//! nodes turn it on.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppsConfig {
    /// Whether wave apps run here. Off: gadgets with an app show as
    /// inert, as before.
    pub enabled: bool,
    /// Most app instances running at once on this node.
    pub max_instances: usize,
    /// Seconds an instance keeps running after its last viewer left.
    pub idle_secs: u64,
}

impl Default for AppsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_instances: 64,
            idle_secs: 300,
        }
    }
}

impl AppsConfig {
    pub fn is_default(&self) -> bool {
        *self == AppsConfig::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_instances == 0 {
            return Err("apps.max_instances must be at least 1".to_owned());
        }
        Ok(())
    }
}
