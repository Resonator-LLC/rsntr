//! Person mode: recovering a person on a fresh device from the recovery
//! phrase.
//!
//! The device holds the person key again (`person.key`), so it issues
//! itself a certificate at start. [`recover`] then:
//! 1. asks every peer it can reach what it knows of the person
//!    (`person-info`: certificates, revocation list, host lease, co-host
//!    grants);
//! 2. asks each peer which wavelets it has that the person takes part in
//!    or that are the person's (`holdings`) and pulls them with
//!    `history`, verifying signatures and the hash chain like any replica
//!    (before step 3, so that the old devices' epochs and deltas verify as
//!    they were made);
//! 3. publishes the next revocation list (`seq` + 1), revoking every
//!    other device of the person if asked to (a lost phone), which cuts
//!    those devices off everywhere it spreads;
//! 4. wakes the lease task, which claims the lease (at once if the holder
//!    was revoked) and takes the person's wavelets over.

use std::sync::Arc;

use iroh::PublicKey;
use resonator_model::id::WaveletName;
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    Holdings, HoldingsResult, PersonInfo, PersonInfoResult, SyncEntry, federation_request,
    federation_response,
};
use resonator_server::waveserver::WaveletProvider;

use crate::context::{
    Context, error, from_proto_version, parse_wavelet_name, to_proto_version, wavelet_uri,
};
use crate::endpoint::Peer;
use crate::person::{PersonKey, verify_revocations};

/// Most wavelets one `holdings` answer names.
const MAX_HOLDINGS: usize = 10_000;

/// Answers `person-info`.
pub(crate) fn person_info(ctx: &Context, request: PersonInfo) -> federation_response::Result {
    let Some(person) = request
        .person
        .as_deref()
        .and_then(|bytes| <&[u8; 32]>::try_from(bytes).ok())
        .and_then(|bytes| PublicKey::from_bytes(bytes).ok())
    else {
        return error(Code::BadRequest, "no person key");
    };
    let persons = ctx.trust.persons();
    let device_cert = persons
        .certified_devices()
        .into_iter()
        .filter_map(|device| persons.cert(&device, &person))
        .map(|cert| cert.signed)
        .collect();
    federation_response::Result::PersonInfo(PersonInfoResult {
        device_cert,
        revocations: persons.revocations(&person).map(|list| list.signed),
        host_lease: ctx
            .leases
            .current(&person)
            .map(|lease| vec![lease.signed])
            .unwrap_or_default(),
        co_host: ctx
            .trust
            .co_hosts()
            .of_person(&person)
            .into_iter()
            .map(|grant| grant.signed)
            .collect(),
    })
}

/// Answers `holdings`: the wavelets here that the asking server's
/// domains take part in or own.
pub(crate) async fn holdings(ctx: &Context, peer: &Peer) -> federation_response::Result {
    let mut entry = Vec::new();
    let Ok(waves) = ctx.server.get_wave_ids().await else {
        return federation_response::Result::Holdings(HoldingsResult { entry });
    };
    'waves: for wave in waves {
        let Ok(wavelets) = ctx.server.get_wavelet_ids(&wave).await else {
            continue;
        };
        for wavelet in wavelets {
            if entry.len() >= MAX_HOLDINGS {
                break 'waves;
            }
            let name = WaveletName::of(wave.clone(), wavelet);
            let owned = peer.speaks_for(name.wavelet_id.domain());
            if !owned && !crate::host::peer_participates(ctx, peer, &name).await {
                continue;
            }
            let Ok(version) = ctx.server.current_version(&name).await else {
                continue;
            };
            let Ok(version) = to_proto_version(&version) else {
                continue;
            };
            let latest = ctx.server.epochs(&name).pop();
            entry.push(SyncEntry {
                wavelet_name: Some(wavelet_uri(&name)),
                version: Some(version),
                epoch: latest.as_ref().map(|record| record.epoch),
                epoch_issued_at: latest.as_ref().and_then(crate::lease::epoch_issued_at),
                holder: latest.map(|record| record.holder),
            });
        }
    }
    federation_response::Result::Holdings(HoldingsResult { entry })
}

/// What [`recover`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Peers that answered.
    pub peers: usize,
    /// The revocation list published (`seq`).
    pub revocation_seq: u64,
    /// Devices revoked by it (all revoked so far).
    pub revoked: usize,
    /// Wavelets pulled.
    pub wavelets: usize,
}

/// Recovers the person of `key` on this device (see the module docs);
/// `peers` are asked besides the static and connected ones.
pub(crate) async fn recover(
    ctx: &Arc<Context>,
    key: &PersonKey,
    peers: &[PublicKey],
    revoke_old: bool,
) -> RecoveryReport {
    let person = key.public();
    let me = ctx.own_key();
    let mut report = RecoveryReport::default();
    let Ok(endpoint) = ctx.endpoint() else {
        return report;
    };
    let mut keys: Vec<PublicKey> = peers.to_vec();
    for key in endpoint
        .connected_peers()
        .into_iter()
        .chain(crate::context::lock(&ctx.static_peers).iter().copied())
    {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys.retain(|key| *key != me);
    // 1. What the peers know of the person.
    let mut answered = Vec::new();
    for peer in &keys {
        let op = federation_request::Op::PersonInfo(PersonInfo {
            person: Some(person.as_bytes().to_vec()),
        });
        let Ok(federation_response::Result::PersonInfo(info)) =
            endpoint.request(*peer, None, op).await
        else {
            tracing::info!(peer = %peer.fmt_short(), "a peer did not answer person-info");
            continue;
        };
        answered.push(*peer);
        for cert in &info.device_cert {
            let _ = ctx.trust.learn_cert(cert).await;
        }
        if let Some(list) = &info.revocations {
            let _ = ctx.trust.learn_revocations(list).await;
        }
        for grant in &info.co_host {
            let _ = ctx.trust.learn_co_host(grant).await;
        }
        for lease in &info.host_lease {
            let _ = ctx.trust.learn_lease(lease).await;
        }
    }
    report.peers = answered.len();
    // 2. The waves, before the old devices are revoked: their epochs and
    //    deltas verify as they were made.
    for peer in &answered {
        let op = federation_request::Op::Holdings(Holdings {});
        let Ok(federation_response::Result::Holdings(holdings)) =
            endpoint.request(*peer, None, op).await
        else {
            continue;
        };
        for entry in holdings.entry {
            let Ok(name) = parse_wavelet_name(entry.wavelet_name.as_deref()) else {
                continue;
            };
            let Ok(theirs) = from_proto_version(entry.version.as_ref()) else {
                continue;
            };
            let mine = ctx
                .server
                .current_version(&name)
                .await
                .map_or(0, |version| version.version());
            if theirs.version() <= mine && entry.epoch.unwrap_or(0) == 0 {
                continue;
            }
            match crate::sync::pull_wavelet_from(ctx, *peer, None, &name, &theirs).await {
                Ok(true) => report.wavelets += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::info!(wavelet = %name, peer = %peer.fmt_short(), %error, "cannot recover a wavelet");
                }
            }
        }
    }
    // 3. The next revocation list.
    let persons = ctx.trust.persons();
    let old: Vec<PublicKey> = if revoke_old {
        persons
            .certified_devices()
            .into_iter()
            .filter(|device| *device != me && persons.cert(device, &person).is_some())
            .collect()
    } else {
        Vec::new()
    };
    let previous = persons.revocations(&person);
    let signed = key.revoke(previous.as_ref(), old, ctx.now());
    match verify_revocations(&signed) {
        Ok(list) => {
            report.revocation_seq = list.seq;
            report.revoked = list.revoked.len();
            if let Err(error) = ctx.trust.learn_revocations(&signed).await {
                tracing::warn!(%error, "cannot record the recovery's revocation list");
            }
            // Spread at once to the peers that answered; the revocation
            // watcher tells everyone else.
            for peer in &answered {
                let op = federation_request::Op::Revocations(
                    resonator_model::proto::federation_rpc::Revocations {
                        person: Some(person.as_bytes().to_vec()),
                        list: Some(signed.clone()),
                    },
                );
                let _ = endpoint.request(*peer, None, op).await;
            }
        }
        Err(error) => tracing::warn!(%error, "cannot make the recovery's revocation list"),
    }
    // 4. The lease.
    ctx.leases.changed.notify_one();
    tracing::info!(
        peers = report.peers,
        wavelets = report.wavelets,
        revoked = report.revoked,
        "recovered the person on this device"
    );
    report
}
