//! "Continue as new wave": a participant of a wave whose host is frozen
//! (its person's devices are gone: the lease ran out with none of them
//! reachable, or the person was revoked) starts a wave of its own with a
//! snapshot of the conversation: the same documents (blips, threads,
//! text and annotations) of the root conversation at the newest version
//! known here, the same participants (the author first), and a
//! `continued-from` data document that points at the old wave
//! (`<link wave="…" wavelet="…" version="…"/>`). A continuation can be
//! continued again; each points at the one before.

use resonator_model::docop::{Attributes, DocInitialization, DocOpBuilder};
use resonator_model::id::constants::CONVERSATION_ROOT_WAVELET;
use resonator_model::id::{
    IdGenerator, IdGeneratorImpl, ParticipantId, WaveId, WaveletId, WaveletName,
};
use resonator_model::operation::{WaveletDelta, WaveletOperation, WaveletOperationContext};
use thiserror::Error;

use super::{SubmitError, WaveServer, WaveServerError};

/// The data document of a continuation that names the wave it continues.
pub const CONTINUED_FROM_DOC: &str = "continued-from";

/// Why a wave could not be continued.
#[derive(Debug, Error)]
pub enum ForkError {
    #[error("there is no such wave here")]
    NotFound,
    #[error("only participants can continue a wave")]
    NotParticipant,
    #[error(transparent)]
    WaveServer(#[from] WaveServerError),
    #[error(transparent)]
    Submit(Box<SubmitError>),
    #[error("{0}")]
    Internal(String),
}

impl From<SubmitError> for ForkError {
    fn from(error: SubmitError) -> Self {
        ForkError::Submit(Box::new(error))
    }
}

/// The content of a `continued-from` document.
pub fn continued_from_content(
    source: &WaveletName,
    version: u64,
) -> Result<DocInitialization, ForkError> {
    let attributes = Attributes::from_pairs([
        ("version", version.to_string().as_str()),
        ("wave", source.wave_id.to_string().as_str()),
        ("wavelet", source.wavelet_id.to_string().as_str()),
    ])
    .map_err(|e| ForkError::Internal(e.to_string()))?;
    DocOpBuilder::new()
        .element_start("link", attributes)
        .element_end()
        .build_initialization()
        .map_err(|e| ForkError::Internal(e.to_string()))
}

impl WaveServer {
    /// Starts a new wave of `author` continuing `source` (see the module
    /// docs); returns its id.
    pub async fn fork_wave(
        &self,
        source: &WaveId,
        author: &ParticipantId,
    ) -> Result<WaveId, ForkError> {
        let wavelet_id = WaveletId::of(source.domain(), CONVERSATION_ROOT_WAVELET)
            .map_err(|e| ForkError::Internal(e.to_string()))?;
        let source_name = WaveletName::of(source.clone(), wavelet_id);
        let snapshot = self
            .with_wavelet(&source_name, |data| {
                let documents: Vec<(String, DocInitialization)> = data
                    .documents()
                    .filter(|document| document.id() != CONTINUED_FROM_DOC)
                    .map(|document| (document.id().to_owned(), document.content().as_operation()))
                    .collect();
                (data.participants().to_vec(), documents, data.version())
            })
            .await?;
        let Some((participants, documents, version)) = snapshot else {
            return Err(ForkError::NotFound);
        };
        if !participants.contains(author) {
            return Err(ForkError::NotParticipant);
        }
        let mut seed = [0u8; 9];
        getrandom::fill(&mut seed).map_err(|e| ForkError::Internal(e.to_string()))?;
        let seed = {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed)
        };
        let mut ids = IdGeneratorImpl::new(self.domain(), move || seed.clone());
        let wave_id = ids
            .new_wave_id()
            .map_err(|e| ForkError::Internal(e.to_string()))?;
        let root = WaveletId::of(self.domain(), CONVERSATION_ROOT_WAVELET)
            .map_err(|e| ForkError::Internal(e.to_string()))?;
        let name = WaveletName::of(wave_id.clone(), root);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        let context = || WaveletOperationContext::new(author.clone(), now, 1);
        let mut ops = vec![WaveletOperation::add_participant(context(), author.clone())];
        for participant in participants.iter().filter(|p| *p != author) {
            ops.push(WaveletOperation::add_participant(
                context(),
                participant.clone(),
            ));
        }
        // The manifest first, so that clients see the blips as they come.
        let (manifest, blips): (Vec<_>, Vec<_>) = documents
            .into_iter()
            .partition(|(id, _)| id == "conversation");
        for (id, content) in manifest.into_iter().chain(blips) {
            if content.as_op().is_empty() {
                continue;
            }
            ops.push(WaveletOperation::blip_content(
                context(),
                id,
                content.as_op().clone(),
            ));
        }
        ops.push(WaveletOperation::blip_content(
            context(),
            CONTINUED_FROM_DOC,
            continued_from_content(&source_name, version)?
                .as_op()
                .clone(),
        ));
        let delta = WaveletDelta::new(author.clone(), self.version_zero(&name), ops);
        self.submit_delta(&name, &delta).await?;
        tracing::info!(source = %source, wave = %wave_id, "continued a wave as a new wave");
        Ok(wave_id)
    }
}
