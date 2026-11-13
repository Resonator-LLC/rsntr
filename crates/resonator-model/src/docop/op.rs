//! Document operations and their components.

use std::fmt;
use std::ops::Deref;

use super::annotation::AnnotationBoundaryMap;
use super::attributes::{Attributes, AttributesUpdate};
use super::error::OperationError;
use super::validate::{ViolationCollector, is_well_formed};

/// One component of a document operation.
///
/// Counts and string lengths are measured in document items; a character
/// item is one UTF-16 code unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DocOpComponent {
    AnnotationBoundary(AnnotationBoundaryMap),
    Characters(String),
    ElementStart {
        tag: String,
        attributes: Attributes,
    },
    ElementEnd,
    Retain(usize),
    DeleteCharacters(String),
    DeleteElementStart {
        tag: String,
        attributes: Attributes,
    },
    DeleteElementEnd,
    ReplaceAttributes {
        old_attributes: Attributes,
        new_attributes: Attributes,
    },
    UpdateAttributes(AttributesUpdate),
}

impl DocOpComponent {
    /// The component's type name, as in the original `DocOpComponentType`.
    pub fn type_name(&self) -> &'static str {
        match self {
            DocOpComponent::AnnotationBoundary(_) => "annotation boundary",
            DocOpComponent::Characters(_) => "characters",
            DocOpComponent::ElementStart { .. } => "element start",
            DocOpComponent::ElementEnd => "element end",
            DocOpComponent::Retain(_) => "retain",
            DocOpComponent::DeleteCharacters(_) => "delete characters",
            DocOpComponent::DeleteElementStart { .. } => "delete element start",
            DocOpComponent::DeleteElementEnd => "delete element end",
            DocOpComponent::ReplaceAttributes { .. } => "replace attributes",
            DocOpComponent::UpdateAttributes(_) => "update attributes",
        }
    }

    /// Whether the component may appear in a document initialization.
    pub fn is_initialization_component(&self) -> bool {
        matches!(
            self,
            DocOpComponent::AnnotationBoundary(_)
                | DocOpComponent::Characters(_)
                | DocOpComponent::ElementStart { .. }
                | DocOpComponent::ElementEnd
        )
    }
}

/// An immutable document operation: a sequence of components that spans the
/// whole document it applies to.
///
/// Equality is syntactic (component by component).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DocOp {
    components: Vec<DocOpComponent>,
}

impl DocOp {
    /// Builds an operation, checking that it is well-formed.
    pub fn new(components: Vec<DocOpComponent>) -> Result<DocOp, OperationError> {
        let op = DocOp { components };
        if !is_well_formed(None, &op) {
            // Check again, collecting violations this time.
            let mut v = ViolationCollector::new();
            is_well_formed(Some(&mut v), &op);
            return Err(OperationError::IllFormed {
                violation: v.to_string(),
                op: op.to_string(),
            });
        }
        Ok(op)
    }

    /// Builds an operation without checking well-formedness.
    pub fn new_unchecked(components: Vec<DocOpComponent>) -> DocOp {
        DocOp { components }
    }

    pub fn components(&self) -> &[DocOpComponent] {
        &self.components
    }

    pub fn into_components(self) -> Vec<DocOpComponent> {
        self.components
    }

    /// Number of components.
    pub fn len(&self) -> usize {
        self.components.len()
    }

    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, DocOpComponent> {
        self.components.iter()
    }
}

impl<'a> IntoIterator for &'a DocOp {
    type Item = &'a DocOpComponent;
    type IntoIter = std::slice::Iter<'a, DocOpComponent>;

    fn into_iter(self) -> Self::IntoIter {
        self.components.iter()
    }
}

impl fmt::Display for DocOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&super::util::to_concise_string(self))
    }
}

/// A document operation that applies to the empty document: only insertion
/// and annotation boundary components.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DocInitialization(DocOp);

impl DocInitialization {
    /// Wraps an operation known to hold only initialization components.
    pub(crate) fn new_unchecked(op: DocOp) -> Self {
        DocInitialization(op)
    }

    pub fn as_op(&self) -> &DocOp {
        &self.0
    }

    pub fn into_op(self) -> DocOp {
        self.0
    }
}

impl Deref for DocInitialization {
    type Target = DocOp;

    fn deref(&self) -> &DocOp {
        &self.0
    }
}

impl TryFrom<DocOp> for DocInitialization {
    type Error = OperationError;

    /// Fails if the operation contains a retain, deletion or attribute
    /// change. Well-formedness is not checked.
    fn try_from(op: DocOp) -> Result<Self, OperationError> {
        match op.iter().find(|c| !c.is_initialization_component()) {
            Some(c) => Err(OperationError::NotAnInitialization(c.type_name())),
            None => Ok(DocInitialization(op)),
        }
    }
}

impl fmt::Display for DocInitialization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A consumer of operation components (the role of the original's
/// `EvaluatingDocOpCursor`). Normalizers are sinks that wrap another sink.
pub trait OpSink {
    fn push(&mut self, component: DocOpComponent);

    /// Flushes any buffered state and returns the collected components.
    fn finish(self) -> Vec<DocOpComponent>;

    fn annotation_boundary(&mut self, map: AnnotationBoundaryMap) {
        self.push(DocOpComponent::AnnotationBoundary(map));
    }

    fn characters(&mut self, chars: impl Into<String>)
    where
        Self: Sized,
    {
        self.push(DocOpComponent::Characters(chars.into()));
    }

    fn element_start(&mut self, tag: impl Into<String>, attributes: Attributes)
    where
        Self: Sized,
    {
        self.push(DocOpComponent::ElementStart {
            tag: tag.into(),
            attributes,
        });
    }

    fn element_end(&mut self) {
        self.push(DocOpComponent::ElementEnd);
    }

    fn retain(&mut self, item_count: usize) {
        self.push(DocOpComponent::Retain(item_count));
    }

    fn delete_characters(&mut self, chars: impl Into<String>)
    where
        Self: Sized,
    {
        self.push(DocOpComponent::DeleteCharacters(chars.into()));
    }

    fn delete_element_start(&mut self, tag: impl Into<String>, attributes: Attributes)
    where
        Self: Sized,
    {
        self.push(DocOpComponent::DeleteElementStart {
            tag: tag.into(),
            attributes,
        });
    }

    fn delete_element_end(&mut self) {
        self.push(DocOpComponent::DeleteElementEnd);
    }

    fn replace_attributes(&mut self, old_attributes: Attributes, new_attributes: Attributes) {
        self.push(DocOpComponent::ReplaceAttributes {
            old_attributes,
            new_attributes,
        });
    }

    fn update_attributes(&mut self, update: AttributesUpdate) {
        self.push(DocOpComponent::UpdateAttributes(update));
    }
}

impl OpSink for Vec<DocOpComponent> {
    fn push(&mut self, component: DocOpComponent) {
        Vec::push(self, component);
    }

    fn finish(self) -> Vec<DocOpComponent> {
        self
    }
}

/// A chaining builder for [`DocOp`]s.
#[derive(Debug, Clone, Default)]
pub struct DocOpBuilder {
    components: Vec<DocOpComponent>,
}

impl DocOpBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the operation, checking that it is well-formed.
    pub fn build(self) -> Result<DocOp, OperationError> {
        DocOp::new(self.components)
    }

    /// Builds the operation without checking well-formedness.
    pub fn build_unchecked(self) -> DocOp {
        DocOp::new_unchecked(self.components)
    }

    /// Builds a well-formed document initialization.
    pub fn build_initialization(self) -> Result<DocInitialization, OperationError> {
        DocInitialization::try_from(self.build()?)
    }

    pub fn component(mut self, component: DocOpComponent) -> Self {
        self.components.push(component);
        self
    }

    pub fn annotation_boundary(self, map: AnnotationBoundaryMap) -> Self {
        self.component(DocOpComponent::AnnotationBoundary(map))
    }

    pub fn characters(self, chars: impl Into<String>) -> Self {
        self.component(DocOpComponent::Characters(chars.into()))
    }

    pub fn element_start(self, tag: impl Into<String>, attributes: Attributes) -> Self {
        self.component(DocOpComponent::ElementStart {
            tag: tag.into(),
            attributes,
        })
    }

    pub fn element_end(self) -> Self {
        self.component(DocOpComponent::ElementEnd)
    }

    pub fn retain(self, item_count: usize) -> Self {
        self.component(DocOpComponent::Retain(item_count))
    }

    pub fn delete_characters(self, chars: impl Into<String>) -> Self {
        self.component(DocOpComponent::DeleteCharacters(chars.into()))
    }

    pub fn delete_element_start(self, tag: impl Into<String>, attributes: Attributes) -> Self {
        self.component(DocOpComponent::DeleteElementStart {
            tag: tag.into(),
            attributes,
        })
    }

    pub fn delete_element_end(self) -> Self {
        self.component(DocOpComponent::DeleteElementEnd)
    }

    pub fn replace_attributes(
        self,
        old_attributes: Attributes,
        new_attributes: Attributes,
    ) -> Self {
        self.component(DocOpComponent::ReplaceAttributes {
            old_attributes,
            new_attributes,
        })
    }

    pub fn update_attributes(self, update: AttributesUpdate) -> Self {
        self.component(DocOpComponent::UpdateAttributes(update))
    }
}
