//! The app manifest (`manifest.json` in a bundle).

use serde::Deserialize;

use crate::AppError;

/// Memory an app gets when its manifest asks for none.
pub const DEFAULT_MEMORY_MB: u32 = 64;
/// The most memory a manifest may ask for.
pub const MAX_MEMORY_MB: u32 = 256;

/// What an app may use. A call into an interface the app was not granted
/// returns `Err("permission denied")` (or nothing, for reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Read and post messages in the app wavelet.
    Wave,
    /// The node-local key/value store.
    Kv,
    /// See peers and send them messages.
    Peers,
    /// Open streams to peers.
    Streams,
    /// The viewer's microphone and speaker.
    Media,
    /// The viewer's camera, and video shown to the viewer.
    Camera,
    /// Video shown to the viewer, without their camera.
    Video,
}

impl Permission {
    /// The name in `manifest.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Wave => "wave",
            Permission::Kv => "kv",
            Permission::Peers => "peers",
            Permission::Streams => "streams",
            Permission::Media => "media",
            Permission::Camera => "camera",
            Permission::Video => "video",
        }
    }
}

/// The parsed `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub memory_mb: Option<u32>,
}

impl Manifest {
    /// Parses and checks a manifest.
    pub fn parse(bytes: &[u8]) -> Result<Self, AppError> {
        let manifest: Manifest = serde_json::from_slice(bytes)
            .map_err(|e| AppError::Bundle(format!("manifest.json: {e}")))?;
        manifest.check()?;
        Ok(manifest)
    }

    fn check(&self) -> Result<(), AppError> {
        let bad = |what: &str| Err(AppError::Bundle(format!("manifest.json: {what}")));
        if self.name.trim().is_empty() || self.name.chars().count() > 64 {
            return bad("name must be 1 to 64 characters");
        }
        if self.version.is_empty() || self.version.len() > 32 {
            return bad("version must be 1 to 32 characters");
        }
        if self
            .description
            .as_ref()
            .is_some_and(|d| d.chars().count() > 500)
        {
            return bad("description is longer than 500 characters");
        }
        if let Some(mb) = self.memory_mb
            && !(1..=MAX_MEMORY_MB).contains(&mb)
        {
            return bad(&format!("memory_mb must be 1 to {MAX_MEMORY_MB}"));
        }
        Ok(())
    }

    /// The memory limit in MiB.
    pub fn memory_mb(&self) -> u32 {
        self.memory_mb.unwrap_or(DEFAULT_MEMORY_MB)
    }

    pub fn grants(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_manifest() {
        let m = Manifest::parse(
            br#"{"name":"walkie","version":"1.0","permissions":["peers","streams","media"],"memory_mb":32}"#,
        )
        .unwrap();
        assert_eq!(m.memory_mb(), 32);
        assert!(m.grants(Permission::Streams));
        assert!(!m.grants(Permission::Kv));
    }

    #[test]
    fn refuses_unknown_fields_and_limits() {
        assert!(Manifest::parse(br#"{"name":"a","version":"1","net":true}"#).is_err());
        assert!(Manifest::parse(br#"{"name":"","version":"1"}"#).is_err());
        assert!(Manifest::parse(br#"{"name":"a","version":"1","memory_mb":1024}"#).is_err());
        assert!(Manifest::parse(br#"{"name":"a","version":"1","permissions":["files"]}"#).is_err());
        assert_eq!(
            Manifest::parse(br#"{"name":"a","version":"1"}"#)
                .unwrap()
                .memory_mb(),
            DEFAULT_MEMORY_MB
        );
    }
}
