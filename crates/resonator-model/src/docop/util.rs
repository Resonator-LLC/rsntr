//! Helpers on operations (`DocOpUtil` in the original): lengths, debug
//! formatting, normalization and the canonical XML form of a document.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use super::annotation::AnnotationBoundaryMap;
use super::attributes::{Attributes, AttributesUpdate};
use super::error::OperationError;
use super::normalize::new_normalizer;
use super::op::{DocInitialization, DocOp, DocOpComponent, OpSink};
use super::utf16::{Utf16Key, utf16_len};

/// Name of the XML processing instruction used for annotations.
pub const PI_TARGET: &str = "a";

/// Views an operation as a document initialization. Fails if it contains
/// anything but insertions and annotation boundaries.
pub fn as_initialization(op: DocOp) -> Result<DocInitialization, OperationError> {
    DocInitialization::try_from(op)
}

fn literal_string(s: Option<&str>) -> String {
    match s {
        None => "null".to_owned(),
        Some(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
    }
}

/// Compact single-line rendering of an operation, for logs and tests.
pub fn to_concise_string(op: &DocOp) -> String {
    let mut b = String::new();
    for component in op {
        let _ = match component {
            DocOpComponent::DeleteCharacters(chars) => {
                write!(b, "--{}; ", literal_string(Some(chars)))
            }
            DocOpComponent::DeleteElementEnd => write!(b, "x>; "),
            DocOpComponent::DeleteElementStart { tag, attributes } => {
                write!(
                    b,
                    "x< {} {}; ",
                    tag,
                    attributes_to_concise_string(attributes)
                )
            }
            DocOpComponent::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => write!(
                b,
                "r@ {} {}; ",
                attributes_to_concise_string(old_attributes),
                attributes_to_concise_string(new_attributes)
            ),
            DocOpComponent::Retain(distance) => write!(b, "__{distance}; "),
            DocOpComponent::UpdateAttributes(update) => {
                write!(b, "u@ {}; ", attributes_update_to_concise_string(update))
            }
            DocOpComponent::AnnotationBoundary(map) => {
                write!(b, "|| {}; ", annotation_boundary_to_concise_string(map))
            }
            DocOpComponent::Characters(chars) => write!(b, "++{}; ", literal_string(Some(chars))),
            DocOpComponent::ElementEnd => write!(b, ">>; "),
            DocOpComponent::ElementStart { tag, attributes } => {
                write!(
                    b,
                    "<< {} {}; ",
                    tag,
                    attributes_to_concise_string(attributes)
                )
            }
        };
    }
    b
}

pub fn attributes_to_concise_string(attributes: &Attributes) -> String {
    if attributes.is_empty() {
        return "{}".to_owned();
    }
    let entries: Vec<String> = attributes
        .iter()
        .map(|(name, value)| format!("{}={}", name, literal_string(Some(value))))
        .collect();
    format!("{{ {} }}", entries.join(", "))
}

pub fn attributes_update_to_concise_string(update: &AttributesUpdate) -> String {
    if update.is_empty() {
        return "{}".to_owned();
    }
    let entries: Vec<String> = update
        .updates()
        .iter()
        .map(|u| {
            format!(
                "{}: {} -> {}",
                u.name,
                literal_string(u.old_value.as_deref()),
                literal_string(u.new_value.as_deref())
            )
        })
        .collect();
    format!("{{ {} }}", entries.join(", "))
}

pub fn annotation_boundary_to_concise_string(map: &AnnotationBoundaryMap) -> String {
    if map.is_empty() {
        return "{}".to_owned();
    }
    let mut entries: Vec<String> = map
        .end_keys()
        .iter()
        .map(|key| literal_string(Some(key)))
        .collect();
    entries.extend(map.changes().iter().map(|c| {
        format!(
            "{}: {} -> {}",
            literal_string(Some(&c.key)),
            literal_string(c.old_value.as_deref()),
            literal_string(c.new_value.as_deref())
        )
    }));
    format!("{{ {} }}", entries.join(", "))
}

/// The minimal, normalized XML representation of a document, with
/// annotations written as `<?a ...?>` processing instructions. Two documents
/// are equal iff their XML strings are equal.
///
/// Fails if the initialization closes an element that was never opened.
pub fn to_xml_string(op: &DocInitialization) -> Result<String, OperationError> {
    let mut b = String::new();
    let mut current_annotations: HashMap<&str, &str> = HashMap::new();
    let mut tags: Vec<&str> = Vec::new();
    let mut element_part: Option<String> = None;
    for component in op.iter() {
        match component {
            DocOpComponent::AnnotationBoundary(map) => {
                // None: the annotation ends or is set to null here.
                let mut changes: BTreeMap<Utf16Key, Option<&str>> = BTreeMap::new();
                for change in map.changes() {
                    let key = change.key.as_str();
                    let value = change.new_value.as_deref();
                    if current_annotations.get(key).copied() != value {
                        match value {
                            Some(value) => current_annotations.insert(key, value),
                            None => current_annotations.remove(key),
                        };
                        changes.insert(Utf16Key(key.to_owned()), value);
                    }
                }
                for key in map.end_keys() {
                    if current_annotations.remove(key.as_str()).is_some() {
                        changes.insert(Utf16Key(key.clone()), None);
                    }
                }
                if changes.is_empty() {
                    continue;
                }
                if let Some(part) = element_part.take() {
                    b.push_str(&part);
                    b.push('>');
                }
                b.push_str("<?");
                b.push_str(PI_TARGET);
                for (key, value) in &changes {
                    let _ = write!(b, " \"{}\"", xml_text_escape(&annotation_escape(&key.0)));
                    if let Some(value) = value {
                        let _ = write!(b, "=\"{}\"", xml_text_escape(&annotation_escape(value)));
                    }
                }
                b.push_str("?>");
            }
            DocOpComponent::Characters(chars) => {
                if let Some(part) = element_part.take() {
                    b.push_str(&part);
                    b.push('>');
                }
                b.push_str(&xml_text_escape(chars));
            }
            DocOpComponent::ElementStart { tag, attributes } => {
                if let Some(part) = element_part.take() {
                    b.push_str(&part);
                    b.push('>');
                }
                let mut part = format!("<{tag}");
                if !attributes.is_empty() {
                    part.push(' ');
                    part.push_str(&attribute_string(attributes));
                }
                element_part = Some(part);
                tags.push(tag);
            }
            DocOpComponent::ElementEnd => {
                let tag = tags.pop().ok_or_else(|| {
                    OperationError::InvalidArgument(
                        "to_xml_string: element end without element start".to_owned(),
                    )
                })?;
                if let Some(part) = element_part.take() {
                    b.push_str(&part);
                    b.push_str("/>");
                } else {
                    let _ = write!(b, "</{tag}>");
                }
            }
            // Unreachable: a DocInitialization holds no other components.
            other => return Err(OperationError::NotAnInitialization(other.type_name())),
        }
    }
    Ok(b)
}

/// Renders attributes as `name="value"` pairs separated by spaces.
pub fn attribute_string(attributes: &Attributes) -> String {
    let entries: Vec<String> = attributes
        .iter()
        .map(|(name, value)| format!("{}=\"{}\"", name, xml_attr_escape(value)))
        .collect();
    entries.join(" ")
}

/// Escapes double quotes only, like the original.
pub fn xml_attr_escape(value: &str) -> String {
    value.replace('"', "&quot;")
}

pub fn xml_text_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn xml_text_unescape(escaped: &str) -> String {
    escaped
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

pub fn annotation_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('?', "\\q")
}

pub fn annotation_unescape(escaped: &str) -> String {
    escaped
        .replace("\\q", "?")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}

/// Normalizes an operation: merges adjacent ranges and annotation boundaries
/// and drops no-op components. Fails if the result is ill-formed.
pub fn normalize(op: &DocOp) -> Result<DocOp, OperationError> {
    let mut normalizer = new_normalizer();
    for component in op {
        normalizer.push(component.clone());
    }
    DocOp::new(normalizer.finish())
}

/// Normalizes a document initialization.
pub fn normalize_initialization(
    op: &DocInitialization,
) -> Result<DocInitialization, OperationError> {
    as_initialization(normalize(op)?)
}

/// Number of items of the document that an operation applies to.
pub fn initial_document_length(op: &DocOp) -> usize {
    op.iter()
        .map(|component| match component {
            DocOpComponent::DeleteCharacters(chars) => utf16_len(chars),
            DocOpComponent::DeleteElementEnd
            | DocOpComponent::DeleteElementStart { .. }
            | DocOpComponent::ReplaceAttributes { .. }
            | DocOpComponent::UpdateAttributes(_) => 1,
            DocOpComponent::Retain(item_count) => *item_count,
            DocOpComponent::AnnotationBoundary(_)
            | DocOpComponent::Characters(_)
            | DocOpComponent::ElementEnd
            | DocOpComponent::ElementStart { .. } => 0,
        })
        .fold(0, usize::saturating_add)
}

/// Number of items of the document that an operation produces.
pub fn resulting_document_length(op: &DocOp) -> usize {
    op.iter()
        .map(|component| match component {
            DocOpComponent::DeleteCharacters(_)
            | DocOpComponent::DeleteElementEnd
            | DocOpComponent::DeleteElementStart { .. }
            | DocOpComponent::AnnotationBoundary(_) => 0,
            DocOpComponent::ReplaceAttributes { .. }
            | DocOpComponent::UpdateAttributes(_)
            | DocOpComponent::ElementEnd
            | DocOpComponent::ElementStart { .. } => 1,
            DocOpComponent::Retain(item_count) => *item_count,
            DocOpComponent::Characters(chars) => utf16_len(chars),
        })
        .fold(0, usize::saturating_add)
}
