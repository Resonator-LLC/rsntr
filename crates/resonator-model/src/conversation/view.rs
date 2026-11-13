//! The conversations of a wave (`WaveBasedConversationView`).

use super::manifest::AnchorData;
use super::model::{Conversation, conversation_id_for, wavelet_id_for};
use super::wavelet_based::{WaveletBasedConversation, make_wavelet_conversational};
use super::{BlipIdGenerator, ConversationError};
use crate::id::util::is_conversational_id;
use crate::id::{IdGenerator, WaveletId};
use crate::wavelet::WaveView;

/// The conversations of a wave: its conversational wavelets (`conv+...`)
/// that have a manifest.
///
/// Owns the wave view and the id generator used for new wavelets, blips
/// and threads. Reads give snapshots ([`Conversation`]); mutations go
/// through [`WaveletBasedConversation`] and are recorded in the wave's
/// wavelets, from which [`WaveView::take_deltas`] collects them.
pub struct WaveBasedConversationView<G> {
    wave: WaveView,
    id_generator: G,
}

impl<G: IdGenerator + BlipIdGenerator> WaveBasedConversationView<G> {
    pub fn new(wave: WaveView, id_generator: G) -> Self {
        WaveBasedConversationView { wave, id_generator }
    }

    /// The view's id: the serialised wave id
    /// (`WaveBasedConversationView.idFor`).
    pub fn id(&self) -> String {
        let wave_id = self.wave.wave_id();
        wave_id
            .serialise()
            .unwrap_or_else(|_| format!("{}/{}", wave_id.domain(), wave_id.id()))
    }

    pub fn wave(&self) -> &WaveView {
        &self.wave
    }

    pub fn wave_mut(&mut self) -> &mut WaveView {
        &mut self.wave
    }

    pub fn into_wave(self) -> WaveView {
        self.wave
    }

    pub fn id_generator(&mut self) -> &mut G {
        &mut self.id_generator
    }

    fn is_conversational(id: &WaveletId) -> bool {
        is_conversational_id(id).unwrap_or(false)
    }

    /// The conversations, ordered by wavelet id (the original's order is
    /// unspecified).
    pub fn conversations(&self) -> Vec<Conversation<'_>> {
        self.wave
            .wavelets()
            .filter(|wavelet| Self::is_conversational(wavelet.data().wavelet_id()))
            .filter_map(|wavelet| Conversation::read(wavelet.data()))
            .collect()
    }

    /// The conversation in a wavelet. Fails if the id is not that of a
    /// conversational wavelet; `None` if the wavelet is missing or has no
    /// manifest.
    pub fn conversation(
        &self,
        wavelet_id: &WaveletId,
    ) -> Result<Option<Conversation<'_>>, ConversationError> {
        if !Self::is_conversational(wavelet_id) {
            return Err(ConversationError::InvalidArgument(format!(
                "Wavelet id {wavelet_id} is not conversational"
            )));
        }
        Ok(self
            .wave
            .wavelet(wavelet_id)
            .and_then(|wavelet| Conversation::read(wavelet.data())))
    }

    /// The conversation with a conversation id (see
    /// [`conversation_id_for`]).
    pub fn conversation_by_id(
        &self,
        conversation_id: &str,
    ) -> Result<Option<Conversation<'_>>, ConversationError> {
        match wavelet_id_for(conversation_id) {
            Some(wavelet_id) => self.conversation(&wavelet_id),
            None => Err(ConversationError::InvalidArgument(format!(
                "Invalid conversation id {conversation_id}"
            ))),
        }
    }

    /// The conversation of the root wavelet, if any (`getRoot`).
    pub fn root(&self) -> Option<Conversation<'_>> {
        Conversation::read(self.wave.root()?.data())
    }

    /// The conversation in a wavelet, for mutation.
    pub fn conversation_mut(
        &mut self,
        wavelet_id: &WaveletId,
    ) -> Result<WaveletBasedConversation<'_>, ConversationError> {
        if !Self::is_conversational(wavelet_id) {
            return Err(ConversationError::InvalidArgument(format!(
                "Wavelet id {wavelet_id} is not conversational"
            )));
        }
        let wavelet = self
            .wave
            .wavelet_mut(wavelet_id)
            .ok_or(ConversationError::NoManifest)?;
        WaveletBasedConversation::new(wavelet, &mut self.id_generator)
    }

    /// The root conversation, for mutation.
    pub fn root_mut(&mut self) -> Result<WaveletBasedConversation<'_>, ConversationError> {
        let root_id = self.wave.root_id()?;
        self.conversation_mut(&root_id)
    }

    /// Creates the root wavelet with the viewer as participant and an
    /// empty conversation in it (`createRoot`).
    pub fn create_root(&mut self) -> Result<WaveletBasedConversation<'_>, ConversationError> {
        let wavelet = self.wave.create_root()?;
        make_wavelet_conversational(wavelet)?;
        WaveletBasedConversation::new(wavelet, &mut self.id_generator)
    }

    /// Creates a conversation in a new wavelet (`createConversation`).
    pub fn create_conversation(
        &mut self,
    ) -> Result<WaveletBasedConversation<'_>, ConversationError> {
        let wavelet = self
            .wave
            .create_conversation_wavelet(&mut self.id_generator)?;
        make_wavelet_conversational(wavelet)?;
        WaveletBasedConversation::new(wavelet, &mut self.id_generator)
    }

    /// The conversation and blip a conversation is anchored at, if its
    /// manifest names an existing blip of a conversation of this wave
    /// (`Conversation.getAnchor`). Returns the wavelet id of the anchor
    /// conversation and the blip id.
    pub fn anchor_of(&self, wavelet_id: &WaveletId) -> Option<(WaveletId, String)> {
        let conversation = self.conversation(wavelet_id).ok()??;
        let AnchorData {
            conversation_id: Some(conversation_id),
            blip_id: Some(blip_id),
        } = conversation.anchor().clone()
        else {
            return None;
        };
        let anchor_wavelet = wavelet_id_for(&conversation_id)?;
        let anchor_conversation = self.conversation(&anchor_wavelet).ok()??;
        anchor_conversation.blip(&blip_id)?;
        Some((anchor_wavelet, blip_id))
    }

    /// Anchors a conversation at a blip of another conversation of this
    /// wave, or removes its anchor (`Conversation.setAnchor`).
    pub fn set_anchor(
        &mut self,
        wavelet_id: &WaveletId,
        anchor: Option<(&WaveletId, &str)>,
    ) -> Result<(), ConversationError> {
        if let Some((anchor_wavelet, _)) = anchor
            && anchor_wavelet == wavelet_id
        {
            return Err(ConversationError::InvalidArgument(
                "A conversation cannot be anchored in itself".to_owned(),
            ));
        }
        let anchor = anchor.map(|(wavelet, blip)| (conversation_id_for(wavelet), blip));
        self.conversation_mut(wavelet_id)?
            .set_anchor(anchor.as_ref().map(|(c, b)| (c.as_str(), *b)))
    }
}
