//! A read-only snapshot of a conversation.

use std::collections::BTreeMap;

use super::blips::{THREAD_INLINE_ANCHOR_ID_ATTR, THREAD_INLINE_ANCHOR_TAGNAME};
use super::manifest::{AnchorData, Manifest, ManifestBlip, ManifestThread};
use super::{tags, title};
use crate::data::{BlipData, WaveletData};
use crate::doc::DocView;
use crate::docop::BootstrapDocument;
use crate::id::constants::{MANIFEST_DOCUMENT_ID, TAGS_DOC_ID};
use crate::id::{IdSerialiser, ModernIdSerialiser, ParticipantId, WaveletId};

/// A thread of a conversation: a sequence of blips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationThread {
    id: String,
    inline: bool,
    blips: Vec<ConversationBlip>,
    pub(crate) manifest_location: usize,
}

impl ConversationThread {
    /// The thread's id; the empty string for the root thread.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the manifest marks the thread as inline. Where a thread is
    /// shown is decided by its anchor in the parent blip, see
    /// [`Conversation::locate_reply_threads`]; this flag only records how
    /// the thread was created.
    pub fn is_inline(&self) -> bool {
        self.inline
    }

    /// The blips of the thread that have a document, in manifest order.
    pub fn blips(&self) -> &[ConversationBlip] {
        &self.blips
    }

    pub fn first_blip(&self) -> Option<&ConversationBlip> {
        self.blips.first()
    }
}

/// A blip of a conversation: a manifest entry whose document exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationBlip {
    id: String,
    reply_threads: Vec<ConversationThread>,
    pub(crate) manifest_location: usize,
}

impl ConversationBlip {
    /// The id of the blip, which is also the id of its document.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The reply threads, inline or not, in manifest order.
    pub fn reply_threads(&self) -> &[ConversationThread] {
        &self.reply_threads
    }

    pub fn reply_thread(&self, id: &str) -> Option<&ConversationThread> {
        self.reply_threads.iter().rev().find(|t| t.id == id)
    }
}

/// The structure of a conversation as of the moment it was read, with
/// access to the wavelet it lives in (`WaveletBasedConversation` and its
/// threads and blips, read-only).
///
/// Degraded but safe on malformed manifests, like the original: manifest
/// blips without an id or without a document in the wavelet are left out
/// (with their replies); reply threads without an id are left out too
/// (the original would index them under a null key).
#[derive(Debug, Clone)]
pub struct Conversation<'a> {
    wavelet: &'a WaveletData,
    anchor: AnchorData,
    root_thread: ConversationThread,
}

fn adapt_thread(wavelet: &WaveletData, thread: &ManifestThread, id: String) -> ConversationThread {
    ConversationThread {
        id,
        inline: thread.inline,
        blips: thread
            .blips
            .iter()
            .filter_map(|blip| adapt_blip(wavelet, blip))
            .collect(),
        manifest_location: thread.location,
    }
}

fn adapt_blip(wavelet: &WaveletData, blip: &ManifestBlip) -> Option<ConversationBlip> {
    let id = blip.id.as_ref()?;
    wavelet.document(id)?;
    Some(ConversationBlip {
        id: id.clone(),
        reply_threads: blip
            .threads
            .iter()
            .filter_map(|thread| {
                let id = thread.id.clone()?;
                Some(adapt_thread(wavelet, thread, id))
            })
            .collect(),
        manifest_location: blip.location,
    })
}

/// A reply thread with the location of its anchor in the parent blip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocatedReplyThread<'c> {
    pub thread: &'c ConversationThread,
    /// `None` if the blip has no anchor for the thread, as for non-inline
    /// replies (`Blips.INVALID_INLINE_LOCATION`).
    pub location: Option<usize>,
}

impl<'a> Conversation<'a> {
    /// Reads the conversation of a wavelet, or `None` if the wavelet has no
    /// conversation manifest.
    pub fn read(wavelet: &'a WaveletData) -> Option<Conversation<'a>> {
        let manifest = Manifest::read(wavelet.document(MANIFEST_DOCUMENT_ID)?.content())?;
        Some(Self::from_manifest(wavelet, &manifest))
    }

    pub(crate) fn from_manifest(wavelet: &'a WaveletData, manifest: &Manifest) -> Self {
        Conversation {
            wavelet,
            anchor: manifest.anchor.clone(),
            root_thread: adapt_thread(wavelet, &manifest.root_thread, String::new()),
        }
    }

    /// The conversation's id: the serialised id of its wavelet
    /// (`WaveletBasedConversation.idFor`).
    pub fn id(&self) -> String {
        conversation_id_for(self.wavelet.wavelet_id())
    }

    pub fn wavelet_id(&self) -> &WaveletId {
        self.wavelet.wavelet_id()
    }

    pub fn wavelet(&self) -> &'a WaveletData {
        self.wavelet
    }

    pub fn participants(&self) -> &'a [ParticipantId] {
        self.wavelet.participants()
    }

    /// The raw anchor of the manifest. Whether it refers to an existing
    /// blip of another conversation is for the conversation view to say.
    pub fn anchor(&self) -> &AnchorData {
        &self.anchor
    }

    pub fn root_thread(&self) -> &ConversationThread {
        &self.root_thread
    }

    /// The blips in the order of the original's object construction:
    /// depth first, a blip before its replies.
    fn walk(&self) -> Vec<(&ConversationThread, &ConversationBlip)> {
        fn visit<'c>(
            thread: &'c ConversationThread,
            out: &mut Vec<(&'c ConversationThread, &'c ConversationBlip)>,
        ) {
            for blip in &thread.blips {
                out.push((thread, blip));
                for reply in &blip.reply_threads {
                    visit(reply, out);
                }
            }
        }
        let mut out = Vec::new();
        visit(&self.root_thread, &mut out);
        out
    }

    /// The blip with an id. If the manifest lists the id several times,
    /// the last entry wins, as in the original.
    pub fn blip(&self, id: &str) -> Option<&ConversationBlip> {
        self.walk()
            .into_iter()
            .rev()
            .find(|(_, blip)| blip.id == id)
            .map(|(_, blip)| blip)
    }

    /// The thread a blip is in.
    pub fn thread_of(&self, blip_id: &str) -> Option<&ConversationThread> {
        self.walk()
            .into_iter()
            .rev()
            .find(|(_, blip)| blip.id == blip_id)
            .map(|(thread, _)| thread)
    }

    /// The thread with an id; the root thread for the empty string.
    pub fn thread(&self, id: &str) -> Option<&ConversationThread> {
        if id.is_empty() {
            return Some(&self.root_thread);
        }
        self.walk()
            .into_iter()
            .rev()
            .find_map(|(_, blip)| blip.reply_thread(id))
    }

    /// The blip a reply thread hangs off; `None` for the root thread.
    pub fn parent_blip_of(&self, thread_id: &str) -> Option<&ConversationBlip> {
        if thread_id.is_empty() {
            return None;
        }
        self.walk()
            .into_iter()
            .rev()
            .map(|(_, blip)| blip)
            .find(|blip| blip.reply_thread(thread_id).is_some())
    }

    /// Whether a blip is the first blip of the root thread
    /// (`ConversationBlip.isRoot`).
    pub fn is_root_blip(&self, blip_id: &str) -> bool {
        self.root_thread
            .first_blip()
            .is_some_and(|blip| blip.id == blip_id)
    }

    /// All blips, breadth first: the blips of a thread in order, then the
    /// blips of their reply threads, thread by thread
    /// (`BlipIterators.breadthFirst`).
    pub fn blips_breadth_first(&self) -> Vec<&ConversationBlip> {
        let mut result = Vec::new();
        let mut threads = std::collections::VecDeque::from([&self.root_thread]);
        while let Some(thread) = threads.pop_front() {
            for blip in &thread.blips {
                threads.extend(blip.reply_threads.iter());
                result.push(blip);
            }
        }
        result
    }

    /// The document and metadata of a blip: author, contributors,
    /// last-modified time and version, content.
    pub fn blip_data(&self, blip_id: &str) -> Option<&'a BlipData> {
        self.wavelet.document(blip_id)
    }

    /// The content of a blip.
    pub fn blip_content(&self, blip_id: &str) -> Option<&'a BootstrapDocument> {
        Some(self.blip_data(blip_id)?.content())
    }

    /// The reply threads of a blip with the locations of their anchors in
    /// the blip's document, ordered by location, those without anchor last
    /// (`locateReplyThreads`). The anchor of a thread is the first `reply`
    /// element with the thread's id.
    pub fn locate_reply_threads<'c>(
        &self,
        blip: &'c ConversationBlip,
    ) -> Vec<LocatedReplyThread<'c>> {
        let anchors = self
            .blip_content(&blip.id)
            .map(find_anchors)
            .unwrap_or_default();
        let mut located: Vec<LocatedReplyThread<'c>> = blip
            .reply_threads
            .iter()
            .map(|thread| LocatedReplyThread {
                thread,
                location: anchors.get(thread.id.as_str()).copied(),
            })
            .collect();
        // Stable, like Collections.sort.
        located.sort_by_key(|reply| reply.location.unwrap_or(usize::MAX));
        located
    }

    /// The title of the conversation: that of the first blip of the root
    /// thread, or the empty string (`TitleHelper.getTitle`).
    pub fn title(&self) -> String {
        self.root_thread
            .first_blip()
            .and_then(|blip| self.blip_content(&blip.id))
            .map(title::extract_title)
            .unwrap_or_default()
    }

    /// The tags of the conversation.
    pub fn tags(&self) -> Vec<String> {
        self.wavelet
            .document(TAGS_DOC_ID)
            .map(|doc| tags::tags(doc.content()))
            .unwrap_or_default()
    }
}

/// The id of the conversation in a wavelet: the wavelet id in the modern
/// serialisation, `domain/id` (`WaveletBasedConversation.idFor`).
pub fn conversation_id_for(wavelet_id: &WaveletId) -> String {
    ModernIdSerialiser
        .serialise_wavelet_id(wavelet_id)
        .unwrap_or_else(|_| format!("{}/{}", wavelet_id.domain(), wavelet_id.id()))
}

/// The wavelet of a conversation id (`WaveletBasedConversation.widFor`).
pub fn wavelet_id_for(conversation_id: &str) -> Option<WaveletId> {
    ModernIdSerialiser
        .deserialise_wavelet_id(conversation_id)
        .ok()
}

/// The location of the first anchor of each thread id in a blip document.
pub(crate) fn find_anchors(doc: &BootstrapDocument) -> BTreeMap<String, usize> {
    let view = DocView::new(doc);
    let mut anchors = BTreeMap::new();
    for element in view.descendant_elements(view.root()) {
        if view.has_tag(element, THREAD_INLINE_ANCHOR_TAGNAME)
            && let Some(thread_id) = view.attribute(element, THREAD_INLINE_ANCHOR_ID_ATTR)
        {
            anchors
                .entry(thread_id.to_owned())
                .or_insert_with(|| view.location(element));
        }
    }
    anchors
}
