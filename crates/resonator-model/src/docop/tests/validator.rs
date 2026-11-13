//! Port of `DocOpValidatorTest`, plus tests of well-formedness rules.

use super::{attrs, begin_annotation, boundary, finish_annotation};
use crate::docop::*;

/// `DocOpValidatorTest.TEST_CONSTRAINTS`.
struct TestConstraints;

impl DocumentSchema for TestConstraints {
    fn permits_child(&self, parent: Option<&str>, child: &str) -> bool {
        matches!(
            (parent, child),
            (None, "body") | (Some("body"), "line") | (Some("body"), "otherchildofbody")
        )
    }

    fn permitted_characters(&self, element: Option<&str>) -> PermittedCharacters {
        if element == Some("body") {
            PermittedCharacters::BlipText
        } else {
            PermittedCharacters::None
        }
    }

    fn permits_attribute(&self, _element: &str, _name: &str) -> bool {
        false
    }

    fn permits_attribute_value(&self, _element: &str, _name: &str, _value: &str) -> bool {
        false
    }

    fn required_initial_children(&self, element: Option<&str>) -> Vec<String> {
        if element == Some("body") {
            vec!["line".to_owned()]
        } else {
            Vec::new()
        }
    }
}

/// Initializes a document with `d`, validates `m` against it and checks that
/// the outcome is `expected`.
fn do_test_with_schema(
    schema: &dyn DocumentSchema,
    d: DocOpBuilder,
    m: DocOpBuilder,
    expected: ValidationResult,
) {
    let mut doc = BootstrapDocument::new();
    doc.consume(&d.build_unchecked()).unwrap();
    let m = m.build_unchecked();
    let mut v = ViolationCollector::new();
    let result = validate(Some(&mut v), schema, &doc, &m);
    assert_eq!(expected, result, "op: {m}, violations: {v}");
    assert_eq!(result, v.validation_result());
    assert_eq!(result.is_valid(), v.is_valid());
    assert_eq!(result.is_valid(), v.first_description().is_none());
    // The result must not depend on whether violations are collected.
    assert_eq!(result, validate(None, schema, &doc, &m));
}

fn do_test(d: DocOpBuilder, m: DocOpBuilder, expected: ValidationResult) {
    do_test_with_schema(&NO_SCHEMA_CONSTRAINTS, d, m, expected);
}

fn do_schema_test(d: DocOpBuilder, m: DocOpBuilder, expected: ValidationResult) {
    do_test_with_schema(&TestConstraints, d, m, expected);
}

fn b() -> DocOpBuilder {
    DocOpBuilder::new()
}

fn empty() -> Attributes {
    Attributes::new()
}

use ValidationResult::{IllFormed, InvalidDocument, InvalidSchema, Valid};

#[test]
fn test1() {
    do_test(b(), b(), Valid);
}

#[test]
fn test2() {
    do_test(
        b(),
        b().element_start("<", empty()).element_end(),
        IllFormed,
    );
}

#[test]
fn test3() {
    do_test(b(), b().element_start("blip", empty()).element_end(), Valid);
}

#[test]
fn test4() {
    do_schema_test(
        b(),
        b().element_start("p", empty()).element_end(),
        InvalidSchema,
    );
}

#[test]
fn max_skip_distance_doesnt_assert() {
    do_test(
        b().element_start("blip", empty()).element_end(),
        b().retain(3).retain(1),
        InvalidDocument,
    );
}

#[test]
fn unsorted_attributes() {
    let unchecked = |names: &[&str]| {
        Attributes::from_sorted_unchecked(names.iter().map(|n| Attribute::new(*n, "1")).collect())
    };
    do_test(
        b(),
        b().element_start("blip", unchecked(&["a", "b"]))
            .element_end(),
        Valid,
    );
    do_test(
        b(),
        b().element_start("blip", unchecked(&["b", "a"]))
            .element_end(),
        IllFormed,
    );
    do_test(
        b(),
        b().element_start("blip", unchecked(&["a", "a"]))
            .element_end(),
        IllFormed,
    );
}

/// A boundary built without the argument checks of the regular constructor
/// (`DumbAnnotationBoundaryMap`).
fn dumb(
    end_keys: &[&str],
    changes: &[(&str, Option<&str>, Option<&str>)],
) -> AnnotationBoundaryMap {
    AnnotationBoundaryMap::new_unchecked(
        end_keys.iter().map(|k| k.to_string()).collect(),
        changes
            .iter()
            .map(|(key, old, new)| AnnotationChange::new(*key, *old, *new))
            .collect(),
    )
}

#[test]
fn duplicate_annotation_keys() {
    do_test(
        b().characters("ab"),
        b().annotation_boundary(begin_annotation("a", None, Some("1")))
            .retain(1)
            .annotation_boundary(boundary(&["a"], &[("b", None, Some("2"))]))
            .retain(1)
            .annotation_boundary(finish_annotation(&["b"])),
        Valid,
    );
    do_test(
        b().characters("ab"),
        b().annotation_boundary(begin_annotation("a", None, Some("1")))
            .retain(1)
            .annotation_boundary(dumb(&["a"], &[("a", None, Some("2"))]))
            .retain(1)
            .annotation_boundary(finish_annotation(&["a"])),
        IllFormed,
    );
}

#[test]
fn unsorted_annotation_keys() {
    let start = || boundary(&[], &[("a", None, Some("1")), ("b", None, Some("2"))]);
    let middle = || boundary(&["a"], &[("b", None, Some("2")), ("c", None, Some("3"))]);
    // all ok
    do_test(
        b().characters("ab"),
        b().annotation_boundary(start())
            .retain(1)
            .annotation_boundary(middle())
            .retain(1)
            .annotation_boundary(finish_annotation(&["b", "c"])),
        Valid,
    );
    // change keys in wrong order
    do_test(
        b().characters("ab"),
        b().annotation_boundary(dumb(&[], &[("b", None, Some("2")), ("a", None, Some("1"))]))
            .retain(1)
            .annotation_boundary(middle())
            .retain(1)
            .annotation_boundary(finish_annotation(&["b", "c"])),
        IllFormed,
    );
    // end keys in wrong order
    do_test(
        b().characters("ab"),
        b().annotation_boundary(start())
            .retain(1)
            .annotation_boundary(middle())
            .retain(1)
            .annotation_boundary(dumb(&["c", "b"], &[])),
        IllFormed,
    );
}

fn annotated_a_b() -> DocOpBuilder {
    b().annotation_boundary(begin_annotation("a", None, Some("1")))
        .characters("a")
        .characters("b")
        .annotation_boundary(finish_annotation(&["a"]))
}

#[test]
fn deletion_annotations_are_relative() {
    // annotations are not needed if previous character has the same annotations
    do_test(annotated_a_b(), b().retain(1).delete_characters("b"), Valid);
    // but may be specified
    do_test(
        annotated_a_b(),
        b().retain(1)
            .annotation_boundary(begin_annotation("a", Some("1"), Some("1")))
            .delete_characters("b")
            .annotation_boundary(finish_annotation(&["a"])),
        Valid,
    );
    // even for null values
    do_test(
        b().characters("a").characters("b"),
        b().retain(1)
            .annotation_boundary(begin_annotation("a", None, None))
            .delete_characters("b")
            .annotation_boundary(finish_annotation(&["a"])),
        Valid,
    );
}

#[test]
fn deletion_annotations_are_relative2() {
    let annotated = |text: &str| {
        b().annotation_boundary(begin_annotation("a", None, Some("1")))
            .characters(text)
            .annotation_boundary(finish_annotation(&["a"]))
    };
    // annotations are needed if previous character has different annotations
    do_test(annotated("a"), b().delete_characters("a"), InvalidDocument);
    // (positive case)
    do_test(
        annotated("a"),
        b().annotation_boundary(begin_annotation("a", Some("1"), None))
            .delete_characters("a")
            .annotation_boundary(finish_annotation(&["a"])),
        Valid,
    );
    // this is even true when deleting multiple items in sequence
    do_test(
        annotated("ab"),
        b().annotation_boundary(begin_annotation("a", Some("1"), None))
            .delete_characters("a")
            .annotation_boundary(finish_annotation(&["a"]))
            .delete_characters("b"),
        InvalidDocument,
    );
    // (positive case)
    do_test(
        annotated("ab"),
        b().annotation_boundary(begin_annotation("a", Some("1"), None))
            .delete_characters("a")
            .delete_characters("b")
            .annotation_boundary(finish_annotation(&["a"])),
        Valid,
    );
}

#[test]
fn deletion_annotations_are_relative3() {
    let doc = || {
        b().annotation_boundary(begin_annotation("a", None, Some("1")))
            .characters("a")
            .annotation_boundary(finish_annotation(&["a"]))
            .characters("b")
    };
    // annotations are needed if previous character has more annotations
    do_test(doc(), b().retain(1).delete_characters("b"), InvalidDocument);
    do_test(
        doc(),
        b().retain(1)
            .annotation_boundary(begin_annotation("a", None, Some("1")))
            .delete_characters("b")
            .annotation_boundary(finish_annotation(&["a"])),
        Valid,
    );
}

#[test]
fn required_tag() {
    // ok
    do_schema_test(
        b(),
        b().element_start("body", empty())
            .element_start("line", empty())
            .element_end()
            .element_end(),
        Valid,
    );
    // missing required element
    do_schema_test(
        b(),
        b().element_start("body", empty()).element_end(),
        InvalidSchema,
    );
    // characters before required element
    do_schema_test(
        b(),
        b().element_start("body", empty())
            .characters("a")
            .element_start("line", empty())
            .element_end()
            .element_end(),
        InvalidSchema,
    );
    // different element before required element
    do_schema_test(
        b(),
        b().element_start("body", empty())
            .element_start("otherchildofbody", empty())
            .element_end()
            .element_start("line", empty())
            .element_end()
            .element_end(),
        InvalidSchema,
    );
}

fn body_with_line() -> DocOpBuilder {
    b().element_start("body", empty())
        .element_start("line", empty())
        .element_end()
        .element_end()
}

#[test]
fn deleting_required_tag() {
    // ok
    do_schema_test(
        body_with_line(),
        b().delete_element_start("body", empty())
            .delete_element_start("line", empty())
            .delete_element_end()
            .delete_element_end(),
        Valid,
    );
    // missing required element
    do_schema_test(
        body_with_line(),
        b().retain(1)
            .delete_element_start("line", empty())
            .delete_element_end()
            .retain(1),
        InvalidSchema,
    );
}

#[test]
fn inserting_around_required_tag() {
    // ok to insert after
    do_schema_test(
        body_with_line(),
        b().retain(3)
            .element_start("line", empty())
            .element_end()
            .retain(1),
        Valid,
    );
    // not ok to insert before
    do_schema_test(
        body_with_line(),
        b().retain(1).characters("a").retain(3),
        InvalidSchema,
    );
}

#[test]
fn char_at_past_end() {
    // this should not crash
    do_test(b(), b().retain(1).delete_characters("ab"), InvalidDocument);
}

// The remaining tests are not in the original suite.

#[test]
fn schema_restricts_text_and_attributes() {
    // text is allowed in body after the required line, but only blip text
    do_schema_test(
        body_with_line(),
        b().retain(3).characters("ok").retain(1),
        Valid,
    );
    do_schema_test(
        body_with_line(),
        b().retain(3).characters("a\u{7}").retain(1),
        InvalidSchema,
    );
    // no text in line or at top level
    do_schema_test(
        body_with_line(),
        b().retain(2).characters("a").retain(2),
        InvalidSchema,
    );
    do_schema_test(
        body_with_line(),
        b().retain(4).characters("a"),
        InvalidSchema,
    );
    // no attributes at all
    do_schema_test(
        body_with_line(),
        b().retain(1)
            .update_attributes(AttributesUpdate::single("a", None, Some("1")))
            .retain(2),
        InvalidSchema,
    );
    do_schema_test(
        body_with_line(),
        b().retain(1)
            .replace_attributes(empty(), attrs(&["a", "1"]))
            .retain(2),
        InvalidSchema,
    );
    // removing an attribute is never a schema violation
    do_schema_test(
        body_with_line(),
        b().retain(1)
            .update_attributes(AttributesUpdate::single("a", None, None))
            .retain(2),
        Valid,
    );
}

#[test]
fn attribute_changes_must_match_document() {
    let doc = || b().element_start("p", attrs(&["a", "1"])).element_end();
    do_test(
        doc(),
        b().update_attributes(AttributesUpdate::single("a", Some("1"), Some("2")))
            .retain(1),
        Valid,
    );
    do_test(
        doc(),
        b().update_attributes(AttributesUpdate::single("a", Some("x"), Some("2")))
            .retain(1),
        InvalidDocument,
    );
    do_test(
        doc(),
        b().replace_attributes(attrs(&["a", "1"]), empty())
            .retain(1),
        Valid,
    );
    do_test(
        doc(),
        b().replace_attributes(empty(), empty()).retain(1),
        InvalidDocument,
    );
    do_test(
        doc(),
        b().retain(1)
            .replace_attributes(attrs(&["a", "1"]), empty()),
        InvalidDocument,
    );
    do_test(
        doc(),
        b().delete_element_start("p", empty()).delete_element_end(),
        InvalidDocument,
    );
    do_test(
        doc(),
        b().delete_element_start("q", attrs(&["a", "1"]))
            .delete_element_end(),
        InvalidDocument,
    );
    do_test(
        doc(),
        b().delete_element_start("p", attrs(&["a", "1"]))
            .delete_element_end(),
        Valid,
    );
}

#[test]
fn annotation_old_values_must_match_document() {
    do_test(
        annotated_a_b(),
        b().annotation_boundary(begin_annotation("a", Some("1"), Some("2")))
            .retain(2)
            .annotation_boundary(finish_annotation(&["a"])),
        Valid,
    );
    do_test(
        annotated_a_b(),
        b().annotation_boundary(begin_annotation("a", None, Some("2")))
            .retain(2)
            .annotation_boundary(finish_annotation(&["a"])),
        InvalidDocument,
    );
    // an insertion inherits from the item on its left
    do_test(
        annotated_a_b(),
        b().retain(2)
            .annotation_boundary(begin_annotation("a", Some("1"), None))
            .characters("c")
            .annotation_boundary(finish_annotation(&["a"])),
        Valid,
    );
    do_test(
        annotated_a_b(),
        b().annotation_boundary(begin_annotation("a", Some("1"), None))
            .characters("c")
            .annotation_boundary(finish_annotation(&["a"]))
            .retain(2),
        InvalidDocument,
    );
}

fn assert_ill_formed(m: DocOpBuilder, expected_description: &str) {
    let op = m.build_unchecked();
    let mut v = ViolationCollector::new();
    assert!(!is_well_formed(Some(&mut v), &op), "{op}");
    assert!(v.is_ill_formed());
    let description = v.first_description().unwrap();
    assert!(
        description.starts_with(&format!("ill-formed: {expected_description}")),
        "{description}"
    );
    assert!(matches!(
        DocOp::new(op.into_components()),
        Err(OperationError::IllFormed { .. })
    ));
}

#[test]
fn well_formedness_rules() {
    assert_ill_formed(b().retain(0), "retain item count not positive");
    assert_ill_formed(b().characters(""), "characters is empty");
    assert_ill_formed(b().delete_characters(""), "characters is empty");
    assert_ill_formed(
        b().characters("\u{FFFE}"),
        "characters component contains invalid unicode",
    );
    assert_ill_formed(
        b().delete_characters("\u{FDD0}"),
        "delete characters component contains invalid unicode",
    );
    assert_ill_formed(
        b().element_start("p", empty()),
        "elementStart with no matching elementEnd",
    );
    assert_ill_formed(
        b().element_end(),
        "elementEnd with no matching elementStart",
    );
    assert_ill_formed(
        b().delete_element_start("p", empty()),
        "deleteElementStart with no matching deleteElementEnd",
    );
    assert_ill_formed(
        b().delete_element_end(),
        "deleteElementEnd with no matching deleteElementStart",
    );
    assert_ill_formed(
        b().element_start("p", empty()).retain(1).element_end(),
        "retain inside insert or delete",
    );
    assert_ill_formed(
        b().delete_element_start("p", empty())
            .retain(1)
            .delete_element_end(),
        "retain inside insert or delete",
    );
    assert_ill_formed(
        b().element_start("p", empty())
            .delete_characters("a")
            .element_end(),
        "deletion inside insertion",
    );
    assert_ill_formed(
        b().delete_element_start("p", empty())
            .characters("a")
            .delete_element_end(),
        "insertion inside deletion",
    );
    assert_ill_formed(
        b().element_start("p", empty())
            .update_attributes(AttributesUpdate::new())
            .element_end(),
        "attribute change inside insert or delete",
    );
    assert_ill_formed(
        b().element_start("p", attrs(&["9", "1"])).element_end(),
        "attribute name is not an XML Name",
    );
    assert_ill_formed(
        b().element_start("p", attrs(&["a", "\u{FFFF}"]))
            .element_end(),
        "attribute value is not valid UTF-16",
    );
    assert_ill_formed(
        b().update_attributes(AttributesUpdate::from_sorted_unchecked(vec![
            AttributeUpdate::new("b", None, None),
            AttributeUpdate::new("a", None, None),
        ])),
        "attribute keys not strictly monotonic",
    );
    assert_ill_formed(
        b().annotation_boundary(begin_annotation("a", None, Some("1"))),
        "annotation of key a starts but never ends",
    );
    assert_ill_formed(
        b().annotation_boundary(finish_annotation(&["a"])),
        "annotation of key a ends without having started",
    );
    assert_ill_formed(
        b().annotation_boundary(begin_annotation("a", None, Some("1")))
            .annotation_boundary(finish_annotation(&["a"])),
        "adjacent annotation boundaries",
    );
    assert_ill_formed(
        b().annotation_boundary(dumb(&[], &[("a?", None, None)])),
        "invalid character in annotation key",
    );
    assert_ill_formed(
        b().annotation_boundary(dumb(&[], &[("a", None, Some("\u{FFFE}"))])),
        "annotation value is not valid UTF-16",
    );
}

#[test]
fn well_formed_but_not_applicable_to_empty_document() {
    let op = b()
        .retain(3)
        .delete_characters("abc")
        .annotation_boundary(begin_annotation("a", Some("1"), None))
        .retain(1)
        .annotation_boundary(finish_annotation(&["a"]))
        .build()
        .unwrap();
    assert!(is_well_formed(None, &op));
    assert_eq!(
        InvalidDocument,
        validate(None, &NO_SCHEMA_CONSTRAINTS, &EMPTY_DOCUMENT, &op)
    );
}

#[test]
fn validation_result_merging() {
    assert_eq!(IllFormed, Valid.merge_with(IllFormed));
    assert_eq!(InvalidDocument, InvalidSchema.merge_with(InvalidDocument));
    assert_eq!(InvalidSchema, InvalidSchema.merge_with(Valid));
    assert_eq!(Valid, Valid.merge_with(Valid));
}

#[test]
fn permitted_characters_coerce_string() {
    assert_eq!(None, PermittedCharacters::None.coerce_string("a"));
    assert_eq!(
        Some("a    b c\u{FFFD}😀".to_owned()),
        PermittedCharacters::BlipText.coerce_string("a\tb\nc\u{7}😀")
    );
    assert_eq!(
        Some("a\tb\u{FFFD}".to_owned()),
        PermittedCharacters::Any.coerce_string("a\tb\u{FFFE}")
    );
}
