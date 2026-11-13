//! Supplement state kept in the documents of a user-data wavelet
//! (`WaveletBasedSupplement`).

use std::collections::BTreeMap;

use super::{
    NO_VERSION, PrimitiveSupplement, ReadablePrimitiveSupplement, SupplementError, ThreadState,
    WantedEvaluation,
};
use crate::adt::{
    AdtError, DocumentBasedBasicMap, DocumentBasedBasicSet, DocumentBasedBoolean,
    DocumentBasedElementList, DocumentBasedMonotonicMap, DocumentBasedMonotonicValue,
    InitialAttributes, Locator, Root, Serializable,
};
use crate::data::WaveletData;
use crate::doc::{DocView, MutableDocument, NodeId};
use crate::docop::BootstrapDocument;
use crate::id::WaveletId;
use crate::version::HashedVersion;
use crate::wavelet::OpBasedWavelet;

/// Document of read versions.
pub const READSTATE_DOCUMENT: &str = "m/read";
/// Document of thread states.
pub const PRESENTATION_DOCUMENT: &str = "m/presentation";
/// Document of folders.
pub const FOLDERS_DOCUMENT: &str = "m/folder";
/// Document of archive versions.
pub const ARCHIVING_DOCUMENT: &str = "m/archiving";
/// Document of the muted flag.
pub const MUTED_DOCUMENT: &str = "m/muted";
/// Document of the flag that overrides the archive versions.
pub const CLEARED_DOCUMENT: &str = "m/cleared";
/// Document of wanted evaluations.
pub const ABUSE_DOCUMENT: &str = "m/abuse";
/// Document of seen versions, notified versions and the pending
/// notification flag.
pub const SEEN_DOCUMENT: &str = "m/seen";
/// Document of private gadget states.
pub const GADGETS_DOCUMENT: &str = "m/gadgets";

pub(crate) const SEEN_VERSION_TAG: &str = "seen";
pub(crate) const NOTIFIED_VERSION_TAG: &str = "notified";
pub(crate) const WAVELET_TAG: &str = "wavelet";
pub(crate) const BLIP_READ_TAG: &str = "blip";
pub(crate) const PARTICIPANTS_READ_TAG: &str = "participants";
pub(crate) const TAGS_READ_TAG: &str = "tags";
pub(crate) const WAVELET_READ_TAG: &str = "all";
pub(crate) const CONVERSATION_TAG: &str = "conversation";
pub(crate) const THREAD_TAG: &str = "thread";
pub(crate) const ARCHIVE_TAG: &str = "archive";
pub(crate) const VERSION_ATTR: &str = "v";
pub(crate) const ID_ATTR: &str = "i";
pub(crate) const FOLDER_TAG: &str = "folder";
pub(crate) const MUTED_TAG: &str = "muted";
pub(crate) const MUTED_ATTR: &str = "muted";
pub(crate) const CLEARED_TAG: &str = "cleared";
pub(crate) const CLEARED_ATTR: &str = "cleared";
pub(crate) const SIGNATURE_ATTR: &str = "signature";
pub(crate) const STATE_ATTR: &str = "state";
pub(crate) const NOTIFICATION_TAG: &str = "notification";
pub(crate) const PENDING_NOTIFICATION_ATTR: &str = "pending";
pub(crate) const GADGET_TAG: &str = "gadget";
/// Permitted by the schema but never read or written.
pub(crate) const PERMISSIONS_ATTR: &str = "p";
pub(crate) const STATE_TAG: &str = "state";
pub(crate) const NAME_ATTR: &str = "name";
pub(crate) const VALUE_ATTR: &str = "value";

pub(crate) const WANTED_EVAL_TAG: &str = "wanted";
pub(crate) const AGENT_ATTR: &str = "agent";
pub(crate) const CERTAINTY_ATTR: &str = "certainty";
pub(crate) const COMMENT_ATTR: &str = "comment";
pub(crate) const TIMESTAMP_ATTR: &str = "timestamp";
pub(crate) const WANTED_ATTR: &str = "wanted";
pub(crate) const WAVELET_ID_ATTR: &str = "wavelet_id";
pub(crate) const IGNORED_ATTR: &str = "ignored";
pub(crate) const ADDER_ATTR: &str = "adder";

const FOLDERS: DocumentBasedBasicSet<'static, i32> =
    DocumentBasedBasicSet::new(FOLDER_TAG, ID_ATTR);
const MUTED: DocumentBasedBoolean<'static> = DocumentBasedBoolean::new(MUTED_TAG, MUTED_ATTR);
const CLEARED: DocumentBasedBoolean<'static> = DocumentBasedBoolean::new(CLEARED_TAG, CLEARED_ATTR);
const ARCHIVE: DocumentBasedMonotonicMap<'static, WaveletId, i32> =
    DocumentBasedMonotonicMap::new(ARCHIVE_TAG, ID_ATTR, VERSION_ATTR);
const SEEN: DocumentBasedMonotonicMap<'static, WaveletId, HashedVersion> =
    DocumentBasedMonotonicMap::new(SEEN_VERSION_TAG, ID_ATTR, SIGNATURE_ATTR);
const NOTIFIED: DocumentBasedMonotonicMap<'static, WaveletId, i32> =
    DocumentBasedMonotonicMap::new(NOTIFIED_VERSION_TAG, ID_ATTR, VERSION_ATTR);
const PENDING_NOTIFICATION: DocumentBasedBoolean<'static> =
    DocumentBasedBoolean::new(NOTIFICATION_TAG, PENDING_NOTIFICATION_ATTR);
const BLIPS_READ: DocumentBasedMonotonicMap<'static, String, i32> =
    DocumentBasedMonotonicMap::new(BLIP_READ_TAG, ID_ATTR, VERSION_ATTR);
const PARTICIPANTS_READ: DocumentBasedMonotonicValue<'static, i32> =
    DocumentBasedMonotonicValue::new(PARTICIPANTS_READ_TAG, VERSION_ATTR);
const TAGS_READ: DocumentBasedMonotonicValue<'static, i32> =
    DocumentBasedMonotonicValue::new(TAGS_READ_TAG, VERSION_ATTR);
const WAVELET_READ: DocumentBasedMonotonicValue<'static, i32> =
    DocumentBasedMonotonicValue::new(WAVELET_READ_TAG, VERSION_ATTR);
const THREADS: DocumentBasedBasicMap<'static, String, ThreadState> =
    DocumentBasedBasicMap::new(THREAD_TAG, ID_ATTR, STATE_ATTR);
const GADGET_STATE: DocumentBasedBasicMap<'static, String, String> =
    DocumentBasedBasicMap::new(STATE_TAG, NAME_ATTR, VALUE_ATTR);
const WANTED: DocumentBasedElementList<'static> = DocumentBasedElementList::new(WANTED_EVAL_TAG);

/// The top-level elements with a tag name that stand for something: the
/// first element for each id (`WaveletReadStateCollection` and its
/// siblings ignore later elements with an id they already know, and
/// elements without a usable id).
fn keyed_elements<K: Serializable + PartialEq>(view: &DocView, tag: &str) -> Vec<(K, NodeId)> {
    let mut found: Vec<(K, NodeId)> = Vec::new();
    for element in view.child_elements(view.root()) {
        if !view.has_tag(element, tag) {
            continue;
        }
        let Some(key) = view.attribute(element, ID_ATTR).and_then(K::from_attribute) else {
            continue;
        };
        if !found.iter().any(|(known, _)| *known == key) {
            found.push((key, element));
        }
    }
    found
}

fn keyed_element<K: Serializable + PartialEq>(
    view: &DocView,
    tag: &str,
    key: &K,
) -> Option<NodeId> {
    keyed_elements::<K>(view, tag)
        .into_iter()
        .find(|(known, _)| known == key)
        .map(|(_, element)| element)
}

/// Names the element standing for a key in the current tree.
struct Keyed<'a, K> {
    tag: &'a str,
    key: &'a K,
}

impl<K: Serializable + PartialEq> Locator for Keyed<'_, K> {
    fn locate(&self, view: &DocView) -> Option<NodeId> {
        keyed_element(view, self.tag, self.key)
    }
}

/// Appends the element standing for a key unless there is one
/// (`getSupplement` and `createEntry` of the collections).
fn ensure_keyed<K: Serializable + PartialEq>(
    doc: &mut MutableDocument<'_>,
    tag: &str,
    key: &K,
) -> Result<(), AdtError> {
    if keyed_element(doc.view(), tag, key).is_none() {
        let id = key.to_attribute()?;
        let root = doc.view().root();
        doc.create_child_element(root, tag, &[(ID_ATTR, id.as_str())])?;
    }
    Ok(())
}

fn evaluation_of(view: &DocView, element: NodeId) -> Option<WantedEvaluation> {
    fn parsed<T: Serializable>(
        view: &DocView,
        element: NodeId,
        name: &str,
        default: T,
    ) -> Option<T> {
        match view.attribute(element, name) {
            None => Some(default),
            Some(value) => T::from_attribute(value),
        }
    }
    let text = |name: &str| view.attribute(element, name).unwrap_or("").to_owned();
    let wavelet_id = match view.attribute(element, WAVELET_ID_ATTR) {
        None => None,
        Some(id) => Some(WaveletId::from_attribute(id)?),
    };
    Some(WantedEvaluation {
        wavelet_id,
        adder_address: text(ADDER_ATTR),
        wanted: parsed(view, element, WANTED_ATTR, false)?,
        certainty: parsed(view, element, CERTAINTY_ATTR, 0.0)?,
        timestamp: parsed(view, element, TIMESTAMP_ATTR, 0i64)?,
        agent_identity: text(AGENT_ATTR),
        ignored: parsed(view, element, IGNORED_ATTR, false)?,
        comment: text(COMMENT_ATTR),
    })
}

/// The supplement state in the documents of a user-data wavelet.
#[derive(Clone, Copy)]
struct Reader<'a> {
    data: &'a WaveletData,
    /// Leave out the folders the original removes when it is created.
    hide_legacy_folders: bool,
}

impl Reader<'_> {
    fn view(&self, doc_id: &str) -> DocView {
        match self.data.document(doc_id) {
            Some(document) => DocView::new(document.content()),
            None => DocView::new(&BootstrapDocument::new()),
        }
    }

    fn read_state<T>(
        &self,
        wavelet_id: &WaveletId,
        read: impl FnOnce(&DocView, NodeId) -> Option<T>,
    ) -> Option<T> {
        let view = self.view(READSTATE_DOCUMENT);
        let element = keyed_element(&view, WAVELET_TAG, wavelet_id)?;
        read(&view, element)
    }
}

impl ReadablePrimitiveSupplement for Reader<'_> {
    fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState> {
        let view = self.view(PRESENTATION_DOCUMENT);
        let element = keyed_element(&view, CONVERSATION_TAG, wavelet_id)?;
        THREADS.get(&view, element, &thread_id.to_owned())
    }

    fn wavelets_with_thread_state(&self) -> Vec<WaveletId> {
        let view = self.view(PRESENTATION_DOCUMENT);
        let mut wavelets: Vec<WaveletId> = keyed_elements(&view, CONVERSATION_TAG)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        wavelets.sort();
        wavelets
    }

    fn stateful_threads(&self, wavelet_id: &WaveletId) -> Vec<String> {
        let view = self.view(PRESENTATION_DOCUMENT);
        match keyed_element(&view, CONVERSATION_TAG, wavelet_id) {
            Some(element) => THREADS.key_set(&view, element),
            None => Vec::new(),
        }
    }

    fn last_read_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.read_state(wavelet_id, |view, element| WAVELET_READ.get(view, element))
            .unwrap_or(NO_VERSION)
    }

    fn last_read_blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> i32 {
        self.read_state(wavelet_id, |view, element| {
            BLIPS_READ.get(view, element, &blip_id.to_owned())
        })
        .unwrap_or(NO_VERSION)
    }

    fn last_read_participants_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.read_state(wavelet_id, |view, element| {
            PARTICIPANTS_READ.get(view, element)
        })
        .unwrap_or(NO_VERSION)
    }

    fn last_read_tags_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.read_state(wavelet_id, |view, element| TAGS_READ.get(view, element))
            .unwrap_or(NO_VERSION)
    }

    fn read_wavelets(&self) -> Vec<WaveletId> {
        let view = self.view(READSTATE_DOCUMENT);
        let mut wavelets: Vec<WaveletId> = keyed_elements(&view, WAVELET_TAG)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        wavelets.sort();
        wavelets
    }

    fn read_blips(&self, wavelet_id: &WaveletId) -> Vec<String> {
        self.read_state(wavelet_id, |view, element| {
            Some(BLIPS_READ.key_set(view, element))
        })
        .unwrap_or_default()
    }

    fn folders(&self) -> Vec<i32> {
        let view = self.view(FOLDERS_DOCUMENT);
        let mut folders = FOLDERS.values(&view, view.root());
        if self.hide_legacy_folders {
            folders.retain(|folder| !LEGACY_FOLDERS.contains(folder));
        }
        folders
    }

    fn archive_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
        if self.is_cleared() {
            return NO_VERSION;
        }
        let view = self.view(ARCHIVING_DOCUMENT);
        ARCHIVE
            .get(&view, view.root(), wavelet_id)
            .unwrap_or(NO_VERSION)
    }

    fn archive_wavelets(&self) -> Vec<WaveletId> {
        let view = self.view(ARCHIVING_DOCUMENT);
        ARCHIVE.key_set(&view, view.root())
    }

    fn followed(&self) -> Option<bool> {
        let view = self.view(MUTED_DOCUMENT);
        MUTED.get(&view, view.root()).map(|muted| !muted)
    }

    fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion {
        let view = self.view(SEEN_DOCUMENT);
        SEEN.get(&view, view.root(), wavelet_id)
            .unwrap_or_else(|| HashedVersion::unsigned(0))
    }

    fn seen_wavelets(&self) -> Vec<WaveletId> {
        let view = self.view(SEEN_DOCUMENT);
        SEEN.key_set(&view, view.root())
    }

    fn wanted_evaluations(&self) -> Vec<WantedEvaluation> {
        let view = self.view(ABUSE_DOCUMENT);
        let mut evaluations: Vec<WantedEvaluation> = Vec::new();
        for element in WANTED.elements(&view, view.root()) {
            if let Some(evaluation) = evaluation_of(&view, element)
                && !evaluations.contains(&evaluation)
            {
                evaluations.push(evaluation);
            }
        }
        evaluations
    }

    fn pending_notification(&self) -> bool {
        let view = self.view(SEEN_DOCUMENT);
        PENDING_NOTIFICATION.get(&view, view.root()) == Some(true)
    }

    fn notified_wavelets(&self) -> Vec<WaveletId> {
        let view = self.view(SEEN_DOCUMENT);
        NOTIFIED.key_set(&view, view.root())
    }

    fn notified_version(&self, wavelet_id: &WaveletId) -> i32 {
        let view = self.view(SEEN_DOCUMENT);
        NOTIFIED
            .get(&view, view.root(), wavelet_id)
            .unwrap_or(NO_VERSION)
    }

    fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String> {
        let view = self.view(GADGETS_DOCUMENT);
        match keyed_element(&view, GADGET_TAG, &gadget_id.to_owned()) {
            Some(element) => GADGET_STATE.entries(&view, element).into_iter().collect(),
            None => BTreeMap::new(),
        }
    }
}

impl Reader<'_> {
    fn is_cleared(&self) -> bool {
        let view = self.view(CLEARED_DOCUMENT);
        CLEARED.get(&view, view.root()) == Some(true)
    }
}

/// The inbox and "all" folders, which the original removes from the folder
/// document whenever it creates a supplement (`hackCleanup`).
const LEGACY_FOLDERS: [i32; 2] = [1, 3];

macro_rules! delegate_reads {
    ($ty:ty, $reader:expr) => {
        impl ReadablePrimitiveSupplement for $ty {
            fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState> {
                $reader(self).thread_state(wavelet_id, thread_id)
            }

            fn wavelets_with_thread_state(&self) -> Vec<WaveletId> {
                $reader(self).wavelets_with_thread_state()
            }

            fn stateful_threads(&self, wavelet_id: &WaveletId) -> Vec<String> {
                $reader(self).stateful_threads(wavelet_id)
            }

            fn last_read_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
                $reader(self).last_read_wavelet_version(wavelet_id)
            }

            fn last_read_blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> i32 {
                $reader(self).last_read_blip_version(wavelet_id, blip_id)
            }

            fn last_read_participants_version(&self, wavelet_id: &WaveletId) -> i32 {
                $reader(self).last_read_participants_version(wavelet_id)
            }

            fn last_read_tags_version(&self, wavelet_id: &WaveletId) -> i32 {
                $reader(self).last_read_tags_version(wavelet_id)
            }

            fn read_wavelets(&self) -> Vec<WaveletId> {
                $reader(self).read_wavelets()
            }

            fn read_blips(&self, wavelet_id: &WaveletId) -> Vec<String> {
                $reader(self).read_blips(wavelet_id)
            }

            fn folders(&self) -> Vec<i32> {
                $reader(self).folders()
            }

            fn archive_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
                $reader(self).archive_wavelet_version(wavelet_id)
            }

            fn archive_wavelets(&self) -> Vec<WaveletId> {
                $reader(self).archive_wavelets()
            }

            fn followed(&self) -> Option<bool> {
                $reader(self).followed()
            }

            fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion {
                $reader(self).seen_version(wavelet_id)
            }

            fn seen_wavelets(&self) -> Vec<WaveletId> {
                $reader(self).seen_wavelets()
            }

            fn wanted_evaluations(&self) -> Vec<WantedEvaluation> {
                $reader(self).wanted_evaluations()
            }

            fn pending_notification(&self) -> bool {
                $reader(self).pending_notification()
            }

            fn notified_wavelets(&self) -> Vec<WaveletId> {
                $reader(self).notified_wavelets()
            }

            fn notified_version(&self, wavelet_id: &WaveletId) -> i32 {
                $reader(self).notified_version(wavelet_id)
            }

            fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String> {
                $reader(self).gadget_state(gadget_id)
            }
        }
    };
}

/// A read-only view of the supplement state in a user-data wavelet: what a
/// `WaveletBasedSupplement` created on the wavelet would report, without
/// creating or changing anything.
///
/// The original has no read-only supplement; the server creates a
/// `WaveletBasedSupplement` over a read-only wavelet. Creating one removes
/// the legacy inbox (1) and "all" (3) folders from the folder document, so
/// this view does not report those folders.
///
/// Every query interprets a document afresh. To ask many questions, take a
/// snapshot with [`PrimitiveSupplementImpl::copy_of`](super::PrimitiveSupplementImpl::copy_of).
#[derive(Debug, Clone, Copy)]
pub struct UserDataSupplement<'a> {
    data: &'a WaveletData,
}

impl<'a> UserDataSupplement<'a> {
    pub fn new(user_data: &'a WaveletData) -> Self {
        UserDataSupplement { data: user_data }
    }

    fn reader(&self) -> Reader<'a> {
        Reader {
            data: self.data,
            hide_legacy_folders: true,
        }
    }
}

delegate_reads!(UserDataSupplement<'_>, UserDataSupplement::reader);

/// The supplement state in a user-data wavelet, for reading and writing
/// (`WaveletBasedSupplement`). Every change is made through document edits
/// on the wavelet, which records them as operations to submit.
///
/// # Documents
///
/// Every element below is a direct child of its document, and written
/// without content. `i` attributes holding a wavelet id use the form
/// `domain/id`.
///
/// * `m/read`, read versions:
///   `<wavelet i="example.com/conv+root"><blip i="b+1" v="7"/><participants v="12"/><tags v="3"/><all v="25"/></wavelet>`.
///   The greatest `v` counts where a `blip` id or one of the other elements
///   occurs more than once. Of several `wavelet` elements with one id the
///   first counts.
/// * `m/presentation`, thread states:
///   `<conversation i="example.com/conv+root"><thread i="t+1" state="COLLAPSED"/></conversation>`,
///   state `COLLAPSED` or `EXPANDED`; the last `thread` element of an id
///   counts, and the first `conversation` element of an id.
/// * `m/folder`: `<folder i="8"/>` per folder.
/// * `m/archiving`: `<archive i="example.com/conv+root" v="48"/>` per
///   wavelet; the greatest `v` counts.
/// * `m/cleared`: `<cleared cleared="true"/>`; while true, the archive
///   versions are disregarded. The last element counts.
/// * `m/muted`: `<muted muted="true"/>` when unfollowed, `"false"` when
///   followed, absent when neither; the last element counts.
/// * `m/seen`: `<seen i="example.com/conv+root" signature="12:aGFzaA=="/>`
///   (version, colon, base64 history hash; the greatest counts),
///   `<notified i="example.com/conv+root" v="12"/>` (the greatest counts)
///   and `<notification pending="true"/>`.
/// * `m/gadgets`: `<gadget i="gadget id"><state name="key" value="value"/></gadget>`;
///   the last `state` of a name counts, and the first `gadget` of an id.
/// * `m/abuse`: one `<wanted wavelet_id=".." wanted="true" certainty="0.5" timestamp="1234" adder=".." agent=".." comment=".." ignored="false"/>`
///   per evaluation.
///
/// New `blip`, `thread`, `state`, `archive`, `seen` and `notified` elements
/// are inserted first in their parent, after the element they replace has
/// been deleted; all others are appended, the elements they replace being
/// deleted afterwards. Any write to one of these structures also deletes
/// the superseded elements it finds in it.
///
/// # Differences from the original
///
/// * The original object caches the documents and follows their changes;
///   this one reads the documents on every call. The two agree except
///   after [`clear_read_state`](PrimitiveSupplement::clear_read_state) on a
///   read-state document with several `wavelet` elements for one wavelet:
///   only the first is deleted, the original object then reports no read
///   state for the wavelet until it is created anew, whereas here (as for
///   a newly created original) the next element takes over at once.
/// * Elements whose id or value does not parse are ignored (see
///   [`crate::adt`]); the original fails to create the supplement.
/// * [`clear_blip_read_state`](PrimitiveSupplement::clear_blip_read_state)
///   for a wavelet without read state does nothing; the original throws a
///   `NullPointerException`.
/// * Where the original deletes several elements in hash order (clearing
///   the read state, superseded elements) they are deleted in document
///   order, and folders are removed in ascending order.
///
/// Quirks kept: archive versions never decrease, so archiving again at a
/// lower version after the archive state was cleared restores the old,
/// higher version; setting a gadget state to `None` creates the `gadget`
/// element if there is none; the pending notification flag can be cleared
/// but not set.
pub struct WaveletBasedSupplement<'a> {
    user_data: &'a mut OpBasedWavelet,
}

impl<'a> WaveletBasedSupplement<'a> {
    /// Wraps a user-data wavelet (`WaveletBasedSupplement.create`). As in
    /// the original this removes the legacy inbox (1) and "all" (3) folders
    /// from the folder document if they are there, which emits operations.
    pub fn create(user_data: &'a mut OpBasedWavelet) -> Result<Self, SupplementError> {
        let mut supplement = WaveletBasedSupplement { user_data };
        for folder in LEGACY_FOLDERS {
            supplement.remove_folder(folder)?;
        }
        Ok(supplement)
    }

    /// The wavelet holding the state.
    pub fn user_data(&self) -> &OpBasedWavelet {
        self.user_data
    }

    fn reader(&self) -> Reader<'_> {
        Reader {
            data: self.user_data.data(),
            hide_legacy_folders: false,
        }
    }

    fn edit<R>(
        &mut self,
        doc_id: &str,
        edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<R, AdtError>,
    ) -> Result<R, SupplementError> {
        let mut sink = self.user_data.document_sink(doc_id);
        let mut doc = MutableDocument::new(&mut sink);
        Ok(edit(&mut doc)?)
    }

    fn edit_read_state(
        &mut self,
        wavelet_id: &WaveletId,
        edit: impl FnOnce(&mut MutableDocument<'_>, &dyn Locator) -> Result<bool, AdtError>,
    ) -> Result<(), SupplementError> {
        self.edit(READSTATE_DOCUMENT, |doc| {
            ensure_keyed(doc, WAVELET_TAG, wavelet_id)?;
            let wavelet = Keyed {
                tag: WAVELET_TAG,
                key: wavelet_id,
            };
            edit(doc, &wavelet).map(drop)
        })
    }
}

delegate_reads!(WaveletBasedSupplement<'_>, WaveletBasedSupplement::reader);

impl PrimitiveSupplement for WaveletBasedSupplement<'_> {
    fn set_thread_state(
        &mut self,
        wavelet_id: &WaveletId,
        thread_id: &str,
        state: Option<ThreadState>,
    ) -> Result<(), SupplementError> {
        let thread_id = thread_id.to_owned();
        let conversation = Keyed {
            tag: CONVERSATION_TAG,
            key: wavelet_id,
        };
        self.edit(PRESENTATION_DOCUMENT, |doc| match state {
            None => {
                if conversation.locate(doc.view()).is_some() {
                    THREADS.remove(doc, &conversation, &thread_id)?;
                }
                Ok(())
            }
            Some(state) => {
                ensure_keyed(doc, CONVERSATION_TAG, wavelet_id)?;
                THREADS
                    .put(doc, &conversation, &thread_id, &state)
                    .map(drop)
            }
        })
    }

    fn set_last_read_wavelet_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.edit_read_state(wavelet_id, |doc, wavelet| {
            WAVELET_READ.set(doc, wavelet, &version)
        })
    }

    fn set_last_read_blip_version(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
        version: i32,
    ) -> Result<(), SupplementError> {
        let blip_id = blip_id.to_owned();
        self.edit_read_state(wavelet_id, |doc, wavelet| {
            BLIPS_READ.put(doc, wavelet, &blip_id, &version)
        })
    }

    fn set_last_read_participants_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.edit_read_state(wavelet_id, |doc, wavelet| {
            PARTICIPANTS_READ.set(doc, wavelet, &version)
        })
    }

    fn set_last_read_tags_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.edit_read_state(wavelet_id, |doc, wavelet| {
            TAGS_READ.set(doc, wavelet, &version)
        })
    }

    fn clear_read_state(&mut self) -> Result<(), SupplementError> {
        self.edit(READSTATE_DOCUMENT, |doc| {
            let elements: Vec<NodeId> = keyed_elements::<WaveletId>(doc.view(), WAVELET_TAG)
                .into_iter()
                .map(|(_, element)| element)
                .collect();
            crate::adt::delete_nodes(doc, &elements)
        })
    }

    fn clear_blip_read_state(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
    ) -> Result<(), SupplementError> {
        let blip_id = blip_id.to_owned();
        let wavelet = Keyed {
            tag: WAVELET_TAG,
            key: wavelet_id,
        };
        self.edit(READSTATE_DOCUMENT, |doc| {
            if wavelet.locate(doc.view()).is_some() {
                BLIPS_READ.remove(doc, &wavelet, &blip_id)?;
            }
            Ok(())
        })
    }

    fn add_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        self.edit(FOLDERS_DOCUMENT, |doc| {
            FOLDERS.add(doc, &Root, &id).map(drop)
        })
    }

    fn remove_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        self.edit(FOLDERS_DOCUMENT, |doc| {
            FOLDERS.remove(doc, &Root, &id).map(drop)
        })
    }

    fn remove_all_folders(&mut self) -> Result<(), SupplementError> {
        self.edit(FOLDERS_DOCUMENT, |doc| FOLDERS.clear(doc, &Root).map(drop))
    }

    fn follow(&mut self) -> Result<(), SupplementError> {
        self.edit(MUTED_DOCUMENT, |doc| {
            MUTED.set(doc, &Root, Some(false)).map(drop)
        })
    }

    fn unfollow(&mut self) -> Result<(), SupplementError> {
        self.edit(MUTED_DOCUMENT, |doc| {
            MUTED.set(doc, &Root, Some(true)).map(drop)
        })
    }

    fn clear_follow(&mut self) -> Result<(), SupplementError> {
        self.edit(MUTED_DOCUMENT, |doc| MUTED.set(doc, &Root, None).map(drop))
    }

    fn archive_at_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.edit(ARCHIVING_DOCUMENT, |doc| {
            ARCHIVE.put(doc, &Root, wavelet_id, &version).map(drop)
        })?;
        if self.reader().is_cleared() {
            self.edit(CLEARED_DOCUMENT, |doc| {
                CLEARED.set(doc, &Root, Some(false)).map(drop)
            })?;
        }
        Ok(())
    }

    fn clear_archive_state(&mut self) -> Result<(), SupplementError> {
        // The archive versions cannot be removed or lowered; a flag
        // overrides them instead.
        if !self.reader().is_cleared() {
            self.edit(CLEARED_DOCUMENT, |doc| {
                CLEARED.set(doc, &Root, Some(true)).map(drop)
            })?;
        }
        Ok(())
    }

    fn set_seen_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: &HashedVersion,
    ) -> Result<(), SupplementError> {
        self.edit(SEEN_DOCUMENT, |doc| {
            SEEN.put(doc, &Root, wavelet_id, version).map(drop)
        })
    }

    fn clear_seen_version(&mut self, wavelet_id: &WaveletId) -> Result<(), SupplementError> {
        self.edit(SEEN_DOCUMENT, |doc| SEEN.remove(doc, &Root, wavelet_id))
    }

    fn add_wanted_evaluation(
        &mut self,
        evaluation: &WantedEvaluation,
    ) -> Result<(), SupplementError> {
        // A missing wavelet id makes the original fail when it builds the
        // attributes of the element.
        let wavelet_id = evaluation
            .wavelet_id
            .as_ref()
            .ok_or(SupplementError::IllegalState(
                "a wanted evaluation to store needs a wavelet id",
            ))?
            .to_attribute()?;
        let mut attributes = InitialAttributes::new();
        let mut put = |name: &str, value: String| attributes.insert(name.to_owned(), value);
        put(WAVELET_ID_ATTR, wavelet_id);
        put(WANTED_ATTR, evaluation.wanted.to_attribute()?);
        put(CERTAINTY_ATTR, evaluation.certainty.to_attribute()?);
        put(TIMESTAMP_ATTR, evaluation.timestamp.to_attribute()?);
        put(ADDER_ATTR, evaluation.adder_address.clone());
        put(AGENT_ATTR, evaluation.agent_identity.clone());
        put(COMMENT_ATTR, evaluation.comment.clone());
        put(IGNORED_ATTR, evaluation.ignored.to_attribute()?);
        self.edit(ABUSE_DOCUMENT, |doc| {
            WANTED.add(doc, &Root, &attributes).map(drop)
        })
    }

    fn set_notified_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.edit(SEEN_DOCUMENT, |doc| {
            NOTIFIED.put(doc, &Root, wavelet_id, &version).map(drop)
        })
    }

    fn clear_pending_notification(&mut self) -> Result<(), SupplementError> {
        self.edit(SEEN_DOCUMENT, |doc| {
            PENDING_NOTIFICATION.set(doc, &Root, None).map(drop)
        })
    }

    fn set_gadget_state(
        &mut self,
        gadget_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), SupplementError> {
        let gadget_id = gadget_id.to_owned();
        let key = key.to_owned();
        let gadget = Keyed {
            tag: GADGET_TAG,
            key: &gadget_id,
        };
        self.edit(GADGETS_DOCUMENT, |doc| {
            ensure_keyed(doc, GADGET_TAG, &gadget_id)?;
            match value {
                Some(value) => GADGET_STATE
                    .put(doc, &gadget, &key, &value.to_owned())
                    .map(drop),
                None => GADGET_STATE.remove(doc, &gadget, &key),
            }
        })
    }
}
