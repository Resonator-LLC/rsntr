//! Engine values and their RDF literal mapping.
//!
//! - integer -> `xsd:integer`
//! - real -> `xsd:double`, shortest-round-trip lexical form
//! - text -> plain literal
//! - blob -> `xsd:base64Binary` inline; large blobs as `rsntr:BlobRef` nodes
//! - NULL -> the `rsntr:null` individual in parameter lists, column omission
//!   in result rows

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::error::ProtocolError;

/// One engine value as carried by the envelope.
#[derive(Debug, Clone)]
pub enum Value {
    /// SQL NULL. In parameter lists this is the `rsntr:null` marker; in
    /// result rows a NULL cell is encoded by omitting the column predicate.
    Null,
    /// `xsd:integer`.
    Integer(i64),
    /// `xsd:double`, written in shortest-round-trip form.
    Real(f64),
    /// Plain literal.
    Text(String),
    /// `xsd:base64Binary`, inline.
    Blob(Vec<u8>),
    /// Out-of-band blob: `[] a rsntr:BlobRef ; rsntr:hash "blake3:..." ;
    /// rsntr:bytes N`, fetched via iroh-blobs.
    BlobRef {
        /// Content hash, e.g. `"blake3:..."`.
        hash: String,
        /// Size in bytes.
        bytes: u64,
    },
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Integer(a), Self::Integer(b)) => a == b,
            // Bit equality: -0.0 != 0.0, NaN == NaN. Stricter than IEEE
            // comparison and the right notion for a codec round-trip.
            (Self::Real(a), Self::Real(b)) => a.to_bits() == b.to_bits(),
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Blob(a), Self::Blob(b)) => a == b,
            (
                Self::BlobRef {
                    hash: h1,
                    bytes: b1,
                },
                Self::BlobRef {
                    hash: h2,
                    bytes: b2,
                },
            ) => h1 == h2 && b1 == b2,
            _ => false,
        }
    }
}

impl Eq for Value {}

/// Formats an f64 in a shortest-round-trip `xsd:double` lexical form.
///
/// Rust's `LowerExp` formatting emits the shortest mantissa that uniquely
/// identifies the double, with an explicit exponent, which is inside the
/// `xsd:double` lexical space. Non-finite values use the XSD special tokens.
pub(crate) fn format_double(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f == f64::INFINITY {
        "INF".to_owned()
    } else if f == f64::NEG_INFINITY {
        "-INF".to_owned()
    } else {
        format!("{f:e}")
    }
}

/// Parses an `xsd:double`/`xsd:decimal`/`xsd:float` lexical form.
pub(crate) fn parse_double(lex: &str) -> Result<f64, ProtocolError> {
    match lex {
        "INF" | "+INF" => Ok(f64::INFINITY),
        "-INF" => Ok(f64::NEG_INFINITY),
        "NaN" => Ok(f64::NAN),
        other => other.trim().parse::<f64>().map_err(|e| {
            ProtocolError::malformed(format!("invalid double literal {other:?}: {e}"))
        }),
    }
}

/// Parses an `xsd:integer` lexical form into i64.
pub(crate) fn parse_integer(lex: &str) -> Result<i64, ProtocolError> {
    lex.trim().parse::<i64>().map_err(|e| {
        ProtocolError::malformed(format!(
            "invalid or out-of-range integer literal {lex:?}: {e}"
        ))
    })
}

/// Encodes a blob for `xsd:base64Binary`.
pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Decodes an `xsd:base64Binary` lexical form (XSD permits whitespace).
pub(crate) fn decode_base64(lex: &str) -> Result<Vec<u8>, ProtocolError> {
    let compact: String = lex.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    BASE64
        .decode(compact.as_bytes())
        .map_err(|e| ProtocolError::malformed(format!("invalid base64Binary literal: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_shortest_forms_round_trip() {
        for f in [0.1, 1e308, -0.0, 0.0, 1.5, f64::MIN, f64::MAX, 5e-324] {
            let lex = format_double(f);
            let back = parse_double(&lex).unwrap();
            assert_eq!(f.to_bits(), back.to_bits(), "lex {lex}");
        }
    }

    #[test]
    fn double_specials() {
        assert_eq!(format_double(f64::INFINITY), "INF");
        assert_eq!(format_double(f64::NEG_INFINITY), "-INF");
        assert_eq!(format_double(f64::NAN), "NaN");
        assert!(parse_double("NaN").unwrap().is_nan());
        assert_eq!(parse_double("INF").unwrap(), f64::INFINITY);
        assert_eq!(parse_double("-INF").unwrap(), f64::NEG_INFINITY);
    }

    #[test]
    fn integer_bounds() {
        assert_eq!(parse_integer("-9223372036854775808").unwrap(), i64::MIN);
        assert_eq!(parse_integer("9223372036854775807").unwrap(), i64::MAX);
        assert!(parse_integer("9223372036854775808").is_err());
        assert!(parse_integer("nope").is_err());
    }

    #[test]
    fn base64_round_trip() {
        let data = vec![0u8, 1, 2, 255, 254, 128];
        let lex = encode_base64(&data);
        assert_eq!(decode_base64(&lex).unwrap(), data);
        // XSD allows whitespace inside the lexical form.
        assert_eq!(decode_base64("AA E=").unwrap(), vec![0u8, 1]);
        assert!(decode_base64("!!!").is_err());
    }

    #[test]
    fn value_eq_is_bitwise_for_reals() {
        assert_ne!(Value::Real(0.0), Value::Real(-0.0));
        assert_eq!(Value::Real(f64::NAN), Value::Real(f64::NAN));
    }
}
