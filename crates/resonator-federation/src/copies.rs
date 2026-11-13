//! Person mode: attachments copied ahead between the devices of a person
//! (`[federation] replicate_own_attachments`), so that a phone can show
//! an image uploaded on a tablet that has since gone to sleep.
//!
//! - **Announce.** An attachment uploaded on a device (not a fetched
//!   copy), of the person's domain and within `replicate_max_mb`, is told
//!   at once to every other device of the person (`attachment-copies`),
//!   in the background: the upload's answer does not wait.
//! - **Pull.** A device told of one copies it with `fetch-attachment`
//!   from the device that told it (others if that fails, never waking
//!   one), through the attachment store's usual fetch: size and SHA-256
//!   checked, stored like any fetched copy, thumbnail made.
//! - **Catch-up.** After each device sync, a device asks the other for a
//!   page of the attachments of the person's domain it holds
//!   (`own-attachments`, ordered by creation time and id, from where the
//!   last round stopped) and copies those of wavelets it holds that it
//!   lacks. Bounded per round (items and bytes) and in concurrency; the
//!   position is kept in memory and the scan starts over now and then (a
//!   copy may be older than the position).
//!
//! The copies count against `attachment_cache_mb` like every other fetched
//! copy, and a device whose cache has no room left for one skips it (it
//! is fetched when opened, as before): copying ahead never evicts
//! anything. Larger attachments and those skipped stay lazy. Other
//! people's devices are not told and keep fetching when a user opens one.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use iroh::PublicKey;
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    Ack, AttachmentCopies, AttachmentNotice, OwnAttachments, OwnAttachmentsResult,
    federation_request, federation_response,
};
use resonator_server::attachment::{
    AttachmentId, AttachmentMetadata, AttachmentService, FetchError, UploadListener,
};
use tokio::sync::Semaphore;

use crate::attachments::RemoteOrigin;
use crate::context::{Context, error, lock, parse_wavelet_name, wavelet_uri};
use crate::endpoint::Peer;

/// Copies being fetched at a time (told and caught up alike).
const PULLS: usize = 2;
/// Most notices taken from one `attachment-copies`.
const NOTICES_PER_MESSAGE: usize = 64;
/// Entries in one page of `own-attachments`.
const PAGE: u32 = 128;
/// Most copies fetched in one catch-up round with a device.
const ROUND_ITEMS: usize = 256;
/// Most bytes fetched in one catch-up round, in multiples of the largest
/// attachment copied ahead.
const ROUND_BYTES_FACTOR: u64 = 4;
/// How long a catch-up position with a device is kept before the scan
/// starts over.
const RESCAN: Duration = Duration::from_secs(60 * 60);

/// Where a catch-up with one device stands.
struct Cursor {
    /// The creation time and id of the last entry dealt with.
    after: Option<(i64, String)>,
    /// When the scan began.
    since: Instant,
}

/// What became of one attachment told of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Copied here now.
    Copied,
    /// Here already, or not to be copied (too large, no room, refused).
    Skipped,
    /// Could not be had now; try again in a later round.
    Later,
}

/// The copying of a person's attachments between its devices.
pub(crate) struct Copies {
    attachments: AttachmentService,
    /// Largest attachment copied ahead.
    max_size: u64,
    pulls: Semaphore,
    cursors: Mutex<HashMap<PublicKey, Cursor>>,
    /// Devices a catch-up runs with now.
    catching_up: Mutex<HashSet<PublicKey>>,
}

impl Copies {
    pub(crate) fn new(attachments: AttachmentService, max_size: u64) -> Self {
        Copies {
            attachments,
            max_size,
            pulls: Semaphore::new(PULLS),
            cursors: Mutex::new(HashMap::new()),
            catching_up: Mutex::new(HashSet::new()),
        }
    }

    /// Installs the announcer of uploads in the attachment store.
    pub(crate) fn install(ctx: &Arc<Context>, copies: Arc<Copies>) {
        copies
            .attachments
            .set_upload_listener(Some(Arc::new(Announcer {
                ctx: Arc::downgrade(ctx),
            })));
        ctx.set_copies(Some(copies));
    }

    /// Stops copying (federation shuts down).
    pub(crate) fn uninstall(ctx: &Context) {
        if let Some(copies) = ctx.copies() {
            copies.attachments.set_upload_listener(None);
        }
        ctx.set_copies(None);
    }

    /// Whether `notice` names an attachment worth copying here: of the
    /// person's domain, within the size limit, not here (a copy evicted
    /// for room stays lazy) and, if `held_only`, of a wavelet held here.
    async fn wanted(
        &self,
        ctx: &Context,
        notice: &AttachmentNotice,
        held_only: bool,
    ) -> Option<(AttachmentId, resonator_model::id::WaveletName)> {
        let id = AttachmentId::deserialise(notice.attachment_id.as_deref()?).ok()?;
        let name = parse_wavelet_name(notice.wavelet_name.as_deref()).ok()?;
        if id.domain() != ctx.domain || notice.size.unwrap_or(u64::MAX) > self.max_size {
            return None;
        }
        if !matches!(self.attachments.metadata(&id).await, Ok(None)) {
            return None;
        }
        if held_only && !matches!(ctx.server.with_wavelet(&name, |_| ()).await, Ok(Some(()))) {
            return None;
        }
        Some((id, name))
    }

    /// Copies one attachment told of by `from`.
    async fn pull(
        &self,
        ctx: &Arc<Context>,
        from: PublicKey,
        notice: &AttachmentNotice,
        held_only: bool,
    ) -> Outcome {
        let Some((id, name)) = self.wanted(ctx, notice, held_only).await else {
            return Outcome::Skipped;
        };
        let Ok(_permit) = self.pulls.acquire().await else {
            return Outcome::Later;
        };
        let size = notice.size.unwrap_or_default();
        match self.attachments.cache_free().await {
            Ok(free) if free >= size => {}
            Ok(_) => {
                tracing::debug!(attachment = %id, size, "no room to copy an attachment ahead");
                return Outcome::Skipped;
            }
            Err(_) => return Outcome::Later,
        }
        let origin = RemoteOrigin {
            ctx: Arc::clone(ctx),
            prefer: Some(from),
            wake: false,
        };
        match self
            .attachments
            .fetch_copy(&id, &name, &origin, self.max_size)
            .await
        {
            Ok(_) => {
                tracing::debug!(attachment = %id, size, peer = %from.fmt_short(), "copied an attachment from another device");
                Outcome::Copied
            }
            Err(FetchError::Unavailable { message, .. }) => {
                tracing::debug!(attachment = %id, %message, "cannot copy an attachment now");
                Outcome::Later
            }
            Err(error) => {
                tracing::debug!(attachment = %id, %error, "not copying an attachment");
                Outcome::Skipped
            }
        }
    }

    /// One catch-up round with `device`: pages of what it holds, copies
    /// of what is missing here, until the round's limits or the end.
    async fn catch_up(&self, ctx: &Arc<Context>, device: PublicKey) {
        let Ok(endpoint) = ctx.endpoint() else {
            return;
        };
        let round_bytes = self.max_size.saturating_mul(ROUND_BYTES_FACTOR);
        let (mut items, mut bytes) = (0usize, 0u64);
        loop {
            let after = {
                let mut cursors = lock(&self.cursors);
                let cursor = cursors.entry(device).or_insert_with(|| Cursor {
                    after: None,
                    since: Instant::now(),
                });
                if cursor.since.elapsed() >= RESCAN {
                    *cursor = Cursor {
                        after: None,
                        since: Instant::now(),
                    };
                }
                cursor.after.clone()
            };
            let request = OwnAttachments {
                after_created_at: after.as_ref().map(|(at, _)| *at),
                after_id: after.map(|(_, id)| id),
                limit: Some(PAGE),
            };
            let answer = endpoint
                .request(
                    device,
                    Some(&ctx.domain),
                    federation_request::Op::OwnAttachments(request),
                )
                .await;
            let page = match answer {
                Ok(federation_response::Result::OwnAttachments(page)) => page,
                Ok(_) => return,
                Err(error) => {
                    tracing::trace!(peer = %device.fmt_short(), %error, "no attachment catch-up");
                    return;
                }
            };
            let notices = page.notice;
            if notices.is_empty() {
                return;
            }
            // What to copy of this page, within the round's limits.
            let mut cut = notices.len();
            let mut plan = Vec::new();
            for (i, notice) in notices.iter().enumerate() {
                if self.wanted(ctx, notice, true).await.is_none() {
                    continue;
                }
                let size = notice.size.unwrap_or_default();
                if items + 1 > ROUND_ITEMS || bytes.saturating_add(size) > round_bytes {
                    cut = i;
                    break;
                }
                items += 1;
                bytes = bytes.saturating_add(size);
                plan.push(i);
            }
            let outcomes: Vec<(usize, Outcome)> = futures_util::stream::iter(plan)
                .map(|i| {
                    let notice = &notices[i];
                    async move { (i, self.pull(ctx, device, notice, true).await) }
                })
                .buffered(PULLS)
                .collect()
                .await;
            if let Some((i, _)) = outcomes
                .iter()
                .find(|(_, outcome)| *outcome == Outcome::Later)
            {
                cut = cut.min(*i);
            }
            if cut > 0 {
                let last = &notices[cut - 1];
                if let Some(cursor) = lock(&self.cursors).get_mut(&device) {
                    cursor.after = Some((
                        last.created_at.unwrap_or_default(),
                        last.attachment_id.clone().unwrap_or_default(),
                    ));
                }
            }
            let copied = outcomes
                .iter()
                .filter(|(_, outcome)| *outcome == Outcome::Copied)
                .count();
            if copied > 0 {
                tracing::debug!(peer = %device.fmt_short(), copied, "caught up on attachments");
            }
            if cut < notices.len() || !page.more.unwrap_or(false) {
                return;
            }
        }
    }
}

/// The notice of an attachment held here.
fn notice_of(metadata: &AttachmentMetadata) -> Option<AttachmentNotice> {
    Some(AttachmentNotice {
        attachment_id: Some(metadata.id.serialise()),
        wavelet_name: Some(wavelet_uri(&metadata.wavelet_name()?)),
        size: Some(metadata.size),
        sha256: Some(metadata.sha256.to_vec()),
        image: Some(metadata.image.is_some()),
        created_at: Some(metadata.created_at),
    })
}

/// Tells the person's other devices of attachments uploaded here.
struct Announcer {
    ctx: Weak<Context>,
}

impl UploadListener for Announcer {
    fn uploaded(&self, metadata: &AttachmentMetadata) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let Some(copies) = ctx.copies() else {
            return;
        };
        if metadata.id.domain() != ctx.domain || metadata.size > copies.max_size {
            return;
        }
        let Some(notice) = notice_of(metadata) else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(announce(ctx, notice));
    }
}

/// Sends `notice` to every other device of the person, at once; those
/// that cannot be reached catch up when they sync.
async fn announce(ctx: Arc<Context>, notice: AttachmentNotice) {
    let Ok(endpoint) = ctx.endpoint() else {
        return;
    };
    let me = ctx.own_key();
    let told = ctx
        .trust
        .devices_of(&ctx.domain)
        .into_iter()
        .filter(|device| *device != me)
        .map(|device| {
            let (endpoint, ctx, notice) = (endpoint.clone(), &ctx, notice.clone());
            async move {
                let op = federation_request::Op::AttachmentCopies(AttachmentCopies {
                    notice: vec![notice],
                });
                if let Err(error) = endpoint.request(device, Some(&ctx.domain), op).await {
                    tracing::trace!(peer = %device.fmt_short(), %error, "cannot tell a device of an attachment");
                }
            }
        });
    futures_util::future::join_all(told).await;
}

/// Answers `attachment-copies` from another device of this person: copies
/// what it tells of in the background.
pub(crate) fn serve_copies(
    ctx: &Arc<Context>,
    peer: &Peer,
    request: AttachmentCopies,
) -> federation_response::Result {
    if !ctx.is_own_device(&peer.key) {
        return error(
            Code::NotAuthorized,
            "attachment-copies is for devices of one person",
        );
    }
    let Some(copies) = ctx.copies() else {
        // Not copying ahead here: they are fetched when opened.
        return federation_response::Result::Ack(Ack {});
    };
    let (ctx, from) = (Arc::clone(ctx), peer.key);
    tokio::spawn(async move {
        for notice in request.notice.into_iter().take(NOTICES_PER_MESSAGE) {
            copies.pull(&ctx, from, &notice, false).await;
        }
    });
    federation_response::Result::Ack(Ack {})
}

/// Answers `own-attachments` from another device of this person: a page
/// of the attachments of the person's domain held here.
pub(crate) async fn serve_own(
    ctx: &Context,
    attachments: &AttachmentService,
    peer: &Peer,
    request: OwnAttachments,
) -> federation_response::Result {
    if !ctx.is_own_device(&peer.key) {
        return error(
            Code::NotAuthorized,
            "own-attachments is for devices of one person",
        );
    }
    let limit = request.limit.unwrap_or(PAGE).clamp(1, PAGE) as usize;
    let after = request
        .after_created_at
        .map(|at| (at, request.after_id.unwrap_or_default()));
    match attachments.held_of_domain(&ctx.domain, after, limit).await {
        Ok(held) => federation_response::Result::OwnAttachments(OwnAttachmentsResult {
            more: Some(held.len() == limit),
            notice: held.iter().filter_map(notice_of).collect(),
        }),
        Err(e) => error(Code::InternalServerError, e.to_string()),
    }
}

/// After a sync with `device`: a catch-up round with it in the
/// background, unless one runs already.
pub(crate) fn catch_up_with(ctx: &Arc<Context>, device: PublicKey) {
    let Some(copies) = ctx.copies() else {
        return;
    };
    if !lock(&copies.catching_up).insert(device) {
        return;
    }
    let ctx = Arc::clone(ctx);
    tokio::spawn(async move {
        copies.catch_up(&ctx, device).await;
        lock(&copies.catching_up).remove(&device);
    });
}
