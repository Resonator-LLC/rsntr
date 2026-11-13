//! The wave server's services as the Robots API layer wants them: search,
//! attachments and profiles behind the traits of `resonator_robots::ops`.

use async_trait::async_trait;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_robots::api::{InvalidRequest, ParticipantProfile, RawAttachmentData, SearchDigest};
use resonator_robots::convert::serialise_wave_id;
use resonator_robots::ops::{
    AttachmentService as ApiAttachments, InitialsProfilesFetcher, ProfilesFetcher,
    SearchProvider as ApiSearch, name_from_address,
};
use resonator_server::account::AccountData;
use resonator_server::attachment::{AttachmentId, AttachmentService};
use resonator_server::search::{SearchProvider as _, SearchService};
use resonator_server::storage::AccountStore;

/// `robot.search` on the server's search service.
#[derive(Debug, Clone)]
pub struct SearchAdapter {
    search: SearchService,
    max_results: usize,
}

impl SearchAdapter {
    /// `max_results` caps what one request may ask for
    /// (`search_max_results`).
    pub fn new(search: SearchService, max_results: usize) -> Self {
        SearchAdapter {
            search,
            max_results: max_results.max(1),
        }
    }
}

#[async_trait]
impl ApiSearch for SearchAdapter {
    async fn search(
        &self,
        participant: &ParticipantId,
        query: &str,
        start_at: i32,
        num_results: i32,
    ) -> Result<resonator_robots::api::SearchResult, InvalidRequest> {
        let start = usize::try_from(start_at).unwrap_or(0);
        let count = usize::try_from(num_results)
            .unwrap_or(0)
            .min(self.max_results);
        let found = self
            .search
            .search(participant, query, start, count)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "search for a robot failed");
                InvalidRequest::new("Search failed")
            })?;
        let mut result = resonator_robots::api::SearchResult::new(query);
        for digest in found.digests {
            result.add_digest(SearchDigest {
                title: Some(digest.title),
                snippet: Some(digest.snippet),
                wave_id: Some(serialise_wave_id(&digest.wave_id)),
                last_modified: digest.last_modified,
                created: digest.created,
                unread_count: i32::try_from(digest.unread_count).unwrap_or(i32::MAX),
                blip_count: i32::try_from(digest.blip_count).unwrap_or(i32::MAX),
                participants: digest.participants,
            });
        }
        Ok(result)
    }
}

/// `robot.exportAttachment` and `robot.importAttachment` on the server's
/// attachment store.
#[derive(Clone)]
pub struct AttachmentAdapter {
    attachments: AttachmentService,
}

impl AttachmentAdapter {
    pub fn new(attachments: AttachmentService) -> Self {
        AttachmentAdapter { attachments }
    }
}

#[async_trait]
impl ApiAttachments for AttachmentAdapter {
    async fn export_attachment(&self, attachment_id: &str) -> Result<RawAttachmentData, String> {
        let id = AttachmentId::deserialise(attachment_id)
            .map_err(|_| format!("Invalid attachment id {attachment_id}"))?;
        let unavailable = |error: resonator_server::attachment::AttachmentError| {
            tracing::warn!(%error, "attachment export failed");
            "Attachment storage failed".to_owned()
        };
        let metadata = self
            .attachments
            .metadata(&id)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| format!("No attachment {attachment_id}"))?;
        let data = self
            .attachments
            .read(&id)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| format!("No attachment {attachment_id}"))?;
        Ok(RawAttachmentData {
            file_name: Some(metadata.file_name),
            creator: Some(metadata.creator),
            data,
        })
    }

    async fn import_attachment(
        &self,
        attachment_id: &str,
        wavelet_name: &WaveletName,
        file_name: &str,
        creator: &ParticipantId,
        data: &[u8],
    ) -> Result<(), String> {
        let id = AttachmentId::deserialise(attachment_id)
            .map_err(|_| format!("Invalid attachment id {attachment_id}"))?;
        self.attachments
            .store(&id, wavelet_name, file_name, Some(creator), data)
            .await
            .map(|_| ())
            .map_err(|error| {
                tracing::warn!(%error, "attachment import failed");
                "Storing the attachment failed".to_owned()
            })
    }
}

/// Profiles as the web client's `/profile` shows them (names from the
/// address, initials avatars), with agents' and robots' names marked.
#[derive(Debug, Clone)]
pub struct ProfilesAdapter {
    accounts: AccountStore,
}

impl ProfilesAdapter {
    pub fn new(accounts: AccountStore) -> Self {
        ProfilesAdapter { accounts }
    }
}

#[async_trait]
impl ProfilesFetcher for ProfilesAdapter {
    async fn fetch_profile(&self, address: &str) -> ParticipantProfile {
        let mut profile = InitialsProfilesFetcher.fetch_profile(address).await;
        if let Ok(id) = ParticipantId::of(address)
            && let Ok(Some(AccountData::Robot(robot))) = self.accounts.get_account(&id).await
        {
            let kind = if robot.is_agent() { "agent" } else { "robot" };
            profile.name = Some(format!("{} ({kind})", name_from_address(address)));
        }
        profile
    }
}
