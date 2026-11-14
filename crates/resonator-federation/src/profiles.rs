//! `fetch-profile`: names and avatars between domains, and between the
//! devices of a person.
//!
//! - Answering: the own profiles (accounts of this domain) asked for that
//!   are newer than the asker has; to another device of the person also
//!   the own profiles it did not ask for, so a new one reaches it.
//! - Asking ([`Fetcher`]): the domain of each address, authorised for it
//!   (so only that domain speaks for its addresses).
//! - Device sync ([`sync_with`]): after each wave sync with another device
//!   of the person, the newer versions of the person's own profiles.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::future::{BoxFuture, join_all};
use iroh::PublicKey;
use resonator_model::id::ParticipantId;
use resonator_model::proto::federation_rpc::{
    FetchProfile, FetchProfileResult, MAX_FETCH_PROFILE, ProfileEntry, ProfileWant,
    federation_request, federation_response,
};
use resonator_server::profile::{Fetched, MAX_AVATAR_PNG, RemoteProfiles};
use resonator_server::storage::ProfileRecord;

use crate::context::Context;
use crate::endpoint::Peer;

/// Most avatar bytes in one answer (well below the frame limit); the
/// rest is sent when asked again.
const ANSWER_BUDGET: usize = 2 * 1024 * 1024;

fn entry(record: ProfileRecord) -> ProfileEntry {
    ProfileEntry {
        address: Some(record.address),
        name: Some(record.name),
        updated_at: Some(record.updated_at),
        avatar: record.avatar,
    }
}

fn record(entry: ProfileEntry) -> Option<ProfileRecord> {
    Some(ProfileRecord {
        address: entry.address?,
        name: entry.name?,
        updated_at: entry.updated_at?,
        avatar: entry.avatar.filter(|png| png.len() <= MAX_AVATAR_PNG),
    })
}

pub(crate) async fn serve(
    ctx: &Context,
    peer: &Peer,
    request: FetchProfile,
) -> federation_response::Result {
    let store = ctx.server.database().profile_store();
    let own_device = ctx.is_own_device(&peer.key);
    let mut wanted: HashMap<String, i64> = HashMap::new();
    for want in request.want.into_iter().take(MAX_FETCH_PROFILE) {
        let Some(address) = want.address else {
            continue;
        };
        if ParticipantId::of(&address).is_ok_and(|id| id.domain() == ctx.domain) {
            wanted.insert(address, want.have_updated_at.unwrap_or(-1));
        }
    }
    let mut addresses: Vec<(String, i64)> = wanted.clone().into_iter().collect();
    if own_device && let Ok(own) = store.own().await {
        for profile in own {
            if !wanted.contains_key(&profile.address) {
                addresses.push((profile.address, -1));
            }
        }
    }
    let mut profiles = Vec::new();
    let mut budget = ANSWER_BUDGET;
    for (address, have) in addresses {
        let Ok(Some(stored)) = store.get(&address).await else {
            continue;
        };
        if !stored.own || stored.updated_at <= have {
            continue;
        }
        let Ok(Some(record)) = store.record(&address).await else {
            continue;
        };
        let size = record.avatar.as_ref().map_or(0, Vec::len);
        if size > budget {
            continue;
        }
        budget -= size;
        profiles.push(entry(record));
    }
    federation_response::Result::FetchProfile(FetchProfileResult { profile: profiles })
}

/// Fetches profiles from the domains of their addresses.
pub(crate) struct Fetcher {
    pub(crate) ctx: Arc<Context>,
}

impl RemoteProfiles for Fetcher {
    fn fetch<'a>(
        &'a self,
        addresses: &'a [ParticipantId],
        have: &'a HashMap<String, i64>,
    ) -> BoxFuture<'a, Fetched> {
        Box::pin(fetch(&self.ctx, addresses, have))
    }
}

async fn fetch(ctx: &Context, addresses: &[ParticipantId], have: &HashMap<String, i64>) -> Fetched {
    let mut by_domain: HashMap<&str, Vec<&ParticipantId>> = HashMap::new();
    for address in addresses {
        let domain = address.domain();
        if domain != ctx.domain && ctx.trust.policy().allows(domain) {
            by_domain.entry(domain).or_default().push(address);
        }
    }
    let asks = by_domain.into_iter().flat_map(|(domain, list)| {
        list.chunks(MAX_FETCH_PROFILE)
            .map(|chunk| (domain, chunk.to_vec()))
            .collect::<Vec<_>>()
    });
    let answers = join_all(asks.map(|(domain, chunk)| async move {
        let want = chunk
            .iter()
            .map(|id| ProfileWant {
                address: Some(id.address().to_owned()),
                have_updated_at: have.get(id.address()).copied(),
            })
            .collect();
        let op = federation_request::Op::FetchProfile(FetchProfile { want });
        match ctx.request(domain, None, op).await {
            Ok(federation_response::Result::FetchProfile(result)) => Some((chunk, result)),
            Ok(_) => None,
            Err(error) => {
                tracing::debug!(%domain, %error, "no profiles from a domain");
                None
            }
        }
    }))
    .await;
    let mut fetched = Fetched::default();
    for (chunk, result) in answers.into_iter().flatten() {
        let asked: Vec<&str> = chunk.iter().map(|id| id.address()).collect();
        fetched
            .answered
            .extend(asked.iter().map(|a| (*a).to_owned()));
        fetched.records.extend(
            result
                .profile
                .into_iter()
                .filter_map(record)
                // Only what was asked of that domain.
                .filter(|r| asked.contains(&r.address.as_str())),
        );
    }
    fetched
}

/// Pulls the person's own profiles that `key` (another device of the
/// person) has newer versions of.
pub(crate) async fn sync_with(ctx: &Arc<Context>, key: PublicKey) {
    let Some(profiles) = ctx.profiles.get() else {
        return;
    };
    let Ok(own) = profiles.store().own().await else {
        return;
    };
    let want = own
        .iter()
        .take(MAX_FETCH_PROFILE)
        .map(|p| ProfileWant {
            address: Some(p.address.clone()),
            have_updated_at: Some(p.updated_at),
        })
        .collect();
    let op = federation_request::Op::FetchProfile(FetchProfile { want });
    let answer = match ctx.endpoint() {
        Ok(endpoint) => endpoint.request(key, Some(&ctx.domain), op).await,
        Err(error) => Err(error),
    };
    let Ok(federation_response::Result::FetchProfile(result)) = answer else {
        return;
    };
    for record in result.profile.into_iter().filter_map(record) {
        if !ParticipantId::of(&record.address).is_ok_and(|id| id.domain() == ctx.domain) {
            continue;
        }
        if let Err(error) = profiles.learn_own(record).await {
            tracing::warn!(%error, "could not keep a profile from another device");
        }
    }
}
