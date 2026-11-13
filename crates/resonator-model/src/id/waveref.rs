//! References to a wave, wavelet or document, and their URI path and query
//! encodings (`WaveRef`, `WaverefEncoder` with the JVM escapers).

use std::fmt;

use thiserror::Error;

use super::java;
use super::wave_id::{WaveId, WaveletId};

/// A string that is not a valid encoded wave reference.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid waveRef '{reference}': {message}")]
pub struct InvalidWaveRefError {
    pub reference: String,
    pub message: String,
}

/// A reference to a wave, optionally narrowed to a wavelet and to a document
/// in that wavelet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WaveRef {
    wave_id: WaveId,
    wavelet_id: Option<WaveletId>,
    document_id: Option<String>,
}

impl WaveRef {
    pub fn of(wave_id: WaveId) -> Self {
        WaveRef {
            wave_id,
            wavelet_id: None,
            document_id: None,
        }
    }

    pub fn of_wavelet(wave_id: WaveId, wavelet_id: WaveletId) -> Self {
        WaveRef {
            wave_id,
            wavelet_id: Some(wavelet_id),
            document_id: None,
        }
    }

    pub fn of_document(wave_id: WaveId, wavelet_id: WaveletId, document_id: &str) -> Self {
        WaveRef {
            wave_id,
            wavelet_id: Some(wavelet_id),
            document_id: Some(document_id.to_owned()),
        }
    }

    pub fn wave_id(&self) -> &WaveId {
        &self.wave_id
    }

    pub fn wavelet_id(&self) -> Option<&WaveletId> {
        self.wavelet_id.as_ref()
    }

    pub fn document_id(&self) -> Option<&str> {
        self.document_id.as_deref()
    }

    /// Encodes the reference for use in a URI path.
    pub fn encode_to_uri_path_segment(&self) -> String {
        self.encode(encode_to_uri_path_segment)
    }

    /// Encodes the reference for use in a URI query string.
    pub fn encode_to_uri_query_string(&self) -> String {
        self.encode(encode_to_uri_query_string)
    }

    fn encode(&self, escape: fn(&str) -> String) -> String {
        let mut out = escape(self.wave_id.domain());
        out.push('/');
        out.push_str(&escape(self.wave_id.id()));
        if let Some(wavelet_id) = &self.wavelet_id {
            if wavelet_id.domain() == self.wave_id.domain() {
                out.push_str("/~");
            } else {
                out.push('/');
                out.push_str(&escape(wavelet_id.domain()));
            }
            out.push('/');
            out.push_str(&escape(wavelet_id.id()));
        }
        if let Some(document_id) = &self.document_id {
            out.push('/');
            out.push_str(&escape(document_id));
        }
        out
    }

    /// Decodes `waveDomain/waveId[/waveletDomain/waveletId[/documentId]]`.
    ///
    /// Quirks kept from the original: the wave domain is lower-cased, the
    /// document id is not percent-decoded, tokens after the fifth are
    /// ignored, and an encoded space decodes to `+`.
    pub fn decode_from_path(path: &str) -> Result<WaveRef, InvalidWaveRefError> {
        let error = |message: String| InvalidWaveRefError {
            reference: path.to_owned(),
            message,
        };
        let tokens = java::split(path, '/');
        if tokens.len() < 2 || tokens.len() == 3 {
            return Err(error(
                "Invalid number of tokens in path given to decodeWaveRefFromPath".to_owned(),
            ));
        }
        if tokens[0].is_empty() || tokens[1].is_empty() {
            return Err(error(
                "The wave domain and thewave Id must not be empty.".to_owned(),
            ));
        }
        let wave_domain = decode(&tokens[0].to_lowercase())?;
        let wave_id = WaveId::of(&wave_domain, &decode(tokens[1])?)
            .map_err(|e| error(format!("Invalid WaveID:{e}")))?;
        if tokens.len() == 2 {
            return Ok(WaveRef::of(wave_id));
        }
        if tokens[2].is_empty() || tokens[3].is_empty() {
            return Err(error(
                "The wavelet domain and thewavelet Id must not be empty.".to_owned(),
            ));
        }
        let mut wavelet_domain = decode(tokens[2])?;
        if wavelet_domain == "~" {
            wavelet_domain = wave_domain;
        }
        let wavelet_id = WaveletId::of(&wavelet_domain, &decode(tokens[3])?)
            .map_err(|_| error("Invalid WaveletID".to_owned()))?;
        match tokens.get(4) {
            None => Ok(WaveRef::of_wavelet(wave_id, wavelet_id)),
            Some(document_id) => Ok(WaveRef::of_document(wave_id, wavelet_id, document_id)),
        }
    }
}

impl fmt::Display for WaveRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[WaveRef:{}", self.wave_id)?;
        if let Some(wavelet_id) = &self.wavelet_id {
            write!(f, "{wavelet_id}")?;
        }
        if let Some(document_id) = &self.document_id {
            write!(f, "[documentId:{document_id}]")?;
        }
        f.write_str("]")
    }
}

const SAFE_PATH_CHARS: &str = "-_.!~*'()@:$&,;=+";
const SAFE_QUERY_CHARS: &str = "-_.!~*'()@:$,;/?:+";

fn percent_escape(s: &str, safe: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || safe.as_bytes().contains(&b) {
            out.push(b as char);
        } else {
            java::push_percent_encoded(&mut out, b);
        }
    }
    out
}

/// Percent-escapes a string for use in a URI path segment.
pub fn encode_to_uri_path_segment(s: &str) -> String {
    percent_escape(s, SAFE_PATH_CHARS)
}

/// Percent-escapes a string for use in a URI query string.
pub fn encode_to_uri_query_string(s: &str) -> String {
    percent_escape(s, SAFE_QUERY_CHARS)
}

/// `URLDecoder.decode` followed by replacing spaces with `+`. Bytes that are
/// not UTF-8 decode to U+FFFD.
fn decode(encoded: &str) -> Result<String, InvalidWaveRefError> {
    let bytes = java::url_decode_bytes(encoded).map_err(|_| InvalidWaveRefError {
        reference: encoded.to_owned(),
        message: "Failed to decode".to_owned(),
    })?;
    Ok(String::from_utf8_lossy(&bytes).replace(' ', "+"))
}
