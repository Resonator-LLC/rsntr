//! `fetch-attachment`: attachments travel from the server they were
//! uploaded to (the domain of their id, the origin) to the servers of the
//! participants who look at them.
//!
//! The origin answers only for an attachment uploaded to it, of the
//! wavelet named, and only to a server whose domain has a participant on
//! that wavelet. Its answer says what follows (size, SHA-256, file name,
//! uploader); the bytes follow as chunks on the same stream. The receiving
//! side ([`RemoteOrigin`]) writes them into an upload of its attachment
//! store, which checks size and hash and keeps the copy in its cache.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use resonator_model::id::WaveletName;
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    FetchAttachment, FetchAttachmentResult, federation_request, federation_response,
};
use resonator_server::attachment::{
    AttachmentId, AttachmentOrigin, AttachmentService, FetchError, RemoteAttachment, Upload,
    wavelet_name_to_wave_ref,
};

use crate::context::{Context, error, parse_wavelet_name, wavelet_uri};
use crate::endpoint::{Body, Peer, RequestError};
use crate::host::peer_participates;

/// How many servers are asked for an attachment whose id names none.
const MAX_CANDIDATES: usize = 8;

/// Answers `fetch-attachment` as the origin.
pub(crate) async fn serve(
    ctx: &Context,
    attachments: &AttachmentService,
    peer: &Peer,
    request: FetchAttachment,
) -> (federation_response::Result, Option<Body>) {
    // One answer for every refusal: no such attachment, not of that
    // wavelet, not for that server.
    let not_found = || {
        (
            error(Code::ItemNotFound, "no such attachment to fetch"),
            None,
        )
    };
    let name = match parse_wavelet_name(request.wavelet_name.as_deref()) {
        Ok(name) => name,
        Err(e) => return (federation_response::Result::Error(e), None),
    };
    let Ok(id) = AttachmentId::deserialise(request.attachment_id.as_deref().unwrap_or_default())
    else {
        return not_found();
    };
    // An id without a domain (made by a client that did not know its
    // server federates) may be of here: it is if it was uploaded here,
    // which the row says below.
    let bare = id.domain().is_empty();
    if !bare && id.domain() != ctx.domain {
        return not_found();
    }
    // Another device of this person may have any of its attachments.
    let own_device = ctx.is_own_device(&peer.key);
    if !own_device && !peer_participates(ctx, peer, &name).await {
        return not_found();
    }
    let metadata = match attachments.metadata(&id).await {
        Ok(Some(metadata)) if !metadata.evicted => metadata,
        // Person mode: uploaded to another device of the person; fetched
        // from there for a peer of another person.
        Ok(_) if ctx.person_mode && !own_device && !bare => {
            match attachments.fetch_remote(&id, &name).await {
                Ok(metadata) => metadata,
                Err(_) => return not_found(),
            }
        }
        Ok(_) => return not_found(),
        Err(e) => return (error(Code::InternalServerError, e.to_string()), None),
    };
    // A copy of another server's attachment is not served on; a copy
    // from another device of this person is.
    if metadata
        .origin
        .as_deref()
        .is_some_and(|origin| origin != ctx.domain)
        || (bare && metadata.origin.is_some())
        || metadata.wave_ref != wavelet_name_to_wave_ref(&name)
        || metadata.size > attachments.settings().max_size
    {
        return not_found();
    }
    let (file, length) = match attachments.open_file(&id).await {
        Ok(Some(opened)) => opened,
        Ok(None) => return not_found(),
        Err(e) => return (error(Code::InternalServerError, e.to_string()), None),
    };
    if length != metadata.size {
        return (
            error(
                Code::InternalServerError,
                "the stored file has another size",
            ),
            None,
        );
    }
    tracing::debug!(attachment = %id, peer = ?peer.domains, "serving attachment to a peer");
    let result = FetchAttachmentResult {
        file_name: Some(metadata.file_name),
        size: Some(metadata.size),
        creator: Some(metadata.creator),
        sha256: Some(metadata.sha256.to_vec()),
        created_at: Some(metadata.created_at),
    };
    (
        federation_response::Result::FetchAttachment(result),
        Some(Box::pin(tokio::io::AsyncReadExt::take(file, length))),
    )
}

/// Fetches attachments from their origins for this server's attachment
/// store.
pub(crate) struct RemoteOrigin {
    pub(crate) ctx: Arc<Context>,
    /// Person mode: the device of the person to ask first for an
    /// attachment of the person's domain (the one that told of it).
    pub(crate) prefer: Option<iroh::PublicKey>,
    /// Whether a device that cannot be reached is woken through push and
    /// asked again (a user is waiting); not for copies made ahead.
    pub(crate) wake: bool,
}

impl RemoteOrigin {
    /// Fetches for users of this server.
    pub(crate) fn new(ctx: Arc<Context>) -> Self {
        RemoteOrigin {
            ctx,
            prefer: None,
            wake: true,
        }
    }
}

impl AttachmentOrigin for RemoteOrigin {
    fn fetch<'a>(
        &'a self,
        id: &'a AttachmentId,
        wavelet: &'a WaveletName,
        upload: &'a mut Upload,
    ) -> BoxFuture<'a, Result<RemoteAttachment, FetchError>> {
        Box::pin(self.fetch_inner(id, wavelet, upload))
    }
}

impl RemoteOrigin {
    async fn fetch_inner(
        &self,
        id: &AttachmentId,
        wavelet: &WaveletName,
        upload: &mut Upload,
    ) -> Result<RemoteAttachment, FetchError> {
        let named = id.origin_domain(wavelet).to_owned();
        let unavailable_from = |domain: &str, message: String| FetchError::Unavailable {
            domain: domain.to_owned(),
            message,
        };
        let failed_from = |domain: &str, e: RequestError| match e {
            RequestError::Remote { error, .. }
                if error.code() == Code::ItemNotFound || error.code() == Code::BadRequest =>
            {
                FetchError::NotFound
            }
            RequestError::Protocol { message, .. } => FetchError::Rejected(message),
            other => unavailable_from(domain, other.to_string()),
        };
        let failed = |e: RequestError| failed_from(&named, e);
        let endpoint = self.ctx.endpoint().map_err(failed)?;
        let op = || {
            federation_request::Op::FetchAttachment(FetchAttachment {
                attachment_id: Some(id.serialise()),
                wavelet_name: Some(wavelet_uri(wavelet)),
            })
        };
        let (answer, origin) = if self.ctx.person_mode && id.domain() == self.ctx.domain {
            // Uploaded to another device of this person: ask each.
            let me = self.ctx.own_key();
            let mut last = FetchError::NotFound;
            let mut found = None;
            let mut asleep = Vec::new();
            let mut devices = self.ctx.trust.devices_of(&named);
            if let Some(at) = self
                .prefer
                .and_then(|prefer| devices.iter().position(|device| *device == prefer))
            {
                let prefer = devices.remove(at);
                devices.insert(0, prefer);
            }
            for device in devices {
                if device == me {
                    continue;
                }
                match endpoint.request_with_body(device, Some(&named), op()).await {
                    Ok(answer) => {
                        found = Some(answer);
                        break;
                    }
                    Err(e) => {
                        last = failed(e);
                        if matches!(last, FetchError::Unavailable { .. }) {
                            asleep.push((device, named.clone()));
                        }
                    }
                }
            }
            if self.wake && found.is_none() && matches!(last, FetchError::Unavailable { .. }) {
                found = self
                    .wake_and_retry(&endpoint, &asleep, &op)
                    .await
                    .map(|(answer, _)| answer);
            }
            match found {
                Some(answer) => (answer, named.clone()),
                None => return Err(last),
            }
        } else {
            // The server the id names; for an id that names none, the
            // wavelet's host and then the servers of its participants.
            let mut last = FetchError::NotFound;
            let mut found = None;
            let mut asleep = Vec::new();
            for domain in self.candidates(id, wavelet).await {
                let (asked, key) = match self.ctx.key_of(&domain).await {
                    Ok(key) => (
                        endpoint.request_with_body(key, Some(&domain), op()).await,
                        Some(key),
                    ),
                    Err(e) => (Err(e), None),
                };
                match asked {
                    Ok(answer) => {
                        found = Some((answer, domain));
                        break;
                    }
                    // "Not there" from one server does not outweigh
                    // another that could not be asked.
                    Err(e) => match failed_from(&domain, e) {
                        FetchError::NotFound => {}
                        other => {
                            if let (FetchError::Unavailable { .. }, Some(key)) = (&other, key) {
                                asleep.push((key, domain.clone()));
                            }
                            last = other;
                        }
                    },
                }
            }
            if self.wake && found.is_none() && matches!(last, FetchError::Unavailable { .. }) {
                found = self.wake_and_retry(&endpoint, &asleep, &op).await;
            }
            match found {
                Some(found) => found,
                None => return Err(last),
            }
        };
        let (result, mut body) = answer;
        let federation_response::Result::FetchAttachment(result) = result else {
            return Err(FetchError::Rejected(
                "answered with something else".to_owned(),
            ));
        };
        let size = result.size.unwrap_or_default();
        let sha256: [u8; 32] = result
            .sha256
            .unwrap_or_default()
            .try_into()
            .map_err(|_| FetchError::Rejected("no SHA-256".to_owned()))?;
        let max = upload.max_size();
        if size > max {
            return Err(FetchError::Rejected(format!(
                "{size} bytes, more than the {max} allowed here"
            )));
        }
        let mut received = 0u64;
        while let Some(chunk) = body.next_chunk().await.map_err(failed)? {
            received += chunk.len() as u64;
            if received > size {
                return Err(FetchError::Rejected("more bytes than announced".to_owned()));
            }
            upload
                .write(&chunk)
                .await
                .map_err(|e| FetchError::Rejected(e.to_string()))?;
        }
        Ok(RemoteAttachment {
            file_name: result.file_name.unwrap_or_default(),
            size,
            creator: result.creator.unwrap_or_default(),
            sha256,
            created_at: result.created_at.unwrap_or_default(),
            origin,
        })
    }

    /// Push: wakes the first of `asleep` (devices that could not be
    /// reached) that has a push target, and asks it again until it
    /// answers or `wake_fetch` has passed. `None` without push, without a
    /// target, or if it stays away (the fetch is then unavailable as
    /// before, and asked again later).
    async fn wake_and_retry(
        &self,
        endpoint: &crate::endpoint::FederationEndpoint,
        asleep: &[(iroh::PublicKey, String)],
        op: &(dyn Fn() -> federation_request::Op + Sync),
    ) -> Option<(
        (federation_response::Result, crate::endpoint::BodyStream),
        String,
    )> {
        let push = self.ctx.push.as_ref()?;
        let (key, domain) = asleep
            .iter()
            .find(|(key, _)| push.request_wake(key, "attachment fetch").woken())?;
        let deadline = tokio::time::Instant::now() + self.ctx.tuning.wake_fetch;
        let mut pause = self.ctx.tuning.wake_retry;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return None;
            }
            tokio::time::sleep(pause.min(deadline - now)).await;
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if let Ok(Ok(answer)) = tokio::time::timeout(
                left.max(std::time::Duration::from_millis(1)),
                endpoint.request_with_body(*key, Some(domain), op()),
            )
            .await
            {
                push.note_reached(key);
                return Some((answer, domain.clone()));
            }
            pause = (pause * 2).min(std::time::Duration::from_secs(2));
        }
    }

    /// The domains to ask for `id` of `wavelet`, in order.
    async fn candidates(&self, id: &AttachmentId, wavelet: &WaveletName) -> Vec<String> {
        if !id.domain().is_empty() {
            return vec![id.domain().to_owned()];
        }
        let mut domains = vec![wavelet.wavelet_id.domain().to_owned()];
        if let Ok(Some(participants)) = self
            .ctx
            .server
            .with_wavelet(wavelet, |wavelet| {
                wavelet
                    .participants()
                    .iter()
                    .map(|p| p.domain().to_owned())
                    .collect::<Vec<_>>()
            })
            .await
        {
            for domain in participants {
                if !domains.contains(&domain) {
                    domains.push(domain);
                }
            }
        }
        domains.retain(|domain| *domain != self.ctx.domain);
        domains.truncate(MAX_CANDIDATES);
        domains
    }
}
