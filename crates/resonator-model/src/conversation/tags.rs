//! The tags of a conversation, kept in the `tags` data document as a list
//! of `<tag>name</tag>` elements (`TagsDocument`).

use crate::doc::{DocError, DocView, MutableDocument, XmlFragment};
use crate::docop::BootstrapDocument;

pub const TAG_TAGNAME: &str = "tag";

/// The tags of a tags document, in document order: the text of the first
/// child of every top-level node that starts with text. Nodes without such
/// text are skipped, as `TagsDocument.processInitialState` does.
pub fn tags(doc: &BootstrapDocument) -> Vec<String> {
    let view = DocView::new(doc);
    view.children(view.root())
        .iter()
        .filter_map(|&node| view.text(view.first_child(node)?))
        .map(str::to_owned)
        .collect()
}

fn xml_for(tag_name: &str) -> XmlFragment {
    XmlFragment::text(tag_name).wrap(TAG_TAGNAME)
}

/// Appends a tag (`TagsDocument.addTag(String)`). Duplicates are not
/// checked.
pub fn add_tag(doc: &mut MutableDocument<'_>, tag_name: &str) -> Result<(), DocError> {
    doc.append_xml(&xml_for(tag_name))?;
    Ok(())
}

/// Inserts a tag before the `index`th top-level node, or at the end if
/// there are not that many (`TagsDocument.addTag(String, int)`).
pub fn insert_tag(
    doc: &mut MutableDocument<'_>,
    tag_name: &str,
    index: usize,
) -> Result<(), DocError> {
    let view = doc.view();
    let location = match view.children(view.root()).get(index) {
        // The original takes the node only if it is an element.
        Some(&node) if view.is_element(node) => view.location(node),
        _ => view.size(),
    };
    doc.insert_xml(location, &xml_for(tag_name))?;
    Ok(())
}

/// Deletes the first tag with a name, if any (`TagsDocument.deleteTag`).
/// Returns whether one was deleted.
pub fn delete_tag(doc: &mut MutableDocument<'_>, tag_name: &str) -> Result<bool, DocError> {
    let view = doc.view();
    let found = view.child_elements(view.root()).find(|&element| {
        view.first_child(element)
            .and_then(|child| view.text(child))
            .is_some_and(|text| text == tag_name)
    });
    match found {
        Some(element) => {
            doc.delete_node(element)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Deletes the top-level elements that do not start with text, first to
/// last (the cleanup of `TagsDocument.processInitialState`).
pub fn delete_empty_tags(doc: &mut MutableDocument<'_>) -> Result<(), DocError> {
    loop {
        let view = doc.view();
        let empty = view.child_elements(view.root()).find(|&element| {
            view.first_child(element)
                .and_then(|c| view.text(c))
                .is_none()
        });
        match empty {
            Some(element) => doc.delete_node(element)?,
            None => return Ok(()),
        }
    }
}
