//! Ports of `ImmutableStateMapTest` and `ImmutableUpdateMapTest`, plus
//! tests of the compatibility-check quirks of the original maps.

use super::attrs;
use crate::docop::attributes::{check_attributes_sorted, check_updates_sorted};
use crate::docop::*;

fn update(triples: &[(&str, Option<&str>, Option<&str>)]) -> AttributesUpdate {
    AttributesUpdate::from_triples(triples.iter().copied()).unwrap()
}

#[test]
fn removal_in_update_with() {
    let updated = attrs(&["a", "1"])
        .update_with(&AttributesUpdate::single("a", Some("1"), None))
        .unwrap();
    assert!(!updated.contains_key("a"));
}

#[test]
fn varargs_constructor() {
    let a = attrs(&["c", "0", "a", "1", "b", "2"]);
    let expected = Attributes::from_sorted(vec![
        Attribute::new("a", "1"),
        Attribute::new("b", "2"),
        Attribute::new("c", "0"),
    ])
    .unwrap();
    assert_eq!(expected, a);
    assert!(Attributes::from_pairs([("a", "1"), ("a", "2")]).is_err());
}

#[test]
fn update_adds_attribute() {
    let a = attrs(&["a", "0"])
        .update_with(&AttributesUpdate::single("b", None, Some("1")))
        .unwrap();
    assert_eq!(Some("0"), a.get("a"));
    assert_eq!(Some("1"), a.get("b"));
    assert_eq!(2, a.len());
}

// The null-element cases of the Java tests have no counterpart here.
#[test]
fn check_attributes_sorted_cases() {
    let list = |names: &[&str]| -> Vec<Attribute> {
        names.iter().map(|n| Attribute::new(*n, "1")).collect()
    };
    assert!(check_attributes_sorted(&list(&[])).is_ok());
    assert!(check_attributes_sorted(&list(&["a"])).is_ok());
    assert!(check_attributes_sorted(&list(&["aa", "ab"])).is_ok());
    assert!(check_attributes_sorted(&list(&["a", "b", "c"])).is_ok());
    assert!(check_attributes_sorted(&list(&["asdfa", "asdfb", "asdfb"])).is_err());
    assert!(check_attributes_sorted(&list(&["rar", "rar", "rbr"])).is_err());
    assert!(check_attributes_sorted(&list(&["a", "a"])).is_err());
    assert!(check_attributes_sorted(&list(&["a", "c", "b"])).is_err());
}

#[test]
fn check_updates_sorted_cases() {
    let list = |names: &[&str]| -> Vec<AttributeUpdate> {
        names
            .iter()
            .map(|n| AttributeUpdate::new(*n, Some("0"), None))
            .collect()
    };
    assert!(check_updates_sorted(&list(&[])).is_ok());
    assert!(check_updates_sorted(&list(&["a"])).is_ok());
    assert!(check_updates_sorted(&list(&["aa", "ab"])).is_ok());
    assert!(check_updates_sorted(&list(&["a", "b", "c"])).is_ok());
    assert!(check_updates_sorted(&list(&["asdfa", "asdfb", "asdfb"])).is_err());
    assert!(check_updates_sorted(&list(&["rar", "rar", "rbr"])).is_err());
    assert!(check_updates_sorted(&list(&["a", "a"])).is_err());
    assert!(check_updates_sorted(&list(&["ard", "ard"])).is_err());
    assert!(check_updates_sorted(&list(&["a", "c", "b"])).is_err());
}

#[test]
fn update_with_checks_old_values() {
    let a = attrs(&["b", "1", "d", "2"]);
    // Wrong old value of an existing key.
    assert!(
        a.update_with(&update(&[("b", Some("x"), Some("3"))]))
            .is_err()
    );
    assert!(a.update_with(&update(&[("b", None, Some("3"))])).is_err());
    // Non-null old value of a key that sorts before an existing key.
    assert!(
        a.update_with(&update(&[("a", Some("x"), Some("3"))]))
            .is_err()
    );
    assert!(a.update_with(&update(&[("c", Some("x"), None)])).is_err());
    // Without the check the new values simply win.
    assert_eq!(
        attrs(&["a", "3", "b", "1", "d", "2"]),
        a.update_with_no_compatibility_check(&update(&[("a", Some("x"), Some("3"))]))
    );
    assert_eq!(
        attrs(&["d", "2"]),
        a.update_with_no_compatibility_check(&update(&[("b", Some("x"), None)]))
    );
}

// Quirk of ImmutableStateMap.updateWith preserved from the original: the old
// value of an update is not checked once the existing entries are exhausted.
#[test]
fn update_with_does_not_check_keys_past_the_last_entry() {
    let a = attrs(&["b", "1"]);
    assert_eq!(
        attrs(&["b", "1", "c", "3"]),
        a.update_with(&update(&[("c", Some("bogus"), Some("3"))]))
            .unwrap()
    );
    assert_eq!(
        attrs(&["z", "3"]),
        Attributes::new()
            .update_with(&update(&[("z", Some("bogus"), Some("3"))]))
            .unwrap()
    );
}

#[test]
fn compose_with_merges_and_checks_shared_keys() {
    let first = update(&[("a", None, Some("1")), ("c", Some("0"), Some("1"))]);
    let second = update(&[("b", Some("x"), None), ("c", Some("1"), Some("2"))]);
    assert_eq!(
        update(&[
            ("a", None, Some("1")),
            ("b", Some("x"), None),
            ("c", Some("0"), Some("2")),
        ]),
        first.compose_with(&second).unwrap()
    );
    // The second update's old value must equal the first's new value.
    let mismatched = update(&[("c", Some("9"), Some("2"))]);
    assert!(matches!(
        first.compose_with(&mismatched),
        Err(OperationError::MismatchedOldValue(_))
    ));
    assert_eq!(first, first.compose_with(&AttributesUpdate::new()).unwrap());
    assert_eq!(first, AttributesUpdate::new().compose_with(&first).unwrap());
}

#[test]
fn update_invert_and_exclude() {
    let u = update(&[("a", None, Some("1")), ("b", Some("x"), None)]);
    assert_eq!(
        update(&[("a", Some("1"), None), ("b", None, Some("x"))]),
        u.invert().unwrap()
    );
    assert_eq!(
        update(&[("b", Some("x"), None)]),
        u.exclude(|key| key == "a")
    );
    assert!(AttributesUpdate::from_triples([("a", None, None), ("a", None, None)]).is_err());
}

#[test]
fn compose_with_boundary() {
    let open = update(&[("b", None, Some("1")), ("d", Some("x"), Some("y"))]);
    let map = super::boundary(
        &["b"],
        &[("a", None, Some("2")), ("d", Some("p"), Some("q"))],
    );
    assert_eq!(
        update(&[("a", None, Some("2")), ("d", Some("p"), Some("q"))]),
        open.compose_with_boundary(&map).unwrap()
    );
    // Ending a key that is not open.
    assert!(
        open.compose_with_boundary(&super::finish_annotation(&["c"]))
            .is_err()
    );
    assert!(
        open.compose_with_boundary(&super::finish_annotation(&["z"]))
            .is_err()
    );
    // A key both ended and changed.
    let both = AnnotationBoundaryMap::new_unchecked(
        vec!["b".to_owned()],
        vec![AnnotationChange::new("b", None, Some("2"))],
    );
    assert!(open.compose_with_boundary(&both).is_err());
}

#[test]
fn annotation_boundary_map_argument_checks() {
    let ends = |keys: &[&str]| AnnotationBoundaryMap::ends(keys.iter().copied());
    assert!(ends(&["a", "b"]).is_ok());
    // Order is not checked at construction.
    assert!(ends(&["b", "a"]).is_ok());
    assert!(ends(&["a", "a"]).is_err());
    assert!(ends(&[""]).is_err());
    assert!(ends(&["a?"]).is_err());
    assert!(ends(&["a@b"]).is_err());
    assert!(
        AnnotationBoundaryMap::new(
            vec!["a".to_owned()],
            vec![AnnotationChange::new("a", None, Some("1"))]
        )
        .is_err()
    );
    assert!(
        AnnotationBoundaryMap::changes_only([("a", None, Some("1")), ("a", None, Some("2"))])
            .is_err()
    );
}
