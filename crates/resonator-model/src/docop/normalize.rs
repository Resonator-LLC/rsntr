//! Operation normalizers: sinks that merge adjacent ranges and drop
//! redundant annotation changes.

use std::collections::BTreeMap;

use super::annotation::{AnnotationBoundaryMap, AnnotationChange};
use super::op::{DocOpComponent, OpSink};
use super::utf16::Utf16Key;

enum RangeCache {
    Empty,
    Retain(usize),
    Characters(String),
    DeleteCharacters(String),
}

/// Merges adjacent retains, adjacent insertions of characters and adjacent
/// deletions of characters, and drops empty ones.
pub struct RangeNormalizer<S: OpSink> {
    target: S,
    cache: RangeCache,
}

impl<S: OpSink> RangeNormalizer<S> {
    pub fn new(target: S) -> Self {
        RangeNormalizer {
            target,
            cache: RangeCache::Empty,
        }
    }

    fn flush(&mut self) {
        match std::mem::replace(&mut self.cache, RangeCache::Empty) {
            RangeCache::Empty => {}
            RangeCache::Retain(distance) => self.target.push(DocOpComponent::Retain(distance)),
            RangeCache::Characters(chars) => self.target.push(DocOpComponent::Characters(chars)),
            RangeCache::DeleteCharacters(chars) => {
                self.target.push(DocOpComponent::DeleteCharacters(chars))
            }
        }
    }
}

impl<S: OpSink> OpSink for RangeNormalizer<S> {
    fn push(&mut self, component: DocOpComponent) {
        match component {
            DocOpComponent::Retain(item_count) => {
                if item_count > 0 {
                    if let RangeCache::Retain(distance) = &mut self.cache {
                        *distance = distance.saturating_add(item_count);
                    } else {
                        self.flush();
                        self.cache = RangeCache::Retain(item_count);
                    }
                }
            }
            DocOpComponent::Characters(chars) => {
                if !chars.is_empty() {
                    if let RangeCache::Characters(cached) = &mut self.cache {
                        cached.push_str(&chars);
                    } else {
                        self.flush();
                        self.cache = RangeCache::Characters(chars);
                    }
                }
            }
            DocOpComponent::DeleteCharacters(chars) => {
                if !chars.is_empty() {
                    if let RangeCache::DeleteCharacters(cached) = &mut self.cache {
                        cached.push_str(&chars);
                    } else {
                        self.flush();
                        self.cache = RangeCache::DeleteCharacters(chars);
                    }
                }
            }
            other => {
                self.flush();
                self.target.push(other);
            }
        }
    }

    fn finish(mut self) -> Vec<DocOpComponent> {
        self.flush();
        self.target.finish()
    }
}

type ChangeValues = (Option<String>, Option<String>);

/// Merges consecutive annotation boundaries and drops changes that repeat
/// the change already in effect, and ends of keys that are not open.
pub struct AnnotationsNormalizer<S: OpSink> {
    target: S,
    /// Changes currently in effect in the output.
    annotation_tracker: BTreeMap<Utf16Key, ChangeValues>,
    /// Changes (`Some`) and ends (`None`) seen since the last flush.
    annotation_changes: BTreeMap<Utf16Key, Option<ChangeValues>>,
}

impl<S: OpSink> AnnotationsNormalizer<S> {
    pub fn new(target: S) -> Self {
        AnnotationsNormalizer {
            target,
            annotation_tracker: BTreeMap::new(),
            annotation_changes: BTreeMap::new(),
        }
    }

    fn flush_annotations(&mut self) {
        let mut changes = Vec::new();
        let mut ends = Vec::new();
        for (key, values) in std::mem::take(&mut self.annotation_changes) {
            match values {
                None => {
                    if self.annotation_tracker.remove(&key).is_some() {
                        ends.push(key.0);
                    }
                }
                Some(values) => {
                    if self.annotation_tracker.get(&key) != Some(&values) {
                        changes.push(AnnotationChange {
                            key: key.0.clone(),
                            old_value: values.0.clone(),
                            new_value: values.1.clone(),
                        });
                        self.annotation_tracker.insert(key, values);
                    }
                }
            }
        }
        if !changes.is_empty() || !ends.is_empty() {
            self.target.push(DocOpComponent::AnnotationBoundary(
                AnnotationBoundaryMap::new_unchecked(ends, changes),
            ));
        }
    }
}

impl<S: OpSink> OpSink for AnnotationsNormalizer<S> {
    fn push(&mut self, component: DocOpComponent) {
        match component {
            DocOpComponent::AnnotationBoundary(map) => {
                for change in map.changes() {
                    self.annotation_changes.insert(
                        Utf16Key(change.key.clone()),
                        Some((change.old_value.clone(), change.new_value.clone())),
                    );
                }
                for key in map.end_keys() {
                    self.annotation_changes.insert(Utf16Key(key.clone()), None);
                }
            }
            DocOpComponent::Retain(0) => {}
            DocOpComponent::Characters(ref chars) | DocOpComponent::DeleteCharacters(ref chars)
                if chars.is_empty() => {}
            other => {
                self.flush_annotations();
                self.target.push(other);
            }
        }
    }

    fn finish(mut self) -> Vec<DocOpComponent> {
        self.flush_annotations();
        self.target.finish()
    }
}

/// The standard normalizer: annotations first, then ranges.
pub type OpNormalizer = AnnotationsNormalizer<RangeNormalizer<Vec<DocOpComponent>>>;

/// Creates the standard normalizer writing to a fresh buffer.
pub fn new_normalizer() -> OpNormalizer {
    AnnotationsNormalizer::new(RangeNormalizer::new(Vec::new()))
}
