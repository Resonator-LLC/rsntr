//! The push token of this device: what the app got from the platform's
//! push service (APNs, FCM), so that other nodes can have a sleeping
//! device woken (through the push relay, `docs/push-relay.md`).
//!
//! The token is kept in `<data dir>/push-token` (mode 600), one line:
//! `<platform> <token>`. It is never logged.

use std::path::{Path, PathBuf};

/// The push service a token belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPlatform {
    /// Apple Push Notification service, production.
    Apns,
    /// APNs sandbox (development builds).
    ApnsSandbox,
    /// Firebase Cloud Messaging.
    Fcm,
}

impl PushPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            PushPlatform::Apns => "apns",
            PushPlatform::ApnsSandbox => "apns-sandbox",
            PushPlatform::Fcm => "fcm",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "apns" => Some(PushPlatform::Apns),
            "apns-sandbox" => Some(PushPlatform::ApnsSandbox),
            "fcm" => Some(PushPlatform::Fcm),
            _ => None,
        }
    }
}

/// A device's push token.
#[derive(Clone, PartialEq, Eq)]
pub struct PushToken {
    pub platform: PushPlatform,
    pub token: String,
}

impl std::fmt::Debug for PushToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token.
        f.debug_struct("PushToken")
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

/// The token as federation takes it.
pub(crate) fn raw(token: PushToken) -> resonator_federation::push::RawPushToken {
    resonator_federation::push::RawPushToken {
        platform: token.platform.as_str().to_owned(),
        token: token.token,
    }
}

impl PushToken {
    /// Checks the form of a token: APNs tokens are hex (32 bytes today,
    /// up to 100 allowed), FCM tokens up to 4 KiB of `[A-Za-z0-9:_-]`.
    pub fn new(platform: PushPlatform, token: &str) -> Result<Self, String> {
        let token = token.trim();
        let valid = match platform {
            PushPlatform::Apns | PushPlatform::ApnsSandbox => {
                token.len() >= 16
                    && token.len() <= 200
                    && token.len().is_multiple_of(2)
                    && token.bytes().all(|b| b.is_ascii_hexdigit())
            }
            PushPlatform::Fcm => {
                !token.is_empty()
                    && token.len() <= 4096
                    && token
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
            }
        };
        if !valid {
            return Err(format!("not a {} push token", platform.as_str()));
        }
        let token = match platform {
            PushPlatform::Fcm => token.to_owned(),
            _ => token.to_ascii_lowercase(),
        };
        Ok(PushToken { platform, token })
    }
}

fn path_in(dir: &Path) -> PathBuf {
    dir.join("push-token")
}

/// The token kept in `dir`, if any (a malformed file counts as none).
pub fn load(dir: &Path) -> Option<PushToken> {
    let text = std::fs::read_to_string(path_in(dir)).ok()?;
    let (platform, token) = text.trim().split_once(' ')?;
    PushToken::new(PushPlatform::parse(platform)?, token).ok()
}

/// Keeps `token` in `dir`, or removes the kept one (`None`).
pub fn store(dir: &Path, token: Option<&PushToken>) -> std::io::Result<()> {
    let path = path_in(dir);
    let Some(token) = token else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    };
    let line = format!("{} {}\n", token.platform.as_str(), token.token);
    let temp = dir.join("push-token.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_checked_and_kept() {
        let dir = std::env::temp_dir().join(format!("wn-push-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(&dir), None);
        let apns = PushToken::new(PushPlatform::Apns, &"AB".repeat(32)).unwrap();
        assert_eq!(apns.token, "ab".repeat(32));
        store(&dir, Some(&apns)).unwrap();
        assert_eq!(load(&dir), Some(apns.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("push-token"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store(&dir, None).unwrap();
        store(&dir, None).unwrap();
        assert_eq!(load(&dir), None);
        assert!(PushToken::new(PushPlatform::Apns, "xyz").is_err());
        assert!(PushToken::new(PushPlatform::Fcm, "a b").is_err());
        assert!(PushToken::new(PushPlatform::Fcm, "abc:DEF_12-x").is_ok());
        assert!(!format!("{apns:?}").contains("abab"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
