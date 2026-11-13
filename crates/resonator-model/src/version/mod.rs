//! Wavelet versions paired with a history hash.
//!
//! Derived from the `org.waveprotocol.wave.model.version` package of Apache
//! Wave, licensed under the Apache License, Version 2.0.
//!
//! The history hash at version zero is the UTF-8 encoding of the wavelet
//! name's `wave://` URI. Each applied delta extends the chain: the new hash
//! is the first 20 bytes of `SHA-256(previous hash ++ applied delta bytes)`,
//! where the bytes are the serialised `ProtocolAppliedWaveletDelta`.

use std::cmp::Ordering;
use std::fmt;

use base64::Engine;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::id::{IdUriCodec, WaveletName};

#[cfg(test)]
mod tests;

/// A wavelet version number and the history hash at that version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HashedVersion {
    version: u64,
    history_hash: Vec<u8>,
}

impl HashedVersion {
    pub fn of(version: u64, history_hash: impl Into<Vec<u8>>) -> Self {
        HashedVersion {
            version,
            history_hash: history_hash.into(),
        }
    }

    /// A version with an empty hash.
    pub fn unsigned(version: u64) -> Self {
        HashedVersion {
            version,
            history_hash: Vec::new(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn history_hash(&self) -> &[u8] {
        &self.history_hash
    }

    /// Parses the form written by `Display`, `<version>:<base64 hash>`. The
    /// hash and its colon may be absent.
    pub fn from_string(s: &str) -> Result<Self, HashedVersionParseError> {
        let (version, hash) = match s.split_once(':') {
            Some((version, hash)) => (version, Some(hash)),
            None => (s, None),
        };
        // Parsed as a Java long, then checked to be non-negative.
        let version: i64 = version
            .parse()
            .map_err(|_| HashedVersionParseError::Version(version.to_owned()))?;
        let version = u64::try_from(version)
            .map_err(|_| HashedVersionParseError::Version(version.to_string()))?;
        let history_hash = match hash {
            None => Vec::new(),
            Some(hash) => LENIENT_BASE64
                .decode(hash)
                .map_err(|_| HashedVersionParseError::Hash(hash.to_owned()))?,
        };
        Ok(HashedVersion {
            version,
            history_hash,
        })
    }
}

/// Standard alphabet, padding optional.
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// A string that is not a serialised hashed version.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HashedVersionParseError {
    #[error("Invalid version: '{0}'")]
    Version(String),
    #[error("Invalid base64 hash: '{0}'")]
    Hash(String),
}

/// Orders by version, then by hash. Hash bytes compare as signed values, as
/// in the original, and a hash sorts after its proper prefixes.
impl Ord for HashedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.version.cmp(&other.version).then_with(|| {
            let signed = |bytes: &[u8]| bytes.iter().map(|&b| b as i8).collect::<Vec<i8>>();
            signed(&self.history_hash).cmp(&signed(&other.history_hash))
        })
    }
}

impl PartialOrd for HashedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `<version>:<hash in padded standard base64>`.
impl fmt::Display for HashedVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}",
            self.version,
            STANDARD.encode(&self.history_hash)
        )
    }
}

/// Number of bits of the SHA-256 digest kept in a history hash.
pub const HASH_SIZE_BITS: usize = 160;

/// Creates hashed versions (`HashedVersionFactoryImpl` with the server's URI
/// codec).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HashedVersionFactory;

impl HashedVersionFactory {
    /// The version of a wavelet before any delta: version 0 with the UTF-8
    /// bytes of the wavelet name URI as its hash.
    pub fn create_version_zero(wavelet_name: &WaveletName) -> HashedVersion {
        HashedVersion::of(
            0,
            IdUriCodec::wavelet_name_to_uri(wavelet_name).into_bytes(),
        )
    }

    /// The version after applying a delta at `version_applied_at`.
    /// `applied_delta_bytes` are the serialised `ProtocolAppliedWaveletDelta`.
    pub fn create(
        applied_delta_bytes: &[u8],
        version_applied_at: &HashedVersion,
        operations_applied: u32,
    ) -> HashedVersion {
        Self::create_with_hash_size(
            applied_delta_bytes,
            version_applied_at,
            operations_applied,
            HASH_SIZE_BITS,
        )
    }

    /// [`HashedVersionFactory::create`] with a hash truncated to
    /// `hash_size_bits` (at most 256) instead of [`HASH_SIZE_BITS`].
    pub fn create_with_hash_size(
        applied_delta_bytes: &[u8],
        version_applied_at: &HashedVersion,
        operations_applied: u32,
        hash_size_bits: usize,
    ) -> HashedVersion {
        let mut hasher = Sha256::new();
        hasher.update(version_applied_at.history_hash());
        hasher.update(applied_delta_bytes);
        let digest = hasher.finalize();
        let size = (hash_size_bits / 8).min(digest.len());
        HashedVersion::of(
            version_applied_at
                .version()
                .saturating_add(u64::from(operations_applied)),
            &digest[..size],
        )
    }
}
