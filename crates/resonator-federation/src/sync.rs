//! Person mode: device sync. The devices of a person compare what they
//! have of every wavelet (`sync-summary`: version, hash and host epoch of
//! each) when they meet and periodically, and each pulls what it lacks
//! from the other with `history`: the person's own wavelets (the holder's
//! replicas), replicas of other people's wavelets, and the user-data
//! wavelets that hold read state. A device that was offline catches up
//! from whichever device of its person it reaches first.
//!
//! Also here: the drain of pending submissions
//! ([`WaveServer::drain_pending`](resonator_server::waveserver::WaveServer::drain_pending)),
//! woken when peers connect and the host lease moves.

use std::sync::Arc;

use iroh::PublicKey;
use resonator_model::id::WaveletName;
use resonator_model::proto::ByteStringMessage;
use resonator_model::proto::federation_error::{FederationError, federation_error::Code};
use resonator_model::proto::federation_rpc::{
    History, SyncEntry, SyncSummary, SyncSummaryResult, federation_request, federation_response,
};
use resonator_server::storage::AppliedDelta;
use resonator_server::waveserver::{ReplicaOutcome, WaveletProvider};
use tokio_util::sync::CancellationToken;

use crate::context::{
    Context, error, from_proto_version, parse_wavelet_name, to_proto_version, wavelet_uri,
};
use crate::endpoint::{Peer, RequestError};

/// What this device has of every wavelet.
async fn summary(ctx: &Context) -> Vec<SyncEntry> {
    let mut entries = Vec::new();
    let Ok(waves) = ctx.server.get_wave_ids().await else {
        return entries;
    };
    for wave in waves {
        let Ok(wavelets) = ctx.server.get_wavelet_ids(&wave).await else {
            continue;
        };
        for wavelet in wavelets {
            let name = WaveletName::of(wave.clone(), wavelet);
            let Ok(version) = ctx.server.current_version(&name).await else {
                continue;
            };
            let Ok(version) = to_proto_version(&version) else {
                continue;
            };
            let latest = ctx.server.epochs(&name).pop();
            entries.push(SyncEntry {
                wavelet_name: Some(wavelet_uri(&name)),
                version: Some(version),
                epoch: latest.as_ref().map(|record| record.epoch),
                epoch_issued_at: latest.as_ref().and_then(crate::lease::epoch_issued_at),
                holder: latest.map(|record| record.holder),
            });
        }
    }
    entries
}

/// Answers `sync-summary` from another device of this person with this
/// device's summary, and pulls what the other device has more of.
pub(crate) async fn serve(
    ctx: &Arc<Context>,
    peer: &Peer,
    request: SyncSummary,
) -> federation_response::Result {
    if !ctx.is_own_device(&peer.key) {
        return error(
            Code::NotAuthorized,
            "sync-summary is for devices of one person",
        );
    }
    let entries = summary(ctx).await;
    let ctx = Arc::clone(ctx);
    let key = peer.key;
    tokio::spawn(async move {
        pull(&ctx, key, request.entry).await;
    });
    federation_response::Result::SyncSummary(SyncSummaryResult { entry: entries })
}

/// Exchanges summaries with `key` (another device of this person) and
/// pulls what it has more of; returns how many wavelets moved on.
pub(crate) async fn exchange(ctx: &Arc<Context>, key: PublicKey) -> Result<usize, RequestError> {
    let entries = summary(ctx).await;
    let answer = ctx
        .endpoint()?
        .request(
            key,
            Some(&ctx.domain),
            federation_request::Op::SyncSummary(SyncSummary { entry: entries }),
        )
        .await?;
    let federation_response::Result::SyncSummary(theirs) = answer else {
        return Ok(0);
    };
    Ok(pull(ctx, key, theirs.entry).await)
}

/// Pulls from `key` what its summary shows this device lacks.
async fn pull(ctx: &Arc<Context>, key: PublicKey, theirs: Vec<SyncEntry>) -> usize {
    let me = ctx.own_key();
    let term_start = ctx.leases.term_start(&me);
    let mut moved = 0;
    let mut imported = Vec::new();
    for entry in theirs {
        let Ok(name) = parse_wavelet_name(entry.wavelet_name.as_deref()) else {
            continue;
        };
        let Ok(their_version) = from_proto_version(entry.version.as_ref()) else {
            continue;
        };
        let latest = ctx.server.epochs(&name).pop();
        let my_epoch = latest.as_ref().map_or(0, |record| record.epoch);
        let their_epoch = entry.epoch.unwrap_or(0);
        let own = name.wavelet_id.domain() == ctx.domain;
        // Split brain: the other device hosts a wavelet of the person
        // under an epoch at least as new as this holder's, begun before
        // this holder's term (one begun during the term is a deliberate
        // move). The holder takes it over above that; the other device
        // rewinds and resubmits what it applied meanwhile.
        if own
            && let Some(since) = term_start
            && their_epoch >= my_epoch
            && their_epoch > 0
            && entry.holder.as_deref() != Some(me.as_bytes().as_slice())
            && entry.epoch_issued_at.unwrap_or(0) < since
        {
            crate::lease::take_over_at(ctx, &name, their_epoch.max(my_epoch) + 1).await;
            moved += 1;
            continue;
        }
        let Ok(mine) = ctx.server.current_version(&name).await else {
            continue;
        };
        if their_version.version() <= mine.version() && their_epoch <= my_epoch {
            continue;
        }
        match pull_wavelet(ctx, key, &name, &their_version).await {
            Ok(true) => {
                moved += 1;
                if own && ctx.server.is_local_wavelet(&name) {
                    imported.push(name);
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(wavelet = %name, peer = %key.fmt_short(), %error, "cannot sync a wavelet");
            }
        }
    }
    if let Some(since) = term_start
        && !imported.is_empty()
    {
        // History written by an earlier holder: this holder's from now on.
        crate::lease::take_over_wavelets(ctx, &imported, since).await;
    }
    if moved > 0 {
        tracing::debug!(peer = %key.fmt_short(), wavelets = moved, "synced with another device");
    }
    moved
}

/// Pulls one wavelet's history (and host epochs) from `key` up to what it
/// has; returns whether anything changed here.
async fn pull_wavelet(
    ctx: &Context,
    key: PublicKey,
    name: &WaveletName,
    target: &resonator_model::version::HashedVersion,
) -> Result<bool, FederationError> {
    let domain = ctx.domain.clone();
    pull_wavelet_from(ctx, key, Some(&domain), name, target).await
}

/// [`pull_wavelet`] from a server that must be authorised for `domain`
/// (any, if `None`).
pub(crate) async fn pull_wavelet_from(
    ctx: &Context,
    key: PublicKey,
    domain: Option<&str>,
    name: &WaveletName,
    target: &resonator_model::version::HashedVersion,
) -> Result<bool, FederationError> {
    let queue = ctx.replica(name);
    let _queue = queue.lock().await;
    let failed = |message: String| FederationError::new(Code::UndefinedCondition, message);
    let endpoint = ctx.endpoint().map_err(|e| e.to_federation_error())?;
    let mut changed = false;
    // Bounded: each round either applies deltas or adopts epochs.
    for _ in 0..1000 {
        let current = ctx
            .server
            .current_version(name)
            .await
            .map_err(|e| failed(e.to_string()))?;
        // At the target already: only the host epochs may be news.
        let caught_up = current.version() >= target.version();
        let end = if caught_up { &current } else { target };
        let request = History {
            wavelet_name: Some(wavelet_uri(name)),
            start_version: Some(to_proto_version(&current)?),
            end_version: Some(to_proto_version(end)?),
            length_limit: i64::try_from(ctx.tuning.page_bytes).ok(),
        };
        let answer = endpoint
            .request(key, domain, federation_request::Op::History(request))
            .await
            .map_err(|e| e.to_federation_error())?;
        let federation_response::Result::History(history) = answer else {
            return Err(failed("history answered with something else".to_owned()));
        };
        if !history.epoch_start.is_empty()
            && crate::epoch::adopt(ctx, name, &history.epoch_start)
                .await?
                .changed
        {
            changed = true;
            if caught_up {
                return Ok(changed);
            }
            continue;
        }
        if caught_up || history.applied_delta.is_empty() {
            return Ok(changed);
        }
        for bytes in history.applied_delta {
            let applied: AppliedDelta = ByteStringMessage::parse_from(bytes)
                .map_err(|e| FederationError::new(Code::BadRequest, format!("bad delta: {e}")))?;
            crate::replica::verify_from(ctx, name, &applied, Some((key, domain))).await?;
            let outcome = if ctx.server.is_local_wavelet(name) {
                ctx.server.import_delta(name, applied).await
            } else {
                ctx.server.apply_replica_delta(name, applied).await
            }
            .map_err(|e| failed(e.to_string()))?;
            match outcome {
                ReplicaOutcome::Applied(_) => changed = true,
                ReplicaOutcome::AlreadyApplied => {}
                ReplicaOutcome::Ahead { .. } => {
                    return Err(failed("the history does not continue this one".to_owned()));
                }
            }
        }
    }
    Ok(changed)
}

/// Device sync: with every other device of the person, now and then and
/// when one says Hello.
pub(crate) async fn run(ctx: Arc<Context>, cancel: CancellationToken) {
    loop {
        let me = ctx.own_key();
        let devices: Vec<PublicKey> = ctx
            .trust
            .devices_of(&ctx.domain)
            .into_iter()
            .filter(|device| *device != me)
            .collect();
        // Push: what this device has, to wake devices it changed for.
        let fingerprint = match &ctx.push {
            Some(_) if !devices.is_empty() => {
                Some(crate::push::summary_fingerprint(&summary(&ctx).await))
            }
            _ => None,
        };
        for device in devices {
            if cancel.is_cancelled() {
                return;
            }
            let result = exchange(&ctx, device).await;
            if let (Some(push), Some(fingerprint)) = (&ctx.push, fingerprint) {
                match &result {
                    Ok(_) => push.synced(&device, fingerprint),
                    Err(_) => push.sync_failed(&device, fingerprint),
                }
            }
            match result {
                // Attachments of the person this device lacks.
                Ok(_) => crate::copies::catch_up_with(&ctx, device),
                Err(error) => {
                    tracing::trace!(peer = %device.fmt_short(), %error, "no sync with a device");
                }
            }
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(ctx.tuning.sync_interval) => {}
            () = ctx.leases.hello_seen.notified() => {}
        }
    }
}

/// Sends pending submissions now and then, when peers say Hello and when
/// the host lease moves.
pub(crate) async fn drain_pending(ctx: Arc<Context>, cancel: CancellationToken) {
    let every = ctx.tuning.sync_interval.min(ctx.tuning.retry_max);
    loop {
        let answered = ctx.server.drain_pending().await;
        if answered > 0 {
            tracing::debug!(answered, "sent pending submissions");
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(every) => {}
            () = ctx.pending_wake.notified() => {}
            () = ctx.leases.hello_seen.notified() => {}
        }
    }
}
