//! Validity rules for the domain and local parts of wave and wavelet ids
//! (`WaveIdentifiers` in the original).

use crate::docop::utf16::is_code_point_valid;

fn is_safe_ascii(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '+' | '*' | '@')
}

/// The `ucschar` production of RFC 3987.
fn is_ucs_char(c: u32) -> bool {
    (0xA0..=0xD7FF).contains(&c)
        || (0xF900..=0xFDCF).contains(&c)
        || (0xFDF0..=0xFFEF).contains(&c)
        || ((0x10000..=0xDFFFD).contains(&c) && (c & 0xFFFF) <= 0xFFFD)
        || (0xE1000..=0xEFFFD).contains(&c)
}

/// Whether `id` is a valid local identifier: non-empty, and made only of
/// ASCII letters, digits, `-._~+*@` and non-private UCS characters.
pub fn is_valid_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| {
            if !is_code_point_valid(c) {
                false
            } else if (c as u32) < 0x7F {
                is_safe_ascii(c)
            } else {
                is_ucs_char(c as u32)
            }
        })
}

/// Whether `x`, from UTF-16 offset `start` on, is a valid domain name:
/// dot-separated labels of lower-case ASCII letters, digits and inner
/// dashes, at most 253 code units in total.
///
/// `start` must not be greater than the length of `x`.
pub fn is_valid_domain(start: usize, x: &str) -> bool {
    let units: Vec<u16> = x.encode_utf16().collect();
    let Some(length) = units.len().checked_sub(start) else {
        return false;
    };
    if !(1..=253).contains(&length) {
        return false;
    }
    let alnum = |c: u16| (b'a' as u16..=b'z' as u16).contains(&c) || is_digit(c);
    let dash = b'-' as u16;
    let mut index = start;
    while index < units.len() {
        let mut c = units[index];
        // A label must begin with a letter or a digit.
        if !alnum(c) {
            return false;
        }
        let mut d = c;
        loop {
            index += 1;
            if index >= units.len() {
                break;
            }
            c = units[index];
            // Subsequent characters may be letters, digits or the dash.
            if !alnum(c) && c != dash {
                break;
            }
            d = c;
        }
        if index >= units.len() {
            return d != dash;
        }
        // Labels must be separated by dots, and may not end with the dash.
        if c != b'.' as u16 || d == dash {
            return false;
        }
        index += 1;
    }
    // The domain ended in a dot.
    false
}

fn is_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}
