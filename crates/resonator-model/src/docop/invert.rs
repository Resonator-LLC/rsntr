//! Inversion of document operations (`DocOpInverter`).

use super::error::OperationError;
use super::op::{DocOp, DocOpComponent};

/// The operation that undoes `op` when applied to the document `op`
/// produces. Fails if the result is ill-formed.
pub fn invert(op: &DocOp) -> Result<DocOp, OperationError> {
    let mut components = Vec::with_capacity(op.len());
    for component in op {
        components.push(invert_component(component)?);
    }
    DocOp::new(components)
}

fn invert_component(component: &DocOpComponent) -> Result<DocOpComponent, OperationError> {
    use DocOpComponent as C;
    Ok(match component {
        C::Retain(item_count) => C::Retain(*item_count),
        C::Characters(chars) => C::DeleteCharacters(chars.clone()),
        C::ElementStart { tag, attributes } => C::DeleteElementStart {
            tag: tag.clone(),
            attributes: attributes.clone(),
        },
        C::ElementEnd => C::DeleteElementEnd,
        C::DeleteCharacters(chars) => C::Characters(chars.clone()),
        C::DeleteElementStart { tag, attributes } => C::ElementStart {
            tag: tag.clone(),
            attributes: attributes.clone(),
        },
        C::DeleteElementEnd => C::ElementEnd,
        C::ReplaceAttributes {
            old_attributes,
            new_attributes,
        } => C::ReplaceAttributes {
            old_attributes: new_attributes.clone(),
            new_attributes: old_attributes.clone(),
        },
        C::UpdateAttributes(update) => C::UpdateAttributes(update.invert()?),
        C::AnnotationBoundary(map) => C::AnnotationBoundary(map.invert()),
    })
}
