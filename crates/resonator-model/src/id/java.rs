//! Small emulations of `java.lang.String` and `java.net` behaviour that the
//! original code relies on.

/// `String.split` with a single literal separator: empty tokens are kept
/// except trailing ones, and a string without the separator (including the
/// empty string) yields itself.
pub(crate) fn split(s: &str, separator: char) -> Vec<&str> {
    if !s.contains(separator) {
        return vec![s];
    }
    let mut parts: Vec<&str> = s.split(separator).collect();
    while parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    parts
}

const UPPER_HEX: &[u8; 16] = b"0123456789ABCDEF";

pub(crate) fn push_percent_encoded(out: &mut String, byte: u8) {
    out.push('%');
    out.push(UPPER_HEX[usize::from(byte >> 4)] as char);
    out.push(UPPER_HEX[usize::from(byte & 0xF)] as char);
}

/// `URLEncoder.encode(s, "UTF-8")`: alphanumerics and `.-*_` are kept, a
/// space becomes `+`, everything else is percent-encoded as UTF-8.
pub(crate) fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'*' | b'_') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            push_percent_encoded(&mut out, b);
        }
    }
    out
}

/// The byte-level part of `URLDecoder.decode`: `+` becomes a space and
/// `%XX` becomes a byte. Fails on an incomplete or non-hexadecimal escape,
/// where the original throws `IllegalArgumentException`.
pub(crate) fn url_decode_bytes(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (
                    bytes.get(i + 1).copied().and_then(hex),
                    bytes.get(i + 2).copied().and_then(hex),
                ) {
                    (Some(hi), Some(lo)) => out.push((hi * 16 + lo) as u8),
                    _ => return Err(format!("illegal escape (%) pattern in '{s}'")),
                }
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(out)
}
