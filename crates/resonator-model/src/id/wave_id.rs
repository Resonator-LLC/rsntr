//! Wave ids, wavelet ids and wavelet names.

use std::cmp::Ordering;
use std::fmt;

use thiserror::Error;

use super::constants::TOKEN_SEPARATOR;
use super::escaper::DEFAULT_ESCAPER;
use super::identifiers::{is_valid_domain, is_valid_identifier};
use super::serialiser::{DualIdSerialiser, IdSerialiser, LegacyIdSerialiser};
use crate::docop::utf16::cmp_utf16;

/// An id that is invalid or cannot be (de)serialised (`InvalidIdException`,
/// and `IllegalArgumentException` from the id factories).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid id '{id}': {message}")]
pub struct InvalidIdError {
    pub id: String,
    pub message: String,
}

impl InvalidIdError {
    pub fn new(id: impl Into<String>, message: impl Into<String>) -> Self {
        InvalidIdError {
            id: id.into(),
            message: message.into(),
        }
    }
}

fn check_modern(domain: &str, id: &str) -> Result<(), InvalidIdError> {
    if !is_valid_domain(0, domain) {
        return Err(InvalidIdError::new(domain, "Invalid domain"));
    }
    if !is_valid_identifier(id) {
        return Err(InvalidIdError::new(id, "Invalid id"));
    }
    Ok(())
}

fn check_legacy(domain: &str, id: &str) -> Result<(), InvalidIdError> {
    if domain.is_empty() {
        return Err(InvalidIdError::new(domain, "Empty domain"));
    }
    if id.is_empty() {
        return Err(InvalidIdError::new(id, "Empty id"));
    }
    if DEFAULT_ESCAPER.has_escape_characters(domain) {
        return Err(InvalidIdError::new(
            domain,
            format!("Domain cannot contain characters that requires escaping: {domain}"),
        ));
    }
    if !DEFAULT_ESCAPER.is_escaped_properly(TOKEN_SEPARATOR, id) {
        return Err(InvalidIdError::new(
            id,
            format!("Id is not properly escaped: {id}"),
        ));
    }
    Ok(())
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $label:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name {
            domain: String,
            id: String,
        }

        impl $name {
            /// Creates an id from a domain and a local identifier, checking
            /// both against the current validity rules (`of` and `ofChecked`
            /// in the original).
            pub fn of(domain: &str, id: &str) -> Result<Self, InvalidIdError> {
                check_modern(domain, id)?;
                Ok($name {
                    domain: domain.to_owned(),
                    id: id.to_owned(),
                })
            }

            /// Creates an id under the looser legacy rules: both parts
            /// non-empty, no `~`, `+` or `!` in the domain, and the id
            /// properly escaped. The result may not be serialisable in the
            /// modern form.
            pub fn of_legacy(domain: &str, id: &str) -> Result<Self, InvalidIdError> {
                check_legacy(domain, id)?;
                Ok($name {
                    domain: domain.to_owned(),
                    id: id.to_owned(),
                })
            }

            pub fn domain(&self) -> &str {
                &self.domain
            }

            pub fn id(&self) -> &str {
                &self.id
            }
        }

        impl Ord for $name {
            fn cmp(&self, other: &Self) -> Ordering {
                cmp_utf16(&self.domain, &other.domain).then_with(|| cmp_utf16(&self.id, &other.id))
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        /// Formats like the original `toString`, e.g. `[WaveId example.com/w+abc]`.
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "[{} {}/{}]", $label, self.domain, self.id)
            }
        }
    };
}

id_type!(
    /// Identifies a wave: a wave provider domain and an id unique within it.
    WaveId,
    "WaveId"
);
id_type!(
    /// Identifies a wavelet within a wave: a domain and an id unique within
    /// the wave.
    WaveletId,
    "WaveletId"
);

impl WaveId {
    /// Deserialises a wave id in the modern (`domain/id`) or, failing that,
    /// the legacy (`domain!id`) form.
    pub fn deserialise(s: &str) -> Result<WaveId, InvalidIdError> {
        DualIdSerialiser::MODERN.deserialise_wave_id(s)
    }

    /// Serialises in the modern form, `domain/id`. Fails for a legacy id
    /// that the modern rules reject.
    pub fn serialise(&self) -> Result<String, InvalidIdError> {
        DualIdSerialiser::MODERN.serialise_wave_id(self)
    }
}

impl WaveletId {
    /// Deserialises a wavelet id in the legacy form, `domain!id`.
    pub fn deserialise(s: &str) -> Result<WaveletId, InvalidIdError> {
        LegacyIdSerialiser.deserialise_wavelet_id(s)
    }

    /// Serialises in the legacy form, `domain!id` (unlike
    /// [`WaveId::serialise`]; the asymmetry is in the original).
    pub fn serialise(&self) -> String {
        format!("{}!{}", self.domain, self.id)
    }
}

/// The globally unique name of a wavelet: a wave id and a wavelet id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WaveletName {
    pub wave_id: WaveId,
    pub wavelet_id: WaveletId,
}

impl WaveletName {
    pub fn of(wave_id: WaveId, wavelet_id: WaveletId) -> Self {
        WaveletName {
            wave_id,
            wavelet_id,
        }
    }

    pub fn of_parts(
        wave_domain: &str,
        wave_id: &str,
        wavelet_domain: &str,
        wavelet_id: &str,
    ) -> Result<Self, InvalidIdError> {
        Ok(WaveletName {
            wave_id: WaveId::of(wave_domain, wave_id)?,
            wavelet_id: WaveletId::of(wavelet_domain, wavelet_id)?,
        })
    }
}

impl Ord for WaveletName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wave_id
            .cmp(&other.wave_id)
            .then_with(|| self.wavelet_id.cmp(&other.wavelet_id))
    }
}

impl PartialOrd for WaveletName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for WaveletName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[WaveletName {}/{}/{}/{}]",
            self.wave_id.domain, self.wave_id.id, self.wavelet_id.domain, self.wavelet_id.id
        )
    }
}
