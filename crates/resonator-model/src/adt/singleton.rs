//! A value stored in at most one element (`DocumentBasedSingleton`).

use super::initializer::InitialAttributes;
use super::{AdtError, Locator, locate};
use crate::doc::{DocView, MutableDocument, NodeId};

/// An optional value held by the first element with a given tag name
/// anywhere below a container; further such elements are redundant.
///
/// Setting the value inserts a new element as the first child of the
/// container and then deletes all others, last to first. As in the
/// original, elements with the tag name nested deeper in the container
/// count too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentBasedSingleton<'a> {
    value_tag: &'a str,
}

impl<'a> DocumentBasedSingleton<'a> {
    pub const fn new(value_tag: &'a str) -> Self {
        DocumentBasedSingleton { value_tag }
    }

    fn all(&self, view: &DocView, container: NodeId) -> Vec<NodeId> {
        let mut elements = view.descendant_elements(container);
        elements.retain(|&element| view.has_tag(element, self.value_tag));
        elements
    }

    /// The element holding the value, if any (`get`).
    pub fn get(&self, view: &DocView, container: NodeId) -> Option<NodeId> {
        view.element_with_tag_name(self.value_tag, container)
    }

    pub fn has_value(&self, view: &DocView, container: NodeId) -> bool {
        self.get(view, container).is_some()
    }

    /// Deletes all elements with the tag name but the first, last to first.
    fn cleanup(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
    ) -> Result<(), AdtError> {
        loop {
            let node = locate(container, doc.view())?;
            let all = self.all(doc.view(), node);
            match all.last() {
                Some(&last) if all.len() > 1 => doc.delete_node(last)?,
                _ => return Ok(()),
            }
        }
    }

    /// Replaces the value by a new element with the given attributes and
    /// returns that element (`set`).
    pub fn set(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        attributes: &InitialAttributes,
    ) -> Result<NodeId, AdtError> {
        let node = locate(container, doc.view())?;
        let start = doc.view().content_start(node);
        let pairs: Vec<(&str, &str)> = attributes
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        doc.create_element(start, self.value_tag, &pairs)?;
        self.cleanup(doc, container)?;
        let node = locate(container, doc.view())?;
        self.get(doc.view(), node).ok_or(AdtError::MissingContainer)
    }

    /// Removes the value: deletes every element with the tag name.
    pub fn clear(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
    ) -> Result<(), AdtError> {
        self.cleanup(doc, container)?;
        let node = locate(container, doc.view())?;
        if let Some(element) = self.get(doc.view(), node) {
            doc.delete_node(element)?;
        }
        Ok(())
    }
}
