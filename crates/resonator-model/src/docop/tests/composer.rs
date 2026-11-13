//! Ports of `ComposerTest` and `DocOpCollectorTest`, plus direct tests of
//! composition.

use super::{attrs, begin_annotation, finish_annotation};
use crate::docop::*;

#[test]
fn document_length_mismatch() {
    let empty = DocOpBuilder::new().build().unwrap();
    let retain = DocOpBuilder::new().retain(1).build().unwrap();
    assert_eq!(
        Err(OperationError::IllegalComposition),
        compose(&empty, &retain)
    );
    assert_eq!(
        Err(OperationError::SizeMismatch {
            resulting: 1,
            initial: 0
        }),
        compose(&retain, &empty)
    );
}

#[test]
fn composer_checking() {
    let checked = DocOpBuilder::new().build().unwrap();
    let unchecked = DocOpBuilder::new()
        .element_start(".!$*.4!(,", Attributes::new())
        .element_end()
        .build_unchecked();
    assert!(matches!(
        compose(&checked, &unchecked),
        Err(OperationError::IllFormed { .. })
    ));
    // Composing unchecked this time, nothing should fail.
    assert_eq!(unchecked, compose_unchecked(&checked, &unchecked).unwrap());
}

#[test]
fn simple_monotonic_composition() {
    let mut collector = DocOpCollector::new();
    assert!(collector.is_empty());
    let a = DocOpBuilder::new().characters("a").build().unwrap();
    let b = DocOpBuilder::new()
        .retain(1)
        .characters("b")
        .build()
        .unwrap();
    let c = DocOpBuilder::new()
        .retain(2)
        .characters("c")
        .build()
        .unwrap();
    let d = DocOpBuilder::new()
        .retain(3)
        .characters("d")
        .build()
        .unwrap();
    let all = [a.clone(), b.clone(), c.clone(), d.clone()];
    collector.add(a).unwrap();
    collector.add(b).unwrap();
    collector.add(c).unwrap();
    collector.add(d).unwrap();
    assert!(!collector.is_empty());
    let expected = DocOpBuilder::new().characters("abcd").build().unwrap();
    assert_eq!(Some(expected.clone()), collector.compose_all().unwrap());
    assert!(collector.is_empty());
    assert_eq!(Some(expected), compose_all(&all).unwrap());
}

#[test]
fn empty_collection_composes_to_none() {
    assert_eq!(None, DocOpCollector::new().compose_all().unwrap());
    assert_eq!(None, compose_all(&[]).unwrap());
}

#[test]
fn collector_reports_composition_errors() {
    let mut collector = DocOpCollector::new();
    collector
        .add(DocOpBuilder::new().characters("a").build().unwrap())
        .unwrap();
    // Applies to the empty document, but the collected op produces one item.
    let mismatched = DocOpBuilder::new().build().unwrap();
    assert!(collector.add(mismatched).is_err());
}

#[test]
fn insertion_then_deletion_cancel() {
    let insert = DocOpBuilder::new()
        .retain(1)
        .characters("abc")
        .retain(1)
        .build()
        .unwrap();
    let delete = DocOpBuilder::new()
        .retain(2)
        .delete_characters("bc")
        .retain(1)
        .build()
        .unwrap();
    let expected = DocOpBuilder::new()
        .retain(1)
        .characters("a")
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(expected, compose(&insert, &delete).unwrap());
}

#[test]
fn attribute_changes_compose() {
    let insert = DocOpBuilder::new()
        .element_start("p", attrs(&["a", "1"]))
        .element_end()
        .build()
        .unwrap();
    let update = DocOpBuilder::new()
        .update_attributes(
            AttributesUpdate::from_triples([("a", Some("1"), None), ("b", None, Some("2"))])
                .unwrap(),
        )
        .retain(1)
        .build()
        .unwrap();
    let expected = DocOpBuilder::new()
        .element_start("p", attrs(&["b", "2"]))
        .element_end()
        .build()
        .unwrap();
    assert_eq!(expected, compose(&insert, &update).unwrap());

    // An update that does not match the inserted attributes is an error.
    let bad_update = DocOpBuilder::new()
        .update_attributes(AttributesUpdate::single("a", Some("9"), None))
        .retain(1)
        .build()
        .unwrap();
    assert!(matches!(
        compose(&insert, &bad_update),
        Err(OperationError::MismatchedOldValue(_))
    ));

    let first = DocOpBuilder::new()
        .update_attributes(AttributesUpdate::single("a", Some("1"), Some("2")))
        .retain(1)
        .build()
        .unwrap();
    let second = DocOpBuilder::new()
        .replace_attributes(attrs(&["a", "2"]), attrs(&["c", "3"]))
        .retain(1)
        .build()
        .unwrap();
    let expected = DocOpBuilder::new()
        .replace_attributes(attrs(&["a", "1"]), attrs(&["c", "3"]))
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(expected, compose(&first, &second).unwrap());
}

#[test]
fn structurally_incompatible_composition_is_an_error() {
    let insert_text = DocOpBuilder::new().characters("ab").build().unwrap();
    let delete_element = DocOpBuilder::new()
        .delete_element_start("p", Attributes::new())
        .delete_element_end()
        .build()
        .unwrap();
    assert_eq!(
        Err(OperationError::IllegalComposition),
        compose(&insert_text, &delete_element)
    );
}

#[test]
fn annotations_compose() {
    let first = DocOpBuilder::new()
        .annotation_boundary(begin_annotation("k", None, Some("1")))
        .retain(2)
        .annotation_boundary(finish_annotation(&["k"]))
        .retain(1)
        .build()
        .unwrap();
    let second = DocOpBuilder::new()
        .retain(1)
        .annotation_boundary(begin_annotation("k", Some("1"), Some("2")))
        .retain(2)
        .annotation_boundary(finish_annotation(&["k"]))
        .build()
        .unwrap();
    let expected = DocOpBuilder::new()
        .annotation_boundary(begin_annotation("k", None, Some("1")))
        .retain(1)
        .annotation_boundary(begin_annotation("k", None, Some("2")))
        .retain(1)
        .annotation_boundary(begin_annotation("k", Some("1"), Some("2")))
        .retain(1)
        .annotation_boundary(finish_annotation(&["k"]))
        .build()
        .unwrap();
    assert_eq!(expected, compose(&first, &second).unwrap());
}

// Quirk preserved from the original: a retain of the second operation that
// outlasts the first operation goes unreported.
#[test]
fn trailing_retain_of_second_operation_is_not_reported() {
    let one = DocOpBuilder::new().retain(1).build().unwrap();
    let five = DocOpBuilder::new().retain(5).build().unwrap();
    assert_eq!(one, compose(&one, &five).unwrap());
}

#[test]
fn inverse_composes_to_identity() {
    let op = DocOpBuilder::new()
        .retain(1)
        .delete_characters("ab")
        .characters("xyz")
        .update_attributes(AttributesUpdate::single("a", Some("1"), Some("2")))
        .replace_attributes(attrs(&["a", "1"]), Attributes::new())
        .annotation_boundary(begin_annotation("k", Some("1"), None))
        .retain(2)
        .annotation_boundary(finish_annotation(&["k"]))
        .delete_element_start("p", attrs(&["q", "r"]))
        .delete_element_end()
        .element_start("s", Attributes::new())
        .element_end()
        .build()
        .unwrap();
    let inverse = invert(&op).unwrap();
    let expected_inverse = DocOpBuilder::new()
        .retain(1)
        .characters("ab")
        .delete_characters("xyz")
        .update_attributes(AttributesUpdate::single("a", Some("2"), Some("1")))
        .replace_attributes(Attributes::new(), attrs(&["a", "1"]))
        .annotation_boundary(begin_annotation("k", None, Some("1")))
        .retain(2)
        .annotation_boundary(finish_annotation(&["k"]))
        .element_start("p", attrs(&["q", "r"]))
        .element_end()
        .delete_element_start("s", Attributes::new())
        .delete_element_end()
        .build()
        .unwrap();
    assert_eq!(expected_inverse, inverse);
    assert_eq!(op, invert(&inverse).unwrap());
    assert_eq!(
        initial_document_length(&op),
        resulting_document_length(&inverse)
    );
    assert_eq!(
        resulting_document_length(&op),
        initial_document_length(&inverse)
    );
}

#[test]
fn decompose_splits_insertions_from_the_rest() {
    let op = DocOpBuilder::new()
        .retain(1)
        .characters("ab")
        .delete_characters("c")
        .element_start("p", Attributes::new())
        .element_end()
        .annotation_boundary(begin_annotation("k", None, Some("1")))
        .retain(1)
        .annotation_boundary(finish_annotation(&["k"]))
        .build()
        .unwrap();
    let (insertion, noninsertion) = decompose(&op).unwrap();
    let expected_insertion = DocOpBuilder::new()
        .retain(1)
        .characters("ab")
        .retain(1)
        .element_start("p", Attributes::new())
        .element_end()
        .retain(1)
        .build()
        .unwrap();
    let expected_noninsertion = DocOpBuilder::new()
        .retain(3)
        .delete_characters("c")
        .retain(2)
        .annotation_boundary(begin_annotation("k", None, Some("1")))
        .retain(1)
        .annotation_boundary(finish_annotation(&["k"]))
        .build()
        .unwrap();
    assert_eq!(expected_insertion, insertion);
    assert_eq!(expected_noninsertion, noninsertion);
}
