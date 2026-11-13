//! The `wave://` URI form of wavelet names (`IdURIEncoderDecoder` with the
//! server's `JavaUrlCodec`). The version-zero history hash of a wavelet is
//! the UTF-8 encoding of this URI.

use thiserror::Error;

use super::constants::WAVE_URI_SCHEME;
use super::escaper::DEFAULT_ESCAPER;
use super::java;
use super::wave_id::{InvalidIdError, WaveId, WaveletId, WaveletName};

/// Failure to decode a wavelet name URI.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EncodingError {
    /// A percent-escape is malformed or does not decode to UTF-8.
    #[error("Unable to decode value {0}")]
    Decode(String),
    /// The URI does not have the expected shape.
    #[error("{0}")]
    Format(String),
    #[error(transparent)]
    InvalidId(#[from] InvalidIdError),
}

/// Characters of an id that are written to a URI path as they are, besides
/// ASCII letters and digits.
const NOT_ESCAPED_SYMBOLS: &str = ":@!$&'()*+,;=-._~";

fn is_not_escaped(c: char) -> bool {
    c.is_ascii_alphanumeric() || NOT_ESCAPED_SYMBOLS.contains(c)
}

/// Converts wavelet names to and from `wave://` URIs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdUriCodec;

impl IdUriCodec {
    /// Percent-encodes an id for use in a URI path segment. Runs of other
    /// characters go through `URLEncoder`, so a space becomes `+`.
    pub fn encode(decoded: &str) -> String {
        let mut out = String::with_capacity(decoded.len());
        let mut rest = decoded;
        while let Some(c) = rest.chars().next() {
            if is_not_escaped(c) {
                out.push(c);
                rest = &rest[1..];
            } else {
                let end = rest.find(is_not_escaped).unwrap_or(rest.len());
                out.push_str(&java::url_encode(&rest[..end]));
                rest = &rest[end..];
            }
        }
        out
    }

    /// Reverses [`IdUriCodec::encode`]. A literal `+` stands for itself.
    /// Fails on malformed escapes, on escapes that are not UTF-8, and (as in
    /// the original) on any value containing U+FFFD.
    pub fn decode(encoded: &str) -> Result<String, EncodingError> {
        let error = || EncodingError::Decode(encoded.to_owned());
        let bytes = java::url_decode_bytes(&encoded.replace('+', "%2B")).map_err(|_| error())?;
        let decoded = String::from_utf8(bytes).map_err(|_| error())?;
        if decoded.contains('\u{FFFD}') {
            return Err(error());
        }
        Ok(decoded)
    }

    /// `wave://` followed by [`IdUriCodec::wavelet_name_to_uri_path`].
    pub fn wavelet_name_to_uri(name: &WaveletName) -> String {
        format!(
            "{WAVE_URI_SCHEME}://{}",
            Self::wavelet_name_to_uri_path(name)
        )
    }

    /// `<wavelet domain>/[<wave domain>!]<wave id>/<wavelet id>`, with the
    /// wave domain omitted when it equals the wavelet domain.
    pub fn wavelet_name_to_uri_path(name: &WaveletName) -> String {
        let wave_domain = if name.wavelet_id.domain() == name.wave_id.domain() {
            String::new()
        } else {
            format!("{}!", name.wave_id.domain())
        };
        format!(
            "{}/{}{}/{}",
            name.wavelet_id.domain(),
            wave_domain,
            Self::encode(name.wave_id.id()),
            Self::encode(name.wavelet_id.id())
        )
    }

    pub fn uri_to_wavelet_name(uri: &str) -> Result<WaveletName, EncodingError> {
        let Some(path) = uri
            .strip_prefix(WAVE_URI_SCHEME)
            .and_then(|rest| rest.strip_prefix("://"))
        else {
            return Err(EncodingError::Format(format!(
                "Invalid scheme for the wavelet name URI: {uri}"
            )));
        };
        Self::uri_path_to_wavelet_name(path)
    }

    /// Parses a URI path into a wavelet name. Ids are created under the
    /// legacy rules.
    pub fn uri_path_to_wavelet_name(path: &str) -> Result<WaveletName, EncodingError> {
        let [wavelet_domain, wave_part, wavelet_part] = java::split(path, '/')[..] else {
            return Err(EncodingError::Format(format!(
                "Wavelet name URI path expected in the format of <wavelet domain>/[<wave \
                 domain>!]<wave id>/<wavelet id> but got: {path}"
            )));
        };
        let wavelet_id = WaveletId::of_legacy(wavelet_domain, &Self::decode(wavelet_part)?)?;
        let wave_id_parts = DEFAULT_ESCAPER
            .split_without_unescaping('!', wave_part)
            .map_err(|e| EncodingError::Format(e.0))?;
        let wave_id = match wave_id_parts[..] {
            [id] => WaveId::of_legacy(wavelet_id.domain(), &Self::decode(id)?)?,
            [domain, id] => WaveId::of_legacy(domain, &Self::decode(id)?)?,
            _ => {
                return Err(EncodingError::Format(format!(
                    "Wave id in URI path is invalid. Expected the format [<wave domain>!]<wave \
                     id> but got: {wave_part}"
                )));
            }
        };
        Ok(WaveletName::of(wave_id, wavelet_id))
    }
}
