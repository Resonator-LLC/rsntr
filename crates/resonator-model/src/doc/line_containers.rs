//! Lines of text in a blip body (`LineContainers`).
//!
//! A blip's text lives in a line container, the `body` element, as a
//! sequence of empty `line` elements each followed by that line's content.
//! The original lets clients register further line container tag names;
//! only `body` is one here.

use super::{DocError, MutableDocument, NodeId, XmlFragment};
use crate::docop::Attributes;

pub const LINE_TAGNAME: &str = "line";

/// The tag name of the top-level line container
/// (`LineContainers.topLevelContainerTagname`, set by `Blips`).
pub const TOP_LEVEL_CONTAINER_TAGNAME: &str = "body";

/// `LineContainers.isLineContainerTagname`.
pub fn is_line_container_tagname(tag: &str) -> bool {
    tag == TOP_LEVEL_CONTAINER_TAGNAME
}

fn last_line_container(doc: &MutableDocument<'_>) -> Option<NodeId> {
    let view = doc.view();
    view.descendant_elements(view.root())
        .into_iter()
        .rev()
        .find(|&el| view.tag_name(el).is_some_and(is_line_container_tagname))
}

/// Appends a line with content to the last line container, creating
/// `<body><line/></body>` if the document has none, and returns the new
/// line element (`LineContainers.appendLine`).
///
/// As in the original, when the container had to be created the line's
/// attributes are set with a separate replace-attributes operation, even
/// if there are none.
pub fn append_line(
    doc: &mut MutableDocument<'_>,
    content: Option<&XmlFragment>,
    attributes: Option<&Attributes>,
) -> Result<NodeId, DocError> {
    let content = content.filter(|c| c.item_count() > 0);
    match last_line_container(doc) {
        None => {
            let initial = XmlFragment::empty()
                .wrap(LINE_TAGNAME)
                .wrap(TOP_LEVEL_CONTAINER_TAGNAME);
            let container_location = doc.size();
            doc.append_xml(&initial)?;
            if let Some(content) = content {
                // The end of the new container: after <body><line></line>.
                doc.insert_xml(container_location + 3, content)?;
            }
            let line = doc
                .view()
                .element_starting_at(container_location + 1)
                .ok_or(DocError::InvalidNode)?;
            if let Some(attributes) = attributes {
                doc.set_element_attributes(line, attributes.clone())?;
            }
            doc.view()
                .element_starting_at(container_location + 1)
                .ok_or(DocError::InvalidNode)
        }
        Some(container) => {
            let pairs: Vec<(&str, &str)> =
                attributes.map(|a| a.iter().collect()).unwrap_or_default();
            let line_location = doc.view().content_end(container);
            doc.create_element(line_location, LINE_TAGNAME, &pairs)?;
            if let Some(content) = content {
                doc.insert_xml(line_location + 2, content)?;
            }
            doc.view()
                .element_starting_at(line_location)
                .ok_or(DocError::InvalidNode)
        }
    }
}

/// Appends content to the end of the last line container, or as a new line
/// if there is no container (`LineContainers.appendToLastLine`).
pub fn append_to_last_line(
    doc: &mut MutableDocument<'_>,
    content: &XmlFragment,
) -> Result<(), DocError> {
    match last_line_container(doc) {
        Some(container) => {
            let location = doc.view().content_end(container);
            doc.insert_xml(location, content)?;
        }
        None => {
            append_line(doc, Some(content), Some(&Attributes::new()))?;
        }
    }
    Ok(())
}
