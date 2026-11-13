//! Attribute maps, annotation maps and updates to them.
//!
//! Ports `ImmutableStateMap`, `ImmutableUpdateMap`, `AttributesImpl`,
//! `AttributesUpdateImpl`, `AnnotationMapImpl` and `AnnotationsUpdateImpl`.

use std::cmp::Ordering;
use std::fmt;

use super::annotation::AnnotationBoundaryMap;
use super::error::OperationError;
use super::utf16::cmp_utf16;

/// A name-value pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attribute {
    pub name: String,
    pub value: String,
}

impl Attribute {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Attribute {
            name: name.into(),
            value: value.into(),
        }
    }
}

impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Attribute({}={})", self.name, self.value)
    }
}

/// A change of one key from an old value to a new value; `None` means unset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttributeUpdate {
    pub name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

impl AttributeUpdate {
    pub fn new(name: impl Into<String>, old_value: Option<&str>, new_value: Option<&str>) -> Self {
        AttributeUpdate {
            name: name.into(),
            old_value: old_value.map(str::to_owned),
            new_value: new_value.map(str::to_owned),
        }
    }
}

impl fmt::Display for AttributeUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}: {} -> {}]",
            self.name,
            self.old_value.as_deref().unwrap_or("null"),
            self.new_value.as_deref().unwrap_or("null")
        )
    }
}

/// An immutable map from names to values, kept as a list sorted by name
/// (UTF-16 order).
///
/// The unchecked constructors can produce unsorted or duplicate entries; such
/// maps are rejected by validation, not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Attributes {
    entries: Vec<Attribute>,
}

/// The annotations of a document item. Same representation as [`Attributes`].
pub type AnnotationMap = Attributes;

impl Attributes {
    /// The empty map.
    pub const fn new() -> Self {
        Attributes {
            entries: Vec::new(),
        }
    }

    /// Builds a map from name-value pairs in any order. Fails on duplicate
    /// names.
    pub fn from_pairs<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Result<Self, OperationError>
    where
        K: Into<String>,
        V: Into<String>,
    {
        Self::from_unsorted(
            pairs
                .into_iter()
                .map(|(k, v)| Attribute::new(k, v))
                .collect(),
        )
    }

    /// Uses the entries as given, after checking that names are strictly
    /// increasing.
    pub fn from_sorted(entries: Vec<Attribute>) -> Result<Self, OperationError> {
        check_attributes_sorted(&entries)?;
        Ok(Attributes { entries })
    }

    /// Uses the entries as given, without any check.
    pub fn from_sorted_unchecked(entries: Vec<Attribute>) -> Self {
        Attributes { entries }
    }

    /// Sorts the entries, failing on duplicate names.
    pub fn from_unsorted(mut entries: Vec<Attribute>) -> Result<Self, OperationError> {
        entries.sort_by(|a, b| cmp_utf16(&a.name, &b.name));
        Self::from_sorted(entries)
    }

    /// Sorts the entries but does not check for duplicate names.
    pub fn from_unsorted_unchecked(mut entries: Vec<Attribute>) -> Self {
        entries.sort_by(|a, b| cmp_utf16(&a.name, &b.name));
        Attributes { entries }
    }

    pub fn entries(&self) -> &[Attribute] {
        &self.entries
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|a| (a.name.as_str(), a.value.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Value of the first entry with the given name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.as_str())
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Applies an update, checking that its old values match this map.
    pub fn update_with(&self, update: &AttributesUpdate) -> Result<Attributes, OperationError> {
        self.update_impl(update, true)
    }

    /// Applies an update, ignoring its old values.
    pub fn update_with_no_compatibility_check(&self, update: &AttributesUpdate) -> Attributes {
        match self.update_impl(update, false) {
            Ok(attributes) => attributes,
            // Unreachable: errors are only raised by the compatibility check.
            Err(_) => self.clone(),
        }
    }

    // Line-by-line port of ImmutableStateMap.updateWith. Note the quirk that
    // an update whose key sorts after every existing entry is never checked
    // against its old value.
    fn update_impl(
        &self,
        update: &AttributesUpdate,
        check_compatibility: bool,
    ) -> Result<Attributes, OperationError> {
        let mut result = Vec::new();
        let mut iterator = self.entries.iter();
        let mut next_attribute = iterator.next();
        for update in &update.updates {
            while let Some(attribute) = next_attribute {
                match cmp_utf16(&update.name, &attribute.name) {
                    Ordering::Greater => {
                        result.push(attribute.clone());
                        next_attribute = iterator.next();
                    }
                    Ordering::Less => {
                        if check_compatibility && update.old_value.is_some() {
                            return Err(OperationError::MismatchedOldValue(format!(
                                "attempt to update unset attribute with {update}"
                            )));
                        }
                        break;
                    }
                    Ordering::Equal => {
                        if check_compatibility
                            && update.old_value.as_deref() != Some(attribute.value.as_str())
                        {
                            return Err(OperationError::MismatchedOldValue(format!(
                                "attempt to update {attribute} with {update}"
                            )));
                        }
                        next_attribute = iterator.next();
                        break;
                    }
                }
            }
            if let Some(new_value) = &update.new_value {
                result.push(Attribute::new(update.name.clone(), new_value.clone()));
            }
        }
        if let Some(attribute) = next_attribute {
            result.push(attribute.clone());
            result.extend(iterator.cloned());
        }
        Ok(Attributes { entries: result })
    }
}

/// Checks that names are strictly increasing in UTF-16 order.
pub fn check_attributes_sorted(entries: &[Attribute]) -> Result<(), OperationError> {
    for pair in entries.windows(2) {
        if cmp_utf16(&pair[0].name, &pair[1].name) != Ordering::Less {
            return Err(OperationError::InvalidArgument(format!(
                "attribute keys not strictly monotonic: {}, {}",
                pair[0].name, pair[1].name
            )));
        }
    }
    Ok(())
}

/// Checks that names are strictly increasing in UTF-16 order.
pub fn check_updates_sorted(updates: &[AttributeUpdate]) -> Result<(), OperationError> {
    for pair in updates.windows(2) {
        if cmp_utf16(&pair[0].name, &pair[1].name) != Ordering::Less {
            return Err(OperationError::InvalidArgument(format!(
                "attribute keys not strictly monotonic: {}, {}",
                pair[0].name, pair[1].name
            )));
        }
    }
    Ok(())
}

/// An immutable set of per-key changes, kept as a list sorted by name
/// (UTF-16 order).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AttributesUpdate {
    updates: Vec<AttributeUpdate>,
}

/// The annotation changes in effect at a point in an operation. Same
/// representation as [`AttributesUpdate`].
pub type AnnotationsUpdate = AttributesUpdate;

impl AttributesUpdate {
    /// The empty update.
    pub const fn new() -> Self {
        AttributesUpdate {
            updates: Vec::new(),
        }
    }

    /// An update of a single key.
    pub fn single(
        name: impl Into<String>,
        old_value: Option<&str>,
        new_value: Option<&str>,
    ) -> Self {
        AttributesUpdate {
            updates: vec![AttributeUpdate::new(name, old_value, new_value)],
        }
    }

    /// Builds an update from `(name, old, new)` triples in any order. Fails
    /// on duplicate names.
    pub fn from_triples<'a>(
        triples: impl IntoIterator<Item = (&'a str, Option<&'a str>, Option<&'a str>)>,
    ) -> Result<Self, OperationError> {
        let mut updates: Vec<AttributeUpdate> = triples
            .into_iter()
            .map(|(name, old, new)| AttributeUpdate::new(name, old, new))
            .collect();
        updates.sort_by(|a, b| cmp_utf16(&a.name, &b.name));
        for pair in updates.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(OperationError::InvalidArgument(format!(
                    "duplicate key: {}",
                    pair[1].name
                )));
            }
        }
        Ok(AttributesUpdate { updates })
    }

    /// Uses the updates as given, after checking that names are strictly
    /// increasing.
    pub fn from_sorted(updates: Vec<AttributeUpdate>) -> Result<Self, OperationError> {
        check_updates_sorted(&updates)?;
        Ok(AttributesUpdate { updates })
    }

    /// Uses the updates as given, without any check.
    pub fn from_sorted_unchecked(updates: Vec<AttributeUpdate>) -> Self {
        AttributesUpdate { updates }
    }

    /// Sorts the updates, failing on duplicate names.
    pub fn from_unsorted(mut updates: Vec<AttributeUpdate>) -> Result<Self, OperationError> {
        updates.sort_by(|a, b| cmp_utf16(&a.name, &b.name));
        Self::from_sorted(updates)
    }

    /// Sorts the updates but does not check for duplicate names.
    pub fn from_unsorted_unchecked(mut updates: Vec<AttributeUpdate>) -> Self {
        updates.sort_by(|a, b| cmp_utf16(&a.name, &b.name));
        AttributesUpdate { updates }
    }

    pub fn updates(&self) -> &[AttributeUpdate] {
        &self.updates
    }

    pub fn len(&self) -> usize {
        self.updates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.updates.iter().any(|u| u.name == name)
    }

    /// Drops the changes whose key satisfies `excluded`.
    pub fn exclude(&self, excluded: impl Fn(&str) -> bool) -> AttributesUpdate {
        AttributesUpdate {
            updates: self
                .updates
                .iter()
                .filter(|u| !excluded(&u.name))
                .cloned()
                .collect(),
        }
    }

    /// The update equivalent to applying `self` and then `mutation`.
    ///
    /// Line-by-line port of ImmutableUpdateMap.composeWith: old values are
    /// only checked for keys present in both updates.
    pub fn compose_with(
        &self,
        mutation: &AttributesUpdate,
    ) -> Result<AttributesUpdate, OperationError> {
        let mut result = Vec::new();
        let mut iterator = self.updates.iter();
        let mut next_attribute = iterator.next();
        'mutation: for attribute in &mutation.updates {
            while let Some(existing) = next_attribute {
                match cmp_utf16(&attribute.name, &existing.name) {
                    Ordering::Less => break,
                    Ordering::Greater => {
                        result.push(existing.clone());
                        next_attribute = iterator.next();
                    }
                    Ordering::Equal => {
                        if existing.new_value != attribute.old_value {
                            return Err(OperationError::MismatchedOldValue(format!(
                                "attempt to update {existing} with {attribute}"
                            )));
                        }
                        result.push(AttributeUpdate {
                            name: attribute.name.clone(),
                            old_value: existing.old_value.clone(),
                            new_value: attribute.new_value.clone(),
                        });
                        next_attribute = iterator.next();
                        continue 'mutation;
                    }
                }
            }
            result.push(attribute.clone());
        }
        if let Some(existing) = next_attribute {
            result.push(existing.clone());
            result.extend(iterator.cloned());
        }
        Ok(AttributesUpdate { updates: result })
    }

    /// The update with old and new values swapped.
    ///
    /// Built, like the original, by composing single-key updates one by one.
    pub fn invert(&self) -> Result<AttributesUpdate, OperationError> {
        let mut inverse = AttributesUpdate::new();
        for u in &self.updates {
            inverse = inverse.compose_with(&AttributesUpdate {
                updates: vec![AttributeUpdate {
                    name: u.name.clone(),
                    old_value: u.new_value.clone(),
                    new_value: u.old_value.clone(),
                }],
            })?;
        }
        Ok(inverse)
    }

    /// Folds an annotation boundary into the set of open annotation changes
    /// (`AnnotationsUpdateImpl.composeWith`). End keys must be open and the
    /// boundary's keys sorted.
    pub fn compose_with_boundary(
        &self,
        map: &AnnotationBoundaryMap,
    ) -> Result<AnnotationsUpdate, OperationError> {
        // An exhausted list compares greater than any key (the original uses
        // the sentinel "￿").
        fn cmp_keys(a: Option<&str>, b: Option<&str>) -> Ordering {
            match (a, b) {
                (Some(a), Some(b)) => cmp_utf16(a, b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        let changes = map.changes();
        let ends = map.end_keys();
        let mut result = Vec::new();
        let (mut existing_index, mut change_index, mut end_index) = (0, 0, 0);
        while existing_index < self.updates.len()
            || change_index < changes.len()
            || end_index < ends.len()
        {
            let existing_key = self.updates.get(existing_index).map(|u| u.name.as_str());
            let change = changes.get(change_index);
            let change_key = change.map(|c| c.key.as_str());
            let end_key = ends.get(end_index).map(String::as_str);
            let new_update = || {
                change.map(|c| AttributeUpdate {
                    name: c.key.clone(),
                    old_value: c.old_value.clone(),
                    new_value: c.new_value.clone(),
                })
            };
            let existing_vs_change = cmp_keys(existing_key, change_key);
            match cmp_keys(existing_key, end_key) {
                Ordering::Less => match existing_vs_change {
                    Ordering::Less => {
                        result.push(self.updates[existing_index].clone());
                        existing_index += 1;
                    }
                    Ordering::Equal => {
                        result.extend(new_update());
                        existing_index += 1;
                        change_index += 1;
                    }
                    Ordering::Greater => {
                        result.extend(new_update());
                        change_index += 1;
                    }
                },
                Ordering::Equal => match existing_vs_change {
                    Ordering::Less => {
                        existing_index += 1;
                        end_index += 1;
                    }
                    Ordering::Equal => {
                        return Err(OperationError::InvalidArgument(format!(
                            "annotation boundary with key both in change and end: {}",
                            change_key.unwrap_or_default()
                        )));
                    }
                    Ordering::Greater => {
                        result.extend(new_update());
                        existing_index += 1;
                        end_index += 1;
                        change_index += 1;
                    }
                },
                Ordering::Greater => {
                    return Err(OperationError::InvalidArgument(format!(
                        "attempt to end key that is not part of the update: {}",
                        end_key.unwrap_or_default()
                    )));
                }
            }
        }
        Ok(AttributesUpdate { updates: result })
    }
}

impl fmt::Display for AttributesUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Updates: [")?;
        for (i, u) in self.updates.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{u}")?;
        }
        write!(f, "]")
    }
}
