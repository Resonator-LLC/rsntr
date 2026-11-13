//! Mutation of a conversation through wavelet operations
//! (`WaveletBasedConversation`, `WaveletBasedConversationThread`,
//! `WaveletBasedConversationBlip`).

use super::blips::{self, THREAD_INLINE_ANCHOR_ID_ATTR, THREAD_INLINE_ANCHOR_TAGNAME};
use super::manifest::{
    self, ANCHOR_BLIP_ATTR, ANCHOR_WAVELET_ATTR, BLIP_ID_ATTR, BLIP_TAG, INLINE_ATTR, Manifest,
    THREAD_ID_ATTR, THREAD_TAG,
};
use super::model::{Conversation, ConversationBlip, ConversationThread};
use super::{BlipIdGenerator, ConversationError, tags, title};
use crate::data::WaveletData;
use crate::doc::{DocError, MutableDocument, Nindo};
use crate::docop::DocInitialization;
use crate::id::ParticipantId;
use crate::id::constants::{MANIFEST_DOCUMENT_ID, TAGS_DOC_ID};
use crate::id::util::is_blip_id;
use crate::wavelet::OpBasedWavelet;

/// Whether a wavelet has a conversation manifest as the first element of
/// its manifest document (`WaveletBasedConversation.waveletHasConversation`).
pub fn wavelet_has_conversation(wavelet: &WaveletData) -> bool {
    wavelet
        .document(MANIFEST_DOCUMENT_ID)
        .is_some_and(|doc| manifest::document_has_manifest(doc.content()))
}

/// Gives a wavelet an empty conversation manifest
/// (`WaveletBasedConversation.makeWaveletConversational`). Fails if its
/// manifest document has any content.
pub fn make_wavelet_conversational(wavelet: &mut OpBasedWavelet) -> Result<(), ConversationError> {
    wavelet.edit_document(MANIFEST_DOCUMENT_ID, manifest::initialise_document_manifest)?;
    Ok(())
}

/// A conversation in a wavelet, for mutation. Every mutation is recorded
/// in the wavelet as the operations the original model emits for it.
///
/// Threads and blips are named by id; the root thread's id is the empty
/// string. The structure is read afresh from the manifest for every call
/// ([`WaveletBasedConversation::read`] gives the current snapshot), so
/// there are no stale handles: a deleted blip or thread is simply not
/// found.
///
/// Where the original iterates the reply threads of a blip or the blips of
/// a thread in hash-map order while deleting them, this goes in manifest
/// order.
pub struct WaveletBasedConversation<'a> {
    wavelet: &'a mut OpBasedWavelet,
    id_generator: &'a mut dyn BlipIdGenerator,
}

impl<'a> WaveletBasedConversation<'a> {
    /// The conversation of a wavelet. Fails if the wavelet has no
    /// manifest.
    pub fn new(
        wavelet: &'a mut OpBasedWavelet,
        id_generator: &'a mut dyn BlipIdGenerator,
    ) -> Result<Self, ConversationError> {
        if Conversation::read(wavelet.data()).is_none() {
            return Err(ConversationError::NoManifest);
        }
        Ok(WaveletBasedConversation {
            wavelet,
            id_generator,
        })
    }

    pub fn wavelet(&self) -> &OpBasedWavelet {
        self.wavelet
    }

    pub fn wavelet_mut(&mut self) -> &mut OpBasedWavelet {
        self.wavelet
    }

    /// The current structure of the conversation.
    pub fn read(&self) -> Result<Conversation<'_>, ConversationError> {
        Conversation::read(self.wavelet.data()).ok_or(ConversationError::NoManifest)
    }

    fn thread(&self, thread_id: &str) -> Result<ConversationThread, ConversationError> {
        self.read()?
            .thread(thread_id)
            .cloned()
            .ok_or_else(|| ConversationError::NoSuchThread(thread_id.to_owned()))
    }

    fn blip(&self, blip_id: &str) -> Result<ConversationBlip, ConversationError> {
        self.read()?
            .blip(blip_id)
            .cloned()
            .ok_or_else(|| ConversationError::NoSuchBlip(blip_id.to_owned()))
    }

    fn edit_manifest<R>(
        &mut self,
        edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<R, DocError>,
    ) -> Result<R, ConversationError> {
        Ok(self.wavelet.edit_document(MANIFEST_DOCUMENT_ID, edit)?)
    }

    /// Edits the content of a blip.
    pub fn edit_blip<R>(
        &mut self,
        blip_id: &str,
        edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<R, DocError>,
    ) -> Result<R, ConversationError> {
        self.blip(blip_id)?;
        Ok(self.wavelet.edit_document(blip_id, edit)?)
    }

    /// Edits a data document of the conversation's wavelet
    /// (`getDataDocument`): any document but blips and the manifest.
    pub fn edit_data_document<R>(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<R, DocError>,
    ) -> Result<R, ConversationError> {
        if is_blip_id(name) {
            return Err(ConversationError::InvalidArgument(format!(
                "Cannot fetch blip document {name} as a data document"
            )));
        }
        if name == MANIFEST_DOCUMENT_ID {
            return Err(ConversationError::InvalidArgument(
                "Cannot fetch conversation manifest as a data document".to_owned(),
            ));
        }
        Ok(self.wavelet.edit_document(name, edit)?)
    }

    /// Creates the document of a new blip (`ComponentHelper.createBlip`):
    /// with `content`, or else with an empty body.
    fn create_blip(
        &mut self,
        content: Option<&DocInitialization>,
    ) -> Result<String, ConversationError> {
        let blip_id = self.id_generator.new_blip_id()?;
        if self.wavelet.has_document(&blip_id) {
            return Err(ConversationError::BlipExists(blip_id));
        }
        self.wavelet.edit_document(&blip_id, |doc| match content {
            Some(content) => doc.consume(&Nindo::from_doc_op(content.as_op(), false)),
            None => doc
                .insert_xml(doc.size(), &blips::initial_content())
                .map(drop),
        })?;
        Ok(blip_id)
    }

    /// Appends a new blip with an empty body to a thread and returns its id
    /// (`ConversationThread.appendBlip`).
    pub fn append_blip(&mut self, thread_id: &str) -> Result<String, ConversationError> {
        self.append_blip_with_content(thread_id, None)
    }

    /// Appends a new blip with the given content to a thread
    /// (`appendBlip(DocInitialization)`).
    pub fn append_blip_with_content(
        &mut self,
        thread_id: &str,
        content: Option<&DocInitialization>,
    ) -> Result<String, ConversationError> {
        let thread = self.thread(thread_id)?;
        let blip_id = self.create_blip(content)?;
        self.edit_manifest(|doc| {
            let element = doc
                .view()
                .element_starting_at(thread.manifest_location)
                .ok_or(DocError::InvalidNode)?;
            doc.create_child_element(element, BLIP_TAG, &[(BLIP_ID_ATTR, &blip_id)])
                .map(drop)
        })?;
        Ok(blip_id)
    }

    /// Inserts a new blip into a thread before the blip `successor_id` of
    /// that thread and returns its id (`ConversationThread.insertBlip`).
    pub fn insert_blip(
        &mut self,
        thread_id: &str,
        successor_id: &str,
    ) -> Result<String, ConversationError> {
        let thread = self.thread(thread_id)?;
        let Some(successor) = thread.blips().iter().rev().find(|b| b.id() == successor_id) else {
            return Err(ConversationError::InvalidArgument(format!(
                "Can't insert blip before blip {successor_id} not from this thread"
            )));
        };
        let location = successor.manifest_location;
        let blip_id = self.create_blip(None)?;
        self.edit_manifest(|doc| {
            doc.create_element(location, BLIP_TAG, &[(BLIP_ID_ATTR, &blip_id)])
                .map(drop)
        })?;
        Ok(blip_id)
    }

    fn append_reply(
        &mut self,
        blip: &ConversationBlip,
        thread_id: &str,
        inline: bool,
    ) -> Result<(), ConversationError> {
        let location = blip.manifest_location;
        self.edit_manifest(|doc| {
            let element = doc
                .view()
                .element_starting_at(location)
                .ok_or(DocError::InvalidNode)?;
            let mut attributes = vec![(THREAD_ID_ATTR, thread_id)];
            if inline {
                attributes.push((INLINE_ATTR, "true"));
            }
            doc.create_child_element(element, THREAD_TAG, &attributes)
                .map(drop)
        })
    }

    /// Adds an empty, non-inline reply thread to a blip and returns its id
    /// (`ConversationBlip.addReplyThread()`).
    ///
    /// The thread's id is the id the generator will give out next, without
    /// consuming it (`peekBlipId`), so the first blip appended to the
    /// thread normally gets the same id as the thread.
    pub fn add_reply_thread(&mut self, blip_id: &str) -> Result<String, ConversationError> {
        let blip = self.blip(blip_id)?;
        let thread_id = self.id_generator.peek_blip_id()?;
        self.append_reply(&blip, &thread_id, false)?;
        Ok(thread_id)
    }

    /// Adds an empty inline reply thread anchored at a location of the
    /// blip's document and returns its id
    /// (`ConversationBlip.addReplyThread(int)`): first a `reply` anchor
    /// element is inserted into the blip, then the thread is added to the
    /// manifest.
    pub fn add_inline_reply_thread(
        &mut self,
        blip_id: &str,
        location: usize,
    ) -> Result<String, ConversationError> {
        let blip = self.blip(blip_id)?;
        let thread_id = self.id_generator.peek_blip_id()?;
        self.wavelet.edit_document(blip_id, |doc| {
            doc.create_element(
                location,
                THREAD_INLINE_ANCHOR_TAGNAME,
                &[(THREAD_INLINE_ANCHOR_ID_ATTR, &thread_id)],
            )
            .map(drop)
        })?;
        self.append_reply(&blip, &thread_id, true)?;
        Ok(thread_id)
    }

    /// Deletes anchors from a blip's document, last first: those of one
    /// thread, or all (`clearInlineReplyAnchors`,
    /// `clearAllInlineReplyAnchors`).
    fn clear_inline_reply_anchors(
        &mut self,
        blip_id: &str,
        thread_id: Option<&str>,
    ) -> Result<(), ConversationError> {
        let view = self.wavelet.document_view(blip_id);
        let mut locations: Vec<usize> = view
            .descendant_elements(view.root())
            .into_iter()
            .filter(|&el| {
                view.has_tag(el, THREAD_INLINE_ANCHOR_TAGNAME)
                    && thread_id.is_none_or(|id| {
                        view.attribute(el, THREAD_INLINE_ANCHOR_ID_ATTR) == Some(id)
                    })
            })
            .map(|el| view.location(el))
            .collect();
        if locations.is_empty() {
            return Ok(());
        }
        locations.reverse();
        self.wavelet.edit_document(blip_id, |doc| {
            for location in locations {
                // Deleting from the back leaves earlier locations valid.
                let element = doc
                    .view()
                    .element_starting_at(location)
                    .ok_or(DocError::InvalidNode)?;
                doc.delete_node(element)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    fn delete_manifest_element(&mut self, location: usize) -> Result<(), ConversationError> {
        self.edit_manifest(|doc| {
            let element = doc
                .view()
                .element_starting_at(location)
                .ok_or(DocError::InvalidNode)?;
            doc.delete_node(element)
        })
    }

    /// The current state of the thread whose manifest element is at a
    /// location. Deleting things inside an element does not move it.
    fn thread_at(&self, location: usize) -> Result<ConversationThread, ConversationError> {
        fn find(thread: &ConversationThread, location: usize) -> Option<&ConversationThread> {
            if thread.manifest_location == location {
                return Some(thread);
            }
            thread
                .blips()
                .iter()
                .flat_map(|blip| blip.reply_threads())
                .find_map(|reply| find(reply, location))
        }
        let conversation = self.read()?;
        find(conversation.root_thread(), location)
            .cloned()
            .ok_or_else(|| ConversationError::NoSuchThread(format!("at {location}")))
    }

    fn blip_at(&self, location: usize) -> Result<ConversationBlip, ConversationError> {
        fn find(thread: &ConversationThread, location: usize) -> Option<&ConversationBlip> {
            thread.blips().iter().find_map(|blip| {
                if blip.manifest_location == location {
                    return Some(blip);
                }
                blip.reply_threads()
                    .iter()
                    .find_map(|reply| find(reply, location))
            })
        }
        let conversation = self.read()?;
        find(conversation.root_thread(), location)
            .cloned()
            .ok_or_else(|| ConversationError::NoSuchBlip(format!("at {location}")))
    }

    /// `WaveletBasedConversationThread.deleteBlips`: deletes every blip of
    /// the thread, keeping the thread.
    fn delete_blips_of_thread(&mut self, thread_location: usize) -> Result<(), ConversationError> {
        // Each deletion removes the first remaining blip.
        while let Some(blip) = self.thread_at(thread_location)?.first_blip().cloned() {
            self.delete_blip_from_thread(&blip)?;
        }
        Ok(())
    }

    /// `WaveletBasedConversationThread.deleteBlip` without deleting the
    /// thread: deletes the blip's reply threads, all anchors left in it,
    /// its manifest entry and its content.
    fn delete_blip_from_thread(
        &mut self,
        blip: &ConversationBlip,
    ) -> Result<(), ConversationError> {
        let location = blip.manifest_location;
        // deleteThreads: unlike deleteThread, anchors are cleared once at
        // the end, all of them.
        while let Some(reply) = self.blip_at(location)?.reply_threads().first().cloned() {
            self.delete_blips_of_thread(reply.manifest_location)?;
            self.delete_manifest_element(reply.manifest_location)?;
        }
        self.clear_inline_reply_anchors(blip.id(), None)?;
        self.delete_manifest_element(location)?;
        // clearContent
        if self.wavelet.document_view(blip.id()).size() != 0 {
            self.wavelet
                .edit_document(blip.id(), |doc| doc.empty_element(doc.view().root()))?;
        }
        Ok(())
    }

    /// `WaveletBasedConversationBlip.deleteThread`: deletes the thread's
    /// blips, its manifest entry and its anchors in the parent blip.
    fn delete_reply_thread(
        &mut self,
        parent_blip_id: &str,
        thread: &ConversationThread,
    ) -> Result<(), ConversationError> {
        self.delete_blips_of_thread(thread.manifest_location)?;
        self.delete_manifest_element(thread.manifest_location)?;
        self.clear_inline_reply_anchors(parent_blip_id, Some(thread.id()))
    }

    /// Deletes a blip with all its replies (`ConversationBlip.delete`):
    /// its reply threads one by one, then its manifest entry, then its
    /// content (the document stays, empty). If that leaves a non-root
    /// thread without blips, the thread is deleted too.
    pub fn delete_blip(&mut self, blip_id: &str) -> Result<(), ConversationError> {
        let conversation = self.read()?;
        let blip = conversation
            .blip(blip_id)
            .cloned()
            .ok_or_else(|| ConversationError::NoSuchBlip(blip_id.to_owned()))?;
        let thread = conversation
            .thread_of(blip_id)
            .cloned()
            .ok_or_else(|| ConversationError::NoSuchBlip(blip_id.to_owned()))?;
        let parent_blip_id = conversation
            .parent_blip_of(thread.id())
            .map(|parent| parent.id().to_owned());
        let location = blip.manifest_location;
        while let Some(reply) = self.blip_at(location)?.reply_threads().first().cloned() {
            self.delete_reply_thread(blip_id, &reply)?;
        }
        self.delete_blip_from_thread(&blip)?;
        // deleteSelfIfEmpty
        if let Some(parent_blip_id) = parent_blip_id {
            let thread = self.thread_at(thread.manifest_location)?;
            if thread.blips().is_empty() {
                self.delete_reply_thread(&parent_blip_id, &thread)?;
            }
        }
        Ok(())
    }

    /// Deletes a thread with all its blips (`ConversationThread.delete`).
    /// The root thread itself stays; only its blips go.
    pub fn delete_thread(&mut self, thread_id: &str) -> Result<(), ConversationError> {
        let conversation = self.read()?;
        let thread = conversation
            .thread(thread_id)
            .cloned()
            .ok_or_else(|| ConversationError::NoSuchThread(thread_id.to_owned()))?;
        match conversation
            .parent_blip_of(thread_id)
            .map(|b| b.id().to_owned())
        {
            None => self.delete_blips_of_thread(thread.manifest_location),
            Some(parent_blip_id) => self.delete_reply_thread(&parent_blip_id, &thread),
        }
    }

    /// Deletes the conversation: all its blips and then the manifest
    /// (`Conversation.delete`).
    pub fn delete(mut self) -> Result<(), ConversationError> {
        let root_location = self.read()?.root_thread().manifest_location;
        self.delete_blips_of_thread(root_location)?;
        self.edit_manifest(manifest::delete_manifest)
    }

    /// Sets or clears the anchor: the id of another conversation and of a
    /// blip in it (`setAnchor`). Both manifest attributes are written in
    /// one operation, whatever they were.
    pub fn set_anchor(&mut self, anchor: Option<(&str, &str)>) -> Result<(), ConversationError> {
        self.edit_manifest(|doc| {
            let top = manifest::manifest_element(doc.view()).ok_or(DocError::InvalidNode)?;
            doc.update_element_attributes(
                top,
                &[
                    (
                        ANCHOR_WAVELET_ATTR,
                        anchor.map(|(conversation, _)| conversation),
                    ),
                    (ANCHOR_BLIP_ATTR, anchor.map(|(_, blip)| blip)),
                ],
            )
        })
    }

    pub fn add_participant(
        &mut self,
        participant: &ParticipantId,
    ) -> Result<(), ConversationError> {
        Ok(self.wavelet.add_participant(participant)?)
    }

    pub fn remove_participant(
        &mut self,
        participant: &ParticipantId,
    ) -> Result<(), ConversationError> {
        Ok(self.wavelet.remove_participant(participant)?)
    }

    /// The first blip of the root thread, which carries the title.
    fn root_blip_id(&self) -> Result<String, ConversationError> {
        self.read()?
            .root_thread()
            .first_blip()
            .map(|blip| blip.id().to_owned())
            .ok_or_else(|| ConversationError::NoSuchBlip("root blip".to_owned()))
    }

    /// Sets an explicit title on the root blip
    /// (`TitleHelper.setExplicitTitle`).
    pub fn set_explicit_title(&mut self, new_title: &str) -> Result<(), ConversationError> {
        let blip_id = self.root_blip_id()?;
        Ok(self
            .wavelet
            .edit_document(&blip_id, |doc| title::set_explicit_title(doc, new_title))?)
    }

    /// Marks the first line or sentence of the root blip as the title,
    /// unless it has an explicit one
    /// (`TitleHelper.maybeFindAndSetImplicitTitle`).
    pub fn update_implicit_title(&mut self) -> Result<(), ConversationError> {
        let blip_id = self.root_blip_id()?;
        Ok(self
            .wavelet
            .edit_document(&blip_id, title::maybe_find_and_set_implicit_title)?)
    }

    /// Appends a tag to the conversation's tags document.
    pub fn add_tag(&mut self, tag: &str) -> Result<(), ConversationError> {
        Ok(self
            .wavelet
            .edit_document(TAGS_DOC_ID, |doc| tags::add_tag(doc, tag))?)
    }

    /// Removes the first tag with a name. Returns whether there was one.
    pub fn remove_tag(&mut self, tag: &str) -> Result<bool, ConversationError> {
        Ok(self
            .wavelet
            .edit_document(TAGS_DOC_ID, |doc| tags::delete_tag(doc, tag))?)
    }
}

/// The manifest of a wavelet, if it has one.
pub fn read_manifest(wavelet: &WaveletData) -> Option<Manifest> {
    Manifest::read(wavelet.document(MANIFEST_DOCUMENT_ID)?.content())
}
