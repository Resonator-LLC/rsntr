//! Person mode: pairing by a short code a person types, beside the link
//! of [`crate::pairing`].
//!
//! A code is `<nameplate>-<word>-<word>`, e.g. `482913-ocean-tiger`
//! ([`PairCode`]):
//! - the **nameplate** ([`NAMEPLATE_DIGITS`] random digits) is not
//!   secret; it only finds the inviting device;
//! - the **words** ([`CODE_WORDS`] from the BIP39 English list, 11 bits
//!   each) are the password of a PAKE.
//!
//! **Rendezvous.** The inviting device publishes a small signed record
//! (its node id, relay URL, socket addresses and the expiry) as a pkarr
//! signed packet under a key anyone can derive from the nameplate
//! ([`nameplate_key`]: HKDF-SHA256 of `"resonator/pair-nameplate/1\0"` ‖
//! nameplate, used as an Ed25519 seed), on a pkarr relay ([`Rendezvous`],
//! [`PkarrRendezvous`]; n0's by default). The new device derives the same
//! key, resolves the record and dials the inviting device on the pairing
//! ALPN. Since anyone can derive the key, anyone can overwrite the record:
//! that only makes the PAKE fail (a denial of service, not a way in).
//!
//! **Proof.** SPAKE2 (Ed25519 group) over the words: the joiner is side A,
//! the inviting device side B, and the identities bind the joiner's key
//! (the QUIC connection's), the inviting device's key and the nameplate
//! ([`identities`]). The PAKE's key gives the session key
//! ([`session_key`], also binding the invitation id, the person, the admin
//! flag and the account), which takes the place of the link's secret: the
//! joiner's MAC in `pair`, the inviting device's key confirmation
//! ([`inviter_confirm`]) and the pad sealing the person key of an admin
//! pairing. A low-entropy code is safe this way: a guess can only be
//! checked by running the protocol with the inviting device, which allows
//! [`MAX_CODE_FAILURES`] wrong ones per invitation.
//!
//! Never log a code, its words or a session key.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use iroh::{PublicKey, RelayUrl, SecretKey};
use iroh_dns::pkarr::SignedPacket;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

/// Digits of a nameplate.
pub const NAMEPLATE_DIGITS: usize = 6;
/// Words of a code.
pub const CODE_WORDS: usize = 2;
/// Wrong codes that burn an invitation.
pub const MAX_CODE_FAILURES: u32 = 3;
/// The TXT name of the rendezvous record inside its packet.
pub const RECORD_NAME: &str = "_resonator-pair";
/// The TTL of the rendezvous record (seconds); it holds its own expiry.
const RECORD_TTL: u32 = 30;
/// Most socket addresses a record carries (a packet holds 1000 bytes).
const MAX_RECORD_ADDRS: usize = 12;

const NAMEPLATE_CONTEXT: &[u8] = b"resonator/pair-nameplate/1\0";
const NAMEPLATE_INFO: &[u8] = b"ed25519 seed";
const JOINER_ID: &[u8] = b"resonator/pair-code/1 joiner\0";
const INVITER_ID: &[u8] = b"resonator/pair-code/1 inviter\0";
const SESSION_INFO: &[u8] = b"resonator/pair-code/1 session\0";
const CONFIRM_CONTEXT: &[u8] = b"resonator/pair-code/1 inviter\0";

/// Why a code was not understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error(
        "a pairing code is {NAMEPLATE_DIGITS} digits and {CODE_WORDS} words, like 482913-ocean-tiger"
    )]
    Shape,
    #[error("'{0}' is not one of the code words")]
    Word(String),
}

/// A pairing code: a nameplate and the words. Its `Debug` form leaves the
/// words out.
#[derive(Clone, PartialEq, Eq)]
pub struct PairCode {
    nameplate: String,
    /// Indexes into the BIP39 English list.
    words: [u16; CODE_WORDS],
}

impl std::fmt::Debug for PairCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairCode")
            .field("nameplate", &self.nameplate)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for PairCode {
    /// `<nameplate>-<word>-<word>`. Never log it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.nameplate, self.password_text().as_str())
    }
}

fn word_list() -> &'static [&'static str; 2048] {
    bip39::Language::English.word_list()
}

/// The BIP39 word `text` names: the word itself or, since the first four
/// letters of BIP39 words are unique, a prefix of at least four letters.
fn find_word(text: &str) -> Option<u16> {
    let language = bip39::Language::English;
    if let Some(index) = language.find_word(text) {
        return Some(index);
    }
    if text.chars().count() < 4 {
        return None;
    }
    match language.words_by_prefix(text) {
        [word] => language.find_word(word),
        _ => None,
    }
}

fn random_below(bound: u32) -> Result<u32, getrandom::Error> {
    // Rejection sampling: no bias towards small values.
    let zone = u32::MAX - (u32::MAX % bound);
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes)?;
        let value = u32::from_le_bytes(bytes);
        if value < zone {
            return Ok(value % bound);
        }
    }
}

/// A random nameplate of [`NAMEPLATE_DIGITS`] digits.
pub fn random_nameplate() -> Result<String, getrandom::Error> {
    let bound = 10u32.pow(NAMEPLATE_DIGITS as u32);
    Ok(format!(
        "{:0width$}",
        random_below(bound)?,
        width = NAMEPLATE_DIGITS
    ))
}

impl PairCode {
    /// A new code with a random nameplate and random words.
    pub fn generate() -> Result<PairCode, getrandom::Error> {
        let mut words = [0u16; CODE_WORDS];
        for word in &mut words {
            *word = u16::try_from(random_below(2048)?).unwrap_or_default();
        }
        Ok(PairCode {
            nameplate: random_nameplate()?,
            words,
        })
    }

    /// This code with another nameplate (the words stay).
    pub fn with_nameplate(&self, nameplate: String) -> PairCode {
        PairCode {
            nameplate,
            words: self.words,
        }
    }

    /// Reads a code as people type it: case does not matter, the parts
    /// may be separated by hyphens or spaces (or both), surrounding space
    /// is ignored, and a word may be given by its first four letters or
    /// more.
    pub fn parse(text: &str) -> Result<PairCode, CodeError> {
        let lower = text.trim().to_lowercase();
        let parts: Vec<&str> = lower
            .split(|c: char| c == '-' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .collect();
        let [nameplate, words @ ..] = parts.as_slice() else {
            return Err(CodeError::Shape);
        };
        if words.len() != CODE_WORDS
            || nameplate.len() != NAMEPLATE_DIGITS
            || !nameplate.chars().all(|c| c.is_ascii_digit())
        {
            return Err(CodeError::Shape);
        }
        let mut indexes = [0u16; CODE_WORDS];
        for (index, word) in indexes.iter_mut().zip(words) {
            *index = find_word(word).ok_or_else(|| CodeError::Word((*word).to_owned()))?;
        }
        Ok(PairCode {
            nameplate: (*nameplate).to_owned(),
            words: indexes,
        })
    }

    pub fn nameplate(&self) -> &str {
        &self.nameplate
    }

    /// The words, joined by hyphens: the PAKE password. Never log it.
    fn password_text(&self) -> Zeroizing<String> {
        let list = word_list();
        Zeroizing::new(
            self.words
                .iter()
                .map(|index| list[usize::from(*index) % list.len()])
                .collect::<Vec<_>>()
                .join("-"),
        )
    }

    /// The password bytes as stored with the invitation.
    pub fn password(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(self.password_text().as_bytes().to_vec())
    }
}

/// Whether `text` is meant as a code rather than a link.
pub fn is_code(text: &str) -> bool {
    !text.trim().starts_with(crate::pairing::PAIR_LINK_PREFIX)
}

/// The key of a nameplate's rendezvous record. Anyone can derive it.
pub fn nameplate_key(nameplate: &str) -> SecretKey {
    let ikm = [NAMEPLATE_CONTEXT, nameplate.as_bytes()].concat();
    let hkdf = Hkdf::<Sha256>::new(None, &ikm);
    let mut seed = Zeroizing::new([0u8; 32]);
    hkdf.expand(NAMEPLATE_INFO, seed.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    SecretKey::from_bytes(&seed)
}

/// What the rendezvous record of a nameplate says: how to reach the
/// inviting device, until when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendezvousRecord {
    pub node: PublicKey,
    pub relay: Option<RelayUrl>,
    pub addrs: Vec<SocketAddr>,
    /// Milliseconds since the epoch.
    pub expires_at: i64,
}

impl RendezvousRecord {
    /// The record as a pkarr packet signed with the nameplate's key.
    pub fn to_packet(&self, nameplate: &str) -> Result<SignedPacket, String> {
        let mut values = vec![
            "v=1".to_owned(),
            format!("node={}", self.node.to_z32()),
            format!("exp={}", self.expires_at),
        ];
        if let Some(relay) = &self.relay {
            values.push(format!("relay={relay}"));
        }
        values.extend(
            self.addrs
                .iter()
                .take(MAX_RECORD_ADDRS)
                .map(|addr| format!("addr={addr}")),
        );
        SignedPacket::from_txt_strings(&nameplate_key(nameplate), RECORD_NAME, values, RECORD_TTL)
            .map_err(|error| error.to_string())
    }

    /// The record a packet holds; `None` for an empty (withdrawn) or
    /// unreadable one.
    pub fn from_packet(packet: &SignedPacket) -> Option<RendezvousRecord> {
        let mut node = None;
        let mut relay = None;
        let mut addrs = Vec::new();
        let mut expires_at = None;
        let mut version = false;
        for value in packet.txt_records(RECORD_NAME) {
            let Some((key, value)) = value.split_once('=') else {
                continue;
            };
            match key {
                "v" => version = value == "1",
                "node" => node = crate::key::parse_key_domain(value).ok().flatten(),
                "exp" => expires_at = value.parse().ok(),
                "relay" => relay = value.parse().ok(),
                "addr" => addrs.extend(value.parse::<SocketAddr>().ok()),
                _ => {}
            }
        }
        if !version {
            return None;
        }
        Some(RendezvousRecord {
            node: node?,
            relay,
            addrs,
            expires_at: expires_at?,
        })
    }
}

/// Where rendezvous records are published and looked up: a pkarr relay
/// ([`PkarrRendezvous`]), or memory in tests ([`MemoryRendezvous`]).
#[async_trait]
pub trait Rendezvous: Send + Sync {
    /// Publishes a signed packet under its key.
    async fn publish(&self, packet: &SignedPacket) -> Result<(), String>;
    /// The packet published under `key`, if any (its signature checked).
    async fn resolve(&self, key: &PublicKey) -> Result<Option<SignedPacket>, String>;
}

/// The record published for `nameplate`, if one is there and has not
/// expired.
pub async fn lookup(
    rendezvous: &dyn Rendezvous,
    nameplate: &str,
    now: i64,
) -> Result<Option<RendezvousRecord>, String> {
    let key = nameplate_key(nameplate).public();
    Ok(rendezvous
        .resolve(&key)
        .await?
        .as_ref()
        .and_then(RendezvousRecord::from_packet)
        .filter(|record| record.expires_at > now))
}

/// Publishes the record of `nameplate`.
pub async fn publish(
    rendezvous: &dyn Rendezvous,
    nameplate: &str,
    record: &RendezvousRecord,
) -> Result<(), String> {
    rendezvous.publish(&record.to_packet(nameplate)?).await
}

/// Replaces the record of `nameplate` with an empty one.
pub async fn withdraw(rendezvous: &dyn Rendezvous, nameplate: &str) -> Result<(), String> {
    let packet = SignedPacket::from_txt_strings(
        &nameplate_key(nameplate),
        RECORD_NAME,
        std::iter::empty::<&str>(),
        RECORD_TTL,
    )
    .map_err(|error| error.to_string())?;
    rendezvous.publish(&packet).await
}

/// A pkarr relay over HTTP (`PUT`/`GET <relay>/<z32 key>`, the body
/// being the signature, the timestamp and the DNS packet), as iroh's
/// address lookup uses.
#[derive(Debug, Clone)]
pub struct PkarrRendezvous {
    url: String,
    http: reqwest::Client,
}

impl PkarrRendezvous {
    pub fn new(url: &str) -> Result<PkarrRendezvous, String> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(PkarrRendezvous {
            url: url.trim_end_matches('/').to_owned(),
            http,
        })
    }

    fn url_of(&self, key: &PublicKey) -> String {
        format!("{}/{}", self.url, key.to_z32())
    }
}

#[async_trait]
impl Rendezvous for PkarrRendezvous {
    async fn publish(&self, packet: &SignedPacket) -> Result<(), String> {
        let response = self
            .http
            .put(self.url_of(&packet.public_key()))
            .body(packet.to_relay_payload())
            .send()
            .await
            .map_err(|error| format!("the pairing rendezvous cannot be reached: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "the pairing rendezvous refused the record: {}",
                response.status()
            ));
        }
        Ok(())
    }

    async fn resolve(&self, key: &PublicKey) -> Result<Option<SignedPacket>, String> {
        let response = self
            .http
            .get(self.url_of(key))
            .send()
            .await
            .map_err(|error| format!("the pairing rendezvous cannot be reached: {error}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!(
                "the pairing rendezvous answered {}",
                response.status()
            ));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("the pairing rendezvous's answer broke off: {error}"))?;
        SignedPacket::from_relay_payload(key, &body)
            .map(Some)
            .map_err(|error| format!("the rendezvous record does not verify: {error}"))
    }
}

/// Rendezvous records in memory, shared by the nodes of one test.
#[derive(Debug, Default)]
pub struct MemoryRendezvous {
    packets: Mutex<HashMap<PublicKey, SignedPacket>>,
}

impl MemoryRendezvous {
    pub fn new() -> MemoryRendezvous {
        MemoryRendezvous::default()
    }

    /// How many keys have a packet (an empty one included).
    pub fn len(&self) -> usize {
        self.packets.lock().map_or(0, |packets| packets.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl Rendezvous for MemoryRendezvous {
    async fn publish(&self, packet: &SignedPacket) -> Result<(), String> {
        let packet = SignedPacket::from_bytes(packet.as_bytes()).map_err(|e| e.to_string())?;
        let mut packets = self.packets.lock().map_err(|e| e.to_string())?;
        let newer = packets
            .get(&packet.public_key())
            .is_none_or(|known| packet.more_recent_than(known));
        if newer {
            packets.insert(packet.public_key(), packet);
        }
        Ok(())
    }

    async fn resolve(&self, key: &PublicKey) -> Result<Option<SignedPacket>, String> {
        Ok(self
            .packets
            .lock()
            .map_err(|e| e.to_string())?
            .get(key)
            .cloned())
    }
}

/// The rendezvous `url` names (see `federation.pairing_rendezvous`).
pub fn rendezvous_from_url(url: Option<String>) -> Option<std::sync::Arc<dyn Rendezvous>> {
    let url = url?;
    match PkarrRendezvous::new(&url) {
        Ok(rendezvous) => Some(std::sync::Arc::new(rendezvous)),
        Err(error) => {
            tracing::warn!(%error, "pairing by code is off: no HTTP client for the rendezvous");
            None
        }
    }
}

/// The SPAKE2 identities: the joiner's (side A) and the inviting
/// device's (side B), each binding its key and the nameplate.
fn identities(nameplate: &str, joiner: &PublicKey, inviter: &PublicKey) -> (Identity, Identity) {
    let side = |context: &[u8], key: &PublicKey| {
        Identity::new(&[context, nameplate.as_bytes(), b"\0", key.as_bytes()].concat())
    };
    (side(JOINER_ID, joiner), side(INVITER_ID, inviter))
}

/// The joiner's start of the PAKE: its state and its message.
pub(crate) fn pake_start(
    code: &PairCode,
    joiner: &PublicKey,
    inviter: &PublicKey,
) -> (Spake2<Ed25519Group>, Vec<u8>) {
    let (a, b) = identities(&code.nameplate, joiner, inviter);
    let password = code.password();
    Spake2::<Ed25519Group>::start_a(&Password::new(password.as_slice()), &a, &b)
}

/// The joiner's end of the PAKE: the shared key.
pub(crate) fn pake_finish(
    state: Spake2<Ed25519Group>,
    answer: &[u8],
) -> Result<Zeroizing<Vec<u8>>, String> {
    state
        .finish(answer)
        .map(Zeroizing::new)
        .map_err(|error| format!("{error:?}"))
}

/// The inviting device's side of the PAKE, given the joiner's message:
/// its own message and the shared key.
pub(crate) fn pake_answer(
    password: &[u8],
    nameplate: &str,
    joiner: &PublicKey,
    inviter: &PublicKey,
    message: &[u8],
) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), String> {
    let (a, b) = identities(nameplate, joiner, inviter);
    let (state, answer) = Spake2::<Ed25519Group>::start_b(&Password::new(password), &a, &b);
    let key = state
        .finish(message)
        .map_err(|error| format!("{error:?}"))?;
    Ok((answer, Zeroizing::new(key)))
}

/// What the session key binds besides the PAKE's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionContext<'a> {
    pub id: &'a str,
    pub person: &'a PublicKey,
    pub admin: bool,
    pub account: &'a str,
}

/// The session key: in place of the link's secret for the MACs and the
/// sealing of the person key.
pub(crate) fn session_key(pake_key: &[u8], context: &SessionContext<'_>) -> Zeroizing<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(None, pake_key);
    let length = |text: &str| u32::try_from(text.len()).unwrap_or(u32::MAX).to_be_bytes();
    let info = [
        SESSION_INFO,
        &length(context.id),
        context.id.as_bytes(),
        context.person.as_bytes(),
        &[u8::from(context.admin)],
        &length(context.account),
        context.account.as_bytes(),
    ]
    .concat();
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf.expand(&info, key.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// The inviting device's key confirmation.
pub(crate) fn inviter_confirm(
    session: &[u8],
    inviter: &PublicKey,
    joiner: &PublicKey,
    id: &str,
) -> Hmac<Sha256> {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(session).expect("HMAC takes any key length");
    mac.update(CONFIRM_CONTEXT);
    mac.update(inviter.as_bytes());
    mac.update(joiner.as_bytes());
    mac.update(id.as_bytes());
    mac
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicKey {
        SecretKey::from_bytes(&[byte; 32]).public()
    }

    fn code(text: &str) -> PairCode {
        PairCode::parse(text).unwrap()
    }

    #[test]
    fn codes_are_generated_in_shape_and_read_back() {
        for _ in 0..50 {
            let made = PairCode::generate().unwrap();
            let text = made.to_string();
            let parts: Vec<&str> = text.split('-').collect();
            assert_eq!(parts.len(), 1 + CODE_WORDS, "{text}");
            assert_eq!(parts[0].len(), NAMEPLATE_DIGITS);
            assert!(parts[0].chars().all(|c| c.is_ascii_digit()));
            assert_eq!(PairCode::parse(&text).unwrap(), made);
        }
        assert!(!format!("{:?}", code("482913-ocean-tiger")).contains("ocean"));
    }

    #[test]
    fn codes_are_read_as_people_type_them() {
        let canonical = code("482913-ocean-tiger");
        assert_eq!(canonical.to_string(), "482913-ocean-tiger");
        assert_eq!(canonical.nameplate(), "482913");
        for typed in [
            "482913-OCEAN-Tiger",
            "  482913 ocean tiger\n",
            "482913 - ocean - tiger",
            "482913--ocean  tiger",
            "482913-ocea-tige",
            "482913 oceAN-tigE ",
        ] {
            assert_eq!(code(typed), canonical, "{typed}");
        }
        assert_eq!(code("000042-abandon-zoo").nameplate(), "000042");
    }

    #[test]
    fn malformed_codes_and_bad_words_are_refused() {
        for bad in [
            "",
            "482913",
            "482913-ocean",
            "482913-ocean-tiger-apple",
            "48291-ocean-tiger",
            "4829135-ocean-tiger",
            "48291x-ocean-tiger",
            "ocean-tiger-482913",
            "resonator:pair/abc",
        ] {
            assert_eq!(PairCode::parse(bad), Err(CodeError::Shape), "{bad}");
        }
        assert_eq!(
            PairCode::parse("482913-ocean-tigr"),
            Err(CodeError::Word("tigr".to_owned()))
        );
        // Too short a prefix to name one word.
        assert_eq!(
            PairCode::parse("482913-oce-tiger"),
            Err(CodeError::Word("oce".to_owned()))
        );
        assert!(is_code("482913-ocean-tiger"));
        assert!(!is_code(" resonator:pair/abc"));
    }

    #[test]
    fn nameplate_keys_are_stable_and_distinct() {
        let key = nameplate_key("482913");
        assert_eq!(key.to_bytes(), nameplate_key("482913").to_bytes());
        assert_ne!(key.public(), nameplate_key("482914").public());
        // Pinned: the derivation must not change between versions, or old
        // and new devices would not find each other.
        assert_eq!(
            key.public().to_z32(),
            nameplate_key("482913").public().to_z32()
        );
        let pinned = {
            let hkdf = Hkdf::<Sha256>::new(None, b"resonator/pair-nameplate/1\x00482913");
            let mut seed = [0u8; 32];
            hkdf.expand(b"ed25519 seed", &mut seed).unwrap();
            SecretKey::from_bytes(&seed).public()
        };
        assert_eq!(key.public(), pinned);
    }

    #[test]
    fn records_round_trip_as_signed_packets() {
        let record = RendezvousRecord {
            node: key(7),
            relay: Some("https://relay.example.com".parse().unwrap()),
            addrs: vec![
                "127.0.0.1:4000".parse().unwrap(),
                "[::1]:4001".parse().unwrap(),
            ],
            expires_at: 1_700_000_000_000,
        };
        let packet = record.to_packet("482913").unwrap();
        assert_eq!(packet.public_key(), nameplate_key("482913").public());
        let again =
            SignedPacket::from_relay_payload(&packet.public_key(), &packet.to_relay_payload())
                .unwrap();
        assert_eq!(RendezvousRecord::from_packet(&again), Some(record));
    }

    #[tokio::test]
    async fn memory_rendezvous_keeps_the_newest_and_withdraws() {
        let rendezvous = MemoryRendezvous::new();
        let record = RendezvousRecord {
            node: key(7),
            relay: None,
            addrs: vec!["127.0.0.1:4000".parse().unwrap()],
            expires_at: 2_000,
        };
        publish(&rendezvous, "111111", &record).await.unwrap();
        assert_eq!(
            lookup(&rendezvous, "111111", 1_000).await.unwrap(),
            Some(record.clone())
        );
        // Expired, or another nameplate: nothing.
        assert_eq!(lookup(&rendezvous, "111111", 3_000).await.unwrap(), None);
        assert_eq!(lookup(&rendezvous, "222222", 1_000).await.unwrap(), None);
        withdraw(&rendezvous, "111111").await.unwrap();
        assert_eq!(lookup(&rendezvous, "111111", 1_000).await.unwrap(), None);
    }

    /// A pkarr relay in miniature: `PUT /<key>` keeps the body, `GET
    /// /<key>` gives it back or 404 (one request per connection).
    async fn fake_relay() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store: std::sync::Arc<Mutex<HashMap<String, Vec<u8>>>> = Default::default();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let store = store.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let (head, body_start) = loop {
                        let read = socket.read(&mut chunk).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            break (String::from_utf8_lossy(&buffer[..end]).to_string(), end + 4);
                        }
                    };
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    while buffer.len() < body_start + length {
                        let read = socket.read(&mut chunk).await.unwrap();
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let body = buffer[body_start..body_start + length].to_vec();
                    let mut words = head.split_whitespace();
                    let (method, path) = (words.next().unwrap(), words.next().unwrap());
                    let answer = match method {
                        "PUT" => {
                            store.lock().unwrap().insert(path.to_owned(), body);
                            (200, Vec::new())
                        }
                        _ => match store.lock().unwrap().get(path) {
                            Some(body) => (200, body.clone()),
                            None => (404, Vec::new()),
                        },
                    };
                    let head = format!(
                        "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        answer.0,
                        answer.1.len()
                    );
                    socket.write_all(head.as_bytes()).await.unwrap();
                    socket.write_all(&answer.1).await.unwrap();
                });
            }
        });
        format!("http://{addr}/pkarr/")
    }

    #[tokio::test]
    async fn records_go_through_a_pkarr_relay_over_http() {
        let rendezvous = PkarrRendezvous::new(&fake_relay().await).unwrap();
        let record = RendezvousRecord {
            node: key(7),
            relay: Some("https://relay.example.com".parse().unwrap()),
            addrs: vec!["192.0.2.1:4000".parse().unwrap()],
            expires_at: i64::MAX,
        };
        assert_eq!(lookup(&rendezvous, "333333", 0).await.unwrap(), None);
        publish(&rendezvous, "333333", &record).await.unwrap();
        assert_eq!(
            lookup(&rendezvous, "333333", 0).await.unwrap(),
            Some(record)
        );
        withdraw(&rendezvous, "333333").await.unwrap();
        assert_eq!(lookup(&rendezvous, "333333", 0).await.unwrap(), None);
        // Nothing listens: an error, not "not found".
        let gone = PkarrRendezvous::new("http://127.0.0.1:9/pkarr").unwrap();
        assert!(lookup(&gone, "333333", 0).await.is_err());
    }

    /// Against n0's public relay; run by hand:
    /// `cargo test -p resonator-federation -- --ignored n0_relay`.
    #[tokio::test]
    #[ignore = "needs the internet"]
    async fn records_go_through_n0_relay() {
        let rendezvous = PkarrRendezvous::new("https://dns.iroh.link/pkarr").unwrap();
        let nameplate = random_nameplate().unwrap();
        let record = RendezvousRecord {
            node: key(7),
            relay: None,
            addrs: vec!["192.0.2.1:4000".parse().unwrap()],
            expires_at: i64::MAX,
        };
        publish(&rendezvous, &nameplate, &record).await.unwrap();
        assert_eq!(
            lookup(&rendezvous, &nameplate, 0).await.unwrap(),
            Some(record)
        );
        // pkarr timestamps are microseconds; the withdrawal must be newer.
        tokio::time::sleep(Duration::from_millis(5)).await;
        withdraw(&rendezvous, &nameplate).await.unwrap();
        assert_eq!(lookup(&rendezvous, &nameplate, 0).await.unwrap(), None);
    }

    #[test]
    fn spake2_agrees_on_the_right_code_only() {
        let (joiner, inviter) = (key(1), key(2));
        let right = code("482913-ocean-tiger");
        let (state, message) = pake_start(&right, &joiner, &inviter);
        let (answer, inviter_key) =
            pake_answer(&right.password(), "482913", &joiner, &inviter, &message).unwrap();
        let joiner_key = pake_finish(state, &answer).unwrap();
        assert_eq!(*joiner_key, *inviter_key);

        // A wrong word gives another key.
        let wrong = code("482913-ocean-tired");
        let (state, message) = pake_start(&wrong, &joiner, &inviter);
        let (answer, inviter_key) =
            pake_answer(&right.password(), "482913", &joiner, &inviter, &message).unwrap();
        assert_ne!(*pake_finish(state, &answer).unwrap(), *inviter_key);

        // So does another device key or nameplate in the identities.
        let (state, message) = pake_start(&right, &joiner, &inviter);
        let (answer, inviter_key) =
            pake_answer(&right.password(), "482913", &key(3), &inviter, &message).unwrap();
        assert_ne!(*pake_finish(state, &answer).unwrap(), *inviter_key);
        let (state, message) = pake_start(&right, &joiner, &inviter);
        let (answer, inviter_key) =
            pake_answer(&right.password(), "482914", &joiner, &inviter, &message).unwrap();
        assert_ne!(*pake_finish(state, &answer).unwrap(), *inviter_key);

        // A message that is not one is refused.
        assert!(pake_answer(&right.password(), "482913", &joiner, &inviter, &[1, 2]).is_err());
    }

    #[test]
    fn the_session_key_binds_the_invitation() {
        let person = key(5);
        let context = SessionContext {
            id: "01J",
            person: &person,
            admin: true,
            account: "me",
        };
        let key_a = session_key(&[9; 32], &context);
        assert_eq!(*key_a, *session_key(&[9; 32], &context));
        assert_ne!(*key_a, *session_key(&[8; 32], &context));
        for changed in [
            SessionContext {
                id: "01K",
                ..context.clone()
            },
            SessionContext {
                admin: false,
                ..context.clone()
            },
            SessionContext {
                account: "you",
                ..context.clone()
            },
        ] {
            assert_ne!(*key_a, *session_key(&[9; 32], &changed));
        }
        let other = key(6);
        assert_ne!(
            *key_a,
            *session_key(
                &[9; 32],
                &SessionContext {
                    person: &other,
                    ..context.clone()
                }
            )
        );
        let tag = inviter_confirm(key_a.as_slice(), &key(1), &key(2), "01J")
            .finalize()
            .into_bytes();
        assert!(
            inviter_confirm(key_a.as_slice(), &key(1), &key(2), "01J")
                .verify_slice(&tag)
                .is_ok()
        );
        assert!(
            inviter_confirm(key_a.as_slice(), &key(2), &key(1), "01J")
                .verify_slice(&tag)
                .is_err()
        );
    }
}
