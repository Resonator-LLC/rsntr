//! The conversation manifest: the document that holds the thread structure
//! of a conversation (`DocumentBasedManifest`, `DocumentBasedManifestThread`,
//! `DocumentBasedManifestBlip`).
//!
//! ```text
//! <conversation anchorWavelet="..." anchorBlip="...">   root thread
//!   <blip id="b+1">
//!     <thread id="b+2" inline="true">                   reply thread
//!       <blip id="b+2"/>
//!     </thread>
//!   </blip>
//! </conversation>
//! ```
//!
//! A thread's blips are its `blip` child elements and a blip's reply
//! threads its `thread` child elements, in document order; children with
//! other tags and text are ignored.

use crate::doc::{DocError, DocView, MutableDocument, NodeId};
use crate::docop::BootstrapDocument;

pub const MANIFEST_TOP_TAG: &str = "conversation";
pub const BLIP_TAG: &str = "blip";
pub const THREAD_TAG: &str = "thread";
pub const BLIP_ID_ATTR: &str = "id";
pub const THREAD_ID_ATTR: &str = "id";
pub const INLINE_ATTR: &str = "inline";
pub const ANCHOR_WAVELET_ATTR: &str = "anchorWavelet";
pub const ANCHOR_BLIP_ATTR: &str = "anchorBlip";

/// Where a conversation is anchored: the serialised wavelet id of another
/// conversation and a blip in it (`AnchorData`). Either may be missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnchorData {
    pub conversation_id: Option<String>,
    pub blip_id: Option<String>,
}

/// A thread entry of the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestThread {
    /// `None` if the element has no id (as for the root thread).
    pub id: Option<String>,
    pub inline: bool,
    pub blips: Vec<ManifestBlip>,
    /// Location of the element in the manifest document it was read from.
    pub location: usize,
}

/// A blip entry of the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestBlip {
    /// `None` if the element has no id.
    pub id: Option<String>,
    pub threads: Vec<ManifestThread>,
    /// Location of the element in the manifest document it was read from.
    pub location: usize,
}

/// The content of a manifest document at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub anchor: AnchorData,
    pub root_thread: ManifestThread,
}

pub(crate) fn read_thread(view: &DocView, element: NodeId) -> ManifestThread {
    ManifestThread {
        id: view.attribute(element, THREAD_ID_ATTR).map(str::to_owned),
        // Serializer.BOOLEAN: Boolean.valueOf.
        inline: view
            .attribute(element, INLINE_ATTR)
            .is_some_and(|value| value.eq_ignore_ascii_case("true")),
        blips: view
            .child_elements(element)
            .filter(|&child| view.has_tag(child, BLIP_TAG))
            .map(|child| read_blip(view, child))
            .collect(),
        location: view.location(element),
    }
}

pub(crate) fn read_blip(view: &DocView, element: NodeId) -> ManifestBlip {
    ManifestBlip {
        id: view.attribute(element, BLIP_ID_ATTR).map(str::to_owned),
        threads: view
            .child_elements(element)
            .filter(|&child| view.has_tag(child, THREAD_TAG))
            .map(|child| read_thread(view, child))
            .collect(),
        location: view.location(element),
    }
}

/// The manifest element: the first `conversation` element of the document
/// (the canonical element of the original's `DocumentBasedSingleton`).
pub fn manifest_element(view: &DocView) -> Option<NodeId> {
    view.element_with_tag_name(MANIFEST_TOP_TAG, view.root())
}

impl Manifest {
    /// Reads the manifest of a document, or `None` if it has none.
    pub fn read(doc: &BootstrapDocument) -> Option<Manifest> {
        Self::from_view(&DocView::new(doc))
    }

    pub fn from_view(view: &DocView) -> Option<Manifest> {
        let top = manifest_element(view)?;
        Some(Manifest {
            anchor: AnchorData {
                conversation_id: view.attribute(top, ANCHOR_WAVELET_ATTR).map(str::to_owned),
                blip_id: view.attribute(top, ANCHOR_BLIP_ATTR).map(str::to_owned),
            },
            root_thread: read_thread(view, top),
        })
    }
}

/// Whether the first element of a document is a manifest
/// (`DocumentBasedManifest.documentHasManifest`).
pub fn document_has_manifest(doc: &BootstrapDocument) -> bool {
    let view = DocView::new(doc);
    view.first_child_element(view.root())
        .is_some_and(|top| view.has_tag(top, MANIFEST_TOP_TAG))
}

/// Creates an empty manifest in an empty document
/// (`DocumentBasedManifest.initialiseDocumentManifest`). Fails if the
/// document has any content.
pub fn initialise_document_manifest(doc: &mut MutableDocument<'_>) -> Result<(), DocError> {
    if doc.view().first_child(doc.view().root()).is_some() {
        return Err(DocError::Structure(
            "Document already has top-level node".to_owned(),
        ));
    }
    doc.create_child_element(doc.view().root(), MANIFEST_TOP_TAG, &[])?;
    Ok(())
}

/// Deletes the manifest, which must be the first node of the document
/// (`DocumentBasedManifest.delete`).
pub fn delete_manifest(doc: &mut MutableDocument<'_>) -> Result<(), DocError> {
    let view = doc.view();
    let Some(first) = view.first_child(view.root()) else {
        return Err(DocError::Structure(
            "Document has no top-level node".to_owned(),
        ));
    };
    if !view.has_tag(first, MANIFEST_TOP_TAG) {
        return Err(DocError::Structure(
            "First node is not a conversation manifest".to_owned(),
        ));
    }
    doc.delete_node(first)
}
