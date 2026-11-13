//! The remote side of the wave operations: replicas of wavelets hosted
//! elsewhere, fed by their hosts' `update` and `commit` requests
//! (`RemoteWaveletContainerImpl` in the original).
//!
//! Every applied delta must carry valid signatures (unknown signers are
//! fetched from the host with `signer-info`) and continue the replica's
//! hash chain. A delta that arrives ahead of the replica is held while the
//! deltas in between are fetched with `history`; a commit is acknowledged
//! once the replica has every delta it covers, fetching them if need be.
//! Replicas apply through the wave server, so the wave bus announces them
//! to clients, search, robots and agents like local wavelets.

use resonator_model::id::WaveletName;
use resonator_model::proto::federation_error::{FederationError, federation_error::Code};
use resonator_model::proto::federation_rpc::{
    Ack, Commit, GetSignerInfo, History, SignedEpochStart, Update, federation_request,
    federation_response,
};
use resonator_model::proto::{ByteStringMessage, calculate_resulting_hashed_version};
use resonator_model::version::HashedVersion;
use resonator_server::storage::AppliedDelta;
use resonator_server::waveserver::ReplicaOutcome;

use crate::context::{
    Context, ReplicaQueue, from_proto_version, parse_wavelet_name, to_proto_version, wavelet_uri,
};
use crate::endpoint::Peer;
use crate::epoch;
use crate::host::try_answer;
use crate::signing::{
    is_unsigned_host_delta, parse_signer_info, signer_info_record, verify_applied_delta,
};

type Answer = federation_response::Result;

fn failure(code: Code, message: impl Into<String>) -> FederationError {
    FederationError::new(code, message)
}

/// The wavelet named by a host's request, after adopting the epoch
/// statements it carries and checking that `peer` is its host. A sender
/// that hosted an earlier epoch is told of the newer ones (`Err(Ok)`).
async fn hosted_by_peer(
    ctx: &Context,
    peer: &Peer,
    uri: Option<&str>,
    statements: &[SignedEpochStart],
) -> Result<WaveletName, Answer> {
    let name = parse_wavelet_name(uri).map_err(Answer::Error)?;
    if !statements.is_empty() {
        let queue = ctx.replica(&name);
        let mut queue = queue.lock().await;
        let adopted = epoch::adopt(ctx, &name, statements)
            .await
            .map_err(Answer::Error)?;
        if adopted.changed {
            queue.queued.clear();
        }
    }
    let host = ctx.server.host_domain(&name);
    let local = ctx.server.is_local_wavelet(&name);
    // Person mode: another device of this node's person hosts it; only
    // that device sends its updates.
    let delegated_elsewhere = ctx
        .server
        .host_key(&name)
        .is_some_and(|holder| holder != peer.key.as_bytes());
    if local || !peer.speaks_for(&host) || delegated_elsewhere {
        if !ctx.server.epochs(&name).is_empty() {
            tracing::info!(wavelet = %name, %host, "fenced off a former host");
            return Err(epoch::fenced(&ctx.server, &name));
        }
        if local {
            return Err(Answer::Error(failure(
                Code::BadRequest,
                format!("{name} is hosted here, not by the sender"),
            )));
        }
        return Err(Answer::Error(failure(
            Code::NotAuthorized,
            format!("the sender does not speak for {host}, the host of {name}"),
        )));
    }
    // A device of a person that hosts the person's wavelets is the one to
    // dial for the person from now on.
    if host != ctx.domain && ctx.trust.persons().has_unrevoked_cert(&peer.key) {
        ctx.trust.persons().set_route(&host, peer.key);
    }
    Ok(name)
}

/// `update`: applied deltas from the host.
pub(crate) async fn update(ctx: &Context, peer: &Peer, request: Update) -> Answer {
    let name = match hosted_by_peer(
        ctx,
        peer,
        request.wavelet_name.as_deref(),
        &request.epoch_start,
    )
    .await
    {
        Ok(name) => name,
        Err(answer) => return answer,
    };
    let mut deltas = Vec::with_capacity(request.applied_delta.len());
    for bytes in request.applied_delta {
        match ByteStringMessage::parse_from(bytes) {
            Ok(delta) => deltas.push(delta),
            Err(e) => {
                return Answer::Error(failure(Code::BadRequest, format!("bad applied delta: {e}")));
            }
        }
    }
    let queue = ctx.replica(&name);
    let mut queue = queue.lock().await;
    for applied in deltas {
        try_answer!(offer(ctx, &name, &mut queue, applied).await);
    }
    Answer::Ack(Ack {})
}

/// `commit`: the host has stored the wavelet up to a version.
pub(crate) async fn commit(ctx: &Context, peer: &Peer, request: Commit) -> Answer {
    let name = match hosted_by_peer(
        ctx,
        peer,
        request.wavelet_name.as_deref(),
        &request.epoch_start,
    )
    .await
    {
        Ok(name) => name,
        Err(answer) => return answer,
    };
    let committed = try_answer!(from_proto_version(request.committed_version.as_ref()));
    let queue = ctx.replica(&name);
    let mut queue = queue.lock().await;
    try_answer!(catch_up(ctx, &name, &mut queue, &committed).await);
    match ctx
        .server
        .hashed_version_at(&name, committed.version())
        .await
    {
        Ok(Some(version)) if version == committed => Answer::Ack(Ack {}),
        Ok(_) => {
            tracing::error!(
                wavelet = %name,
                version = committed.version(),
                "replica does not match the host's committed version"
            );
            Answer::Error(failure(
                Code::NotAcceptable,
                format!(
                    "the replica of {name} differs from the host at version {}",
                    committed.version()
                ),
            ))
        }
        Err(e) => Answer::Error(failure(Code::InternalServerError, e.to_string())),
    }
}

/// Verifies an applied delta and applies it, or queues it and fills the
/// gap before it.
async fn offer(
    ctx: &Context,
    name: &WaveletName,
    queue: &mut ReplicaQueue,
    applied: AppliedDelta,
) -> Result<(), FederationError> {
    verify(ctx, name, &applied).await?;
    match apply(ctx, name, applied.clone()).await? {
        ReplicaOutcome::Applied(_) | ReplicaOutcome::AlreadyApplied => Ok(()),
        ReplicaOutcome::Ahead {
            current,
            applied_at,
        } => {
            tracing::debug!(
                wavelet = %name,
                have = current.version(),
                got = applied_at.version(),
                "delta ahead of the replica; filling the gap"
            );
            if queue.queued.len() >= ctx.tuning.max_queued_deltas {
                queue.queued.clear();
            }
            queue.queued.insert(applied_at.version(), applied);
            catch_up(ctx, name, queue, &applied_at).await
        }
    }
}

async fn apply(
    ctx: &Context,
    name: &WaveletName,
    applied: AppliedDelta,
) -> Result<ReplicaOutcome, FederationError> {
    ctx.server
        .apply_replica_delta(name, applied)
        .await
        .map_err(|e| {
            tracing::warn!(wavelet = %name, error = %e, "rejected a remote delta");
            failure(Code::NotAcceptable, e.to_string())
        })
}

/// Applies the queued deltas that continue the replica.
async fn drain(
    ctx: &Context,
    name: &WaveletName,
    queue: &mut ReplicaQueue,
) -> Result<HashedVersion, FederationError> {
    loop {
        let current = current_version(ctx, name).await?;
        let Some(entry) = queue.queued.first_entry() else {
            return Ok(current);
        };
        if *entry.key() > current.version() {
            return Ok(current);
        }
        let applied = entry.remove();
        if let ReplicaOutcome::Ahead { .. } = apply(ctx, name, applied).await? {
            return Err(failure(
                Code::InternalServerError,
                "queued delta out of order",
            ));
        }
    }
}

async fn current_version(
    ctx: &Context,
    name: &WaveletName,
) -> Result<HashedVersion, FederationError> {
    ctx.server
        .current_version(name)
        .await
        .map_err(|e| failure(Code::InternalServerError, e.to_string()))
}

/// Brings the replica up to `target` (a version of the host's history):
/// applies queued deltas and fetches the rest with `history`.
async fn catch_up(
    ctx: &Context,
    name: &WaveletName,
    queue: &mut ReplicaQueue,
    target: &HashedVersion,
) -> Result<(), FederationError> {
    loop {
        let current = drain(ctx, name, queue).await?;
        if current.version() >= target.version() {
            return Ok(());
        }
        let request = History {
            wavelet_name: Some(wavelet_uri(name)),
            start_version: Some(to_proto_version(&current)?),
            end_version: Some(to_proto_version(target)?),
            length_limit: i64::try_from(ctx.tuning.page_bytes).ok(),
        };
        let answer = ctx
            .request_host(name, None, federation_request::Op::History(request))
            .await
            .map_err(|e| e.to_federation_error())?;
        let federation_response::Result::History(history) = answer else {
            return Err(failure(
                Code::BadRequest,
                "history answered with something else",
            ));
        };
        if !history.epoch_start.is_empty()
            && epoch::adopt(ctx, name, &history.epoch_start).await?.changed
        {
            queue.queued.clear();
            continue;
        }
        tracing::debug!(
            wavelet = %name,
            from = current.version(),
            to = target.version(),
            deltas = history.applied_delta.len(),
            "fetched history"
        );
        if history.applied_delta.is_empty() {
            return Err(failure(
                Code::UndefinedCondition,
                format!(
                    "the host sent no history of {name} after {}",
                    current.version()
                ),
            ));
        }
        for bytes in history.applied_delta {
            let applied: AppliedDelta = ByteStringMessage::parse_from(bytes)
                .map_err(|e| failure(Code::BadRequest, format!("bad applied delta: {e}")))?;
            verify(ctx, name, &applied).await?;
            if let ReplicaOutcome::Ahead { .. } = apply(ctx, name, applied).await? {
                return Err(failure(
                    Code::NotAcceptable,
                    "the history does not continue the replica",
                ));
            }
        }
    }
}

/// Checks an applied delta's signatures, fetching unknown signers' info
/// from the host (`prefetchDeltaSignerInfo`).
pub(crate) async fn verify(
    ctx: &Context,
    name: &WaveletName,
    applied: &AppliedDelta,
) -> Result<(), FederationError> {
    verify_from(ctx, name, applied, None).await
}

/// [`verify`], asking `source` (the server the delta came from, and the
/// domain to authorise it for) for unknown signers before the host.
pub(crate) async fn verify_from(
    ctx: &Context,
    name: &WaveletName,
    applied: &AppliedDelta,
    source: Option<(iroh::PublicKey, Option<&str>)>,
) -> Result<(), FederationError> {
    let Some(signed) = applied.message().signed_original_delta.as_ref() else {
        return Err(failure(Code::BadRequest, "applied delta without a delta"));
    };
    // History the host wrote for its own users before it federated.
    if is_unsigned_host_delta(signed, name.wavelet_id.domain()) {
        tracing::debug!(wavelet = %name, "taking an unsigned delta of the host's own domain");
        return Ok(());
    }
    let mut signers = ctx.known_signers(signed).await;
    for signature in &signed.signature {
        let Some(id) = signature.signer_id.as_deref() else {
            continue;
        };
        if signers.contains_key(id) {
            continue;
        }
        let version = calculate_resulting_hashed_version(applied)
            .map_err(|e| failure(Code::BadRequest, e.to_string()))?;
        let request = GetSignerInfo {
            wavelet_name: Some(wavelet_uri(name)),
            signer_id: Some(id.to_vec()),
            version: Some(to_proto_version(&version)?),
        };
        let from_source = match (source, ctx.endpoint()) {
            (Some((key, domain)), Ok(endpoint)) => endpoint
                .request(
                    key,
                    domain,
                    federation_request::Op::GetSignerInfo(request.clone()),
                )
                .await
                .ok()
                .filter(|answer| matches!(answer, federation_response::Result::SignerInfo(_))),
            _ => None,
        };
        let answer = match from_source {
            Some(answer) => answer,
            None => ctx
                .request_host(name, None, federation_request::Op::GetSignerInfo(request))
                .await
                .map_err(|e| e.to_federation_error())?,
        };
        let federation_response::Result::SignerInfo(result) = answer else {
            return Err(failure(
                Code::BadRequest,
                "signer-info answered with something else",
            ));
        };
        let info = result
            .signer_info
            .ok_or_else(|| failure(Code::BadRequest, "no signer info"))?;
        let (signer_id, signer) =
            parse_signer_info(&info).map_err(|e| failure(Code::NotAcceptable, e.to_string()))?;
        if signer_id != id {
            return Err(failure(
                Code::NotAcceptable,
                "signer info of another signer",
            ));
        }
        // The host applied the delta: a device revoked since signed it
        // while it was not, so the signature stands.
        ctx.learn_signer_cert(&info, &signer).await;
        match ctx
            .trust
            .verify_historical_signer(&signer.key, &signer.domain)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return Err(failure(
                    Code::NotAcceptable,
                    format!("signer key is not bound to {}", signer.domain),
                ));
            }
            Err(e) => return Err(failure(Code::UndefinedCondition, e.to_string())),
        }
        let record =
            signer_info_record(&info).map_err(|e| failure(Code::NotAcceptable, e.to_string()))?;
        ctx.store
            .put_signer_info(record)
            .await
            .map_err(|e| failure(Code::InternalServerError, e.to_string()))?;
        signers.insert(signer_id, signer);
    }
    verify_applied_delta(applied, |id| signers.get(id).cloned()).map_err(|e| {
        tracing::warn!(wavelet = %name, error = %e, "remote delta signature does not verify");
        failure(Code::NotAcceptable, e.to_string())
    })?;
    Ok(())
}
