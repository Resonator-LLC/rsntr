//! A set stored as one element per value (`DocumentBasedBasicSet`).

use std::marker::PhantomData;

use super::{AdtError, Locator, Serializable, delete_nodes, in_document_order, locate};
use crate::doc::{DocView, MutableDocument, NodeId};

/// A set stored as one element per value. Of several elements with the same
/// value the first in the document counts and the others are redundant.
///
/// ```text
/// <container>
///   <entry value="3"/>
///   <entry value="12"/>
///   <entry value="12"/>      redundant
/// </container>
/// ```
///
/// Adding a value appends an element to the container. Every change to the
/// set also deletes the redundant elements.
#[derive(Debug)]
pub struct DocumentBasedBasicSet<'a, T> {
    entry_tag: &'a str,
    value_attr: &'a str,
    types: PhantomData<fn() -> T>,
}

impl<T> Clone for DocumentBasedBasicSet<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for DocumentBasedBasicSet<'_, T> {}

impl<'a, T: Serializable + Ord> DocumentBasedBasicSet<'a, T> {
    pub const fn new(entry_tag: &'a str, value_attr: &'a str) -> Self {
        DocumentBasedBasicSet {
            entry_tag,
            value_attr,
            types: PhantomData,
        }
    }

    /// The values with their elements, ordered by value, and the redundant
    /// elements.
    fn load(&self, view: &DocView, container: NodeId) -> (Vec<(T, NodeId)>, Vec<NodeId>) {
        let mut values: Vec<(T, NodeId)> = Vec::new();
        let mut obsolete = Vec::new();
        for element in view.child_elements(container) {
            if !view.has_tag(element, self.entry_tag) {
                continue;
            }
            let Some(value) = view
                .attribute(element, self.value_attr)
                .and_then(T::from_attribute)
            else {
                continue;
            };
            if values.iter().any(|(known, _)| *known == value) {
                obsolete.push(element);
            } else {
                values.push((value, element));
            }
        }
        values.sort_by(|a, b| a.0.cmp(&b.0));
        (values, obsolete)
    }

    /// The values, in order (`getValues`).
    pub fn values(&self, view: &DocView, container: NodeId) -> Vec<T> {
        let (values, _) = self.load(view, container);
        values.into_iter().map(|(value, _)| value).collect()
    }

    pub fn contains(&self, view: &DocView, container: NodeId, value: &T) -> bool {
        let (values, _) = self.load(view, container);
        values.iter().any(|(known, _)| known == value)
    }

    /// Adds a value unless present. Returns whether the document was
    /// changed.
    pub fn add(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        value: &T,
    ) -> Result<bool, AdtError> {
        let node = locate(container, doc.view())?;
        let (values, obsolete) = self.load(doc.view(), node);
        if values.iter().any(|(known, _)| known == value) {
            return Ok(false);
        }
        let value = value.to_attribute()?;
        let locations: Vec<usize> = obsolete
            .iter()
            .map(|&element| doc.view().location(element))
            .collect();
        doc.create_child_element(node, self.entry_tag, &[(self.value_attr, value.as_str())])?;
        let obsolete: Vec<NodeId> = locations
            .into_iter()
            .filter_map(|location| doc.view().element_starting_at(location))
            .collect();
        delete_nodes(doc, &obsolete)?;
        Ok(true)
    }

    /// Removes a value if present. Returns whether the document was
    /// changed.
    pub fn remove(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        value: &T,
    ) -> Result<bool, AdtError> {
        let node = locate(container, doc.view())?;
        let (values, obsolete) = self.load(doc.view(), node);
        let Some(&(_, element)) = values.iter().find(|(known, _)| known == value) else {
            return Ok(false);
        };
        let mut doomed = in_document_order(doc.view(), obsolete);
        doomed.push(element);
        delete_nodes(doc, &doomed)?;
        Ok(true)
    }

    /// Removes all values. Returns whether the document was changed.
    pub fn clear(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
    ) -> Result<bool, AdtError> {
        let node = locate(container, doc.view())?;
        let (values, obsolete) = self.load(doc.view(), node);
        if values.is_empty() {
            return Ok(false);
        }
        let mut doomed = in_document_order(doc.view(), obsolete);
        doomed.extend(values.iter().map(|&(_, element)| element));
        delete_nodes(doc, &doomed)?;
        Ok(true)
    }
}
