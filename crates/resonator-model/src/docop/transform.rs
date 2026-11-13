//! Transformation of concurrent document operations (`Transformer`,
//! `Decomposer`, `InsertionTransformer`, `InsertionNoninsertionTransformer`
//! and `NoninsertionTransformer`).
//!
//! Where the original uses two "target" objects that reference each other,
//! each transformer here is one struct holding the state of both sides.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::annotation::{AnnotationBoundaryMap, AnnotationChange};
use super::attributes::{Attributes, AttributesUpdate};
use super::compose::{ValueUpdate, compose};
use super::error::{OperationError, TransformError};
use super::normalize::{OpNormalizer, RangeNormalizer, new_normalizer};
use super::op::{DocOp, DocOpComponent, OpSink};
use super::utf16::{split_at_utf16, utf16_len};

/// The result of transforming a client operation against a server operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationPair {
    /// The client operation, transformed to apply after the server's.
    pub client_op: DocOp,
    /// The server operation, transformed to apply after the client's.
    pub server_op: DocOp,
}

/// Transforms a pair of operations that apply to the same document, such
/// that `compose(client, server') == compose(server, client')` in effect.
pub fn transform(client_op: &DocOp, server_op: &DocOp) -> Result<OperationPair, TransformError> {
    // Each operation is decomposed into an insertion operation followed by
    // an insertion-free operation, and four transforms are performed,
    // structured as in the following diagram:
    //     ci0     cn0
    // si0     si1     si2
    //     ci1     cn1
    // sn0     sn1     sn2
    //     ci2     cn2
    let (ci0, cn0) = decompose(client_op)?;
    let (si0, sn0) = decompose(server_op)?;
    let (ci1, si1) = transform_insertions(&ci0, &si0)?;
    let (ci2, sn1) = transform_insertion_noninsertion(&ci1, &sn0)?;
    let (si2, cn1) = transform_insertion_noninsertion(&si1, &cn0)?;
    let (cn2, sn2) = transform_noninsertions(&cn1, &sn1)?;
    Ok(OperationPair {
        client_op: compose(&ci2, &cn2)?,
        server_op: compose(&si2, &sn2)?,
    })
}

/// Decomposes an operation into an insertion operation and an operation
/// that contains no insertions, such that their composition is equivalent
/// to the original.
pub fn decompose(op: &DocOp) -> Result<(DocOp, DocOp), OperationError> {
    use DocOpComponent as C;
    let mut insertion_op = new_normalizer();
    let mut noninsertion_op = new_normalizer();
    for component in op {
        match component {
            C::Retain(item_count) => {
                insertion_op.retain(*item_count);
                noninsertion_op.retain(*item_count);
            }
            C::Characters(chars) => {
                insertion_op.push(component.clone());
                noninsertion_op.retain(utf16_len(chars));
            }
            C::ElementStart { .. } | C::ElementEnd => {
                insertion_op.push(component.clone());
                noninsertion_op.retain(1);
            }
            C::DeleteCharacters(chars) => {
                insertion_op.retain(utf16_len(chars));
                noninsertion_op.push(component.clone());
            }
            C::DeleteElementStart { .. }
            | C::DeleteElementEnd
            | C::ReplaceAttributes { .. }
            | C::UpdateAttributes(_) => {
                insertion_op.retain(1);
                noninsertion_op.push(component.clone());
            }
            C::AnnotationBoundary(_) => noninsertion_op.push(component.clone()),
        }
    }
    Ok((
        DocOp::new(insertion_op.finish())?,
        DocOp::new(noninsertion_op.finish())?,
    ))
}

/// Tracks the positions of two cursors relative to each other, in items of
/// the document both operations apply to (`PositionTracker`).
#[derive(Default)]
struct PositionTracker {
    /// Position of the first cursor minus position of the second.
    position: i64,
}

/// Returned by the `resolve_range` methods when the whole range was
/// resolved. As in the original, indistinguishable from a resolved portion
/// of -1, which callers treat the same way.
const ENTIRELY_RESOLVED: i64 = -1;

/// Index of one of the two operations being transformed.
type Side = usize;
const FIRST: Side = 0;
const SECOND: Side = 1;

impl PositionTracker {
    /// Position of `side`'s cursor relative to the other side's.
    fn get(&self, side: Side) -> i64 {
        if side == FIRST {
            self.position
        } else {
            -self.position
        }
    }

    fn increase(&mut self, side: Side, amount: usize) {
        // Saturating, and never i64::MIN, so that negation cannot overflow
        // even for absurd retain counts.
        let amount = i64::try_from(amount).unwrap_or(i64::MAX);
        let position = if side == FIRST {
            self.position.saturating_add(amount)
        } else {
            self.position.saturating_sub(amount)
        };
        self.position = position.max(-i64::MAX);
    }
}

/// Feeds the components of two operations to `step` in a linearly ordered
/// interleaving: the first operation leads, and the second catches up
/// whenever the first cursor is ahead.
fn interleave<T>(
    state: &mut T,
    ops: [&DocOp; 2],
    names: [&'static str; 2],
    position: impl Fn(&T) -> i64,
    mut step: impl FnMut(&mut T, Side, &DocOpComponent) -> Result<(), TransformError>,
) -> Result<(), TransformError> {
    let mut second = ops[SECOND].iter();
    for (index, component) in ops[FIRST].iter().enumerate() {
        step(state, FIRST, component)?;
        while position(state) > 0 {
            let Some(component) = second.next() else {
                return Err(TransformError::RanOut {
                    this: names[FIRST],
                    other: names[SECOND],
                    other_size: ops[SECOND].len(),
                    index: index + 1,
                    size: ops[FIRST].len(),
                    spare: position(state),
                });
            };
            step(state, SECOND, component)?;
        }
    }
    for component in second {
        step(state, SECOND, component)?;
    }
    Ok(())
}

fn unexpected(component: &DocOpComponent, op: &'static str) -> TransformError {
    TransformError::UnexpectedComponent {
        component: component.type_name(),
        op,
    }
}

// ---------------------------------------------------------------------------
// Insertion vs insertion

struct InsertionTransformer {
    outputs: [RangeNormalizer<Vec<DocOpComponent>>; 2],
    tracker: PositionTracker,
}

impl InsertionTransformer {
    fn step(&mut self, side: Side, component: &DocOpComponent) -> Result<(), TransformError> {
        use DocOpComponent as C;
        let other = 1 - side;
        match component {
            C::Retain(item_count) => {
                let old_position = self.tracker.get(side);
                self.tracker.increase(side, *item_count);
                if self.tracker.get(side) < 0 {
                    self.outputs[side].retain(*item_count);
                    self.outputs[other].retain(*item_count);
                } else if old_position < 0 {
                    self.outputs[side].retain(-old_position as usize);
                    self.outputs[other].retain(-old_position as usize);
                }
            }
            C::Characters(chars) => {
                self.outputs[side].push(component.clone());
                self.outputs[other].retain(utf16_len(chars));
            }
            C::ElementStart { .. } | C::ElementEnd => {
                self.outputs[side].push(component.clone());
                self.outputs[other].retain(1);
            }
            _ => return Err(unexpected(component, "insertion")),
        }
        Ok(())
    }
}

/// Transforms a pair of insertion operations. On a tie the client's
/// insertion ends up before the server's.
fn transform_insertions(
    client_op: &DocOp,
    server_op: &DocOp,
) -> Result<(DocOp, DocOp), TransformError> {
    let mut transformer = InsertionTransformer {
        outputs: [
            RangeNormalizer::new(Vec::new()),
            RangeNormalizer::new(Vec::new()),
        ],
        tracker: PositionTracker::default(),
    };
    interleave(
        &mut transformer,
        [client_op, server_op],
        ["client", "server"],
        |t| t.tracker.get(FIRST),
        InsertionTransformer::step,
    )?;
    let [client, server] = transformer.outputs;
    Ok((DocOp::new(client.finish())?, DocOp::new(server.finish())?))
}

// ---------------------------------------------------------------------------
// Insertion vs insertion-free

/// The effect of the most recent component of an insertion-free operation on
/// the range that the other operation has yet to pass.
enum RangeCache {
    Retain,
    DeleteCharacters(String),
    DeleteElementStart {
        tag: String,
        attributes: Attributes,
    },
    DeleteElementEnd,
    ReplaceAttributes {
        old_attributes: Attributes,
        new_attributes: Attributes,
    },
    UpdateAttributes(AttributesUpdate),
}

struct InsertionNoninsertionTransformer {
    insertion_output: RangeNormalizer<Vec<DocOpComponent>>,
    noninsertion_output: OpNormalizer,
    /// Insertion cursor is FIRST, insertion-free cursor is SECOND.
    tracker: PositionTracker,
    range_cache: RangeCache,
    /// Current depth of element deletions.
    depth: i64,
}

impl InsertionNoninsertionTransformer {
    /// Emits the cached insertion-free component for the next `item_count`
    /// items.
    fn resolve(&mut self, cache: &mut RangeCache, item_count: usize) -> Result<(), TransformError> {
        match cache {
            RangeCache::Retain => {
                self.noninsertion_output.retain(item_count);
                self.insertion_output.retain(item_count);
            }
            RangeCache::DeleteCharacters(characters) => {
                let (head, tail) = split_at_utf16(characters, item_count)?;
                self.noninsertion_output.delete_characters(head);
                *characters = tail.to_owned();
            }
            RangeCache::DeleteElementStart { tag, attributes } => {
                self.noninsertion_output
                    .delete_element_start(tag.as_str(), attributes.clone());
                self.depth += 1;
            }
            RangeCache::DeleteElementEnd => {
                self.noninsertion_output.delete_element_end();
                self.depth -= 1;
            }
            RangeCache::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => {
                self.noninsertion_output
                    .replace_attributes(old_attributes.clone(), new_attributes.clone());
                self.insertion_output.retain(1);
            }
            RangeCache::UpdateAttributes(update) => {
                self.noninsertion_output.update_attributes(update.clone());
                self.insertion_output.retain(1);
            }
        }
        Ok(())
    }

    /// Advances the insertion-free cursor by `size` and resolves as much of
    /// `cache` as the insertion cursor has already passed. Returns the
    /// resolved portion, or -1 if the entire range was resolved.
    fn resolve_range(
        &mut self,
        size: usize,
        cache: &mut RangeCache,
    ) -> Result<i64, TransformError> {
        let old_position = self.tracker.get(SECOND);
        self.tracker.increase(SECOND, size);
        if self.tracker.get(SECOND) > 0 {
            if old_position < 0 {
                self.resolve(cache, -old_position as usize)?;
            }
            Ok(-old_position)
        } else {
            self.resolve(cache, size)?;
            Ok(ENTIRELY_RESOLVED)
        }
    }

    fn step(&mut self, side: Side, component: &DocOpComponent) -> Result<(), TransformError> {
        if side == FIRST {
            self.step_insertion(component)
        } else {
            self.step_noninsertion(component)
        }
    }

    fn step_insertion(&mut self, component: &DocOpComponent) -> Result<(), TransformError> {
        use DocOpComponent as C;
        match component {
            C::Retain(item_count) => {
                let old_position = self.tracker.get(FIRST);
                self.tracker.increase(FIRST, *item_count);
                let resolved = if self.tracker.get(FIRST) < 0 {
                    Some(*item_count)
                } else if old_position < 0 {
                    Some(-old_position as usize)
                } else {
                    None
                };
                if let Some(resolved) = resolved {
                    let mut cache = std::mem::replace(&mut self.range_cache, RangeCache::Retain);
                    let result = self.resolve(&mut cache, resolved);
                    self.range_cache = cache;
                    result?;
                }
            }
            C::Characters(chars) => {
                if self.depth > 0 {
                    self.noninsertion_output.delete_characters(chars.as_str());
                } else {
                    self.insertion_output.push(component.clone());
                    self.noninsertion_output.retain(utf16_len(chars));
                }
            }
            C::ElementStart { tag, attributes } => {
                if self.depth > 0 {
                    self.noninsertion_output
                        .delete_element_start(tag.as_str(), attributes.clone());
                } else {
                    self.insertion_output.push(component.clone());
                    self.noninsertion_output.retain(1);
                }
            }
            C::ElementEnd => {
                if self.depth > 0 {
                    self.noninsertion_output.delete_element_end();
                } else {
                    self.insertion_output.push(component.clone());
                    self.noninsertion_output.retain(1);
                }
            }
            _ => return Err(unexpected(component, "insertion")),
        }
        Ok(())
    }

    fn step_noninsertion(&mut self, component: &DocOpComponent) -> Result<(), TransformError> {
        use DocOpComponent as C;
        let (size, mut cache) = match component {
            C::Retain(item_count) => {
                self.resolve_range(*item_count, &mut RangeCache::Retain)?;
                self.range_cache = RangeCache::Retain;
                return Ok(());
            }
            C::AnnotationBoundary(_) => {
                self.noninsertion_output.push(component.clone());
                return Ok(());
            }
            C::DeleteCharacters(chars) => {
                // Resolution trims the cache down to the unresolved part.
                let mut cache = RangeCache::DeleteCharacters(chars.clone());
                if self.resolve_range(utf16_len(chars), &mut cache)? >= 0 {
                    self.range_cache = cache;
                }
                return Ok(());
            }
            C::DeleteElementStart { tag, attributes } => (
                1,
                RangeCache::DeleteElementStart {
                    tag: tag.clone(),
                    attributes: attributes.clone(),
                },
            ),
            C::DeleteElementEnd => (1, RangeCache::DeleteElementEnd),
            C::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => (
                1,
                RangeCache::ReplaceAttributes {
                    old_attributes: old_attributes.clone(),
                    new_attributes: new_attributes.clone(),
                },
            ),
            C::UpdateAttributes(update) => (1, RangeCache::UpdateAttributes(update.clone())),
            _ => return Err(unexpected(component, "insertion-free")),
        };
        if self.resolve_range(size, &mut cache)? == 0 {
            self.range_cache = cache;
        }
        Ok(())
    }
}

/// Transforms an insertion operation against an insertion-free operation.
/// Insertions that fall inside a deleted element are deleted along with it.
fn transform_insertion_noninsertion(
    insertion_op: &DocOp,
    noninsertion_op: &DocOp,
) -> Result<(DocOp, DocOp), TransformError> {
    let mut transformer = InsertionNoninsertionTransformer {
        insertion_output: RangeNormalizer::new(Vec::new()),
        noninsertion_output: new_normalizer(),
        tracker: PositionTracker::default(),
        range_cache: RangeCache::Retain,
        depth: 0,
    };
    interleave(
        &mut transformer,
        [insertion_op, noninsertion_op],
        ["insertion", "noninsertion"],
        |t| t.tracker.get(FIRST),
        InsertionNoninsertionTransformer::step,
    )?;
    Ok((
        DocOp::new(transformer.insertion_output.finish())?,
        DocOp::new(transformer.noninsertion_output.finish())?,
    ))
}

// ---------------------------------------------------------------------------
// Insertion-free vs insertion-free

const CLIENT: Side = 0;
const SERVER: Side = 1;

/// Tracks annotation changes of one operation at the current cursor
/// position.
#[derive(Default)]
struct AnnotationTracker {
    /// Changes open in the original operation.
    tracked: HashMap<String, ValueUpdate>,
    /// Changes open in the transformed output.
    active: HashMap<String, ValueUpdate>,
    /// Changes to restore when the current deletion concludes; `None` means
    /// the key is to be ended.
    temporary: BTreeMap<String, Option<ValueUpdate>>,
    /// What `active` held, per key, when the key last changed since the
    /// trackers were synced.
    propagating: BTreeMap<String, Option<ValueUpdate>>,
}

struct NoninsertionSide {
    output: OpNormalizer,
    tracker: AnnotationTracker,
    range_cache: RangeCache,
}

impl NoninsertionSide {
    fn new() -> Self {
        NoninsertionSide {
            output: new_normalizer(),
            tracker: AnnotationTracker::default(),
            range_cache: RangeCache::Retain,
        }
    }
}

// The original also tracks an element deletion depth per side here, but
// never reads it, so it is omitted.
struct NoninsertionTransformer {
    sides: [NoninsertionSide; 2],
    /// Client cursor is FIRST, server cursor is SECOND.
    position: PositionTracker,
}

fn boundary(
    end_keys: Vec<String>,
    changes: Vec<AnnotationChange>,
) -> Result<AnnotationBoundaryMap, TransformError> {
    Ok(AnnotationBoundaryMap::new(end_keys, changes)?)
}

fn change(key: &str, old_value: &Option<String>, new_value: &Option<String>) -> AnnotationChange {
    AnnotationChange {
        key: key.to_owned(),
        old_value: old_value.clone(),
        new_value: new_value.clone(),
    }
}

impl NoninsertionTransformer {
    // Annotation tracking.

    fn commence_deletion(&mut self, side: Side) -> Result<(), TransformError> {
        let other = 1 - side;
        let mut changes = Vec::new();
        let propagating = self.sides[other].tracker.propagating.clone();
        for (key, update) in &propagating {
            let for_combining = self.sides[side].tracker.active.get(key).cloned();
            let other_active = self.sides[other].tracker.active.get(key);
            if let Some(update) = update {
                let old_value = if let Some(for_combining) = &for_combining {
                    &for_combining.old_value
                } else if let Some(other_active) = other_active {
                    &other_active.new_value
                } else {
                    &update.old_value
                };
                changes.push(change(key, old_value, &update.new_value));
            } else if let Some(current_active) = other_active {
                let new_value = if let Some(for_combining) = &for_combining {
                    &for_combining.new_value
                } else {
                    &current_active.old_value
                };
                changes.push(change(key, &current_active.new_value, new_value));
            }
            self.sides[side]
                .tracker
                .temporary
                .insert(key.clone(), for_combining);
        }
        self.commit(side, boundary(Vec::new(), changes)?);
        Ok(())
    }

    fn conclude_deletion(&mut self, side: Side) -> Result<(), TransformError> {
        let mut end_keys = Vec::new();
        let mut changes = Vec::new();
        for (key, update) in std::mem::take(&mut self.sides[side].tracker.temporary) {
            match update {
                Some(update) => changes.push(change(&key, &update.old_value, &update.new_value)),
                None => end_keys.push(key),
            }
        }
        self.sync(side);
        self.commit(side, boundary(end_keys, changes)?);
        Ok(())
    }

    /// Records an annotation boundary of `side`'s original operation.
    fn register(&mut self, side: Side, map: &AnnotationBoundaryMap) -> Result<(), TransformError> {
        let tracked = &mut self.sides[side].tracker.tracked;
        for key in map.end_keys() {
            tracked.remove(key);
        }
        for c in map.changes() {
            tracked.insert(
                c.key.clone(),
                ValueUpdate {
                    old_value: c.old_value.clone(),
                    new_value: c.new_value.clone(),
                },
            );
        }
        if side == CLIENT {
            self.process_client(map)
        } else {
            self.process_server(map)
        }
    }

    /// Writes a boundary to `side`'s output, updating its tracker.
    fn commit(&mut self, side: Side, map: AnnotationBoundaryMap) {
        let NoninsertionSide {
            output, tracker, ..
        } = &mut self.sides[side];
        for key in map.end_keys() {
            if !tracker.propagating.contains_key(key) {
                tracker
                    .propagating
                    .insert(key.clone(), tracker.active.get(key).cloned());
            }
            tracker.active.remove(key);
        }
        for c in map.changes() {
            let new_active = ValueUpdate {
                old_value: c.old_value.clone(),
                new_value: c.new_value.clone(),
            };
            let old_active = tracker.active.get(&c.key);
            if old_active != Some(&new_active) {
                if !tracker.propagating.contains_key(&c.key) {
                    tracker
                        .propagating
                        .insert(c.key.clone(), old_active.cloned());
                }
                tracker.active.insert(c.key.clone(), new_active);
            }
        }
        output.annotation_boundary(map);
    }

    fn sync(&mut self, side: Side) {
        self.sides[side].tracker.propagating.clear();
    }

    fn sync_annotations(&mut self) {
        self.sync(CLIENT);
        self.sync(SERVER);
    }

    /// A client annotation change wins over a concurrent server change of
    /// the same key.
    fn process_client(&mut self, map: &AnnotationBoundaryMap) -> Result<(), TransformError> {
        let mut client_end_keys = Vec::new();
        let mut client_changes = Vec::new();
        let mut server_end_keys = Vec::new();
        let mut server_changes = Vec::new();
        let server_tracked = &self.sides[SERVER].tracker.tracked;
        for key in map.end_keys() {
            client_end_keys.push(key.clone());
            if let Some(server_values) = server_tracked.get(key) {
                server_changes.push(change(
                    key,
                    &server_values.old_value,
                    &server_values.new_value,
                ));
            }
        }
        for c in map.changes() {
            match server_tracked.get(&c.key) {
                Some(server_values) => {
                    client_changes.push(change(&c.key, &server_values.new_value, &c.new_value));
                    server_end_keys.push(c.key.clone());
                }
                None => client_changes.push(c.clone()),
            }
        }
        self.commit(CLIENT, boundary(client_end_keys, client_changes)?);
        self.commit(SERVER, boundary(server_end_keys, server_changes)?);
        Ok(())
    }

    fn process_server(&mut self, map: &AnnotationBoundaryMap) -> Result<(), TransformError> {
        let mut server_end_keys = Vec::new();
        let mut server_changes = Vec::new();
        let mut client_changes = Vec::new();
        let client_tracked = &self.sides[CLIENT].tracker.tracked;
        for key in map.end_keys() {
            match client_tracked.get(key) {
                Some(client_values) => client_changes.push(change(
                    key,
                    &client_values.old_value,
                    &client_values.new_value,
                )),
                None => server_end_keys.push(key.clone()),
            }
        }
        for c in map.changes() {
            match client_tracked.get(&c.key) {
                Some(client_values) => {
                    client_changes.push(change(&c.key, &c.new_value, &client_values.new_value));
                }
                None => server_changes.push(c.clone()),
            }
        }
        self.commit(SERVER, boundary(server_end_keys, server_changes)?);
        self.commit(CLIENT, boundary(Vec::new(), client_changes)?);
        Ok(())
    }

    // Deletions surrounded by the annotation adjustments they need.

    fn do_delete(&mut self, side: Side, component: DocOpComponent) -> Result<(), TransformError> {
        self.commence_deletion(side)?;
        self.sides[side].output.push(component);
        self.conclude_deletion(side)
    }

    fn do_delete_characters(&mut self, side: Side, chars: &str) -> Result<(), TransformError> {
        self.do_delete(side, DocOpComponent::DeleteCharacters(chars.to_owned()))
    }

    fn do_delete_element_start(
        &mut self,
        side: Side,
        tag: &str,
        attributes: Attributes,
    ) -> Result<(), TransformError> {
        self.do_delete(
            side,
            DocOpComponent::DeleteElementStart {
                tag: tag.to_owned(),
                attributes,
            },
        )
    }

    fn do_delete_element_end(&mut self, side: Side) -> Result<(), TransformError> {
        self.do_delete(side, DocOpComponent::DeleteElementEnd)
    }

    /// Resolves `size` items of `component`, which belongs to side `p`,
    /// against the cached component of the other side `w`.
    fn resolve(
        &mut self,
        p: Side,
        component: &DocOpComponent,
        size: usize,
        cache: &mut RangeCache,
    ) -> Result<(), TransformError> {
        use DocOpComponent as C;
        let w = 1 - p;
        match cache {
            RangeCache::Retain => match component {
                C::Retain(_) => {
                    self.sync_annotations();
                    self.sides[w].output.retain(size);
                    self.sides[p].output.retain(size);
                }
                C::DeleteCharacters(chars) => {
                    let (head, _) = split_at_utf16(chars, size)?;
                    self.do_delete_characters(p, head)?;
                }
                C::DeleteElementStart { tag, attributes } => {
                    self.do_delete_element_start(p, tag, attributes.clone())?;
                }
                C::DeleteElementEnd => self.do_delete_element_end(p)?,
                C::ReplaceAttributes { .. } | C::UpdateAttributes(_) => {
                    self.sync_annotations();
                    self.sides[w].output.retain(1);
                    self.sides[p].output.push(component.clone());
                }
                _ => return Err(unexpected(component, "insertion-free")),
            },
            RangeCache::DeleteCharacters(characters) => match component {
                C::Retain(_) => {
                    let (head, tail) = split_at_utf16(characters, size)?;
                    let (head, tail) = (head.to_owned(), tail.to_owned());
                    self.do_delete_characters(w, &head)?;
                    *characters = tail;
                }
                C::DeleteCharacters(_) => {
                    // Both sides delete the same characters; neither needs
                    // to any more.
                    *characters = split_at_utf16(characters, size)?.1.to_owned();
                }
                _ => return Err(TransformError::Incompatible),
            },
            RangeCache::DeleteElementStart { tag, attributes } => match component {
                C::Retain(_) => self.do_delete_element_start(w, tag, attributes.clone())?,
                C::DeleteElementStart { .. } => {}
                C::ReplaceAttributes { new_attributes, .. } => {
                    self.do_delete_element_start(w, tag, new_attributes.clone())?;
                }
                C::UpdateAttributes(update) => {
                    let attributes = attributes.update_with(update)?;
                    self.do_delete_element_start(w, tag, attributes)?;
                }
                _ => return Err(TransformError::Incompatible),
            },
            RangeCache::DeleteElementEnd => match component {
                C::Retain(_) => self.do_delete_element_end(w)?,
                C::DeleteElementEnd => {}
                _ => return Err(TransformError::Incompatible),
            },
            RangeCache::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => match component {
                C::Retain(_) => {
                    self.sync_annotations();
                    self.sides[w]
                        .output
                        .replace_attributes(old_attributes.clone(), new_attributes.clone());
                    self.sides[p].output.retain(1);
                }
                C::DeleteElementStart { tag, .. } => {
                    self.do_delete_element_start(p, tag, new_attributes.clone())?;
                }
                C::ReplaceAttributes {
                    new_attributes: other_new_attributes,
                    ..
                } => {
                    self.sync_annotations();
                    self.sides[w]
                        .output
                        .replace_attributes(other_new_attributes.clone(), new_attributes.clone());
                    self.sides[p].output.retain(1);
                }
                C::UpdateAttributes(update) => {
                    self.sync_annotations();
                    let updated = old_attributes.update_with(update)?;
                    self.sides[w]
                        .output
                        .replace_attributes(updated, new_attributes.clone());
                    self.sides[p].output.retain(1);
                }
                _ => return Err(TransformError::Incompatible),
            },
            RangeCache::UpdateAttributes(cached) => match component {
                C::Retain(_) => {
                    self.sync_annotations();
                    self.sides[w].output.update_attributes(cached.clone());
                    self.sides[p].output.retain(1);
                }
                C::DeleteElementStart { tag, attributes } => {
                    let attributes = attributes.update_with(cached)?;
                    self.do_delete_element_start(p, tag, attributes)?;
                }
                C::ReplaceAttributes {
                    old_attributes,
                    new_attributes,
                } => {
                    self.sync_annotations();
                    self.sides[w].output.retain(1);
                    let updated = old_attributes.update_with(cached)?;
                    self.sides[p]
                        .output
                        .replace_attributes(updated, new_attributes.clone());
                }
                C::UpdateAttributes(update) => {
                    self.sync_annotations();
                    // The cached update wins on shared keys: its old values
                    // become the other update's new values, and the other
                    // update loses those keys.
                    let updated: HashMap<&str, &Option<String>> = update
                        .updates()
                        .iter()
                        .map(|u| (u.name.as_str(), &u.new_value))
                        .collect();
                    let mut new_update = AttributesUpdate::new();
                    for u in cached.updates() {
                        let new_old_value = match updated.get(u.name.as_str()) {
                            Some(value) => (*value).clone(),
                            None => u.old_value.clone(),
                        };
                        new_update = new_update.compose_with(&AttributesUpdate::single(
                            u.name.clone(),
                            new_old_value.as_deref(),
                            u.new_value.as_deref(),
                        ))?;
                    }
                    self.sides[w].output.update_attributes(new_update);
                    let key_set: HashSet<&str> =
                        cached.updates().iter().map(|u| u.name.as_str()).collect();
                    let transformed = update.exclude(|key| key_set.contains(key));
                    self.sides[p].output.update_attributes(transformed);
                }
                _ => return Err(TransformError::Incompatible),
            },
        }
        Ok(())
    }

    /// Advances `side`'s cursor by `size` and resolves as much of
    /// `component` as the other cursor has already passed. Returns the
    /// resolved portion, or -1 if the entire range was resolved.
    fn resolve_range(
        &mut self,
        side: Side,
        size: usize,
        component: &DocOpComponent,
    ) -> Result<i64, TransformError> {
        let other = 1 - side;
        let old_position = self.position.get(side);
        self.position.increase(side, size);
        let (resolved, result) = if self.position.get(side) > 0 {
            let resolved = (old_position < 0).then_some(-old_position as usize);
            (resolved, -old_position)
        } else {
            (Some(size), ENTIRELY_RESOLVED)
        };
        if let Some(resolved) = resolved {
            let mut cache =
                std::mem::replace(&mut self.sides[other].range_cache, RangeCache::Retain);
            let outcome = self.resolve(side, component, resolved, &mut cache);
            self.sides[other].range_cache = cache;
            outcome?;
        }
        Ok(result)
    }

    fn step(&mut self, side: Side, component: &DocOpComponent) -> Result<(), TransformError> {
        use DocOpComponent as C;
        let cache = match component {
            C::AnnotationBoundary(map) => return self.register(side, map),
            C::Retain(item_count) => {
                self.resolve_range(side, *item_count, component)?;
                self.sides[side].range_cache = RangeCache::Retain;
                return Ok(());
            }
            C::DeleteCharacters(chars) => {
                let resolution_size = self.resolve_range(side, utf16_len(chars), component)?;
                if resolution_size >= 0 {
                    let rest = split_at_utf16(chars, resolution_size as usize)?
                        .1
                        .to_owned();
                    self.sides[side].range_cache = RangeCache::DeleteCharacters(rest);
                }
                return Ok(());
            }
            C::DeleteElementStart { tag, attributes } => RangeCache::DeleteElementStart {
                tag: tag.clone(),
                attributes: attributes.clone(),
            },
            C::DeleteElementEnd => RangeCache::DeleteElementEnd,
            C::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => RangeCache::ReplaceAttributes {
                old_attributes: old_attributes.clone(),
                new_attributes: new_attributes.clone(),
            },
            C::UpdateAttributes(update) => RangeCache::UpdateAttributes(update.clone()),
            _ => return Err(unexpected(component, "insertion-free")),
        };
        if self.resolve_range(side, 1, component)? == 0 {
            self.sides[side].range_cache = cache;
        }
        Ok(())
    }
}

/// Transforms a pair of insertion-free operations.
fn transform_noninsertions(
    client_op: &DocOp,
    server_op: &DocOp,
) -> Result<(DocOp, DocOp), TransformError> {
    let mut transformer = NoninsertionTransformer {
        sides: [NoninsertionSide::new(), NoninsertionSide::new()],
        position: PositionTracker::default(),
    };
    interleave(
        &mut transformer,
        [client_op, server_op],
        ["client", "server"],
        |t| t.position.get(CLIENT),
        NoninsertionTransformer::step,
    )?;
    let [client, server] = transformer.sides;
    Ok((
        DocOp::new(client.output.finish())?,
        DocOp::new(server.output.finish())?,
    ))
}
