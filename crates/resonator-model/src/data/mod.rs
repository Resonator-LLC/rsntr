//! Wavelet state: participants, documents and their metadata.
//!
//! Derived from `org.waveprotocol.wave.model.wave.data` (`WaveletDataImpl`,
//! `BlipDataImpl`) of Apache Wave, licensed under the Apache License,
//! Version 2.0. Listener plumbing is not ported.
//!
//! Documents are held as [`BootstrapDocument`]s. Unlike the original, which
//! iterates documents in hash-map order, documents are kept ordered by id
//! (UTF-16 order).

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;

use crate::docop::utf16::Utf16Key;
use crate::docop::{BootstrapDocument, DocInitialization, OperationError};
use crate::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use crate::version::HashedVersion;

#[cfg(test)]
mod tests;

/// Misuse of the wavelet data API (`IllegalArgumentException` and
/// `IndexOutOfBoundsException` in the original).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DataError {
    #[error("Duplicate doc id: {0}")]
    DuplicateDocument(String),
    #[error("position {position} out of bounds for {size} participants")]
    PositionOutOfBounds { position: usize, size: usize },
    #[error("invalid document content: {0}")]
    Content(#[from] OperationError),
}

/// A document in a wavelet with its metadata. Blips and data documents are
/// both represented this way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlipData {
    id: String,
    author: ParticipantId,
    /// Insertion-ordered set.
    contributors: Vec<ParticipantId>,
    content: BootstrapDocument,
    last_modified_time: i64,
    last_modified_version: u64,
}

impl BlipData {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn author(&self) -> &ParticipantId {
        &self.author
    }

    /// Contributors in the order they were added.
    pub fn contributors(&self) -> &[ParticipantId] {
        &self.contributors
    }

    pub fn content(&self) -> &BootstrapDocument {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut BootstrapDocument {
        &mut self.content
    }

    /// Milliseconds since the epoch.
    pub fn last_modified_time(&self) -> i64 {
        self.last_modified_time
    }

    pub fn last_modified_version(&self) -> u64 {
        self.last_modified_version
    }

    /// Adds a contributor unless already present.
    pub fn add_contributor(&mut self, participant: ParticipantId) {
        if !self.contributors.contains(&participant) {
            self.contributors.push(participant);
        }
    }

    pub fn remove_contributor(&mut self, participant: &ParticipantId) {
        self.contributors.retain(|p| p != participant);
    }

    /// Returns the previous value.
    pub fn set_last_modified_time(&mut self, new_time: i64) -> i64 {
        std::mem::replace(&mut self.last_modified_time, new_time)
    }

    /// Returns the previous value.
    pub fn set_last_modified_version(&mut self, new_version: u64) -> u64 {
        std::mem::replace(&mut self.last_modified_version, new_version)
    }
}

impl fmt::Display for BlipData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Blip state = [id:{}] [author: {}] [contributors: {}] [lastModifiedVersion:{}] \
             [lastModifiedTime:{}]",
            self.id,
            self.author,
            list(&self.contributors),
            self.last_modified_version,
            self.last_modified_time
        )
    }
}

fn list(participants: &[ParticipantId]) -> String {
    let addresses: Vec<&str> = participants.iter().map(ParticipantId::address).collect();
    format!("[{}]", addresses.join(", "))
}

/// The state of a wavelet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveletData {
    wave_id: WaveId,
    wavelet_id: WaveletId,
    creator: ParticipantId,
    creation_time: i64,
    version: u64,
    hashed_version: HashedVersion,
    last_modified_time: i64,
    /// Insertion-ordered set.
    participants: Vec<ParticipantId>,
    documents: BTreeMap<Utf16Key, BlipData>,
}

impl WaveletData {
    /// Creates a wavelet without participants or documents. `version` and
    /// `hashed_version` are stored independently, as in the original.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wavelet_id: WaveletId,
        creator: ParticipantId,
        creation_time: i64,
        version: u64,
        hashed_version: HashedVersion,
        last_modified_time: i64,
        wave_id: WaveId,
    ) -> Self {
        WaveletData {
            wave_id,
            wavelet_id,
            creator,
            creation_time,
            version,
            hashed_version,
            last_modified_time,
            participants: Vec::new(),
            documents: BTreeMap::new(),
        }
    }

    /// An empty wavelet at `version`, last modified at its creation time
    /// (`WaveletDataUtil.createEmptyWavelet`).
    pub fn empty(
        name: &WaveletName,
        creator: ParticipantId,
        version: HashedVersion,
        creation_time: i64,
    ) -> Self {
        Self::new(
            name.wavelet_id.clone(),
            creator,
            creation_time,
            version.version(),
            version,
            creation_time,
            name.wave_id.clone(),
        )
    }

    pub fn wave_id(&self) -> &WaveId {
        &self.wave_id
    }

    pub fn wavelet_id(&self) -> &WaveletId {
        &self.wavelet_id
    }

    pub fn wavelet_name(&self) -> WaveletName {
        WaveletName::of(self.wave_id.clone(), self.wavelet_id.clone())
    }

    pub fn creator(&self) -> &ParticipantId {
        &self.creator
    }

    /// Milliseconds since the epoch.
    pub fn creation_time(&self) -> i64 {
        self.creation_time
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn hashed_version(&self) -> &HashedVersion {
        &self.hashed_version
    }

    /// Milliseconds since the epoch.
    pub fn last_modified_time(&self) -> i64 {
        self.last_modified_time
    }

    /// Participants in the order they were added.
    pub fn participants(&self) -> &[ParticipantId] {
        &self.participants
    }

    /// Appends a participant. Returns false if already present.
    pub fn add_participant(&mut self, participant: ParticipantId) -> bool {
        if self.participants.contains(&participant) {
            return false;
        }
        self.participants.push(participant);
        true
    }

    /// Inserts a participant at `position`. Returns `Ok(false)` if already
    /// present; fails if `position` is beyond the end.
    pub fn add_participant_at(
        &mut self,
        participant: ParticipantId,
        position: usize,
    ) -> Result<bool, DataError> {
        if self.participants.contains(&participant) {
            return Ok(false);
        }
        if position > self.participants.len() {
            return Err(DataError::PositionOutOfBounds {
                position,
                size: self.participants.len(),
            });
        }
        self.participants.insert(position, participant);
        Ok(true)
    }

    /// Returns false if the participant was not present.
    pub fn remove_participant(&mut self, participant: &ParticipantId) -> bool {
        match self.participants.iter().position(|p| p == participant) {
            Some(index) => {
                self.participants.remove(index);
                true
            }
            None => false,
        }
    }

    /// Returns the previous value.
    pub fn set_version(&mut self, new_version: u64) -> u64 {
        std::mem::replace(&mut self.version, new_version)
    }

    /// Returns the previous value.
    pub fn set_hashed_version(&mut self, new_hashed_version: HashedVersion) -> HashedVersion {
        std::mem::replace(&mut self.hashed_version, new_hashed_version)
    }

    /// Returns the previous value.
    pub fn set_last_modified_time(&mut self, new_time: i64) -> i64 {
        std::mem::replace(&mut self.last_modified_time, new_time)
    }

    /// Creates a document. Fails if the id is taken. Duplicate contributors
    /// are dropped.
    pub fn create_document(
        &mut self,
        doc_id: &str,
        author: ParticipantId,
        contributors: impl IntoIterator<Item = ParticipantId>,
        content: &DocInitialization,
        last_modified_time: i64,
        last_modified_version: u64,
    ) -> Result<&mut BlipData, DataError> {
        let key = Utf16Key(doc_id.to_owned());
        if self.documents.contains_key(&key) {
            return Err(DataError::DuplicateDocument(doc_id.to_owned()));
        }
        let mut document = BootstrapDocument::new();
        document.consume(content.as_op())?;
        let mut blip = BlipData {
            id: doc_id.to_owned(),
            author,
            contributors: Vec::new(),
            content: document,
            last_modified_time,
            last_modified_version,
        };
        for contributor in contributors {
            blip.add_contributor(contributor);
        }
        Ok(self.documents.entry(key).or_insert(blip))
    }

    pub fn document(&self, doc_id: &str) -> Option<&BlipData> {
        self.documents.get(&Utf16Key(doc_id.to_owned()))
    }

    pub fn document_mut(&mut self, doc_id: &str) -> Option<&mut BlipData> {
        self.documents.get_mut(&Utf16Key(doc_id.to_owned()))
    }

    /// Document ids in UTF-16 order.
    pub fn document_ids(&self) -> impl Iterator<Item = &str> {
        self.documents.keys().map(|key| key.0.as_str())
    }

    /// Documents ordered by id.
    pub fn documents(&self) -> impl Iterator<Item = &BlipData> {
        self.documents.values()
    }

    /// Whether `user` may access this wavelet: an explicit participant, or
    /// anyone if the shared domain participant is one
    /// (`WaveletDataUtil.checkAccessPermission` for an existing wavelet).
    pub fn check_access_permission(
        &self,
        user: &ParticipantId,
        shared_domain_participant: Option<&ParticipantId>,
    ) -> bool {
        self.participants.contains(user)
            || shared_domain_participant.is_some_and(|p| self.participants.contains(p))
    }
}

impl fmt::Display for WaveletData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WaveletDataImpl: {}/{} \n[version:{}] \n[creator: {}] \n[participants: {}] \
             \n[creation time: {}] \n[lastModifiedTime:{}]\n[documents:",
            self.wave_id,
            self.wavelet_id,
            self.version,
            self.creator,
            list(&self.participants),
            self.creation_time,
            self.last_modified_time
        )?;
        for blip in self.documents.values() {
            write!(f, " \n  [{}: {}]", blip.id, blip)?;
        }
        f.write_str(" \n]")
    }
}
