//! Annotation boundary maps.

use std::collections::HashSet;
use std::fmt;

use super::error::OperationError;

/// A change of one annotation key, starting at an annotation boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnnotationChange {
    pub key: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

impl AnnotationChange {
    pub fn new(key: impl Into<String>, old_value: Option<&str>, new_value: Option<&str>) -> Self {
        AnnotationChange {
            key: key.into(),
            old_value: old_value.map(str::to_owned),
            new_value: new_value.map(str::to_owned),
        }
    }
}

/// The payload of an annotation boundary component: the keys whose change
/// ends here and the keys whose change starts (or is replaced) here.
///
/// In a well-formed operation both lists are strictly increasing in UTF-16
/// order; that is checked by validation, not by this type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AnnotationBoundaryMap {
    end_keys: Vec<String>,
    changes: Vec<AnnotationChange>,
}

impl AnnotationBoundaryMap {
    /// The boundary that ends and changes nothing.
    pub const fn empty() -> Self {
        AnnotationBoundaryMap {
            end_keys: Vec::new(),
            changes: Vec::new(),
        }
    }

    /// Builds a boundary with the same argument checks as the original
    /// `AnnotationBoundaryMapImpl`: keys unique, non-empty, free of `?` and
    /// `@`, and end keys disjoint from change keys. Order is not checked.
    pub fn new(
        end_keys: Vec<String>,
        changes: Vec<AnnotationChange>,
    ) -> Result<Self, OperationError> {
        let invalid = |message: &str| Err(OperationError::InvalidArgument(message.to_owned()));
        let change_key_set: HashSet<&str> = changes.iter().map(|c| c.key.as_str()).collect();
        let end_key_set: HashSet<&str> = end_keys.iter().map(String::as_str).collect();
        if change_key_set.len() != changes.len() || end_key_set.len() != end_keys.len() {
            return invalid("annotation keys must all be unique");
        }
        if change_key_set.contains("") || end_key_set.contains("") {
            return invalid("empty-string annotation keys are not permitted");
        }
        for key in change_key_set.iter().chain(end_key_set.iter()) {
            validate_annotation_key(key)?;
        }
        if end_key_set.iter().any(|key| change_key_set.contains(key)) {
            return invalid("change keys and end keys must be disjoint sets");
        }
        Ok(AnnotationBoundaryMap { end_keys, changes })
    }

    /// Builds a boundary without any check.
    pub fn new_unchecked(end_keys: Vec<String>, changes: Vec<AnnotationChange>) -> Self {
        AnnotationBoundaryMap { end_keys, changes }
    }

    /// A boundary that only ends the given keys.
    pub fn ends<'a>(keys: impl IntoIterator<Item = &'a str>) -> Result<Self, OperationError> {
        Self::new(keys.into_iter().map(str::to_owned).collect(), Vec::new())
    }

    /// A boundary that only starts the given `(key, old, new)` changes.
    pub fn changes_only<'a>(
        triples: impl IntoIterator<Item = (&'a str, Option<&'a str>, Option<&'a str>)>,
    ) -> Result<Self, OperationError> {
        Self::new(
            Vec::new(),
            triples
                .into_iter()
                .map(|(key, old, new)| AnnotationChange::new(key, old, new))
                .collect(),
        )
    }

    pub fn end_keys(&self) -> &[String] {
        &self.end_keys
    }

    pub fn changes(&self) -> &[AnnotationChange] {
        &self.changes
    }

    pub fn is_empty(&self) -> bool {
        self.end_keys.is_empty() && self.changes.is_empty()
    }

    /// The boundary with old and new values of every change swapped.
    pub fn invert(&self) -> AnnotationBoundaryMap {
        AnnotationBoundaryMap {
            end_keys: self.end_keys.clone(),
            changes: self
                .changes
                .iter()
                .map(|c| AnnotationChange {
                    key: c.key.clone(),
                    old_value: c.new_value.clone(),
                    new_value: c.old_value.clone(),
                })
                .collect(),
        }
    }
}

/// Rejects annotation keys containing `?` or `@`.
pub fn validate_annotation_key(key: &str) -> Result<(), OperationError> {
    if key.contains('?') || key.contains('@') {
        return Err(OperationError::InvalidArgument(
            "annotation keys must not contain the '?' or '@' characters".to_owned(),
        ));
    }
    Ok(())
}

impl fmt::Display for AnnotationBoundaryMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&super::util::annotation_boundary_to_concise_string(self))
    }
}
