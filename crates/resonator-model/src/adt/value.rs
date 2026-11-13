//! Single values stored in attributes or in one element per write.

use std::marker::PhantomData;

use super::initializer::{InitialAttributes, Initializer, initialise_attribute};
use super::{AdtError, Locator, Serializable, delete_nodes, in_document_order, locate};
use crate::doc::{DocView, MutableDocument, NodeId};

/// A value stored in one attribute of an element
/// (`DocumentBasedBasicValue`).
#[derive(Debug)]
pub struct DocumentBasedBasicValue<'a, T> {
    value_attr: &'a str,
    types: PhantomData<fn() -> T>,
}

impl<T> Clone for DocumentBasedBasicValue<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for DocumentBasedBasicValue<'_, T> {}

impl<'a, T: Serializable> DocumentBasedBasicValue<'a, T> {
    pub const fn new(value_attr: &'a str) -> Self {
        DocumentBasedBasicValue {
            value_attr,
            types: PhantomData,
        }
    }

    /// The value; `None` if the attribute is absent or does not parse.
    pub fn get(&self, view: &DocView, element: NodeId) -> Option<T> {
        T::from_attribute(view.attribute(element, self.value_attr)?)
    }

    /// Sets the attribute, or removes it for `None`. Does nothing if the
    /// attribute already has that string.
    pub fn set(
        &self,
        doc: &mut MutableDocument<'_>,
        element: NodeId,
        value: Option<&T>,
    ) -> Result<(), AdtError> {
        let value = value.map(T::to_attribute).transpose()?;
        doc.set_element_attribute(element, self.value_attr, value.as_deref())?;
        Ok(())
    }

    /// An initializer that gives a new element this value
    /// (`createInitialiser`); it does nothing for `None`.
    pub fn create_initialiser(&self, value: Option<&T>) -> impl Initializer + use<T> {
        let attribute = value.map(|value| (self.value_attr.to_owned(), value.to_attribute()));
        move |target: &mut InitialAttributes| match &attribute {
            None => Ok(()),
            Some((name, value)) => initialise_attribute(target, name, &value.clone()?),
        }
    }
}

/// A set of named values stored in the attributes of one element, the names
/// being the attribute names (`DocumentBasedStructuredValue`, whose names
/// are the constants of an enum).
#[derive(Debug)]
pub struct DocumentBasedStructuredValue<K, V> {
    types: PhantomData<fn() -> (K, V)>,
}

impl<K, V> Clone for DocumentBasedStructuredValue<K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> Copy for DocumentBasedStructuredValue<K, V> {}

impl<K, V> Default for DocumentBasedStructuredValue<K, V> {
    fn default() -> Self {
        DocumentBasedStructuredValue { types: PhantomData }
    }
}

impl<K: AsRef<str>, V: Serializable> DocumentBasedStructuredValue<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of a field; `None` if the attribute is absent or does not
    /// parse.
    pub fn get(&self, view: &DocView, element: NodeId, name: &K) -> Option<V> {
        V::from_attribute(view.attribute(element, name.as_ref())?)
    }

    /// Sets one field, or removes it for `None`. Does nothing if the
    /// attribute already has that string.
    pub fn set(
        &self,
        doc: &mut MutableDocument<'_>,
        element: NodeId,
        name: &K,
        value: Option<&V>,
    ) -> Result<(), AdtError> {
        let value = value.map(V::to_attribute).transpose()?;
        doc.set_element_attribute(element, name.as_ref(), value.as_deref())?;
        Ok(())
    }

    /// Sets several fields in one operation, `None` removing a field. The
    /// operation is emitted even if no field changes.
    pub fn set_all(
        &self,
        doc: &mut MutableDocument<'_>,
        element: NodeId,
        values: &[(K, Option<V>)],
    ) -> Result<(), AdtError> {
        let mut strings = Vec::with_capacity(values.len());
        for (name, value) in values {
            strings.push((
                name.as_ref(),
                value.as_ref().map(V::to_attribute).transpose()?,
            ));
        }
        let updates: Vec<(&str, Option<&str>)> = strings
            .iter()
            .map(|(name, value)| (*name, value.as_deref()))
            .collect();
        doc.update_element_attributes(element, &updates)?;
        Ok(())
    }

    /// An initializer that gives a new element these fields
    /// (`createInitialiser`); fields that are `None` are skipped.
    pub fn create_initialiser(&self, values: &[(K, Option<V>)]) -> impl Initializer + use<K, V> {
        let attributes: Vec<(String, Result<String, AdtError>)> = values
            .iter()
            .filter_map(|(name, value)| {
                let value = value.as_ref()?;
                Some((name.as_ref().to_owned(), value.to_attribute()))
            })
            .collect();
        move |target: &mut InitialAttributes| {
            for (name, value) in &attributes {
                initialise_attribute(target, name, &value.clone()?)?;
            }
            Ok(())
        }
    }
}

/// A value that only ever increases, stored as one element per write
/// (`DocumentBasedMonotonicValue`). The element with the greatest value
/// counts; of several, the first in the document.
///
/// ```text
/// <container>
///   <entry value="10"/>
///   <entry value="30"/>      the value is 30
///   <entry value="20"/>
/// </container>
/// ```
///
/// A write appends a new element to the container and then deletes all
/// others.
#[derive(Debug)]
pub struct DocumentBasedMonotonicValue<'a, C> {
    entry_tag: &'a str,
    value_attr: &'a str,
    types: PhantomData<fn() -> C>,
}

impl<C> Clone for DocumentBasedMonotonicValue<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for DocumentBasedMonotonicValue<'_, C> {}

impl<'a, C: Serializable + Ord> DocumentBasedMonotonicValue<'a, C> {
    pub const fn new(entry_tag: &'a str, value_attr: &'a str) -> Self {
        DocumentBasedMonotonicValue {
            entry_tag,
            value_attr,
            types: PhantomData,
        }
    }

    /// The element that counts, with its value, and the other elements.
    fn load(&self, view: &DocView, container: NodeId) -> (Option<(C, NodeId)>, Vec<NodeId>) {
        let mut current: Option<(C, NodeId)> = None;
        let mut obsolete = Vec::new();
        for element in view.child_elements(container) {
            if !view.has_tag(element, self.entry_tag) {
                continue;
            }
            let Some(value) = view
                .attribute(element, self.value_attr)
                .and_then(C::from_attribute)
            else {
                continue;
            };
            match &current {
                Some((known, _)) if *known >= value => obsolete.push(element),
                _ => {
                    obsolete.extend(current.take().map(|(_, node)| node));
                    current = Some((value, element));
                }
            }
        }
        (current, obsolete)
    }

    pub fn get(&self, view: &DocView, container: NodeId) -> Option<C> {
        self.load(view, container).0.map(|(value, _)| value)
    }

    /// Sets the value if it is greater than the current one. Returns
    /// whether the document was changed.
    pub fn set(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        value: &C,
    ) -> Result<bool, AdtError> {
        let node = locate(container, doc.view())?;
        let (current, mut obsolete) = self.load(doc.view(), node);
        if let Some((current, element)) = current {
            if current >= *value {
                return Ok(false);
            }
            obsolete.push(element);
        }
        let value = value.to_attribute()?;
        // The elements to delete all precede the new one, so their
        // locations stay put.
        let locations: Vec<usize> = obsolete
            .iter()
            .map(|&element| doc.view().location(element))
            .collect();
        doc.create_child_element(node, self.entry_tag, &[(self.value_attr, value.as_str())])?;
        let obsolete: Vec<NodeId> = locations
            .into_iter()
            .filter_map(|location| doc.view().element_starting_at(location))
            .collect();
        let obsolete = in_document_order(doc.view(), obsolete);
        delete_nodes(doc, &obsolete)?;
        Ok(true)
    }
}

/// An optional boolean stored as one element per write
/// (`DocumentBasedBoolean`). The last element in the document counts; its
/// value is true unless its attribute is exactly `false`.
///
/// ```text
/// <container>
///   <flag value="false"/>
///   <flag value="true"/>     the value is true
/// </container>
/// ```
///
/// Writing true or false appends a new element and then deletes all others,
/// first to last. Writing `None` deletes all elements, the one that counts
/// last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentBasedBoolean<'a> {
    tag: &'a str,
    value_attr: &'a str,
}

impl<'a> DocumentBasedBoolean<'a> {
    pub const fn new(tag: &'a str, value_attr: &'a str) -> Self {
        DocumentBasedBoolean { tag, value_attr }
    }

    fn elements(&self, view: &DocView, container: NodeId) -> Vec<NodeId> {
        view.child_elements(container)
            .filter(|&element| view.has_tag(element, self.tag))
            .collect()
    }

    pub fn get(&self, view: &DocView, container: NodeId) -> Option<bool> {
        let element = self.elements(view, container).pop()?;
        Some(view.attribute(element, self.value_attr) != Some("false"))
    }

    /// Sets or clears the value; does nothing if it is that already.
    /// Returns whether the document was changed.
    pub fn set(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        value: Option<bool>,
    ) -> Result<bool, AdtError> {
        let node = locate(container, doc.view())?;
        if self.get(doc.view(), node) == value {
            return Ok(false);
        }
        match value {
            Some(value) => {
                let old = self.elements(doc.view(), node);
                let locations: Vec<usize> = old
                    .iter()
                    .map(|&element| doc.view().location(element))
                    .collect();
                let value = if value { "true" } else { "false" };
                doc.create_child_element(node, self.tag, &[(self.value_attr, value)])?;
                let old: Vec<NodeId> = locations
                    .into_iter()
                    .filter_map(|location| doc.view().element_starting_at(location))
                    .collect();
                delete_nodes(doc, &old)?;
            }
            None => {
                let all = self.elements(doc.view(), node);
                delete_nodes(doc, &all)?;
            }
        }
        Ok(true)
    }
}
