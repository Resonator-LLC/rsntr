//! Maps stored as one element per entry (`AbstractDocumentBasedMap`).

use std::marker::PhantomData;

use super::{AdtError, Locator, Serializable, delete_nodes, in_document_order, locate};
use crate::doc::{DocView, MutableDocument, NodeId};

/// What a container holds for a map: the entry element that counts for each
/// key, and the entry elements that are superseded.
struct Loaded<K, V> {
    entries: Vec<(K, V, NodeId)>,
    obsolete: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spec<'a> {
    entry_tag: &'a str,
    key_attr: &'a str,
    value_attr: &'a str,
}

impl Spec<'_> {
    /// Replays the original's load: the entries in document order, each
    /// either taking the place of the entry known for its key or becoming
    /// obsolete.
    fn load<K: Serializable + PartialEq, V: Serializable>(
        &self,
        view: &DocView,
        container: NodeId,
        can_replace: impl Fn(&V, &V) -> bool,
    ) -> Loaded<K, V> {
        let mut loaded = Loaded {
            entries: Vec::new(),
            obsolete: Vec::new(),
        };
        for element in view.child_elements(container) {
            if !view.has_tag(element, self.entry_tag) {
                continue;
            }
            let key = view.attribute(element, self.key_attr);
            let value = view.attribute(element, self.value_attr);
            let (Some(key), Some(value)) = (
                key.and_then(K::from_attribute),
                value.and_then(V::from_attribute),
            ) else {
                continue;
            };
            match loaded.entries.iter_mut().find(|(k, _, _)| *k == key) {
                None => loaded.entries.push((key, value, element)),
                Some(known) if can_replace(&known.1, &value) => {
                    loaded.obsolete.push(known.2);
                    *known = (key, value, element);
                }
                Some(_) => loaded.obsolete.push(element),
            }
        }
        loaded
    }

    fn get<K: Serializable + PartialEq, V: Serializable>(
        &self,
        view: &DocView,
        container: NodeId,
        key: &K,
        can_replace: impl Fn(&V, &V) -> bool,
    ) -> Option<V> {
        self.load::<K, V>(view, container, can_replace)
            .entries
            .into_iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, value, _)| value)
    }

    fn entries<K: Serializable + Ord, V: Serializable>(
        &self,
        view: &DocView,
        container: NodeId,
        can_replace: impl Fn(&V, &V) -> bool,
    ) -> Vec<(K, V)> {
        let mut entries: Vec<(K, V)> = self
            .load::<K, V>(view, container, can_replace)
            .entries
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// `remove`: deletes the entry of the key, if any, and every obsolete
    /// entry of the map.
    fn remove<K: Serializable + PartialEq, V: Serializable>(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        key: &K,
        can_replace: impl Fn(&V, &V) -> bool,
    ) -> Result<(), AdtError> {
        let node = locate(container, doc.view())?;
        let loaded = self.load::<K, V>(doc.view(), node, can_replace);
        let mut doomed = loaded.obsolete;
        doomed.extend(
            loaded
                .entries
                .iter()
                .filter(|(k, _, _)| k == key)
                .map(|&(_, _, element)| element),
        );
        let doomed = in_document_order(doc.view(), doomed);
        delete_nodes(doc, &doomed)
    }

    /// `put`: unless redundant, removes the key and then inserts a new
    /// entry as the first child of the container.
    fn put<K: Serializable + PartialEq, V: Serializable>(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        key: &K,
        value: &V,
        can_replace: impl Fn(&V, &V) -> bool + Copy,
        is_redundant: impl Fn(&V, &V) -> bool,
    ) -> Result<bool, AdtError> {
        let node = locate(container, doc.view())?;
        let current = self.get::<K, V>(doc.view(), node, key, can_replace);
        if current.is_some_and(|current| is_redundant(&current, value)) {
            return Ok(false);
        }
        let key_string = key.to_attribute()?;
        let value_string = value.to_attribute()?;
        self.remove::<K, V>(doc, container, key, can_replace)?;
        let node = locate(container, doc.view())?;
        let start = doc.view().content_start(node);
        doc.create_element(
            start,
            self.entry_tag,
            &[
                (self.key_attr, key_string.as_str()),
                (self.value_attr, value_string.as_str()),
            ],
        )?;
        Ok(true)
    }
}

/// A map in which the entry element that comes last in the document counts
/// (`DocumentBasedBasicMap`).
///
/// ```text
/// <container>
///   <entry key="a" value="2"/>
///   <entry key="b" value="7"/>
///   <entry key="a" value="1"/>     the value of "a" is 1
/// </container>
/// ```
///
/// A new entry is inserted as the first child of the container, after the
/// entry it replaces and all superseded entries have been deleted.
#[derive(Debug)]
pub struct DocumentBasedBasicMap<'a, K, V> {
    spec: Spec<'a>,
    types: PhantomData<fn() -> (K, V)>,
}

impl<K, V> Clone for DocumentBasedBasicMap<'_, K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> Copy for DocumentBasedBasicMap<'_, K, V> {}

impl<'a, K: Serializable + Ord, V: Serializable + PartialEq> DocumentBasedBasicMap<'a, K, V> {
    pub const fn new(entry_tag: &'a str, key_attr: &'a str, value_attr: &'a str) -> Self {
        DocumentBasedBasicMap {
            spec: Spec {
                entry_tag,
                key_attr,
                value_attr,
            },
            types: PhantomData,
        }
    }

    // A later entry always replaces an earlier one.
    fn can_replace(_: &V, _: &V) -> bool {
        true
    }

    /// The value of a key.
    pub fn get(&self, view: &DocView, container: NodeId, key: &K) -> Option<V> {
        self.spec.get(view, container, key, Self::can_replace)
    }

    /// The entries, ordered by key.
    pub fn entries(&self, view: &DocView, container: NodeId) -> Vec<(K, V)> {
        self.spec.entries(view, container, Self::can_replace)
    }

    /// The keys, in order (`keySet`).
    pub fn key_set(&self, view: &DocView, container: NodeId) -> Vec<K> {
        let entries = self.entries(view, container);
        entries.into_iter().map(|(key, _)| key).collect()
    }

    /// Sets the value of a key, unless it has that value already. Returns
    /// whether the document was changed.
    pub fn put(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        key: &K,
        value: &V,
    ) -> Result<bool, AdtError> {
        let redundant = |old: &V, new: &V| old == new;
        self.spec
            .put(doc, container, key, value, Self::can_replace, redundant)
    }

    /// Removes a key. Also deletes all superseded entries, whether or not
    /// the key is present.
    pub fn remove(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        key: &K,
    ) -> Result<(), AdtError> {
        self.spec
            .remove::<K, V>(doc, container, key, Self::can_replace)
    }
}

/// A map in which the greatest value of a key counts, so that values only
/// ever increase (`DocumentBasedMonotonicMap`). Of several entries with the
/// greatest value, the first in the document counts.
///
/// A new entry is inserted as the first child of the container, after the
/// entry it replaces and all superseded entries have been deleted.
#[derive(Debug)]
pub struct DocumentBasedMonotonicMap<'a, K, V> {
    spec: Spec<'a>,
    types: PhantomData<fn() -> (K, V)>,
}

impl<K, V> Clone for DocumentBasedMonotonicMap<'_, K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> Copy for DocumentBasedMonotonicMap<'_, K, V> {}

impl<'a, K: Serializable + Ord, V: Serializable + Ord> DocumentBasedMonotonicMap<'a, K, V> {
    pub const fn new(entry_tag: &'a str, key_attr: &'a str, value_attr: &'a str) -> Self {
        DocumentBasedMonotonicMap {
            spec: Spec {
                entry_tag,
                key_attr,
                value_attr,
            },
            types: PhantomData,
        }
    }

    fn can_replace(old: &V, new: &V) -> bool {
        old < new
    }

    /// The value of a key.
    pub fn get(&self, view: &DocView, container: NodeId, key: &K) -> Option<V> {
        self.spec.get(view, container, key, Self::can_replace)
    }

    /// The entries, ordered by key.
    pub fn entries(&self, view: &DocView, container: NodeId) -> Vec<(K, V)> {
        self.spec.entries(view, container, Self::can_replace)
    }

    /// The keys, in order (`keySet`).
    pub fn key_set(&self, view: &DocView, container: NodeId) -> Vec<K> {
        let entries = self.entries(view, container);
        entries.into_iter().map(|(key, _)| key).collect()
    }

    /// Sets the value of a key if it is greater than the current one.
    /// Returns whether the document was changed.
    pub fn put(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        key: &K,
        value: &V,
    ) -> Result<bool, AdtError> {
        let redundant = |old: &V, new: &V| old >= new;
        self.spec
            .put(doc, container, key, value, Self::can_replace, redundant)
    }

    /// Removes a key, after which a lesser value can be put. Also deletes
    /// all superseded entries, whether or not the key is present.
    pub fn remove(
        &self,
        doc: &mut MutableDocument<'_>,
        container: &dyn Locator,
        key: &K,
    ) -> Result<(), AdtError> {
        self.spec
            .remove::<K, V>(doc, container, key, Self::can_replace)
    }
}
