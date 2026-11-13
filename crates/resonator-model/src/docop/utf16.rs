//! UTF-16 helpers.
//!
//! Text is stored as Rust `String`s, but every length, retain count, split
//! offset and key ordering in the operation model is defined in UTF-16 code
//! units, as in the Java original and the browser client.

use std::cmp::Ordering;

use super::error::OperationError;

/// Unicode REPLACEMENT CHARACTER.
pub const REPLACEMENT_CHARACTER: char = '\u{FFFD}';

/// Length of `s` in UTF-16 code units (Java `String.length()`).
pub fn utf16_len(s: &str) -> usize {
    // Fast path: ASCII strings have one unit per byte.
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().map(char::len_utf16).sum()
    }
}

/// Byte index corresponding to the UTF-16 offset `offset`.
///
/// Fails if the offset is past the end or inside a surrogate pair.
pub fn utf16_offset_to_byte(s: &str, offset: usize) -> Result<usize, OperationError> {
    if s.is_ascii() {
        return if offset <= s.len() {
            Ok(offset)
        } else {
            Err(OperationError::OffsetOutOfBounds {
                offset,
                len: s.len(),
            })
        };
    }
    let mut units = 0;
    for (byte, c) in s.char_indices() {
        if units == offset {
            return Ok(byte);
        }
        units += c.len_utf16();
        if units > offset {
            return Err(OperationError::SurrogateSplit { offset });
        }
    }
    if units == offset {
        Ok(s.len())
    } else {
        Err(OperationError::OffsetOutOfBounds { offset, len: units })
    }
}

/// Splits `s` at a UTF-16 offset (Java `substring(0, n)` / `substring(n)`).
///
/// Fails, rather than panicking, if the offset is out of bounds or lands
/// inside a surrogate pair.
pub fn split_at_utf16(s: &str, offset: usize) -> Result<(&str, &str), OperationError> {
    let byte = utf16_offset_to_byte(s, offset)?;
    Ok(s.split_at(byte))
}

/// Orders strings like Java `String.compareTo`, i.e. by UTF-16 code units.
///
/// This differs from Rust's byte order for strings that mix supplementary
/// characters with characters in U+E000..=U+FFFF. All key ordering (attribute
/// names, annotation keys) must go through this function.
pub fn cmp_utf16(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        a.cmp(b)
    } else {
        a.encode_utf16().cmp(b.encode_utf16())
    }
}

/// A string key ordered by [`cmp_utf16`], for use in ordered collections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Utf16Key(pub String);

impl Ord for Utf16Key {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_utf16(&self.0, &other.0)
    }
}

impl PartialOrd for Utf16Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}

pub fn is_low_surrogate(unit: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&unit)
}

pub fn is_supplementary_code_point(c: char) -> bool {
    c as u32 >= 0x10000
}

/// False for Unicode noncharacters.
pub fn is_code_point_valid(c: char) -> bool {
    let c = c as u32;
    let d = c & 0xFFFF;
    if d == 0xFFFE || d == 0xFFFF {
        return false;
    }
    !(0xFDD0..=0xFDEF).contains(&c)
}

/// Classification of a code point for use in blip text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlipCodePointResult {
    Ok,
    Control,
    Deprecated,
    Bidi,
    Tag,
    Noncharacter,
}

/// Whether a code point is acceptable in blip content (based on RFC 5198).
pub fn is_code_point_good_for_blip(c: char) -> BlipCodePointResult {
    if !is_code_point_valid(c) {
        return BlipCodePointResult::Noncharacter;
    }
    let c = c as u32;
    if c <= 0x1f || (0x7f..=0x9f).contains(&c) {
        return BlipCodePointResult::Control;
    }
    if (0x206A..=0x206F).contains(&c) {
        return BlipCodePointResult::Deprecated;
    }
    if c == 0x200E || c == 0x200F || (0x202A..=0x202E).contains(&c) {
        return BlipCodePointResult::Bidi;
    }
    if (0xE0000..=0xE007F).contains(&c) {
        return BlipCodePointResult::Tag;
    }
    BlipCodePointResult::Ok
}

/// XML `NameStartChar`, restricted to valid Unicode.
pub fn is_xml_name_start_char(ch: char) -> bool {
    let c = ch as u32;
    ch == ':'
        || ch.is_ascii_uppercase()
        || ch == '_'
        || ch.is_ascii_lowercase()
        || (0xC0..=0xD6).contains(&c)
        || (0xD8..=0xF6).contains(&c)
        || (0xF8..=0x2FF).contains(&c)
        || (0x370..=0x37D).contains(&c)
        || (0x37F..=0x1FFF).contains(&c)
        || (0x200C..=0x200D).contains(&c)
        || (0x2070..=0x218F).contains(&c)
        || (0x2C00..=0x2FEF).contains(&c)
        || (0x3001..=0xD7FF).contains(&c)
        || (0xF900..=0xFDCF).contains(&c)
        || (0xFDF0..=0xFFFD).contains(&c)
        || ((0x10000..=0xEFFFF).contains(&c) && is_code_point_valid(ch))
}

/// XML `NameChar`, restricted to valid Unicode.
pub fn is_xml_name_char(ch: char) -> bool {
    if !is_code_point_valid(ch) {
        return false;
    }
    let c = ch as u32;
    is_xml_name_start_char(ch)
        || ch == '-'
        || ch == '.'
        || ch.is_ascii_digit()
        || c == 0xB7
        || (0x0300..=0x036F).contains(&c)
        || (0x203F..=0x2040).contains(&c)
}

/// Whether `s` is an XML `Name`.
pub fn is_xml_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        None => false,
        Some(first) => is_xml_name_start_char(first) && chars.all(is_xml_name_char),
    }
}

/// Java `Utf16Util.isValidUtf16`: no noncharacters. Unpaired surrogates, the
/// other thing the original rejects, cannot occur in a Rust string.
pub fn is_valid_text(s: &str) -> bool {
    s.chars().all(is_code_point_valid)
}

/// Whether every code point of `s` is acceptable blip text.
pub fn is_good_for_blip(s: &str) -> bool {
    s.chars()
        .all(|c| is_code_point_good_for_blip(c) == BlipCodePointResult::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_utf16() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("é"), 1);
        assert_eq!(utf16_len("a😀b"), 4);
    }

    #[test]
    fn split_on_boundaries() {
        assert_eq!(split_at_utf16("abc", 1).unwrap(), ("a", "bc"));
        assert_eq!(split_at_utf16("abc", 0).unwrap(), ("", "abc"));
        assert_eq!(split_at_utf16("abc", 3).unwrap(), ("abc", ""));
        assert_eq!(split_at_utf16("a😀b", 1).unwrap(), ("a", "😀b"));
        assert_eq!(split_at_utf16("a😀b", 3).unwrap(), ("a😀", "b"));
        assert_eq!(split_at_utf16("é😀", 3).unwrap(), ("é😀", ""));
    }

    #[test]
    fn split_inside_surrogate_pair_is_an_error() {
        assert_eq!(
            split_at_utf16("a😀b", 2),
            Err(OperationError::SurrogateSplit { offset: 2 })
        );
    }

    #[test]
    fn split_out_of_bounds_is_an_error() {
        assert_eq!(
            split_at_utf16("abc", 4),
            Err(OperationError::OffsetOutOfBounds { offset: 4, len: 3 })
        );
        assert_eq!(
            split_at_utf16("a😀", 5),
            Err(OperationError::OffsetOutOfBounds { offset: 5, len: 3 })
        );
    }

    #[test]
    fn ordering_matches_java_compare_to() {
        assert_eq!(cmp_utf16("a", "b"), Ordering::Less);
        assert_eq!(cmp_utf16("ab", "a"), Ordering::Greater);
        assert_eq!(cmp_utf16("a", "a"), Ordering::Equal);
        // U+FF5E sorts after U+1F600 in UTF-16 (0xFF5E > 0xD83D) but before
        // it in UTF-8 / code point order.
        let bmp = "\u{FF5E}";
        let astral = "\u{1F600}";
        assert_eq!(bmp.cmp(astral), Ordering::Less);
        assert_eq!(cmp_utf16(bmp, astral), Ordering::Greater);
    }

    #[test]
    fn xml_names() {
        assert!(is_xml_name("body"));
        assert!(is_xml_name("_t"));
        assert!(is_xml_name("a-b.c9"));
        assert!(!is_xml_name(""));
        assert!(!is_xml_name("<"));
        assert!(!is_xml_name("9a"));
        assert!(!is_xml_name(".!$*.4!(,"));
    }

    #[test]
    fn noncharacters_are_invalid() {
        assert!(is_valid_text("a😀"));
        assert!(!is_valid_text("\u{FFFE}"));
        assert!(!is_valid_text("\u{FDD0}"));
        assert!(!is_valid_text("\u{1FFFF}"));
    }
}
