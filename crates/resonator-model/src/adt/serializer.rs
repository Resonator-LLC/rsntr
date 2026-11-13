//! Conversion of values to and from attribute strings (`Serializer`).

use super::AdtError;
use crate::id::{IdSerialiser, ModernIdSerialiser, WaveletId};
use crate::version::HashedVersion;

/// A value that can be stored in an attribute (`Serializer<T>`; the original
/// has one serializer object per type, here the type carries the
/// conversion).
pub trait Serializable: Sized {
    /// The attribute form of the value (`Serializer.toString`).
    fn to_attribute(&self) -> Result<String, AdtError>;

    /// The value of an attribute, or `None` if the string is not a value of
    /// this type (`Serializer.fromString`, which throws in that case).
    fn from_attribute(attribute: &str) -> Option<Self>;
}

/// `Serializer.STRING`.
impl Serializable for String {
    fn to_attribute(&self) -> Result<String, AdtError> {
        Ok(self.clone())
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        Some(attribute.to_owned())
    }
}

/// `Serializer.INTEGER`.
impl Serializable for i32 {
    fn to_attribute(&self) -> Result<String, AdtError> {
        Ok(self.to_string())
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        attribute.parse().ok()
    }
}

/// `Serializer.LONG`.
impl Serializable for i64 {
    fn to_attribute(&self) -> Result<String, AdtError> {
        Ok(self.to_string())
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        attribute.parse().ok()
    }
}

/// `Serializer.BOOLEAN`: anything but "true" in any letter case is false.
impl Serializable for bool {
    fn to_attribute(&self) -> Result<String, AdtError> {
        Ok(self.to_string())
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        Some(attribute.eq_ignore_ascii_case("true"))
    }
}

/// `Serializer.DOUBLE`, with the string forms of `Double.toString` and
/// `Double.parseDouble`.
impl Serializable for f64 {
    fn to_attribute(&self) -> Result<String, AdtError> {
        Ok(java_double_to_string(*self))
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        parse_java_double(attribute)
    }
}

/// `WaveletIdSerializer`: the modern form, `domain/id`.
impl Serializable for WaveletId {
    fn to_attribute(&self) -> Result<String, AdtError> {
        ModernIdSerialiser
            .serialise_wavelet_id(self)
            .map_err(|e| AdtError::Unserialisable(e.to_string()))
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        ModernIdSerialiser.deserialise_wavelet_id(attribute).ok()
    }
}

/// `HashedVersionSerializer`: `<version>:<base64 hash>`; when reading, the
/// colon and hash may be absent. Unlike the original, a negative version is
/// not a value.
impl Serializable for HashedVersion {
    fn to_attribute(&self) -> Result<String, AdtError> {
        Ok(self.to_string())
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        HashedVersion::from_string(attribute).ok()
    }
}

/// `Double.toString` (as specified since JDK 19): plain decimal notation
/// for magnitudes in `[1e-3, 1e7)`, otherwise "computerized scientific
/// notation" such as `1.0E10`, always with at least one digit after the
/// point, and with as few digits as identify the value.
pub fn java_double_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let magnitude = value.abs();
    if magnitude == 0.0 {
        return format!("{sign}0.0");
    }
    // Shortest digits and decimal exponent: d.ddd * 10^exponent. Where one
    // digit would do, Java takes the two-digit decimal closest to the value
    // (which matters for the smallest subnormal numbers: 4.9E-324).
    let mut scientific = format!("{magnitude:e}");
    if !scientific.contains('.') {
        scientific = format!("{magnitude:.1e}");
    }
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = match digits.trim_end_matches('0') {
        "" => "0",
        trimmed => trimmed,
    };
    if (-3..7).contains(&exponent) {
        let mut text = String::from(sign);
        if exponent < 0 {
            text.push_str("0.");
            text.extend(std::iter::repeat_n('0', (-exponent - 1) as usize));
            text.push_str(digits);
        } else {
            let point = exponent as usize + 1;
            if digits.len() <= point {
                text.push_str(digits);
                text.extend(std::iter::repeat_n('0', point - digits.len()));
                text.push_str(".0");
            } else {
                text.push_str(&digits[..point]);
                text.push('.');
                text.push_str(&digits[point..]);
            }
        }
        text
    } else {
        let fraction = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{sign}{}.{fraction}E{exponent}", &digits[..1])
    }
}

/// `Double.parseDouble`: surrounding whitespace is ignored; an optional
/// sign is followed by `NaN`, `Infinity`, a decimal number with optional
/// exponent, or a hexadecimal floating-point number, and an optional type
/// suffix `f`, `F`, `d` or `D`. `None` where the original throws.
pub fn parse_java_double(text: &str) -> Option<f64> {
    // String.trim removes everything up to U+0020 from both ends.
    let text = text.trim_matches(|c: char| c <= ' ');
    let (negative, body) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = match body {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        _ => {
            let body = body.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(body);
            if body.starts_with("0x") || body.starts_with("0X") {
                parse_hex_float(&body[2..])?
            } else {
                parse_decimal_float(body)?
            }
        }
    };
    Some(if negative { -magnitude } else { magnitude })
}

fn parse_decimal_float(body: &str) -> Option<f64> {
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(fraction) || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }
    if let Some(exponent) = exponent {
        let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if digits.is_empty() || !all_digits(digits) {
            return None;
        }
    }
    // Rust wants a digit on each side it is given; "1." and ".5" are fine
    // for Java.
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = if fraction.is_empty() { "0" } else { fraction };
    let exponent = exponent.map_or(String::new(), |e| format!("e{e}"));
    match format!("{whole}.{fraction}{exponent}").parse::<f64>() {
        Ok(value) => Some(value),
        // An exponent too large for Rust's parser: the value overflows or
        // underflows.
        Err(_) => {
            let zero = whole.bytes().chain(fraction.bytes()).all(|b| b == b'0');
            Some(if zero || exponent.starts_with("e-") {
                0.0
            } else {
                f64::INFINITY
            })
        }
    }
}

/// A hexadecimal floating-point number without its `0x`: hex digits with an
/// optional point, then a mandatory binary exponent `p<decimal>`.
fn parse_hex_float(body: &str) -> Option<f64> {
    let at = body.find(['p', 'P'])?;
    let (mantissa, exponent) = (&body[..at], &body[at + 1..]);
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let all_hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
    if !all_hex(whole) || !all_hex(fraction) || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }
    let exponent_digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    if exponent_digits.is_empty() || !exponent_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let exponent: i32 = exponent.parse().unwrap_or(if exponent.starts_with('-') {
        i32::MIN / 2
    } else {
        i32::MAX / 2
    });
    // Not correctly rounded for mantissas of more than 53 significant bits,
    // which is of no consequence for validation.
    let mut value = 0.0f64;
    for digit in whole.chars().chain(fraction.chars()) {
        value = value * 16.0 + f64::from(digit.to_digit(16)?);
    }
    let exponent = exponent.saturating_sub(4 * fraction.len() as i32);
    Some(value * 2.0f64.powi(exponent.clamp(-5000, 5000)))
}
