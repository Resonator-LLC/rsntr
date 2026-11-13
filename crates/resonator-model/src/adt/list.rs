//! A list stored as one element per item (`DocumentBasedElementList`).

use super::initializer::InitialAttributes;
use super::{AdtError, Locator, locate};
use crate::doc::{DocView, MutableDocument, NodeId};

/// A list whose items are the child elements of a container with a given
/// tag name, in document order. Other children are ignored.
///
/// The original adapts every element to a value object through a `Factory`
/// and identifies items by those objects. Here an item is identified by its
/// index, [`Self::elements`] gives the elements to read values from, and a
/// new item is created with the attributes an
/// [`Initializer`](super::Initializer) built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentBasedElementList<'a> {
    child_tag: &'a str,
}

impl<'a> DocumentBasedElementList<'a> {
    pub const fn new(child_tag: &'a str) -> Self {
        DocumentBasedElementList { child_tag }
    }

    /// The elements of the items, in order (`getValues`).
    pub fn elements(&self, view: &DocView, container: NodeId) -> Vec<NodeId> {
        view.child_elements(container)
            .filter(|&element| view.has_tag(element, self.child_tag))
            .collect()
    }

    /// The items, each adapted from its element.
    pub fn values<T>(
        &self,
        view: &DocView,
        container: NodeId,
        adapt: impl Fn(&DocView, NodeId) -> T,
    ) -> Vec<T> {
        let elements = self.elements(view, container);
        elements.into_iter().map(|e| adapt(view, e)).collect()
    }

    /// The element of the item at an index (`get`).
    pub fn get(&self, view: &DocView, container: NodeId, index: usize) -> Option<NodeId> {
        self.elements(view, container).get(index).copied()
    }

    /// The index of the item an element stands for (`indexOf`).
    pub fn index_of(&self, view: &DocView, container: NodeId, element: NodeId) -> Option<usize> {
        let elements = self.elements(view, container);
        elements.iter().position(|&e| e == element)
    }

    pub fn size(&self, view: &DocView, container: NodeId) -> usize {
        self.elements(view, container).len()
    }

    fn pairs(attributes: &InitialAttributes) -> Vec<(&str, &str)> {
        attributes
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
    }

    /// Appends an item, as the last child of the container, and returns
    /// its element (`add(I)`).
    pub fn add(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        attributes: &InitialAttributes,
    ) -> Result<NodeId, AdtError> {
        let node = locate(container, doc.view())?;
        Ok(doc.create_child_element(node, self.child_tag, &Self::pairs(attributes))?)
    }

    /// Inserts an item so that it gets an index, just before the element of
    /// the item that has that index now, and returns its element
    /// (`add(int, I)`).
    pub fn add_at(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        index: usize,
        attributes: &InitialAttributes,
    ) -> Result<NodeId, AdtError> {
        let node = locate(container, doc.view())?;
        let elements = self.elements(doc.view(), node);
        if index > elements.len() {
            return Err(AdtError::IndexOutOfBounds {
                index,
                size: elements.len(),
            });
        }
        match elements.get(index) {
            None => self.add(doc, container, attributes),
            Some(&after) => {
                let location = doc.view().location(after);
                Ok(doc.create_element(location, self.child_tag, &Self::pairs(attributes))?)
            }
        }
    }

    /// Deletes the item at an index with its element. Returns false if
    /// there is no such item.
    pub fn remove(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        index: usize,
    ) -> Result<bool, AdtError> {
        let node = locate(container, doc.view())?;
        let Some(element) = self.get(doc.view(), node, index) else {
            return Ok(false);
        };
        doc.delete_node(element)?;
        Ok(true)
    }

    /// Deletes all items, first to last, one operation each.
    pub fn clear(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
    ) -> Result<(), AdtError> {
        while self.remove(doc, container, 0)? {}
        Ok(())
    }
}
