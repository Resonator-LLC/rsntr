//! DOM-like editing of a document, emitting document operations.

use std::collections::BTreeSet;

use super::nindo::{Nindo, NindoBuilder, attributes_from_pairs};
use super::view::{DocView, NodeId, Point};
use super::{DocError, XmlFragment, XmlItem};
use crate::docop::annotation::validate_annotation_key;
use crate::docop::{Attributes, AutomatonDocument, BootstrapDocument, DocOp, normalize};

/// Where a [`MutableDocument`] reads its document from and sends its
/// operations to (the original's `OperationSequencer` together with the
/// document substrate).
pub trait DocumentSink {
    /// The current document.
    fn document(&self) -> &BootstrapDocument;

    /// Applies an operation to the document and passes it on.
    fn consume(&mut self, op: DocOp) -> Result<(), DocError>;
}

/// A document on its own that records the operations applied to it.
#[derive(Debug, Clone, Default)]
pub struct StandaloneDocument {
    document: BootstrapDocument,
    ops: Vec<DocOp>,
}

impl StandaloneDocument {
    pub fn new(document: BootstrapDocument) -> Self {
        StandaloneDocument {
            document,
            ops: Vec::new(),
        }
    }

    /// The operations consumed so far, oldest first.
    pub fn ops(&self) -> &[DocOp] {
        &self.ops
    }

    pub fn take_ops(&mut self) -> Vec<DocOp> {
        std::mem::take(&mut self.ops)
    }

    pub fn mutable(&mut self) -> MutableDocument<'_> {
        MutableDocument::new(self)
    }
}

impl DocumentSink for StandaloneDocument {
    fn document(&self) -> &BootstrapDocument {
        &self.document
    }

    fn consume(&mut self, op: DocOp) -> Result<(), DocError> {
        self.document.consume(&op)?;
        self.ops.push(op);
        Ok(())
    }
}

/// An annotation value over a range of locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangedAnnotation {
    pub key: String,
    /// `None` where the key is not set.
    pub value: Option<String>,
    pub start: usize,
    pub end: usize,
}

/// The annotations of a document, read by location.
pub trait AnnotationReader {
    fn annotation_document(&self) -> &BootstrapDocument;

    /// The value of `key` on the item at `location` (`getAnnotation`).
    fn annotation(&self, location: usize, key: &str) -> Option<String> {
        self.annotation_document().get_annotation(location, key)
    }

    /// The first location in `start..end` where `key` does not have
    /// `from_value` (`firstAnnotationChange`; `None` for the original's -1).
    fn find_annotation_change(
        &self,
        start: usize,
        end: usize,
        key: &str,
        from_value: Option<&str>,
    ) -> Option<usize> {
        AutomatonDocument::first_annotation_change(
            self.annotation_document(),
            start,
            end,
            key,
            from_value,
        )
    }

    /// The annotation keys in use anywhere in the document.
    fn annotation_keys(&self) -> BTreeSet<String> {
        let doc = self.annotation_document();
        let mut keys = BTreeSet::new();
        let mut previous = None;
        for location in 0..doc.length() {
            let annotations = doc.annotations_at(location);
            if previous.as_ref() != Some(&annotations) {
                keys.extend(annotations.iter().map(|(key, _)| key.to_owned()));
                previous = Some(annotations);
            }
        }
        keys
    }

    /// For each key (all keys in use if `keys` is `None`), the maximal
    /// ranges of equal value that intersect `start..end`, unset ranges
    /// included, ordered by start and then key (`rangedAnnotations`; the
    /// original leaves the order of ranges with equal starts unspecified).
    /// Ranges are not clipped to `start..end`.
    fn ranged_annotations(
        &self,
        start: usize,
        end: usize,
        keys: Option<&[&str]>,
    ) -> Vec<RangedAnnotation> {
        let doc = self.annotation_document();
        let size = doc.length();
        let end = end.min(size);
        let mut result = Vec::new();
        if start >= end {
            return result;
        }
        let keys: Vec<String> = match keys {
            Some(keys) => keys.iter().map(|&key| key.to_owned()).collect(),
            None => self.annotation_keys().into_iter().collect(),
        };
        for key in keys {
            let mut range_start = start;
            let value = doc.get_annotation(start, &key);
            while range_start > 0 && doc.get_annotation(range_start - 1, &key) == value {
                range_start -= 1;
            }
            let mut value = value;
            while range_start < end {
                let range_end = doc
                    .find_annotation_change(range_start, size, &key, value.as_deref())
                    .unwrap_or(size);
                result.push(RangedAnnotation {
                    key: key.clone(),
                    value,
                    start: range_start,
                    end: range_end,
                });
                range_start = range_end;
                value = doc.get_annotation(range_start, &key);
            }
        }
        result.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.key.cmp(&b.key)));
        result
    }
}

impl AnnotationReader for BootstrapDocument {
    fn annotation_document(&self) -> &BootstrapDocument {
        self
    }
}

/// A document that can be read as an element tree and edited through
/// DOM-like calls, each of which sends one document operation to the sink
/// (`MutableDocumentImpl`).
///
/// Every edit rebuilds the tree, so a [`NodeId`] obtained before an edit
/// must not be used after it. Locations stay meaningful as far as the edit
/// did not shift them.
///
/// Like the original, an edit that changes nothing in the document (empty
/// text, an empty fragment, an empty range inside a multi-element deletion)
/// still emits an operation, one that only retains.
///
/// The operations emitted are normalized; they equal the original's after
/// normalization. (The original's raw operations split retains and
/// deletions in ways that depend on the history of its DOM.)
pub struct MutableDocument<'a> {
    sink: &'a mut dyn DocumentSink,
    view: DocView,
}

impl AnnotationReader for MutableDocument<'_> {
    fn annotation_document(&self) -> &BootstrapDocument {
        self.sink.document()
    }
}

impl<'a> MutableDocument<'a> {
    pub fn new(sink: &'a mut dyn DocumentSink) -> Self {
        let view = DocView::new(sink.document());
        MutableDocument { sink, view }
    }

    /// The element tree as of the last edit.
    pub fn view(&self) -> &DocView {
        &self.view
    }

    pub fn document(&self) -> &BootstrapDocument {
        self.sink.document()
    }

    pub fn size(&self) -> usize {
        self.view.size()
    }

    pub fn to_xml_string(&self) -> String {
        self.sink.document().to_xml_string()
    }

    /// Converts a non-invertible operation against the current document,
    /// applies it and sends it on (`hackConsume`). The operation sent is
    /// normalized.
    pub fn consume(&mut self, nindo: &Nindo) -> Result<(), DocError> {
        let op = normalize(&nindo.to_doc_op(self.sink.document())?)?;
        self.sink.consume(op)?;
        self.view = DocView::new(self.sink.document());
        Ok(())
    }

    fn at(location: usize) -> NindoBuilder {
        let mut b = Nindo::builder();
        b.skip(location);
        b
    }

    fn check_location(&self, location: usize) -> Result<(), DocError> {
        if location > self.size() {
            return Err(DocError::InvalidLocation {
                location,
                size: self.size(),
            });
        }
        Ok(())
    }

    fn check_range(&self, start: usize, end: usize) -> Result<(), DocError> {
        self.check_location(end)?;
        if start > end {
            return Err(DocError::InvalidArgument(format!(
                "range start {start} is after its end {end}"
            )));
        }
        Ok(())
    }

    /// A non-root element of the current view.
    fn check_element(&self, element: NodeId) -> Result<(), DocError> {
        if !self.view.is_element(element) || element == self.view.root() {
            return Err(DocError::InvalidNode);
        }
        Ok(())
    }

    /// The location of a point (`getLocation`).
    pub fn point_location(&self, point: Point) -> Result<usize, DocError> {
        self.view.point_location(point).ok_or(DocError::InvalidNode)
    }

    pub fn insert_text(&mut self, location: usize, text: &str) -> Result<(), DocError> {
        self.check_location(location)?;
        let mut b = Self::at(location);
        b.characters(text);
        self.consume(&b.build())
    }

    /// Inserts a fragment and returns the element then starting at the
    /// insertion location, if any (`insertXml`).
    pub fn insert_xml(
        &mut self,
        location: usize,
        xml: &XmlFragment,
    ) -> Result<Option<NodeId>, DocError> {
        self.check_location(location)?;
        let mut b = Self::at(location);
        for item in xml.items() {
            match item {
                XmlItem::Text(text) => b.characters(text),
                XmlItem::Start { tag, attributes } => b.element_start(tag, attributes.clone()),
                XmlItem::End => b.element_end(),
            }
        }
        self.consume(&b.build())?;
        Ok(self.view.element_starting_at(location))
    }

    /// Inserts a fragment at the end of the document (`appendXml`).
    pub fn append_xml(&mut self, xml: &XmlFragment) -> Result<Option<NodeId>, DocError> {
        self.insert_xml(self.size(), xml)
    }

    /// Inserts an empty element and returns it (`createElement`).
    pub fn create_element(
        &mut self,
        location: usize,
        tag: &str,
        attributes: &[(&str, &str)],
    ) -> Result<NodeId, DocError> {
        self.check_location(location)?;
        let mut b = Self::at(location);
        b.element_start(tag, attributes_from_pairs(attributes.iter().copied()));
        b.element_end();
        self.consume(&b.build())?;
        self.view
            .element_starting_at(location)
            .ok_or(DocError::InvalidNode)
    }

    /// Inserts an empty element at the end of `parent`
    /// (`createChildElement`).
    pub fn create_child_element(
        &mut self,
        parent: NodeId,
        tag: &str,
        attributes: &[(&str, &str)],
    ) -> Result<NodeId, DocError> {
        if !self.view.is_element(parent) {
            return Err(DocError::InvalidNode);
        }
        self.create_element(self.view.content_end(parent), tag, attributes)
    }

    fn delete_element_into(&self, element: NodeId, b: &mut NindoBuilder) {
        b.delete_element_start();
        self.empty_element_into(element, b);
        b.delete_element_end();
    }

    fn empty_element_into(&self, element: NodeId, b: &mut NindoBuilder) {
        for &child in self.view.children(element) {
            if self.view.is_element(child) {
                self.delete_element_into(child, b);
            } else {
                b.delete_characters(self.view.item_size(child));
            }
        }
    }

    /// Deletes an element with its content (`deleteNode`).
    pub fn delete_node(&mut self, element: NodeId) -> Result<(), DocError> {
        self.check_element(element)?;
        let mut b = Self::at(self.view.location(element));
        self.delete_element_into(element, &mut b);
        self.consume(&b.build())
    }

    /// Deletes the content of an element, which may be the root
    /// (`emptyElement`).
    pub fn empty_element(&mut self, element: NodeId) -> Result<(), DocError> {
        if !self.view.is_element(element) {
            return Err(DocError::InvalidNode);
        }
        let mut b = Self::at(self.view.content_start(element));
        self.empty_element_into(element, &mut b);
        self.consume(&b.build())
    }

    /// Deletes what lies between two locations of the same element.
    fn delete_range_internal(&self, start: usize, end: usize) -> Result<Nindo, DocError> {
        let mut b = Self::at(start);
        let (Some(start_point), Some(end_point)) = (self.view.locate(start), self.view.locate(end))
        else {
            return Err(DocError::InvalidLocation {
                location: end,
                size: self.size(),
            });
        };
        let mut node = match start_point {
            Point::InText { node, offset } => {
                if let Point::InText {
                    node: end_node,
                    offset: end_offset,
                } = end_point
                    && end_node == node
                {
                    b.delete_characters(end_offset.saturating_sub(offset));
                    return Ok(b.build());
                }
                b.delete_characters(self.view.item_size(node) - offset);
                self.view.next_sibling(node)
            }
            Point::InElement { node_after, .. } => node_after,
        };
        let stop = match end_point {
            Point::InText { node, .. } => Some(node),
            Point::InElement { node_after, .. } => node_after,
        };
        while node != stop {
            let Some(current) = node else {
                break;
            };
            if self.view.is_element(current) {
                self.delete_element_into(current, &mut b);
            } else {
                b.delete_characters(self.view.item_size(current));
            }
            node = self.view.next_sibling(current);
        }
        if let Point::InText { offset, .. } = end_point {
            b.delete_characters(offset);
        }
        Ok(b.build())
    }

    /// Deletes the content between two locations (`deleteRange`). If they
    /// are in different elements, only what lies between them is deleted,
    /// in several operations from right to left; the elements they are in
    /// are kept.
    pub fn delete_range(&mut self, start: usize, end: usize) -> Result<(), DocError> {
        self.check_range(start, end)?;
        if start == end {
            return Ok(());
        }
        let (Some(start_el), Some(end_el)) =
            (self.view.element_at(start), self.view.element_at(end))
        else {
            return Err(DocError::InvalidNode);
        };
        if start_el == end_el {
            let nindo = self.delete_range_internal(start, end)?;
            return self.consume(&nindo);
        }
        // All sections are computed up front: each one lies to the left of
        // those deleted before it, so its locations do not move.
        let mut start_ancestors = vec![start_el];
        while let Some(parent) = self.view.parent(start_ancestors[start_ancestors.len() - 1]) {
            start_ancestors.push(parent);
        }
        let mut sections = Vec::new();
        let mut right_el = end_el;
        let mut prev_right_el = end_el;
        let common_index;
        loop {
            let ancestor_index = start_ancestors.iter().position(|&el| el == right_el);
            let section_start = match ancestor_index {
                Some(0) => start,
                Some(index) => self.view.location_after(start_ancestors[index - 1]),
                None => self.view.content_start(right_el),
            };
            let section_end = if right_el == end_el {
                end
            } else {
                self.view.location(prev_right_el)
            };
            sections.push((section_start, section_end));
            prev_right_el = right_el;
            if let Some(index) = ancestor_index {
                common_index = index;
                break;
            }
            match self.view.parent(right_el) {
                Some(parent) => right_el = parent,
                // Unreachable: the root is an ancestor of every element.
                None => return Err(DocError::InvalidNode),
            }
        }
        // The tails of the start element's ancestors below the common one,
        // outermost first, then the tail of the start element itself.
        for i in (1..common_index).rev() {
            sections.push((
                self.view.location_after(start_ancestors[i - 1]),
                self.view.content_end(start_ancestors[i]),
            ));
        }
        if common_index >= 1 {
            sections.push((start, self.view.content_end(start_el)));
        }
        for (section_start, section_end) in sections {
            let nindo = self.delete_range_internal(section_start, section_end)?;
            self.consume(&nindo)?;
        }
        Ok(())
    }

    /// Sets or removes one attribute; does nothing if it already has the
    /// value (`setElementAttribute`).
    pub fn set_element_attribute(
        &mut self,
        element: NodeId,
        name: &str,
        value: Option<&str>,
    ) -> Result<(), DocError> {
        self.check_element(element)?;
        if self.view.attribute(element, name) == value {
            return Ok(());
        }
        self.update_element_attributes(element, &[(name, value)])
    }

    /// Replaces all attributes (`setElementAttributes`).
    pub fn set_element_attributes(
        &mut self,
        element: NodeId,
        attributes: Attributes,
    ) -> Result<(), DocError> {
        self.check_element(element)?;
        let mut b = Self::at(self.view.location(element));
        b.replace_attributes(attributes);
        self.consume(&b.build())
    }

    /// Sets or removes several attributes in one operation, including any
    /// that already have the value (`updateElementAttributes`).
    pub fn update_element_attributes(
        &mut self,
        element: NodeId,
        updates: &[(&str, Option<&str>)],
    ) -> Result<(), DocError> {
        self.check_element(element)?;
        let mut b = Self::at(self.view.location(element));
        b.update_attributes(
            updates
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned)))
                .collect(),
        );
        self.consume(&b.build())
    }

    fn check_persistent_key(key: &str) -> Result<(), DocError> {
        validate_annotation_key(key).map_err(|e| DocError::InvalidArgument(e.to_string()))
    }

    /// Sets annotation `key` over `start..end`; `None` removes it
    /// (`setAnnotation`).
    pub fn set_annotation(
        &mut self,
        start: usize,
        end: usize,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), DocError> {
        Self::check_persistent_key(key)?;
        self.check_range(start, end)?;
        if start == end {
            return Ok(());
        }
        self.consume(&Nindo::set_annotation(start, end, key, value))
    }

    /// Sets annotation `key` over `start..end` and removes it everywhere
    /// else (`resetAnnotation`).
    pub fn reset_annotation(
        &mut self,
        start: usize,
        end: usize,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), DocError> {
        Self::check_persistent_key(key)?;
        self.check_range(start, end)?;
        let size = self.size();
        let mut b = Nindo::builder();
        if start > 0 {
            b.start_annotation(key, None);
            b.skip(start);
        }
        if start != end {
            b.start_annotation(key, value);
            b.skip(end - start);
        }
        if size != end {
            b.start_annotation(key, None);
            b.skip(size - end);
        }
        if size == 0 {
            // The original would end an annotation it never started.
            return Ok(());
        }
        b.end_annotation(key);
        self.consume(&b.build())
    }

    /// `Annotations.guardedResetAnnotation`: [`Self::reset_annotation`]
    /// unless the document already is in that state. Returns whether an
    /// operation was emitted.
    pub fn guarded_reset_annotation(
        &mut self,
        start: usize,
        end: usize,
        key: &str,
        value: Option<&str>,
    ) -> Result<bool, DocError> {
        self.check_range(start, end)?;
        let doc = self.sink.document();
        let is_set = doc.find_annotation_change(0, start, key, None).is_none()
            && doc.find_annotation_change(start, end, key, value).is_none()
            && doc
                .find_annotation_change(end, self.size(), key, None)
                .is_none();
        if is_set {
            return Ok(false);
        }
        self.reset_annotation(start, end, key, value)?;
        Ok(true)
    }
}
