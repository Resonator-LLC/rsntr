//! Composition of document operations (`Composer` and `DocOpCollector`).

use std::collections::HashMap;

use super::annotation::{AnnotationBoundaryMap, AnnotationChange};
use super::attributes::{Attributes, AttributesUpdate};
use super::error::OperationError;
use super::normalize::{OpNormalizer, new_normalizer};
use super::op::{DocInitialization, DocOp, DocOpComponent, OpSink};
use super::utf16::{split_at_utf16, utf16_len};
use super::util::{initial_document_length, resulting_document_length};

/// The old and new value of an annotation change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValueUpdate {
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

/// What the composer is waiting for.
///
/// `*Pre` states consume components of the first operation; `*Post` states
/// hold a pending component of the first operation (or the tail of one) and
/// consume components of the second.
enum Target {
    DefaultPre,
    /// The second operation retains this many more items.
    RetainPre(usize),
    /// The second operation deletes these characters.
    DeleteCharactersPre(String),
    RetainPost(usize),
    CharactersPost(String),
    ElementStartPost {
        tag: String,
        attributes: Attributes,
    },
    ElementEndPost,
    ReplaceAttributesPost {
        old_attributes: Attributes,
        new_attributes: Attributes,
    },
    UpdateAttributesPost(AttributesUpdate),
    /// The first operation is exhausted; only insertions may follow.
    FinisherPost,
}

impl Target {
    fn is_post_target(&self) -> bool {
        !matches!(
            self,
            Target::DefaultPre | Target::RetainPre(_) | Target::DeleteCharactersPre(_)
        )
    }
}

struct Composer {
    normalizer: OpNormalizer,
    pre_annotations: HashMap<String, ValueUpdate>,
    post_annotations: HashMap<String, ValueUpdate>,
    pre_annotation_queue: Vec<AnnotationBoundaryMap>,
    post_annotation_queue: Vec<AnnotationBoundaryMap>,
    target: Target,
}

impl Composer {
    fn new() -> Self {
        Composer {
            normalizer: new_normalizer(),
            pre_annotations: HashMap::new(),
            post_annotations: HashMap::new(),
            pre_annotation_queue: Vec::new(),
            post_annotation_queue: Vec::new(),
            target: Target::DefaultPre,
        }
    }

    fn flush_pre_annotations(&mut self) -> Result<(), OperationError> {
        for map in std::mem::take(&mut self.pre_annotation_queue) {
            let mut end_keys = Vec::new();
            let mut changes = Vec::new();
            for key in map.end_keys() {
                match self.post_annotations.get(key) {
                    Some(post_values) => changes.push(AnnotationChange {
                        key: key.clone(),
                        old_value: post_values.old_value.clone(),
                        new_value: post_values.new_value.clone(),
                    }),
                    None => end_keys.push(key.clone()),
                }
                self.pre_annotations.remove(key);
            }
            for change in map.changes() {
                let new_value = match self.post_annotations.get(&change.key) {
                    Some(post_values) => post_values.new_value.clone(),
                    None => change.new_value.clone(),
                };
                changes.push(AnnotationChange {
                    key: change.key.clone(),
                    old_value: change.old_value.clone(),
                    new_value,
                });
                self.pre_annotations.insert(
                    change.key.clone(),
                    ValueUpdate {
                        old_value: change.old_value.clone(),
                        new_value: change.new_value.clone(),
                    },
                );
            }
            self.normalizer
                .annotation_boundary(AnnotationBoundaryMap::new(end_keys, changes)?);
        }
        Ok(())
    }

    fn flush_post_annotations(&mut self) -> Result<(), OperationError> {
        for map in std::mem::take(&mut self.post_annotation_queue) {
            let mut end_keys = Vec::new();
            let mut changes = Vec::new();
            for key in map.end_keys() {
                match self.pre_annotations.get(key) {
                    Some(pre_values) => changes.push(AnnotationChange {
                        key: key.clone(),
                        old_value: pre_values.old_value.clone(),
                        new_value: pre_values.new_value.clone(),
                    }),
                    None => end_keys.push(key.clone()),
                }
                self.post_annotations.remove(key);
            }
            for change in map.changes() {
                let old_value = match self.pre_annotations.get(&change.key) {
                    Some(pre_values) => pre_values.old_value.clone(),
                    None => change.old_value.clone(),
                };
                changes.push(AnnotationChange {
                    key: change.key.clone(),
                    old_value,
                    new_value: change.new_value.clone(),
                });
                self.post_annotations.insert(
                    change.key.clone(),
                    ValueUpdate {
                        old_value: change.old_value.clone(),
                        new_value: change.new_value.clone(),
                    },
                );
            }
            self.normalizer
                .annotation_boundary(AnnotationBoundaryMap::new(end_keys, changes)?);
        }
        Ok(())
    }

    fn flush_annotations(&mut self) -> Result<(), OperationError> {
        self.flush_pre_annotations()?;
        self.flush_post_annotations()
    }

    /// The state after `size` items of a pending retain of `item_count` have
    /// been matched.
    fn cancel_retain(item_count: usize, size: usize, rest: fn(usize) -> Target) -> Target {
        if size < item_count {
            rest(item_count - size)
        } else {
            Target::DefaultPre
        }
    }

    /// The state after `size` code units of pending `chars` have been
    /// matched.
    fn cancel_characters(
        chars: &str,
        size: usize,
        rest: fn(String) -> Target,
    ) -> Result<Target, OperationError> {
        if size < utf16_len(chars) {
            Ok(rest(split_at_utf16(chars, size)?.1.to_owned()))
        } else {
            Ok(Target::DefaultPre)
        }
    }

    /// The state after a pending single-item component met a retain.
    fn after_single_item(item_count: usize) -> Target {
        if item_count > 1 {
            Target::RetainPre(item_count - 1)
        } else {
            Target::DefaultPre
        }
    }

    /// Feeds a component of the first operation; the current target is a
    /// `*Pre` state.
    fn apply_pre(&mut self, component: &DocOpComponent) -> Result<(), OperationError> {
        use DocOpComponent as C;
        // Handled the same way by every pre target.
        match component {
            C::DeleteCharacters(_) | C::DeleteElementStart { .. } | C::DeleteElementEnd => {
                self.flush_pre_annotations()?;
                self.normalizer.push(component.clone());
                return Ok(());
            }
            C::AnnotationBoundary(map) => {
                self.pre_annotation_queue.push(map.clone());
                return Ok(());
            }
            _ => {}
        }
        match std::mem::replace(&mut self.target, Target::DefaultPre) {
            Target::DefaultPre => {
                self.target = match component {
                    C::Retain(item_count) => Target::RetainPost(*item_count),
                    C::Characters(chars) => Target::CharactersPost(chars.clone()),
                    C::ElementStart { tag, attributes } => Target::ElementStartPost {
                        tag: tag.clone(),
                        attributes: attributes.clone(),
                    },
                    C::ElementEnd => Target::ElementEndPost,
                    C::ReplaceAttributes {
                        old_attributes,
                        new_attributes,
                    } => Target::ReplaceAttributesPost {
                        old_attributes: old_attributes.clone(),
                        new_attributes: new_attributes.clone(),
                    },
                    C::UpdateAttributes(update) => Target::UpdateAttributesPost(update.clone()),
                    _ => Target::DefaultPre,
                };
            }
            Target::RetainPre(pending) => {
                self.flush_annotations()?;
                self.target = match component {
                    C::Retain(item_count) => {
                        if *item_count <= pending {
                            self.normalizer.retain(*item_count);
                            Self::cancel_retain(pending, *item_count, Target::RetainPre)
                        } else {
                            self.normalizer.retain(pending);
                            Target::RetainPost(item_count - pending)
                        }
                    }
                    C::Characters(chars) => {
                        let length = utf16_len(chars);
                        if length <= pending {
                            self.normalizer.characters(chars.as_str());
                            Self::cancel_retain(pending, length, Target::RetainPre)
                        } else {
                            let (head, tail) = split_at_utf16(chars, pending)?;
                            self.normalizer.characters(head);
                            Target::CharactersPost(tail.to_owned())
                        }
                    }
                    _ => {
                        // Element start, element end or attribute change.
                        self.normalizer.push(component.clone());
                        Self::cancel_retain(pending, 1, Target::RetainPre)
                    }
                };
            }
            Target::DeleteCharactersPre(pending) => {
                let pending_length = utf16_len(&pending);
                match component {
                    C::Retain(item_count) => {
                        self.flush_annotations()?;
                        if *item_count <= pending_length {
                            let (head, _) = split_at_utf16(&pending, *item_count)?;
                            self.normalizer.delete_characters(head);
                            self.target = Self::cancel_characters(
                                &pending,
                                *item_count,
                                Target::DeleteCharactersPre,
                            )?;
                        } else {
                            self.normalizer.delete_characters(pending);
                            self.target = Target::RetainPost(item_count - pending_length);
                        }
                    }
                    C::Characters(chars) => {
                        self.flush_annotations()?;
                        // As in the original, the inserted and deleted
                        // characters are not compared.
                        let length = utf16_len(chars);
                        self.target = if length <= pending_length {
                            Self::cancel_characters(&pending, length, Target::DeleteCharactersPre)?
                        } else {
                            Target::CharactersPost(
                                split_at_utf16(chars, pending_length)?.1.to_owned(),
                            )
                        };
                    }
                    _ => return Err(OperationError::IllegalComposition),
                }
            }
            // Unreachable: apply_pre is only called in a pre state.
            _ => return Err(OperationError::IllegalComposition),
        }
        Ok(())
    }

    /// Feeds a component of the second operation; the current target is a
    /// `*Post` state.
    fn apply_post(&mut self, component: &DocOpComponent) -> Result<(), OperationError> {
        use DocOpComponent as C;
        // Handled the same way by every post target.
        match component {
            C::Characters(_) | C::ElementStart { .. } | C::ElementEnd => {
                self.flush_post_annotations()?;
                self.normalizer.push(component.clone());
                return Ok(());
            }
            C::AnnotationBoundary(map) => {
                self.post_annotation_queue.push(map.clone());
                return Ok(());
            }
            _ => {}
        }
        match std::mem::replace(&mut self.target, Target::DefaultPre) {
            Target::RetainPost(pending) => {
                self.flush_annotations()?;
                self.target = match component {
                    C::Retain(item_count) => {
                        if *item_count <= pending {
                            self.normalizer.retain(*item_count);
                            Self::cancel_retain(pending, *item_count, Target::RetainPost)
                        } else {
                            self.normalizer.retain(pending);
                            Target::RetainPre(item_count - pending)
                        }
                    }
                    C::DeleteCharacters(chars) => {
                        let length = utf16_len(chars);
                        if length <= pending {
                            self.normalizer.delete_characters(chars.as_str());
                            Self::cancel_retain(pending, length, Target::RetainPost)
                        } else {
                            let (head, tail) = split_at_utf16(chars, pending)?;
                            self.normalizer.delete_characters(head);
                            Target::DeleteCharactersPre(tail.to_owned())
                        }
                    }
                    _ => {
                        // Element deletion or attribute change.
                        self.normalizer.push(component.clone());
                        Self::cancel_retain(pending, 1, Target::RetainPost)
                    }
                };
            }
            Target::CharactersPost(pending) => {
                let pending_length = utf16_len(&pending);
                match component {
                    C::Retain(item_count) => {
                        self.flush_annotations()?;
                        if *item_count <= pending_length {
                            let (head, _) = split_at_utf16(&pending, *item_count)?;
                            self.normalizer.characters(head);
                            self.target = Self::cancel_characters(
                                &pending,
                                *item_count,
                                Target::CharactersPost,
                            )?;
                        } else {
                            self.normalizer.characters(pending);
                            self.target = Target::RetainPre(item_count - pending_length);
                        }
                    }
                    C::DeleteCharacters(chars) => {
                        self.flush_annotations()?;
                        // As in the original, the inserted and deleted
                        // characters are not compared.
                        let length = utf16_len(chars);
                        self.target = if length <= pending_length {
                            Self::cancel_characters(&pending, length, Target::CharactersPost)?
                        } else {
                            Target::DeleteCharactersPre(
                                split_at_utf16(chars, pending_length)?.1.to_owned(),
                            )
                        };
                    }
                    _ => return Err(OperationError::IllegalComposition),
                }
            }
            Target::ElementStartPost { tag, attributes } => match component {
                C::Retain(item_count) => {
                    self.flush_annotations()?;
                    self.normalizer.element_start(tag, attributes);
                    self.target = Self::after_single_item(*item_count);
                }
                C::DeleteElementStart { .. } => self.flush_annotations()?,
                C::ReplaceAttributes { new_attributes, .. } => {
                    self.flush_annotations()?;
                    self.normalizer.element_start(tag, new_attributes.clone());
                }
                C::UpdateAttributes(update) => {
                    self.flush_annotations()?;
                    self.normalizer
                        .element_start(tag, attributes.update_with(update)?);
                }
                _ => return Err(OperationError::IllegalComposition),
            },
            Target::ElementEndPost => match component {
                C::Retain(item_count) => {
                    self.flush_annotations()?;
                    self.normalizer.element_end();
                    self.target = Self::after_single_item(*item_count);
                }
                C::DeleteElementEnd => self.flush_annotations()?,
                _ => return Err(OperationError::IllegalComposition),
            },
            Target::ReplaceAttributesPost {
                old_attributes,
                new_attributes,
            } => match component {
                C::Retain(item_count) => {
                    self.flush_annotations()?;
                    self.normalizer
                        .replace_attributes(old_attributes, new_attributes);
                    self.target = Self::after_single_item(*item_count);
                }
                C::DeleteElementStart { tag, .. } => {
                    self.flush_annotations()?;
                    self.normalizer
                        .delete_element_start(tag.as_str(), old_attributes);
                }
                C::ReplaceAttributes {
                    new_attributes: replacement,
                    ..
                } => {
                    self.flush_annotations()?;
                    self.normalizer
                        .replace_attributes(old_attributes, replacement.clone());
                }
                C::UpdateAttributes(update) => {
                    self.flush_annotations()?;
                    self.normalizer
                        .replace_attributes(old_attributes, new_attributes.update_with(update)?);
                }
                _ => return Err(OperationError::IllegalComposition),
            },
            Target::UpdateAttributesPost(pending) => match component {
                C::Retain(item_count) => {
                    self.flush_annotations()?;
                    self.normalizer.update_attributes(pending);
                    self.target = Self::after_single_item(*item_count);
                }
                C::DeleteElementStart { tag, attributes } => {
                    self.flush_annotations()?;
                    self.normalizer.delete_element_start(
                        tag.as_str(),
                        attributes.update_with(&pending.invert()?)?,
                    );
                }
                C::ReplaceAttributes {
                    old_attributes,
                    new_attributes,
                } => {
                    self.flush_annotations()?;
                    self.normalizer.replace_attributes(
                        old_attributes.update_with(&pending.invert()?)?,
                        new_attributes.clone(),
                    );
                }
                C::UpdateAttributes(update) => {
                    self.flush_annotations()?;
                    self.normalizer
                        .update_attributes(pending.compose_with(update)?);
                }
                _ => return Err(OperationError::IllegalComposition),
            },
            // The finisher accepts insertions only; pre states are
            // unreachable here.
            _ => return Err(OperationError::IllegalComposition),
        }
        Ok(())
    }

    fn compose_operations(
        mut self,
        op1: &DocOp,
        op2: &DocOp,
    ) -> Result<Vec<DocOpComponent>, OperationError> {
        let mut op2_components = op2.iter();
        for component in op1 {
            self.apply_pre(component)?;
            while self.target.is_post_target() {
                let Some(component) = op2_components.next() else {
                    return Err(OperationError::SizeMismatch {
                        resulting: resulting_document_length(op1),
                        initial: initial_document_length(op2),
                    });
                };
                self.apply_post(component)?;
            }
        }
        // Note that, as in the original, a retain or deletion of the second
        // operation left pending at this point goes unreported.
        let mut remaining = op2_components.peekable();
        if remaining.peek().is_some() {
            self.target = Target::FinisherPost;
            for component in remaining {
                self.apply_post(component)?;
            }
        }
        self.flush_annotations()?;
        Ok(self.normalizer.finish())
    }
}

/// Composes two operations: the result is equivalent to applying `op1` and
/// then `op2`. The result is normalized and checked for well-formedness.
pub fn compose(op1: &DocOp, op2: &DocOp) -> Result<DocOp, OperationError> {
    DocOp::new(Composer::new().compose_operations(op1, op2)?)
}

/// Like [`compose`], but does not check the result for well-formedness.
pub fn compose_unchecked(op1: &DocOp, op2: &DocOp) -> Result<DocOp, OperationError> {
    Ok(DocOp::new_unchecked(
        Composer::new().compose_operations(op1, op2)?,
    ))
}

/// Applies an operation to a document given as an initialization.
pub fn compose_initialization(
    op1: &DocInitialization,
    op2: &DocOp,
) -> Result<DocInitialization, OperationError> {
    DocInitialization::try_from(compose(op1, op2)?)
}

/// Composes a sequence of operations; `None` for an empty sequence.
pub fn compose_all<'a>(
    operations: impl IntoIterator<Item = &'a DocOp>,
) -> Result<Option<DocOp>, OperationError> {
    let mut collector = DocOpCollector::new();
    for operation in operations {
        collector.add(operation.clone())?;
    }
    collector.compose_all()
}

/// Collects operations and composes them pairwise in a tree-like manner,
/// which costs O(n log N) instead of O(n^2) for sequential composition of N
/// operations of total size n.
#[derive(Debug, Clone, Default)]
pub struct DocOpCollector {
    // Slot i holds either nothing or the composition of 2^i consecutive
    // operations; later operations sit in lower slots.
    operations: Vec<Option<DocOp>>,
}

impl DocOpCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the next operation of the sequence.
    pub fn add(&mut self, mut operation: DocOp) -> Result<(), OperationError> {
        for slot in &mut self.operations {
            match slot.take() {
                None => {
                    *slot = Some(operation);
                    return Ok(());
                }
                Some(earlier) => operation = compose(&earlier, &operation)?,
            }
        }
        self.operations.push(Some(operation));
        Ok(())
    }

    /// Composes everything collected so far and empties the collector.
    /// Returns `None` if nothing was collected.
    pub fn compose_all(&mut self) -> Result<Option<DocOp>, OperationError> {
        let mut result: Option<DocOp> = None;
        for operation in std::mem::take(&mut self.operations).into_iter().flatten() {
            result = Some(match result {
                Some(later) => compose(&operation, &later)?,
                None => operation,
            });
        }
        Ok(result)
    }

    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}
