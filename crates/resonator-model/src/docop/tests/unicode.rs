//! Tests for the deliberate deviations from the original: text is counted
//! and ordered in UTF-16 code units, characters outside the BMP are allowed,
//! and nothing may split a surrogate pair.

use super::{begin_annotation, finish_annotation};
use crate::docop::testing::creator;
use crate::docop::*;

use ValidationResult::{InvalidDocument, Valid};

fn doc_with(text: &str) -> BootstrapDocument {
    let mut doc = BootstrapDocument::new();
    doc.consume(&DocOpBuilder::new().characters(text).build().unwrap())
        .unwrap();
    doc
}

fn check(doc: &BootstrapDocument, op: &DocOp) -> ValidationResult {
    validate(None, &NO_SCHEMA_CONSTRAINTS, doc, op)
}

#[test]
fn supplementary_characters_are_well_formed() {
    // The original rejects these as "characters component contains surrogate".
    let op = DocOpBuilder::new()
        .characters("a😀")
        .delete_characters("𝄞")
        .build();
    assert!(op.is_ok());
}

#[test]
fn lengths_count_utf16_code_units() {
    let doc = doc_with("a😀b");
    assert_eq!(4, doc.length());
    let op = DocOpBuilder::new()
        .retain(1)
        .delete_characters("😀")
        .characters("𝄞𝄞")
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(4, initial_document_length(&op));
    assert_eq!(6, resulting_document_length(&op));
    assert_eq!(Valid, check(&doc, &op));
    // Counting code points instead would make these apply.
    assert_eq!(InvalidDocument, check(&doc, &creator::identity(3)));
    assert_eq!(Valid, check(&doc, &creator::identity(4)));
}

#[test]
fn document_round_trips_supplementary_characters() {
    let mut doc = doc_with("a😀b");
    assert_eq!("a😀b", doc.to_xml_string());
    doc.consume(&creator::delete_characters(4, 1, "😀"))
        .unwrap();
    assert_eq!("ab", doc.to_xml_string());
    doc.consume(&creator::insert_characters(2, 1, "𝄞\u{10FFFD}"))
        .unwrap();
    assert_eq!("a𝄞\u{10FFFD}b", doc.to_xml_string());
    let mut copy = BootstrapDocument::new();
    copy.consume(&doc.as_operation()).unwrap();
    assert_eq!(doc, copy);
}

#[test]
fn retain_must_not_end_inside_a_surrogate_pair() {
    let doc = doc_with("a😀b");
    // Between the two halves of the pair.
    assert_eq!(
        InvalidDocument,
        check(&doc, &creator::insert_characters(4, 2, "x"))
    );
    assert_eq!(
        InvalidDocument,
        check(
            &doc,
            &DocOpBuilder::new().retain(2).retain(2).build().unwrap()
        )
    );
    assert_eq!(
        InvalidDocument,
        check(
            &doc,
            &DocOpBuilder::new()
                .retain(2)
                .element_start("p", Attributes::new())
                .element_end()
                .retain(2)
                .build()
                .unwrap()
        )
    );
    assert_eq!(
        InvalidDocument,
        check(
            &doc,
            &creator::set_annotation(4, 2, 4, "k", None, Some("v"))
        )
    );
    assert_eq!(
        InvalidDocument,
        check(
            &doc,
            &creator::set_annotation(4, 0, 2, "k", None, Some("v"))
        )
    );
    // On either side of the pair is fine.
    assert_eq!(Valid, check(&doc, &creator::insert_characters(4, 1, "x")));
    assert_eq!(Valid, check(&doc, &creator::insert_characters(4, 3, "x")));
    assert_eq!(
        Valid,
        check(
            &doc,
            &creator::set_annotation(4, 1, 3, "k", None, Some("v"))
        )
    );

    let mut v = ViolationCollector::new();
    validate(
        Some(&mut v),
        &NO_SCHEMA_CONSTRAINTS,
        &doc,
        &creator::insert_characters(4, 2, "x"),
    );
    assert!(
        v.first_description()
            .unwrap()
            .contains("retain ends inside a surrogate pair")
    );
}

#[test]
fn insertion_inside_a_surrogate_pair_is_invalid() {
    let doc = doc_with("😀");
    let mut automaton = DocOpAutomaton::new(&doc, &NO_SCHEMA_CONSTRAINTS);
    assert_eq!(InvalidDocument, automaton.check_retain(1, None));
    assert_eq!(Valid, automaton.check_retain(2, None));
    assert_eq!(Valid, automaton.check_characters("x", None));
    // Force the automaton into the pair, as a generator of invalid ops might.
    automaton.do_retain(1);
    assert_eq!(InvalidDocument, automaton.check_characters("x", None));
    assert_eq!(
        InvalidDocument,
        automaton.check_element_start("p", &Attributes::new(), None)
    );
}

#[test]
fn deletion_must_match_whole_pairs() {
    let doc = doc_with("a😀b");
    assert_eq!(Valid, check(&doc, &creator::delete_characters(4, 1, "😀")));
    assert_eq!(
        Valid,
        check(&doc, &creator::delete_characters(4, 0, "a😀b"))
    );
    // Same high surrogate, different low surrogate.
    assert_eq!(
        InvalidDocument,
        check(&doc, &creator::delete_characters(4, 1, "😁"))
    );
    // A deletion cannot start or end in the middle of the pair: a Rust
    // string cannot hold half a pair, and the retain before it is invalid.
    assert_eq!(
        InvalidDocument,
        check(
            &doc,
            &DocOpBuilder::new()
                .retain(2)
                .delete_characters("b")
                .retain(1)
                .build()
                .unwrap()
        )
    );
    // Deleting only the character before the pair leaves the pair intact.
    assert_eq!(Valid, check(&doc, &creator::delete_characters(4, 0, "a")));
}

#[test]
fn compose_splits_text_between_pairs() {
    let insert = creator::insert_characters(0, 0, "a😀b𝄞");
    // Deletes the first pair and annotates the second.
    let second = DocOpBuilder::new()
        .retain(1)
        .delete_characters("😀")
        .retain(1)
        .annotation_boundary(begin_annotation("k", None, Some("v")))
        .retain(2)
        .annotation_boundary(finish_annotation(&["k"]))
        .build()
        .unwrap();
    let expected = DocOpBuilder::new()
        .characters("ab")
        .annotation_boundary(begin_annotation("k", None, Some("v")))
        .characters("𝄞")
        .annotation_boundary(finish_annotation(&["k"]))
        .build()
        .unwrap();
    assert_eq!(expected, compose(&insert, &second).unwrap());
}

#[test]
fn compose_reports_a_split_surrogate_pair() {
    let insert = creator::insert_characters(0, 0, "😀");
    // Not valid after `insert`: retains half of the pair.
    let split = creator::insert_characters(2, 1, "x");
    assert_eq!(
        Err(OperationError::SurrogateSplit { offset: 1 }),
        compose(&insert, &split)
    );
    let delete_half = DocOpBuilder::new()
        .delete_characters("a")
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(
        Err(OperationError::SurrogateSplit { offset: 1 }),
        compose(&insert, &delete_half)
    );
}

#[test]
fn transform_counts_utf16_code_units() {
    // Document "a😀b"; client inserts after the pair, server before it.
    let client = creator::insert_characters(4, 3, "𝄞");
    let server = creator::insert_characters(4, 1, "🚀🚀");
    let pair = transform(&client, &server).unwrap();
    assert_eq!(creator::insert_characters(8, 7, "𝄞"), pair.client_op);
    assert_eq!(creator::insert_characters(6, 1, "🚀🚀"), pair.server_op);

    // Concurrent overlapping deletions of text with pairs.
    let client = creator::delete_characters(6, 0, "a😀");
    let server = creator::delete_characters(6, 1, "😀b𝄞");
    let pair = transform(&client, &server).unwrap();
    assert_eq!(creator::delete_characters(1, 0, "a"), pair.client_op);
    assert_eq!(creator::delete_characters(3, 0, "b𝄞"), pair.server_op);

    let mut client_doc = doc_with("a😀b𝄞");
    let mut server_doc = client_doc.clone();
    client_doc.consume(&client).unwrap();
    client_doc.consume(&pair.server_op).unwrap();
    server_doc.consume(&server).unwrap();
    server_doc.consume(&pair.client_op).unwrap();
    assert_eq!(client_doc, server_doc);
    assert_eq!("", client_doc.to_xml_string());
}

#[test]
fn transform_reports_a_split_surrogate_pair() {
    // Document "😀"; the server op is invalid: it inserts inside the pair.
    let client = creator::delete_characters(2, 0, "😀");
    let server = creator::insert_characters(2, 1, "x");
    assert_eq!(
        Err(TransformError::Operation(OperationError::SurrogateSplit {
            offset: 1
        })),
        transform(&client, &server)
    );
    assert!(transform(&server, &client).is_err());
}

#[test]
fn keys_are_ordered_by_utf16_code_units() {
    // U+FF5E sorts after U+10000 in UTF-16 (0xFF5E > 0xD800) but before it
    // in code point (and UTF-8 byte) order. Both are XML name characters.
    let bmp = "\u{FF5E}";
    let astral = "\u{10000}";
    assert!(bmp < astral);

    let attributes = Attributes::from_pairs([(bmp, "1"), (astral, "2")]).unwrap();
    let names: Vec<&str> = attributes.iter().map(|(name, _)| name).collect();
    assert_eq!(vec![astral, bmp], names);

    let utf16_order = Attributes::from_sorted_unchecked(vec![
        Attribute::new(astral, "2"),
        Attribute::new(bmp, "1"),
    ]);
    let code_point_order = Attributes::from_sorted_unchecked(vec![
        Attribute::new(bmp, "1"),
        Attribute::new(astral, "2"),
    ]);
    assert!(Attributes::from_sorted(utf16_order.entries().to_vec()).is_ok());
    assert!(Attributes::from_sorted(code_point_order.entries().to_vec()).is_err());
    let element = |attributes: Attributes| {
        DocOpBuilder::new()
            .element_start("p", attributes)
            .element_end()
            .build_unchecked()
    };
    assert!(is_well_formed(None, &element(utf16_order)));
    assert!(!is_well_formed(None, &element(code_point_order)));

    // Annotation keys: validation and the normalizer use the same order.
    let annotate = |keys: [&str; 2]| {
        DocOpBuilder::new()
            .annotation_boundary(AnnotationBoundaryMap::new_unchecked(
                Vec::new(),
                keys.iter()
                    .map(|key| AnnotationChange::new(*key, None, Some("1")))
                    .collect(),
            ))
            .characters("x")
            .annotation_boundary(AnnotationBoundaryMap::new_unchecked(
                keys.iter().map(|key| key.to_string()).collect(),
                Vec::new(),
            ))
            .build_unchecked()
    };
    assert!(is_well_formed(None, &annotate([astral, bmp])));
    assert!(!is_well_formed(None, &annotate([bmp, astral])));
    assert_eq!(
        annotate([astral, bmp]),
        normalize(&annotate([bmp, astral])).unwrap()
    );

    // Updates sort the same way.
    let update = AttributesUpdate::from_triples([(bmp, None, None), (astral, None, None)]).unwrap();
    assert_eq!(astral, update.updates()[0].name);

    // And so does the document's XML form.
    let mut doc = BootstrapDocument::new();
    doc.consume(&annotate([astral, bmp])).unwrap();
    assert_eq!(
        format!("<?a \"{astral}\"=\"1\" \"{bmp}\"=\"1\"?>x<?a \"{astral}\" \"{bmp}\"?>"),
        doc.to_xml_string()
    );
}
