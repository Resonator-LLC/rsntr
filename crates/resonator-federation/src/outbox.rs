//! Fan-out: a host tells the servers of a wavelet's remote participants
//! about its new deltas (`WaveletNotificationDispatcher` and
//! `LocalWaveletContainerImpl.domainsToNotify` in the original).
//!
//! The wave bus feeds [`fan_out`], which notes in `federation_outbox` which
//! remote domains have to hear of which wavelet from which version on: the
//! domains of the participants after the deltas plus those the deltas
//! removed, minus this server's own. One row per domain and wavelet, so
//! notifications coalesce. [`worker`] delivers the rows: an `update` with
//! the applied deltas since the row's version (in pages under the frame
//! limit), then a `commit`. A remote `Ok` acknowledges; a failure is
//! retried with exponential backoff, and a row undelivered for days
//! expires. The queue is in SQLite, so retries survive a restart.
//!
//! A domain of a person (see [`crate::person`]) stands for each of its
//! known devices: its rows are `<domain>/<device key>`, one per device,
//! each delivered to that device. In person mode, the other devices of
//! this node's own person get rows too. A person domain whose devices
//! are not known here is reached through its device directory
//! ([`crate::directory`]): its row is handed to the devices listed.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use resonator_model::id::{IdUriCodec, ParticipantId, WaveletName};
use resonator_model::operation::WaveletOperationKind;
use resonator_model::proto::calculate_resulting_hashed_version;
use resonator_model::proto::federation_rpc::{
    Commit, Update, federation_request, federation_response,
};
use resonator_server::storage::{OutboxEntry, OutboxStatus};
use resonator_server::waveserver::WaveBusEvent;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::context::{Context, parse_target, to_proto_version, wavelet_uri};
use crate::endpoint::{RequestError, new_request_id};

/// The remote domains to notify of an update: participants after it plus
/// participants it removed, minus `local` (`domainsToNotify`).
pub fn domains_to_notify(
    participants: &[ParticipantId],
    removed: impl IntoIterator<Item = ParticipantId>,
    local: &str,
) -> BTreeSet<String> {
    participants
        .iter()
        .cloned()
        .chain(removed)
        .map(|participant| participant.domain().to_owned())
        .filter(|domain| domain != local)
        .collect()
}

/// Turns wave bus updates of hosted wavelets into outbox rows.
pub(crate) async fn fan_out(
    ctx: Arc<Context>,
    mut events: mpsc::UnboundedReceiver<WaveBusEvent>,
    cancel: CancellationToken,
) {
    loop {
        let event = tokio::select! {
            () = cancel.cancelled() => break,
            event = events.recv() => event,
        };
        let Some(event) = event else {
            break;
        };
        let WaveBusEvent::Update {
            wavelet_name,
            deltas,
            participants,
            ..
        } = event
        else {
            continue;
        };
        if !ctx.server.is_local_wavelet(&wavelet_name) {
            continue;
        }
        let Some(first) = deltas.first() else {
            continue;
        };
        let removed = deltas.iter().flat_map(|delta| {
            delta
                .transformed
                .ops()
                .iter()
                .filter_map(|op| match op.kind() {
                    WaveletOperationKind::RemoveParticipant { participant } => {
                        Some(participant.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        // In person mode the other devices of this node's person are told
        // too (they are replicas of its wavelets).
        let local = if ctx.person_mode {
            ""
        } else {
            ctx.domain.as_str()
        };
        let domains: BTreeSet<String> = domains_to_notify(&participants, removed, local)
            .iter()
            .flat_map(|domain| ctx.targets(domain))
            .collect();
        if domains.is_empty() {
            continue;
        }
        let uri = wavelet_uri(&wavelet_name);
        let from = first.applied_at_version.version();
        for domain in domains {
            if let Err(error) = ctx
                .store
                .outbox_enqueue(&domain, &uri, from, &new_request_id(), ctx.now())
                .await
            {
                tracing::error!(%domain, wavelet = %wavelet_name, %error, "cannot queue a federation notification");
            }
        }
        ctx.outbox_wake.notify_one();
    }
}

/// At start: a delivered row whose wavelet has moved on since, while the
/// domain still has a participant on it, is due again. This catches deltas
/// whose notification was lost (a crash after the delta was stored but
/// before its row was), and the remote server fills any gap with
/// `history`. Expired rows stay expired.
pub(crate) async fn reconcile(ctx: Arc<Context>) {
    let entries = match ctx.store.outbox_entries().await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(%error, "cannot read the federation outbox");
            return;
        }
    };
    let mut requeued = 0usize;
    for entry in entries {
        if entry.status != OutboxStatus::Delivered {
            continue;
        }
        let Ok(name) = IdUriCodec::uri_to_wavelet_name(&entry.wavelet) else {
            continue;
        };
        if !ctx.server.is_local_wavelet(&name) {
            continue;
        }
        let Ok(current) = ctx.server.current_version(&name).await else {
            continue;
        };
        if current.version() <= entry.from_version {
            continue;
        }
        let participates = ctx
            .server
            .with_wavelet(&name, |wavelet| {
                wavelet
                    .participants()
                    .iter()
                    .any(|p| p.domain() == parse_target(&entry.domain).0)
            })
            .await;
        if !matches!(participates, Ok(Some(true))) {
            continue;
        }
        match ctx
            .store
            .outbox_enqueue(
                &entry.domain,
                &entry.wavelet,
                entry.from_version,
                &new_request_id(),
                ctx.now(),
            )
            .await
        {
            Ok(()) => requeued += 1,
            Err(error) => tracing::error!(%error, "cannot queue a federation notification"),
        }
    }
    if requeued > 0 {
        tracing::info!(requeued, "federation notifications found missing at start");
        ctx.outbox_wake.notify_one();
    }
}

/// Delivers due outbox rows until cancelled.
pub(crate) async fn worker(ctx: Arc<Context>, cancel: CancellationToken) {
    loop {
        if !ctx.outbox_paused.load(Ordering::SeqCst) {
            match ctx.store.outbox_due(ctx.now(), 64).await {
                Ok(due) => {
                    let mut deliveries = tokio::task::JoinSet::new();
                    for entry in due {
                        let ctx = Arc::clone(&ctx);
                        deliveries.spawn(async move { deliver(&ctx, entry).await });
                    }
                    // A delivery cut short at shutdown is retried after
                    // the next start: the outbox row is still pending.
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        () = async { while deliveries.join_next().await.is_some() {} } => {}
                    }
                }
                Err(error) => tracing::error!(%error, "cannot read the federation outbox"),
            }
        }
        let wait = match ctx.store.outbox_next_at().await {
            Ok(Some(next_at)) => {
                Duration::from_millis(next_at.saturating_sub(ctx.now()).max(0) as u64)
            }
            _ => ctx.tuning.retry_max,
        }
        .clamp(Duration::from_millis(10), ctx.tuning.retry_max);
        tokio::select! {
            () = cancel.cancelled() => break,
            () = ctx.outbox_wake.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// Sends one row: the deltas since its version, then the commit.
async fn deliver(ctx: &Context, entry: OutboxEntry) {
    let expiry = i64::try_from(ctx.tuning.outbox_expiry.as_millis()).unwrap_or(i64::MAX);
    if entry
        .pending_since
        .is_some_and(|since| ctx.now().saturating_sub(since) > expiry)
    {
        tracing::warn!(domain = %entry.domain, wavelet = %entry.wavelet, "federation notification expired");
        if let Err(error) = ctx.store.outbox_expire(&entry.domain, &entry.wavelet).await {
            tracing::error!(%error, "cannot expire a federation notification");
        }
        return;
    }
    let Ok(name) = IdUriCodec::uri_to_wavelet_name(&entry.wavelet) else {
        tracing::error!(wavelet = %entry.wavelet, "bad wavelet name in the federation outbox");
        let _ = ctx.store.outbox_expire(&entry.domain, &entry.wavelet).await;
        return;
    };
    // A bare person domain names no endpoint: the row goes to its
    // devices instead (one row each), as its device directory lists them.
    if ctx.server.is_local_wavelet(&name)
        && let Some(devices) = crate::directory::hand_to_devices(ctx, &entry.domain).await
    {
        hand_over(ctx, &entry, &devices).await;
        return;
    }
    let mut sent_to = entry.from_version;
    // A wavelet hosted elsewhere now (after a change of host epoch) is
    // the new host's to send.
    let result = if ctx.server.is_local_wavelet(&name) {
        send(ctx, &entry, &name, &mut sent_to).await
    } else {
        tracing::debug!(domain = %entry.domain, wavelet = %name, "no longer the host; dropping a notification");
        Ok(())
    };
    match result {
        Ok(()) => {
            if let Some(push) = &ctx.push
                && let Some(device) = target_device(&entry.domain)
            {
                push.note_reached(&device);
            }
            tracing::debug!(domain = %entry.domain, wavelet = %name, version = sent_to, "federation notification delivered");
            if let Err(error) = ctx
                .store
                .outbox_delivered(
                    &entry.domain,
                    &entry.wavelet,
                    &entry.request_id,
                    sent_to,
                    ctx.now(),
                )
                .await
            {
                tracing::error!(%error, "cannot record a federation delivery");
            }
        }
        Err(error) => {
            let mut wait = ctx.tuning.backoff(entry.attempts);
            // A device that is asleep (and none of its domain's devices
            // up) is woken, and tried again soon.
            if wake_for(ctx, &entry.domain) {
                wait = wait.min(ctx.tuning.wake_retry);
            }
            tracing::info!(
                domain = %entry.domain,
                wavelet = %name,
                attempts = entry.attempts + 1,
                retry_in_ms = wait.as_millis() as u64,
                %error,
                "federation notification not delivered"
            );
            // Deltas that got through are not sent again; what is left
            // goes under a new request id, since it is another request.
            let request_id = if sent_to > entry.from_version {
                new_request_id()
            } else {
                entry.request_id.clone()
            };
            let next_at = ctx
                .now()
                .saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
            if let Err(error) = ctx
                .store
                .outbox_retry(&entry.domain, &entry.wavelet, sent_to, &request_id, next_at)
                .await
            {
                tracing::error!(%error, "cannot record a federation retry");
            }
        }
    }
}

/// Replaces the row of a bare person domain with one row per device
/// (`<domain>/<device key>`) from the same version on.
async fn hand_over(ctx: &Context, entry: &OutboxEntry, devices: &[iroh::PublicKey]) {
    for device in devices {
        let target = format!("{}/{}", entry.domain, device.to_z32());
        if let Err(error) = ctx
            .store
            .outbox_enqueue(
                &target,
                &entry.wavelet,
                entry.from_version,
                &new_request_id(),
                ctx.now(),
            )
            .await
        {
            tracing::error!(%target, %error, "cannot queue a federation notification");
            return;
        }
    }
    tracing::info!(
        domain = %entry.domain,
        wavelet = %entry.wavelet,
        devices = devices.len(),
        "handed a notification to the devices of a person"
    );
    if let Err(error) = ctx.store.outbox_expire(&entry.domain, &entry.wavelet).await {
        tracing::error!(%error, "cannot retire a federation notification");
    }
    ctx.outbox_wake.notify_one();
}

/// The device of a `<domain>/<device key>` outbox target.
fn target_device(target: &str) -> Option<iroh::PublicKey> {
    let (_, device) = crate::context::parse_target(target);
    crate::key::parse_key_domain(device?).ok().flatten()
}

/// Push: wakes the device of an outbox target that was not reached,
/// unless another device of its domain is connected; returns whether it
/// was just woken.
fn wake_for(ctx: &Context, target: &str) -> bool {
    let Some(push) = &ctx.push else {
        return false;
    };
    let Some(device) = target_device(target) else {
        return false;
    };
    let (domain, _) = crate::context::parse_target(target);
    if let Ok(endpoint) = ctx.endpoint() {
        let connected = endpoint.connected_peers();
        if ctx
            .trust
            .devices_of(domain)
            .iter()
            .any(|other| *other != device && connected.contains(other))
        {
            return false;
        }
    }
    push.request_wake(&device, "undelivered update").woken()
}

fn internal(message: impl std::fmt::Display) -> RequestError {
    RequestError::Protocol {
        peer: "local".to_owned(),
        message: message.to_string(),
    }
}

async fn send(
    ctx: &Context,
    entry: &OutboxEntry,
    name: &WaveletName,
    sent_to: &mut u64,
) -> Result<(), RequestError> {
    let Some((current, _)) = ctx.server.get_versions(name).await.map_err(internal)? else {
        // Nothing (left) to send.
        return Ok(());
    };
    let uri = wavelet_uri(name);
    let mut start = match ctx
        .server
        .hashed_version_at(name, entry.from_version.min(current.version()))
        .await
        .map_err(internal)?
    {
        Some(start) => start,
        None => ctx.server.version_zero(name),
    };
    let mut first = true;
    while start.version() < current.version() {
        let deltas = ctx
            .server
            .get_applied_history(name, &start, &current, Some(ctx.tuning.page_deltas))
            .await
            .map_err(internal)?;
        let mut page = Vec::new();
        let mut bytes = 0usize;
        let mut end = start.clone();
        for delta in &deltas {
            if !page.is_empty() && bytes + delta.bytes().len() > ctx.tuning.page_bytes {
                break;
            }
            bytes += delta.bytes().len();
            end = calculate_resulting_hashed_version(delta).map_err(internal)?;
            page.push(delta.bytes().to_vec());
        }
        if page.is_empty() {
            return Err(internal("no deltas to send"));
        }
        let id = if first {
            entry.request_id.clone()
        } else {
            new_request_id()
        };
        first = false;
        let update = Update {
            wavelet_name: Some(uri.clone()),
            applied_delta: page,
            epoch_start: crate::epoch::statements(&ctx.server, name),
        };
        let answer = ctx
            .request_target(
                &entry.domain,
                Some(id),
                federation_request::Op::Update(update),
            )
            .await?;
        if step_down(ctx, name, answer).await? {
            return Ok(());
        }
        *sent_to = end.version();
        start = end;
    }
    let committed = tokio::time::timeout(
        ctx.tuning.commit_wait,
        ctx.server.wait_committed(name, current.version()),
    )
    .await
    .map_err(|_| internal("the commit did not come in time"))?
    .map_err(internal)?;
    let commit = Commit {
        wavelet_name: Some(uri),
        committed_version: Some(to_proto_version(&committed).map_err(internal)?),
        epoch_start: crate::epoch::statements(&ctx.server, name),
    };
    let answer = ctx
        .request_target(&entry.domain, None, federation_request::Op::Commit(commit))
        .await?;
    if step_down(ctx, name, answer).await? {
        return Ok(());
    }
    *sent_to = (*sent_to).max(current.version());
    Ok(())
}

/// Handles a peer's answer that another server hosts the wavelet in a
/// newer epoch: adopts that epoch, which rewinds the history here and
/// turns this server into a replica. Returns whether that happened.
async fn step_down(
    ctx: &Context,
    name: &WaveletName,
    answer: federation_response::Result,
) -> Result<bool, RequestError> {
    let federation_response::Result::Fenced(fenced) = answer else {
        return Ok(false);
    };
    tracing::info!(wavelet = %name, "a peer knows a newer host; stepping down");
    crate::epoch::adopt(ctx, name, &fenced.epoch_start)
        .await
        .map_err(|error| internal(format!("cannot step down: {error}")))?;
    if ctx.server.is_local_wavelet(name) {
        return Err(internal("fenced, but still the host"));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_to_notify_are_remote_participants_and_removed_ones() {
        let p = |a: &str| ParticipantId::of(a).unwrap();
        let domains = domains_to_notify(
            &[p("alice@a.example"), p("bob@b.example"), p("@c.example")],
            [p("dan@d.example"), p("anne@a.example")],
            "a.example",
        );
        assert_eq!(
            domains.into_iter().collect::<Vec<_>>(),
            ["b.example", "c.example", "d.example"]
        );
    }
}
