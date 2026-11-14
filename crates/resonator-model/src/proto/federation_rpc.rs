//! The resonator federation wire protocol (ALPN `resonator/fed/1`). Not
//! part of Apache Wave: it replaces the XMPP transport of the original with
//! frames on QUIC streams.
//!
//! Every frame is a [`FederationFrame`] preceded by its length as a
//! big-endian `u32`. The first bidirectional stream of a connection carries
//! a [`Hello`] request and the [`Hello`] response; after that every request
//! has a stream of its own: the dialer writes one request frame and
//! finishes, the acceptor writes one response frame.
//!
//! Field tags 20 to 39 of the request and response bodies are the wave
//! operations: `submit` (20), `history` (21), `signer-info` (22),
//! `post-signer-info` (23), `update` (24) and `commit` (25), sent to a
//! wavelet's host (the first four) or by the host to the servers of its
//! participants (the last two). Tag 26 is fetch attachment; 27 is
//! `resubmit` (orphaned deltas after a host change, answered by tag 25),
//! and a request refused because a newer host epoch is known is answered
//! with `fenced` (26). Request tag 28 is `revocations` (fetch or push a
//! person's signed revocation list, answered by tag 27). Request tags 29 to 34
//! are `lease`, `sync-summary`, `pair` (only on ALPN `resonator/pair/1`),
//! `person-info`, `holdings` and `co-hosts`; 35 is `pair-code`, 36
//! `push-targets`, 37 `attachment-copies` (answered by `ack`) and 38
//! `own-attachments` (answered by tag 34); 39 is `fetch-profile`
//! (answered by tag 35).
//! Wavelets are named by their `wave://` URI, as in the original's
//! federation messages.
//!
//! Person identity: a *person key* signs [`DeviceCert`]s for the devices
//! (nodes) that act for the person, and [`RevocationList`]s withdrawing
//! them. A device presents its certificate in its [`Hello`] and with its
//! signer info.

use super::federation::{ProtocolHashedVersion, ProtocolSignedDelta, ProtocolSignerInfo};
use super::federation_error::FederationError;
use super::{ProtoError, WaveMessage, wave_message};

/// The protocol version spoken by this build.
pub const PROTOCOL_VERSION: u32 = 1;

/// A statement by a domain's old server key that `new_key` replaces it.
/// Signed as a [`SignedKeyRotation`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct KeyRotation {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub domain: Option<String>,
    /// Required. The raw 32-byte Ed25519 key being replaced.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub old_key: Option<Vec<u8>>,
    /// Required. The raw 32-byte Ed25519 key replacing it.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub new_key: Option<Vec<u8>>,
    /// Required. When the rotation was made, in milliseconds since the
    /// epoch.
    #[prost(int64, optional, tag = "4")]
    pub at: Option<i64>,
}
wave_message!(KeyRotation {
    required: [domain, old_key, new_key, at],
});

/// A [`KeyRotation`] with the old key's signature over its bytes.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedKeyRotation {
    /// Required. The serialised [`KeyRotation`], kept as signed.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub statement: Option<Vec<u8>>,
    /// Required. The old key's Ed25519 signature (64 bytes).
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedKeyRotation {
    required: [statement, signature],
});

/// The first request and response of a connection: who the server is.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Hello {
    /// Required. [`PROTOCOL_VERSION`] of the sender.
    #[prost(uint32, optional, tag = "1")]
    pub protocol_version: Option<u32>,
    /// The wave domains the sender serves.
    #[prost(string, repeated, tag = "2")]
    pub domain: Vec<String>,
    /// Optional features the sender supports.
    #[prost(string, repeated, tag = "3")]
    pub capability: Vec<String>,
    /// Rotation statements that chain the sender's key to keys its domains
    /// were known by before.
    #[prost(message, repeated, tag = "4")]
    pub key_rotation: Vec<SignedKeyRotation>,
    /// Person mode: the sender's certificate, signed by the person key of
    /// the domains it claims.
    #[prost(message, optional, tag = "5")]
    pub device_cert: Option<SignedDeviceCert>,
    /// Person mode: the `seq` of the newest revocation list of the
    /// sender's person that the sender knows (0 or absent: none).
    #[prost(uint64, optional, tag = "6")]
    pub revocation_seq: Option<u64>,
    /// In an answer: revocation lists of the dialer's person newer than
    /// the `revocation_seq` the dialer announced.
    #[prost(message, repeated, tag = "7")]
    pub revocations: Vec<SignedRevocationList>,
    /// Person mode: the newest host lease of the sender's person that the
    /// sender knows.
    #[prost(message, repeated, tag = "8")]
    pub host_lease: Vec<SignedHostLease>,
    /// Co-host grants: the sender's person's, and in an answer those
    /// naming the dialer's domain.
    #[prost(message, repeated, tag = "9")]
    pub co_host: Vec<SignedCoHost>,
}
wave_message!(Hello {
    required: [protocol_version],
    nested: [device_cert],
    repeated: [key_rotation, revocations, host_lease, co_host],
});

/// Person mode: which device of a person hosts the person's wavelets
/// (holds the host lease) until `expires_at`. Signed as a
/// [`SignedHostLease`] by `device` over `"resonator/host-lease/1\0"`
/// followed by the statement bytes. Leases are ordered by `term`, then by
/// `device` (the greater key wins a tie); the holder renews its lease
/// within the same term. A new term is valid if it was issued after the
/// previous lease expired, or carries the previous holder's handover.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct HostLease {
    /// Required. The raw 32-byte person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
    /// Required. The person domain the lease is for.
    #[prost(string, optional, tag = "2")]
    pub domain: Option<String>,
    /// Required; greater than 0.
    #[prost(uint64, optional, tag = "3")]
    pub term: Option<u64>,
    /// Required. The raw 32-byte key of the holding device.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub device: Option<Vec<u8>>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "5")]
    pub issued_at: Option<i64>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "6")]
    pub expires_at: Option<i64>,
    /// The previous holder's device key (absent for the first term).
    #[prost(bytes = "vec", optional, tag = "7")]
    pub prev: Option<Vec<u8>>,
    /// The previous holder's handover to `device`, if the term began
    /// with one.
    #[prost(message, optional, tag = "8")]
    pub handover: Option<SignedHostHandover>,
    /// Whether the holder is an anchor device (preferred holder).
    #[prost(bool, optional, tag = "9")]
    pub anchor: Option<bool>,
    /// Set when `device` is not a device of the person but of this
    /// domain, which a [`CoHost`] grant of the person lets hold the
    /// lease.
    #[prost(string, optional, tag = "10")]
    pub co_host_domain: Option<String>,
}
wave_message!(HostLease {
    required: [person, domain, term, device, issued_at, expires_at],
    nested: [handover],
});

/// A [`HostLease`] with the holding device's signature.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedHostLease {
    /// Required. The serialised [`HostLease`], kept as signed.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub lease: Option<Vec<u8>>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedHostLease {
    required: [lease, signature],
});

/// The holder of lease `term` gives the host role to device `to`. Signed
/// as a [`SignedHostHandover`] by `from` over
/// `"resonator/host-handover/1\0"` followed by the statement bytes.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct HostHandover {
    /// Required. The raw 32-byte person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
    /// Required. The term the handover ends.
    #[prost(uint64, optional, tag = "2")]
    pub term: Option<u64>,
    /// Required. The holder of `term`.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub from: Option<Vec<u8>>,
    /// Required. The device to hold the next term.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub to: Option<Vec<u8>>,
    /// Milliseconds since the epoch, informational.
    #[prost(int64, optional, tag = "5")]
    pub issued_at: Option<i64>,
}
wave_message!(HostHandover {
    required: [person, term, from, to],
});

/// A [`HostHandover`] with the signature of its `from` device.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedHostHandover {
    /// Required. The serialised [`HostHandover`], kept as signed.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub handover: Option<Vec<u8>>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedHostHandover {
    required: [handover, signature],
});

/// `lease`: tells a device of the same person (or anyone) of a host
/// lease, and/or hands the host role over to the receiver. Answered with
/// [`LeaseResult`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Lease {
    #[prost(message, repeated, tag = "1")]
    pub host_lease: Vec<SignedHostLease>,
    /// A handover to the receiver.
    #[prost(message, optional, tag = "2")]
    pub handover: Option<SignedHostHandover>,
}
wave_message!(Lease {
    nested: [handover],
    repeated: [host_lease],
});

/// The answer to [`Lease`]: the newest lease of the receiver's person it
/// knows, and whether the receiver is an anchor device.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct LeaseResult {
    #[prost(message, repeated, tag = "1")]
    pub host_lease: Vec<SignedHostLease>,
    #[prost(bool, optional, tag = "2")]
    pub anchor: Option<bool>,
}
wave_message!(LeaseResult {
    repeated: [host_lease],
});

/// One wavelet in a [`SyncSummary`]: how far the sender has it.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SyncEntry {
    /// Required. The wavelet's `wave://` URI.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// Required. The sender's current version.
    #[prost(message, optional, tag = "2")]
    pub version: Option<ProtocolHashedVersion>,
    /// The newest host epoch the sender knows (0: none).
    #[prost(uint64, optional, tag = "3")]
    pub epoch: Option<u64>,
    /// The holder of that epoch (raw key), if any.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub holder: Option<Vec<u8>>,
    /// When that epoch's statement was issued (milliseconds since the
    /// epoch).
    #[prost(int64, optional, tag = "5")]
    pub epoch_issued_at: Option<i64>,
}
wave_message!(SyncEntry {
    required: [wavelet_name, version],
    nested: [version],
});

/// `sync-summary`: between devices of one person, what each has of every
/// wavelet; the receiver answers with its own summary
/// ([`SyncSummaryResult`]) and each side pulls what it lacks with
/// `history`.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SyncSummary {
    #[prost(message, repeated, tag = "1")]
    pub entry: Vec<SyncEntry>,
}
wave_message!(SyncSummary { repeated: [entry] });

/// The answer to a [`SyncSummary`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SyncSummaryResult {
    #[prost(message, repeated, tag = "1")]
    pub entry: Vec<SyncEntry>,
}
wave_message!(SyncSummaryResult { repeated: [entry] });

/// A person key's statement that `device_key` (a node's iroh key) acts for
/// the person. Signed as a [`SignedDeviceCert`] by the person key over
/// `"resonator/device-cert/1\0"` followed by the statement bytes.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct DeviceCert {
    /// Required. The raw 32-byte Ed25519 person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
    /// Required. The raw 32-byte Ed25519 device key.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub device_key: Option<Vec<u8>>,
    /// A name for people to recognise the device by.
    #[prost(string, optional, tag = "3")]
    pub name: Option<String>,
    /// Whether the device holds the person key (may pair and revoke).
    #[prost(bool, optional, tag = "4")]
    pub admin: Option<bool>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "5")]
    pub issued_at: Option<i64>,
}
wave_message!(DeviceCert {
    required: [person, device_key, issued_at],
});

/// A [`DeviceCert`] with the person key's signature.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedDeviceCert {
    /// Required. The serialised [`DeviceCert`], kept as signed.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub cert: Option<Vec<u8>>,
    /// Required. The person key's Ed25519 signature (64 bytes).
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedDeviceCert {
    required: [cert, signature],
});

/// The devices a person has withdrawn. Each list replaces the previous
/// one: `seq` only increases, and a list holds every device revoked so
/// far. Signed as a [`SignedRevocationList`] by the person key over
/// `"resonator/revocations/1\0"` followed by the list bytes.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct RevocationList {
    /// Required. The raw 32-byte Ed25519 person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
    /// Required; greater than 0.
    #[prost(uint64, optional, tag = "2")]
    pub seq: Option<u64>,
    /// Raw 32-byte device keys.
    #[prost(bytes = "vec", repeated, tag = "3")]
    pub revoked: Vec<Vec<u8>>,
    /// Milliseconds since the epoch, informational.
    #[prost(int64, optional, tag = "4")]
    pub issued_at: Option<i64>,
}
wave_message!(RevocationList {
    required: [person, seq],
});

/// A [`RevocationList`] with the person key's signature.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedRevocationList {
    /// Required. The serialised [`RevocationList`], kept as signed.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub list: Option<Vec<u8>>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedRevocationList {
    required: [list, signature],
});

/// `revocations`: without `list`, asks for the newest revocation list the
/// receiver knows of `person`; with `list`, tells the receiver of it
/// (which keeps it if it is newer than what it has). Answered with
/// [`RevocationsResult`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Revocations {
    /// Required. The raw 32-byte person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
    #[prost(message, optional, tag = "2")]
    pub list: Option<SignedRevocationList>,
}
wave_message!(Revocations {
    required: [person],
    nested: [list],
});

/// The newest revocation list of a person that the receiver knows, if any.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct RevocationsResult {
    #[prost(message, optional, tag = "1")]
    pub list: Option<SignedRevocationList>,
}
wave_message!(RevocationsResult { nested: [list] });

/// Asks whether a participant address exists on the receiving server.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Probe {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub address: Option<String>,
}
wave_message!(Probe {
    required: [address],
});

/// The answer to a [`Probe`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProbeResult {
    /// Required.
    #[prost(bool, optional, tag = "1")]
    pub exists: Option<bool>,
}
wave_message!(ProbeResult { required: [exists] });

/// A payload sent back unchanged; for diagnostics.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Echo {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub payload: Option<Vec<u8>>,
}
wave_message!(Echo {});

/// Submits a signed delta to the wavelet's host (`submitRequest`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Submit {
    /// Required. The `wave://` URI of the wavelet.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// Required.
    #[prost(message, optional, tag = "2")]
    pub signed_delta: Option<ProtocolSignedDelta>,
}
wave_message!(Submit {
    required: [wavelet_name, signed_delta],
    nested: [signed_delta],
});

/// The outcome of a [`Submit`] (`SubmitRequestListener.onSuccess`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SubmitResult {
    /// Required. Zero if the delta transformed away.
    #[prost(int32, optional, tag = "1")]
    pub operations_applied: Option<i32>,
    /// Required.
    #[prost(message, optional, tag = "2")]
    pub hashed_version_after_application: Option<ProtocolHashedVersion>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "3")]
    pub application_timestamp: Option<i64>,
}
wave_message!(SubmitResult {
    required: [
        operations_applied,
        hashed_version_after_application,
        application_timestamp
    ],
    nested: [hashed_version_after_application],
});

/// Asks the host for applied deltas between two delta boundaries
/// (`requestHistory`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct History {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// Required.
    #[prost(message, optional, tag = "2")]
    pub start_version: Option<ProtocolHashedVersion>,
    /// Required.
    #[prost(message, optional, tag = "3")]
    pub end_version: Option<ProtocolHashedVersion>,
    /// At most this many bytes of applied deltas; the host may send fewer.
    #[prost(int64, optional, tag = "4")]
    pub length_limit: Option<i64>,
}
wave_message!(History {
    required: [wavelet_name, start_version, end_version],
    nested: [start_version, end_version],
});

/// The answer to a [`History`] request.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct HistoryResult {
    /// The serialised `ProtocolAppliedWaveletDelta`s, exactly as hashed.
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub applied_delta: Vec<Vec<u8>>,
    /// The host's committed version, if known.
    #[prost(message, optional, tag = "2")]
    pub last_committed_version: Option<ProtocolHashedVersion>,
    /// Set if the answer stops short of the end version: the version
    /// reached, from which to ask again.
    #[prost(int64, optional, tag = "3")]
    pub version_truncated_at: Option<i64>,
    /// The host epochs of the wavelet after epoch 0, oldest first.
    #[prost(message, repeated, tag = "4")]
    pub epoch_start: Vec<SignedEpochStart>,
}
wave_message!(HistoryResult {
    nested: [last_committed_version],
    repeated: [epoch_start],
});

/// Asks the host for the signer info of a signer of the delta ending at
/// `version` (`getDeltaSignerInfo`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct GetSignerInfo {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signer_id: Option<Vec<u8>>,
    /// Required. The resulting version of a delta the signer signed.
    #[prost(message, optional, tag = "3")]
    pub version: Option<ProtocolHashedVersion>,
}
wave_message!(GetSignerInfo {
    required: [wavelet_name, signer_id, version],
    nested: [version],
});

/// The answer to a [`GetSignerInfo`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignerInfoResult {
    /// Required.
    #[prost(message, optional, tag = "1")]
    pub signer_info: Option<ProtocolSignerInfo>,
}
wave_message!(SignerInfoResult {
    required: [signer_info],
    nested: [signer_info],
});

/// Gives a host the signer info of the sender's signer, before the
/// sender submits deltas it signed (`postSignerInfo`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PostSignerInfo {
    /// Required.
    #[prost(message, optional, tag = "1")]
    pub signer_info: Option<ProtocolSignerInfo>,
}
wave_message!(PostSignerInfo {
    required: [signer_info],
    nested: [signer_info],
});

/// Applied deltas of a wavelet, sent by its host to a server with
/// participants on it (`waveletDeltaUpdate`). Consecutive, in order.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Update {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// The serialised `ProtocolAppliedWaveletDelta`s, exactly as hashed.
    #[prost(bytes = "vec", repeated, tag = "2")]
    pub applied_delta: Vec<Vec<u8>>,
    /// The host epochs of the wavelet after epoch 0, oldest first; the
    /// last names the sender.
    #[prost(message, repeated, tag = "3")]
    pub epoch_start: Vec<SignedEpochStart>,
}
wave_message!(Update {
    required: [wavelet_name],
    repeated: [epoch_start],
});

/// The host has durably stored a wavelet up to a version
/// (`waveletCommitUpdate`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Commit {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// Required.
    #[prost(message, optional, tag = "2")]
    pub committed_version: Option<ProtocolHashedVersion>,
    /// As in [`Update`].
    #[prost(message, repeated, tag = "3")]
    pub epoch_start: Vec<SignedEpochStart>,
}
wave_message!(Commit {
    required: [wavelet_name, committed_version],
    nested: [committed_version],
    repeated: [epoch_start],
});

/// A statement that from `start_version` on, the host role of a wavelet
/// (or of all wavelets of a person, for the per-person lease of a later
/// phase) belongs to the holder of `holder_key` in host epoch `epoch`.
/// Epoch 0 is implicit: the wavelet domain's server from version 0. Signed
/// as a [`SignedEpochStart`] by `holder_key`.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct EpochStart {
    /// The wavelet's `wave://` URI; this or `person` is required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// A participant address: the statement covers the wavelets this
    /// person's lease decides about. Not used yet.
    #[prost(string, optional, tag = "2")]
    pub person: Option<String>,
    /// Required; greater than 0.
    #[prost(uint64, optional, tag = "3")]
    pub epoch: Option<u64>,
    /// Required (for a wavelet): the version and history hash the epoch
    /// starts at, a delta boundary of the previous epoch's history.
    #[prost(message, optional, tag = "4")]
    pub start_version: Option<ProtocolHashedVersion>,
    /// Required. The ed25519 public key of the new holder.
    #[prost(bytes = "vec", optional, tag = "5")]
    pub holder_key: Option<Vec<u8>>,
    /// Required. The domain the holder serves the wavelet as.
    #[prost(string, optional, tag = "6")]
    pub holder_domain: Option<String>,
    /// Milliseconds since the epoch, informational.
    #[prost(int64, optional, tag = "7")]
    pub issued_at: Option<i64>,
}
wave_message!(EpochStart {
    required: [epoch, holder_key, holder_domain],
    nested: [start_version],
});

/// An [`EpochStart`] with the holder's signature over
/// `"resonator/epoch/1\0"` followed by the statement bytes.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedEpochStart {
    /// Required. The serialised [`EpochStart`].
    #[prost(bytes = "vec", optional, tag = "1")]
    pub statement: Option<Vec<u8>>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedEpochStart {
    required: [statement, signature],
});

/// Applied deltas that a rewind took out of a wavelet's history, sent to
/// its current host to be applied again, rebased onto the history since
/// the fork. Consecutive, starting at `fork_version`, exactly as hashed
/// (`ProtocolAppliedWaveletDelta`s); the host deduplicates them by the
/// SHA-256 of their signed originals, so any server that held them may
/// send them, any number of times.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Resubmit {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// Required. Where the orphaned deltas branch off the host's history.
    #[prost(message, optional, tag = "2")]
    pub fork_version: Option<ProtocolHashedVersion>,
    #[prost(bytes = "vec", repeated, tag = "3")]
    pub applied_delta: Vec<Vec<u8>>,
    /// Signer infos of the deltas' signers that the host may lack.
    #[prost(message, repeated, tag = "4")]
    pub signer_info: Vec<ProtocolSignerInfo>,
}
wave_message!(Resubmit {
    required: [wavelet_name, fork_version],
    nested: [fork_version],
    repeated: [signer_info],
});

/// The answer to [`Resubmit`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ResubmitResult {
    /// Deltas applied now.
    #[prost(uint32, optional, tag = "1")]
    pub applied: Option<u32>,
    /// Deltas the host already had (deduplicated) or that transformed
    /// away.
    #[prost(uint32, optional, tag = "2")]
    pub skipped: Option<u32>,
    /// The host's version afterwards.
    #[prost(message, optional, tag = "3")]
    pub version: Option<ProtocolHashedVersion>,
}
wave_message!(ResubmitResult { nested: [version] });

/// The answer to an `update`, `commit` or `submit` from a server that is no
/// longer the wavelet's host: the newer epochs it has to adopt.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Fenced {
    #[prost(message, repeated, tag = "1")]
    pub epoch_start: Vec<SignedEpochStart>,
}
wave_message!(Fenced {
    repeated: [epoch_start],
});

/// The success answer of requests that return nothing else
/// (`post-signer-info`, `update`, `commit`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Ack {}
wave_message!(Ack {});

/// Asks the server an attachment was uploaded to for its bytes
/// (`fetch-attachment`). Answered only if the attachment belongs to the
/// wavelet and the asking server's domain has a participant on it.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FetchAttachment {
    /// Required. The attachment id, `<domain>/<id>`.
    #[prost(string, optional, tag = "1")]
    pub attachment_id: Option<String>,
    /// Required. The `wave://` URI of the wavelet the attachment is on.
    #[prost(string, optional, tag = "2")]
    pub wavelet_name: Option<String>,
}
wave_message!(FetchAttachment {
    required: [attachment_id, wavelet_name],
});

/// The answer to a [`FetchAttachment`]: what is about to be sent. The
/// bytes follow in [`Chunk`] frames on the same stream, `size` bytes in
/// all, then the stream ends.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FetchAttachmentResult {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub file_name: Option<String>,
    /// Required.
    #[prost(uint64, optional, tag = "2")]
    pub size: Option<u64>,
    /// Required. Address of the uploader.
    #[prost(string, optional, tag = "3")]
    pub creator: Option<String>,
    /// Required. SHA-256 of the bytes.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub sha256: Option<Vec<u8>>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "5")]
    pub created_at: Option<i64>,
}
wave_message!(FetchAttachmentResult {
    required: [file_name, size, creator, sha256, created_at],
});

/// `fetch-profile`: asks the domain of some addresses (its server, or a
/// device of the person) for the names and avatars their owners set, or
/// a device of a person asks another for the person's own. At most
/// [`MAX_FETCH_PROFILE`] wants.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FetchProfile {
    #[prost(message, repeated, tag = "1")]
    pub want: Vec<ProfileWant>,
}
wave_message!(FetchProfile { repeated: [want] });

/// Most wants in one [`FetchProfile`].
pub const MAX_FETCH_PROFILE: usize = 50;

/// One address asked for, and the version the asker has.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProfileWant {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub address: Option<String>,
    /// The `updated_at` of the version the asker has; only newer ones are
    /// sent.
    #[prost(int64, optional, tag = "2")]
    pub have_updated_at: Option<i64>,
}
wave_message!(ProfileWant {
    required: [address]
});

/// The answer to a [`FetchProfile`]: the newer versions. Addresses the
/// domain does not know, or has nothing newer of, are left out.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FetchProfileResult {
    #[prost(message, repeated, tag = "1")]
    pub profile: Vec<ProfileEntry>,
}
wave_message!(FetchProfileResult {
    repeated: [profile]
});

/// A version of a profile.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProfileEntry {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub address: Option<String>,
    /// Required. Empty: the name derived from the address.
    #[prost(string, optional, tag = "2")]
    pub name: Option<String>,
    /// Required. Milliseconds since the epoch, set by the owner.
    #[prost(int64, optional, tag = "3")]
    pub updated_at: Option<i64>,
    /// The avatar, a PNG; absent without one.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub avatar: Option<Vec<u8>>,
}
wave_message!(ProfileEntry {
    required: [address, name, updated_at]
});

/// A piece of the bytes following a response.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Chunk {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub data: Option<Vec<u8>>,
}
wave_message!(Chunk {});

/// A person's grant letting the devices of another domain (a friend's
/// node, a home server) take its host lease while the person's own
/// devices are away (*co-host*). Signed as a [`SignedCoHost`] by the
/// person key over `"resonator/co-host/1\0"` followed by the statement
/// bytes. The newest grant (by `issued_at`) of a person for a delegate
/// domain replaces older ones; `until` 0 (or in the past) withdraws it.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct CoHost {
    /// Required. The raw 32-byte person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
    /// Required. The domain whose devices may hold the lease.
    #[prost(string, optional, tag = "2")]
    pub delegate_domain: Option<String>,
    /// Required. Milliseconds since the epoch; the grant ends then.
    #[prost(int64, optional, tag = "3")]
    pub until: Option<i64>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "4")]
    pub issued_at: Option<i64>,
}
wave_message!(CoHost {
    required: [person, delegate_domain, until, issued_at],
});

/// A [`CoHost`] with the person key's signature.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedCoHost {
    /// Required. The serialised [`CoHost`], kept as signed.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub grant: Option<Vec<u8>>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedCoHost {
    required: [grant, signature],
});

/// `co-hosts`: tells the receiver of co-host grants (it keeps the newer
/// ones). Answered with `Ack`.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct CoHosts {
    #[prost(message, repeated, tag = "1")]
    pub co_host: Vec<SignedCoHost>,
}
wave_message!(CoHosts {
    repeated: [co_host],
});

/// Where and how a device can be woken by a silent push: the push relay
/// it uses and its push token sealed by that relay (opaque; only the
/// relay can open it). Signed as a [`SignedPushTarget`] by the device key
/// over `"resonator/push-target/1\0"` followed by the statement bytes. The
/// newest (by `issued_at`) of a device replaces older ones; an empty
/// `sealed` withdraws it.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PushTarget {
    /// Required. The raw 32-byte device key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub device: Option<Vec<u8>>,
    /// Required. The relay's base URL.
    #[prost(string, optional, tag = "2")]
    pub relay: Option<String>,
    /// Required. The sealed token (empty: none).
    #[prost(string, optional, tag = "3")]
    pub sealed: Option<String>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "4")]
    pub issued_at: Option<i64>,
}
wave_message!(PushTarget {
    required: [device, relay, sealed, issued_at],
});

/// A [`PushTarget`] with the device key's signature.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SignedPushTarget {
    /// Required. The serialised [`PushTarget`], kept as signed.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub target: Option<Vec<u8>>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signature: Option<Vec<u8>>,
}
wave_message!(SignedPushTarget {
    required: [target, signature],
});

/// `push-targets`: tells the receiver how to wake the sender and the
/// other devices of its person (it keeps the newer ones). Answered with
/// `Ack`; refused by nodes without push.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PushTargets {
    #[prost(message, repeated, tag = "1")]
    pub push_target: Vec<SignedPushTarget>,
}
wave_message!(PushTargets {
    repeated: [push_target],
});

/// One attachment of a person's own domain that a device holds, as told
/// to the person's other devices so they can copy it (with
/// `fetch-attachment`) while the device holding it is awake.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AttachmentNotice {
    /// Required. The attachment id, `<domain>/<id>`.
    #[prost(string, optional, tag = "1")]
    pub attachment_id: Option<String>,
    /// Required. The `wave://` URI of the wavelet the attachment is on.
    #[prost(string, optional, tag = "2")]
    pub wavelet_name: Option<String>,
    /// Required. Bytes.
    #[prost(uint64, optional, tag = "3")]
    pub size: Option<u64>,
    /// Required. SHA-256 of the bytes.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub sha256: Option<Vec<u8>>,
    /// Whether it is an image (with a thumbnail).
    #[prost(bool, optional, tag = "5")]
    pub image: Option<bool>,
    /// Milliseconds since the epoch; orders `own-attachments`.
    #[prost(int64, optional, tag = "6")]
    pub created_at: Option<i64>,
}
wave_message!(AttachmentNotice {
    required: [attachment_id, wavelet_name, size, sha256],
});

/// `attachment-copies`: between devices of one person, attachments just
/// uploaded to the sender; the receiver copies them in the background
/// with `fetch-attachment`. Answered with `Ack` at once.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AttachmentCopies {
    #[prost(message, repeated, tag = "1")]
    pub notice: Vec<AttachmentNotice>,
}
wave_message!(AttachmentCopies { repeated: [notice] });

/// `own-attachments`: between devices of one person, a page of the
/// attachments of the person's domain the receiver holds, ordered by
/// (`created_at`, id), after the given position.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct OwnAttachments {
    /// List after this `created_at` (with `after_id`); none: from the
    /// start.
    #[prost(int64, optional, tag = "1")]
    pub after_created_at: Option<i64>,
    #[prost(string, optional, tag = "2")]
    pub after_id: Option<String>,
    /// Most entries wanted.
    #[prost(uint32, optional, tag = "3")]
    pub limit: Option<u32>,
}
wave_message!(OwnAttachments {});

/// The answer to [`OwnAttachments`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct OwnAttachmentsResult {
    #[prost(message, repeated, tag = "1")]
    pub notice: Vec<AttachmentNotice>,
    /// Whether more follow.
    #[prost(bool, optional, tag = "2")]
    pub more: Option<bool>,
}
wave_message!(OwnAttachmentsResult { repeated: [notice] });

/// What a pairing link (`resonator:pair/<base64url>`) carries: how to
/// reach the inviting device and the one-time secret proving the joiner
/// was shown the link. Never sent over the network.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PairInvitation {
    /// Required. The raw 32-byte person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
    /// Required. The inviting device's raw 32-byte key.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub device: Option<Vec<u8>>,
    /// Socket addresses (`host:port`) of the inviting device.
    #[prost(string, repeated, tag = "3")]
    pub addr: Vec<String>,
    /// The inviting device's relay URL, if any.
    #[prost(string, optional, tag = "4")]
    pub relay: Option<String>,
    /// Required. Names the invitation.
    #[prost(string, optional, tag = "5")]
    pub id: Option<String>,
    /// Required. 16 random bytes, used once.
    #[prost(bytes = "vec", optional, tag = "6")]
    pub secret: Option<Vec<u8>>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "7")]
    pub expires_at: Option<i64>,
    /// Whether the new device gets the person key (admin).
    #[prost(bool, optional, tag = "8")]
    pub admin: Option<bool>,
    /// The device-mode account name of the person.
    #[prost(string, optional, tag = "9")]
    pub account: Option<String>,
}
wave_message!(PairInvitation {
    required: [person, device, id, secret, expires_at],
});

/// `pair` (ALPN `resonator/pair/1` only): a new device asks the inviting
/// device for a certificate. `mac` is HMAC-SHA256 keyed with the
/// invitation's secret over `"resonator/pair/1\0"`, the inviter's key,
/// the joiner's key (that of the connection) and the invitation id.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Pair {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub invitation: Option<String>,
    /// Required. 32 bytes.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub mac: Option<Vec<u8>>,
    /// A name for the new device.
    #[prost(string, optional, tag = "3")]
    pub device_name: Option<String>,
    /// Socket addresses of the new device.
    #[prost(string, repeated, tag = "4")]
    pub addr: Vec<String>,
}
wave_message!(Pair {
    required: [invitation, mac],
});

/// The answer to [`Pair`]. `waiting`: an admin pairing waits for the
/// inviting device to confirm it; ask again. Otherwise the new device's
/// certificate and what it needs to start as a device of the person.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PairResult {
    #[prost(bool, optional, tag = "1")]
    pub waiting: Option<bool>,
    /// The new device's certificate.
    #[prost(message, optional, tag = "2")]
    pub cert: Option<SignedDeviceCert>,
    /// Certificates of the person's other devices.
    #[prost(message, repeated, tag = "3")]
    pub device_cert: Vec<SignedDeviceCert>,
    #[prost(message, optional, tag = "4")]
    pub revocations: Option<SignedRevocationList>,
    #[prost(message, repeated, tag = "5")]
    pub host_lease: Vec<SignedHostLease>,
    /// Admin pairing: the person key's 32-byte BIP39 entropy, XORed with
    /// HKDF-SHA256(secret, `"resonator/pair/1 key"` ‖ inviter ‖ joiner).
    #[prost(bytes = "vec", optional, tag = "6")]
    pub sealed_key: Option<Vec<u8>>,
    /// Known device addresses: `<z32 key> <host:port>`.
    #[prost(string, repeated, tag = "7")]
    pub device_addr: Vec<String>,
    #[prost(message, repeated, tag = "8")]
    pub co_host: Vec<SignedCoHost>,
    /// The device-mode account name of the person.
    #[prost(string, optional, tag = "9")]
    pub account: Option<String>,
    /// Pairing by code: the inviting device's key confirmation,
    /// HMAC-SHA256(session key, `"resonator/pair-code/1 inviter\0"` ‖
    /// inviter ‖ joiner ‖ invitation id). Sent once the joiner's MAC
    /// checked out (also while waiting for confirmation).
    #[prost(bytes = "vec", optional, tag = "10")]
    pub confirm: Option<Vec<u8>>,
    /// Pairing by code: the joiner's MAC was wrong (a wrong code); how
    /// many attempts the invitation has left (0: it is burnt).
    #[prost(uint32, optional, tag = "11")]
    pub tries_left: Option<u32>,
}
wave_message!(PairResult {
    nested: [cert, revocations],
    repeated: [device_cert, host_lease, co_host],
});

/// `pair-code` (ALPN `resonator/pair/1` only): the start of pairing by a
/// short code (`<nameplate>-<word>-<word>`). `pake` is the joiner's
/// SPAKE2 message (side A) over the code's words; the identities bind the
/// joiner's key (that of the connection), the inviter's key and the
/// nameplate. Then the joiner sends [`Pair`] with a MAC keyed by the
/// session key the PAKE gave.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PairCode {
    /// Required. The digits of the code.
    #[prost(string, optional, tag = "1")]
    pub nameplate: Option<String>,
    /// Required. 33 bytes.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub pake: Option<Vec<u8>>,
}
wave_message!(PairCode {
    required: [nameplate, pake],
});

/// The answer to [`PairCode`]: the inviting device's SPAKE2 message (side
/// B) and the invitation it is about. Nothing here is trusted before the
/// key confirmation ([`PairResult::confirm`]): the session key binds every
/// field, so a changed one makes the MACs fail.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PairCodeResult {
    /// Required. 33 bytes.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub pake: Option<Vec<u8>>,
    /// Required. The invitation id, for [`Pair::invitation`].
    #[prost(string, optional, tag = "2")]
    pub invitation: Option<String>,
    /// Required. The raw 32-byte person key.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub person: Option<Vec<u8>>,
    /// Whether the new device gets the person key (admin).
    #[prost(bool, optional, tag = "4")]
    pub admin: Option<bool>,
    /// The device-mode account name of the person.
    #[prost(string, optional, tag = "5")]
    pub account: Option<String>,
    /// Milliseconds since the epoch.
    #[prost(int64, optional, tag = "6")]
    pub expires_at: Option<i64>,
}
wave_message!(PairCodeResult {
    required: [pake, invitation, person],
});

/// `person-info`: what the receiver knows of a person: device
/// certificates, the revocation list, the host lease and co-host grants.
/// Asked by a device recovering its person.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PersonInfo {
    /// Required. The raw 32-byte person key.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub person: Option<Vec<u8>>,
}
wave_message!(PersonInfo { required: [person] });

/// The answer to [`PersonInfo`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct PersonInfoResult {
    #[prost(message, repeated, tag = "1")]
    pub device_cert: Vec<SignedDeviceCert>,
    #[prost(message, optional, tag = "2")]
    pub revocations: Option<SignedRevocationList>,
    #[prost(message, repeated, tag = "3")]
    pub host_lease: Vec<SignedHostLease>,
    #[prost(message, repeated, tag = "4")]
    pub co_host: Vec<SignedCoHost>,
}
wave_message!(PersonInfoResult {
    nested: [revocations],
    repeated: [device_cert, host_lease, co_host],
});

/// `holdings`: the wavelets the receiver has in which the sender's
/// domain takes part or that are of the sender's domain, with their
/// versions (a recovering device pulls them with `history`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Holdings {}
wave_message!(Holdings {});

/// The answer to [`Holdings`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct HoldingsResult {
    #[prost(message, repeated, tag = "1")]
    pub entry: Vec<SyncEntry>,
}
wave_message!(HoldingsResult { repeated: [entry] });

/// A request from one server to another.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FederationRequest {
    /// Required. A ULID naming the request; the receiver uses it to
    /// recognise retries.
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(
        oneof = "federation_request::Op",
        tags = "10, 11, 12, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39"
    )]
    pub op: Option<federation_request::Op>,
}

pub mod federation_request {
    #[derive(Clone, PartialEq, Eq, prost::Oneof)]
    pub enum Op {
        #[prost(message, tag = "10")]
        Hello(super::Hello),
        #[prost(message, tag = "11")]
        Probe(super::Probe),
        #[prost(message, tag = "12")]
        Echo(super::Echo),
        #[prost(message, tag = "20")]
        Submit(super::Submit),
        #[prost(message, tag = "21")]
        History(super::History),
        #[prost(message, tag = "22")]
        GetSignerInfo(super::GetSignerInfo),
        #[prost(message, tag = "23")]
        PostSignerInfo(super::PostSignerInfo),
        #[prost(message, tag = "24")]
        Update(super::Update),
        #[prost(message, tag = "25")]
        Commit(super::Commit),
        #[prost(message, tag = "26")]
        FetchAttachment(super::FetchAttachment),
        #[prost(message, tag = "27")]
        Resubmit(super::Resubmit),
        #[prost(message, tag = "28")]
        Revocations(super::Revocations),
        #[prost(message, tag = "29")]
        Lease(super::Lease),
        #[prost(message, tag = "30")]
        SyncSummary(super::SyncSummary),
        /// Only on ALPN `resonator/pair/1`.
        #[prost(message, tag = "31")]
        Pair(super::Pair),
        #[prost(message, tag = "32")]
        PersonInfo(super::PersonInfo),
        #[prost(message, tag = "33")]
        Holdings(super::Holdings),
        #[prost(message, tag = "34")]
        CoHosts(super::CoHosts),
        /// Only on ALPN `resonator/pair/1`.
        #[prost(message, tag = "35")]
        PairCode(super::PairCode),
        #[prost(message, tag = "36")]
        PushTargets(super::PushTargets),
        #[prost(message, tag = "37")]
        AttachmentCopies(super::AttachmentCopies),
        #[prost(message, tag = "38")]
        OwnAttachments(super::OwnAttachments),
        #[prost(message, tag = "39")]
        FetchProfile(super::FetchProfile),
    }
}

impl WaveMessage for FederationRequest {
    fn check_initialized(&self) -> Result<(), ProtoError> {
        if self.id.is_none() {
            return Err(ProtoError::MissingField("FederationRequest.id"));
        }
        match &self.op {
            None => Err(ProtoError::MissingField("FederationRequest.op")),
            Some(federation_request::Op::Hello(m)) => m.check_initialized(),
            Some(federation_request::Op::Probe(m)) => m.check_initialized(),
            Some(federation_request::Op::Echo(m)) => m.check_initialized(),
            Some(federation_request::Op::Submit(m)) => m.check_initialized(),
            Some(federation_request::Op::History(m)) => m.check_initialized(),
            Some(federation_request::Op::GetSignerInfo(m)) => m.check_initialized(),
            Some(federation_request::Op::PostSignerInfo(m)) => m.check_initialized(),
            Some(federation_request::Op::Update(m)) => m.check_initialized(),
            Some(federation_request::Op::Commit(m)) => m.check_initialized(),
            Some(federation_request::Op::FetchAttachment(m)) => m.check_initialized(),
            Some(federation_request::Op::Resubmit(m)) => m.check_initialized(),
            Some(federation_request::Op::Revocations(m)) => m.check_initialized(),
            Some(federation_request::Op::Lease(m)) => m.check_initialized(),
            Some(federation_request::Op::SyncSummary(m)) => m.check_initialized(),
            Some(federation_request::Op::Pair(m)) => m.check_initialized(),
            Some(federation_request::Op::PersonInfo(m)) => m.check_initialized(),
            Some(federation_request::Op::Holdings(m)) => m.check_initialized(),
            Some(federation_request::Op::CoHosts(m)) => m.check_initialized(),
            Some(federation_request::Op::PairCode(m)) => m.check_initialized(),
            Some(federation_request::Op::PushTargets(m)) => m.check_initialized(),
            Some(federation_request::Op::AttachmentCopies(m)) => m.check_initialized(),
            Some(federation_request::Op::OwnAttachments(m)) => m.check_initialized(),
            Some(federation_request::Op::FetchProfile(m)) => m.check_initialized(),
        }
    }
}

/// The answer to a [`FederationRequest`], with the same id.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FederationResponse {
    /// Required. The id of the request answered.
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(
        oneof = "federation_response::Result",
        tags = "2, 10, 11, 12, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35"
    )]
    pub result: Option<federation_response::Result>,
}

pub mod federation_response {
    #[derive(Clone, PartialEq, Eq, prost::Oneof)]
    pub enum Result {
        #[prost(message, tag = "2")]
        Error(super::FederationError),
        #[prost(message, tag = "10")]
        Hello(super::Hello),
        #[prost(message, tag = "11")]
        Probe(super::ProbeResult),
        #[prost(message, tag = "12")]
        Echo(super::Echo),
        #[prost(message, tag = "20")]
        Submit(super::SubmitResult),
        #[prost(message, tag = "21")]
        History(super::HistoryResult),
        #[prost(message, tag = "22")]
        SignerInfo(super::SignerInfoResult),
        /// The answer to `post-signer-info`, `update` and `commit`.
        #[prost(message, tag = "23")]
        Ack(super::Ack),
        #[prost(message, tag = "24")]
        FetchAttachment(super::FetchAttachmentResult),
        #[prost(message, tag = "25")]
        Resubmit(super::ResubmitResult),
        #[prost(message, tag = "26")]
        Fenced(super::Fenced),
        #[prost(message, tag = "27")]
        Revocations(super::RevocationsResult),
        #[prost(message, tag = "28")]
        Lease(super::LeaseResult),
        #[prost(message, tag = "29")]
        SyncSummary(super::SyncSummaryResult),
        #[prost(message, tag = "30")]
        Pair(super::PairResult),
        #[prost(message, tag = "31")]
        PersonInfo(super::PersonInfoResult),
        #[prost(message, tag = "32")]
        Holdings(super::HoldingsResult),
        #[prost(message, tag = "33")]
        PairCode(super::PairCodeResult),
        #[prost(message, tag = "34")]
        OwnAttachments(super::OwnAttachmentsResult),
        #[prost(message, tag = "35")]
        FetchProfile(super::FetchProfileResult),
    }
}

impl WaveMessage for FederationResponse {
    fn check_initialized(&self) -> Result<(), ProtoError> {
        if self.id.is_none() {
            return Err(ProtoError::MissingField("FederationResponse.id"));
        }
        match &self.result {
            None => Err(ProtoError::MissingField("FederationResponse.result")),
            Some(federation_response::Result::Error(m)) => m.check_initialized(),
            Some(federation_response::Result::Hello(m)) => m.check_initialized(),
            Some(federation_response::Result::Probe(m)) => m.check_initialized(),
            Some(federation_response::Result::Echo(m)) => m.check_initialized(),
            Some(federation_response::Result::Submit(m)) => m.check_initialized(),
            Some(federation_response::Result::History(m)) => m.check_initialized(),
            Some(federation_response::Result::SignerInfo(m)) => m.check_initialized(),
            Some(federation_response::Result::Ack(m)) => m.check_initialized(),
            Some(federation_response::Result::FetchAttachment(m)) => m.check_initialized(),
            Some(federation_response::Result::Resubmit(m)) => m.check_initialized(),
            Some(federation_response::Result::Fenced(m)) => m.check_initialized(),
            Some(federation_response::Result::Revocations(m)) => m.check_initialized(),
            Some(federation_response::Result::Lease(m)) => m.check_initialized(),
            Some(federation_response::Result::SyncSummary(m)) => m.check_initialized(),
            Some(federation_response::Result::Pair(m)) => m.check_initialized(),
            Some(federation_response::Result::PersonInfo(m)) => m.check_initialized(),
            Some(federation_response::Result::Holdings(m)) => m.check_initialized(),
            Some(federation_response::Result::PairCode(m)) => m.check_initialized(),
            Some(federation_response::Result::OwnAttachments(m)) => m.check_initialized(),
            Some(federation_response::Result::FetchProfile(m)) => m.check_initialized(),
        }
    }
}

/// One frame on a federation stream.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FederationFrame {
    #[prost(oneof = "federation_frame::Body", tags = "1, 2, 3")]
    pub body: Option<federation_frame::Body>,
}

pub mod federation_frame {
    #[derive(Clone, PartialEq, Eq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "1")]
        Request(super::FederationRequest),
        #[prost(message, tag = "2")]
        Response(super::FederationResponse),
        /// Bytes following a response (`fetch-attachment`).
        #[prost(message, tag = "3")]
        Chunk(super::Chunk),
    }
}

impl WaveMessage for FederationFrame {
    fn check_initialized(&self) -> Result<(), ProtoError> {
        match &self.body {
            None => Err(ProtoError::MissingField("FederationFrame.body")),
            Some(federation_frame::Body::Request(m)) => m.check_initialized(),
            Some(federation_frame::Body::Response(m)) => m.check_initialized(),
            Some(federation_frame::Body::Chunk(m)) => m.check_initialized(),
        }
    }
}
