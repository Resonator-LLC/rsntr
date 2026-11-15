//! Server-to-server federation for resonator over iroh.
//!
//! Wave's federation model is kept: the domain of a wavelet id is its one
//! host, which orders deltas and owns the hash chain; other servers keep
//! read-only replicas. What replaces the XMPP transport and certificate
//! chains of Apache Wave:
//!
//! - [`key`]: every server has one Ed25519 server key. It is the iroh
//!   endpoint identity and the delta-signing key. Its z-base-32 form (52
//!   characters) is a *key domain*: `user@<key domain>` needs no DNS.
//! - [`dns`]: a DNS domain binds to keys with a TXT record,
//!   `_resonator.<domain>. TXT "v=wn1; k=<z32 key>[; k=<key>]"`.
//! - [`rotation`]: signed key rotation statements.
//! - [`trust`]: trust on first use of domain bindings, rotation and
//!   quarantine, the allowlist policy, and the binding check
//!   [`trust::Trust::authorise`].
//! - [`signing`]: Ed25519 delta signatures, signer info, and verification
//!   with one signature per domain of the address path.
//! - [`codec`] and [`endpoint`]: the wire protocol (ALPN
//!   `resonator/fed/1`, length-prefixed protobuf frames) and the endpoint
//!   with its Hello, per-request streams, connection cache and accept
//!   loop.
//! - [`host`]: the host side of the wave operations (`submit`, `history`,
//!   `signer-info`, `post-signer-info`).
//! - [`replica`]: replicas of remote wavelets, fed by `update` and
//!   `commit`, with signature checks and gap filling.
//! - [`outbox`]: the fan-out of a host's deltas to remote servers through
//!   a durable outbox.
//! - [`remote`]: local users' submissions forwarded to remote hosts.
//! - [`service`]: federation as part of a running server.
//! - [`person`]: person identity (person keys from recovery phrases,
//!   device certificates, revocation lists): a domain whose devices act
//!   for one person, checked by [`trust`] alongside server keys.
//!
//! - [`pairing`]: pairing a new device with a person by link (ALPN
//!   `resonator/pair/1`).
//! - [`pair_code`]: pairing by a short code: the code, its rendezvous
//!   record on a pkarr relay, and SPAKE2.
//! - [`directory`]: the device directory of a person, a pkarr record
//!   under the person key listing its devices, by which a server that
//!   knows none of them reaches the person.
//! - [`cohost`]: co-host grants letting another domain hold a person's
//!   host lease while the person's devices are away.
//!
//! Attachments (`fetch-attachment`) are not here yet; see `HANDOFF.md`.

pub mod admin;
pub mod app_channel;
mod attachments;
pub mod codec;
pub mod cohost;
mod context;
pub mod control;
mod copies;
pub mod directory;
pub mod dns;
pub mod endpoint;
pub mod epoch;
pub mod host;
pub mod key;
pub mod lease;
pub mod outbox;
pub mod pair_code;
pub mod pairing;
pub mod person;
mod profiles;
pub mod push;
pub mod recovery;
pub mod remote;
pub mod replica;
pub mod rotation;
pub mod service;
pub mod signing;
mod sync;
pub mod trust;

pub use context::Tuning;
pub use epoch::{CoHosts, EpochAuthority, WaveletDomainOnly};
pub use person::{
    FileKeyStore, KeyStore, PersonBook, PersonDevices, PersonKey, RecoveryPhrase, VerifiedCert,
    VerifiedRevocations,
};

pub use endpoint::{ALPN, EndpointOptions, FederationEndpoint, LocalIdentity, Peer};
pub use key::{ServerKey, is_key_domain, key_domain, parse_key_domain};
pub use lease::{LeaseError, LeaseState, VerifiedLease};
pub use service::{Federation, FederationSetup, OwnDevice, SetupError, StaticPeer};
pub use signing::{Ed25519DeltaSigner, verify_delta};
pub use trust::{Authorisation, Policy, Trust};

/// Re-exported so that users need not depend on iroh themselves.
pub use iroh::{EndpointAddr, PublicKey, SecretKey};
