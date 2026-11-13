//! Signatures on the server's calls to robots.
//!
//! The original authenticated its calls to a robot not at all (and the
//! robot's calls to the server with OAuth 1). Here every request the
//! server makes to a robot's URL is signed, so that the robot can tell
//! the server's calls from anyone else's:
//!
//! ```text
//! X-Resonator-Robot: echo@example.com
//! X-Resonator-Timestamp: 1767225600
//! X-Resonator-Signature: v1=5f2c…(64 hex digits)
//! ```
//!
//! The signature is HMAC-SHA256, keyed with the robot's webhook secret
//! (the `webhook_secret` returned once when the robot is registered or
//! the secret is rotated; its bytes as written, prefix included), over
//!
//! ```text
//! "v1:" + <X-Resonator-Timestamp> + ":" + <the request body as sent>
//! ```
//!
//! written as lower-case hex after `v1=`. For the `GET` of
//! `capabilities.xml` the body is empty. The timestamp is in seconds
//! since the Unix epoch.
//!
//! To verify, a robot computes the same HMAC over the raw body bytes,
//! compares it in constant time, and rejects timestamps more than a few
//! minutes from its own clock, which bounds how long a captured request
//! can be replayed. [`verify`] does all of that. Delivery is at most
//! once; there are no retries and so no delivery ids to deduplicate.

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const ROBOT_HEADER: &str = "x-resonator-robot";
pub const TIMESTAMP_HEADER: &str = "x-resonator-timestamp";
pub const SIGNATURE_HEADER: &str = "x-resonator-signature";

/// The version prefix of the signature scheme.
pub const SCHEME: &str = "v1";

/// The prefix of webhook secrets.
pub const SECRET_PREFIX: &str = "wn_whsec_";

/// How far a timestamp may be from the verifier's clock, by default.
pub const DEFAULT_TOLERANCE_SECS: i64 = 300;

fn mac(secret: &str, timestamp: i64, body: &[u8]) -> Hmac<Sha256> {
    // HMAC accepts keys of any length.
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| Hmac::<Sha256>::new(&Default::default()));
    mac.update(SCHEME.as_bytes());
    mac.update(b":");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b":");
    mac.update(body);
    mac
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// The value of the signature header for a body sent at a time.
pub fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    format!(
        "{SCHEME}={}",
        hex(&mac(secret, timestamp, body).finalize().into_bytes())
    )
}

/// Whether a signature header is the server's for this body and
/// timestamp, and the timestamp is within `tolerance_secs` of `now`. The
/// comparison takes the same time whatever the signature.
pub fn verify(
    secret: &str,
    timestamp: i64,
    body: &[u8],
    signature: &str,
    now: i64,
    tolerance_secs: i64,
) -> bool {
    if (now - timestamp).abs() > tolerance_secs {
        return false;
    }
    let Some(given) = signature
        .trim()
        .strip_prefix(SCHEME)
        .and_then(|rest| rest.strip_prefix('='))
        .and_then(unhex)
    else {
        return false;
    };
    mac(secret, timestamp, body).verify_slice(&given).is_ok()
}

/// A new webhook secret: 256 random bits.
pub fn new_secret() -> Result<String, getrandom::Error> {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(format!(
        "{SECRET_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_match_a_known_vector() {
        // RFC 4231 test case 2 keys HMAC-SHA256 with "Jefe"; the message
        // here is what the scheme signs, checked against an independent
        // implementation (openssl dgst -sha256 -hmac Jefe).
        let signature = sign("Jefe", 1_700_000_000, b"{\"events\":[]}");
        assert_eq!(signature.len(), 3 + 64);
        assert!(signature.starts_with("v1="));
        assert_eq!(
            signature,
            format!(
                "v1={}",
                hex(&{
                    let mut mac = Hmac::<Sha256>::new_from_slice(b"Jefe").unwrap();
                    mac.update(b"v1:1700000000:{\"events\":[]}");
                    mac.finalize().into_bytes()
                })
            )
        );
        // The primitive itself, against RFC 4231 test case 2.
        let mut rfc = Hmac::<Sha256>::new_from_slice(b"Jefe").unwrap();
        rfc.update(b"what do ya want for nothing?");
        assert_eq!(
            hex(&rfc.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn verification_checks_everything() {
        let (secret, now, body) = ("wn_whsec_abc", 1_700_000_000, b"body".as_slice());
        let signature = sign(secret, now, body);
        assert!(verify(secret, now, body, &signature, now, 300));
        assert!(verify(secret, now, body, &signature, now + 300, 300));
        assert!(verify(secret, now, body, &signature, now - 300, 300));
        // Stale or from the future.
        assert!(!verify(secret, now, body, &signature, now + 301, 300));
        assert!(!verify(secret, now, body, &signature, now - 301, 300));
        // Another secret, body or timestamp.
        assert!(!verify("wn_whsec_abd", now, body, &signature, now, 300));
        assert!(!verify(secret, now, b"body2", &signature, now, 300));
        assert!(!verify(secret, now + 1, body, &signature, now, 300));
        // Malformed signatures.
        for bad in [
            "",
            "v1=",
            "v1=zz",
            "v2=00",
            &signature[3..],
            &signature[..signature.len() - 2],
            "v1=é",
        ] {
            assert!(!verify(secret, now, body, bad, now, 300), "{bad}");
        }
        // The empty body of a GET.
        let signature = sign(secret, now, b"");
        assert!(verify(secret, now, b"", &signature, now, 300));
        // Upper-case hex is accepted.
        let upper = format!("v1={}", signature[3..].to_uppercase());
        assert!(verify(secret, now, b"", &upper, now, 300));
    }

    #[test]
    fn secrets_are_random_and_marked() {
        let (a, b) = (new_secret().unwrap(), new_secret().unwrap());
        assert!(a.starts_with(SECRET_PREFIX));
        assert_eq!(a.len(), SECRET_PREFIX.len() + 43);
        assert_ne!(a, b);
    }
}
