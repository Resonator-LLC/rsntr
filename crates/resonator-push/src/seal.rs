//! Sealing: a device's push token, encrypted under the relay's key into
//! an opaque blob that the device hands to other nodes instead of the
//! token. Only the relay can open it.
//!
//! The blob is base64url (no padding) of `0x01 ‖ nonce (12 bytes) ‖
//! AES-256-GCM(key, nonce, aad = "resonator/sealed/1", plaintext)`, the
//! plaintext being `{"p": platform, "t": token, "i": issued_at}` (JSON).

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

const VERSION: u8 = 1;
const AAD: &[u8] = b"resonator/sealed/1";
/// Longest blob accepted (an FCM token is at most 4 KiB).
pub const MAX_SEALED_LEN: usize = 8 * 1024;

/// The push service of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Apns,
    ApnsSandbox,
    Fcm,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Apns => "apns",
            Platform::ApnsSandbox => "apns-sandbox",
            Platform::Fcm => "fcm",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "apns" => Some(Platform::Apns),
            "apns-sandbox" => Some(Platform::ApnsSandbox),
            "fcm" => Some(Platform::Fcm),
            _ => None,
        }
    }
}

/// Checks a token's form (as `resonator_node::push::PushToken` does) and
/// normalises it: APNs tokens are hex (lower-cased), FCM tokens up to
/// 4 KiB of `[A-Za-z0-9:_-]`.
pub fn check_token(platform: Platform, token: &str) -> Option<String> {
    let token = token.trim();
    let valid = match platform {
        Platform::Apns | Platform::ApnsSandbox => {
            token.len() >= 16
                && token.len() <= 200
                && token.len().is_multiple_of(2)
                && token.bytes().all(|b| b.is_ascii_hexdigit())
        }
        Platform::Fcm => {
            !token.is_empty()
                && token.len() <= 4096
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
        }
    };
    valid.then(|| match platform {
        Platform::Fcm => token.to_owned(),
        _ => token.to_ascii_lowercase(),
    })
}

/// What a blob holds. `Debug` hides the token.
#[derive(Clone, PartialEq, Eq)]
pub struct Sealed {
    pub platform: Platform,
    pub token: String,
    /// Unix seconds.
    pub issued_at: i64,
}

impl std::fmt::Debug for Sealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealed")
            .field("platform", &self.platform)
            .field("issued_at", &self.issued_at)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct Plain {
    p: String,
    t: String,
    i: i64,
}

/// A blob that does not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a sealed token of this relay")]
pub struct BadBlob;

/// The relay's sealing key. `Debug` hides it.
pub struct SealingKey {
    key: LessSafeKey,
    rng: SystemRandom,
}

impl std::fmt::Debug for SealingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealingKey(<redacted>)")
    }
}

impl SealingKey {
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        let key = UnboundKey::new(&AES_256_GCM, bytes).expect("a 32-byte AES key");
        SealingKey {
            key: LessSafeKey::new(key),
            rng: SystemRandom::new(),
        }
    }

    /// Reads the key kept in `path` (64 hex characters), or makes one
    /// and keeps it there (mode 600, never overwriting a file).
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let bytes = decode_hex(text.trim()).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{} does not hold a sealing key", path.display()),
                    )
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(path)?.permissions().mode();
                    if mode & 0o077 != 0 {
                        tracing::warn!(path = %path.display(), "the sealing key can be read by others; chmod 600 it");
                    }
                }
                Ok(Self::from_bytes(&bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut bytes = [0u8; 32];
                SystemRandom::new()
                    .fill(&mut bytes)
                    .map_err(|_| std::io::Error::other("no randomness"))?;
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                {
                    use std::io::Write;
                    let mut file = options.open(path)?;
                    file.write_all(format!("{}\n", encode_hex(&bytes)).as_bytes())?;
                    file.sync_all()?;
                }
                tracing::info!(path = %path.display(), "made a new sealing key");
                Ok(Self::from_bytes(&bytes))
            }
            Err(error) => Err(error),
        }
    }

    /// Seals a token.
    pub fn seal(&self, sealed: &Sealed) -> String {
        let plain = serde_json::to_vec(&Plain {
            p: sealed.platform.as_str().to_owned(),
            t: sealed.token.clone(),
            i: sealed.issued_at,
        })
        .expect("JSON of strings and a number");
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce).expect("randomness");
        let mut in_out = plain;
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(AAD),
                &mut in_out,
            )
            .expect("sealing");
        let mut blob = Vec::with_capacity(1 + NONCE_LEN + in_out.len());
        blob.push(VERSION);
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&in_out);
        URL_SAFE_NO_PAD.encode(blob)
    }

    /// Opens a blob of this key.
    pub fn unseal(&self, blob: &str) -> Result<Sealed, BadBlob> {
        if blob.len() > MAX_SEALED_LEN {
            return Err(BadBlob);
        }
        let bytes = URL_SAFE_NO_PAD.decode(blob).map_err(|_| BadBlob)?;
        if bytes.len() < 1 + NONCE_LEN + AES_256_GCM.tag_len() || bytes[0] != VERSION {
            return Err(BadBlob);
        }
        let nonce: [u8; NONCE_LEN] = bytes[1..1 + NONCE_LEN].try_into().map_err(|_| BadBlob)?;
        let mut in_out = bytes[1 + NONCE_LEN..].to_vec();
        let plain = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(AAD),
                &mut in_out,
            )
            .map_err(|_| BadBlob)?;
        let plain: Plain = serde_json::from_slice(plain).map_err(|_| BadBlob)?;
        let platform = Platform::parse(&plain.p).ok_or(BadBlob)?;
        let token = check_token(platform, &plain.t).ok_or(BadBlob)?;
        Ok(Sealed {
            platform,
            token,
            issued_at: plain.i,
        })
    }
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> Sealed {
        Sealed {
            platform: Platform::ApnsSandbox,
            token: "ab".repeat(32),
            issued_at: 1_700_000_000,
        }
    }

    #[test]
    fn a_sealed_token_opens_and_a_tampered_one_does_not() {
        let key = SealingKey::from_bytes(&[7; 32]);
        let blob = key.seal(&token());
        assert!(!blob.contains(&"ab".repeat(4)));
        assert_eq!(key.unseal(&blob), Ok(token()));
        // Random nonces: the same token seals differently.
        assert_ne!(key.seal(&token()), blob);
        // Any changed byte is refused.
        let bytes = URL_SAFE_NO_PAD.decode(&blob).unwrap();
        for i in 0..bytes.len() {
            let mut tampered = bytes.clone();
            tampered[i] ^= 0x01;
            assert_eq!(key.unseal(&URL_SAFE_NO_PAD.encode(&tampered)), Err(BadBlob));
        }
        assert_eq!(key.unseal(&blob[..blob.len() - 2]), Err(BadBlob));
        assert_eq!(key.unseal("not base64!"), Err(BadBlob));
        assert_eq!(key.unseal(""), Err(BadBlob));
        // Another relay's key.
        assert_eq!(SealingKey::from_bytes(&[8; 32]).unseal(&blob), Err(BadBlob));
        assert!(!format!("{:?}", token()).contains("abab"));
    }

    #[test]
    fn the_key_is_made_once_with_mode_600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/sealing.key");
        let first = SealingKey::load_or_create(&path).unwrap();
        let blob = first.seal(&token());
        let again = SealingKey::load_or_create(&path).unwrap();
        assert_eq!(again.unseal(&blob), Ok(token()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(&path, "short").unwrap();
        assert!(SealingKey::load_or_create(&path).is_err());
        assert!(!format!("{first:?}").contains('7'));
    }

    #[test]
    fn tokens_are_checked() {
        assert_eq!(
            check_token(Platform::Apns, &"AB".repeat(32)),
            Some("ab".repeat(32))
        );
        assert_eq!(check_token(Platform::Apns, "abc"), None);
        assert_eq!(check_token(Platform::Fcm, "a b"), None);
        assert_eq!(
            check_token(Platform::Fcm, "x:Y_1-z"),
            Some("x:Y_1-z".to_owned())
        );
    }
}
