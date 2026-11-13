//! The host side of the wave operations: what this server answers about
//! the wavelets it hosts (`WaveServerImpl.submitRequest`,
//! `requestHistory`, `getDeltaSignerInfo` and `postSignerInfo` in the
//! original).

use resonator_model::id::ParticipantId;
use resonator_model::proto::federation::{ProtocolSignerInfo, ProtocolWaveletDelta};
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    Ack, GetSignerInfo, History, HistoryResult, PostSignerInfo, SignerInfoResult, Submit,
    SubmitResult, federation_response,
};
use resonator_model::proto::{calculate_resulting_hashed_version, decode};
use resonator_server::waveserver::{SubmitError, SubmitErrorKind, SubmitReceipt};

use crate::context::{Context, error, from_proto_version, parse_wavelet_name, to_proto_version};
use crate::endpoint::Peer;
use crate::signing::{parse_signer_info, signer_info_record, verify_delta};

type Answer = federation_response::Result;

macro_rules! try_answer {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => return federation_response::Result::Error(error),
        }
    };
}
pub(crate) use try_answer;

fn submit_error(error: &SubmitError) -> Answer {
    let code = match error {
        SubmitError::NotParticipant { .. } => Code::NotAuthorized,
        _ if error.kind() == SubmitErrorKind::BadRequest => Code::BadRequest,
        _ => Code::InternalServerError,
    };
    self::error(code, error.to_string())
}

/// The wire answer of an applied submission.
pub(crate) fn submit_result(result: &resonator_server::waveserver::SubmitResult) -> Answer {
    Answer::Submit(SubmitResult {
        operations_applied: Some(i32::try_from(result.operations_applied).unwrap_or(i32::MAX)),
        hashed_version_after_application: Some(try_answer!(to_proto_version(
            &result.hashed_version_after_application
        ))),
        application_timestamp: Some(result.application_timestamp),
    })
}

/// `submit`: a remote server submits a delta by one of its users. If the
/// delta is applied, `receipt` is recorded with it in one transaction;
/// returns whether it was.
pub(crate) async fn submit(
    ctx: &Context,
    peer: &Peer,
    request: Submit,
    receipt: Option<SubmitReceipt>,
) -> (Answer, bool) {
    (submit_inner(ctx, peer, request, receipt).await).unwrap_or_else(|answer| (answer, false))
}

async fn submit_inner(
    ctx: &Context,
    peer: &Peer,
    request: Submit,
    receipt: Option<SubmitReceipt>,
) -> Result<(Answer, bool), Answer> {
    macro_rules! try_answer {
        ($result:expr) => {
            match $result {
                Ok(value) => value,
                Err(error) => return Err(federation_response::Result::Error(error)),
            }
        };
    }
    macro_rules! error {
        ($code:expr, $message:expr) => {
            return Err(error($code, $message))
        };
    }
    let name = try_answer!(parse_wavelet_name(request.wavelet_name.as_deref()));
    if !ctx.server.is_local_wavelet(&name) {
        if !ctx.server.epochs(&name).is_empty() {
            return Err(crate::epoch::fenced(&ctx.server, &name));
        }
        // Not a fault of the delta: the sender has an old idea of who
        // hosts the wavelet (person mode: the host lease moved) and may
        // try again elsewhere.
        error!(
            Code::UndefinedCondition,
            format!("{name} is not hosted here")
        );
    }
    let Some(signed) = request.signed_delta else {
        error!(Code::BadRequest, "no delta");
    };
    let delta: ProtocolWaveletDelta = match signed.delta.as_deref().map(decode) {
        Some(Ok(delta)) => delta,
        _ => error!(Code::BadRequest, "the signed delta holds no valid delta"),
    };
    let author = match ParticipantId::of(delta.author.as_deref().unwrap_or_default()) {
        Ok(author) => author,
        Err(e) => error!(Code::BadRequest, e.to_string()),
    };
    if !peer.speaks_for(author.domain()) {
        error!(
            Code::NotAuthorized,
            format!("the peer does not speak for {}", author.domain())
        );
    }
    // Remote users cannot create wavelets here: the first delta of a
    // wavelet is its creator's, and the creator is the host's. Another
    // device of the wavelet domain's person may (person mode).
    let version = delta
        .hashed_version
        .as_ref()
        .and_then(|v| v.version)
        .unwrap_or(0);
    if version <= 0 && !peer.speaks_for(name.wavelet_id.domain()) {
        error!(
            Code::NotAuthorized,
            "a remote server cannot create a wavelet (delta at version 0)"
        );
    }
    let signers = ctx.known_signers(&signed).await;
    // A revoked device's new deltas are refused; those applied before
    // stay.
    if let Some(revoked) = signers.values().find(|s| ctx.trust.is_revoked(&s.key)) {
        error!(
            Code::NotAuthorized,
            format!("the signer {} has been revoked", revoked.key.to_z32())
        );
    }
    if let Err(e) = verify_delta(&signed, |id| signers.get(id).cloned()) {
        let code = match e {
            crate::signing::VerifyError::UnknownSigner(_) => Code::NotAuthorized,
            _ => Code::NotAcceptable,
        };
        error!(code, e.to_string());
    }
    match ctx
        .server
        .submit_remote_request_with_receipt(&author, &name, signed, receipt)
        .await
    {
        Ok((result, recorded)) => {
            if recorded {
                // Answered once the delta and its receipt are durable, so
                // that a retry after this answer finds the receipt.
                let _ = ctx
                    .server
                    .wait_committed(&name, result.hashed_version_after_application.version())
                    .await;
            }
            Ok((submit_result(&result), recorded))
        }
        Err(e) => Err(submit_error(&e)),
    }
}

/// Whether any domain `peer` speaks for has a participant on the wavelet.
pub(crate) async fn peer_participates(
    ctx: &Context,
    peer: &Peer,
    name: &resonator_model::id::WaveletName,
) -> bool {
    matches!(
        ctx.server
            .with_wavelet(name, |wavelet| {
                wavelet
                    .participants()
                    .iter()
                    .any(|p| peer.speaks_for(p.domain()))
            })
            .await,
        Ok(Some(true))
    )
}

/// Whether `peer` may read `name` from here: another device of this
/// person reads anything; a server whose domain has a participant on a
/// wavelet hosted here reads it; and a device of the wavelet's own domain
/// reads it from any replica (a device recovering its person's waves).
pub(crate) async fn may_read(
    ctx: &Context,
    peer: &Peer,
    name: &resonator_model::id::WaveletName,
) -> bool {
    if ctx.is_own_device(&peer.key) {
        return true;
    }
    if ctx.server.is_local_wavelet(name) {
        return peer_participates(ctx, peer, name).await;
    }
    peer.speaks_for(name.wavelet_id.domain())
}

/// `history`: applied deltas of a hosted wavelet, for a server whose
/// domain has a participant on it (the original serves anyone).
pub(crate) async fn history(ctx: &Context, peer: &Peer, request: History) -> Answer {
    let name = try_answer!(parse_wavelet_name(request.wavelet_name.as_deref()));
    // One answer for "no such wavelet" and "not yours to read".
    let not_found = || error(Code::ItemNotFound, format!("no wavelet {name} to read"));
    // Another device of this person may read any wavelet this device has,
    // replicas included (device sync).
    if !may_read(ctx, peer, &name).await {
        return not_found();
    }
    let start = try_answer!(from_proto_version(request.start_version.as_ref()));
    let end = try_answer!(from_proto_version(request.end_version.as_ref()));
    let byte_limit = request
        .length_limit
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| *limit > 0)
        .map_or(ctx.tuning.page_bytes, |limit| {
            limit.min(ctx.tuning.page_bytes)
        });
    let deltas = match ctx
        .server
        .get_applied_history(&name, &start, &end, Some(ctx.tuning.page_deltas))
        .await
    {
        Ok(deltas) => deltas,
        Err(e) => return error(Code::BadRequest, e.to_string()),
    };
    let mut result = HistoryResult::default();
    let mut bytes = 0usize;
    let mut reached = start.version();
    for delta in &deltas {
        if !result.applied_delta.is_empty() && bytes + delta.bytes().len() > byte_limit {
            break;
        }
        bytes += delta.bytes().len();
        reached = try_answer!(calculate_resulting_hashed_version(delta).map_err(|e| {
            resonator_model::proto::federation_error::FederationError::new(
                Code::InternalServerError,
                e.to_string(),
            )
        }))
        .version();
        result.applied_delta.push(delta.bytes().to_vec());
    }
    if reached < end.version() {
        result.version_truncated_at = i64::try_from(reached).ok();
    }
    if let Ok(Some((_, committed))) = ctx.server.get_versions(&name).await {
        result.last_committed_version = to_proto_version(&committed).ok();
    }
    result.epoch_start = crate::epoch::statements(&ctx.server, &name);
    Answer::History(result)
}

/// `signer-info`: the signer info of a signer of a delta of a hosted
/// wavelet.
pub(crate) async fn signer_info(ctx: &Context, peer: &Peer, request: GetSignerInfo) -> Answer {
    let name = try_answer!(parse_wavelet_name(request.wavelet_name.as_deref()));
    let version = try_answer!(from_proto_version(request.version.as_ref()));
    let signer_id = request.signer_id.unwrap_or_default();
    let not_found = || error(Code::ItemNotFound, "no such signer of that delta");
    // Hosted here, or a replica read by its own domain's device.
    if !ctx.server.is_local_wavelet(&name) && !(may_read(ctx, peer, &name).await) {
        return not_found();
    }
    match ctx
        .server
        .is_delta_signer(&name, &version, &signer_id)
        .await
    {
        Ok(true) => {}
        _ => return not_found(),
    }
    let info = match ctx.store.signer_info(&signer_id).await {
        Ok(Some(record)) => record.info,
        _ => return not_found(),
    };
    match decode::<ProtocolSignerInfo>(&info) {
        Ok(info) => Answer::SignerInfo(SignerInfoResult {
            signer_info: Some(info),
        }),
        Err(e) => error(Code::InternalServerError, e.to_string()),
    }
}

/// `post-signer-info`: a remote server announces the signer of the deltas
/// it is about to submit. It must speak for the signer's domain, and the
/// key must be bound to it.
pub(crate) async fn post_signer_info(
    ctx: &Context,
    peer: &Peer,
    request: PostSignerInfo,
) -> Answer {
    let Some(info) = request.signer_info else {
        return error(Code::BadRequest, "no signer info");
    };
    let (_, signer) = match parse_signer_info(&info) {
        Ok(parsed) => parsed,
        Err(e) => return error(Code::NotAcceptable, e.to_string()),
    };
    if !peer.speaks_for(&signer.domain) {
        return error(
            Code::NotAuthorized,
            format!("the peer does not speak for {}", signer.domain),
        );
    }
    ctx.learn_signer_cert(&info, &signer).await;
    if ctx.trust.is_revoked(&signer.key) {
        return error(Code::NotAuthorized, "the signer has been revoked");
    }
    let bound = signer.key == peer.key
        || matches!(
            ctx.trust.verify_signer(&signer.key, &signer.domain).await,
            Ok(true)
        );
    if !bound {
        return error(
            Code::NotAuthorized,
            format!("the key is not bound to {}", signer.domain),
        );
    }
    let record = match signer_info_record(&info) {
        Ok(record) => record,
        Err(e) => return error(Code::NotAcceptable, e.to_string()),
    };
    match ctx.store.put_signer_info(record).await {
        Ok(()) => Answer::Ack(Ack {}),
        Err(e) => error(Code::InternalServerError, e.to_string()),
    }
}
