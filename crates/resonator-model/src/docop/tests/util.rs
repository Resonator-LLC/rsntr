//! Port of `DocOpUtilTest`, the document test of
//! `ModifiableDocumentTestBase` / `BootstrapDocumentTest`, and tests of the
//! other helpers.

use super::{attrs, begin_annotation, boundary, finish_annotation};
use crate::docop::*;

/// The first part of the test document, shared by both Java tests.
fn test_doc_prefix(text: &str) -> DocOpBuilder {
    // Check the 3 things we need to escape.
    let link1 = || begin_annotation("link", None, Some("12?\"\\3"));
    let link2 = begin_annotation("link", None, Some("1"));
    let ann = boundary(&[], &[("x", None, Some("3")), ("y", None, Some("3"))]);
    DocOpBuilder::new()
        .element_start("p", Attributes::new())
        .characters("hi ")
        .characters(text)
        .element_start("q", attrs(&["a", "1"]))
        // Check things we need to escape.
        .characters("<some>markup&")
        .element_end()
        // Check things we need to escape.
        .element_start("r", attrs(&["a", "2", "b", "\\\"'"]))
        .element_end()
        .element_start("q", Attributes::new())
        .annotation_boundary(link1())
        .element_end()
        .element_start("q", Attributes::new())
        .annotation_boundary(AnnotationBoundaryMap::empty())
        .element_end()
        .element_start("q", Attributes::new())
        .annotation_boundary(link1())
        .element_end()
        .element_start("r", Attributes::new())
        .annotation_boundary(link1())
        .element_end()
        .element_start("r", Attributes::new())
        .annotation_boundary(link2)
        .element_end()
        .annotation_boundary(ann)
        .characters("abc")
}

/// `DocOpUtilTest.TEST_DOC1`.
fn util_test_doc() -> DocOp {
    let link_null = begin_annotation("link", None, None);
    let change = boundary(
        &["x"],
        &[
            ("xa", None, None),
            ("xb", None, Some("5")),
            ("xc", None, Some("6")),
            ("z", None, Some("4")),
            ("zz", None, Some("7")),
        ],
    );
    let finish = finish_annotation(&["link", "xa", "xb", "xc", "y", "z", "zz"]);
    test_doc_prefix("there")
        .annotation_boundary(link_null)
        .characters("def")
        .annotation_boundary(change)
        .characters("ghi")
        .annotation_boundary(finish)
        .element_end()
        .build()
        .unwrap()
}

/// `ModifiableDocumentTestBase.TEST_DOC1`.
fn document_test_doc() -> DocOp {
    let link_null = finish_annotation(&["link"]);
    let change = boundary(
        &["x"],
        &[
            ("xb", None, Some("5")),
            ("xc", None, Some("6")),
            ("z", None, Some("4")),
            ("zz", None, Some("7")),
        ],
    );
    let finish = finish_annotation(&["xb", "xc", "y", "z", "zz"]);
    test_doc_prefix("therW")
        .annotation_boundary(link_null)
        .characters("def")
        .annotation_boundary(change)
        .characters("ghi")
        .annotation_boundary(finish)
        .element_end()
        .build()
        .unwrap()
}

fn test_doc_xml(text: &str) -> String {
    format!(
        "<p>hi {text}\
         <q a=\"1\">&lt;some&gt;markup&amp;</q>\
         <r a=\"2\" b=\"\\&quot;'\"/>\
         <q><?a \"link\"=\"12\\q\\\"\\\\3\"?></q>\
         <q/>\
         <q/>\
         <r/>\
         <r><?a \"link\"=\"1\"?></r>\
         <?a \"x\"=\"3\" \"y\"=\"3\"?>abc\
         <?a \"link\"?>def\
         <?a \"x\" \"xb\"=\"5\" \"xc\"=\"6\" \"z\"=\"4\" \"zz\"=\"7\"?>ghi\
         <?a \"xb\" \"xc\" \"y\" \"z\" \"zz\"?>\
         </p>"
    )
}

#[test]
fn xml_string_representation_is_minimal_and_normalised() {
    let init = as_initialization(util_test_doc()).unwrap();
    assert_eq!(test_doc_xml("there"), to_xml_string(&init).unwrap());
}

/// Checks the document's XML and that it survives a round trip through its
/// own initialization.
fn check(xml: &str, doc: &BootstrapDocument) {
    assert_eq!(xml, doc.to_xml_string());
    let mut copy = BootstrapDocument::new();
    copy.consume(&doc.as_operation()).unwrap();
    assert_eq!(xml, copy.to_xml_string());
    assert_eq!(*doc, copy);
}

// ModifiableDocumentTestBase.testDoc1
#[test]
fn document_applies_operations() {
    let mut doc = BootstrapDocument::new();
    doc.consume(&document_test_doc()).unwrap();
    check(&test_doc_xml("therW"), &doc);

    let op = DocOpBuilder::new()
        .update_attributes(AttributesUpdate::single("a", None, Some("2")))
        .delete_characters("hi t")
        .characters("you ")
        .retain(4)
        .replace_attributes(attrs(&["a", "1"]), attrs(&["b", "2"]))
        .retain("<some>markup&".len() + 1)
        .update_attributes(AttributesUpdate::single("a", Some("2"), Some("3")))
        .retain(2)
        .annotation_boundary(begin_annotation("link", Some("12?\"\\3"), Some("blah")))
        .retain(3)
        .annotation_boundary(finish_annotation(&["link"]))
        .retain(16)
        .build()
        .unwrap();
    doc.consume(&op).unwrap();
    check(
        "<p a=\"2\">you herW\
         <q b=\"2\">&lt;some&gt;markup&amp;</q>\
         <r a=\"3\" b=\"\\&quot;'\"/><q><?a \"link\"=\"blah\"?>\
         </q><q/><?a \"link\"=\"12\\q\\\"\\\\3\"?>\
         <q/>\
         <r/>\
         <r><?a \"link\"=\"1\"?></r>\
         <?a \"x\"=\"3\" \"y\"=\"3\"?>abc\
         <?a \"link\"?>def\
         <?a \"x\" \"xb\"=\"5\" \"xc\"=\"6\" \"z\"=\"4\" \"zz\"=\"7\"?>ghi\
         <?a \"xb\" \"xc\" \"y\" \"z\" \"zz\"?>\
         </p>",
        &doc,
    );
}

#[test]
fn document_rejects_invalid_operations_and_stays_unchanged() {
    let mut doc = BootstrapDocument::new();
    doc.consume(&DocOpBuilder::new().characters("abc").build().unwrap())
        .unwrap();
    let before = doc.clone();
    for op in [
        DocOpBuilder::new().retain(2).build().unwrap(),
        DocOpBuilder::new().retain(4).build().unwrap(),
        DocOpBuilder::new()
            .delete_characters("abd")
            .build()
            .unwrap(),
        DocOpBuilder::new()
            .retain(3)
            .element_start("p", Attributes::new())
            .build_unchecked(),
    ] {
        assert!(matches!(
            doc.consume(&op),
            Err(OperationError::Validation(_))
        ));
        assert_eq!(before, doc);
    }
}

#[test]
fn document_as_operation_is_normalized_and_valid() {
    let mut doc = BootstrapDocument::new();
    doc.consume(&document_test_doc()).unwrap();
    let init = doc.as_operation();
    assert_eq!(
        ValidationResult::Valid,
        validate_initialization(None, &NO_SCHEMA_CONSTRAINTS, &init)
    );
    assert_eq!(init, normalize_initialization(&init).unwrap());
    assert_eq!(doc.length(), resulting_document_length(&init));
    assert_eq!(0, initial_document_length(&init));
}

#[test]
fn concise_string() {
    let op = DocOpBuilder::new()
        .retain(3)
        .characters("a\"b\\")
        .delete_characters("x")
        .element_start("p", attrs(&["a", "1", "b", "2"]))
        .element_end()
        .delete_element_start("q", Attributes::new())
        .delete_element_end()
        .replace_attributes(Attributes::new(), attrs(&["a", "1"]))
        .update_attributes(
            AttributesUpdate::from_triples([("a", Some("1"), None), ("b", None, Some("2"))])
                .unwrap(),
        )
        .annotation_boundary(begin_annotation("k", None, Some("v")))
        .retain(1)
        .annotation_boundary(boundary(&["k"], &[("l", Some("1"), None)]))
        .retain(1)
        .annotation_boundary(finish_annotation(&["l"]))
        .annotation_boundary(AnnotationBoundaryMap::empty())
        .build_unchecked();
    assert_eq!(
        "__3; ++\"a\\\"b\\\\\"; --\"x\"; << p { a=\"1\", b=\"2\" }; >>; x< q {}; x>; \
         r@ {} { a=\"1\" }; u@ { a: \"1\" -> null, b: null -> \"2\" }; \
         || { \"k\": null -> \"v\" }; __1; || { \"k\", \"l\": \"1\" -> null }; __1; \
         || { \"l\" }; || {}; ",
        to_concise_string(&op)
    );
    assert_eq!(to_concise_string(&op), op.to_string());
}

#[test]
fn document_lengths() {
    let op = DocOpBuilder::new()
        .retain(3)
        .characters("ab")
        .delete_characters("xyz")
        .element_start("p", Attributes::new())
        .element_end()
        .delete_element_start("q", Attributes::new())
        .delete_element_end()
        .replace_attributes(Attributes::new(), Attributes::new())
        .update_attributes(AttributesUpdate::new())
        .build_unchecked();
    assert_eq!(3 + 3 + 2 + 2, initial_document_length(&op));
    assert_eq!(3 + 2 + 2 + 2, resulting_document_length(&op));
}

#[test]
fn initialization_rejects_other_components() {
    let op = DocOpBuilder::new().retain(1).build().unwrap();
    assert_eq!(
        Err(OperationError::NotAnInitialization("retain")),
        as_initialization(op)
    );
    assert!(
        DocOpBuilder::new()
            .characters("a")
            .build_initialization()
            .is_ok()
    );
}

#[test]
fn escapes_round_trip() {
    assert_eq!("a&amp;b&lt;c&gt;", util::xml_text_escape("a&b<c>"));
    assert_eq!("a&b<c>", util::xml_text_unescape("a&amp;b&lt;c&gt;"));
    assert_eq!("a\\\\b\\\"c\\q", util::annotation_escape("a\\b\"c?"));
    assert_eq!("a\\b\"c?", util::annotation_unescape("a\\\\b\\\"c\\q"));
    assert_eq!("&quot;", util::xml_attr_escape("\""));
}
