//! Wave, wavelet and participant identifiers, their validity rules and
//! their serialised forms.
//!
//! Derived from the `org.waveprotocol.wave.model.id` and
//! `org.waveprotocol.wave.model.waveref` packages and
//! `org.waveprotocol.wave.model.wave.ParticipantId` of Apache Wave, licensed
//! under the Apache License, Version 2.0.
//!
//! Strings are ordered by UTF-16 code units, as Java `String.compareTo`
//! does. Where the original throws `IllegalArgumentException` or a checked
//! exception on bad input, the functions here return an error.

pub mod constants;
pub mod escaper;
pub mod filter;
pub mod generator;
pub mod identifiers;
pub(crate) mod java;
pub mod participant;
pub mod serialiser;
pub mod uri;
pub mod util;
pub mod wave_id;
pub mod waveref;

#[cfg(test)]
mod tests;

pub use escaper::{DEFAULT_ESCAPER, EscapeError, SimplePrefixEscaper};
pub use filter::IdFilter;
pub use generator::{IdGenerator, IdGeneratorImpl, TokenGenerator, base64_encode_int};
pub use identifiers::{is_valid_domain, is_valid_identifier};
pub use participant::{InvalidParticipantAddress, ParticipantId};
pub use serialiser::{DualIdSerialiser, IdSerialiser, LegacyIdSerialiser, ModernIdSerialiser};
pub use uri::{EncodingError, IdUriCodec};
pub use wave_id::{InvalidIdError, WaveId, WaveletId, WaveletName};
pub use waveref::{InvalidWaveRefError, WaveRef};
