//! Operation validation: a port of `DocOpAutomaton` and `DocOpValidator`.
//!
//! The automaton accepts exactly the operations that are well-formed, apply
//! to a given document and preserve a given schema. Each `check_*` method
//! classifies a proposed next component; the matching `do_*` method performs
//! the transition. Ill-formed transitions must not be performed.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;

use super::annotation::AnnotationBoundaryMap;
use super::attributes::{AnnotationMap, AnnotationsUpdate, Attributes, AttributesUpdate};
use super::op::{DocInitialization, DocOp, DocOpComponent};
use super::schema::{DocumentSchema, NO_SCHEMA_CONSTRAINTS, PermittedCharacters};
use super::utf16::{
    cmp_utf16, is_good_for_blip, is_high_surrogate, is_low_surrogate, is_valid_text, is_xml_name,
    utf16_len,
};

/// The overall result of validating an operation, most severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValidationResult {
    /// The operation is meaningless: illegal field values or illegal
    /// sequence of components.
    IllFormed,
    /// The operation is well-formed but does not match the document.
    InvalidDocument,
    /// The operation applies but the result would violate the schema.
    InvalidSchema,
    /// The operation is valid for the document and schema.
    Valid,
}

impl ValidationResult {
    pub fn is_ill_formed(self) -> bool {
        self == ValidationResult::IllFormed
    }

    pub fn is_invalid_document(self) -> bool {
        self == ValidationResult::InvalidDocument
    }

    pub fn is_invalid_schema(self) -> bool {
        self == ValidationResult::InvalidSchema
    }

    pub fn is_valid(self) -> bool {
        self == ValidationResult::Valid
    }

    /// The more severe of the two results.
    pub fn merge_with(self, other: ValidationResult) -> ValidationResult {
        self.min(other)
    }
}

/// One reason why an operation is not valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    kind: ValidationResult,
    description: String,
    original_document_pos: usize,
    resulting_document_pos: usize,
}

impl Violation {
    pub fn validation_result(&self) -> ValidationResult {
        self.kind
    }

    /// A developer-readable description of the violation.
    pub fn description(&self) -> String {
        format!(
            "{} at original document position {} / resulting document position {}",
            self.description, self.original_document_pos, self.resulting_document_pos
        )
    }
}

/// Collects the violations found by the automaton's `check_*` methods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViolationCollector {
    operation_ill_formed: Vec<Violation>,
    operation_invalid: Vec<Violation>,
    schema_violations: Vec<Violation>,
}

impl ViolationCollector {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, violation: Violation) {
        match violation.kind {
            ValidationResult::IllFormed => self.operation_ill_formed.push(violation),
            ValidationResult::InvalidDocument => self.operation_invalid.push(violation),
            ValidationResult::InvalidSchema => self.schema_violations.push(violation),
            ValidationResult::Valid => {}
        }
    }

    pub fn is_ill_formed(&self) -> bool {
        self.validation_result().is_ill_formed()
    }

    pub fn is_invalid_document(&self) -> bool {
        self.validation_result().is_invalid_document()
    }

    pub fn is_invalid_schema(&self) -> bool {
        self.validation_result().is_invalid_schema()
    }

    pub fn is_valid(&self) -> bool {
        self.validation_result().is_valid()
    }

    /// The most severe result among the collected violations.
    pub fn validation_result(&self) -> ValidationResult {
        if !self.operation_ill_formed.is_empty() {
            ValidationResult::IllFormed
        } else if !self.operation_invalid.is_empty() {
            ValidationResult::InvalidDocument
        } else if !self.schema_violations.is_empty() {
            ValidationResult::InvalidSchema
        } else {
            ValidationResult::Valid
        }
    }

    /// All collected violations, most severe kind first.
    pub fn violations(&self) -> impl Iterator<Item = &Violation> {
        self.operation_ill_formed
            .iter()
            .chain(&self.operation_invalid)
            .chain(&self.schema_violations)
    }

    /// Description of the first (most severe) violation, if any.
    pub fn first_description(&self) -> Option<String> {
        self.violations().next().map(|v| {
            let prefix = match v.kind {
                ValidationResult::IllFormed => "ill-formed",
                ValidationResult::InvalidDocument => "invalid operation",
                _ => "schema violation",
            };
            format!("{}: {}", prefix, v.description())
        })
    }

    fn size(&self) -> usize {
        self.operation_ill_formed.len()
            + self.operation_invalid.len()
            + self.schema_violations.len()
    }
}

impl fmt::Display for ViolationCollector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.first_description() {
            None => write!(f, "ViolationCollector[0]"),
            Some(first) => write!(f, "ViolationCollector[{}: {}]", self.size(), first),
        }
    }
}

/// The document queries the automaton needs. Positions are item indices; a
/// character item is one UTF-16 code unit. All queries must tolerate
/// positions past the end of the document.
pub trait AutomatonDocument {
    /// The tag of the element starting at `pos`, if any.
    fn element_starting_at(&self, pos: usize) -> Option<&str>;

    /// The attributes of the element starting at `pos`, if any.
    fn attributes_at(&self, pos: usize) -> Option<&Attributes>;

    /// The tag of the element ending at `pos`, if any.
    fn element_ending_at(&self, pos: usize) -> Option<&str>;

    /// The UTF-16 code unit at `pos`, or `None` at an element start or end.
    fn char_at(&self, pos: usize) -> Option<u16>;

    /// Tag of the `depth`-th element enclosing the insertion point; depth 0
    /// is the innermost. `None` if nesting is not that deep.
    fn nth_enclosing_element_tag(&self, insertion_point: usize, depth: usize) -> Option<&str>;

    /// Number of characters after the insertion point before the next
    /// element start, element end or document end.
    fn remaining_characters_in_element(&self, insertion_point: usize) -> usize;

    fn annotations_at(&self, pos: usize) -> AnnotationMap;

    fn get_annotation(&self, pos: usize, key: &str) -> Option<String>;

    /// Number of items.
    fn length(&self) -> usize;

    /// First position in `start..end` whose value for `key` differs from
    /// `from_value`.
    fn first_annotation_change(
        &self,
        start: usize,
        end: usize,
        key: &str,
        from_value: Option<&str>,
    ) -> Option<usize>;
}

/// The empty document.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyDocument;

/// The empty document.
pub static EMPTY_DOCUMENT: EmptyDocument = EmptyDocument;

impl AutomatonDocument for EmptyDocument {
    fn element_starting_at(&self, _pos: usize) -> Option<&str> {
        None
    }

    fn attributes_at(&self, _pos: usize) -> Option<&Attributes> {
        None
    }

    fn element_ending_at(&self, _pos: usize) -> Option<&str> {
        None
    }

    fn char_at(&self, _pos: usize) -> Option<u16> {
        None
    }

    fn nth_enclosing_element_tag(&self, _insertion_point: usize, _depth: usize) -> Option<&str> {
        None
    }

    fn remaining_characters_in_element(&self, _insertion_point: usize) -> usize {
        0
    }

    fn annotations_at(&self, _pos: usize) -> AnnotationMap {
        AnnotationMap::new()
    }

    fn get_annotation(&self, _pos: usize, _key: &str) -> Option<String> {
        None
    }

    fn length(&self) -> usize {
        0
    }

    fn first_annotation_change(
        &self,
        _start: usize,
        _end: usize,
        _key: &str,
        _from_value: Option<&str>,
    ) -> Option<usize> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocSymbol {
    Character,
    Open,
    Close,
    End,
}

type Collector<'c> = Option<&'c mut ViolationCollector>;

fn null_or(s: Option<&str>) -> &str {
    s.unwrap_or("null")
}

/// A state machine over operation components for one document and schema.
#[derive(Clone)]
pub struct DocOpAutomaton<'a> {
    doc: &'a dyn AutomatonDocument,
    constraints: &'a dyn DocumentSchema,
    /// Position in the original document.
    effective_pos: usize,
    /// Tags of the inserted elements that are still open; last is innermost.
    insertion_stack: Vec<String>,
    next_required_element: Option<String>,
    deletion_stack_depth: usize,
    annotations_update: AnnotationsUpdate,
    after_annotation_boundary: bool,
    /// `None` once the operation has run past the end of the document.
    target_annotations_for_deletion: Option<AnnotationMap>,
    /// Position in the resulting document, for diagnostics only.
    resulting_pos: usize,
}

impl<'a> DocOpAutomaton<'a> {
    /// Creates an automaton accepting the operations valid on `doc` under
    /// `constraints`.
    pub fn new(doc: &'a dyn AutomatonDocument, constraints: &'a dyn DocumentSchema) -> Self {
        let mut automaton = DocOpAutomaton {
            doc,
            constraints,
            effective_pos: 0,
            insertion_stack: Vec::new(),
            next_required_element: None,
            deletion_stack_depth: 0,
            annotations_update: AnnotationsUpdate::new(),
            after_annotation_boundary: false,
            target_annotations_for_deletion: Some(AnnotationMap::new()),
            resulting_pos: 0,
        };
        automaton.next_required_element = automaton.required_first_child(None);
        automaton
    }

    // Violation constructors.

    fn violation(
        &self,
        v: Collector<'_>,
        kind: ValidationResult,
        description: impl FnOnce() -> String,
    ) -> ValidationResult {
        if let Some(collector) = v {
            collector.add(Violation {
                kind,
                description: description(),
                original_document_pos: self.effective_pos,
                resulting_document_pos: self.resulting_pos,
            });
        }
        kind
    }

    fn ill_formed(
        &self,
        v: Collector<'_>,
        description: impl FnOnce() -> String,
    ) -> ValidationResult {
        self.violation(v, ValidationResult::IllFormed, description)
    }

    fn invalid(&self, v: Collector<'_>, description: impl FnOnce() -> String) -> ValidationResult {
        self.violation(v, ValidationResult::InvalidDocument, description)
    }

    fn schema_violation(
        &self,
        v: Collector<'_>,
        description: impl FnOnce() -> String,
    ) -> ValidationResult {
        self.violation(v, ValidationResult::InvalidSchema, description)
    }

    fn old_annotations_differ_from_document(
        &self,
        v: Collector<'_>,
        key: &str,
        old_value: Option<&str>,
        value_in_doc: Option<&str>,
    ) -> ValidationResult {
        self.invalid(v, || {
            format!(
                "old annotations differ from document: purported old value for key {} is {}, \
                 actual value in document is {}",
                key,
                null_or(old_value),
                null_or(value_in_doc)
            )
        })
    }

    fn missing_annotation_for_deletion(
        &self,
        v: Collector<'_>,
        key: &str,
        value_in_doc: Option<&str>,
        required_value: Option<&str>,
    ) -> ValidationResult {
        self.invalid(v, || {
            format!(
                "deletion does not reset value for key {} from {} to {}",
                key,
                null_or(value_in_doc),
                null_or(required_value)
            )
        })
    }

    fn attribute_change_inside_insert_or_delete(&self, v: Collector<'_>) -> ValidationResult {
        self.ill_formed(v, || "attribute change inside insert or delete".to_owned())
    }

    fn invalid_attribute(
        &self,
        v: Collector<'_>,
        element: &str,
        name: &str,
        value: &str,
    ) -> ValidationResult {
        self.schema_violation(v, || {
            format!("type {element} does not permit attribute {name} with value {value}")
        })
    }

    // Schema accessors.

    /// The type of the first required child of `parent`, if any.
    fn required_first_child(&self, parent: Option<&str>) -> Option<String> {
        self.constraints
            .required_initial_children(parent)
            .into_iter()
            .next()
    }

    // Current state readers.

    fn effective_doc_symbol(&self) -> DocSymbol {
        if self.effective_pos >= self.doc.length() {
            DocSymbol::End
        } else if self.doc.element_starting_at(self.effective_pos).is_some() {
            DocSymbol::Open
        } else if self.doc.element_ending_at(self.effective_pos).is_some() {
            DocSymbol::Close
        } else {
            DocSymbol::Character
        }
    }

    fn insertion_stack_is_empty(&self) -> bool {
        self.insertion_stack.is_empty()
    }

    fn deletion_stack_is_empty(&self) -> bool {
        self.deletion_stack_depth == 0
    }

    /// `None` at top level.
    fn effective_enclosing_element_tag(&self) -> Option<&str> {
        if let Some(tag) = self.insertion_stack.last() {
            return Some(tag);
        }
        if self.effective_pos > self.doc.length() {
            return None;
        }
        self.doc.nth_enclosing_element_tag(self.effective_pos, 0)
    }

    /// Deviation from the original, which forbids surrogates altogether:
    /// characters outside the BMP are allowed, so validation must instead
    /// reject operations that would cut a surrogate pair of the document.
    fn splits_surrogate_pair(&self, pos: usize) -> bool {
        pos > 0
            && pos < self.doc.length()
            && self.doc.char_at(pos - 1).is_some_and(is_high_surrogate)
            && self.doc.char_at(pos).is_some_and(is_low_surrogate)
    }

    /// The maximum item count of a valid retain at this point.
    pub fn max_retain_item_count(&self) -> usize {
        self.doc.length().saturating_sub(self.effective_pos)
    }

    pub fn current_element_start_tag(&self) -> Option<&'a str> {
        self.doc.element_starting_at(self.effective_pos)
    }

    pub fn current_element_start_attributes(&self) -> Option<&'a Attributes> {
        self.doc.attributes_at(self.effective_pos)
    }

    pub fn current_annotations(&self) -> AnnotationMap {
        if self.effective_pos >= self.doc.length() {
            AnnotationMap::new()
        } else {
            self.doc.annotations_at(self.effective_pos)
        }
    }

    /// The code unit `offset` items ahead, if that item is a character.
    pub fn next_char(&self, offset: usize) -> Option<u16> {
        if offset >= self.doc.length().saturating_sub(self.effective_pos) {
            return None;
        }
        self.doc.char_at(self.effective_pos + offset)
    }

    /// Number of inserted elements that are still open.
    pub fn insertion_stack_complexity_measure(&self) -> usize {
        self.insertion_stack.len()
    }

    /// Number of deleted elements that are still open.
    pub fn deletion_stack_complexity_measure(&self) -> usize {
        self.deletion_stack_depth
    }

    /// Keys of the annotation changes currently open, in sorted order.
    pub fn open_annotations(&self) -> Vec<String> {
        self.annotations_update
            .updates()
            .iter()
            .map(|u| u.name.clone())
            .collect()
    }

    /// The maximum number of characters a `delete_characters` component can
    /// delete here, assuming one is permitted at all.
    pub fn max_characters_to_delete(&self) -> usize {
        if self.effective_pos >= self.doc.length() {
            return 0;
        }
        self.doc.remaining_characters_in_element(self.effective_pos)
    }

    /// The annotations an insertion at the current position inherits.
    pub fn inherited_annotations(&self) -> AnnotationMap {
        if self.effective_pos == 0 || self.effective_pos > self.doc.length() {
            AnnotationMap::new()
        } else {
            self.doc.annotations_at(self.effective_pos - 1)
        }
    }

    fn update_deletion_target_annotations(&mut self) {
        self.target_annotations_for_deletion = if self.effective_pos > self.doc.length() {
            None
        } else {
            Some(
                self.inherited_annotations()
                    .update_with_no_compatibility_check(&self.annotations_update),
            )
        };
    }

    // Check and transition methods.

    fn check_annotations_for_retain(
        &self,
        v: Collector<'_>,
        item_count: usize,
    ) -> ValidationResult {
        for update in self.annotations_update.updates() {
            let old_value = update.old_value.as_deref();
            if let Some(first_change) = self.doc.first_annotation_change(
                self.effective_pos,
                self.effective_pos + item_count,
                &update.name,
                old_value,
            ) {
                return self.old_annotations_differ_from_document(
                    v,
                    &update.name,
                    old_value,
                    self.doc
                        .get_annotation(first_change, &update.name)
                        .as_deref(),
                );
            }
        }
        ValidationResult::Valid
    }

    pub fn check_retain(&self, item_count: usize, v: Collector<'_>) -> ValidationResult {
        // well-formedness
        if item_count == 0 {
            return self.ill_formed(v, || "retain item count not positive".to_owned());
        }
        if !self.insertion_stack_is_empty() || !self.deletion_stack_is_empty() {
            return self.ill_formed(v, || "retain inside insert or delete".to_owned());
        }
        // validity
        if item_count > self.max_retain_item_count() {
            return self.invalid(v, || {
                format!(
                    "retain past end of document, document length {}, retain item count {}",
                    self.doc.length(),
                    item_count
                )
            });
        }
        if self.splits_surrogate_pair(self.effective_pos + item_count) {
            return self.invalid(v, || "retain ends inside a surrogate pair".to_owned());
        }
        self.check_annotations_for_retain(v, item_count)
    }

    pub fn do_retain(&mut self, item_count: usize) {
        self.effective_pos = self.effective_pos.saturating_add(item_count);
        self.update_deletion_target_annotations();
        self.resulting_pos = self.resulting_pos.saturating_add(item_count);
        self.after_annotation_boundary = false;
    }

    fn validate_annotation_key(&self, key: &str, v: Collector<'_>) -> ValidationResult {
        if key.contains('?') || key.contains('@') {
            return self.ill_formed(v, || format!("invalid character in annotation key: {key}"));
        }
        if !is_valid_text(key) {
            return self.ill_formed(v, || "annotation key is not valid UTF-16".to_owned());
        }
        ValidationResult::Valid
    }

    fn validate_annotation_value(&self, value: Option<&str>, v: Collector<'_>) -> ValidationResult {
        match value {
            Some(value) if !is_valid_text(value) => {
                self.ill_formed(v, || "annotation value is not valid UTF-16".to_owned())
            }
            _ => ValidationResult::Valid,
        }
    }

    pub fn check_annotation_boundary(
        &self,
        map: &AnnotationBoundaryMap,
        mut v: Collector<'_>,
    ) -> ValidationResult {
        // well-formedness
        if self.after_annotation_boundary {
            return self.ill_formed(v, || "adjacent annotation boundaries".to_owned());
        }
        let not_monotonic = |v: Collector<'_>, key1: &str, key2: &str| {
            self.ill_formed(v, || {
                format!("annotation keys not strictly monotonic: {key1} >= {key2}")
            })
        };
        let mut end_keys: HashSet<&str> = HashSet::new();
        let ends = map.end_keys();
        for (i, key) in ends.iter().enumerate() {
            let r = self.validate_annotation_key(key, v.as_deref_mut());
            if !r.is_valid() {
                return r;
            }
            if i > 0 && cmp_utf16(&ends[i - 1], key) != Ordering::Less {
                return not_monotonic(v, &ends[i - 1], key);
            }
            if !self.annotations_update.contains_key(key) {
                return self.ill_formed(v, || {
                    format!("annotation of key {key} ends without having started")
                });
            }
            end_keys.insert(key);
        }
        let changes = map.changes();
        for (i, change) in changes.iter().enumerate() {
            let r = self.validate_annotation_key(&change.key, v.as_deref_mut());
            if !r.is_valid() {
                return r;
            }
            let r = self.validate_annotation_value(change.old_value.as_deref(), v.as_deref_mut());
            if !r.is_valid() {
                return r;
            }
            let r = self.validate_annotation_value(change.new_value.as_deref(), v.as_deref_mut());
            if !r.is_valid() {
                return r;
            }
            if i > 0 && cmp_utf16(&changes[i - 1].key, &change.key) != Ordering::Less {
                return not_monotonic(v, &changes[i - 1].key, &change.key);
            }
            if end_keys.contains(change.key.as_str()) {
                return self.ill_formed(v, || {
                    format!("annotation boundary contains duplicate key {}", change.key)
                });
            }
        }
        ValidationResult::Valid
    }

    pub fn do_annotation_boundary(&mut self, map: &AnnotationBoundaryMap) {
        // Cannot fail for a boundary that is not ill-formed.
        if let Ok(update) = self.annotations_update.compose_with_boundary(map) {
            self.annotations_update = update;
        }
        self.after_annotation_boundary = true;
    }

    fn check_annotations_for_insertion(&self, v: Collector<'_>) -> ValidationResult {
        if self.effective_pos > self.doc.length() {
            // Invalid operation, nothing to check.
            return ValidationResult::Valid;
        }
        // Part of the surrogate deviation, see splits_surrogate_pair.
        if self.splits_surrogate_pair(self.effective_pos) {
            return self.invalid(v, || "insertion inside a surrogate pair".to_owned());
        }
        for update in self.annotations_update.updates() {
            let default_from_document = if self.effective_pos == 0 {
                None
            } else {
                self.doc
                    .get_annotation(self.effective_pos - 1, &update.name)
            };
            if update.old_value != default_from_document {
                return self.old_annotations_differ_from_document(
                    v,
                    &update.name,
                    update.old_value.as_deref(),
                    default_from_document.as_deref(),
                );
            }
        }
        ValidationResult::Valid
    }

    fn check_for_insertion_before_required_child(&self, v: Collector<'_>) -> ValidationResult {
        if self.effective_pos < self.doc.length() && self.insertion_stack_is_empty() {
            let parent_type = self.doc.nth_enclosing_element_tag(self.effective_pos, 0);
            let required_first_child = self.required_first_child(parent_type);
            let is_first_child = self.effective_pos == 0
                || self
                    .doc
                    .element_starting_at(self.effective_pos - 1)
                    .is_some();
            if is_first_child && required_first_child.is_some() {
                return self
                    .schema_violation(v, || "attempt to insert before required child".to_owned());
            }
        }
        ValidationResult::Valid
    }

    pub fn check_characters(&self, chars: &str, mut v: Collector<'_>) -> ValidationResult {
        // well-formedness
        if chars.is_empty() {
            return self.ill_formed(v, || "characters is empty".to_owned());
        }
        // Deviation from the original: no "characters component contains
        // surrogate" rule, so text outside the BMP is accepted.
        if !is_valid_text(chars) {
            return self.ill_formed(v, || {
                "characters component contains invalid unicode".to_owned()
            });
        }
        if !self.deletion_stack_is_empty() {
            return self.ill_formed(v, || "insertion inside deletion".to_owned());
        }
        // validity
        let r = self.check_annotations_for_insertion(v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        // schema
        if let Some(required) = &self.next_required_element {
            return self.schema_violation(v, || {
                format!("child element required, expected type {required}")
            });
        }
        let r = self.check_for_insertion_before_required_child(v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        let enclosing_tag = self.effective_enclosing_element_tag();
        match self.constraints.permitted_characters(enclosing_tag) {
            PermittedCharacters::None => self.schema_violation(v, || {
                format!(
                    "element type {} does not allow text content",
                    null_or(enclosing_tag)
                )
            }),
            PermittedCharacters::BlipText if !is_good_for_blip(chars) => {
                self.schema_violation(v, || {
                    format!(
                        "element type {} only allows blip text content, not arbitrary characters",
                        null_or(enclosing_tag)
                    )
                })
            }
            _ => ValidationResult::Valid,
        }
    }

    pub fn do_characters(&mut self, chars: &str) {
        self.update_deletion_target_annotations();
        self.resulting_pos = self.resulting_pos.saturating_add(utf16_len(chars));
        self.after_annotation_boundary = false;
    }

    fn check_annotations_for_deletion(
        &self,
        v: Collector<'_>,
        item_count: usize,
    ) -> ValidationResult {
        let Some(target) = &self.target_annotations_for_deletion else {
            // Invalid operation, nothing to check.
            return ValidationResult::Valid;
        };
        // All annotations in the update must have correct old and new values.
        for update in self.annotations_update.updates() {
            let key = update.name.as_str();
            let old_value = update.old_value.as_deref();
            if let Some(first_change) = self.doc.first_annotation_change(
                self.effective_pos,
                self.effective_pos + item_count,
                key,
                old_value,
            ) {
                return self.old_annotations_differ_from_document(
                    v,
                    key,
                    old_value,
                    self.doc.get_annotation(first_change, key).as_deref(),
                );
            }
            if update.new_value.as_deref() != target.get(key) {
                return self.invalid(v, || {
                    "new annotation value incorrect for deletion".to_owned()
                });
            }
        }
        // The update must contain all values that need to be reset: the keys
        // to check are those at each deleted position and those at the
        // position the deletion inherits from.
        for offset in 0..item_count {
            let annotations_here = self.doc.annotations_at(self.effective_pos + offset);
            let keys = annotations_here
                .iter()
                .map(|(key, _)| key)
                .chain(target.iter().map(|(key, _)| key));
            for key in keys {
                let value_in_doc = annotations_here.get(key);
                let required_value = target.get(key);
                if value_in_doc != required_value && !self.annotations_update.contains_key(key) {
                    return self.missing_annotation_for_deletion(
                        v,
                        key,
                        value_in_doc,
                        required_value,
                    );
                }
            }
        }
        ValidationResult::Valid
    }

    fn check_attributes_well_formed(
        &self,
        attributes: &Attributes,
        v: Collector<'_>,
    ) -> ValidationResult {
        let mut previous_key: Option<&str> = None;
        for (key, value) in attributes.iter() {
            if !is_xml_name(key) {
                return self.ill_formed(v, || {
                    format!("attribute name is not an XML Name: \"{key}\"")
                });
            }
            if !is_valid_text(value) {
                return self.ill_formed(v, || "attribute value is not valid UTF-16".to_owned());
            }
            if let Some(previous) = previous_key
                && cmp_utf16(previous, key) != Ordering::Less
            {
                return self.ill_formed(v, || {
                    format!("attribute keys not strictly monotonic: {previous} >= {key}")
                });
            }
            previous_key = Some(key);
        }
        ValidationResult::Valid
    }

    fn check_attributes_update_well_formed(
        &self,
        update: &AttributesUpdate,
        v: Collector<'_>,
    ) -> ValidationResult {
        let mut previous_key: Option<&str> = None;
        for u in update.updates() {
            let key = u.name.as_str();
            if !is_xml_name(key) {
                return self.ill_formed(v, || {
                    format!("attribute name is not an XML Name: \"{key}\"")
                });
            }
            if let Some(previous) = previous_key
                && cmp_utf16(previous, key) != Ordering::Less
            {
                return self.ill_formed(v, || {
                    format!("attribute keys not strictly monotonic: {previous} >= {key}")
                });
            }
            let valid = |value: &Option<String>| value.as_deref().is_none_or(is_valid_text);
            if !valid(&u.old_value) || !valid(&u.new_value) {
                return self.ill_formed(v, || "attribute value is not valid UTF-16".to_owned());
            }
            previous_key = Some(key);
        }
        ValidationResult::Valid
    }

    fn validate_attributes(
        &self,
        tag: &str,
        attributes: &Attributes,
        v: Collector<'_>,
    ) -> ValidationResult {
        for (key, value) in attributes.iter() {
            if !self.constraints.permits_attribute_value(tag, key, value) {
                return self.invalid_attribute(v, tag, key, value);
            }
        }
        ValidationResult::Valid
    }

    pub fn check_element_start(
        &self,
        tag: &str,
        attributes: &Attributes,
        mut v: Collector<'_>,
    ) -> ValidationResult {
        // well-formedness
        if !is_xml_name(tag) {
            return self.ill_formed(v, || format!("element type is not an XML Name: \"{tag}\""));
        }
        let r = self.check_attributes_well_formed(attributes, v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        if !self.deletion_stack_is_empty() {
            return self.ill_formed(v, || "insertion inside deletion".to_owned());
        }
        // validity
        let r = self.check_annotations_for_insertion(v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        // schema
        let r = self.validate_attributes(tag, attributes, v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        let parent_tag = self.effective_enclosing_element_tag();
        if !self.constraints.permits_child(parent_tag, tag) {
            return self.schema_violation(v, || match parent_tag {
                None => format!("element type {tag} not permitted at top level"),
                Some(parent) => {
                    format!("element type {parent} does not permit subelement type {tag}")
                }
            });
        }
        let r = self.check_for_insertion_before_required_child(v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        if let Some(required) = &self.next_required_element
            && required != tag
        {
            return self.schema_violation(v, || {
                format!("element of type {required} required, not {tag}")
            });
        }
        ValidationResult::Valid
    }

    pub fn do_element_start(&mut self, tag: &str, _attributes: &Attributes) {
        self.update_deletion_target_annotations();
        self.insertion_stack.push(tag.to_owned());
        self.next_required_element = self.required_first_child(Some(tag));
        self.resulting_pos = self.resulting_pos.saturating_add(1);
        self.after_annotation_boundary = false;
    }

    pub fn check_element_end(&self, mut v: Collector<'_>) -> ValidationResult {
        // well-formedness
        if !self.deletion_stack_is_empty() {
            return self.ill_formed(v, || "insertion inside deletion".to_owned());
        }
        if self.insertion_stack_is_empty() {
            return self.ill_formed(v, || "elementEnd with no matching elementStart".to_owned());
        }
        // validity
        let r = self.check_annotations_for_insertion(v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        // schema
        if let Some(required) = &self.next_required_element {
            return self.schema_violation(v, || {
                format!("child element required, expected type {required}")
            });
        }
        ValidationResult::Valid
    }

    pub fn do_element_end(&mut self) {
        self.update_deletion_target_annotations();
        self.insertion_stack.pop();
        self.resulting_pos = self.resulting_pos.saturating_add(1);
        self.after_annotation_boundary = false;
    }

    fn attributes_equal(a: &Attributes, b: &Attributes) -> bool {
        a.len() == b.len() && a.iter().all(|(key, value)| b.get(key) == Some(value))
    }

    pub fn check_delete_characters(&self, chars: &str, v: Collector<'_>) -> ValidationResult {
        // well-formedness
        if chars.is_empty() {
            return self.ill_formed(v, || "characters is empty".to_owned());
        }
        // Deviation from the original: surrogates are allowed, see
        // check_characters.
        if !is_valid_text(chars) {
            return self.ill_formed(v, || {
                "delete characters component contains invalid unicode".to_owned()
            });
        }
        if !self.insertion_stack_is_empty() {
            return self.ill_formed(v, || "deletion inside insertion".to_owned());
        }
        // validity
        let doc_length = self.doc.length();
        let attempted = utf16_len(chars);
        let cannot_delete_so_many = |v: Collector<'_>, available: usize| {
            self.invalid(v, || {
                format!("cannot delete {attempted} characters, only {available} available")
            })
        };
        for (offset, unit) in chars.encode_utf16().enumerate() {
            let pos = self.effective_pos.saturating_add(offset);
            if pos >= doc_length {
                return cannot_delete_so_many(v, offset);
            }
            let Some(char_here) = self.doc.char_at(pos) else {
                return cannot_delete_so_many(v, offset);
            };
            if char_here != unit {
                return self.invalid(v, || {
                    format!(
                        "attempt to delete character {} when the actual character is {}",
                        String::from_utf16_lossy(&[unit]),
                        String::from_utf16_lossy(&[char_here])
                    )
                });
            }
        }
        self.check_annotations_for_deletion(v, attempted)
    }

    pub fn do_delete_characters(&mut self, chars: &str) {
        self.effective_pos = self.effective_pos.saturating_add(utf16_len(chars));
        self.after_annotation_boundary = false;
    }

    pub fn check_delete_element_start(
        &self,
        tag: &str,
        attributes: &Attributes,
        mut v: Collector<'_>,
    ) -> ValidationResult {
        // well-formedness
        if !is_xml_name(tag) {
            return self.ill_formed(v, || format!("element type is not an XML Name: \"{tag}\""));
        }
        let r = self.check_attributes_well_formed(attributes, v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        if !self.insertion_stack_is_empty() {
            return self.ill_formed(v, || "deletion inside insertion".to_owned());
        }
        // validity
        let Some((doc_tag, doc_attributes)) = self.effective_element_start() else {
            return self.invalid(v, || "no element start to delete here".to_owned());
        };
        if doc_tag != tag {
            return self.invalid(v, || "old element type differs from document".to_owned());
        }
        if !Self::attributes_equal(attributes, doc_attributes) {
            return self.invalid(v, || "old attributes differ from document".to_owned());
        }
        let r = self.check_annotations_for_deletion(v.as_deref_mut(), 1);
        if !r.is_valid() {
            return r;
        }
        // schema
        if self.deletion_stack_depth == 0 && self.effective_pos < self.doc.length() {
            let parent_type = self.doc.nth_enclosing_element_tag(self.effective_pos, 0);
            let required_first_child = self.required_first_child(parent_type);
            let is_first_child = self.effective_pos == 0
                || self
                    .doc
                    .element_starting_at(self.effective_pos - 1)
                    .is_some();
            if is_first_child && required_first_child.is_some() {
                return self.schema_violation(v, || "attempt to delete required child".to_owned());
            }
        }
        ValidationResult::Valid
    }

    pub fn do_delete_element_start(&mut self, _tag: &str, _attributes: &Attributes) {
        self.deletion_stack_depth += 1;
        self.effective_pos = self.effective_pos.saturating_add(1);
        self.after_annotation_boundary = false;
    }

    pub fn check_delete_element_end(&self, v: Collector<'_>) -> ValidationResult {
        // well-formedness
        if !self.insertion_stack_is_empty() {
            return self.ill_formed(v, || "deletion inside insertion".to_owned());
        }
        if self.deletion_stack_is_empty() {
            return self.ill_formed(v, || {
                "deleteElementEnd with no matching deleteElementStart".to_owned()
            });
        }
        // validity
        if self.effective_doc_symbol() != DocSymbol::Close {
            return self.invalid(v, || "no element end to delete here".to_owned());
        }
        self.check_annotations_for_deletion(v, 1)
    }

    pub fn do_delete_element_end(&mut self) {
        self.deletion_stack_depth = self.deletion_stack_depth.saturating_sub(1);
        self.effective_pos = self.effective_pos.saturating_add(1);
        self.after_annotation_boundary = false;
    }

    /// The tag and attributes of the element starting at the current
    /// position, if the position is inside the document.
    fn effective_element_start(&self) -> Option<(&'a str, &'a Attributes)> {
        if self.effective_doc_symbol() != DocSymbol::Open {
            return None;
        }
        Some((
            self.doc.element_starting_at(self.effective_pos)?,
            self.doc.attributes_at(self.effective_pos)?,
        ))
    }

    pub fn check_update_attributes(
        &self,
        update: &AttributesUpdate,
        mut v: Collector<'_>,
    ) -> ValidationResult {
        // well-formedness
        let r = self.check_attributes_update_well_formed(update, v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        if !self.deletion_stack_is_empty() || !self.insertion_stack_is_empty() {
            return self.attribute_change_inside_insert_or_delete(v);
        }
        // validity
        let Some((element, old_attributes)) = self.effective_element_start() else {
            return self.invalid(v, || {
                "no element start to change attributes here".to_owned()
            });
        };
        for u in update.updates() {
            if u.old_value.as_deref() != old_attributes.get(&u.name) {
                return self.invalid(v, || "old attributes differ from document".to_owned());
            }
        }
        let r = self.check_annotations_for_retain(v.as_deref_mut(), 1);
        if !r.is_valid() {
            return r;
        }
        // schema
        for u in update.updates() {
            if let Some(value) = &u.new_value
                && !self
                    .constraints
                    .permits_attribute_value(element, &u.name, value)
            {
                return self.invalid_attribute(v, element, &u.name, value);
            }
        }
        ValidationResult::Valid
    }

    pub fn do_update_attributes(&mut self, _update: &AttributesUpdate) {
        self.effective_pos = self.effective_pos.saturating_add(1);
        self.update_deletion_target_annotations();
        self.resulting_pos = self.resulting_pos.saturating_add(1);
        self.after_annotation_boundary = false;
    }

    pub fn check_replace_attributes(
        &self,
        old_attributes: &Attributes,
        new_attributes: &Attributes,
        mut v: Collector<'_>,
    ) -> ValidationResult {
        // well-formedness
        let r = self.check_attributes_well_formed(old_attributes, v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        let r = self.check_attributes_well_formed(new_attributes, v.as_deref_mut());
        if !r.is_valid() {
            return r;
        }
        if !self.deletion_stack_is_empty() || !self.insertion_stack_is_empty() {
            return self.attribute_change_inside_insert_or_delete(v);
        }
        // validity
        let Some((element, actual_old_attributes)) = self.effective_element_start() else {
            return self.invalid(v, || {
                "no element start to change attributes here".to_owned()
            });
        };
        if !Self::attributes_equal(actual_old_attributes, old_attributes) {
            return self.invalid(v, || "old attributes differ from document".to_owned());
        }
        let r = self.check_annotations_for_retain(v.as_deref_mut(), 1);
        if !r.is_valid() {
            return r;
        }
        // schema
        self.validate_attributes(element, new_attributes, v)
    }

    pub fn do_replace_attributes(
        &mut self,
        _old_attributes: &Attributes,
        _new_attributes: &Attributes,
    ) {
        self.effective_pos = self.effective_pos.saturating_add(1);
        self.update_deletion_target_annotations();
        self.resulting_pos = self.resulting_pos.saturating_add(1);
        self.after_annotation_boundary = false;
    }

    /// Whether the operation may end here.
    pub fn check_finish(&self, v: Collector<'_>) -> ValidationResult {
        // well-formedness
        if !self.insertion_stack_is_empty() {
            return self.ill_formed(v, || "elementStart with no matching elementEnd".to_owned());
        }
        if !self.deletion_stack_is_empty() {
            return self.ill_formed(v, || {
                "deleteElementStart with no matching deleteElementEnd".to_owned()
            });
        }
        if let Some(update) = self.annotations_update.updates().first() {
            return self.ill_formed(v, || {
                format!("annotation of key {} starts but never ends", update.name)
            });
        }
        // validity
        if self.effective_pos != self.doc.length() {
            return self.invalid(v, || {
                format!(
                    "operation shorter than document, document length {}, length of input of \
                     operation {}",
                    self.doc.length(),
                    self.effective_pos
                )
            });
        }
        ValidationResult::Valid
    }

    /// Dispatches to the `check_*` method for the component.
    pub fn check_component(
        &self,
        component: &DocOpComponent,
        v: Collector<'_>,
    ) -> ValidationResult {
        match component {
            DocOpComponent::AnnotationBoundary(map) => self.check_annotation_boundary(map, v),
            DocOpComponent::Characters(chars) => self.check_characters(chars, v),
            DocOpComponent::ElementStart { tag, attributes } => {
                self.check_element_start(tag, attributes, v)
            }
            DocOpComponent::ElementEnd => self.check_element_end(v),
            DocOpComponent::Retain(item_count) => self.check_retain(*item_count, v),
            DocOpComponent::DeleteCharacters(chars) => self.check_delete_characters(chars, v),
            DocOpComponent::DeleteElementStart { tag, attributes } => {
                self.check_delete_element_start(tag, attributes, v)
            }
            DocOpComponent::DeleteElementEnd => self.check_delete_element_end(v),
            DocOpComponent::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => self.check_replace_attributes(old_attributes, new_attributes, v),
            DocOpComponent::UpdateAttributes(update) => self.check_update_attributes(update, v),
        }
    }

    /// Dispatches to the `do_*` method for the component.
    pub fn do_component(&mut self, component: &DocOpComponent) {
        match component {
            DocOpComponent::AnnotationBoundary(map) => self.do_annotation_boundary(map),
            DocOpComponent::Characters(chars) => self.do_characters(chars),
            DocOpComponent::ElementStart { tag, attributes } => {
                self.do_element_start(tag, attributes)
            }
            DocOpComponent::ElementEnd => self.do_element_end(),
            DocOpComponent::Retain(item_count) => self.do_retain(*item_count),
            DocOpComponent::DeleteCharacters(chars) => self.do_delete_characters(chars),
            DocOpComponent::DeleteElementStart { tag, attributes } => {
                self.do_delete_element_start(tag, attributes)
            }
            DocOpComponent::DeleteElementEnd => self.do_delete_element_end(),
            DocOpComponent::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => self.do_replace_attributes(old_attributes, new_attributes),
            DocOpComponent::UpdateAttributes(update) => self.do_update_attributes(update),
        }
    }
}

/// Classifies `op` against `doc` and `schema`: whether it is well-formed,
/// applies to the document and preserves the schema. Does not modify `doc`.
///
/// Checking stops at the first ill-formed component.
pub fn validate(
    mut v: Collector<'_>,
    schema: &dyn DocumentSchema,
    doc: &dyn AutomatonDocument,
    op: &DocOp,
) -> ValidationResult {
    let mut automaton = DocOpAutomaton::new(doc, schema);
    let mut accu = ValidationResult::Valid;
    for component in op {
        accu = accu.merge_with(automaton.check_component(component, v.as_deref_mut()));
        if accu.is_ill_formed() {
            return ValidationResult::IllFormed;
        }
        automaton.do_component(component);
    }
    accu.merge_with(automaton.check_finish(v))
}

/// Classifies a document initialization against the empty document.
pub fn validate_initialization(
    v: Collector<'_>,
    schema: &dyn DocumentSchema,
    op: &DocInitialization,
) -> ValidationResult {
    validate(v, schema, &EMPTY_DOCUMENT, op)
}

/// Whether `op` is well-formed, independent of any document.
///
/// Violations recorded in `v` other than well-formedness violations are
/// meaningless.
pub fn is_well_formed(v: Collector<'_>, op: &DocOp) -> bool {
    // Validating against the empty document will usually report the
    // operation as invalid; only ill-formedness matters here.
    !validate(v, &NO_SCHEMA_CONSTRAINTS, &EMPTY_DOCUMENT, op).is_ill_formed()
}
