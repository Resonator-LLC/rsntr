//! Random generator of valid document operations (`RandomDocOpGenerator`).
//!
//! The generator drives a [`DocOpAutomaton`] and only ever picks components
//! the automaton accepts as valid, so its output is valid for the document
//! by construction. It consumes random numbers in the same order as the
//! original, so that with [`RandomProviderImpl`] and the same seed both
//! produce the same operations.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use crate::docop::annotation::{AnnotationBoundaryMap, AnnotationChange};
use crate::docop::attributes::{Attributes, AttributesUpdate};
use crate::docop::op::{DocOp, DocOpComponent};
use crate::docop::schema::NO_SCHEMA_CONSTRAINTS;
use crate::docop::utf16::{Utf16Key, cmp_utf16, is_high_surrogate};
use crate::docop::validate::{AutomatonDocument, DocOpAutomaton, ValidationResult};

/// Source of random numbers for the generator.
pub trait RandomProvider {
    /// A number in `0..upper_bound`.
    fn next_int(&mut self, upper_bound: usize) -> usize;

    fn next_boolean(&mut self) -> bool {
        self.next_int(2) != 0
    }
}

/// Port of the original's simplistic linear congruential `RandomProviderImpl`.
#[derive(Debug, Clone)]
pub struct RandomProviderImpl {
    next32: i32,
}

impl RandomProviderImpl {
    pub fn of_seed(seed: i32) -> Self {
        RandomProviderImpl { next32: seed }
    }
}

impl RandomProvider for RandomProviderImpl {
    fn next_int(&mut self, upper_bound: usize) -> usize {
        assert!(upper_bound > 0, "upper_bound must be positive");
        self.next32 = 0x77DD_9E95_i64
            .wrapping_mul(self.next32 as i64)
            .wrapping_add(1) as i32;
        let d = (self.next32 as f64 + 2147483648.0) / 4294967296.0;
        (d * upper_bound as f64) as usize
    }
}

/// An annotation key the generator may use, with its possible values.
#[derive(Debug, Clone)]
pub struct AnnotationOption {
    pub key: String,
    pub value_alternatives: Vec<Option<String>>,
}

/// Tuning knobs of the generator. The defaults are those of the original.
#[derive(Debug, Clone)]
pub struct Parameters {
    pub max_opening_components: usize,
    pub max_insert_length: usize,
    pub max_delete_length: usize,
    pub element_types: Vec<String>,
    pub attribute_names: Vec<String>,
    /// Attribute removals are only generated if `None` is in this list.
    pub attribute_values: Vec<Option<String>>,
    pub annotation_options: Vec<AnnotationOption>,
    /// Not in the original: mix characters outside the BMP into inserted
    /// text. Changes the sequence of random numbers consumed.
    pub supplementary_characters: bool,
}

impl Default for Parameters {
    fn default() -> Self {
        let strings = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let option = |key: &str, values: &[Option<&str>]| AnnotationOption {
            key: key.to_owned(),
            value_alternatives: values.iter().map(|v| v.map(str::to_owned)).collect(),
        };
        let mut attribute_values = vec![None];
        attribute_values.extend(
            [
                "title", "li", "h1", "h2", "h3", "h4", "", "0", "1", "2", "3", "4", "5", "114",
                "9817",
            ]
            .iter()
            .map(|s| Some(s.to_string())),
        );
        Parameters {
            max_opening_components: 16,
            max_insert_length: 10,
            max_delete_length: 5,
            element_types: strings(&["body", "line", "input", "image", "caption", "br"]),
            attribute_names: strings(&[
                "_t",
                "t",
                "i",
                "attachment",
                "style",
                "blipId",
                "state",
                "url",
                "fontWeight",
                "fontStyle",
                "invalid_dummy",
            ]),
            attribute_values,
            annotation_options: vec![
                option("a", &[None, Some("1"), Some("2")]),
                option("b", &[None, Some("1")]),
                option("c", &[None, Some("1")]),
            ],
            supplementary_characters: false,
        }
    }
}

const SUPPLEMENTARY_CHARACTERS: [char; 4] = ['😀', '𝄞', '🚀', '\u{10FFFD}'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// All components are permitted.
    Unrestricted,
    /// Only components that close open deletions and insertions.
    CloseStructure,
    /// Close the open annotations.
    CloseAnnotations,
    /// Retain to the end of the document.
    SkipToEnd,
}

#[derive(Debug, Clone, Copy)]
enum ComponentGenerator {
    AnnotationBoundary,
    Characters,
    ElementStart,
    ElementEnd,
    Skip,
    DeleteCharacters,
    DeleteElementStart,
    DeleteElementEnd,
    ReplaceAttributes,
    UpdateAttributes,
}

const COMPONENT_GENERATORS: [ComponentGenerator; 10] = [
    ComponentGenerator::AnnotationBoundary,
    ComponentGenerator::Characters,
    ComponentGenerator::ElementStart,
    ComponentGenerator::ElementEnd,
    ComponentGenerator::Skip,
    ComponentGenerator::DeleteCharacters,
    ComponentGenerator::DeleteElementStart,
    ComponentGenerator::DeleteElementEnd,
    ComponentGenerator::ReplaceAttributes,
    ComponentGenerator::UpdateAttributes,
];

fn random_int_from_range(r: &mut dyn RandomProvider, min: usize, limit: usize) -> usize {
    r.next_int(limit - min) + min
}

fn shuffle<T>(r: &mut dyn RandomProvider, a: &mut [T]) {
    let n = a.len();
    for i in 0..n {
        let j = random_int_from_range(r, i, n);
        a.swap(i, j);
    }
}

/// Tries the elements of `input` in random order and returns the first
/// value `mapper` produces.
fn pick_random_mapped_element<I: Clone, O>(
    r: &mut dyn RandomProvider,
    input: &[I],
    mut mapper: impl FnMut(&mut dyn RandomProvider, &I) -> Option<O>,
) -> Option<O> {
    let mut list = input.to_vec();
    while !list.is_empty() {
        let index = random_int_from_range(r, 0, list.len());
        if let Some(value) = mapper(r, &list[index]) {
            return Some(value);
        }
        list.swap_remove(index);
    }
    None
}

/// Orders `None` before any string, and strings by UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NullableKey(Option<String>);

impl Ord for NullableKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (&self.0, &other.0) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => cmp_utf16(a, b),
        }
    }
}

impl PartialOrd for NullableKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct Generator<'p> {
    p: &'p Parameters,
}

impl Generator<'_> {
    fn is_valid(a: &DocOpAutomaton<'_>, component: &DocOpComponent) -> bool {
        a.check_component(component, None).is_valid()
    }

    fn valid_or_none(a: &DocOpAutomaton<'_>, component: DocOpComponent) -> Option<DocOpComponent> {
        Self::is_valid(a, &component).then_some(component)
    }

    fn generate_component(
        &self,
        generator: ComponentGenerator,
        r: &mut dyn RandomProvider,
        a: &DocOpAutomaton<'_>,
        stage: Stage,
    ) -> Option<DocOpComponent> {
        let unrestricted = stage == Stage::Unrestricted;
        let closing = stage == Stage::CloseStructure;
        let deleting = a.deletion_stack_complexity_measure() > 0;
        match generator {
            ComponentGenerator::AnnotationBoundary => match stage {
                Stage::Unrestricted | Stage::CloseStructure => {
                    self.annotation_with_lookahead(r, a, stage)
                }
                Stage::CloseAnnotations => Self::closing_annotation(a),
                Stage::SkipToEnd => None,
            },
            ComponentGenerator::Characters => self.characters(r, a, stage),
            ComponentGenerator::ElementStart => {
                if !unrestricted {
                    return None;
                }
                pick_random_mapped_element(r, &self.p.element_types, |r, tag| {
                    self.element_start_given_tag(r, a, tag)
                })
            }
            ComponentGenerator::ElementEnd => {
                if unrestricted || (closing && a.insertion_stack_complexity_measure() > 0) {
                    Self::valid_or_none(a, DocOpComponent::ElementEnd)
                } else {
                    None
                }
            }
            ComponentGenerator::Skip => Self::skip(r, a, stage),
            ComponentGenerator::DeleteCharacters => self.delete_characters(r, a, stage),
            ComponentGenerator::DeleteElementStart => {
                if !(unrestricted || (closing && deleting)) {
                    return None;
                }
                let tag = a.current_element_start_tag()?;
                let attributes = a.current_element_start_attributes()?;
                Self::valid_or_none(
                    a,
                    DocOpComponent::DeleteElementStart {
                        tag: tag.to_owned(),
                        attributes: attributes.clone(),
                    },
                )
            }
            ComponentGenerator::DeleteElementEnd => {
                if unrestricted || (closing && deleting) {
                    Self::valid_or_none(a, DocOpComponent::DeleteElementEnd)
                } else {
                    None
                }
            }
            ComponentGenerator::ReplaceAttributes => {
                if !unrestricted {
                    return None;
                }
                let old_attributes = a.current_element_start_attributes()?;
                let replace = |update: &AttributesUpdate| {
                    Some(DocOpComponent::ReplaceAttributes {
                        old_attributes: old_attributes.clone(),
                        new_attributes: old_attributes.update_with(update).ok()?,
                    })
                };
                let update = self.random_attributes_update(r, old_attributes, &|update| {
                    replace(update).is_some_and(|c| Self::is_valid(a, &c))
                })?;
                replace(&update)
            }
            ComponentGenerator::UpdateAttributes => {
                if !unrestricted {
                    return None;
                }
                let old_attributes = a.current_element_start_attributes()?;
                let update = self.random_attributes_update(r, old_attributes, &|update| {
                    a.check_update_attributes(update, None).is_valid()
                })?;
                Some(DocOpComponent::UpdateAttributes(update))
            }
        }
    }

    fn skip(
        r: &mut dyn RandomProvider,
        a: &DocOpAutomaton<'_>,
        stage: Stage,
    ) -> Option<DocOpComponent> {
        let max_distance = a.max_retain_item_count();
        match stage {
            Stage::Unrestricted => {
                if max_distance == 0 || !a.check_retain(1, None).is_valid() {
                    return None;
                }
                let mut distance = random_int_from_range(r, 1, max_distance + 1);
                while !a.check_retain(distance, None).is_valid() {
                    distance -= 1;
                }
                Some(DocOpComponent::Retain(distance))
            }
            Stage::CloseStructure | Stage::CloseAnnotations => None,
            Stage::SkipToEnd => {
                if max_distance == 0 {
                    None
                } else {
                    Self::valid_or_none(a, DocOpComponent::Retain(max_distance))
                }
            }
        }
    }

    fn characters(
        &self,
        r: &mut dyn RandomProvider,
        a: &DocOpAutomaton<'_>,
        stage: Stage,
    ) -> Option<DocOpComponent> {
        if stage != Stage::Unrestricted || !a.check_characters("a", None).is_valid() {
            return None;
        }
        let max = self.p.max_insert_length;
        if max == 0 {
            return None;
        }
        let count = random_int_from_range(r, 1, max + 1);
        let start_char = if r.next_boolean() { b'a' } else { b'A' };
        let mut s = String::new();
        for i in 0..count {
            let mut c = if i <= 26 {
                (start_char + i as u8) as char
            } else {
                '.'
            };
            if self.p.supplementary_characters && r.next_int(4) == 0 {
                c = SUPPLEMENTARY_CHARACTERS[r.next_int(SUPPLEMENTARY_CHARACTERS.len())];
            }
            s.push(c);
        }
        Some(DocOpComponent::Characters(s))
    }

    fn delete_characters(
        &self,
        r: &mut dyn RandomProvider,
        a: &DocOpAutomaton<'_>,
        stage: Stage,
    ) -> Option<DocOpComponent> {
        if stage != Stage::Unrestricted
            && (stage != Stage::CloseStructure || a.deletion_stack_complexity_measure() == 0)
        {
            return None;
        }
        a.next_char(0)?;
        if a.check_delete_characters("a", None).is_ill_formed() {
            return None;
        }
        let max = a.max_characters_to_delete().min(self.p.max_delete_length);
        if max == 0 {
            return None;
        }
        let count = random_int_from_range(r, 1, max + 1);
        // Extend the deletion one character at a time for as long as it
        // stays valid. A surrogate pair is taken as a whole, so the deletion
        // may be one code unit longer than `count`.
        let mut deleted = String::new();
        let mut units: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < count {
            let Some(unit) = a.next_char(i) else { break };
            units.push(unit);
            i += 1;
            if is_high_surrogate(unit) {
                let Some(low) = a.next_char(i) else { break };
                units.push(low);
                i += 1;
            }
            let Ok(candidate) = String::from_utf16(&units) else {
                break;
            };
            if !a.check_delete_characters(&candidate, None).is_valid() {
                break;
            }
            deleted = candidate;
        }
        if deleted.is_empty() {
            return None;
        }
        Self::valid_or_none(a, DocOpComponent::DeleteCharacters(deleted))
    }

    /// Grows a random update of `old_attributes` for as long as `is_valid`
    /// accepts it.
    fn random_attributes_update(
        &self,
        r: &mut dyn RandomProvider,
        old_attributes: &Attributes,
        is_valid: &dyn Fn(&AttributesUpdate) -> bool,
    ) -> Option<AttributesUpdate> {
        let mut accu = AttributesUpdate::new();
        if !is_valid(&accu) {
            return None;
        }
        while r.next_boolean() {
            let new_accu = pick_random_mapped_element(r, &self.p.attribute_names, |r, name| {
                if accu.contains_key(name) {
                    return None;
                }
                pick_random_mapped_element(r, &self.p.attribute_values, |_, value| {
                    let b = accu
                        .compose_with(&AttributesUpdate::single(
                            name.clone(),
                            old_attributes.get(name),
                            value.as_deref(),
                        ))
                        .ok()?;
                    is_valid(&b).then_some(b)
                })
            });
            match new_accu {
                None => return Some(accu),
                Some(new_accu) => accu = new_accu,
            }
        }
        Some(accu)
    }

    fn element_start_given_tag(
        &self,
        r: &mut dyn RandomProvider,
        a: &DocOpAutomaton<'_>,
        tag: &str,
    ) -> Option<DocOpComponent> {
        let empty = Attributes::new();
        if !a.check_element_start(tag, &empty, None).is_valid() {
            // Early exit if we can't build an element start with this tag.
            return None;
        }
        let update = self.random_attributes_update(r, &empty, &|update| {
            empty
                .update_with(update)
                .is_ok_and(|attributes| a.check_element_start(tag, &attributes, None).is_valid())
        })?;
        Some(DocOpComponent::ElementStart {
            tag: tag.to_owned(),
            attributes: empty.update_with(&update).ok()?,
        })
    }

    fn closing_annotation(a: &DocOpAutomaton<'_>) -> Option<DocOpComponent> {
        let open = a.open_annotations();
        if open.is_empty() {
            return None;
        }
        Some(DocOpComponent::AnnotationBoundary(
            AnnotationBoundaryMap::new_unchecked(open, Vec::new()),
        ))
    }

    /// Searches for an annotation boundary after which some valid component
    /// can follow.
    fn annotation_with_lookahead(
        &self,
        r: &mut dyn RandomProvider,
        a: &DocOpAutomaton<'_>,
        stage: Stage,
    ) -> Option<DocOpComponent> {
        let probe = AnnotationBoundaryMap::new_unchecked(
            Vec::new(),
            vec![AnnotationChange::new("a", None, Some("1"))],
        );
        if a.check_annotation_boundary(&probe, None).is_ill_formed() {
            return None;
        }
        let mut key_set: BTreeSet<Utf16Key> = BTreeSet::new();
        for option in &self.p.annotation_options {
            key_set.insert(Utf16Key(option.key.clone()));
        }
        for (key, _) in a.current_annotations().iter() {
            key_set.insert(Utf16Key(key.to_owned()));
        }
        for (key, _) in a.inherited_annotations().iter() {
            key_set.insert(Utf16Key(key.to_owned()));
        }
        let mut search = AnnotationSearch {
            generator: self,
            a,
            stage,
            keys: key_set.into_iter().map(|k| k.0).collect(),
            open: a.open_annotations(),
            keys_to_end: Vec::new(),
            changes: Vec::new(),
        };
        search
            .next_key(r, 0)
            .map(DocOpComponent::AnnotationBoundary)
    }

    fn pick_component(
        &self,
        r: &mut dyn RandomProvider,
        a: &DocOpAutomaton<'_>,
        stage: Stage,
    ) -> Option<DocOpComponent> {
        pick_random_mapped_element(r, &COMPONENT_GENERATORS, |r, generator| {
            self.generate_component(*generator, r, a, stage)
        })
    }
}

/// Backtracking search over annotation boundaries: for every key, either
/// leave it alone, end it (if open) or change it between any pair of
/// plausible old and new values.
struct AnnotationSearch<'s, 'a> {
    generator: &'s Generator<'s>,
    a: &'s DocOpAutomaton<'a>,
    stage: Stage,
    keys: Vec<String>,
    open: Vec<String>,
    keys_to_end: Vec<String>,
    changes: Vec<AnnotationChange>,
}

impl AnnotationSearch<'_, '_> {
    fn try_this_option(&mut self, r: &mut dyn RandomProvider) -> Option<AnnotationBoundaryMap> {
        let map =
            AnnotationBoundaryMap::new_unchecked(self.keys_to_end.clone(), self.changes.clone());
        let mut temp = self.a.clone();
        temp.do_annotation_boundary(&map);
        self.generator
            .pick_component(r, &temp, self.stage)
            .map(|_| map)
    }

    fn take(
        &mut self,
        r: &mut dyn RandomProvider,
        next_key_index: usize,
        key: &str,
    ) -> Option<AnnotationBoundaryMap> {
        if self.open.iter().any(|k| k == key) {
            self.keys_to_end.push(key.to_owned());
            let found = self.next_key(r, next_key_index);
            self.keys_to_end.pop();
            if found.is_some() {
                return found;
            }
        }
        let mut value_set: BTreeSet<NullableKey> = BTreeSet::new();
        for option in &self.generator.p.annotation_options {
            if option.key == key {
                value_set.extend(option.value_alternatives.iter().cloned().map(NullableKey));
            }
        }
        value_set.insert(NullableKey(
            self.a.inherited_annotations().get(key).map(str::to_owned),
        ));
        let mut new_values: Vec<Option<String>> = value_set.iter().map(|v| v.0.clone()).collect();
        value_set.insert(NullableKey(
            self.a.current_annotations().get(key).map(str::to_owned),
        ));
        let mut old_values: Vec<Option<String>> = value_set.into_iter().map(|v| v.0).collect();
        shuffle(r, &mut old_values);
        shuffle(r, &mut new_values);
        for old_value in &old_values {
            for new_value in &new_values {
                self.changes.push(AnnotationChange {
                    key: key.to_owned(),
                    old_value: old_value.clone(),
                    new_value: new_value.clone(),
                });
                let found = self.next_key(r, next_key_index);
                self.changes.pop();
                if found.is_some() {
                    return found;
                }
            }
        }
        None
    }

    fn next_key(
        &mut self,
        r: &mut dyn RandomProvider,
        next_key_index: usize,
    ) -> Option<AnnotationBoundaryMap> {
        if next_key_index >= self.keys.len() {
            return self.try_this_option(r);
        }
        let key = self.keys[next_key_index].clone();
        if r.next_boolean() {
            self.take(r, next_key_index + 1, &key)
                .or_else(|| self.next_key(r, next_key_index + 1))
        } else {
            self.next_key(r, next_key_index + 1)
                .or_else(|| self.take(r, next_key_index + 1, &key))
        }
    }
}

/// Generates a random operation that is valid for `doc` (without schema
/// constraints).
///
/// Panics if no valid component can be found at some point, which would
/// indicate a bug in the generator or the automaton.
pub fn generate_random_op(
    r: &mut dyn RandomProvider,
    p: &Parameters,
    doc: &dyn AutomatonDocument,
) -> DocOp {
    let generator = Generator { p };
    let mut a = DocOpAutomaton::new(doc, &NO_SCHEMA_CONSTRAINTS);
    let mut components = Vec::new();
    let mut step = |r: &mut dyn RandomProvider, a: &mut DocOpAutomaton<'_>, stage: Stage| {
        let component = generator
            .pick_component(r, a, stage)
            .unwrap_or_else(|| panic!("no valid component in stage {stage:?}"));
        a.do_component(&component);
        let is_annotation_boundary = matches!(component, DocOpComponent::AnnotationBoundary(_));
        components.push(component);
        is_annotation_boundary
    };
    let desired_num_components = random_int_from_range(r, 0, p.max_opening_components);
    let mut num_components_picked = 0;
    while num_components_picked < desired_num_components {
        if !step(r, &mut a, Stage::Unrestricted) {
            num_components_picked += 1;
        }
    }
    while a.deletion_stack_complexity_measure() > 0 {
        step(r, &mut a, Stage::CloseStructure);
    }
    while a.insertion_stack_complexity_measure() > 0 {
        step(r, &mut a, Stage::CloseStructure);
    }
    if !a.open_annotations().is_empty() {
        step(r, &mut a, Stage::CloseAnnotations);
    }
    if a.max_retain_item_count() > 0 {
        step(r, &mut a, Stage::SkipToEnd);
    }
    assert_eq!(a.check_finish(None), ValidationResult::Valid);
    DocOp::new(components).expect("generated operation is well-formed")
}
