//! Constructors for simple single-purpose operations (`DocOpCreator`).
//!
//! `size` is always the length of the document the operation applies to.
//! The functions panic if the arguments describe an ill-formed operation.

use crate::docop::annotation::AnnotationBoundaryMap;
use crate::docop::attributes::{Attributes, AttributesUpdate};
use crate::docop::op::{DocOp, DocOpComponent};
use crate::docop::utf16::utf16_len;

/// Builds an operation from components, dropping empty retains and
/// character runs.
fn build(components: Vec<DocOpComponent>) -> DocOp {
    let components = components
        .into_iter()
        .filter(|c| match c {
            DocOpComponent::Retain(0) => false,
            DocOpComponent::Characters(s) | DocOpComponent::DeleteCharacters(s) => !s.is_empty(),
            _ => true,
        })
        .collect();
    DocOp::new(components).expect("creator arguments describe a well-formed operation")
}

pub fn insert_characters(size: usize, location: usize, characters: &str) -> DocOp {
    build(vec![
        DocOpComponent::Retain(location),
        DocOpComponent::Characters(characters.to_owned()),
        DocOpComponent::Retain(size - location),
    ])
}

pub fn insert_element(size: usize, location: usize, tag: &str, attributes: Attributes) -> DocOp {
    build(vec![
        DocOpComponent::Retain(location),
        DocOpComponent::ElementStart {
            tag: tag.to_owned(),
            attributes,
        },
        DocOpComponent::ElementEnd,
        DocOpComponent::Retain(size - location),
    ])
}

pub fn delete_characters(size: usize, location: usize, characters: &str) -> DocOp {
    build(vec![
        DocOpComponent::Retain(location),
        DocOpComponent::DeleteCharacters(characters.to_owned()),
        DocOpComponent::Retain(size - location - utf16_len(characters)),
    ])
}

pub fn delete_element(size: usize, location: usize, tag: &str, attributes: Attributes) -> DocOp {
    build(vec![
        DocOpComponent::Retain(location),
        DocOpComponent::DeleteElementStart {
            tag: tag.to_owned(),
            attributes,
        },
        DocOpComponent::DeleteElementEnd,
        DocOpComponent::Retain(size - location - 2),
    ])
}

pub fn replace_attributes(
    size: usize,
    location: usize,
    old_attributes: Attributes,
    new_attributes: Attributes,
) -> DocOp {
    build(vec![
        DocOpComponent::Retain(location),
        DocOpComponent::ReplaceAttributes {
            old_attributes,
            new_attributes,
        },
        DocOpComponent::Retain(size - location - 1),
    ])
}

pub fn set_attribute(
    size: usize,
    location: usize,
    name: &str,
    old_value: Option<&str>,
    new_value: Option<&str>,
) -> DocOp {
    build(vec![
        DocOpComponent::Retain(location),
        DocOpComponent::UpdateAttributes(AttributesUpdate::single(name, old_value, new_value)),
        DocOpComponent::Retain(size - location - 1),
    ])
}

pub fn set_annotation(
    size: usize,
    start: usize,
    end: usize,
    key: &str,
    old_value: Option<&str>,
    new_value: Option<&str>,
) -> DocOp {
    let mut components = vec![DocOpComponent::Retain(start)];
    if end > start {
        components.push(DocOpComponent::AnnotationBoundary(
            AnnotationBoundaryMap::changes_only([(key, old_value, new_value)])
                .expect("valid annotation key"),
        ));
        components.push(DocOpComponent::Retain(end - start));
        components.push(DocOpComponent::AnnotationBoundary(
            AnnotationBoundaryMap::ends([key]).expect("valid annotation key"),
        ));
    }
    components.push(DocOpComponent::Retain(size - end));
    build(components)
}

pub fn identity(size: usize) -> DocOp {
    build(vec![DocOpComponent::Retain(size)])
}
