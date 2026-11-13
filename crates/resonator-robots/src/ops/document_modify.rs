//! `document.modify`: changes at one or more places of a blip
//! (`DocumentModifyService`).

use std::collections::BTreeMap;

use resonator_model::doc::line_containers;
use resonator_model::doc::{AnnotationReader, DocError, MutableDocument, NodeId, XmlFragment};
use resonator_model::docop::Attributes;
use resonator_model::id::ParticipantId;

use super::context::OperationContext;
use crate::api::{
    DocumentModifyAction, DocumentModifyQuery, Element, ElementType, InvalidRequest, ModifyHow,
    OperationRequest, ParamsProperty, Range,
};
use crate::view::{ApiView, AttachmentContext, DocumentHitIterator, api_element_to_xml};

/// A failed modification: a bad request, or an edit the document refused.
enum ModifyError {
    Invalid(InvalidRequest),
    Doc(DocError),
}

impl From<InvalidRequest> for ModifyError {
    fn from(error: InvalidRequest) -> Self {
        ModifyError::Invalid(error)
    }
}

impl From<DocError> for ModifyError {
    fn from(error: DocError) -> Self {
        ModifyError::Doc(error)
    }
}

type ModifyResult<T = ()> = Result<T, ModifyError>;

fn invalid<T>(message: impl Into<String>) -> ModifyResult<T> {
    Err(ModifyError::Invalid(InvalidRequest::new(message)))
}

/// `document.modify`.
///
/// Where to act is given by one of the parameters `range`, `index` and
/// `modifyQuery`; without any it is the whole blip. What to do there is
/// the `modifyAction`.
///
/// Differences from the original:
///
/// * A query with a `textMatch` and no `elementMatch` finds the
///   occurrences of the text. The original ignores `textMatch` (its text
///   matcher is never used) and fails on such a query.
/// * Inserting an element that is neither a gadget nor a form element
///   (a line, an image, an installer, an attachment) puts it at the
///   position; the original refuses. Gadgets and form elements go on a new
///   line at the end of the blip whatever the position, as in the
///   original.
pub(super) async fn execute(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let action: DocumentModifyAction = operation.required(ParamsProperty::ModifyAction)?;
    let name = context
        .open_operation_wavelet(operation, participant)
        .await?;
    let conversation = context.conversation(&name, participant)?;
    let blip = conversation.blip_id(&blip_id)?;
    let attachments = AttachmentContext::of(conversation.wavelet.data());
    let outcome = conversation.wavelet.edit_document(&blip, |doc| {
        let mut view = ApiView::new(doc.view(), &attachments);
        Ok(modify(operation, doc, &mut view, &action))
    });
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(ModifyError::Invalid(error))) => Err(error),
        Ok(Err(ModifyError::Doc(error))) | Err(error) => {
            Err(InvalidRequest::new(error.to_string()))
        }
    }
}

fn modify(
    operation: &OperationRequest,
    doc: &mut MutableDocument<'_>,
    view: &mut ApiView,
    action: &DocumentModifyAction,
) -> ModifyResult {
    let mut hits = hit_iterator(operation, view)?;
    match action.modify_how {
        Some(ModifyHow::Annotate) => annotate(doc, view, &mut hits, action, false),
        Some(ModifyHow::ClearAnnotation) => annotate(doc, view, &mut hits, action, true),
        Some(ModifyHow::Delete) => delete(doc, view, &mut hits),
        Some(ModifyHow::Insert) => insert(doc, view, &mut hits, action, false),
        Some(ModifyHow::InsertAfter) => insert(doc, view, &mut hits, action, true),
        Some(ModifyHow::Replace) => replace(doc, view, &mut hits, action),
        Some(ModifyHow::UpdateElement) => update_element(doc, view, &mut hits, action),
        None => invalid("Unsupported ModifyHow null"),
    }
}

/// `getDocumentHitIterator`.
fn hit_iterator(operation: &OperationRequest, view: &ApiView) -> ModifyResult<DocumentHitIterator> {
    let range: Option<Range> = operation.typed(ParamsProperty::Range);
    let index = operation.integer(ParamsProperty::Index);
    let query: Option<DocumentModifyQuery> = operation.typed(ParamsProperty::ModifyQuery);
    let specified = [range.is_some(), index.is_some(), query.is_some()]
        .into_iter()
        .filter(|&given| given)
        .count();
    if specified > 1 {
        return invalid(
            "At most one parameter out of RANGE, INDEX and MODIFY_QUERY must be specified",
        );
    }
    Ok(if let Some(range) = range {
        DocumentHitIterator::singleshot(range)
    } else if let Some(index) = index {
        DocumentHitIterator::singleshot(Range::new(index, index.saturating_add(1)))
    } else if let Some(query) = query {
        match (query.element_match, &query.text_match) {
            (Some(element_type), _) => DocumentHitIterator::element_matcher(
                element_type,
                query.restrictions.clone().unwrap_or_default(),
                query.max_res,
            ),
            (None, Some(text)) => DocumentHitIterator::text_matcher(text, query.max_res),
            (None, None) => {
                return invalid("The type of element to search for must not be null");
            }
        }
    } else {
        DocumentHitIterator::singleshot(Range::new(0, view.len() as i32))
    })
}

/// The document locations of an API range, as signed numbers: a negative
/// position lies before the first item of the body, as in the original.
fn xml_range(view: &ApiView, range: Range) -> Option<(i64, i64)> {
    let offset = |position: i32| -> Option<i64> {
        match usize::try_from(position) {
            Ok(position) => Some(view.transform_to_xml_offset(position)? as i64),
            Err(_) => Some(view.transform_to_xml_offset(0)? as i64 + i64::from(position)),
        }
    };
    Some((offset(range.start)?, offset(range.end)?))
}

/// `setDocumentAnnotation`, with the messages of the original's bounds
/// check.
fn set_annotation(
    doc: &mut MutableDocument<'_>,
    range: Option<(i64, i64)>,
    key: &str,
    value: Option<&str>,
) -> ModifyResult {
    let out_of_bounds = |detail: String| {
        invalid(format!(
            "Can't set annotation for out of bounds indices {detail}"
        ))
    };
    let Some((start, end)) = range else {
        return out_of_bounds("the blip has no content".to_owned());
    };
    let size = doc.size() as i64;
    if start < 0 {
        return out_of_bounds(format!(
            "start index ({start}) must not be less than start of range (0)"
        ));
    }
    if end < start {
        return out_of_bounds(format!(
            "end index ({end}) must not be less than start index ({start})"
        ));
    }
    if end > size {
        return out_of_bounds(format!(
            "end index ({end}) must not be greater than end of range ({size})"
        ));
    }
    Ok(doc.set_annotation(start as usize, end as usize, key, value)?)
}

/// `annotate` and `clearAnnotation`.
fn annotate(
    doc: &mut MutableDocument<'_>,
    view: &ApiView,
    hits: &mut DocumentHitIterator,
    action: &DocumentModifyAction,
    clear: bool,
) -> ModifyResult {
    let Some(key) = action.annotation_key.as_deref() else {
        return invalid("The annotation key must not be null");
    };
    let mut value_index = 0;
    while let Some(range) = hits.next(view) {
        let value = if clear {
            None
        } else {
            action.value(value_index)
        };
        set_annotation(doc, xml_range(view, range), key, value)?;
        value_index += 1;
    }
    Ok(())
}

fn delete(
    doc: &mut MutableDocument<'_>,
    view: &mut ApiView,
    hits: &mut DocumentHitIterator,
) -> ModifyResult {
    while let Some(range) = hits.next(view) {
        // The first newline cannot be deleted.
        let start = if range.start == 0 { 1 } else { range.start };
        let end = range.end;
        if start >= end {
            return invalid(format!("Invalid range specified, {start}>={end}"));
        }
        let (Ok(from), Ok(to)) = (usize::try_from(start), usize::try_from(end)) else {
            return invalid(format!("Invalid range specified, {start}, {end}"));
        };
        view.delete(doc, from, to)?;
        // The original shifts by the positive length, which only its
        // unused text matcher would notice; the text removed shifts what
        // follows to the left.
        hits.shift(i64::from(start), -i64::from(end - start));
    }
    Ok(())
}

/// `insert` and `insertAfter`.
fn insert(
    doc: &mut MutableDocument<'_>,
    view: &mut ApiView,
    hits: &mut DocumentHitIterator,
    action: &DocumentModifyAction,
    after: bool,
) -> ModifyResult {
    let mut value_index = 0;
    while let Some(range) = hits.next(view) {
        let insert_at = if after { range.end } else { range.start };
        let inserted = insert_into(doc, view, insert_at, action, value_index)?;
        hits.shift(i64::from(insert_at), inserted as i64);
        if after {
            // Do not search what was just inserted.
            hits.skip_to(i64::from(insert_at) + inserted as i64);
        }
        value_index += 1;
    }
    Ok(())
}

fn replace(
    doc: &mut MutableDocument<'_>,
    view: &mut ApiView,
    hits: &mut DocumentHitIterator,
    action: &DocumentModifyAction,
) -> ModifyResult {
    let mut value_index = 0;
    while let Some(range) = hits.next(view) {
        let replace_at = range.start;
        let inserted = insert_into(doc, view, replace_at, action, value_index)?;
        // Remove what follows the inserted content, so that it looks
        // replaced.
        let (Ok(from), Ok(to)) = (
            usize::try_from(i64::from(replace_at) + inserted as i64),
            usize::try_from(i64::from(range.end) + inserted as i64),
        ) else {
            return invalid(format!(
                "Invalid range specified, {}, {}",
                range.start, range.end
            ));
        };
        view.delete(doc, from, to)?;
        let removed = i64::from(range.start - range.end).min(0);
        hits.shift(i64::from(replace_at), inserted as i64 + removed);
        value_index += 1;
    }
    Ok(())
}

/// The document content of a new gadget (`GadgetXmlUtil.constructXml`):
/// every property, the URL included, becomes a state element.
fn gadget_xml(gadget: &Element) -> XmlFragment {
    let mut content = XmlFragment::empty();
    for (key, value) in &gadget.properties {
        content = content.append(&state_xml(key, value.as_deref()));
    }
    let attributes = [
        ("url", gadget.property(Element::URL)),
        ("title", Some("")),
        ("prefs", Some("")),
        ("state", Some("")),
        ("author", gadget.property(Element::AUTHOR)),
    ];
    content.wrap_with_attributes("gadget", attributes_of(&attributes))
}

/// `GadgetXmlUtil.constructStateXml`.
fn state_xml(key: &str, value: Option<&str>) -> XmlFragment {
    XmlFragment::empty().wrap_with_attributes(
        "state",
        attributes_of(&[("name", Some(key)), ("value", value)]),
    )
}

fn attributes_of(pairs: &[(&str, Option<&str>)]) -> Attributes {
    Attributes::from_pairs(pairs.iter().filter_map(|(k, v)| Some((*k, (*v)?)))).unwrap_or_default()
}

/// Inserts the action's value or element for a match at an API position
/// and returns the length inserted (`insertInto`).
fn insert_into(
    doc: &mut MutableDocument<'_>,
    view: &mut ApiView,
    insert_at: i32,
    action: &DocumentModifyAction,
    value_index: usize,
) -> ModifyResult<usize> {
    let Ok(position) = usize::try_from(insert_at) else {
        return invalid(format!("Invalid position {insert_at}"));
    };
    if let Some(value) = action.value(value_index) {
        let mut to_insert = value.to_owned();
        // The blip's text has to start with a newline.
        if position == 0 && !to_insert.is_empty() && !to_insert.starts_with('\n') {
            to_insert.insert(0, '\n');
        }
        view.insert_text(doc, position, &to_insert)?;
        let length = to_insert.encode_utf16().count();
        if let Some(bundled) = &action.bundled_annotations {
            let range = view
                .transform_to_xml_offset(position)
                .zip(view.transform_to_xml_offset(position + length))
                .map(|(start, end)| (start as i64, end as i64));
            // Clear whatever the inserted text inherited, then annotate.
            if let Some((start, end)) = range {
                for annotation in doc.ranged_annotations(start as usize, end as usize, None) {
                    set_annotation(doc, range, &annotation.key, None)?;
                }
            }
            for annotation in bundled {
                let Some(key) = annotation.key.as_deref() else {
                    return invalid("The annotation key must not be null");
                };
                set_annotation(doc, range, key, annotation.value.as_deref())?;
            }
        }
        return Ok(length);
    }
    if let Some(element) = action.element(value_index) {
        if element.is_gadget() {
            line_containers::append_line(
                doc,
                Some(&gadget_xml(element)),
                Some(&Attributes::new()),
            )?;
        } else if element.is_form_element() {
            line_containers::append_line(
                doc,
                Some(&api_element_to_xml(element)),
                Some(&Attributes::new()),
            )?;
        } else if element.element_type == ElementType::InlineBlip {
            return invalid(
                "Can't insert an inline blip as an element, use document.insertInlineBlip",
            );
        } else {
            view.insert_element(doc, position, element)?;
            return Ok(1);
        }
        // As in the original, the view is not told about what was
        // appended: the positions before it are unchanged, and a query
        // must not find the new line.
    }
    // Elements have a length of one in the API view.
    Ok(1)
}

fn update_element(
    doc: &mut MutableDocument<'_>,
    view: &ApiView,
    hits: &mut DocumentHitIterator,
    action: &DocumentModifyAction,
) -> ModifyResult {
    let mut index = 0;
    while let Some(range) = hits.next(view) {
        if let Some(element) = action.element(index) {
            if !element.is_gadget() {
                return invalid("Can't update other elements than gadgets at the moment");
            }
            let location = usize::try_from(range.start)
                .ok()
                .and_then(|start| view.transform_to_xml_offset(start))
                .filter(|&location| {
                    doc.view()
                        .element_starting_at(location)
                        .is_some_and(|el| doc.view().has_tag(el, "gadget"))
                });
            let Some(location) = location else {
                return invalid("There is no gadget at the position to update");
            };
            update_existing_gadget(doc, location, element)?;
        }
        index += 1;
    }
    Ok(())
}

/// The gadget element at a location and its child elements by key: the
/// tag name, or for state elements `state <name>`.
fn gadget_children(
    doc: &MutableDocument<'_>,
    location: usize,
) -> ModifyResult<(NodeId, BTreeMap<String, NodeId>)> {
    let view = doc.view();
    let Some(gadget) = view.element_starting_at(location) else {
        return Err(DocError::InvalidNode.into());
    };
    let mut children = BTreeMap::new();
    for child in view.child_elements(gadget) {
        let Some(tag) = view.tag_name(child) else {
            continue;
        };
        let key = if tag == "state" {
            format!("state {}", view.attribute(child, "name").unwrap_or("null"))
        } else {
            tag.to_owned()
        };
        children.insert(key, child);
    }
    Ok((gadget, children))
}

/// `updateExistingGadgetElement`: sets the URL, and sets, adds or (for a
/// null value) deletes the state named by every other property.
fn update_existing_gadget(
    doc: &mut MutableDocument<'_>,
    location: usize,
    element: &Element,
) -> ModifyResult {
    if let Some(url) = element.property(Element::URL) {
        let (gadget, _) = gadget_children(doc, location)?;
        doc.set_element_attribute(gadget, "url", Some(url))?;
    }
    for (property, value) in &element.properties {
        let (key, is_state) = match property.as_str() {
            "title" | "thumbnail" | "author" => (property.clone(), false),
            "name" | "pref" | "url" => continue,
            _ => (format!("state {property}"), true),
        };
        // Every edit invalidates the nodes, so look them up afresh.
        let (gadget, children) = gadget_children(doc, location)?;
        let child = children.get(&key).copied();
        match (value, child) {
            (None, None) => {}
            (None, Some(child)) => doc.delete_node(child)?,
            (Some(value), Some(child)) if is_state => {
                doc.set_element_attribute(child, "value", Some(value))?;
            }
            (Some(value), Some(child)) => {
                // The original puts the value into the element as text.
                let start = doc.view().content_start(child);
                doc.empty_element(child)?;
                doc.insert_text(start, value)?;
            }
            (Some(value), None) => {
                let end = doc.view().content_end(gadget);
                doc.insert_xml(end, &state_xml(property, Some(value)))?;
            }
        }
    }
    Ok(())
}
