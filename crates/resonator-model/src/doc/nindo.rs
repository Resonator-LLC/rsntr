//! Non-invertible document operations and their conversion to [`DocOp`]s.
//!
//! A [`Nindo`] says what to do to a document without saying what was there
//! before: deletions carry only sizes, attribute updates only new values,
//! annotation changes only the value to set. [`Nindo::to_doc_op`] fills in
//! the old state from the document the operation applies to, following
//! `IndexedDocumentImpl.NonInvertibleCursor` step by step so that the
//! resulting operation is the one the original would have emitted.

use std::collections::{BTreeMap, BTreeSet};

use super::DocError;
use crate::docop::{
    AnnotationBoundaryMap, AnnotationChange, AnnotationMap, AnnotationsNormalizer, Attribute,
    AttributeUpdate, Attributes, AttributesUpdate, AutomatonDocument, DocOp, DocOpComponent,
    OpSink,
};

/// One component of a [`Nindo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NindoComponent {
    /// Moves over `n` items, applying the open annotation changes to them.
    Skip(usize),
    Characters(String),
    ElementStart {
        tag: String,
        attributes: Attributes,
    },
    ElementEnd,
    /// Deletes this many UTF-16 code units of text.
    DeleteCharacters(usize),
    DeleteElementStart,
    DeleteElementEnd,
    /// Replaces all attributes of the element starting here.
    ReplaceAttributes(Attributes),
    /// Sets (`Some`) or removes (`None`) attributes of the element starting
    /// here.
    UpdateAttributes(Vec<(String, Option<String>)>),
    /// From here on, sets annotation `key` to `value` (`None` removes it).
    StartAnnotation {
        key: String,
        value: Option<String>,
    },
    EndAnnotation(String),
}

/// A non-invertible document operation. Unlike a [`DocOp`] it need not span
/// the whole document: the rest is implicitly skipped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Nindo {
    components: Vec<NindoComponent>,
}

impl Nindo {
    pub fn new(components: Vec<NindoComponent>) -> Self {
        Nindo { components }
    }

    pub fn builder() -> NindoBuilder {
        NindoBuilder::default()
    }

    pub fn components(&self) -> &[NindoComponent] {
        &self.components
    }

    /// Sets annotation `key` to `value` over `start..end`
    /// (`Nindo.setAnnotation`). Empty if the range is.
    pub fn set_annotation(start: usize, end: usize, key: &str, value: Option<&str>) -> Nindo {
        let mut b = Nindo::builder();
        if start < end {
            b.skip(start);
            b.start_annotation(key, value);
            b.skip(end - start);
            b.end_annotation(key);
        }
        b.build()
    }

    /// The non-invertible form of `op` (`Nindo.fromDocOp`), optionally
    /// without a trailing skip.
    pub fn from_doc_op(op: &DocOp, remove_trailing_skip: bool) -> Nindo {
        let mut b = Nindo::builder();
        for component in op {
            match component {
                DocOpComponent::AnnotationBoundary(map) => {
                    for key in map.end_keys() {
                        b.end_annotation(key);
                    }
                    for change in map.changes() {
                        b.start_annotation(&change.key, change.new_value.as_deref());
                    }
                }
                DocOpComponent::Characters(chars) => b.characters(chars),
                DocOpComponent::ElementStart { tag, attributes } => {
                    b.element_start(tag, attributes.clone())
                }
                DocOpComponent::ElementEnd => b.element_end(),
                DocOpComponent::Retain(count) => b.skip(*count),
                DocOpComponent::DeleteCharacters(chars) => {
                    b.delete_characters(chars.encode_utf16().count())
                }
                DocOpComponent::DeleteElementStart { .. } => b.delete_element_start(),
                DocOpComponent::DeleteElementEnd => b.delete_element_end(),
                DocOpComponent::ReplaceAttributes { new_attributes, .. } => {
                    b.replace_attributes(new_attributes.clone())
                }
                DocOpComponent::UpdateAttributes(update) => b.update_attributes(
                    update
                        .updates()
                        .iter()
                        .map(|u| (u.name.clone(), u.new_value.clone()))
                        .collect(),
                ),
            }
        }
        let mut nindo = b.build();
        if remove_trailing_skip && matches!(nindo.components.last(), Some(NindoComponent::Skip(_)))
        {
            nindo.components.pop();
        }
        nindo
    }

    /// Converts the operation to the invertible operation that has the same
    /// effect on `doc`, as the original document implementation does when
    /// it consumes a `Nindo`.
    ///
    /// The result is not range-normalized: like the original's, it can hold
    /// adjacent retains. It is not validated against a schema either.
    ///
    /// Checked against the original with random operations that mix
    /// insertions, deletions, attribute changes and annotation changes
    /// (`testdata/NindoGoldenGenerator.java`).
    pub fn to_doc_op(&self, doc: &dyn AutomatonDocument) -> Result<DocOp, DocError> {
        let mut cursor = Converter::new(doc);
        for component in &self.components {
            cursor.component(component)?;
        }
        cursor.finish()
    }
}

/// Builds a [`Nindo`]. Zero-length skips, insertions and deletions are
/// dropped, as `Nindo.Builder` does for empty text.
#[derive(Debug, Clone, Default)]
pub struct NindoBuilder {
    components: Vec<NindoComponent>,
}

impl NindoBuilder {
    pub fn build(self) -> Nindo {
        Nindo {
            components: self.components,
        }
    }

    pub fn skip(&mut self, count: usize) {
        if count > 0 {
            self.components.push(NindoComponent::Skip(count));
        }
    }

    pub fn characters(&mut self, chars: &str) {
        if !chars.is_empty() {
            self.components
                .push(NindoComponent::Characters(chars.to_owned()));
        }
    }

    pub fn element_start(&mut self, tag: &str, attributes: Attributes) {
        self.components.push(NindoComponent::ElementStart {
            tag: tag.to_owned(),
            attributes,
        });
    }

    pub fn element_end(&mut self) {
        self.components.push(NindoComponent::ElementEnd);
    }

    pub fn delete_characters(&mut self, count: usize) {
        if count > 0 {
            self.components
                .push(NindoComponent::DeleteCharacters(count));
        }
    }

    pub fn delete_element_start(&mut self) {
        self.components.push(NindoComponent::DeleteElementStart);
    }

    pub fn delete_element_end(&mut self) {
        self.components.push(NindoComponent::DeleteElementEnd);
    }

    pub fn replace_attributes(&mut self, attributes: Attributes) {
        self.components
            .push(NindoComponent::ReplaceAttributes(attributes));
    }

    pub fn update_attributes(&mut self, updates: Vec<(String, Option<String>)>) {
        self.components
            .push(NindoComponent::UpdateAttributes(updates));
    }

    pub fn start_annotation(&mut self, key: &str, value: Option<&str>) {
        self.components.push(NindoComponent::StartAnnotation {
            key: key.to_owned(),
            value: value.map(str::to_owned),
        });
    }

    pub fn end_annotation(&mut self, key: &str) {
        self.components
            .push(NindoComponent::EndAnnotation(key.to_owned()));
    }
}

type Values = BTreeMap<String, Option<String>>;

enum Event {
    /// An annotation change starting at an index, with the old value.
    Start(usize, String, Option<String>),
    End(usize, String),
}

impl Event {
    fn index(&self) -> usize {
        match self {
            Event::Start(index, ..) | Event::End(index, _) => *index,
        }
    }
}

/// Local annotations (keys starting with `@`) are never inherited.
fn is_local(key: &str) -> bool {
    key.starts_with('@')
}

/// The state of `IndexedDocumentImpl.NonInvertibleCursor`, over an
/// unmodified document: positions are positions in the old document.
struct Converter<'a> {
    doc: &'a dyn AutomatonDocument,
    size: usize,
    /// The next old item to process.
    pos: usize,
    out: AnnotationsNormalizer<Vec<DocOpComponent>>,
    requested: Values,
    new_values: Values,
    end_keys: BTreeSet<String>,
    deletion_values: Values,
    /// Old annotations of the last old item passed (skipped or deleted).
    inherited: BTreeMap<String, String>,
    /// New annotations of the last item of the new document so far.
    left: BTreeMap<String, String>,
    /// The annotation keys in use in the document. Computed on demand.
    doc_keys: Option<BTreeSet<String>>,
    /// Keys this operation has set on at least one item so far. Together
    /// with the document's keys these are the keys the original's
    /// annotation set "knows" at this point: it learns a key only when an
    /// open annotation that covers something is ended or replaced.
    applied_keys: BTreeSet<String>,
    /// For each open annotation, its value and the number of items passed
    /// or inserted when it was started.
    open: BTreeMap<String, (Option<String>, usize)>,
    /// The number of items passed or inserted so far.
    cursor: usize,
    deletion_depth: usize,
    insertion_depth: usize,
}

fn to_map(annotations: &AnnotationMap) -> BTreeMap<String, String> {
    annotations
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn apply_values(base: &BTreeMap<String, String>, values: &Values) -> BTreeMap<String, String> {
    let mut result = base.clone();
    for (key, value) in values {
        match value {
            Some(value) => result.insert(key.clone(), value.clone()),
            None => result.remove(key),
        };
    }
    result
}

impl<'a> Converter<'a> {
    fn new(doc: &'a dyn AutomatonDocument) -> Self {
        Converter {
            doc,
            size: doc.length(),
            pos: 0,
            out: AnnotationsNormalizer::new(Vec::new()),
            requested: Values::new(),
            new_values: Values::new(),
            end_keys: BTreeSet::new(),
            deletion_values: Values::new(),
            inherited: BTreeMap::new(),
            left: BTreeMap::new(),
            doc_keys: None,
            applied_keys: BTreeSet::new(),
            open: BTreeMap::new(),
            cursor: 0,
            deletion_depth: 0,
            insertion_depth: 0,
        }
    }

    fn start(&mut self, key: &str, old_value: Option<&str>, new_value: Option<&str>) {
        self.out.push(DocOpComponent::AnnotationBoundary(
            AnnotationBoundaryMap::new_unchecked(
                Vec::new(),
                vec![AnnotationChange::new(key, old_value, new_value)],
            ),
        ));
    }

    fn end(&mut self, key: &str) {
        self.out.push(DocOpComponent::AnnotationBoundary(
            AnnotationBoundaryMap::new_unchecked(vec![key.to_owned()], Vec::new()),
        ));
    }

    fn old_annotations(&self, pos: usize) -> BTreeMap<String, String> {
        to_map(&self.doc.annotations_at(pos))
    }

    fn check_remaining(&self, count: usize, what: &str) -> Result<(), DocError> {
        if count > self.size - self.pos {
            return Err(DocError::InvalidOperation(format!(
                "{what} past end of document [location: {}] [doc size: {}]",
                self.pos + count,
                self.size
            )));
        }
        Ok(())
    }

    fn component(&mut self, component: &NindoComponent) -> Result<(), DocError> {
        match component {
            NindoComponent::Skip(count) => self.skip(*count),
            NindoComponent::Characters(chars) => {
                self.begin_insert();
                self.out.push(DocOpComponent::Characters(chars.clone()));
                self.inserted(chars.encode_utf16().count());
                Ok(())
            }
            NindoComponent::ElementStart { tag, attributes } => {
                self.begin_insert();
                self.out.push(DocOpComponent::ElementStart {
                    tag: tag.clone(),
                    attributes: attributes.clone(),
                });
                self.insertion_depth += 1;
                self.inserted(1);
                Ok(())
            }
            NindoComponent::ElementEnd => {
                if self.insertion_depth == 0 {
                    return Err(DocError::InvalidOperation(
                        "element end without a matching start".to_owned(),
                    ));
                }
                self.begin_insert();
                self.out.push(DocOpComponent::ElementEnd);
                self.insertion_depth -= 1;
                self.inserted(1);
                Ok(())
            }
            NindoComponent::DeleteCharacters(count) => self.delete_characters(*count),
            NindoComponent::DeleteElementStart => self.delete_element_start(),
            NindoComponent::DeleteElementEnd => self.delete_element_end(),
            NindoComponent::ReplaceAttributes(new_attributes) => {
                let old_attributes = self.begin_attribute_change()?;
                self.out.push(DocOpComponent::ReplaceAttributes {
                    old_attributes,
                    new_attributes: new_attributes.clone(),
                });
                Ok(())
            }
            NindoComponent::UpdateAttributes(updates) => {
                let old_attributes = self.begin_attribute_change()?;
                let updates = updates
                    .iter()
                    .map(|(name, new_value)| {
                        AttributeUpdate::new(
                            name.as_str(),
                            old_attributes.get(name),
                            new_value.as_deref(),
                        )
                    })
                    .collect();
                let update = AttributesUpdate::from_unsorted(updates)
                    .map_err(|e| DocError::InvalidOperation(e.to_string()))?;
                self.out.push(DocOpComponent::UpdateAttributes(update));
                Ok(())
            }
            NindoComponent::StartAnnotation { key, value } => {
                self.end_keys.remove(key);
                if self.open.get(key).map(|(open_value, _)| open_value) != Some(value) {
                    self.close(key);
                    self.open.insert(key.clone(), (value.clone(), self.cursor));
                }
                self.requested.insert(key.clone(), value.clone());
                self.new_values.insert(key.clone(), value.clone());
                Ok(())
            }
            NindoComponent::EndAnnotation(key) => {
                if self.requested.remove(key).is_none() {
                    return Err(DocError::InvalidOperation(format!(
                        "end of annotation {key} that was not started"
                    )));
                }
                self.new_values.remove(key);
                if self.deletion_values.contains_key(key) {
                    self.end_keys.insert(key.clone());
                }
                self.close(key);
                Ok(())
            }
        }
    }

    /// Ends an open annotation as the annotation set sees it.
    fn close(&mut self, key: &str) {
        if let Some((_, start)) = self.open.remove(key)
            && self.cursor > start
        {
            self.applied_keys.insert(key.to_owned());
        }
    }

    fn inserted(&mut self, count: usize) {
        self.cursor += count;
        self.left = apply_values(&self.inherited, &self.requested);
    }

    fn close_end_keys(&mut self) {
        for key in std::mem::take(&mut self.end_keys) {
            self.end(&key);
        }
    }

    fn begin_insert(&mut self) {
        for (key, value) in std::mem::take(&mut self.new_values) {
            let inherited = if is_local(&key) {
                None
            } else {
                self.inherited.get(&key).cloned()
            };
            self.start(&key, inherited.as_deref(), value.as_deref());
        }
        self.deletion_values = self.requested.clone();
        self.close_end_keys();
    }

    /// Expects `pos` to be inside the document.
    fn begin_update(&mut self) {
        let current = self.old_annotations(self.pos);
        for (key, value) in self.requested.clone() {
            self.start(
                &key,
                current.get(&key).map(String::as_str),
                value.as_deref(),
            );
        }
        self.new_values.clear();
        self.deletion_values = self.requested.clone();
        self.close_end_keys();
    }

    fn passed(&mut self, count: usize) {
        self.pos += count;
        self.cursor += count;
        self.inherited = self.old_annotations(self.pos - 1);
        self.left = apply_values(&self.inherited, &self.requested);
    }

    fn maybe_renew_annotation(&mut self, key: &str) {
        if !self.new_values.contains_key(key)
            && let Some(value) = self.requested.get(key)
        {
            self.new_values.insert(key.to_owned(), value.clone());
        }
    }

    fn skip(&mut self, count: usize) -> Result<(), DocError> {
        if count == 0 {
            return Ok(());
        }
        self.check_remaining(count, "Retain")?;
        self.begin_update();
        let final_pos = self.pos + count;
        let mut current = self.pos;
        if !self.requested.is_empty() {
            let mut events = Vec::new();
            let mut open = BTreeSet::new();
            let keys: Vec<String> = self.requested.keys().cloned().collect();
            let mut previous: Option<Vec<Option<String>>> = None;
            for index in self.pos..final_pos {
                let annotations = self.doc.annotations_at(index);
                let here: Vec<Option<String>> = keys
                    .iter()
                    .map(|key| annotations.get(key).map(str::to_owned))
                    .collect();
                if previous.as_ref() == Some(&here) {
                    continue;
                }
                for (key, old_value) in keys.iter().zip(&here) {
                    if self.requested.get(key) != Some(old_value) {
                        events.push(Event::Start(index, key.clone(), old_value.clone()));
                        open.insert(key.clone());
                    } else if open.remove(key) {
                        events.push(Event::End(index, key.clone()));
                    }
                }
                previous = Some(here);
            }
            for key in open {
                events.push(Event::End(final_pos, key));
            }
            for event in events {
                if event.index() > current {
                    self.out.retain(event.index() - current);
                    current = event.index();
                }
                match event {
                    Event::End(_, key) => {
                        self.maybe_renew_annotation(&key);
                        self.end(&key);
                    }
                    Event::Start(_, key, old_value) => {
                        let new_value = self.requested.get(&key).cloned().flatten();
                        self.start(&key, old_value.as_deref(), new_value.as_deref());
                    }
                }
            }
        }
        if current < final_pos {
            self.out.retain(final_pos - current);
        }
        self.passed(count);
        Ok(())
    }

    fn begin_attribute_change(&mut self) -> Result<Attributes, DocError> {
        let Some(attributes) = self.doc.attributes_at(self.pos).cloned() else {
            return Err(DocError::InvalidOperation(format!(
                "no element start to change attributes of at {}",
                self.pos
            )));
        };
        self.begin_update();
        self.passed(1);
        Ok(attributes)
    }

    fn known_keys(&mut self) -> Vec<String> {
        let (doc, size) = (self.doc, self.size);
        let doc_keys = self.doc_keys.get_or_insert_with(|| {
            let mut keys = BTreeSet::new();
            let mut previous = AnnotationMap::new();
            for index in 0..size {
                let annotations = doc.annotations_at(index);
                if annotations != previous {
                    keys.extend(annotations.iter().map(|(key, _)| key.to_owned()));
                    previous = annotations;
                }
            }
            keys
        });
        doc_keys.union(&self.applied_keys).cloned().collect()
    }

    /// The annotation events for deleting `size` items at the current
    /// position (`deleteAnnotations`): every known annotation is restarted
    /// at each annotation interval of the deleted range and ended after it.
    fn delete_annotations(&mut self, size: usize) -> Vec<Event> {
        let (start, end) = (self.pos, self.pos + size);
        let keys = self.known_keys();
        let mut events = Vec::new();
        let mut open = BTreeSet::new();
        let mut previous: Option<Vec<Option<String>>> = None;
        for index in start..end {
            let annotations = self.doc.annotations_at(index);
            let here: Vec<Option<String>> = keys
                .iter()
                .map(|key| annotations.get(key).map(str::to_owned))
                .collect();
            if previous.as_ref() == Some(&here) {
                continue;
            }
            for key in self.deletion_values.keys() {
                events.push(Event::Start(
                    index,
                    key.clone(),
                    annotations.get(key).map(str::to_owned),
                ));
                open.insert(key.clone());
            }
            for (key, value) in keys.iter().zip(&here) {
                if !self.deletion_values.contains_key(key) {
                    events.push(Event::Start(index, key.clone(), value.clone()));
                    open.insert(key.clone());
                }
            }
            previous = Some(here);
        }
        for key in open {
            events.push(Event::End(end, key));
        }
        events
    }

    fn left_neighbour_annotation(&self, key: &str) -> Option<String> {
        match self.deletion_values.get(key) {
            Some(value) => value.clone(),
            None => self.left.get(key).cloned(),
        }
    }

    fn deleted(&mut self, count: usize) {
        self.pos += count;
        self.inherited = self.old_annotations(self.pos - 1);
    }

    fn delete_characters(&mut self, count: usize) -> Result<(), DocError> {
        if count == 0 {
            return Ok(());
        }
        self.check_remaining(count, "Deletion")?;
        let mut units = Vec::with_capacity(count);
        for index in self.pos..self.pos + count {
            match self.doc.char_at(index) {
                Some(unit) => units.push(unit),
                None => {
                    return Err(DocError::InvalidOperation(format!(
                        "no character to delete at {index}"
                    )));
                }
            }
        }
        let piece = |from: usize, to: usize| {
            String::from_utf16(&units[from..to]).map_err(|_| {
                DocError::InvalidOperation("deletion splits a surrogate pair".to_owned())
            })
        };
        let events = self.delete_annotations(count);
        let mut index = 0;
        for event in events {
            let new_index = event.index() - self.pos;
            if new_index > index {
                let chars = piece(index, new_index)?;
                self.out.push(DocOpComponent::DeleteCharacters(chars));
                index = new_index;
            }
            match event {
                Event::End(_, key) => {
                    self.maybe_renew_annotation(&key);
                    self.end(&key);
                }
                Event::Start(_, key, old_value) => {
                    let new_value = self.left_neighbour_annotation(&key);
                    self.start(&key, old_value.as_deref(), new_value.as_deref());
                }
            }
        }
        if index < count {
            let chars = piece(index, count)?;
            self.out.push(DocOpComponent::DeleteCharacters(chars));
        }
        self.deleted(count);
        Ok(())
    }

    /// Deletes one element start or end (`doSingleDelete`).
    fn single_delete(&mut self, delete: DocOpComponent) {
        let events = self.delete_annotations(1);
        let mut delete = Some(delete);
        for event in events {
            if event.index() > self.pos
                && let Some(delete) = delete.take()
            {
                self.out.push(delete);
            }
            match event {
                Event::End(_, key) => {
                    self.maybe_renew_annotation(&key);
                    self.end(&key);
                }
                Event::Start(_, key, old_value) => {
                    let new_value = self.left_neighbour_annotation(&key);
                    self.start(&key, old_value.as_deref(), new_value.as_deref());
                }
            }
        }
        if let Some(delete) = delete {
            self.out.push(delete);
        }
        self.deleted(1);
    }

    fn delete_element_start(&mut self) -> Result<(), DocError> {
        let (Some(tag), Some(attributes)) = (
            self.doc.element_starting_at(self.pos),
            self.doc.attributes_at(self.pos),
        ) else {
            return Err(DocError::InvalidOperation(format!(
                "no element to delete at {}",
                self.pos
            )));
        };
        let delete = DocOpComponent::DeleteElementStart {
            tag: tag.to_owned(),
            attributes: attributes.clone(),
        };
        self.single_delete(delete);
        self.deletion_depth += 1;
        Ok(())
    }

    fn delete_element_end(&mut self) -> Result<(), DocError> {
        if self.deletion_depth == 0 || self.doc.element_ending_at(self.pos).is_none() {
            return Err(DocError::InvalidOperation(format!(
                "no element end to delete at {}",
                self.pos
            )));
        }
        self.single_delete(DocOpComponent::DeleteElementEnd);
        self.deletion_depth -= 1;
        Ok(())
    }

    fn finish(mut self) -> Result<DocOp, DocError> {
        if let Some(key) = self.requested.keys().next() {
            return Err(DocError::InvalidOperation(format!(
                "annotation {key} still open at end of operation"
            )));
        }
        if self.deletion_depth != 0 || self.insertion_depth != 0 {
            return Err(DocError::InvalidOperation(
                "unbalanced elements at end of operation".to_owned(),
            ));
        }
        self.close_end_keys();
        let remaining = self.size - self.pos;
        if remaining > 0 {
            self.out.retain(remaining);
        }
        Ok(DocOp::new_unchecked(self.out.finish()))
    }
}

/// Sorted attributes from name/value pairs, the later of two equal names
/// winning (a Java `Map` built by successive `put`s).
pub(crate) fn attributes_from_pairs<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Attributes {
    let map: BTreeMap<&str, &str> = pairs.into_iter().collect();
    Attributes::from_unsorted_unchecked(
        map.into_iter()
            .map(|(name, value)| Attribute::new(name, value))
            .collect(),
    )
}
