//! Delta signing at the wave server's seam: a signed applied delta keeps
//! a valid hash chain through serialisation, and its signature verifies;
//! without a signer nothing changes.

use std::sync::Arc;

use resonator_federation::signing::{Signer, VerifyError, verify_applied_delta};
use resonator_federation::{Ed25519DeltaSigner, ServerKey};
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{
    NO_TIMESTAMP, WaveletDelta, WaveletOperation, WaveletOperationContext,
};
use resonator_model::proto::federation::ProtocolWaveletDelta;
use resonator_model::proto::federation::protocol_signature::SignatureAlgorithm;
use resonator_model::proto::{
    ByteStringMessage, calculate_resulting_hashed_version, serialize_delta,
};
use resonator_model::version::{HashedVersion, HashedVersionFactory};
use resonator_server::clock::ManualClock;
use resonator_server::storage::{AppliedDelta, Database, DatabaseOptions};
use resonator_server::waveserver::{SharedDeltaSigner, WaveServer, WaveServerOptions};

const DOMAIN: &str = "a.example";

fn participant(address: &str) -> ParticipantId {
    ParticipantId::of(address).unwrap()
}

fn delta(
    author: &ParticipantId,
    target: &HashedVersion,
    ops: Vec<WaveletOperation>,
) -> ProtocolWaveletDelta {
    serialize_delta(&WaveletDelta::new(author.clone(), target.clone(), ops)).unwrap()
}

fn add(author: &ParticipantId, who: &ParticipantId) -> WaveletOperation {
    WaveletOperation::add_participant(
        WaveletOperationContext::new(author.clone(), NO_TIMESTAMP, 1),
        who.clone(),
    )
}

/// Applies the same three deltas on a fresh server and returns the applied
/// history from version zero.
async fn history(signer: Option<SharedDeltaSigner>) -> (Vec<AppliedDelta>, HashedVersion) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("wave.db"), DatabaseOptions::default()).unwrap();
    let options = WaveServerOptions {
        delta_signer: signer,
        ..WaveServerOptions::new(DOMAIN)
    };
    let server = WaveServer::with_clock(db, options, Arc::new(ManualClock::new(1_000_000)));
    let name = WaveletName::of_parts(DOMAIN, "w+1", DOMAIN, "conv+root").unwrap();
    let alice = participant("alice@a.example");
    let mut version = server.version_zero(&name);
    for who in ["alice@a.example", "bob@a.example", "carol@b.example"] {
        let result = server
            .submit_request_durably(
                &name,
                delta(&alice, &version, vec![add(&alice, &participant(who))]),
            )
            .await
            .unwrap();
        version = result.hashed_version_after_application;
    }
    let applied = server
        .get_applied_history(&name, &server.version_zero(&name), &version, None)
        .await
        .unwrap();
    server.database().close().await;
    (applied, version)
}

/// Follows the hash chain from version zero over serialised applied
/// deltas; returns the final version.
fn replay_chain(start: HashedVersion, applied: &[Vec<u8>]) -> HashedVersion {
    let mut version = start;
    for bytes in applied {
        let parsed = ByteStringMessage::parse_from(bytes.clone()).unwrap();
        assert_eq!(parsed.bytes(), bytes.as_slice(), "bytes kept as received");
        let next = calculate_resulting_hashed_version(&parsed).unwrap();
        assert_eq!(
            next,
            HashedVersionFactory::create(bytes, &version, 1),
            "each delta extends the chain from the previous version"
        );
        version = next;
    }
    version
}

#[tokio::test]
async fn signed_applied_deltas_keep_their_hash_chain_and_verify() {
    let key = ServerKey::generate();
    let signer = SharedDeltaSigner::new(Ed25519DeltaSigner::new(key.secret().clone()));
    let (applied, end) = history(Some(signer)).await;
    assert_eq!(applied.len(), 3);

    // Serialise as stored or sent, parse again: the chain from version
    // zero arrives at the server's version.
    let wire: Vec<Vec<u8>> = applied.iter().map(|delta| delta.bytes().to_vec()).collect();
    let name = WaveletName::of_parts(DOMAIN, "w+1", DOMAIN, "conv+root").unwrap();
    assert_eq!(
        replay_chain(HashedVersionFactory::create_version_zero(&name), &wire),
        end
    );

    let known = Signer {
        domain: DOMAIN.to_owned(),
        key: key.public(),
    };
    let lookup = |id: &[u8]| (id == key.signer_id().as_slice()).then(|| known.clone());
    for bytes in &wire {
        let parsed: AppliedDelta = ByteStringMessage::parse_from(bytes.clone()).unwrap();
        let signed = parsed.message().signed_original_delta.as_ref().unwrap();
        assert_eq!(signed.signature.len(), 1);
        let signature = &signed.signature[0];
        assert_eq!(
            signature.signature_algorithm,
            Some(SignatureAlgorithm::Ed25519 as i32)
        );
        assert_eq!(
            signature.signer_id.as_deref(),
            Some(key.signer_id().as_slice())
        );
        assert_eq!(signature.signature_bytes.as_ref().map(Vec::len), Some(64));
        let delta = verify_applied_delta(&parsed, lookup).unwrap();
        assert_eq!(delta.message().author.as_deref(), Some("alice@a.example"));
    }

    // Any change to the signed bytes breaks the signature, and the hash.
    let mut tampered: AppliedDelta = ByteStringMessage::parse_from(wire[1].clone()).unwrap();
    let mut message = tampered.message().clone();
    let signed = message.signed_original_delta.as_mut().unwrap();
    let mut inner: ProtocolWaveletDelta =
        resonator_model::proto::decode(signed.delta.as_deref().unwrap()).unwrap();
    inner.author = Some("mallory@a.example".to_owned());
    signed.delta = Some(resonator_model::proto::encode(&inner).unwrap());
    tampered = ByteStringMessage::serialize_message(message).unwrap();
    assert_eq!(
        verify_applied_delta(&tampered, lookup),
        Err(VerifyError::BadSignature(DOMAIN.to_owned()))
    );
    assert_ne!(
        calculate_resulting_hashed_version(&tampered).unwrap(),
        calculate_resulting_hashed_version(
            &ByteStringMessage::parse_from(wire[1].clone()).unwrap()
        )
        .unwrap()
    );
}

#[tokio::test]
async fn without_a_signer_applied_deltas_are_unsigned_and_unchanged() {
    let (unsigned, unsigned_end) = history(None).await;
    for delta in &unsigned {
        assert!(
            delta
                .message()
                .signed_original_delta
                .as_ref()
                .unwrap()
                .signature
                .is_empty()
        );
    }
    // Deterministic: same deltas, clock and wavelet give the same bytes
    // and hashes on another unsigned server.
    let (again, again_end) = history(None).await;
    assert_eq!(unsigned, again);
    assert_eq!(unsigned_end, again_end);
    // The first signed delta differs from the unsigned one only by its
    // signature (later ones also by the history hashes they target).
    let key = ServerKey::generate();
    let (signed, signed_end) = history(Some(SharedDeltaSigner::new(Ed25519DeltaSigner::new(
        key.secret().clone(),
    ))))
    .await;
    assert_ne!(signed_end, unsigned_end);
    let mut stripped = signed[0].message().clone();
    stripped
        .signed_original_delta
        .as_mut()
        .unwrap()
        .signature
        .clear();
    assert_eq!(&stripped, unsigned[0].message());
    assert_eq!(
        resonator_model::proto::encode(&stripped).unwrap(),
        unsigned[0].bytes()
    );
}
