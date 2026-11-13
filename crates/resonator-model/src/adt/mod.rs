//! Data types stored in the elements and attributes of a document.
//!
//! The supplement and the conversation manifest keep their state in small
//! XML documents through a handful of embeddings: a map is a list of
//! `<entry key=".." value=".."/>` elements, a set a list of
//! `<entry value=".."/>` elements, and so on. Each embedding tolerates the
//! redundant elements that concurrent writers leave behind, and removes them
//! the next time it writes.
//!
//! Derived from `org.waveprotocol.wave.model.adt.docbased`
//! (`AbstractDocumentBasedMap`, `DocumentBasedBasicMap`,
//! `DocumentBasedMonotonicMap`, `DocumentBasedBasicSet`,
//! `DocumentBasedBasicValue`, `DocumentBasedMonotonicValue`,
//! `DocumentBasedBoolean`, `DocumentBasedElementList`,
//! `DocumentBasedSingleton`, `DocumentBasedStructuredValue`, `Initializer`,
//! `CompoundInitializer`) and `org.waveprotocol.wave.model.util.Serializer`
//! of Apache Wave, licensed under the Apache License, Version 2.0.
//!
//! Differences from the original:
//!
//! * The original types are live objects: each one caches what it found in
//!   the document and keeps the cache current by listening to document
//!   events. Here a type is only a description of the embedding (tag and
//!   attribute names). Every read interprets the document as it is, exactly
//!   as a freshly created original object would, and every write does what
//!   that fresh object would do. Listeners and events are not ported, nor
//!   are the in-memory implementations (`ObservableBasicMapImpl` and
//!   friends, `InMemoryElementList`): use the standard collections.
//! * A node of a [`DocView`] does not survive an edit, so the element that
//!   holds an embedding is named by a [`Locator`] wherever a write needs
//!   several edits.
//! * Where the original deletes redundant elements in the iteration order
//!   of a hash set of document nodes, which is arbitrary, they are deleted
//!   here in document order. [`DocumentBasedBasicSet::clear`] deletes the
//!   value elements in ascending order of value (the original: hash-map
//!   order of the values, which for small non-negative integers is the
//!   same). Key and value listings are sorted.
//! * An entry whose key or value attribute is missing or does not parse is
//!   ignored: it is neither a value nor a redundant element, and is never
//!   deleted. The original throws when it meets a value that does not parse
//!   (while loading, so the object cannot even be created) and treats a
//!   missing attribute as a `null` key or value, which later makes some of
//!   its writes fail half-way (removing all values of a set, for one).
//! * Numbers are parsed with Rust's rules, which accept the same strings as
//!   `Integer.parseInt` and `Long.parseLong` except digits outside ASCII.

mod initializer;
mod list;
mod map;
mod serializer;
mod set;
mod singleton;
mod value;

#[cfg(test)]
mod tests;

use thiserror::Error;

pub use initializer::{
    CompoundInitializer, InitialAttributes, Initializer, build_attributes, initialise_attribute,
};
pub use list::DocumentBasedElementList;
pub use map::{DocumentBasedBasicMap, DocumentBasedMonotonicMap};
pub use serializer::{Serializable, java_double_to_string, parse_java_double};
pub use set::DocumentBasedBasicSet;
pub use singleton::DocumentBasedSingleton;
pub use value::{
    DocumentBasedBasicValue, DocumentBasedBoolean, DocumentBasedMonotonicValue,
    DocumentBasedStructuredValue,
};

use crate::doc::{DocError, DocView, MutableDocument, NodeId};

/// Failure of a write to a document-based data type.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdtError {
    #[error(transparent)]
    Document(#[from] DocError),
    /// The element that should hold the data is not in the document.
    #[error("the container element is not in the document")]
    MissingContainer,
    /// `Initializer.Helper.initialiseAttribute` met a key twice.
    #[error("Duplicate initialiser key '{0}' in initialiser")]
    DuplicateInitialiserKey(String),
    /// A list index beyond the end of the list (the original throws
    /// `IndexOutOfBoundsException`).
    #[error("index {index} is out of bounds for a list of {size}")]
    IndexOutOfBounds { index: usize, size: usize },
    /// A value that has no attribute form (the original throws
    /// `IllegalArgumentException`).
    #[error("value cannot be serialised: {0}")]
    Unserialisable(String),
}

/// Finds the element that holds an embedding in the current tree of a
/// document. Stands in for the element object the original types keep.
pub trait Locator {
    fn locate(&self, view: &DocView) -> Option<NodeId>;
}

impl<F: Fn(&DocView) -> Option<NodeId>> Locator for F {
    fn locate(&self, view: &DocView) -> Option<NodeId> {
        self(view)
    }
}

/// The document itself (the original's document element).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Root;

impl Locator for Root {
    fn locate(&self, view: &DocView) -> Option<NodeId> {
        Some(view.root())
    }
}

/// The first top-level element with a tag name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstTopLevel<'a>(pub &'a str);

impl Locator for FirstTopLevel<'_> {
    fn locate(&self, view: &DocView) -> Option<NodeId> {
        view.child_elements(view.root())
            .find(|&child| view.has_tag(child, self.0))
    }
}

pub(crate) fn locate(container: &dyn Locator, view: &DocView) -> Result<NodeId, AdtError> {
    container
        .locate(view)
        .filter(|&node| view.is_element(node))
        .ok_or(AdtError::MissingContainer)
}

/// Deletes elements of the current tree, none inside another, one edit
/// each, in the order given.
pub(crate) fn delete_nodes(
    doc: &mut MutableDocument<'_>,
    nodes: &[NodeId],
) -> Result<(), AdtError> {
    let spans: Vec<(usize, usize)> = nodes
        .iter()
        .map(|&node| (doc.view().location(node), doc.view().item_size(node)))
        .collect();
    for (index, &(location, _)) in spans.iter().enumerate() {
        // Earlier deletions to the left have shifted this element.
        let shift: usize = spans[..index]
            .iter()
            .filter(|&&(earlier, _)| earlier < location)
            .map(|&(_, size)| size)
            .sum();
        let node = doc
            .view()
            .element_starting_at(location - shift)
            .ok_or(DocError::InvalidNode)?;
        doc.delete_node(node)?;
    }
    Ok(())
}

/// The elements of the current tree in document order.
pub(crate) fn in_document_order(view: &DocView, mut nodes: Vec<NodeId>) -> Vec<NodeId> {
    nodes.sort_by_key(|&node| view.location(node));
    nodes
}
