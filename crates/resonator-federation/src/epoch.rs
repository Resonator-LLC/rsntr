//! Host epochs over federation: moving a wavelet's host role to another
//! server, and what the other servers do about it.
//!
//! A server takes over a wavelet ([`take_over`]) by signing an
//! [`EpochStart`] with its server key: a higher epoch than any it knows,
//! starting at its current version of the wavelet. The statement travels
//! with every `update`, `commit` and `history` answer of the new host, and
//! a server that is told of it adopts it ([`adopt`]): a replica that has
//! deltas past the start (from the old host) rewinds to the start, keeps
//! those deltas as orphans and resubmits them to the new host
//! ([`orphan_worker`]); the old host does the same and becomes a replica.
//! A server that knows a newer epoch refuses the old host's `update` and
//! `commit` with `fenced`, which carries the statements, so the old host
//! learns of the epoch from the first server it talks to.
//!
//! Who may hold a wavelet's host role is an [`EpochAuthority`]'s call; by
//! default only the wavelet domain's own server ([`WaveletDomainOnly`]),
//! which never signs a statement: nothing changes unless an admin or a test
//! configures [`CoHosts`] (or, later, a per-person lease).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use iroh::{PublicKey, SecretKey, Signature};
use resonator_model::id::{IdUriCodec, WaveletName};
use resonator_model::proto::federation::ProtocolSignerInfo;
use resonator_model::proto::federation_error::{FederationError, federation_error::Code};
use resonator_model::proto::federation_rpc::{
    EpochStart, Fenced, Resubmit, ResubmitResult, SignedEpochStart, federation_request,
    federation_response,
};
use resonator_model::proto::{ByteStringMessage, decode, encode};
use resonator_model::version::HashedVersion;
use resonator_server::storage::{AppliedDelta, OrphanDelta};
use resonator_server::waveserver::{EpochAdoption, EpochRecord, WaveServer};
use tokio_util::sync::CancellationToken;

use crate::context::{
    Context, from_proto_version, parse_wavelet_name, to_proto_version, wavelet_uri,
};
use crate::endpoint::Peer;
use crate::signing::{parse_signer_info, signer_info_record, verify_applied_delta};

/// What an epoch statement's signature covers, before the statement bytes.
pub const EPOCH_SIGNATURE_CONTEXT: &[u8] = b"resonator/epoch/1\0";

/// Decides which servers may hold the host role of a wavelet. The key of
/// a statement's holder must in any case be bound to its domain (checked
/// with [`crate::trust::Trust::verify_signer`]).
pub trait EpochAuthority: Send + Sync + 'static {
    /// Whether `holder_domain` may host `wavelet` from `statement` on.
    fn may_host(&self, wavelet: &WaveletName, holder_domain: &str, statement: &EpochStart) -> bool;
}

/// The default: only the wavelet domain's own server.
#[derive(Debug, Clone, Copy, Default)]
pub struct WaveletDomainOnly;

impl EpochAuthority for WaveletDomainOnly {
    fn may_host(&self, wavelet: &WaveletName, holder_domain: &str, _: &EpochStart) -> bool {
        wavelet.wavelet_id.domain() == holder_domain
    }
}

/// Any of a set of domains may host any wavelet of any of them: servers
/// that agree to stand in for each other (phase 1: tests and admins).
#[derive(Debug, Clone, Default)]
pub struct CoHosts {
    pub domains: BTreeSet<String>,
}

impl CoHosts {
    pub fn new<S: Into<String>>(domains: impl IntoIterator<Item = S>) -> Self {
        CoHosts {
            domains: domains.into_iter().map(Into::into).collect(),
        }
    }
}

impl EpochAuthority for CoHosts {
    fn may_host(&self, wavelet: &WaveletName, holder_domain: &str, _: &EpochStart) -> bool {
        wavelet.wavelet_id.domain() == holder_domain
            || (self.domains.contains(holder_domain)
                && self.domains.contains(wavelet.wavelet_id.domain()))
    }
}

fn failure(code: Code, message: impl Into<String>) -> FederationError {
    FederationError::new(code, message)
}

/// Signs an epoch statement.
pub fn sign_epoch_start(key: &SecretKey, statement: &EpochStart) -> SignedEpochStart {
    let bytes = encode(statement).unwrap_or_default();
    let signature = key.sign(&[EPOCH_SIGNATURE_CONTEXT, &bytes].concat());
    SignedEpochStart {
        statement: Some(bytes),
        signature: Some(signature.to_bytes().to_vec()),
    }
}

/// Checks a signed statement's form and signature (by its own holder
/// key) and returns it with the holder key.
pub fn verify_epoch_start(
    signed: &SignedEpochStart,
) -> Result<(EpochStart, PublicKey), FederationError> {
    let bad = |message: &str| failure(Code::NotAcceptable, format!("epoch statement: {message}"));
    let bytes = signed.statement.as_deref().ok_or_else(|| bad("missing"))?;
    let statement: EpochStart = decode(bytes).map_err(|e| bad(&e.to_string()))?;
    let key: &[u8; 32] = statement
        .holder_key
        .as_deref()
        .unwrap_or_default()
        .try_into()
        .map_err(|_| bad("no 32-byte holder key"))?;
    let key = PublicKey::from_bytes(key).map_err(|_| bad("not an Ed25519 key"))?;
    let signature: &[u8; 64] = signed
        .signature
        .as_deref()
        .unwrap_or_default()
        .try_into()
        .map_err(|_| bad("no 64-byte signature"))?;
    key.verify(
        &[EPOCH_SIGNATURE_CONTEXT, bytes].concat(),
        &Signature::from_bytes(signature),
    )
    .map_err(|_| bad("signature does not verify"))?;
    if statement.epoch.unwrap_or(0) == 0 {
        return Err(bad("epoch 0 is implicit"));
    }
    Ok((statement, key))
}

/// The statements of a wavelet's epochs after 0, oldest first, for the
/// wire.
pub(crate) fn statements(server: &WaveServer, name: &WaveletName) -> Vec<SignedEpochStart> {
    server
        .epochs(name)
        .iter()
        .filter_map(|record| decode::<SignedEpochStart>(&record.statement).ok())
        .collect()
}

/// The answer that tells an old host of the newer epochs.
pub(crate) fn fenced(server: &WaveServer, name: &WaveletName) -> federation_response::Result {
    federation_response::Result::Fenced(Fenced {
        epoch_start: statements(server, name),
    })
}

/// Checks a statement about `name` and turns it into a record: signed by
/// its holder, whose key is bound to its domain, which the authority lets
/// host the wavelet.
async fn record_of(
    ctx: &Context,
    name: &WaveletName,
    signed: &SignedEpochStart,
) -> Result<EpochRecord, FederationError> {
    let (statement, key) = verify_epoch_start(signed)?;
    let about = statement.wavelet_name.as_deref().ok_or_else(|| {
        failure(
            Code::NotAcceptable,
            "per-person epochs are not supported yet",
        )
    })?;
    if IdUriCodec::uri_to_wavelet_name(about).ok().as_ref() != Some(name) {
        return Err(failure(
            Code::BadRequest,
            "epoch statement about another wavelet",
        ));
    }
    let holder_domain = statement.holder_domain.clone().unwrap_or_default();
    if !ctx
        .epoch_authority
        .may_host(name, &holder_domain, &statement)
    {
        return Err(failure(
            Code::NotAuthorized,
            format!("{holder_domain} may not host {name}"),
        ));
    }
    let own = holder_domain == ctx.domain && key == ctx.secret.public();
    // A holder that a revocation withdrew may not start epochs.
    if !own && ctx.trust.is_revoked(&key) {
        return Err(failure(
            Code::NotAuthorized,
            format!("the epoch holder's key was revoked by {holder_domain}"),
        ));
    }
    if !own {
        match ctx.trust.verify_signer(&key, &holder_domain).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(failure(
                    Code::NotAuthorized,
                    format!("the epoch holder's key is not bound to {holder_domain}"),
                ));
            }
            Err(e) => return Err(failure(Code::UndefinedCondition, e.to_string())),
        }
    }
    let start = from_proto_version(statement.start_version.as_ref())?;
    Ok(EpochRecord {
        epoch: statement.epoch.unwrap_or(0),
        start,
        holder: key.as_bytes().to_vec(),
        holder_domain,
        statement: encode(signed).map_err(|e| failure(Code::BadRequest, e.to_string()))?,
    })
}

/// What adopting statements came to.
#[derive(Debug, Default)]
pub(crate) struct Adopted {
    /// A statement was new to this server.
    pub changed: bool,
    /// A statement was older than what this server knows.
    pub stale: bool,
}

/// Adopts epoch statements about `name`, oldest first. A rewind wakes the
/// orphan worker.
pub(crate) async fn adopt(
    ctx: &Context,
    name: &WaveletName,
    signed: &[SignedEpochStart],
) -> Result<Adopted, FederationError> {
    let mut adopted = Adopted::default();
    // Statements adopted before are not checked again: their holder may
    // have been revoked since (a lost device), which bars it from new
    // epochs, not from the ones it held.
    let known: Vec<Vec<u8>> = ctx
        .server
        .epochs(name)
        .into_iter()
        .map(|record| record.statement)
        .collect();
    for statement in signed {
        if encode(statement).is_ok_and(|bytes| known.contains(&bytes)) {
            continue;
        }
        let record = record_of(ctx, name, statement).await?;
        let epoch = record.epoch;
        match ctx.server.adopt_epoch(name, record).await {
            Ok(EpochAdoption::Known) => {}
            Ok(EpochAdoption::Stale(_)) => adopted.stale = true,
            Ok(EpochAdoption::Recorded) => adopted.changed = true,
            Ok(EpochAdoption::Rewound { orphans, .. }) => {
                adopted.changed = true;
                tracing::info!(wavelet = %name, epoch, orphans, "rewound for a newer host epoch");
                ctx.orphans_wake.notify_one();
            }
            Err(e) => {
                tracing::warn!(wavelet = %name, epoch, error = %e, "cannot adopt a host epoch");
                return Err(failure(Code::NotAcceptable, e.to_string()));
            }
        }
    }
    Ok(adopted)
}

/// Makes this server the host of a wavelet in epoch `epoch`, which must
/// be higher than any known, from its current version on, and tells the
/// servers of the wavelet's participants. Returns the epoch's start.
pub(crate) async fn take_over(
    ctx: &Context,
    name: &WaveletName,
    epoch: u64,
) -> Result<HashedVersion, FederationError> {
    let known = ctx
        .server
        .epochs(name)
        .last()
        .map_or(0, |record| record.epoch);
    if epoch <= known {
        return Err(failure(
            Code::BadRequest,
            format!("epoch {epoch} is not above the known epoch {known}"),
        ));
    }
    let start = ctx
        .server
        .current_version(name)
        .await
        .map_err(|e| failure(Code::InternalServerError, e.to_string()))?;
    let statement = EpochStart {
        wavelet_name: Some(wavelet_uri(name)),
        person: None,
        epoch: Some(epoch),
        start_version: Some(to_proto_version(&start)?),
        holder_key: Some(ctx.secret.public().as_bytes().to_vec()),
        holder_domain: Some(ctx.domain.clone()),
        issued_at: Some(ctx.now()),
    };
    let signed = sign_epoch_start(&ctx.secret, &statement);
    adopt(ctx, name, &[signed]).await?;
    // Every server with participants on the wavelet hears of the new host
    // with a commit (and the deltas it lacks).
    let participants = ctx
        .server
        .with_wavelet(name, |wavelet| wavelet.participants().to_vec())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let local = if ctx.person_mode {
        ""
    } else {
        ctx.domain.as_str()
    };
    let domains: BTreeSet<String> = participants
        .iter()
        .map(|participant| participant.domain().to_owned())
        .chain([name.wavelet_id.domain().to_owned()])
        .filter(|domain| *domain != local)
        .flat_map(|domain| ctx.targets(&domain))
        .collect();
    let uri = wavelet_uri(name);
    for domain in domains {
        if let Err(error) = ctx
            .store
            .outbox_enqueue(
                &domain,
                &uri,
                start.version(),
                &crate::endpoint::new_request_id(),
                ctx.now(),
            )
            .await
        {
            tracing::error!(%domain, wavelet = %name, %error, "cannot queue a federation notification");
        }
    }
    ctx.outbox_wake.notify_one();
    tracing::info!(wavelet = %name, epoch, version = start.version(), "took over as host");
    Ok(start)
}

/// `resubmit`: orphaned deltas of a wavelet hosted here, from any server
/// with participants on it that held them.
pub(crate) async fn resubmit(
    ctx: &Context,
    peer: &Peer,
    request: Resubmit,
) -> federation_response::Result {
    match resubmit_inner(ctx, peer, request).await {
        Ok(result) => federation_response::Result::Resubmit(result),
        Err(error) => federation_response::Result::Error(error),
    }
}

async fn resubmit_inner(
    ctx: &Context,
    peer: &Peer,
    request: Resubmit,
) -> Result<ResubmitResult, FederationError> {
    let name = parse_wavelet_name(request.wavelet_name.as_deref())?;
    if !ctx.server.is_local_wavelet(&name) {
        return Err(failure(
            Code::BadRequest,
            format!("{name} is not hosted here"),
        ));
    }
    if !peer.speaks_for(name.wavelet_id.domain())
        && !crate::host::peer_participates(ctx, peer, &name).await
    {
        return Err(failure(
            Code::ItemNotFound,
            format!("no wavelet {name} to write"),
        ));
    }
    let fork = from_proto_version(request.fork_version.as_ref())?;
    for info in &request.signer_info {
        learn_signer(ctx, info).await?;
    }
    let mut chain = Vec::with_capacity(request.applied_delta.len());
    for bytes in request.applied_delta {
        let applied: AppliedDelta = ByteStringMessage::parse_from(bytes)
            .map_err(|e| failure(Code::BadRequest, format!("bad applied delta: {e}")))?;
        let Some(signed) = applied.message().signed_original_delta.as_ref() else {
            return Err(failure(Code::BadRequest, "applied delta without a delta"));
        };
        let signers = ctx.known_signers(signed).await;
        verify_applied_delta(&applied, |id| signers.get(id).cloned())
            .map_err(|e| failure(Code::NotAcceptable, e.to_string()))?;
        chain.push(applied);
    }
    let outcome = ctx
        .server
        .resubmit_orphans(&name, &fork, chain)
        .await
        .map_err(|e| failure(Code::NotAcceptable, e.to_string()))?;
    Ok(ResubmitResult {
        applied: u32::try_from(outcome.applied).ok(),
        skipped: u32::try_from(outcome.skipped).ok(),
        version: Some(to_proto_version(&outcome.version)?),
    })
}

/// Stores a signer info whose key is bound to its domain.
async fn learn_signer(ctx: &Context, info: &ProtocolSignerInfo) -> Result<(), FederationError> {
    let (signer_id, signer) =
        parse_signer_info(info).map_err(|e| failure(Code::NotAcceptable, e.to_string()))?;
    if ctx.stored_signer(&signer_id).await.is_some() {
        return Ok(());
    }
    // These deltas were applied before; a device revoked since signed
    // them while it was not.
    ctx.learn_signer_cert(info, &signer).await;
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
        signer_info_record(info).map_err(|e| failure(Code::NotAcceptable, e.to_string()))?;
    ctx.store
        .put_signer_info(record)
        .await
        .map_err(|e| failure(Code::InternalServerError, e.to_string()))
}

/// The signer infos of the signers of some applied deltas, as stored here.
async fn signer_infos(ctx: &Context, chain: &[AppliedDelta]) -> Vec<ProtocolSignerInfo> {
    let mut ids = BTreeSet::new();
    for applied in chain {
        if let Some(signed) = &applied.message().signed_original_delta {
            for signature in &signed.signature {
                if let Some(id) = &signature.signer_id {
                    ids.insert(id.clone());
                }
            }
        }
    }
    let mut infos = Vec::new();
    for id in ids {
        if let Ok(Some(record)) = ctx.store.signer_info(&id).await
            && let Ok(info) = decode::<ProtocolSignerInfo>(&record.info)
        {
            infos.push(info);
        }
    }
    infos
}

/// Sends one wavelet's pending orphans (one fork) to its host, or applies
/// them here if this server is the host now.
async fn resubmit_fork(
    ctx: &Context,
    name: &WaveletName,
    fork: &HashedVersion,
    orphans: &[OrphanDelta],
) -> Result<(), FederationError> {
    let mut chain = Vec::with_capacity(orphans.len());
    for orphan in orphans {
        chain.push(
            ByteStringMessage::parse_from(orphan.applied.clone())
                .map_err(|e| failure(Code::InternalServerError, e.to_string()))?,
        );
    }
    if ctx.server.is_local_wavelet(name) {
        ctx.server
            .resubmit_orphans(name, fork, chain)
            .await
            .map_err(|e| failure(Code::NotAcceptable, e.to_string()))?;
        return Ok(());
    }
    let host = ctx.server.host_domain(name);
    let request = Resubmit {
        wavelet_name: Some(wavelet_uri(name)),
        fork_version: Some(to_proto_version(fork)?),
        signer_info: signer_infos(ctx, &chain).await,
        applied_delta: chain
            .iter()
            .map(|applied| applied.bytes().to_vec())
            .collect(),
    };
    let answer = ctx
        .request_host(name, None, federation_request::Op::Resubmit(request))
        .await
        .map_err(|e| e.to_federation_error())?;
    match answer {
        federation_response::Result::Resubmit(result) => {
            tracing::info!(
                wavelet = %name,
                %host,
                applied = result.applied.unwrap_or(0),
                skipped = result.skipped.unwrap_or(0),
                "orphaned deltas resubmitted"
            );
            Ok(())
        }
        federation_response::Result::Fenced(fenced) => {
            adopt(ctx, name, &fenced.epoch_start).await?;
            Err(failure(
                Code::UndefinedCondition,
                "the host moved on; again",
            ))
        }
        _ => Err(failure(
            Code::BadRequest,
            "resubmit answered with something else",
        )),
    }
}

/// Resubmits pending orphans until cancelled: at start, after each rewind,
/// and with backoff after failures.
pub(crate) async fn orphan_worker(ctx: Arc<Context>, cancel: CancellationToken) {
    let mut attempts = 0u32;
    loop {
        let mut failed = false;
        match ctx.server.database().delta_store().pending_orphans().await {
            Ok(orphans) => {
                let mut forks: BTreeMap<(String, u64, Vec<u8>), Vec<OrphanDelta>> = BTreeMap::new();
                for orphan in orphans {
                    forks
                        .entry((
                            orphan.wavelet.clone(),
                            orphan.fork.version(),
                            orphan.fork.history_hash().to_vec(),
                        ))
                        .or_default()
                        .push(orphan);
                }
                for ((wavelet, _, _), orphans) in forks {
                    let Ok(name) = IdUriCodec::uri_to_wavelet_name(&wavelet) else {
                        continue;
                    };
                    let fork = orphans[0].fork.clone();
                    match resubmit_fork(&ctx, &name, &fork, &orphans).await {
                        Ok(()) => {
                            let shas = orphans.iter().map(|o| o.sha256.clone()).collect();
                            if let Err(error) = ctx
                                .server
                                .database()
                                .delta_store()
                                .orphans_done(&wavelet, shas)
                                .await
                            {
                                tracing::error!(%error, "cannot record resubmitted orphans");
                            }
                        }
                        Err(error) => {
                            failed = true;
                            tracing::info!(wavelet = %name, %error, "orphaned deltas not resubmitted yet");
                        }
                    }
                }
            }
            Err(error) => {
                failed = true;
                tracing::error!(%error, "cannot read orphaned deltas");
            }
        }
        let wait = if failed {
            attempts = attempts.saturating_add(1);
            ctx.tuning.backoff(attempts.saturating_sub(1))
        } else {
            attempts = 0;
            ctx.tuning.retry_max.max(std::time::Duration::from_secs(60))
        };
        tokio::select! {
            () = cancel.cancelled() => break,
            () = ctx.orphans_wake.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}
