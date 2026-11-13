//! The conversation model: the threads and blips of a wave's conversations,
//! read from and written to wavelets.
//!
//! A conversation lives in a conversational wavelet (`conv+root` or
//! `conv+<token>`). Its structure is in the manifest document
//! `conversation` (see [`manifest`]); each blip's content is a document of
//! its own, named by the blip id; a blip's inline replies are anchored by
//! `reply` elements in its content; tags are in the `tags` document; the
//! title is an annotation on the first blip of the root thread (see
//! [`title`]).
//!
//! * [`Conversation`] is a read-only snapshot over a
//!   [`WaveletData`](crate::data::WaveletData): root thread, blips, reply
//!   threads, participants, blip metadata, traversal, title, tags.
//! * [`WaveletBasedConversation`] mutates a conversation through an
//!   [`OpBasedWavelet`](crate::wavelet::OpBasedWavelet), which records the
//!   wavelet operations for submission.
//! * [`WaveBasedConversationView`] is the set of conversations of a wave
//!   and creates new ones.
//!
//! Derived from `org.waveprotocol.wave.model.conversation` of Apache Wave
//! (`WaveBasedConversationView`, `WaveletBasedConversation`,
//! `WaveletBasedConversationThread`, `WaveletBasedConversationBlip`,
//! `DocumentBasedManifest`, `DocumentBasedManifestThread`,
//! `DocumentBasedManifestBlip`, `AnchorData`, `BlipIterators`, `Blips`,
//! `TitleHelper`, `TagsDocument`, `WrapperIterator`, `AnnotationConstants`),
//! licensed under the
//! Apache License, Version 2.0.
//!
//! Differences from the original:
//!
//! * No object graph and no listeners: reads are snapshots, mutations name
//!   threads and blips by id. A deleted thread or blip is "not found"
//!   rather than an unusable object.
//! * Deleting a blip or thread walks children in manifest order where the
//!   original walks a hash map; the operations are the same, their order
//!   can differ when a blip has several reply threads or a thread several
//!   blips.
//! * Not ported: `ConversationCopier`, `AnchorManager`, `BlipMappers`,
//!   `ConversationStructure` (client-side helpers), and the `Observable*`
//!   interfaces.

pub mod annotations;
pub mod blips;
pub mod manifest;
mod model;
pub mod tags;
pub mod title;
mod view;
mod wavelet_based;

#[cfg(test)]
mod tests;

use thiserror::Error;

pub use manifest::{AnchorData, Manifest, ManifestBlip, ManifestThread};
pub use model::{
    Conversation, ConversationBlip, ConversationThread, LocatedReplyThread, conversation_id_for,
    wavelet_id_for,
};
pub use view::WaveBasedConversationView;
pub use wavelet_based::{
    WaveletBasedConversation, make_wavelet_conversational, read_manifest, wavelet_has_conversation,
};

use crate::doc::DocError;
use crate::id::{IdGenerator, IdGeneratorImpl, InvalidIdError};
use crate::wavelet::WaveletError;

/// Failure of a conversation operation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConversationError {
    /// The wavelet has no conversation manifest.
    #[error("the wavelet has no conversation manifest")]
    NoManifest,
    /// No such blip in the conversation (never there, or deleted).
    #[error("no blip {0} in the conversation")]
    NoSuchBlip(String),
    /// No such thread in the conversation (never there, or deleted).
    #[error("no thread {0} in the conversation")]
    NoSuchThread(String),
    /// The id generated for a new blip is already used by a document.
    #[error("a document {0} already exists")]
    BlipExists(String),
    #[error("{0}")]
    InvalidArgument(String),
    #[error(transparent)]
    Document(#[from] DocError),
    #[error(transparent)]
    Wavelet(#[from] WaveletError),
    #[error(transparent)]
    Id(#[from] InvalidIdError),
}

/// The source of ids for new blips and threads.
///
/// A new reply thread takes the id that the next new blip will get,
/// without consuming it (the original's `createThreadId` is
/// `IdGenerator.peekBlipId`), so `peek_blip_id` must return what the next
/// `new_blip_id` will return.
pub trait BlipIdGenerator {
    fn new_blip_id(&mut self) -> Result<String, InvalidIdError>;

    fn peek_blip_id(&mut self) -> Result<String, InvalidIdError>;
}

impl BlipIdGenerator for IdGeneratorImpl {
    fn new_blip_id(&mut self) -> Result<String, InvalidIdError> {
        IdGenerator::new_blip_id(self)
    }

    fn peek_blip_id(&mut self) -> Result<String, InvalidIdError> {
        Ok(IdGeneratorImpl::peek_blip_id(self))
    }
}
