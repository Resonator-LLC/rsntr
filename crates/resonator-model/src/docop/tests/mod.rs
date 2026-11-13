//! Tests ported from the Apache Wave test suite, plus tests for the places
//! where this port deliberately deviates (UTF-16 handling).

use super::*;

mod composer;
mod golden;
mod maps;
mod normalizer;
mod properties;
mod transformer;
mod unicode;
mod util;
mod validator;

/// `new AttributesImpl("name", "value", ...)`.
fn attrs(pairs: &[&str]) -> Attributes {
    assert!(
        pairs.len().is_multiple_of(2),
        "pairs must come in groups of two"
    );
    Attributes::from_pairs(pairs.chunks(2).map(|pair| (pair[0], pair[1]))).unwrap()
}

/// A boundary that starts one annotation change.
fn begin_annotation(
    key: &str,
    old_value: Option<&str>,
    new_value: Option<&str>,
) -> AnnotationBoundaryMap {
    AnnotationBoundaryMap::changes_only([(key, old_value, new_value)]).unwrap()
}

/// A boundary that ends the given keys.
fn finish_annotation(keys: &[&str]) -> AnnotationBoundaryMap {
    AnnotationBoundaryMap::ends(keys.iter().copied()).unwrap()
}

/// A boundary with end keys and `(key, old, new)` changes.
fn boundary(
    end_keys: &[&str],
    changes: &[(&str, Option<&str>, Option<&str>)],
) -> AnnotationBoundaryMap {
    AnnotationBoundaryMap::new(
        end_keys.iter().map(|k| k.to_string()).collect(),
        changes
            .iter()
            .map(|(key, old, new)| AnnotationChange::new(*key, *old, *new))
            .collect(),
    )
    .unwrap()
}
