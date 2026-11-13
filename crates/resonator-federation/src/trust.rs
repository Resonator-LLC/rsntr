//! Which remote server may speak for which domain.
//!
//! The QUIC handshake proves a peer's server key. A request is authorised
//! for domain D only if D is bound to that key:
//!
//! - a key domain is bound to the key it encodes, and to nothing else;
//! - a DNS domain is bound to the key pinned for it in `federation_peers`.
//!   The first key seen that the domain's `_resonator` TXT record lists is
//!   pinned (trust on first use). A different key later replaces the pin
//!   only if the TXT record lists it **and** the pinned key signed a
//!   [`KeyRotation`](resonator_model::proto::federation_rpc::KeyRotation)
//!   to it; otherwise the new key is quarantined until an admin decides.
//!   In `open` mode, a domain that publishes no record at all (a bare
//!   host name such as `relayhost`, or a domain that never set one up) is
//!   pinned to the first key that claims it. A domain that does publish a
//!   record is held to it, and a later, different key is quarantined as
//!   above.
//!
//! On top of the binding, the [`Policy`] decides which domains may federate
//! at all (`open`, the default, or `allowlist`), and a peer can never speak
//! for one of this server's own domains.
//!
//! Person identity ([`crate::person`]) adds a third binding, checked
//! first: a device key with a [`DeviceCert`](resonator_model::proto::federation_rpc::DeviceCert)
//! of a person speaks for that person's domain (the person key's key
//! domain, or a DNS domain whose TXT record lists the person key) unless
//! the person revoked it. Own devices may speak for this node's own
//! (person) domain. The one-trusted-key-per-domain pins above stay for
//! server keys only.

use std::collections::BTreeSet;
use std::sync::Arc;

use iroh::PublicKey;
use resonator_model::proto::federation_rpc::{
    SignedDeviceCert, SignedHostLease, SignedKeyRotation, SignedRevocationList,
};
use resonator_model::proto::{decode, encode};
use resonator_server::clock::Clock;
use thiserror::Error;

use crate::lease::{LeaseError, Leases, VerifiedLease, verify_lease};
use resonator_server::config::{FederationConfig, FederationMode};
use resonator_server::storage::{
    DeviceCertRecord, FederationStore, PeerStatus, RevocationRecord, StorageError,
};

use crate::dns::{DnsError, TxtResolver, lookup_keys};
use crate::key::parse_key_domain;
use crate::person::{
    PersonBook, PersonError, VerifiedCert, VerifiedRevocations, verify_cert, verify_revocations,
};
use crate::rotation::find_rotation;

/// Which domains may federate with this server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub mode: FederationMode,
    pub allow: BTreeSet<String>,
}

impl Policy {
    pub fn open() -> Self {
        Policy {
            mode: FederationMode::Open,
            allow: BTreeSet::new(),
        }
    }

    pub fn allowlist<S: Into<String>>(allow: impl IntoIterator<Item = S>) -> Self {
        Policy {
            mode: FederationMode::Allowlist,
            allow: allow.into_iter().map(Into::into).collect(),
        }
    }

    pub fn from_config(config: &FederationConfig) -> Self {
        Policy {
            mode: config.mode,
            allow: config.allow.iter().cloned().collect(),
        }
    }

    /// Whether `domain` may federate.
    pub fn allows(&self, domain: &str) -> bool {
        match self.mode {
            FederationMode::Open => true,
            FederationMode::Allowlist => self.allow.contains(domain),
        }
    }
}

/// The verdict on a peer key speaking for a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorisation {
    /// The key is bound to the domain.
    Authorised,
    /// The policy does not let the domain federate, or it is one of this
    /// server's own domains.
    NotAllowed,
    /// Nothing binds the key to the domain.
    NotBound,
    /// The key replaced the domain's pinned key without a valid rotation
    /// statement; an admin has to approve it.
    Quarantined,
    /// An admin refused the key.
    Denied,
    /// The key has a certificate of a person of the domain, who revoked
    /// it.
    Revoked,
}

impl Authorisation {
    pub fn is_authorised(self) -> bool {
        self == Authorisation::Authorised
    }
}

/// Failure to reach a verdict.
#[derive(Debug, Error)]
pub enum TrustError {
    #[error(transparent)]
    Dns(#[from] DnsError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("'{0}' looks like a key domain but is not a valid key")]
    InvalidKeyDomain(String),
    #[error(transparent)]
    Person(#[from] PersonError),
    #[error(transparent)]
    Lease(#[from] crate::lease::LeaseError),
}

/// The binding rules, over the `federation_peers` table and DNS.
#[derive(Clone)]
pub struct Trust {
    store: FederationStore,
    resolver: Arc<dyn TxtResolver>,
    policy: Policy,
    local_domains: BTreeSet<String>,
    clock: Arc<dyn Clock>,
    persons: Arc<PersonBook>,
    leases: Arc<Leases>,
    co_hosts: Arc<crate::cohost::CoHostBook>,
    /// Where the device directories of persons are looked up, if
    /// anywhere ([`crate::directory`]).
    directory: Option<Arc<crate::directory::Directory>>,
}

impl std::fmt::Debug for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trust")
            .field("policy", &self.policy)
            .field("local_domains", &self.local_domains)
            .finish_non_exhaustive()
    }
}

impl Trust {
    pub fn new(
        store: FederationStore,
        resolver: Arc<dyn TxtResolver>,
        policy: Policy,
        local_domains: impl IntoIterator<Item = String>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Trust {
            store,
            resolver,
            policy,
            local_domains: local_domains.into_iter().collect(),
            clock,
            persons: PersonBook::new(),
            leases: Leases::new(false),
            co_hosts: crate::cohost::CoHostBook::new(),
            directory: None,
        }
    }

    /// Looks up the device directories of person domains in `directory`.
    pub fn with_directory(mut self, directory: Option<Arc<crate::directory::Directory>>) -> Self {
        self.directory = directory;
        self
    }

    /// Where device directories are looked up, if anywhere.
    pub fn directory(&self) -> Option<&Arc<crate::directory::Directory>> {
        self.directory.as_ref()
    }

    /// The devices the device directory of key domain `domain` lists, not
    /// revoked by its person: looked up (or cached). `None` without a
    /// directory, for this node's own domain, a domain that is no key
    /// domain or one that publishes no record.
    ///
    /// Being listed grants nothing: a device still speaks for the domain
    /// only with a certificate of its person ([`Trust::check`]).
    pub async fn directory_devices(&self, domain: &str) -> Option<Vec<PublicKey>> {
        let directory = self.directory.as_ref()?;
        if self.local_domains.contains(domain) {
            return None;
        }
        let person = parse_key_domain(domain).ok().flatten()?;
        let record = directory.resolve(&person).await?;
        Some(self.unrevoked(&person, record.devices))
    }

    /// The devices of key domain `domain` that its cached device
    /// directory lists and that [`Trust::devices_of`] does not know yet,
    /// not revoked; no lookup.
    pub fn listed_devices(&self, domain: &str) -> Vec<PublicKey> {
        let Some(directory) = &self.directory else {
            return Vec::new();
        };
        if self.local_domains.contains(domain) {
            return Vec::new();
        }
        let Some(person) = parse_key_domain(domain).ok().flatten() else {
            return Vec::new();
        };
        let Some(record) = directory.cached(&person) else {
            return Vec::new();
        };
        let known = self.devices_of(domain);
        self.unrevoked(&person, record.devices)
            .into_iter()
            .filter(|device| !known.contains(device))
            .collect()
    }

    fn unrevoked(&self, person: &PublicKey, devices: Vec<PublicKey>) -> Vec<PublicKey> {
        devices
            .into_iter()
            .filter(|device| {
                device != person
                    && !self.persons.is_revoked_by(person, device)
                    && !self.persons.is_revoked(device)
            })
            .collect()
    }

    /// The co-host grants known.
    pub fn co_hosts(&self) -> &Arc<crate::cohost::CoHostBook> {
        &self.co_hosts
    }

    /// Shares `co_hosts` (instead of an empty book of its own).
    pub fn with_co_hosts(mut self, co_hosts: Arc<crate::cohost::CoHostBook>) -> Self {
        self.co_hosts = co_hosts;
        self
    }

    /// Checks a co-host grant and keeps it (here and in storage) if it is
    /// newer than the one known of its person for its domain. Returns it
    /// if so.
    pub async fn learn_co_host(
        &self,
        signed: &resonator_model::proto::federation_rpc::SignedCoHost,
    ) -> Result<Option<crate::cohost::VerifiedCoHost>, TrustError> {
        let grant = crate::cohost::verify_co_host(signed)
            .map_err(|message| TrustError::Lease(LeaseError::Invalid(message)))?;
        if !self.co_hosts.offer(grant.clone()) {
            return Ok(None);
        }
        tracing::debug!(
            person = %grant.person.fmt_short(),
            delegate = %grant.delegate_domain,
            "learned a co-host grant"
        );
        self.store
            .put_co_host(resonator_server::storage::CoHostRecord {
                person: grant.person.as_bytes().to_vec(),
                delegate_domain: grant.delegate_domain.clone(),
                issued_at: grant.issued_at,
                until: grant.until,
                grant: encode(signed).unwrap_or_default(),
                learned_at: self.clock.now_millis(),
            })
            .await?;
        Ok(Some(grant))
    }

    /// Reads the stored co-host grants (also those the command line
    /// stored meanwhile); returns those newer than the ones known.
    pub async fn load_co_hosts(&self) -> Result<Vec<crate::cohost::VerifiedCoHost>, TrustError> {
        let mut learned = Vec::new();
        for record in self.store.co_hosts().await? {
            let Ok(signed) = resonator_model::proto::decode(&record.grant) else {
                continue;
            };
            if let Ok(grant) = crate::cohost::verify_co_host(&signed)
                && self.co_hosts.offer(grant.clone())
            {
                learned.push(grant);
            }
        }
        Ok(learned)
    }

    /// Shares `leases` (instead of an empty book of its own).
    pub fn with_leases(mut self, leases: Arc<Leases>) -> Self {
        self.leases = leases;
        self
    }

    /// The host leases known.
    pub fn leases(&self) -> Arc<Leases> {
        Arc::clone(&self.leases)
    }

    /// Checks a host lease and takes it (here and in storage) if it beats
    /// the one known of its person: its signature, the device's
    /// certificate of the person (not revoked) and that the person speaks
    /// for the lease's domain. Routes the domain to the holder. Returns
    /// the lease if it was taken.
    pub async fn learn_lease(
        &self,
        signed: &SignedHostLease,
    ) -> Result<Option<VerifiedLease>, TrustError> {
        let lease = verify_lease(signed)?;
        let invalid = |message: String| TrustError::Lease(LeaseError::Invalid(message));
        if let Some(delegate) = &lease.co_host_domain {
            // A co-host: a device of the delegate domain, under a grant
            // active when the lease was issued.
            if !self
                .co_hosts
                .active(&lease.person, delegate, lease.issued_at)
            {
                return Err(invalid(format!(
                    "the person lets {delegate} host only under a grant, and none is active"
                )));
            }
            if !self.local_domains.contains(delegate)
                && !self.verify_signer(&lease.device, delegate).await?
            {
                return Err(invalid(format!(
                    "{} does not speak for {delegate}",
                    lease.device.fmt_short()
                )));
            }
        } else if self.persons.cert(&lease.device, &lease.person).is_none() {
            return Err(invalid(format!(
                "{} has no certificate of the lease's person",
                lease.device.fmt_short()
            )));
        }
        if self.persons.is_revoked_by(&lease.person, &lease.device) {
            return Err(invalid(format!(
                "{} has been revoked",
                lease.device.fmt_short()
            )));
        }
        if !self.person_binds(&lease.person, &lease.domain).await? {
            return Err(invalid(format!(
                "the person does not speak for {}",
                lease.domain
            )));
        }
        if !self.leases.offer(lease.clone()) {
            return Ok(None);
        }
        tracing::debug!(
            domain = %lease.domain,
            term = lease.term,
            holder = %lease.device.fmt_short(),
            "learned a host lease"
        );
        if !self.local_domains.contains(&lease.domain) && lease.co_host_domain.is_none() {
            self.persons.set_route(&lease.domain, lease.device);
        }
        self.store
            .put_host_lease(
                lease.person.as_bytes().to_vec(),
                encode(signed).unwrap_or_default(),
                self.clock.now_millis(),
            )
            .await?;
        Ok(Some(lease))
    }

    /// Reads the stored host leases back (at start).
    pub async fn load_leases(&self) -> Result<(), TrustError> {
        for (_, bytes) in self.store.host_leases().await? {
            let Ok(signed) = decode::<SignedHostLease>(&bytes) else {
                continue;
            };
            if let Err(error) = self.learn_lease(&signed).await {
                tracing::warn!(%error, "ignoring a stored host lease");
            }
        }
        Ok(())
    }

    /// Shares `persons` (instead of an empty book of its own).
    pub fn with_persons(mut self, persons: Arc<PersonBook>) -> Self {
        self.persons = persons;
        self
    }

    /// What this node knows of persons and their devices.
    pub fn persons(&self) -> &Arc<PersonBook> {
        &self.persons
    }

    /// The own domain, in person mode.
    fn own_person_domain(&self) -> Option<&str> {
        self.persons.own()?;
        self.local_domains.iter().next().map(String::as_str)
    }

    /// Whether `person` (a person key) speaks for `domain`: its key
    /// domain, this node's own domain if it is this node's person, or a
    /// DNS domain whose `_resonator` TXT record lists it.
    pub async fn person_binds(&self, person: &PublicKey, domain: &str) -> Result<bool, TrustError> {
        match parse_key_domain(domain) {
            Ok(Some(key)) => return Ok(key == *person),
            Ok(None) => {}
            Err(_) => return Ok(false),
        }
        if self.local_domains.contains(domain) {
            return Ok(self.persons.own() == Some(*person));
        }
        if self.persons.dns_lists(domain, person) {
            return Ok(true);
        }
        let published = lookup_keys(self.resolver.as_ref(), domain).await?;
        let listed = published.contains(person);
        if listed {
            self.persons.note_dns(domain, &published);
        }
        Ok(listed)
    }

    /// The certificate by which `device` speaks for `domain`, if it has
    /// one, and whether its person revoked it.
    pub async fn cert_binding(
        &self,
        device: &PublicKey,
        domain: &str,
    ) -> Result<Option<(VerifiedCert, bool)>, TrustError> {
        let mut found = None;
        for cert in self.persons.certs_of(device) {
            if self.person_binds(&cert.person, domain).await? {
                let revoked = self.persons.is_revoked_by(&cert.person, device);
                if !revoked {
                    return Ok(Some((cert, false)));
                }
                found = Some((cert, true));
            }
        }
        Ok(found)
    }

    /// Checks a device certificate and records it (here and in storage).
    pub async fn learn_cert(&self, signed: &SignedDeviceCert) -> Result<VerifiedCert, TrustError> {
        let cert = verify_cert(signed)?;
        if self.persons.add_cert(cert.clone()) {
            tracing::debug!(
                person = %cert.person.to_z32(),
                device = %cert.device.to_z32(),
                "learned a device certificate"
            );
            self.store
                .put_device_cert(DeviceCertRecord {
                    device_key: cert.device.as_bytes().to_vec(),
                    person: cert.person.as_bytes().to_vec(),
                    name: cert.name.clone(),
                    admin: cert.admin,
                    issued_at: cert.issued_at,
                    cert: encode(signed).unwrap_or_default(),
                    learned_at: self.clock.now_millis(),
                })
                .await?;
        }
        Ok(cert)
    }

    /// Checks a revocation list and records it (here and in storage) if
    /// it is newer than the one known of its person. Returns it if so.
    pub async fn learn_revocations(
        &self,
        signed: &SignedRevocationList,
    ) -> Result<Option<VerifiedRevocations>, TrustError> {
        let list = verify_revocations(signed)?;
        if list.seq <= self.persons.revocation_seq(&list.person) {
            return Ok(None);
        }
        self.store
            .put_revocations(RevocationRecord {
                person: list.person.as_bytes().to_vec(),
                seq: list.seq,
                list: encode(signed).unwrap_or_default(),
                learned_at: self.clock.now_millis(),
            })
            .await?;
        if !self.persons.learn_revocations(list.clone()) {
            return Ok(None);
        }
        tracing::info!(
            person = %list.person.to_z32(),
            seq = list.seq,
            revoked = list.revoked.len(),
            "learned a revocation list"
        );
        Ok(Some(list))
    }

    /// Reads the stored certificates and revocation lists into the person
    /// book; returns the revocation lists that were new to it.
    pub async fn load_persons(&self) -> Result<Vec<VerifiedRevocations>, TrustError> {
        for record in self.store.device_certs().await? {
            match resonator_model::proto::decode::<SignedDeviceCert>(&record.cert)
                .map_err(|e| PersonError::Cert(e.to_string()))
                .and_then(|signed| verify_cert(&signed))
            {
                Ok(cert) => {
                    self.persons.add_cert(cert);
                }
                Err(error) => tracing::warn!(%error, "ignoring a stored device certificate"),
            }
        }
        let mut new = Vec::new();
        for record in self.store.revocations().await? {
            match resonator_model::proto::decode::<SignedRevocationList>(&record.list)
                .map_err(|e| PersonError::Revocations(e.to_string()))
                .and_then(|signed| verify_revocations(&signed))
            {
                Ok(list) => {
                    if self.persons.learn_revocations(list.clone()) {
                        new.push(list);
                    }
                }
                Err(error) => tracing::warn!(%error, "ignoring a stored revocation list"),
            }
        }
        Ok(new)
    }

    /// Whether `device` was revoked by a person that certified it.
    pub fn is_revoked(&self, device: &PublicKey) -> bool {
        self.persons.is_revoked(device)
    }

    /// The devices known for `domain` (configured or certified by a person
    /// of it), not revoked; empty for a server domain.
    pub fn devices_of(&self, domain: &str) -> Vec<PublicKey> {
        let persons = self.persons.persons_of(domain, self.own_person_domain());
        self.persons.devices_of(domain, &persons)
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn store(&self) -> &FederationStore {
        &self.store
    }

    /// The binding check: whether `peer` may speak for `domain`, with no
    /// rotation statements offered.
    pub async fn authorise(&self, peer: &PublicKey, domain: &str) -> bool {
        match self.check(peer, domain, &[]).await {
            Ok(verdict) => verdict.is_authorised(),
            Err(error) => {
                tracing::warn!(%domain, %error, "cannot check a federation peer");
                false
            }
        }
    }

    /// Whether `peer` may speak for `domain`, given the rotation statements
    /// it offered; records what it learns (a first pin, an accepted
    /// rotation, a quarantined key).
    pub async fn check(
        &self,
        peer: &PublicKey,
        domain: &str,
        rotations: &[SignedKeyRotation],
    ) -> Result<Authorisation, TrustError> {
        if let Some((_, revoked)) = self.cert_binding(peer, domain).await? {
            return Ok(if revoked {
                Authorisation::Revoked
            } else if self.local_domains.contains(domain) || self.policy.allows(domain) {
                // An own device may speak for the own domain.
                Authorisation::Authorised
            } else {
                Authorisation::NotAllowed
            });
        }
        if self.local_domains.contains(domain) || !self.policy.allows(domain) {
            return Ok(Authorisation::NotAllowed);
        }
        match parse_key_domain(domain) {
            Ok(Some(key)) => {
                return Ok(if key == *peer {
                    Authorisation::Authorised
                } else {
                    Authorisation::NotBound
                });
            }
            Ok(None) => {}
            Err(_) => return Ok(Authorisation::NotBound),
        }
        let now = self.clock.now_millis();
        let pins = self.store.peer_keys(domain).await?;
        if let Some(pin) = pins.iter().find(|pin| pin.key == peer.as_bytes()) {
            match pin.status {
                PeerStatus::Trusted => {
                    self.store.touch_peer(domain, peer.as_bytes(), now).await?;
                    return Ok(Authorisation::Authorised);
                }
                PeerStatus::Denied => return Ok(Authorisation::Denied),
                // A replaced key stays retired (its old signatures remain
                // valid) and may not speak for the domain again unless an
                // admin trusts it again.
                PeerStatus::Retired => return Ok(Authorisation::NotBound),
                // A quarantined key may since have been properly rotated
                // to: checked again below.
                PeerStatus::Quarantined => {}
            }
        }
        let published = lookup_keys(self.resolver.as_ref(), domain).await?;
        let in_record = published.contains(peer);
        let trusted = pins
            .iter()
            .find(|pin| pin.status == PeerStatus::Trusted)
            .and_then(|pin| <&[u8; 32]>::try_from(pin.key.as_slice()).ok())
            .and_then(|bytes| PublicKey::from_bytes(bytes).ok());
        match trusted {
            None if in_record => {
                tracing::info!(%domain, key = %peer.to_z32(), "pinned a federation peer key");
                self.store
                    .set_peer(domain, peer.as_bytes(), PeerStatus::Trusted, now)
                    .await?;
                Ok(Authorisation::Authorised)
            }
            None if pins
                .iter()
                .any(|pin| pin.key == peer.as_bytes() && pin.status == PeerStatus::Quarantined) =>
            {
                Ok(Authorisation::Quarantined)
            }
            // Open mode accepts everyone: a domain that publishes no
            // record belongs to the first key that claims it.
            None if self.policy.mode == FederationMode::Open && published.is_empty() => {
                tracing::info!(
                    %domain,
                    key = %peer.to_z32(),
                    "pinned a federation peer key on first contact (the domain publishes no record)"
                );
                self.store
                    .set_peer(domain, peer.as_bytes(), PeerStatus::Trusted, now)
                    .await?;
                Ok(Authorisation::Authorised)
            }
            None => Ok(Authorisation::NotBound),
            Some(old) => {
                if in_record && find_rotation(rotations, domain, &old, peer).is_some() {
                    tracing::info!(
                        %domain,
                        old = %old.to_z32(),
                        new = %peer.to_z32(),
                        "accepted a signed federation key rotation"
                    );
                    self.store
                        .set_peer(domain, peer.as_bytes(), PeerStatus::Trusted, now)
                        .await?;
                    Ok(Authorisation::Authorised)
                } else {
                    tracing::warn!(
                        %domain,
                        pinned = %old.to_z32(),
                        presented = %peer.to_z32(),
                        in_record,
                        "quarantined a federation key change without a signed rotation"
                    );
                    self.store
                        .set_peer(domain, peer.as_bytes(), PeerStatus::Quarantined, now)
                        .await?;
                    Ok(Authorisation::Quarantined)
                }
            }
        }
    }

    /// The key to dial for `domain`: its route (a device of a person),
    /// for a key domain a device of its person known here or listed by
    /// its device directory, else the key it encodes; the pinned key of a
    /// DNS domain, or else the first key its TXT record
    /// lists (pinned only once the peer has proved it in a handshake and
    /// passed [`Trust::check`]). `None` if the domain names no key.
    pub async fn resolve(&self, domain: &str) -> Result<Option<PublicKey>, TrustError> {
        // A person domain is reached through one of its devices.
        if let Some(device) = self.persons.route(domain) {
            return Ok(Some(device));
        }
        match parse_key_domain(domain) {
            Ok(Some(key)) => {
                // A person key is no endpoint: one of the person's devices
                // known here, else one its device directory lists. A key
                // domain without either is a server's own key.
                if !self.local_domains.contains(domain) {
                    if let Some(device) = self.devices_of(domain).into_iter().next() {
                        return Ok(Some(device));
                    }
                    if let Some(device) = self
                        .directory_devices(domain)
                        .await
                        .and_then(|devices| devices.into_iter().next())
                    {
                        return Ok(Some(device));
                    }
                }
                return Ok(Some(key));
            }
            Ok(None) => {}
            Err(_) => return Err(TrustError::InvalidKeyDomain(domain.to_owned())),
        }
        if let Some(pin) = self.store.trusted_key(domain).await?
            && let Ok(bytes) = <&[u8; 32]>::try_from(pin.key.as_slice())
            && let Ok(key) = PublicKey::from_bytes(bytes)
        {
            return Ok(Some(key));
        }
        Ok(lookup_keys(self.resolver.as_ref(), domain)
            .await?
            .into_iter()
            .next())
    }

    /// Whether a delta signer's `key` speaks for `domain`: the domain is
    /// the key domain of the key, or the key is or was the domain's trusted
    /// key. Signatures made before a rotation stay valid.
    pub async fn is_signer_bound(&self, key: &PublicKey, domain: &str) -> Result<bool, TrustError> {
        match parse_key_domain(domain) {
            Ok(Some(domain_key)) => return Ok(domain_key == *key),
            Ok(None) => {}
            Err(_) => return Ok(false),
        }
        Ok(self.store.peer_keys(domain).await?.iter().any(|pin| {
            pin.key == key.as_bytes()
                && matches!(pin.status, PeerStatus::Trusted | PeerStatus::Retired)
        }))
    }

    /// [`Trust::is_signer_bound`], or else, for a DNS domain without a
    /// trusted key, whether its TXT record lists `key` (which then gets
    /// pinned). The policy does not apply: deltas on a wavelet may come
    /// from domains this server does not federate with itself.
    pub async fn verify_signer(&self, key: &PublicKey, domain: &str) -> Result<bool, TrustError> {
        if let Some((_, revoked)) = self.cert_binding(key, domain).await? {
            return Ok(!revoked);
        }
        if self.is_signer_bound(key, domain).await? {
            return Ok(true);
        }
        if parse_key_domain(domain).ok().flatten().is_some()
            || self.local_domains.contains(domain)
            || self.store.trusted_key(domain).await?.is_some()
        {
            return Ok(false);
        }
        let published = lookup_keys(self.resolver.as_ref(), domain).await?;
        if !published.contains(key) {
            return Ok(false);
        }
        tracing::info!(%domain, key = %key.to_z32(), "pinned a delta signer key");
        self.store
            .set_peer(
                domain,
                key.as_bytes(),
                PeerStatus::Trusted,
                self.clock.now_millis(),
            )
            .await?;
        Ok(true)
    }

    /// [`Trust::verify_signer`] for signatures already applied by a host:
    /// a device key with a certificate of the domain's person counts even
    /// if the person revoked it since, so revoking a device does not
    /// rewind history (its later signatures are refused by hosts).
    pub async fn verify_historical_signer(
        &self,
        key: &PublicKey,
        domain: &str,
    ) -> Result<bool, TrustError> {
        if self.cert_binding(key, domain).await?.is_some() {
            return Ok(true);
        }
        self.verify_signer(key, domain).await
    }

    /// Binds a DNS domain to `key` because the configuration says so (a
    /// static peer), as if an admin had approved it. Nothing to do for a
    /// key domain.
    pub async fn pin_configured(&self, domain: &str, key: &PublicKey) -> Result<(), TrustError> {
        if parse_key_domain(domain).ok().flatten().is_some() {
            return Ok(());
        }
        let current = self.store.trusted_key(domain).await?;
        if current.is_some_and(|pin| pin.key == key.as_bytes()) {
            return Ok(());
        }
        self.decide(domain, key, true).await
    }

    /// An admin's decision on a key: approving makes it the domain's
    /// trusted key (retiring the previous one), denying refuses it.
    pub async fn decide(
        &self,
        domain: &str,
        key: &PublicKey,
        approve: bool,
    ) -> Result<(), TrustError> {
        let status = if approve {
            PeerStatus::Trusted
        } else {
            PeerStatus::Denied
        };
        self.store
            .set_peer(domain, key.as_bytes(), status, self.clock.now_millis())
            .await?;
        Ok(())
    }
}
