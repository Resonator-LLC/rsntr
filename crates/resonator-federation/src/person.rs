//! Person identity: a federation domain whose authority is a *person key*
//! rather than one server, with several *devices* (nodes) acting for it.
//!
//! - The person key is an Ed25519 key derived from a 24-word BIP39
//!   recovery phrase ([`PersonKey::from_phrase`]): the phrase's seed goes
//!   through HKDF-SHA256 with a resonator-specific info string. Only admin
//!   devices hold it, in a mode-600 file ([`FileKeyStore`]; the
//!   [`KeyStore`] trait lets mobile devices use the OS keychain instead).
//! - The person domain is the person key in z-base-32 (52 characters), or a
//!   DNS domain whose `_resonator` TXT record lists the person key
//!   (`k=<person key>`, the same record format as server keys).
//! - The person key signs a [`DeviceCert`] for each device key (the
//!   device's iroh endpoint key, which also signs its deltas) and
//!   [`RevocationList`]s withdrawing devices. A list's `seq` only grows and
//!   each list holds every device revoked so far.
//! - [`PersonBook`] is what a node knows of persons: certificates,
//!   revocation lists, devices configured as static peers, and which
//!   device to dial for a person domain. [`crate::trust::Trust`] consults
//!   it; [`PersonDevices`] is the epoch authority of person mode.
//!
//! Nothing here prints or logs secret material: [`RecoveryPhrase`] and
//! [`PersonKey`] redact themselves in `Debug`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use hkdf::Hkdf;
use iroh::{PublicKey, SecretKey, Signature};
use resonator_model::id::WaveletName;
use resonator_model::proto::federation_rpc::{
    DeviceCert, EpochStart, RevocationList, SignedDeviceCert, SignedRevocationList,
};
use resonator_model::proto::{decode, encode};
use resonator_server::storage::{DeviceCertRecord, FederationStore, RevocationRecord};
use sha2::Sha256;
use thiserror::Error;
use tokio::sync::Notify;
use zeroize::Zeroizing;

use crate::epoch::EpochAuthority;
use crate::key::{key_domain, parse_key_domain, to_hex, write_new_file};

/// What a device certificate's signature covers, before the certificate
/// bytes.
pub const DEVICE_CERT_CONTEXT: &[u8] = b"resonator/device-cert/1\0";
/// What a revocation list's signature covers, before the list bytes.
pub const REVOCATION_CONTEXT: &[u8] = b"resonator/revocations/1\0";
/// HKDF salt of the person key derivation.
const PERSON_KEY_SALT: &[u8] = b"resonator";
/// HKDF info of the person key derivation; a new derivation gets a new
/// version.
const PERSON_KEY_INFO: &[u8] = b"resonator/person-key/1";
/// Words of a recovery phrase (256 bits of entropy).
pub const PHRASE_WORDS: usize = 24;

/// Failures of person identity.
#[derive(Debug, Error)]
pub enum PersonError {
    #[error("not a valid {PHRASE_WORDS}-word recovery phrase: {0}")]
    Phrase(String),
    #[error("invalid device certificate: {0}")]
    Cert(String),
    #[error("invalid revocation list: {0}")]
    Revocations(String),
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0} already holds another person key; remove it first")]
    Exists(PathBuf),
    #[error(transparent)]
    Storage(#[from] resonator_server::storage::StorageError),
}

/// A 24-word BIP39 recovery phrase. Its `Debug` form is redacted and its
/// memory is wiped when dropped.
#[derive(Clone)]
pub struct RecoveryPhrase(Zeroizing<String>);

impl std::fmt::Debug for RecoveryPhrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryPhrase(<redacted>)")
    }
}

impl RecoveryPhrase {
    /// A new phrase from 32 random bytes.
    pub fn generate() -> Self {
        let mut entropy = Zeroizing::new([0u8; 32]);
        getrandom::fill(entropy.as_mut()).expect("the system random source failed");
        let mnemonic = bip39::Mnemonic::from_entropy(entropy.as_ref())
            .expect("32 bytes are valid BIP39 entropy");
        RecoveryPhrase(Zeroizing::new(mnemonic.to_string()))
    }

    /// Parses a phrase (words separated by any whitespace, any case); the
    /// BIP39 checksum must hold.
    pub fn parse(text: &str) -> Result<Self, PersonError> {
        let normalised = Zeroizing::new(
            text.split_whitespace()
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
                .join(" "),
        );
        let mnemonic = bip39::Mnemonic::parse_normalized(&normalised)
            .map_err(|error| PersonError::Phrase(error.to_string()))?;
        if mnemonic.word_count() != PHRASE_WORDS {
            return Err(PersonError::Phrase(format!(
                "it has {} words",
                mnemonic.word_count()
            )));
        }
        Ok(RecoveryPhrase(normalised))
    }

    /// The words, separated by single spaces. Show them to the person
    /// once; never log them.
    pub fn words(&self) -> &str {
        &self.0
    }

    /// The 32 bytes of BIP39 entropy the words encode (sent, sealed, to a
    /// new admin device when pairing).
    pub fn entropy(&self) -> Zeroizing<[u8; 32]> {
        let mnemonic = bip39::Mnemonic::parse_normalized(self.words())
            .expect("a RecoveryPhrase is a valid mnemonic");
        let mut entropy = Zeroizing::new([0u8; 32]);
        let bytes = Zeroizing::new(mnemonic.to_entropy());
        entropy.copy_from_slice(&bytes[..32]);
        entropy
    }

    /// The phrase of 32 bytes of entropy.
    pub fn from_entropy(entropy: &[u8; 32]) -> Self {
        let mnemonic =
            bip39::Mnemonic::from_entropy(entropy).expect("32 bytes are valid BIP39 entropy");
        RecoveryPhrase(Zeroizing::new(mnemonic.to_string()))
    }
}

/// A person key. Its `Debug` form shows only the public key.
#[derive(Clone)]
pub struct PersonKey {
    secret: SecretKey,
}

impl std::fmt::Debug for PersonKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonKey")
            .field("public", &self.domain())
            .finish_non_exhaustive()
    }
}

impl PersonKey {
    /// A new person key and the phrase it derives from.
    pub fn generate() -> (Self, RecoveryPhrase) {
        let phrase = RecoveryPhrase::generate();
        (Self::from_phrase(&phrase), phrase)
    }

    /// The key a phrase derives: HKDF-SHA256 over the BIP39 seed (empty
    /// passphrase), salt `"resonator"`, info `"resonator/person-key/1"`.
    pub fn from_phrase(phrase: &RecoveryPhrase) -> Self {
        let mnemonic = bip39::Mnemonic::parse_normalized(phrase.words())
            .expect("a RecoveryPhrase is a valid mnemonic");
        let seed = Zeroizing::new(mnemonic.to_seed(""));
        let hkdf = Hkdf::<Sha256>::new(Some(PERSON_KEY_SALT), seed.as_ref());
        let mut bytes = Zeroizing::new([0u8; 32]);
        hkdf.expand(PERSON_KEY_INFO, bytes.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        PersonKey {
            secret: SecretKey::from_bytes(&bytes),
        }
    }

    pub fn public(&self) -> PublicKey {
        self.secret.public()
    }

    /// The secret key, for signing the person's device directory
    /// ([`crate::directory`]). Never log it.
    pub(crate) fn secret(&self) -> &SecretKey {
        &self.secret
    }

    /// The person's key domain: the public key in z-base-32.
    pub fn domain(&self) -> String {
        key_domain(&self.public())
    }

    /// Certifies `device` as a device of this person.
    pub fn issue_cert(
        &self,
        device: &PublicKey,
        name: &str,
        admin: bool,
        issued_at: i64,
    ) -> SignedDeviceCert {
        let cert = DeviceCert {
            person: Some(self.public().as_bytes().to_vec()),
            device_key: Some(device.as_bytes().to_vec()),
            name: (!name.is_empty()).then(|| name.to_owned()),
            admin: Some(admin),
            issued_at: Some(issued_at),
        };
        let bytes = encode(&cert).unwrap_or_default();
        let signature = self.secret.sign(&[DEVICE_CERT_CONTEXT, &bytes].concat());
        SignedDeviceCert {
            cert: Some(bytes),
            signature: Some(signature.to_bytes().to_vec()),
        }
    }

    /// Lets the devices of `delegate_domain` hold this person's host lease
    /// until `until` (milliseconds; 0 withdraws an earlier grant).
    pub fn grant_co_host(
        &self,
        delegate_domain: &str,
        until: i64,
        issued_at: i64,
    ) -> resonator_model::proto::federation_rpc::SignedCoHost {
        let grant = resonator_model::proto::federation_rpc::CoHost {
            person: Some(self.public().as_bytes().to_vec()),
            delegate_domain: Some(delegate_domain.to_owned()),
            until: Some(until),
            issued_at: Some(issued_at),
        };
        let bytes = encode(&grant).unwrap_or_default();
        let signature = self
            .secret
            .sign(&[crate::cohost::CO_HOST_CONTEXT, &bytes].concat());
        resonator_model::proto::federation_rpc::SignedCoHost {
            grant: Some(bytes),
            signature: Some(signature.to_bytes().to_vec()),
        }
    }

    /// The revocation list following `previous` (of this person), with
    /// `devices` added: `seq` one higher, every device revoked so far.
    pub fn revoke(
        &self,
        previous: Option<&VerifiedRevocations>,
        devices: impl IntoIterator<Item = PublicKey>,
        issued_at: i64,
    ) -> SignedRevocationList {
        let previous = previous.filter(|list| list.person == self.public());
        let mut revoked: BTreeSet<[u8; 32]> = previous
            .map(|list| list.revoked.clone())
            .unwrap_or_default();
        revoked.extend(devices.into_iter().map(|key| *key.as_bytes()));
        let list = RevocationList {
            person: Some(self.public().as_bytes().to_vec()),
            seq: Some(previous.map_or(0, |list| list.seq) + 1),
            revoked: revoked.into_iter().map(|key| key.to_vec()).collect(),
            issued_at: Some(issued_at),
        };
        let bytes = encode(&list).unwrap_or_default();
        let signature = self.secret.sign(&[REVOCATION_CONTEXT, &bytes].concat());
        SignedRevocationList {
            list: Some(bytes),
            signature: Some(signature.to_bytes().to_vec()),
        }
    }
}

fn public_key(bytes: Option<&[u8]>) -> Option<PublicKey> {
    let bytes: &[u8; 32] = bytes?.try_into().ok()?;
    PublicKey::from_bytes(bytes).ok()
}

fn verify_signature(
    key: &PublicKey,
    context: &[u8],
    bytes: &[u8],
    signature: Option<&[u8]>,
) -> bool {
    let Some(signature) = signature.and_then(|s| <&[u8; 64]>::try_from(s).ok()) else {
        return false;
    };
    key.verify(
        &[context, bytes].concat(),
        &Signature::from_bytes(signature),
    )
    .is_ok()
}

/// A device certificate whose signature checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCert {
    pub person: PublicKey,
    pub device: PublicKey,
    pub name: String,
    pub admin: bool,
    pub issued_at: i64,
    /// As signed.
    pub signed: SignedDeviceCert,
}

/// Checks a device certificate's form and its signature by the person key
/// it names. Whether that person speaks for a domain is another question
/// ([`crate::trust::Trust`]).
pub fn verify_cert(signed: &SignedDeviceCert) -> Result<VerifiedCert, PersonError> {
    let bad = |message: &str| PersonError::Cert(message.to_owned());
    let bytes = signed.cert.as_deref().ok_or_else(|| bad("missing"))?;
    let cert: DeviceCert = decode(bytes).map_err(|e| PersonError::Cert(e.to_string()))?;
    let person = public_key(cert.person.as_deref()).ok_or_else(|| bad("no person key"))?;
    let device = public_key(cert.device_key.as_deref()).ok_or_else(|| bad("no device key"))?;
    if !verify_signature(
        &person,
        DEVICE_CERT_CONTEXT,
        bytes,
        signed.signature.as_deref(),
    ) {
        return Err(bad("the person key's signature does not verify"));
    }
    Ok(VerifiedCert {
        person,
        device,
        name: cert.name.unwrap_or_default(),
        admin: cert.admin.unwrap_or(false),
        issued_at: cert.issued_at.unwrap_or_default(),
        signed: signed.clone(),
    })
}

/// A revocation list whose signature checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRevocations {
    pub person: PublicKey,
    pub seq: u64,
    /// Raw device keys.
    pub revoked: BTreeSet<[u8; 32]>,
    pub issued_at: i64,
    /// As signed.
    pub signed: SignedRevocationList,
}

impl VerifiedRevocations {
    pub fn contains(&self, device: &PublicKey) -> bool {
        self.revoked.contains(device.as_bytes())
    }

    /// The revoked device keys.
    pub fn devices(&self) -> Vec<PublicKey> {
        self.revoked
            .iter()
            .filter_map(|bytes| PublicKey::from_bytes(bytes).ok())
            .collect()
    }
}

/// Checks a revocation list's form and its signature by the person key it
/// names.
pub fn verify_revocations(
    signed: &SignedRevocationList,
) -> Result<VerifiedRevocations, PersonError> {
    let bad = |message: &str| PersonError::Revocations(message.to_owned());
    let bytes = signed.list.as_deref().ok_or_else(|| bad("missing"))?;
    let list: RevocationList =
        decode(bytes).map_err(|e| PersonError::Revocations(e.to_string()))?;
    let person = public_key(list.person.as_deref()).ok_or_else(|| bad("no person key"))?;
    let seq = list.seq.unwrap_or(0);
    if seq == 0 {
        return Err(bad("seq 0"));
    }
    if !verify_signature(
        &person,
        REVOCATION_CONTEXT,
        bytes,
        signed.signature.as_deref(),
    ) {
        return Err(bad("the person key's signature does not verify"));
    }
    let mut revoked = BTreeSet::new();
    for key in &list.revoked {
        let key: [u8; 32] = key
            .as_slice()
            .try_into()
            .map_err(|_| bad("a revoked key is not 32 bytes"))?;
        revoked.insert(key);
    }
    Ok(VerifiedRevocations {
        person,
        seq,
        revoked,
        issued_at: list.issued_at.unwrap_or_default(),
        signed: signed.clone(),
    })
}

/// A signed device certificate as hex, for `person.cert` and
/// `device.cert` (it holds no secret).
pub fn cert_to_hex(cert: &SignedDeviceCert) -> String {
    to_hex(&encode(cert).unwrap_or_default())
}

/// Parses [`cert_to_hex`]'s output and checks the certificate.
pub fn cert_from_hex(text: &str) -> Result<VerifiedCert, PersonError> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(PersonError::Cert("not hex".to_owned()));
    }
    let bytes: Vec<u8> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap_or(0))
        .collect();
    let signed: SignedDeviceCert = decode(&bytes).map_err(|e| PersonError::Cert(e.to_string()))?;
    verify_cert(&signed)
}

/// Stores a verified certificate in the federation tables (the admin
/// commands; a running node does this through
/// [`crate::trust::Trust::learn_cert`]).
pub async fn save_cert(
    store: &FederationStore,
    cert: &VerifiedCert,
    now: i64,
) -> Result<(), PersonError> {
    store
        .put_device_cert(DeviceCertRecord {
            device_key: cert.device.as_bytes().to_vec(),
            person: cert.person.as_bytes().to_vec(),
            name: cert.name.clone(),
            admin: cert.admin,
            issued_at: cert.issued_at,
            cert: encode(&cert.signed).unwrap_or_default(),
            learned_at: now,
        })
        .await?;
    Ok(())
}

/// The stored certificates that verify.
pub async fn stored_certs(store: &FederationStore) -> Result<Vec<VerifiedCert>, PersonError> {
    Ok(store
        .device_certs()
        .await?
        .into_iter()
        .filter_map(|record| {
            let signed: SignedDeviceCert = decode(&record.cert).ok()?;
            verify_cert(&signed).ok()
        })
        .collect())
}

/// The stored revocation list of `person`, if one verifies.
pub async fn stored_revocations(
    store: &FederationStore,
    person: &PublicKey,
) -> Result<Option<VerifiedRevocations>, PersonError> {
    Ok(store
        .revocations()
        .await?
        .into_iter()
        .filter(|record| record.person == person.as_bytes())
        .find_map(|record| {
            let signed: SignedRevocationList = decode(&record.list).ok()?;
            verify_revocations(&signed).ok()
        }))
}

/// Stores a revocation list if it is newer than the stored one of its
/// person; returns whether it was. A running node reads it from there
/// and spreads it.
pub async fn save_revocations(
    store: &FederationStore,
    list: &VerifiedRevocations,
    now: i64,
) -> Result<bool, PersonError> {
    Ok(store
        .put_revocations(RevocationRecord {
            person: list.person.as_bytes().to_vec(),
            seq: list.seq,
            list: encode(&list.signed).unwrap_or_default(),
            learned_at: now,
        })
        .await?)
}

/// Where an admin device keeps the person key (as its recovery phrase).
pub trait KeyStore: Send + Sync {
    /// The stored phrase; `None` if there is none.
    fn load(&self) -> Result<Option<RecoveryPhrase>, PersonError>;
    /// Stores a phrase. Storing the phrase already stored is a no-op; a
    /// different one is refused ([`PersonError::Exists`]).
    fn store(&self, phrase: &RecoveryPhrase) -> Result<(), PersonError>;
}

/// A [`KeyStore`] in a file readable by its owner only (mode 600), holding
/// the phrase's words on one line.
#[derive(Debug, Clone)]
pub struct FileKeyStore {
    path: PathBuf,
}

impl FileKeyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileKeyStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self) -> Result<Option<RecoveryPhrase>, PersonError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => Zeroizing::new(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(PersonError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        crate::key::warn_if_readable_by_others(&self.path);
        RecoveryPhrase::parse(&text).map(Some)
    }

    fn store(&self, phrase: &RecoveryPhrase) -> Result<(), PersonError> {
        if let Some(current) = self.load()? {
            return if current.words() == phrase.words() {
                Ok(())
            } else {
                Err(PersonError::Exists(self.path.clone()))
            };
        }
        let contents = Zeroizing::new(format!("{}\n", phrase.words()));
        match write_new_file(&self.path, &contents) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(PersonError::Exists(self.path.clone()))
            }
            Err(source) => Err(PersonError::Write {
                path: self.path.clone(),
                source,
            }),
        }
    }
}

/// What a node knows of persons: certificates, revocation lists, which
/// person keys a DNS domain lists, devices configured as static peers,
/// and which device to dial for a person domain. Shared (in an `Arc`) by
/// the trust rules, the epoch authority and the federation tasks; filled
/// from storage at start and from Hellos, signer infos and `revocations`
/// requests as they come.
#[derive(Default)]
pub struct PersonBook {
    inner: RwLock<Book>,
    changed: Notify,
}

#[derive(Default)]
struct Book {
    /// This node's person (person mode).
    own: Option<PublicKey>,
    /// Certificates by device key, then person key.
    certs: HashMap<[u8; 32], BTreeMap<[u8; 32], VerifiedCert>>,
    /// The newest revocation list of each person.
    revocations: HashMap<[u8; 32], VerifiedRevocations>,
    /// Person keys that DNS domains' TXT records were seen to list.
    dns: HashMap<String, BTreeSet<[u8; 32]>>,
    /// Device keys configured for domains (static peers).
    devices: BTreeMap<String, Vec<PublicKey>>,
    /// The device to dial for a domain.
    routes: HashMap<String, PublicKey>,
}

impl std::fmt::Debug for PersonBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let book = self.read();
        f.debug_struct("PersonBook")
            .field("own", &book.own.map(|key| key.to_z32()))
            .field("devices", &book.certs.len())
            .field("revocations", &book.revocations.len())
            .finish_non_exhaustive()
    }
}

impl PersonBook {
    pub fn new() -> Arc<Self> {
        Arc::new(PersonBook::default())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Book> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Book> {
        self.inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Makes `person` this node's own person (person mode).
    pub fn set_own(&self, person: PublicKey) {
        self.write().own = Some(person);
    }

    /// This node's person, in person mode.
    pub fn own(&self) -> Option<PublicKey> {
        self.read().own
    }

    /// Records a verified certificate; returns whether it was new (or
    /// newer than the one recorded for the same device and person).
    pub fn add_cert(&self, cert: VerifiedCert) -> bool {
        let mut book = self.write();
        let by_person = book.certs.entry(*cert.device.as_bytes()).or_default();
        match by_person.get(cert.person.as_bytes()) {
            Some(known) if known.issued_at >= cert.issued_at => false,
            _ => {
                by_person.insert(*cert.person.as_bytes(), cert);
                true
            }
        }
    }

    /// The certificates of a device key.
    pub fn certs_of(&self, device: &PublicKey) -> Vec<VerifiedCert> {
        self.read()
            .certs
            .get(device.as_bytes())
            .map(|by_person| by_person.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The certificate of `device` by `person`, if any.
    pub fn cert(&self, device: &PublicKey, person: &PublicKey) -> Option<VerifiedCert> {
        self.read()
            .certs
            .get(device.as_bytes())?
            .get(person.as_bytes())
            .cloned()
    }

    /// Whether `device` has a certificate of some person that has not
    /// revoked it.
    pub fn has_unrevoked_cert(&self, device: &PublicKey) -> bool {
        let book = self.read();
        book.certs.get(device.as_bytes()).is_some_and(|by_person| {
            by_person
                .keys()
                .any(|person| !book.revoked_by(person, device))
        })
    }

    /// Records a newer revocation list. Returns `false`, changing nothing,
    /// for a list whose `seq` is not above the recorded one's (no
    /// rollback).
    pub fn learn_revocations(&self, list: VerifiedRevocations) -> bool {
        let mut book = self.write();
        let person = *list.person.as_bytes();
        if book
            .revocations
            .get(&person)
            .is_some_and(|known| known.seq >= list.seq)
        {
            return false;
        }
        let revoked = list.revoked.clone();
        book.routes
            .retain(|_, key| !revoked.contains(key.as_bytes()));
        book.revocations.insert(person, list);
        drop(book);
        self.changed.notify_one();
        true
    }

    /// The newest revocation list known of `person`.
    pub fn revocations(&self, person: &PublicKey) -> Option<VerifiedRevocations> {
        self.read().revocations.get(person.as_bytes()).cloned()
    }

    /// Every revocation list known.
    pub fn all_revocations(&self) -> Vec<VerifiedRevocations> {
        self.read().revocations.values().cloned().collect()
    }

    /// The `seq` of the newest revocation list known of `person` (0: none).
    pub fn revocation_seq(&self, person: &PublicKey) -> u64 {
        self.read()
            .revocations
            .get(person.as_bytes())
            .map_or(0, |list| list.seq)
    }

    /// Whether `person` revoked `device`.
    pub fn is_revoked_by(&self, person: &PublicKey, device: &PublicKey) -> bool {
        self.read().revoked_by(person.as_bytes(), device)
    }

    /// Whether a person that certified `device`, or this node's own
    /// person, revoked it.
    pub fn is_revoked(&self, device: &PublicKey) -> bool {
        let book = self.read();
        if let Some(own) = book.own
            && book.revoked_by(own.as_bytes(), device)
        {
            return true;
        }
        book.certs.get(device.as_bytes()).is_some_and(|by_person| {
            by_person
                .keys()
                .any(|person| book.revoked_by(person, device))
        })
    }

    /// Waits until a newer revocation list is recorded.
    pub async fn revocations_changed(&self) {
        self.changed.notified().await;
    }

    /// Notes that the TXT record of DNS domain `domain` lists `persons`.
    pub fn note_dns(&self, domain: &str, persons: &[PublicKey]) {
        self.write().dns.insert(
            domain.to_owned(),
            persons.iter().map(|key| *key.as_bytes()).collect(),
        );
    }

    /// Whether `person` was seen listed for DNS domain `domain`.
    pub fn dns_lists(&self, domain: &str, person: &PublicKey) -> bool {
        self.read()
            .dns
            .get(domain)
            .is_some_and(|persons| persons.contains(person.as_bytes()))
    }

    /// The persons known to speak for `domain` without a lookup: the key
    /// of a key domain, this node's person for `own_domain`, the persons
    /// DNS was seen to list.
    pub fn persons_of(&self, domain: &str, own_domain: Option<&str>) -> Vec<PublicKey> {
        if let Ok(Some(key)) = parse_key_domain(domain) {
            return vec![key];
        }
        let book = self.read();
        if own_domain == Some(domain) {
            return book.own.into_iter().collect();
        }
        book.dns
            .get(domain)
            .map(|persons| {
                persons
                    .iter()
                    .filter_map(|bytes| PublicKey::from_bytes(bytes).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Configures `device` as a device of `domain` (a static peer).
    pub fn add_static_device(&self, domain: &str, device: PublicKey) {
        let mut book = self.write();
        let devices = book.devices.entry(domain.to_owned()).or_default();
        if !devices.contains(&device) {
            devices.push(device);
        }
    }

    /// The device keys known for `domain`, not revoked: those configured
    /// for it, then those certified by a person of it (`persons`).
    pub fn devices_of(&self, domain: &str, persons: &[PublicKey]) -> Vec<PublicKey> {
        let book = self.read();
        let mut devices: Vec<PublicKey> = book.devices.get(domain).cloned().unwrap_or_default();
        for (device, by_person) in &book.certs {
            if persons
                .iter()
                .any(|person| by_person.contains_key(person.as_bytes()))
                && let Ok(key) = PublicKey::from_bytes(device)
                && !devices.contains(&key)
            {
                devices.push(key);
            }
        }
        devices.retain(|device| {
            !persons
                .iter()
                .any(|person| book.revoked_by(person.as_bytes(), device))
                && !book.certs.get(device.as_bytes()).is_some_and(|by_person| {
                    by_person
                        .keys()
                        .any(|person| book.revoked_by(person, device))
                })
        });
        devices
    }

    /// Every device key configured for any domain.
    pub fn static_devices(&self) -> Vec<PublicKey> {
        let book = self.read();
        let mut devices: Vec<PublicKey> = Vec::new();
        for key in book.devices.values().flatten() {
            if !devices.contains(key) {
                devices.push(*key);
            }
        }
        devices
    }

    /// Every device key with a certificate.
    pub fn certified_devices(&self) -> Vec<PublicKey> {
        self.read()
            .certs
            .keys()
            .filter_map(|bytes| PublicKey::from_bytes(bytes).ok())
            .collect()
    }

    /// Dials `device` for `domain` from now on.
    pub fn set_route(&self, domain: &str, device: PublicKey) {
        self.write().routes.insert(domain.to_owned(), device);
    }

    /// Sets the route of `domain` unless it has one.
    pub fn route_if_none(&self, domain: &str, device: PublicKey) {
        self.write()
            .routes
            .entry(domain.to_owned())
            .or_insert(device);
    }

    /// The device to dial for `domain`, if one is set.
    pub fn route(&self, domain: &str) -> Option<PublicKey> {
        self.read().routes.get(domain).copied()
    }
}

impl Book {
    fn revoked_by(&self, person: &[u8; 32], device: &PublicKey) -> bool {
        self.revocations
            .get(person)
            .is_some_and(|list| list.contains(device))
    }
}

/// The epoch authority of person mode: a wavelet's host role may move only
/// within its own domain (as [`crate::WaveletDomainOnly`]), to any device
/// of the domain's person that is not revoked. That the holder's key has a
/// certificate of the domain's person is checked with the rest of the
/// statement ([`crate::trust::Trust::verify_signer`]).
#[derive(Debug, Clone)]
pub struct PersonDevices {
    book: Arc<PersonBook>,
}

impl PersonDevices {
    pub fn new(book: Arc<PersonBook>) -> Self {
        PersonDevices { book }
    }
}

impl EpochAuthority for PersonDevices {
    fn may_host(&self, wavelet: &WaveletName, holder_domain: &str, statement: &EpochStart) -> bool {
        if wavelet.wavelet_id.domain() != holder_domain {
            return false;
        }
        let Some(holder) = public_key(statement.holder_key.as_deref()) else {
            return false;
        };
        !self.book.is_revoked(&holder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon \
                          abandon abandon abandon abandon abandon abandon abandon abandon \
                          abandon abandon abandon abandon abandon abandon abandon art";

    #[test]
    fn the_phrase_derives_the_same_key_every_time() {
        let phrase = RecoveryPhrase::parse(PHRASE).unwrap();
        let first = PersonKey::from_phrase(&phrase);
        let second =
            PersonKey::from_phrase(&RecoveryPhrase::parse(&PHRASE.to_uppercase()).unwrap());
        assert_eq!(first.public(), second.public());
        // Pinned: changing the derivation changes every person domain.
        assert_eq!(
            first.domain(),
            "5yrpbm9g4shdigkusopf8mcxra3fza3f51wbtm7y8azx9mo6jmyy"
        );
        let again = PersonKey::from_phrase(&phrase);
        assert_eq!(again.domain(), first.domain());

        let (generated, words) = PersonKey::generate();
        assert_eq!(words.words().split(' ').count(), PHRASE_WORDS);
        let recovered = PersonKey::from_phrase(&RecoveryPhrase::parse(words.words()).unwrap());
        assert_eq!(recovered.public(), generated.public());
        assert_ne!(generated.public(), first.public());
    }

    #[test]
    fn bad_phrases_are_refused_and_never_shown() {
        assert!(RecoveryPhrase::parse("abandon abandon").is_err());
        // Bad checksum.
        let bad = PHRASE.replace(" art", " abandon");
        assert!(RecoveryPhrase::parse(&bad).is_err());
        // 12 words are valid BIP39 but not a person phrase.
        let twelve = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon about";
        assert!(RecoveryPhrase::parse(twelve).is_err());
        let phrase = RecoveryPhrase::parse(PHRASE).unwrap();
        assert!(!format!("{phrase:?}").contains("abandon"));
        let key = PersonKey::from_phrase(&phrase);
        assert!(format!("{key:?}").contains(&key.domain()));
    }

    #[test]
    fn certificates_verify_and_forgeries_do_not() {
        let (person, _) = PersonKey::generate();
        let (other, _) = PersonKey::generate();
        let device = SecretKey::generate().public();
        let signed = person.issue_cert(&device, "laptop", true, 42);
        let cert = verify_cert(&signed).unwrap();
        assert_eq!(cert.person, person.public());
        assert_eq!(cert.device, device);
        assert_eq!(cert.name, "laptop");
        assert!(cert.admin);
        assert_eq!(cert.issued_at, 42);
        assert_eq!(cert_from_hex(&cert_to_hex(&signed)).unwrap(), cert);

        // Signed by another key than the person it names.
        let forged = other.issue_cert(&device, "laptop", true, 42);
        let mut statement: DeviceCert = decode(forged.cert.as_deref().unwrap()).unwrap();
        statement.person = Some(person.public().as_bytes().to_vec());
        let forged = SignedDeviceCert {
            cert: Some(encode(&statement).unwrap()),
            signature: forged.signature,
        };
        assert!(verify_cert(&forged).is_err());

        // Tampered with after signing.
        let mut statement: DeviceCert = decode(signed.cert.as_deref().unwrap()).unwrap();
        statement.admin = Some(false);
        let tampered = SignedDeviceCert {
            cert: Some(encode(&statement).unwrap()),
            signature: signed.signature.clone(),
        };
        assert!(verify_cert(&tampered).is_err());

        // A revocation list's signature is no certificate's.
        let list = person.revoke(None, [device], 1);
        let confused = SignedDeviceCert {
            cert: list.list.clone(),
            signature: list.signature.clone(),
        };
        assert!(verify_cert(&confused).is_err());
    }

    #[test]
    fn revocation_lists_grow_and_never_roll_back() {
        let (person, _) = PersonKey::generate();
        let (other, _) = PersonKey::generate();
        let first_device = SecretKey::generate().public();
        let second_device = SecretKey::generate().public();
        let first = verify_revocations(&person.revoke(None, [first_device], 1)).unwrap();
        assert_eq!(first.seq, 1);
        assert!(first.contains(&first_device));
        let second = verify_revocations(&person.revoke(Some(&first), [second_device], 2)).unwrap();
        assert_eq!(second.seq, 2);
        assert!(second.contains(&first_device) && second.contains(&second_device));

        // Another key's list about this person does not verify.
        let forged = other.revoke(None, [first_device], 1);
        let mut list: RevocationList = decode(forged.list.as_deref().unwrap()).unwrap();
        list.person = Some(person.public().as_bytes().to_vec());
        let forged = SignedRevocationList {
            list: Some(encode(&list).unwrap()),
            signature: forged.signature,
        };
        assert!(verify_revocations(&forged).is_err());

        let book = PersonBook::default();
        assert!(book.learn_revocations(second.clone()));
        // seq rollback: an older list changes nothing.
        assert!(!book.learn_revocations(first.clone()));
        assert!(!book.learn_revocations(second.clone()));
        assert_eq!(book.revocation_seq(&person.public()), 2);
        assert!(book.is_revoked_by(&person.public(), &second_device));
    }

    #[test]
    fn the_book_knows_devices_by_person_and_revocation() {
        let (person, _) = PersonKey::generate();
        let (other, _) = PersonKey::generate();
        let laptop = SecretKey::generate().public();
        let phone = SecretKey::generate().public();
        let book = PersonBook::default();
        assert!(
            book.add_cert(verify_cert(&person.issue_cert(&laptop, "laptop", true, 1)).unwrap())
        );
        assert!(book.add_cert(verify_cert(&person.issue_cert(&phone, "phone", false, 1)).unwrap()));
        // An older certificate does not replace a newer one.
        assert!(!book.add_cert(verify_cert(&person.issue_cert(&phone, "old", false, 0)).unwrap()));
        let domain = person.domain();
        assert_eq!(book.persons_of(&domain, None), vec![person.public()]);
        let devices = book.devices_of(&domain, &[person.public()]);
        assert!(devices.contains(&laptop) && devices.contains(&phone));
        // The wrong person's domain has none of them.
        assert!(
            book.devices_of(&other.domain(), &[other.public()])
                .is_empty()
        );
        assert!(book.has_unrevoked_cert(&phone));

        book.set_route(&domain, phone);
        let list = verify_revocations(&person.revoke(None, [phone], 5)).unwrap();
        assert!(book.learn_revocations(list));
        assert!(book.is_revoked(&phone));
        assert!(!book.is_revoked(&laptop));
        assert!(!book.has_unrevoked_cert(&phone));
        assert_eq!(book.devices_of(&domain, &[person.public()]), vec![laptop]);
        // Routes to a revoked device are dropped.
        assert_eq!(book.route(&domain), None);
    }

    #[test]
    fn person_devices_may_host_only_their_own_domain() {
        let (person, _) = PersonKey::generate();
        let device = SecretKey::generate().public();
        let book = PersonBook::new();
        let authority = PersonDevices::new(Arc::clone(&book));
        let domain = person.domain();
        let name = WaveletName::of_parts(&domain, "w+1", &domain, "conv+root").unwrap();
        let statement = EpochStart {
            holder_key: Some(device.as_bytes().to_vec()),
            holder_domain: Some(domain.clone()),
            ..EpochStart::default()
        };
        assert!(authority.may_host(&name, &domain, &statement));
        assert!(!authority.may_host(&name, "other.example", &statement));
        book.learn_revocations(verify_revocations(&person.revoke(None, [device], 1)).unwrap());
        book.add_cert(verify_cert(&person.issue_cert(&device, "", false, 1)).unwrap());
        assert!(!authority.may_host(&name, &domain, &statement));
    }

    #[test]
    fn the_key_file_holds_one_phrase_privately() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(dir.path().join("person.key"));
        assert!(store.load().unwrap().is_none());
        let phrase = RecoveryPhrase::parse(PHRASE).unwrap();
        store.store(&phrase).unwrap();
        store.store(&phrase).unwrap();
        assert_eq!(store.load().unwrap().unwrap().words(), phrase.words());
        assert!(matches!(
            store.store(&RecoveryPhrase::generate()),
            Err(PersonError::Exists(_))
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
