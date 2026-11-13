//! A simple document that operations can be applied to: a vector of items,
//! one per UTF-16 code unit, element start or element end, each with its
//! annotations. Easy to understand but not efficient (`BootstrapDocument` in
//! the original).

use std::cmp::Ordering;
use std::fmt;

use super::annotation::{AnnotationBoundaryMap, AnnotationChange};
use super::attributes::{AnnotationMap, AnnotationsUpdate, Attributes};
use super::error::OperationError;
use super::op::{DocInitialization, DocOp, DocOpComponent};
use super::schema::{DocumentSchema, NO_SCHEMA_CONSTRAINTS};
use super::utf16::cmp_utf16;
use super::util::to_xml_string;
use super::validate::{AutomatonDocument, ViolationCollector, validate};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemKind {
    Character(u16),
    ElementStart { tag: String, attributes: Attributes },
    ElementEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    kind: ItemKind,
    annotations: AnnotationMap,
}

/// A document represented as a flat vector of annotated items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootstrapDocument {
    items: Vec<Item>,
    /// For each item, the index of the innermost element start that is open
    /// just before it.
    enclosing: Vec<Option<usize>>,
}

impl BootstrapDocument {
    /// The empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates `op` against this document without schema constraints and
    /// applies it. The document is unchanged on error.
    pub fn consume(&mut self, op: &DocOp) -> Result<(), OperationError> {
        self.consume_with_schema(op, &NO_SCHEMA_CONSTRAINTS)
    }

    /// Validates `op` against this document and `schema` and applies it. The
    /// document is unchanged on error.
    pub fn consume_with_schema(
        &mut self,
        op: &DocOp,
        schema: &dyn DocumentSchema,
    ) -> Result<(), OperationError> {
        let mut v = ViolationCollector::new();
        validate(Some(&mut v), schema, self, op);
        if !v.is_valid() {
            return Err(OperationError::Validation(v.to_string()));
        }
        self.items = self.apply(op)?;
        self.rebuild_index();
        Ok(())
    }

    // The validator makes the error checking here redundant in theory; the
    // original checks anyway in case the validator is wrong, and so do we.
    fn apply(&self, op: &DocOp) -> Result<Vec<Item>, OperationError> {
        let apply_error = |message: String| OperationError::Apply(message);
        let mut result: Vec<Item> = Vec::with_capacity(self.items.len());
        let mut old_items = self.items.iter();
        let mut annotation_updates = AnnotationsUpdate::new();
        // Annotations, before this operation, of the last item passed.
        let mut inherited = AnnotationMap::new();
        let mut next = |what: &str| {
            old_items.next().ok_or_else(|| {
                apply_error(format!(
                    "{what} past end of document, of size: {}",
                    self.items.len()
                ))
            })
        };
        for component in op {
            match component {
                DocOpComponent::AnnotationBoundary(map) => {
                    annotation_updates = annotation_updates.compose_with_boundary(map)?;
                }
                DocOpComponent::Characters(chars) => {
                    let annotations = inherited.update_with(&annotation_updates)?;
                    result.extend(chars.encode_utf16().map(|unit| Item {
                        kind: ItemKind::Character(unit),
                        annotations: annotations.clone(),
                    }));
                }
                DocOpComponent::ElementStart { tag, attributes } => result.push(Item {
                    kind: ItemKind::ElementStart {
                        tag: tag.clone(),
                        attributes: attributes.clone(),
                    },
                    annotations: inherited.update_with(&annotation_updates)?,
                }),
                DocOpComponent::ElementEnd => result.push(Item {
                    kind: ItemKind::ElementEnd,
                    annotations: inherited.update_with(&annotation_updates)?,
                }),
                DocOpComponent::DeleteCharacters(chars) => {
                    for unit in chars.encode_utf16() {
                        let item = next("deletion")?;
                        match item.kind {
                            ItemKind::Character(c) if c == unit => {}
                            ItemKind::Character(_) => {
                                return Err(apply_error(
                                    "mismatched deleted characters".to_owned(),
                                ));
                            }
                            _ => return Err(apply_error("not at a character".to_owned())),
                        }
                        inherited = item.annotations.clone();
                    }
                }
                DocOpComponent::DeleteElementStart { .. } => {
                    let item = next("deletion")?;
                    if !matches!(item.kind, ItemKind::ElementStart { .. }) {
                        return Err(apply_error("not at an element start".to_owned()));
                    }
                    inherited = item.annotations.clone();
                }
                DocOpComponent::DeleteElementEnd => {
                    let item = next("deletion")?;
                    if item.kind != ItemKind::ElementEnd {
                        return Err(apply_error("not at an element end".to_owned()));
                    }
                    inherited = item.annotations.clone();
                }
                DocOpComponent::Retain(distance) => {
                    for _ in 0..*distance {
                        let item = next("retain")?;
                        inherited = item.annotations.clone();
                        result.push(Item {
                            kind: item.kind.clone(),
                            annotations: item.annotations.update_with(&annotation_updates)?,
                        });
                    }
                }
                DocOpComponent::ReplaceAttributes { new_attributes, .. } => {
                    let item = next("attribute change")?;
                    let ItemKind::ElementStart { tag, .. } = &item.kind else {
                        return Err(apply_error("not at an element start".to_owned()));
                    };
                    inherited = item.annotations.clone();
                    result.push(Item {
                        kind: ItemKind::ElementStart {
                            tag: tag.clone(),
                            attributes: new_attributes.clone(),
                        },
                        annotations: item.annotations.update_with(&annotation_updates)?,
                    });
                }
                DocOpComponent::UpdateAttributes(update) => {
                    let item = next("attribute change")?;
                    let ItemKind::ElementStart { tag, attributes } = &item.kind else {
                        return Err(apply_error("not at an element start".to_owned()));
                    };
                    inherited = item.annotations.clone();
                    result.push(Item {
                        kind: ItemKind::ElementStart {
                            tag: tag.clone(),
                            attributes: attributes.update_with(update)?,
                        },
                        annotations: item.annotations.update_with(&annotation_updates)?,
                    });
                }
            }
        }
        let remaining = old_items.count();
        if remaining > 0 {
            return Err(apply_error(format!(
                "missing retain to end of document ({remaining} items)"
            )));
        }
        if !annotation_updates.is_empty() {
            return Err(apply_error(format!(
                "unended annotations at end of operation: {annotation_updates}"
            )));
        }
        Ok(result)
    }

    fn rebuild_index(&mut self) {
        self.enclosing.clear();
        let mut open: Vec<usize> = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            self.enclosing.push(open.last().copied());
            match item.kind {
                ItemKind::ElementStart { .. } => open.push(index),
                ItemKind::ElementEnd => {
                    open.pop();
                }
                ItemKind::Character(_) => {}
            }
        }
    }

    fn tag_at(&self, index: usize) -> Option<&str> {
        match &self.items.get(index)?.kind {
            ItemKind::ElementStart { tag, .. } => Some(tag),
            _ => None,
        }
    }

    /// The document as an operation on the empty document, in normalized
    /// form: an annotation boundary wherever the annotations change.
    pub fn as_operation(&self) -> DocInitialization {
        let mut components = Vec::new();
        let mut text: Vec<u16> = Vec::new();
        fn flush_text(text: &mut Vec<u16>, components: &mut Vec<DocOpComponent>) {
            if !text.is_empty() {
                // Lossless: the document never holds unpaired surrogates.
                components.push(DocOpComponent::Characters(String::from_utf16_lossy(text)));
                text.clear();
            }
        }
        let empty = AnnotationMap::new();
        let mut previous = &empty;
        for item in &self.items {
            if item.annotations != *previous {
                flush_text(&mut text, &mut components);
                components.push(DocOpComponent::AnnotationBoundary(boundary_between(
                    previous,
                    &item.annotations,
                )));
                previous = &item.annotations;
            }
            match &item.kind {
                ItemKind::Character(unit) => text.push(*unit),
                ItemKind::ElementStart { tag, attributes } => {
                    flush_text(&mut text, &mut components);
                    components.push(DocOpComponent::ElementStart {
                        tag: tag.clone(),
                        attributes: attributes.clone(),
                    });
                }
                ItemKind::ElementEnd => {
                    flush_text(&mut text, &mut components);
                    components.push(DocOpComponent::ElementEnd);
                }
            }
        }
        flush_text(&mut text, &mut components);
        if !previous.is_empty() {
            components.push(DocOpComponent::AnnotationBoundary(boundary_between(
                previous, &empty,
            )));
        }
        DocInitialization::new_unchecked(DocOp::new_unchecked(components))
    }

    /// The canonical XML form of the document.
    pub fn to_xml_string(&self) -> String {
        // A document only ever holds balanced elements, so this cannot fail.
        to_xml_string(&self.as_operation()).unwrap_or_default()
    }
}

/// The boundary that turns the open annotation set `from` into `to`, both
/// sorted by key.
fn boundary_between(from: &AnnotationMap, to: &AnnotationMap) -> AnnotationBoundaryMap {
    let mut ends = Vec::new();
    let mut changes = Vec::new();
    let (from, to) = (from.entries(), to.entries());
    let (mut i, mut j) = (0, 0);
    while i < from.len() || j < to.len() {
        let order = match (from.get(i), to.get(j)) {
            (Some(a), Some(b)) => cmp_utf16(&a.name, &b.name),
            (Some(_), None) => Ordering::Less,
            _ => Ordering::Greater,
        };
        match order {
            Ordering::Less => {
                ends.push(from[i].name.clone());
                i += 1;
            }
            Ordering::Greater => {
                changes.push(AnnotationChange::new(
                    &*to[j].name,
                    None,
                    Some(&to[j].value),
                ));
                j += 1;
            }
            Ordering::Equal => {
                if from[i].value != to[j].value {
                    changes.push(AnnotationChange::new(
                        &*to[j].name,
                        None,
                        Some(&to[j].value),
                    ));
                }
                i += 1;
                j += 1;
            }
        }
    }
    AnnotationBoundaryMap::new_unchecked(ends, changes)
}

impl AutomatonDocument for BootstrapDocument {
    fn element_starting_at(&self, pos: usize) -> Option<&str> {
        self.tag_at(pos)
    }

    fn attributes_at(&self, pos: usize) -> Option<&Attributes> {
        match &self.items.get(pos)?.kind {
            ItemKind::ElementStart { attributes, .. } => Some(attributes),
            _ => None,
        }
    }

    fn element_ending_at(&self, pos: usize) -> Option<&str> {
        match self.items.get(pos)?.kind {
            ItemKind::ElementEnd => self.tag_at(self.enclosing[pos]?),
            _ => None,
        }
    }

    fn char_at(&self, pos: usize) -> Option<u16> {
        match self.items.get(pos)?.kind {
            ItemKind::Character(unit) => Some(unit),
            _ => None,
        }
    }

    fn nth_enclosing_element_tag(&self, insertion_point: usize, depth: usize) -> Option<&str> {
        let mut element = *self.enclosing.get(insertion_point)?;
        for _ in 0..depth {
            element = self.enclosing[element?];
        }
        self.tag_at(element?)
    }

    fn remaining_characters_in_element(&self, insertion_point: usize) -> usize {
        self.items
            .iter()
            .skip(insertion_point)
            .take_while(|item| matches!(item.kind, ItemKind::Character(_)))
            .count()
    }

    fn annotations_at(&self, pos: usize) -> AnnotationMap {
        self.items
            .get(pos)
            .map(|item| item.annotations.clone())
            .unwrap_or_default()
    }

    fn get_annotation(&self, pos: usize, key: &str) -> Option<String> {
        self.items.get(pos)?.annotations.get(key).map(str::to_owned)
    }

    fn length(&self) -> usize {
        self.items.len()
    }

    fn first_annotation_change(
        &self,
        start: usize,
        end: usize,
        key: &str,
        from_value: Option<&str>,
    ) -> Option<usize> {
        (start..end.min(self.items.len()))
            .find(|&pos| self.items[pos].annotations.get(key) != from_value)
    }
}

impl fmt::Display for BootstrapDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BootstrapDocument: {}", self.to_xml_string())
    }
}
