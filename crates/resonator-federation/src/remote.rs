//! Local users' submissions to wavelets hosted elsewhere: the wave server
//! signs the delta and hands it here; it goes to the host as `submit`,
//! after this server's signer info was posted to the host once per run.
//! The answer goes back to the client like that of a local submission; the
//! transformed delta itself arrives with the host's next `update`.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use iroh::PublicKey;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::proto::deserialize_hashed_version;
use resonator_model::proto::federation::ProtocolSignedDelta;
use resonator_model::proto::federation_error::{FederationError, federation_error::Code};
use resonator_model::proto::federation_rpc::{
    PostSignerInfo, Probe, Submit, federation_request, federation_response,
};
use resonator_server::http::{ProbeOutcome, RemoteProbe};
use resonator_server::waveserver::{RemoteHost, SubmitError, SubmitResult};

use crate::context::{Context, lock, wavelet_uri};
use crate::endpoint::RequestError;

/// The [`RemoteHost`] of a federating wave server.
pub(crate) struct Forwarder {
    pub ctx: Arc<Context>,
}

impl RemoteHost for Forwarder {
    fn submit(
        &self,
        wavelet_name: WaveletName,
        signed_delta: ProtocolSignedDelta,
        request_id: Option<String>,
    ) -> BoxFuture<'static, Result<SubmitResult, SubmitError>> {
        let ctx = Arc::clone(&self.ctx);
        Box::pin(async move { forward(&ctx, wavelet_name, signed_delta, request_id).await })
    }
}

/// A failed request as a submission error: [`SubmitError::Unreachable`]
/// if the host was not reached (the submission may wait for it).
fn request_failed(domain: &str, error: &RequestError) -> SubmitError {
    match error {
        RequestError::Connect { .. } | RequestError::Timeout { .. } | RequestError::Closed => {
            SubmitError::Unreachable {
                domain: domain.to_owned(),
                message: error.to_string(),
            }
        }
        RequestError::Remote { error, .. } if error.code() == Code::UndefinedCondition => {
            SubmitError::Unreachable {
                domain: domain.to_owned(),
                message: error.message().to_owned(),
            }
        }
        other => submit_error(domain, &other.to_federation_error()),
    }
}

fn submit_error(domain: &str, error: &FederationError) -> SubmitError {
    SubmitError::Remote {
        domain: domain.to_owned(),
        message: format!("{domain}: {}", error.message()),
        bad_request: matches!(
            error.code(),
            Code::BadRequest | Code::NotAcceptable | Code::NotAuthorized | Code::ItemNotFound
        ),
    }
}

async fn post_signer_info(ctx: &Context, domain: &str, key: PublicKey) -> Result<(), RequestError> {
    if lock(&ctx.posted).contains(&key) {
        return Ok(());
    }
    let request = PostSignerInfo {
        signer_info: Some(ctx.signer_info.clone()),
    };
    ctx.endpoint()?
        .request(
            key,
            Some(domain),
            federation_request::Op::PostSignerInfo(request),
        )
        .await?;
    lock(&ctx.posted).insert(key);
    Ok(())
}

async fn forward(
    ctx: &Context,
    wavelet_name: WaveletName,
    signed_delta: ProtocolSignedDelta,
    request_id: Option<String>,
) -> Result<SubmitResult, SubmitError> {
    let host = ctx.server.host_domain(&wavelet_name);
    if ctx.person_mode && ctx.trust.is_revoked(&ctx.own_key()) {
        return Err(SubmitError::Remote {
            domain: host,
            message: "this device has been revoked by its person".to_owned(),
            bad_request: true,
        });
    }
    let failed = |error: RequestError| request_failed(&host, &error);
    let (host, key) = ctx.host_key(&wavelet_name).await.map_err(failed)?;
    let failed = |error: RequestError| request_failed(&host, &error);
    let request = Submit {
        wavelet_name: Some(wavelet_uri(&wavelet_name)),
        signed_delta: Some(signed_delta),
    };
    let mut answer = None;
    for attempt in 0..2 {
        post_signer_info(ctx, &host, key).await.map_err(failed)?;
        let endpoint = ctx.endpoint().map_err(failed)?;
        let op = federation_request::Op::Submit(request.clone());
        let sent = match &request_id {
            Some(id) => {
                endpoint
                    .request_with_id(key, Some(&host), id.clone(), op)
                    .await
            }
            None => endpoint.request(key, Some(&host), op).await,
        };
        match sent {
            // The host may have lost our signer info: post it again once.
            Err(RequestError::Remote { error, .. })
                if attempt == 0 && error.code() == Code::NotAuthorized =>
            {
                lock(&ctx.posted).remove(&key);
                answer = Some(Err(RequestError::Remote {
                    peer: host.clone(),
                    error,
                }));
            }
            other => {
                answer = Some(other);
                break;
            }
        }
    }
    let result = match answer {
        Some(Ok(federation_response::Result::Submit(result))) => result,
        Some(Ok(federation_response::Result::Fenced(fenced))) => {
            // The wavelet moved to another host; learn of it. The
            // submission may wait for the new host, which takes it if its
            // history has the delta's target version.
            if let Err(error) = crate::epoch::adopt(ctx, &wavelet_name, &fenced.epoch_start).await {
                tracing::warn!(wavelet = %wavelet_name, %error, "cannot adopt a host epoch");
            }
            return Err(SubmitError::Unreachable {
                domain: host.clone(),
                message: format!("{wavelet_name} has moved to another host"),
            });
        }
        Some(Ok(_)) => {
            return Err(SubmitError::Remote {
                domain: host.clone(),
                message: format!("{host} answered the submission with something else"),
                bad_request: false,
            });
        }
        Some(Err(error)) => return Err(failed(error)),
        None => unreachable!("the loop runs at least once"),
    };
    let version = result
        .hashed_version_after_application
        .as_ref()
        .and_then(|version| deserialize_hashed_version(version).ok())
        .ok_or_else(|| SubmitError::Remote {
            domain: host.clone(),
            message: format!("{host} answered without a version"),
            bad_request: false,
        })?;
    Ok(SubmitResult {
        operations_applied: usize::try_from(result.operations_applied.unwrap_or(0)).unwrap_or(0),
        hashed_version_after_application: version,
        application_timestamp: result.application_timestamp.unwrap_or_default(),
        pending: None,
    })
}

/// Answers the client's `/federation/probe`: may this server federate with
/// the address's domain, does its server answer, and does it know the
/// address.
pub(crate) struct Prober {
    pub(crate) ctx: Arc<Context>,
}

impl RemoteProbe for Prober {
    fn probe<'a>(&'a self, address: &'a ParticipantId) -> BoxFuture<'a, ProbeOutcome> {
        Box::pin(probe(&self.ctx, address))
    }
}

async fn probe(ctx: &Context, address: &ParticipantId) -> ProbeOutcome {
    let domain = address.domain();
    let outcome =
        |reachable: bool, allowed: bool, exists: Option<bool>, message: Option<String>| {
            ProbeOutcome {
                reachable,
                allowed,
                exists,
                message,
            }
        };
    if !ctx.trust.policy().allows(domain) {
        return outcome(
            false,
            false,
            None,
            Some(format!("this server does not federate with {domain}")),
        );
    }
    let op = federation_request::Op::Probe(Probe {
        address: Some(address.address().to_owned()),
    });
    match ctx.request(domain, None, op).await {
        Ok(federation_response::Result::Probe(result)) => outcome(true, true, result.exists, None),
        Ok(_) => outcome(true, true, None, None),
        Err(error @ (RequestError::Refused { .. } | RequestError::Unauthorised { .. })) => {
            outcome(true, false, None, Some(error.to_string()))
        }
        Err(error @ RequestError::Remote { .. }) => {
            outcome(true, true, None, Some(error.to_string()))
        }
        Err(error) => outcome(false, true, None, Some(error.to_string())),
    }
}
