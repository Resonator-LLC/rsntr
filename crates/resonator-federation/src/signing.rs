//! Ed25519 delta signatures.
//!
//! A federating server signs every delta it applies on behalf of its users
//! with its server key ([`Ed25519DeltaSigner`], injected into the wave
//! server as its [`DeltaSigner`]). The signature covers the exact bytes of
//! the serialised `ProtocolWaveletDelta` (after a context prefix) and is
//! recorded in the `ProtocolSignedDelta` inside the applied delta, so it is
//! part of what the history hash covers.
//!
//! [`verify_delta`] checks a signed delta as `CertificateManagerImpl.verifyDelta`
//! does: one signature per distinct domain of the extended address path
//! (the address path, then the author), in that order, each by a signer of
//! that domain.

use iroh::{PublicKey, SecretKey, Signature};
use resonator_model::id::ParticipantId;
use resonator_model::proto::federation::protocol_signature::SignatureAlgorithm;
use resonator_model::proto::federation::protocol_signer_info::HashAlgorithm;
use resonator_model::proto::federation::{
    ProtocolAppliedWaveletDelta, ProtocolSignature, ProtocolSignedDelta, ProtocolSignerInfo,
    ProtocolWaveletDelta,
};
use resonator_model::proto::federation_rpc::SignedKeyRotation;
use resonator_model::proto::{ByteStringMessage, encode};
use resonator_server::storage::SignerInfoRecord;
use resonator_server::waveserver::DeltaSigner;
use thiserror::Error;

use crate::key::signer_id;

/// What a delta signature covers, before the delta bytes; keeps these
/// signatures apart from other signatures by the same key.
pub const DELTA_SIGNATURE_CONTEXT: &[u8] = b"resonator/delta/1\0";

fn signed_bytes(delta: &[u8]) -> Vec<u8> {
    [DELTA_SIGNATURE_CONTEXT, delta].concat()
}

/// Signs deltas with the server key.
pub struct Ed25519DeltaSigner {
    key: SecretKey,
    signer_id: Vec<u8>,
}

impl std::fmt::Debug for Ed25519DeltaSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ed25519DeltaSigner")
            .field("key", &self.key.public().to_z32())
            .finish_non_exhaustive()
    }
}

impl Ed25519DeltaSigner {
    pub fn new(key: SecretKey) -> Self {
        let signer_id = signer_id(&key.public());
        Ed25519DeltaSigner { key, signer_id }
    }

    /// The signature of `delta` (its serialised bytes).
    pub fn signature(&self, delta: &[u8]) -> ProtocolSignature {
        ProtocolSignature {
            signature_bytes: Some(self.key.sign(&signed_bytes(delta)).to_bytes().to_vec()),
            signer_id: Some(self.signer_id.clone()),
            signature_algorithm: Some(SignatureAlgorithm::Ed25519 as i32),
        }
    }
}

impl DeltaSigner for Ed25519DeltaSigner {
    fn sign(&self, delta: &ByteStringMessage<ProtocolWaveletDelta>) -> Vec<ProtocolSignature> {
        vec![self.signature(delta.bytes())]
    }
}

/// The signer info of an Ed25519 server key speaking for `domain`.
pub fn signer_info(
    key: &PublicKey,
    domain: &str,
    rotations: Vec<SignedKeyRotation>,
) -> ProtocolSignerInfo {
    ProtocolSignerInfo {
        hash_algorithm: Some(HashAlgorithm::Sha256 as i32),
        domain: Some(domain.to_owned()),
        certificate: Vec::new(),
        ed25519_public_key: Some(key.as_bytes().to_vec()),
        key_rotation: rotations,
        device_cert: None,
    }
}

/// The signer info of a device key signing for its person's `domain`
/// (person mode): [`signer_info`] with the device's certificate, by which
/// receivers check that the key speaks for the domain.
pub fn device_signer_info(
    key: &PublicKey,
    domain: &str,
    cert: resonator_model::proto::federation_rpc::SignedDeviceCert,
) -> ProtocolSignerInfo {
    ProtocolSignerInfo {
        device_cert: Some(cert),
        ..signer_info(key, domain, Vec::new())
    }
}

/// A signer: a key and the domain it signs for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    pub domain: String,
    pub key: PublicKey,
}

impl Signer {
    pub fn signer_id(&self) -> Vec<u8> {
        signer_id(&self.key)
    }
}

/// Why a delta or signer info does not verify (`SignatureException`,
/// `UnknownSignerException`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VerifyError {
    #[error("signed delta does not contain a valid delta: {0}")]
    InvalidDelta(String),
    #[error("found {domains} domains in extended address path, but {signatures} signatures")]
    SignatureCount { domains: usize, signatures: usize },
    #[error("unknown signer {}", hex(.0))]
    UnknownSigner(Vec<u8>),
    #[error("signature for domain {expected} is by a signer of {actual}")]
    WrongDomain { expected: String, actual: String },
    #[error("unsupported signature algorithm {0:?}")]
    UnsupportedAlgorithm(Option<i32>),
    #[error("signature for domain {0} does not verify")]
    BadSignature(String),
    #[error("invalid signer info: {0}")]
    InvalidSignerInfo(String),
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The distinct domains of a delta's extended address path: the address
/// path, then the author (`getParticipantDomains`).
pub fn signing_domains(delta: &ProtocolWaveletDelta) -> Result<Vec<String>, VerifyError> {
    let mut domains: Vec<String> = Vec::new();
    for address in delta.address_path.iter().chain(std::iter::once(
        delta.author.as_ref().unwrap_or(&String::new()),
    )) {
        let participant = ParticipantId::of(address)
            .map_err(|error| VerifyError::InvalidDelta(error.to_string()))?;
        if !domains.iter().any(|domain| domain == participant.domain()) {
            domains.push(participant.domain().to_owned());
        }
    }
    Ok(domains)
}

/// Verifies a signed delta and returns the delta, keeping its bytes.
/// `signer` maps a signer id to its signer, `None` if it is unknown (the
/// caller fetches unknown signer info first, as the original's
/// `prefetchDeltaSignerInfo` does, and checks that each signer's key is
/// bound to its domain when storing the signer info).
pub fn verify_delta(
    signed: &ProtocolSignedDelta,
    signer: impl Fn(&[u8]) -> Option<Signer>,
) -> Result<ByteStringMessage<ProtocolWaveletDelta>, VerifyError> {
    let bytes = signed
        .delta
        .as_deref()
        .ok_or_else(|| VerifyError::InvalidDelta("no delta".to_owned()))?;
    let delta = ByteStringMessage::<ProtocolWaveletDelta>::parse_from(bytes)
        .map_err(|error| VerifyError::InvalidDelta(error.to_string()))?;
    let domains = signing_domains(delta.message())?;
    if domains.len() != signed.signature.len() {
        return Err(VerifyError::SignatureCount {
            domains: domains.len(),
            signatures: signed.signature.len(),
        });
    }
    let message = signed_bytes(delta.bytes());
    for (domain, signature) in domains.iter().zip(&signed.signature) {
        if signature.signature_algorithm != Some(SignatureAlgorithm::Ed25519 as i32) {
            return Err(VerifyError::UnsupportedAlgorithm(
                signature.signature_algorithm,
            ));
        }
        let id = signature.signer_id.as_deref().unwrap_or_default();
        let found = signer(id).ok_or_else(|| VerifyError::UnknownSigner(id.to_vec()))?;
        if found.signer_id() != id {
            return Err(VerifyError::UnknownSigner(id.to_vec()));
        }
        if found.domain != *domain {
            return Err(VerifyError::WrongDomain {
                expected: domain.clone(),
                actual: found.domain,
            });
        }
        let bad = || VerifyError::BadSignature(domain.clone());
        let bytes: &[u8; 64] = signature
            .signature_bytes
            .as_deref()
            .unwrap_or_default()
            .try_into()
            .map_err(|_| bad())?;
        found
            .key
            .verify(&message, &Signature::from_bytes(bytes))
            .map_err(|_| bad())?;
    }
    Ok(delta)
}

/// Whether `signed` carries no signature and every domain of its extended
/// address path is `host_domain`: a delta the host applied for its own
/// users before it federated (a server signs only while federation is on).
/// A replica may take such a delta from its host's history: the host could
/// sign deltas of its own domain anyway, so this vouches for nothing the
/// host could not. Submissions to a host never get this allowance.
pub fn is_unsigned_host_delta(signed: &ProtocolSignedDelta, host_domain: &str) -> bool {
    if !signed.signature.is_empty() {
        return false;
    }
    let Some(bytes) = signed.delta.as_deref() else {
        return false;
    };
    let Ok(delta) = ByteStringMessage::<ProtocolWaveletDelta>::parse_from(bytes) else {
        return false;
    };
    signing_domains(delta.message())
        .is_ok_and(|domains| domains.iter().all(|domain| domain == host_domain))
}

/// [`verify_delta`] for the signed delta inside an applied delta.
pub fn verify_applied_delta(
    applied: &ByteStringMessage<ProtocolAppliedWaveletDelta>,
    signer: impl Fn(&[u8]) -> Option<Signer>,
) -> Result<ByteStringMessage<ProtocolWaveletDelta>, VerifyError> {
    let signed = applied
        .message()
        .signed_original_delta
        .as_ref()
        .ok_or_else(|| VerifyError::InvalidDelta("no signed delta".to_owned()))?;
    verify_delta(signed, signer)
}

/// Checks the form of an Ed25519 signer info and returns its signer id and
/// signer. Whether the key is bound to the domain is the caller's to check
/// ([`crate::trust::Trust::is_signer_bound`]).
pub fn parse_signer_info(info: &ProtocolSignerInfo) -> Result<(Vec<u8>, Signer), VerifyError> {
    let invalid = |message: &str| VerifyError::InvalidSignerInfo(message.to_owned());
    if info.hash_algorithm != Some(HashAlgorithm::Sha256 as i32) {
        return Err(invalid("hash algorithm is not SHA-256"));
    }
    let domain = info
        .domain
        .clone()
        .filter(|domain| resonator_model::id::is_valid_domain(0, domain))
        .ok_or_else(|| invalid("no valid domain"))?;
    let bytes: &[u8; 32] = info
        .ed25519_public_key
        .as_deref()
        .unwrap_or_default()
        .try_into()
        .map_err(|_| invalid("no 32-byte Ed25519 key"))?;
    let key = PublicKey::from_bytes(bytes).map_err(|_| invalid("not an Ed25519 key"))?;
    let signer = Signer { domain, key };
    Ok((signer.signer_id(), signer))
}

/// The `signer_info` row for an Ed25519 signer info.
pub fn signer_info_record(info: &ProtocolSignerInfo) -> Result<SignerInfoRecord, VerifyError> {
    let (signer_id, signer) = parse_signer_info(info)?;
    let info = encode(info).map_err(|error| VerifyError::InvalidSignerInfo(error.to_string()))?;
    Ok(SignerInfoRecord {
        signer_id,
        domain: signer.domain,
        key: signer.key.as_bytes().to_vec(),
        info,
    })
}

#[cfg(test)]
mod tests {
    use resonator_model::proto::federation::ProtocolHashedVersion;

    use super::*;

    fn delta(author: &str, address_path: &[&str]) -> ByteStringMessage<ProtocolWaveletDelta> {
        ByteStringMessage::serialize_message(ProtocolWaveletDelta {
            hashed_version: Some(ProtocolHashedVersion {
                version: Some(3),
                history_hash: Some(vec![1, 2, 3]),
            }),
            author: Some(author.to_owned()),
            operation: Vec::new(),
            address_path: address_path.iter().map(|a| (*a).to_owned()).collect(),
        })
        .unwrap()
    }

    fn signed(
        delta: &ByteStringMessage<ProtocolWaveletDelta>,
        signers: &[&Ed25519DeltaSigner],
    ) -> ProtocolSignedDelta {
        ProtocolSignedDelta {
            delta: Some(delta.bytes().to_vec()),
            signature: signers.iter().map(|s| s.signature(delta.bytes())).collect(),
        }
    }

    #[test]
    fn unsigned_deltas_pass_only_for_the_host_domain() {
        let own = delta("alice@a.example", &[]);
        assert!(is_unsigned_host_delta(&signed(&own, &[]), "a.example"));
        assert!(!is_unsigned_host_delta(&signed(&own, &[]), "b.example"));
        let foreign_path = delta("alice@a.example", &["bob@b.example"]);
        assert!(!is_unsigned_host_delta(
            &signed(&foreign_path, &[]),
            "a.example"
        ));
        let signer = Ed25519DeltaSigner::new(SecretKey::generate());
        assert!(!is_unsigned_host_delta(
            &signed(&own, &[&signer]),
            "a.example"
        ));
        let empty = ProtocolSignedDelta {
            delta: None,
            signature: Vec::new(),
        };
        assert!(!is_unsigned_host_delta(&empty, "a.example"));
    }

    #[test]
    fn signatures_verify_against_their_signer() {
        let key = SecretKey::generate();
        let signer = Ed25519DeltaSigner::new(key.clone());
        let known = Signer {
            domain: "a.example".to_owned(),
            key: key.public(),
        };
        let lookup = |id: &[u8]| (id == known.signer_id().as_slice()).then(|| known.clone());
        let delta = delta("alice@a.example", &[]);
        let signed_delta = signed(&delta, &[&signer]);
        assert_eq!(verify_delta(&signed_delta, lookup).unwrap(), delta);
        assert_eq!(
            signed_delta.signature[0].signer_id.as_deref(),
            Some(signer_id(&key.public()).as_slice())
        );

        // Unsigned, or signed for the wrong domain, or by an unknown key.
        let mut unsigned = signed_delta.clone();
        unsigned.signature.clear();
        assert_eq!(
            verify_delta(&unsigned, lookup),
            Err(VerifyError::SignatureCount {
                domains: 1,
                signatures: 0
            })
        );
        let other = signed(&self::delta("bob@b.example", &[]), &[&signer]);
        assert!(matches!(
            verify_delta(&other, lookup),
            Err(VerifyError::WrongDomain { .. })
        ));
        let stranger = Ed25519DeltaSigner::new(SecretKey::generate());
        assert!(matches!(
            verify_delta(&signed(&delta, &[&stranger]), lookup),
            Err(VerifyError::UnknownSigner(_))
        ));

        // Tampered bytes or signature.
        let mut tampered = signed_delta.clone();
        tampered.signature[0].signature_bytes.as_mut().unwrap()[0] ^= 1;
        assert_eq!(
            verify_delta(&tampered, lookup),
            Err(VerifyError::BadSignature("a.example".to_owned()))
        );
        let mut tampered = signed_delta.clone();
        tampered.delta = Some(self::delta("alice@a.example", &["carol@a.example"]).into_bytes());
        assert_eq!(
            verify_delta(&tampered, lookup),
            Err(VerifyError::BadSignature("a.example".to_owned()))
        );
        let mut wrong_algorithm = signed_delta;
        wrong_algorithm.signature[0].signature_algorithm = Some(SignatureAlgorithm::Sha1Rsa as i32);
        assert!(matches!(
            verify_delta(&wrong_algorithm, lookup),
            Err(VerifyError::UnsupportedAlgorithm(Some(1)))
        ));
    }

    #[test]
    fn one_signature_per_domain_of_the_extended_address_path() {
        let a = Ed25519DeltaSigner::new(SecretKey::generate());
        let b = Ed25519DeltaSigner::new(SecretKey::generate());
        let signers = [
            Signer {
                domain: "a.example".to_owned(),
                key: a.key.public(),
            },
            Signer {
                domain: "b.example".to_owned(),
                key: b.key.public(),
            },
        ];
        let lookup = |id: &[u8]| signers.iter().find(|s| s.signer_id() == id).cloned();
        // Address path first, then the author; duplicates count once.
        let delta = delta(
            "bob@b.example",
            &["alice@a.example", "anne@a.example", "bert@b.example"],
        );
        assert_eq!(
            signing_domains(delta.message()).unwrap(),
            ["a.example", "b.example"]
        );
        verify_delta(&signed(&delta, &[&a, &b]), lookup).unwrap();
        assert!(verify_delta(&signed(&delta, &[&b, &a]), lookup).is_err());
        assert!(matches!(
            verify_delta(&signed(&delta, &[&a]), lookup),
            Err(VerifyError::SignatureCount { .. })
        ));
    }

    #[test]
    fn signer_info_round_trips() {
        let key = SecretKey::generate().public();
        let info = signer_info(&key, "a.example", Vec::new());
        let (id, signer) = parse_signer_info(&info).unwrap();
        assert_eq!(id, signer_id(&key));
        assert_eq!(signer.domain, "a.example");
        assert_eq!(signer.key, key);
        let record = signer_info_record(&info).unwrap();
        assert_eq!(record.key, key.as_bytes());
        let reparsed: ProtocolSignerInfo = resonator_model::proto::decode(&record.info).unwrap();
        assert_eq!(reparsed, info);
        let mut broken = info.clone();
        broken.ed25519_public_key = Some(vec![1; 5]);
        assert!(parse_signer_info(&broken).is_err());
        let mut broken = info;
        broken.domain = Some("Not A Domain".to_owned());
        assert!(parse_signer_info(&broken).is_err());
    }
}
