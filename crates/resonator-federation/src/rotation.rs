//! Key rotation statements: a domain's old server key signing that a new
//! key replaces it. A peer presents them in its `Hello`; together with the
//! new key appearing in the domain's TXT record they let a pinned binding
//! move to the new key without an admin (see [`crate::trust`]).

use iroh::{PublicKey, SecretKey, Signature};
use resonator_model::proto::federation_rpc::{KeyRotation, SignedKeyRotation};
use resonator_model::proto::{decode, encode};
use thiserror::Error;

/// What a rotation signature covers, before the statement bytes; keeps
/// these signatures apart from delta signatures by the same key.
pub const ROTATION_SIGNATURE_CONTEXT: &[u8] = b"resonator/key-rotation/1\0";

/// A verified rotation statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    pub domain: String,
    pub old_key: PublicKey,
    pub new_key: PublicKey,
    /// Milliseconds since the epoch.
    pub at: i64,
}

/// A rotation statement that does not verify.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RotationError {
    #[error("malformed key rotation statement: {0}")]
    Malformed(String),
    #[error("key rotation statement is not signed by the old key")]
    BadSignature,
}

fn signed_bytes(statement: &[u8]) -> Vec<u8> {
    [ROTATION_SIGNATURE_CONTEXT, statement].concat()
}

fn public_key(bytes: &[u8], field: &str) -> Result<PublicKey, RotationError> {
    let bytes: &[u8; 32] = bytes
        .try_into()
        .map_err(|_| RotationError::Malformed(format!("{field} is not 32 bytes")))?;
    PublicKey::from_bytes(bytes)
        .map_err(|_| RotationError::Malformed(format!("{field} is not an Ed25519 key")))
}

/// Has `old` sign that `new` replaces it as the key of `domain`.
pub fn sign_rotation(old: &SecretKey, domain: &str, new: &PublicKey, at: i64) -> SignedKeyRotation {
    let statement = KeyRotation {
        domain: Some(domain.to_owned()),
        old_key: Some(old.public().as_bytes().to_vec()),
        new_key: Some(new.as_bytes().to_vec()),
        at: Some(at),
    };
    let statement = encode(&statement).expect("all required fields are set");
    let signature = old.sign(&signed_bytes(&statement));
    SignedKeyRotation {
        statement: Some(statement),
        signature: Some(signature.to_bytes().to_vec()),
    }
}

/// Checks that a rotation statement is well formed and signed by the key
/// it names as old.
pub fn verify_rotation(signed: &SignedKeyRotation) -> Result<Rotation, RotationError> {
    let malformed = |what: &str| RotationError::Malformed(what.to_owned());
    let statement = signed
        .statement
        .as_deref()
        .ok_or(malformed("no statement"))?;
    let signature = signed
        .signature
        .as_deref()
        .ok_or(malformed("no signature"))?;
    let rotation: KeyRotation =
        decode(statement).map_err(|error| RotationError::Malformed(error.to_string()))?;
    let old_key = public_key(rotation.old_key.as_deref().unwrap_or_default(), "old_key")?;
    let new_key = public_key(rotation.new_key.as_deref().unwrap_or_default(), "new_key")?;
    let signature: &[u8; 64] = signature
        .try_into()
        .map_err(|_| malformed("signature is not 64 bytes"))?;
    old_key
        .verify(&signed_bytes(statement), &Signature::from_bytes(signature))
        .map_err(|_| RotationError::BadSignature)?;
    Ok(Rotation {
        domain: rotation.domain.unwrap_or_default(),
        old_key,
        new_key,
        at: rotation.at.unwrap_or_default(),
    })
}

/// Whether one of `statements` validly rotates `domain` from `old` to
/// `new`.
pub fn find_rotation(
    statements: &[SignedKeyRotation],
    domain: &str,
    old: &PublicKey,
    new: &PublicKey,
) -> Option<Rotation> {
    statements
        .iter()
        .filter_map(|signed| verify_rotation(signed).ok())
        .find(|rotation| {
            rotation.domain == domain && rotation.old_key == *old && rotation.new_key == *new
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_verify_and_tampering_is_detected() {
        let old = SecretKey::generate();
        let new = SecretKey::generate().public();
        let signed = sign_rotation(&old, "example.com", &new, 1234);
        let rotation = verify_rotation(&signed).unwrap();
        assert_eq!(rotation.domain, "example.com");
        assert_eq!(rotation.old_key, old.public());
        assert_eq!(rotation.new_key, new);
        assert_eq!(rotation.at, 1234);
        assert!(
            find_rotation(
                std::slice::from_ref(&signed),
                "example.com",
                &old.public(),
                &new
            )
            .is_some()
        );
        assert!(
            find_rotation(
                std::slice::from_ref(&signed),
                "other.com",
                &old.public(),
                &new
            )
            .is_none()
        );
        assert!(
            find_rotation(
                std::slice::from_ref(&signed),
                "example.com",
                &new,
                &old.public()
            )
            .is_none()
        );

        // Signed by someone else than the old key.
        let mallory = SecretKey::generate();
        let mut forged = sign_rotation(&mallory, "example.com", &new, 1234);
        forged.statement = signed.statement.clone();
        assert_eq!(verify_rotation(&forged), Err(RotationError::BadSignature));

        let mut tampered = signed.clone();
        let mut statement = tampered.statement.take().unwrap();
        *statement.last_mut().unwrap() ^= 1;
        tampered.statement = Some(statement);
        assert!(verify_rotation(&tampered).is_err());

        let mut truncated = signed;
        truncated.signature = Some(vec![0; 10]);
        assert!(matches!(
            verify_rotation(&truncated),
            Err(RotationError::Malformed(_))
        ));
    }
}
