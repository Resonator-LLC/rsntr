//! Serialised forms of ids: the modern `domain/id` form and the legacy
//! `domain!id` form.

use super::escaper::DEFAULT_ESCAPER;
use super::identifiers::{is_valid_domain, is_valid_identifier};
use super::java;
use super::wave_id::{InvalidIdError, WaveId, WaveletId, WaveletName};

/// Converts ids to and from strings.
pub trait IdSerialiser {
    fn serialise_wave_id(&self, id: &WaveId) -> Result<String, InvalidIdError>;
    fn serialise_wavelet_id(&self, id: &WaveletId) -> Result<String, InvalidIdError>;
    fn serialise_wavelet_name(&self, name: &WaveletName) -> Result<String, InvalidIdError>;
    fn deserialise_wave_id(&self, serialised: &str) -> Result<WaveId, InvalidIdError>;
    fn deserialise_wavelet_id(&self, serialised: &str) -> Result<WaveletId, InvalidIdError>;
    fn deserialise_wavelet_name(&self, serialised: &str) -> Result<WaveletName, InvalidIdError>;
}

const SEP: char = '/';
const ELIDE: &str = "~";

/// The modern serialisation: `domain/id` for ids and
/// `waveDomain/waveId/waveletDomain/waveletId` for wavelet names, where a
/// wavelet domain equal to the wave domain is written `~`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModernIdSerialiser;

fn serialise_modern(domain: &str, id: &str) -> Result<String, InvalidIdError> {
    // Ids built with `of_legacy` may violate these.
    if !is_valid_domain(0, domain) {
        return Err(InvalidIdError::new(
            domain,
            format!("Invalid domain {domain}"),
        ));
    }
    if !is_valid_identifier(id) {
        return Err(InvalidIdError::new(id, format!("Invalid id {id}")));
    }
    Ok(format!("{domain}{SEP}{id}"))
}

fn two_tokens<'a>(serialised: &'a str, what: &str) -> Result<(&'a str, &'a str), InvalidIdError> {
    match java::split(serialised, SEP)[..] {
        [domain, id] => Ok((domain, id)),
        _ => Err(InvalidIdError::new(
            serialised,
            format!("Required 2 '/'-separated tokens in serialised {what}: {serialised}"),
        )),
    }
}

impl IdSerialiser for ModernIdSerialiser {
    fn serialise_wave_id(&self, id: &WaveId) -> Result<String, InvalidIdError> {
        serialise_modern(id.domain(), id.id())
    }

    fn serialise_wavelet_id(&self, id: &WaveletId) -> Result<String, InvalidIdError> {
        serialise_modern(id.domain(), id.id())
    }

    fn serialise_wavelet_name(&self, name: &WaveletName) -> Result<String, InvalidIdError> {
        let mut out = self.serialise_wave_id(&name.wave_id)?;
        out.push(SEP);
        if name.wavelet_id.domain() == name.wave_id.domain() {
            out.push_str(ELIDE);
        } else {
            out.push_str(name.wavelet_id.domain());
        }
        out.push(SEP);
        // The wavelet id is not re-checked here, as in the original.
        out.push_str(name.wavelet_id.id());
        Ok(out)
    }

    fn deserialise_wave_id(&self, serialised: &str) -> Result<WaveId, InvalidIdError> {
        let (domain, id) = two_tokens(serialised, "wave id")?;
        WaveId::of(domain, id)
    }

    fn deserialise_wavelet_id(&self, serialised: &str) -> Result<WaveletId, InvalidIdError> {
        let (domain, id) = two_tokens(serialised, "wavelet id")?;
        WaveletId::of(domain, id)
    }

    fn deserialise_wavelet_name(&self, serialised: &str) -> Result<WaveletName, InvalidIdError> {
        if serialised.ends_with(SEP) {
            return Err(InvalidIdError::new(
                serialised,
                "Serialised wavelet name had trailing '/'",
            ));
        }
        let [wave_domain, wave_id, wavelet_domain, wavelet_id] = java::split(serialised, SEP)[..]
        else {
            return Err(InvalidIdError::new(
                serialised,
                format!("Required 4 '/'-separated tokens in serialised wavelet name: {serialised}"),
            ));
        };
        if wavelet_domain == wave_domain {
            return Err(InvalidIdError::new(
                serialised,
                "Serialised wavelet name had un-normalised domains",
            ));
        }
        let wavelet_domain = if wavelet_domain == ELIDE {
            wave_domain
        } else {
            wavelet_domain
        };
        Ok(WaveletName::of(
            WaveId::of(wave_domain, wave_id)?,
            WaveletId::of(wavelet_domain, wavelet_id)?,
        ))
    }
}

/// The legacy serialisation, `domain!id`. Wavelet names have no legacy form.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LegacyIdSerialiser;

impl LegacyIdSerialiser {
    pub const PART_SEPARATOR: char = '!';
}

fn legacy_parts<'a>(serialised: &'a str, what: &str) -> Result<(&'a str, &'a str), InvalidIdError> {
    let parts = DEFAULT_ESCAPER
        .split_without_unescaping(LegacyIdSerialiser::PART_SEPARATOR, serialised)
        .map_err(|e| InvalidIdError::new(serialised, e.0))?;
    match parts[..] {
        [domain, id] if !domain.is_empty() && !id.is_empty() => Ok((domain, id)),
        _ => Err(InvalidIdError::new(
            serialised,
            format!("{what} must be of the form <domain>!<id>"),
        )),
    }
}

fn no_legacy_names<T>() -> Result<T, InvalidIdError> {
    Err(InvalidIdError::new(
        "",
        "No legacy serialization for wavelet names",
    ))
}

impl IdSerialiser for LegacyIdSerialiser {
    fn serialise_wave_id(&self, id: &WaveId) -> Result<String, InvalidIdError> {
        Ok(format!("{}!{}", id.domain(), id.id()))
    }

    fn serialise_wavelet_id(&self, id: &WaveletId) -> Result<String, InvalidIdError> {
        Ok(format!("{}!{}", id.domain(), id.id()))
    }

    /// Always fails (`UnsupportedOperationException` in the original).
    fn serialise_wavelet_name(&self, _name: &WaveletName) -> Result<String, InvalidIdError> {
        no_legacy_names()
    }

    fn deserialise_wave_id(&self, serialised: &str) -> Result<WaveId, InvalidIdError> {
        let (domain, id) = legacy_parts(serialised, "Wave id")?;
        WaveId::of_legacy(domain, id).map_err(|e| InvalidIdError::new(serialised, e.message))
    }

    fn deserialise_wavelet_id(&self, serialised: &str) -> Result<WaveletId, InvalidIdError> {
        let (domain, id) = legacy_parts(serialised, "Wavelet id")?;
        WaveletId::of_legacy(domain, id).map_err(|e| InvalidIdError::new(serialised, e.message))
    }

    /// Always fails (`UnsupportedOperationException` in the original).
    fn deserialise_wavelet_name(&self, _serialised: &str) -> Result<WaveletName, InvalidIdError> {
        no_legacy_names()
    }
}

/// Reads both forms (modern first) and writes one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DualIdSerialiser {
    to_modern: bool,
}

impl DualIdSerialiser {
    /// Writes the modern form.
    pub const MODERN: DualIdSerialiser = DualIdSerialiser { to_modern: true };
    /// Writes the legacy form.
    pub const LEGACY: DualIdSerialiser = DualIdSerialiser { to_modern: false };
}

impl IdSerialiser for DualIdSerialiser {
    fn serialise_wave_id(&self, id: &WaveId) -> Result<String, InvalidIdError> {
        if self.to_modern {
            ModernIdSerialiser.serialise_wave_id(id)
        } else {
            LegacyIdSerialiser.serialise_wave_id(id)
        }
    }

    fn serialise_wavelet_id(&self, id: &WaveletId) -> Result<String, InvalidIdError> {
        if self.to_modern {
            ModernIdSerialiser.serialise_wavelet_id(id)
        } else {
            LegacyIdSerialiser.serialise_wavelet_id(id)
        }
    }

    fn serialise_wavelet_name(&self, name: &WaveletName) -> Result<String, InvalidIdError> {
        if self.to_modern {
            ModernIdSerialiser.serialise_wavelet_name(name)
        } else {
            LegacyIdSerialiser.serialise_wavelet_name(name)
        }
    }

    fn deserialise_wave_id(&self, serialised: &str) -> Result<WaveId, InvalidIdError> {
        ModernIdSerialiser
            .deserialise_wave_id(serialised)
            .or_else(|_| LegacyIdSerialiser.deserialise_wave_id(serialised))
    }

    fn deserialise_wavelet_id(&self, serialised: &str) -> Result<WaveletId, InvalidIdError> {
        ModernIdSerialiser
            .deserialise_wavelet_id(serialised)
            .or_else(|_| LegacyIdSerialiser.deserialise_wavelet_id(serialised))
    }

    fn deserialise_wavelet_name(&self, serialised: &str) -> Result<WaveletName, InvalidIdError> {
        ModernIdSerialiser
            .deserialise_wavelet_name(serialised)
            .or_else(|_| LegacyIdSerialiser.deserialise_wavelet_name(serialised))
    }
}
