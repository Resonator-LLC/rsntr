//! The device directory of a person: how a server that has never heard
//! from any device of a person domain reaches it.
//!
//! A person domain (`<z32 person key>`) names no iroh endpoint: the
//! person key is not a device key. A server learns a person's devices
//! from their Hellos (which carry each device's certificate), so a server
//! that no device has contacted yet could not reach the person at all.
//!
//! **Record.** Admin devices (they hold the person key) publish a pkarr
//! signed packet under the person key, on the pkarr relay of pairing by
//! code ([`crate::pair_code::Rendezvous`]; n0's by default), with TXT
//! records at `_resonator-devices`:
//! - `v=1`;
//! - `rev=<seq>`: the person's revocation `seq` when it was published;
//! - `d=<z32 device key>`, one per non-revoked device with a certificate
//!   of the person, admin devices first (at most as many as fit the
//!   1000-byte packet, about a dozen).
//!
//! Certificates are not in the record (one is ~200 bytes base64, so only
//! a few would fit): a device proves itself with the certificate its
//! Hello carries. **The record grants no trust.** It only says which keys
//! to dial; the trust rules ([`crate::trust::Trust::check`]) still need a
//! valid, unrevoked certificate of the person, from the Hello.
//!
//! **When.** At start, whenever the set of devices or the revocation
//! `seq` changes (pairing, revocation; checked every
//! [`Tuning::revocation_poll`](crate::Tuning)), and every
//! [`REPUBLISH_INTERVAL`] (iroh republishes its own address records every
//! 5 minutes; relays keep records much longer than an hour, and the
//! mainline DHT about two hours). A non-admin device cannot sign the
//! record, but puts the record it finds back on the relay as often, which
//! keeps it alive while no admin device is up.
//!
//! **Lookup.** [`Directory::resolve`] caches what it finds for
//! [`FOUND_TTL`], "no record" for [`MISSING_TTL`] and a failed lookup for
//! [`FAILED_TTL`], runs one lookup per person at a time and at most
//! [`LOOKUPS_PER_MINUTE`] in all. Used by:
//! - the outbox ([`hand_to_devices`]): a row for a bare person domain is
//!   handed to one row per device (`<domain>/<device>`); the listed
//!   devices are greeted first, all at once, so that the revocation list
//!   any of them carries is known before any of them is trusted;
//! - [`crate::trust::Trust::resolve`], the key to dial for a domain;
//! - `Context::targets`, which adds the devices of a cached record.
//!
//! A key domain without a record is dialled by its key, as before (a
//! server whose domain is its own key).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::PublicKey;
use iroh_dns::pkarr::SignedPacket;
use tokio_util::sync::CancellationToken;

use crate::context::Context;
use crate::pair_code::Rendezvous;
use crate::person::{FileKeyStore, KeyStore, PersonKey};

/// The TXT name of the record inside its packet.
pub const RECORD_NAME: &str = "_resonator-devices";
/// The TTL of the record (seconds).
pub const RECORD_TTL: u32 = 300;
/// How often an unchanged record is published again.
pub const REPUBLISH_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long a found record is believed.
pub const FOUND_TTL: Duration = Duration::from_secs(5 * 60);
/// How long "no record" is believed.
pub const MISSING_TTL: Duration = Duration::from_secs(2 * 60);
/// How long a failed lookup is not tried again.
pub const FAILED_TTL: Duration = Duration::from_secs(60);
/// Lookups (not answered from the cache) per minute, in all.
pub const LOOKUPS_PER_MINUTE: u32 = 30;
/// How soon a failed publication is tried again.
const PUBLISH_RETRY: Duration = Duration::from_secs(60);
/// How long the devices of a record are greeted before an outbox row is
/// handed to them.
const GREETING_WAIT: Duration = Duration::from_secs(10);

/// What a person's device directory says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDirectory {
    /// The device keys, in the record's order.
    pub devices: Vec<PublicKey>,
    /// The person's revocation `seq` when the record was made.
    pub revocation_seq: u64,
}

impl DeviceDirectory {
    fn values(&self, devices: usize) -> Vec<String> {
        let mut values = vec!["v=1".to_owned(), format!("rev={}", self.revocation_seq)];
        values.extend(
            self.devices
                .iter()
                .take(devices)
                .map(|device| format!("d={}", device.to_z32())),
        );
        values
    }

    /// The record as a pkarr packet signed with the person key: all
    /// devices if they fit, else as many of the first as do. Returns the
    /// packet and how many devices it lists.
    pub fn to_packet(&self, person: &PersonKey) -> Result<(SignedPacket, usize), String> {
        let mut devices = self.devices.len();
        loop {
            match SignedPacket::from_txt_strings(
                person.secret(),
                RECORD_NAME,
                self.values(devices),
                RECORD_TTL,
            ) {
                Ok(packet) => return Ok((packet, devices)),
                Err(error) if devices == 0 => return Err(error.to_string()),
                Err(_) => devices -= 1,
            }
        }
    }

    /// The record a packet holds; `None` for one without it or of another
    /// version.
    pub fn from_packet(packet: &SignedPacket) -> Option<DeviceDirectory> {
        let mut version = false;
        let mut revocation_seq = 0;
        let mut devices = Vec::new();
        for value in packet.txt_records(RECORD_NAME) {
            let Some((key, value)) = value.split_once('=') else {
                continue;
            };
            match key {
                "v" => version = value == "1",
                "rev" => revocation_seq = value.parse().unwrap_or(0),
                "d" => {
                    if let Ok(Some(device)) = crate::key::parse_key_domain(value)
                        && !devices.contains(&device)
                    {
                        devices.push(device);
                    }
                }
                _ => {}
            }
        }
        version.then_some(DeviceDirectory {
            devices,
            revocation_seq,
        })
    }
}

struct Cached {
    at: Instant,
    ttl: Duration,
    record: Option<DeviceDirectory>,
}

impl Cached {
    fn fresh(&self) -> bool {
        self.at.elapsed() < self.ttl
    }
}

/// Publishes and looks up device directories on a pkarr relay, caching
/// what it finds.
pub struct Directory {
    rendezvous: Arc<dyn Rendezvous>,
    cache: Mutex<HashMap<PublicKey, Cached>>,
    /// One lookup per person at a time.
    lookups: Mutex<HashMap<PublicKey, Arc<tokio::sync::Mutex<()>>>>,
    /// The start of the current minute and the lookups made in it.
    budget: Mutex<(Instant, u32)>,
}

impl std::fmt::Debug for Directory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Directory").finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Directory {
    pub fn new(rendezvous: Arc<dyn Rendezvous>) -> Arc<Directory> {
        Arc::new(Directory {
            rendezvous,
            cache: Mutex::new(HashMap::new()),
            lookups: Mutex::new(HashMap::new()),
            budget: Mutex::new((Instant::now(), 0)),
        })
    }

    /// The record of `person` as last found, if that is recent enough.
    pub fn cached(&self, person: &PublicKey) -> Option<DeviceDirectory> {
        lock(&self.cache)
            .get(person)
            .filter(|cached| cached.fresh())
            .and_then(|cached| cached.record.clone())
    }

    /// Forgets what was found of `person`.
    pub fn forget(&self, person: &PublicKey) {
        lock(&self.cache).remove(person);
    }

    fn spend(&self) -> bool {
        let mut budget = lock(&self.budget);
        if budget.0.elapsed() >= Duration::from_secs(60) {
            *budget = (Instant::now(), 0);
        }
        if budget.1 >= LOOKUPS_PER_MINUTE {
            return false;
        }
        budget.1 += 1;
        true
    }

    /// The record of `person`: from the cache, or looked up.
    pub async fn resolve(&self, person: &PublicKey) -> Option<DeviceDirectory> {
        if let Some(cached) = lock(&self.cache).get(person).filter(|c| c.fresh()) {
            return cached.record.clone();
        }
        let slot = Arc::clone(lock(&self.lookups).entry(*person).or_default());
        let _turn = slot.lock().await;
        // Someone else may have looked it up meanwhile.
        if let Some(cached) = lock(&self.cache).get(person).filter(|c| c.fresh()) {
            return cached.record.clone();
        }
        if !self.spend() {
            tracing::debug!(person = %person.fmt_short(), "too many device directory lookups; skipping one");
            return None;
        }
        let (record, ttl) = match self.rendezvous.resolve(person).await {
            Ok(Some(packet)) => match DeviceDirectory::from_packet(&packet) {
                Some(record) => (Some(record), FOUND_TTL),
                None => (None, MISSING_TTL),
            },
            Ok(None) => (None, MISSING_TTL),
            Err(error) => {
                tracing::debug!(person = %person.fmt_short(), %error, "cannot look up a device directory");
                (None, FAILED_TTL)
            }
        };
        if let Some(record) = &record {
            tracing::info!(
                person = %person.fmt_short(),
                devices = record.devices.len(),
                "found the device directory of a person"
            );
        }
        let mut cache = lock(&self.cache);
        if cache.len() > 4096 {
            cache.retain(|_, cached| cached.fresh());
        }
        cache.insert(
            *person,
            Cached {
                at: Instant::now(),
                ttl,
                record: record.clone(),
            },
        );
        drop(cache);
        lock(&self.lookups).retain(|_, slot| Arc::strong_count(slot) > 1);
        record
    }

    /// Publishes `record` signed by `person`; returns how many devices it
    /// lists.
    pub async fn publish(
        &self,
        person: &PersonKey,
        record: &DeviceDirectory,
    ) -> Result<usize, String> {
        let (packet, listed) = record.to_packet(person)?;
        self.rendezvous.publish(&packet).await?;
        Ok(listed)
    }

    /// Puts the record found under `person` back on the relay unchanged
    /// (anyone may: it is signed), so that it does not expire while no
    /// device that holds the person key is up. Returns whether there was
    /// one.
    pub async fn refresh(&self, person: &PublicKey) -> Result<bool, String> {
        let Some(packet) = self.rendezvous.resolve(person).await? else {
            return Ok(false);
        };
        if DeviceDirectory::from_packet(&packet).is_none() {
            return Ok(false);
        }
        self.rendezvous.publish(&packet).await?;
        Ok(true)
    }
}

/// The record this device would publish for its person: the
/// non-revoked devices with a certificate of the person (this one
/// included), admin devices first, then by key.
pub(crate) fn own_directory(ctx: &Context, person: &PublicKey) -> DeviceDirectory {
    let persons = ctx.trust.persons();
    let mut devices: Vec<(bool, PublicKey)> = Vec::new();
    for device in std::iter::once(ctx.own_key()).chain(ctx.trust.devices_of(&ctx.domain)) {
        if devices.iter().any(|(_, known)| *known == device)
            || persons.is_revoked_by(person, &device)
        {
            continue;
        }
        let Some(cert) = persons.cert(&device, person) else {
            continue;
        };
        devices.push((cert.admin, device));
    }
    devices.sort_by(|(a_admin, a), (b_admin, b)| {
        b_admin
            .cmp(a_admin)
            .then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });
    DeviceDirectory {
        devices: devices.into_iter().map(|(_, device)| device).collect(),
        revocation_seq: persons.revocation_seq(person),
    }
}

/// The person key at `path`, if it is there and is `person`'s.
fn load_person_key(path: &std::path::Path, person: &PublicKey) -> Option<PersonKey> {
    let phrase = FileKeyStore::new(path).load().ok().flatten()?;
    let key = PersonKey::from_phrase(&phrase);
    (key.public() == *person).then_some(key)
}

/// Person mode: keeps this person's device directory published (see the
/// module documentation). On a device without the person key it puts the
/// record it finds back on the relay as often instead.
pub(crate) async fn publish_task(
    ctx: Arc<Context>,
    directory: Arc<Directory>,
    key_path: PathBuf,
    cancel: CancellationToken,
) {
    let Some(person) = ctx.trust.persons().own() else {
        return;
    };
    let mut last: Option<(DeviceDirectory, Instant)> = None;
    let mut retry_at: Option<Instant> = None;
    loop {
        let wanted = own_directory(&ctx, &person);
        let due = last
            .as_ref()
            .is_none_or(|(record, at)| *record != wanted || at.elapsed() >= REPUBLISH_INTERVAL)
            && retry_at.is_none_or(|at| Instant::now() >= at);
        if due {
            match load_person_key(&key_path, &person) {
                Some(key) => match directory.publish(&key, &wanted).await {
                    Ok(listed) => {
                        if listed < wanted.devices.len() {
                            tracing::warn!(
                                listed,
                                devices = wanted.devices.len(),
                                "the device directory lists only the devices that fit"
                            );
                        }
                        tracing::info!(
                            devices = listed,
                            revocation_seq = wanted.revocation_seq,
                            "published the device directory"
                        );
                        directory.forget(&person);
                        last = Some((wanted, Instant::now()));
                        retry_at = None;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "cannot publish the device directory");
                        retry_at = Some(Instant::now() + PUBLISH_RETRY);
                    }
                },
                None => {
                    match directory.refresh(&person).await {
                        Ok(found) => {
                            tracing::debug!(found, "refreshed the device directory");
                        }
                        Err(error) => {
                            tracing::debug!(%error, "cannot refresh the device directory");
                        }
                    }
                    last = Some((wanted, Instant::now()));
                }
            }
        }
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(ctx.tuning.revocation_poll) => {}
        }
    }
}

/// The devices an outbox row for a bare key domain (`target`, with no
/// device) is to be handed to instead: those known of it and those its
/// device directory lists, not revoked, this node's own key left out.
/// `None` when the domain is not a person's this node can reach that way
/// (the row then goes to the key itself, as for a server's key domain).
///
/// The devices are greeted first, all at once: their Hellos carry their
/// certificates and the person's revocation list, so a device revoked by
/// a list any of them knows is left out, even if a stale record lists
/// it. Devices not reached get their rows all the same (they may be
/// asleep); whether they are trusted is decided when they are reached.
pub(crate) async fn hand_to_devices(ctx: &Context, target: &str) -> Option<Vec<PublicKey>> {
    let (domain, device) = crate::context::parse_target(target);
    if device.is_some() || domain == ctx.domain {
        return None;
    }
    let person = crate::key::parse_key_domain(domain).ok().flatten()?;
    let own = ctx.own_key();
    let mut devices = ctx.trust.devices_of(domain);
    for device in ctx
        .trust
        .directory_devices(domain)
        .await
        .unwrap_or_default()
    {
        if !devices.contains(&device) {
            devices.push(device);
        }
    }
    devices.retain(|device| *device != own && *device != person);
    if devices.is_empty() {
        return None;
    }
    if let Ok(endpoint) = ctx.endpoint() {
        let mut greetings = tokio::task::JoinSet::new();
        for device in devices.iter().copied() {
            let endpoint = endpoint.clone();
            greetings.spawn(async move {
                if let Err(error) = endpoint.connect(device, None).await {
                    tracing::debug!(device = %device.fmt_short(), %error, "a listed device did not answer");
                }
            });
        }
        let _ = tokio::time::timeout(GREETING_WAIT, async {
            while greetings.join_next().await.is_some() {}
        })
        .await;
    }
    let persons = ctx.trust.persons();
    devices
        .retain(|device| !persons.is_revoked_by(&person, device) && !ctx.trust.is_revoked(device));
    (!devices.is_empty()).then_some(devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pair_code::MemoryRendezvous;
    use iroh::SecretKey;

    fn key(byte: u8) -> PublicKey {
        SecretKey::from_bytes(&[byte; 32]).public()
    }

    #[test]
    fn records_round_trip_as_signed_packets() {
        let (person, _) = PersonKey::generate();
        let record = DeviceDirectory {
            devices: vec![key(1), key(2), key(3)],
            revocation_seq: 7,
        };
        let (packet, listed) = record.to_packet(&person).unwrap();
        assert_eq!(listed, 3);
        assert_eq!(packet.public_key(), person.public());
        let again =
            SignedPacket::from_relay_payload(&packet.public_key(), &packet.to_relay_payload())
                .unwrap();
        assert_eq!(DeviceDirectory::from_packet(&again), Some(record));
    }

    #[test]
    fn an_empty_directory_is_a_record_and_other_packets_are_not() {
        let (person, _) = PersonKey::generate();
        let empty = DeviceDirectory {
            devices: Vec::new(),
            revocation_seq: 0,
        };
        let (packet, _) = empty.to_packet(&person).unwrap();
        assert_eq!(DeviceDirectory::from_packet(&packet), Some(empty));
        // A pairing rendezvous record, another version, junk values.
        let other = SignedPacket::from_txt_strings(
            person.secret(),
            crate::pair_code::RECORD_NAME,
            ["v=1", "node=x"],
            30,
        )
        .unwrap();
        assert_eq!(DeviceDirectory::from_packet(&other), None);
        let newer =
            SignedPacket::from_txt_strings(person.secret(), RECORD_NAME, ["v=2", "d=x"], 30)
                .unwrap();
        assert_eq!(DeviceDirectory::from_packet(&newer), None);
        let device = key(4).to_z32();
        let junk = SignedPacket::from_txt_strings(
            person.secret(),
            RECORD_NAME,
            [
                "v=1".to_owned(),
                "d=not-a-key".to_owned(),
                format!("d={device}"),
                format!("d={device}"),
                "rev=x".to_owned(),
                "what".to_owned(),
            ],
            30,
        )
        .unwrap();
        assert_eq!(
            DeviceDirectory::from_packet(&junk),
            Some(DeviceDirectory {
                devices: vec![key(4)],
                revocation_seq: 0,
            })
        );
    }

    #[test]
    fn a_record_lists_as_many_devices_as_fit_in_a_packet() {
        let (person, _) = PersonKey::generate();
        let record = DeviceDirectory {
            devices: (1..=40).map(key).collect(),
            revocation_seq: u64::MAX,
        };
        let (packet, listed) = record.to_packet(&person).unwrap();
        assert!((10..40).contains(&listed), "{listed}");
        assert!(packet.encoded_packet().len() <= 1000);
        let read = DeviceDirectory::from_packet(&packet).unwrap();
        assert_eq!(read.devices, record.devices[..listed]);
        assert_eq!(read.revocation_seq, u64::MAX);
        // One more would not have fitted.
        let more = DeviceDirectory {
            devices: record.devices[..=listed].to_vec(),
            ..record.clone()
        };
        assert!(
            SignedPacket::from_txt_strings(
                person.secret(),
                RECORD_NAME,
                more.values(listed + 1),
                RECORD_TTL
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn lookups_are_cached_and_records_refreshed() {
        let rendezvous = Arc::new(MemoryRendezvous::new());
        let directory = Directory::new(rendezvous.clone());
        let (person, _) = PersonKey::generate();
        // Nothing yet: remembered as missing.
        assert_eq!(directory.resolve(&person.public()).await, None);
        let record = DeviceDirectory {
            devices: vec![key(1)],
            revocation_seq: 1,
        };
        directory.publish(&person, &record).await.unwrap();
        assert_eq!(directory.resolve(&person.public()).await, None);
        directory.forget(&person.public());
        assert_eq!(
            directory.resolve(&person.public()).await,
            Some(record.clone())
        );
        assert_eq!(directory.cached(&person.public()), Some(record.clone()));
        // Anyone can put the signed record back.
        let other = Directory::new(rendezvous.clone());
        assert!(other.refresh(&person.public()).await.unwrap());
        assert!(!other.refresh(&key(9)).await.unwrap());
        assert_eq!(other.resolve(&person.public()).await, Some(record));
    }

    #[tokio::test]
    async fn lookups_are_rate_limited() {
        let directory = Directory::new(Arc::new(MemoryRendezvous::new()));
        for byte in 0..LOOKUPS_PER_MINUTE {
            assert_eq!(directory.resolve(&key(byte as u8)).await, None);
        }
        assert_eq!(lock(&directory.budget).1, LOOKUPS_PER_MINUTE);
        // Over budget: not looked up, nothing cached.
        let late = key(200);
        assert_eq!(directory.resolve(&late).await, None);
        assert!(!lock(&directory.cache).contains_key(&late));
        // Cached answers cost nothing.
        assert_eq!(directory.resolve(&key(0)).await, None);
    }
}
