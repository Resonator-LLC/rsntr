//! The server key and key domains.
//!
//! Every federating server has one Ed25519 server key. It is the iroh
//! endpoint's identity (QUIC with TLS 1.3 raw public keys proves it in
//! every handshake) and it signs the deltas the server applies.
//!
//! A *key domain* is a server key written in z-base-32: 52 lower-case
//! characters, one DNS label. `user@<key domain>` addresses need no DNS:
//! the domain is the key.

use std::io::Write;
use std::path::{Path, PathBuf};

use iroh::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Length of a key domain: 32 bytes in z-base-32.
pub const KEY_DOMAIN_LEN: usize = 52;

/// The z-base-32 alphabet.
const Z32_ALPHABET: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

/// A server key that cannot be loaded or created.
#[derive(Debug, Error)]
pub enum KeyError {
    #[error("cannot read server key {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write server key {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("server key {path} is not a hex-encoded 32-byte Ed25519 secret key")]
    Invalid { path: PathBuf },
}

/// The key domain of `key`: its z-base-32 encoding.
pub fn key_domain(key: &PublicKey) -> String {
    key.to_z32()
}

/// A domain that has the shape of a key domain but is not one.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("'{0}' looks like a key domain but is not a valid Ed25519 key")]
pub struct InvalidKeyDomain(pub String);

/// Recognises a key domain: a single label of 52 z-base-32 characters
/// that decodes to exactly 32 bytes. Returns `Ok(None)` for any other
/// domain (a DNS domain), and an error for a label that has the shape of a
/// key domain but is not the canonical encoding of a valid Ed25519 key;
/// such a domain must be neither dialled nor looked up in DNS.
pub fn parse_key_domain(domain: &str) -> Result<Option<PublicKey>, InvalidKeyDomain> {
    if domain.len() != KEY_DOMAIN_LEN || !domain.bytes().all(|b| Z32_ALPHABET.contains(&b)) {
        return Ok(None);
    }
    let invalid = || InvalidKeyDomain(domain.to_owned());
    let key = PublicKey::from_z32(domain).map_err(|_| invalid())?;
    // 52 characters carry 260 bits; the encoding is canonical only if the
    // 4 spare bits are zero.
    if key.to_z32() != domain {
        return Err(invalid());
    }
    Ok(Some(key))
}

/// Whether `domain` is a key domain (see [`parse_key_domain`]).
pub fn is_key_domain(domain: &str) -> bool {
    matches!(parse_key_domain(domain), Ok(Some(_)))
}

/// The signer id of a key: the SHA-256 of its 32 raw bytes.
pub fn signer_id(key: &PublicKey) -> Vec<u8> {
    Sha256::digest(key.as_bytes()).to_vec()
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The server key, loaded from or created in a file.
#[derive(Clone)]
pub struct ServerKey {
    secret: SecretKey,
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerKey")
            .field("public", &key_domain(&self.public()))
            .finish_non_exhaustive()
    }
}

impl ServerKey {
    pub fn from_secret(secret: SecretKey) -> Self {
        ServerKey { secret }
    }

    /// A new random key, not stored anywhere.
    pub fn generate() -> Self {
        ServerKey {
            secret: SecretKey::generate(),
        }
    }

    pub fn secret(&self) -> &SecretKey {
        &self.secret
    }

    pub fn public(&self) -> PublicKey {
        self.secret.public()
    }

    /// The key domain of this key.
    pub fn key_domain(&self) -> String {
        key_domain(&self.public())
    }

    pub fn signer_id(&self) -> Vec<u8> {
        signer_id(&self.public())
    }

    /// Loads the key stored at `path`; `None` if there is no such file.
    pub fn load(path: &Path) -> Result<Option<Self>, KeyError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(KeyError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        let text = text.trim();
        let invalid = || KeyError::Invalid {
            path: path.to_owned(),
        };
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let secret: SecretKey = text.parse().map_err(|_| invalid())?;
        warn_if_readable_by_others(path);
        Ok(Some(ServerKey { secret }))
    }

    /// Loads the key stored at `path`, creating it (mode 600) if there is
    /// none. Creation never replaces a key that appears concurrently.
    pub fn load_or_create(path: &Path) -> Result<Self, KeyError> {
        if let Some(key) = Self::load(path)? {
            return Ok(key);
        }
        let key = Self::generate();
        let write_error = |source| KeyError::Write {
            path: path.to_owned(),
            source,
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(write_error)?;
        }
        let result = write_new_file(path, &format!("{}\n", to_hex(&key.secret.to_bytes())));
        match result {
            Ok(()) => {
                tracing::info!(path = %path.display(), key = %key.key_domain(), "created the federation server key");
                Ok(key)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Self::load(path)?
                .ok_or_else(|| KeyError::Invalid {
                    path: path.to_owned(),
                }),
            Err(source) => Err(write_error(source)),
        }
    }
}

/// Writes `contents` to a new file at `path`, readable by the owner only
/// (mode 600), through a temporary file and a hard link: fails with
/// [`std::io::ErrorKind::AlreadyExists`] if `path` exists, so it never
/// replaces a file that appears concurrently. Where hard links are not
/// allowed (Android's app sandbox denies them), the file is created in place
/// with `O_EXCL` instead, which keeps that guarantee.
pub(crate) fn write_new_file(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".tmp-{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, path)
    })();
    let _ = std::fs::remove_file(&temporary);
    match result {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            write_exclusive(path, contents)
        }
        result => result,
    }
}

/// Creates `path` (mode 600) only if it does not exist, and writes
/// `contents` to it; a failed write removes the file again.
fn write_exclusive(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    if written.is_err() {
        let _ = std::fs::remove_file(path);
    }
    written
}

pub(crate) fn warn_if_readable_by_others(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path)
            && metadata.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(
                path = %path.display(),
                "the federation server key is readable by other users; chmod 600 it"
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_exclusive_fallback_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("person.key");
        super::write_exclusive(&path, "first\n").unwrap();
        let again = super::write_exclusive(&path, "second\n").unwrap_err();
        assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    use resonator_model::id::is_valid_domain;

    use super::*;

    #[test]
    fn key_domains_are_valid_domains_and_round_trip() {
        for _ in 0..32 {
            let key = SecretKey::generate().public();
            let domain = key_domain(&key);
            assert_eq!(domain.len(), KEY_DOMAIN_LEN);
            assert!(is_valid_domain(0, &domain), "{domain}");
            assert_eq!(parse_key_domain(&domain), Ok(Some(key)));
            assert!(is_key_domain(&domain));
        }
    }

    #[test]
    fn other_domains_are_not_key_domains() {
        let key = key_domain(&SecretKey::generate().public());
        for domain in [
            "example.com",
            "local.net",
            "localhost",
            &key[..51],
            &format!("{key}y"),
            &format!("{}.com", &key[..48]),
            // Upper case is not z-base-32.
            &key.to_ascii_uppercase(),
            // 'l', 'v' and '2' are not in the alphabet.
            &format!("l{}", &key[1..]),
        ] {
            assert_eq!(parse_key_domain(domain), Ok(None), "{domain}");
        }
    }

    #[test]
    fn non_canonical_or_invalid_keys_are_rejected() {
        let key = key_domain(&SecretKey::generate().public());
        // The last character carries 4 spare bits; setting one makes the
        // encoding non-canonical (or the key invalid).
        let last = key.as_bytes()[51];
        let index = Z32_ALPHABET.iter().position(|&c| c == last).unwrap();
        let other = Z32_ALPHABET[index ^ 1] as char;
        let tampered = format!("{}{other}", &key[..51]);
        assert!(parse_key_domain(&tampered).is_err(), "{tampered}");
    }

    #[test]
    fn signer_id_is_sha256_of_the_key() {
        let key = ServerKey::generate();
        assert_eq!(
            key.signer_id(),
            Sha256::digest(key.public().as_bytes()).to_vec()
        );
        assert_eq!(key.signer_id().len(), 32);
    }

    #[test]
    fn key_file_is_created_once_with_mode_600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("federation.key");
        assert!(ServerKey::load(&path).unwrap().is_none());
        let created = ServerKey::load_or_create(&path).unwrap();
        let loaded = ServerKey::load_or_create(&path).unwrap();
        assert_eq!(created.public(), loaded.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["federation.key"]);
        std::fs::write(&path, "not a key\n").unwrap();
        assert!(matches!(
            ServerKey::load(&path),
            Err(KeyError::Invalid { .. })
        ));
    }
}
