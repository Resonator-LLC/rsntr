//! Port of `org.waveprotocol.wave.model.conversation.SchemaConstraintsTest`
//! and tests of the lookup. The schemas themselves are compared with the
//! original's in `supplement::golden_tests::schema`.

use super::conversation::{BLIP_SCHEMA_CONSTRAINTS, MANIFEST_SCHEMA_CONSTRAINTS};
use super::*;
use crate::docop::{DocOpBuilder, ValidationResult, validate_initialization};

fn wid(id: &str) -> WaveletId {
    WaveletId::of("example.com", id).unwrap()
}

/// `SchemaConstraintsTest.testAbstractHelperClass`.
#[test]
fn abstract_helper_class() {
    let schema = &*BLIP_SCHEMA_CONSTRAINTS;
    assert!(!schema.permits_attribute("body", "x"));
    assert_eq!(schema.required_initial_children(Some("body")), ["line"]);
    assert_eq!(schema.required_initial_children(Some("textarea")), ["line"]);
    assert!(schema.required_initial_children(Some("line")).is_empty());
    assert!(schema.permits_attribute_value("reply", "id", "..."));
    assert!(schema.permits_attribute_value("reply", "id", "b+sdf"));
    assert!(schema.permits_attribute_value("line", "t", "h3"));
    assert!(!schema.permits_attribute_value("line", "t", "z"));
    assert!(schema.permits_attribute("image", "attachment"));
    assert!(schema.permits_attribute_value("image", "attachment", "blahblah"));
    assert!(!schema.permits_attribute("image", "something"));
    assert!(!schema.permits_attribute_value("image", "something", "stuff"));
    assert!(!schema.permits_attribute("caption", "something"));
    assert!(!schema.permits_attribute_value("caption", "something", "stuff"));
    assert!(schema.permits_child(Some("body"), "line"));
    assert!(schema.permits_child(Some("textarea"), "line"));
    assert!(schema.permits_child(None, "body"));
    assert!(!schema.permits_child(None, "blah"));
    assert_eq!(
        schema.permitted_characters(Some("body")),
        PermittedCharacters::BlipText
    );
    assert_eq!(
        schema.permitted_characters(Some("line")),
        PermittedCharacters::None
    );
    assert_eq!(schema.permitted_characters(None), PermittedCharacters::None);
}

#[test]
fn rules() {
    let mut schema = XmlSchemaConstraints::new();
    assert!(!schema.permits_child(None, "a"));
    schema.add_children(None, &["a"]);
    schema.add_children(Some("a"), &["b", "c"]);
    schema.add_children(Some("a"), &["d"]);
    assert!(schema.permits_child(None, "a"));
    assert!(!schema.permits_child(None, "b"));
    for child in ["b", "c", "d"] {
        assert!(schema.permits_child(Some("a"), child));
    }
    assert!(!schema.permits_child(Some("b"), "a"));
    // The empty string is not an element type.
    schema.add_children(Some(""), &["a"]);
    assert!(!schema.permits_child(Some(""), "a"));
    assert!(!schema.permits_child(None, ""));

    schema.add_attr_with_values("a", "x", &["1"]);
    schema.add_attr_with_values("a", "x", &["2"]);
    assert!(schema.permits_attribute("a", "x"));
    assert!(!schema.permits_attribute("a", "y"));
    assert!(!schema.permits_attribute("b", "x"));
    assert!(schema.permits_attribute_value("a", "x", "1"));
    assert!(schema.permits_attribute_value("a", "x", "2"));
    assert!(!schema.permits_attribute_value("a", "x", "3"));
    assert!(!schema.permits_attribute_value("a", "y", "1"));
    // Adding the attribute without values lifts the restriction, and
    // values added afterwards restrict it anew.
    schema.add_attrs("a", &["x"]);
    assert!(schema.permits_attribute_value("a", "x", "3"));
    schema.add_attr_with_values("a", "x", &["4"]);
    assert!(!schema.permits_attribute_value("a", "x", "1"));
    assert!(schema.permits_attribute_value("a", "x", "4"));

    schema.set_value_check(|tag, attr, value| (tag == "a" && attr == "n").then(|| value == "ok"));
    assert!(schema.permits_attribute_value("a", "n", "ok"));
    assert!(!schema.permits_attribute_value("a", "n", "no"));
    // The check answers for the value only: the attribute is not permitted.
    assert!(!schema.permits_attribute("a", "n"));
    assert!(schema.permits_attribute_value("a", "x", "4"));

    assert_eq!(
        schema.permitted_characters(Some("a")),
        PermittedCharacters::None
    );
    schema.contains_blip_text(&["a"]);
    schema.contains_any_text(&["b"]);
    assert_eq!(
        schema.permitted_characters(Some("a")),
        PermittedCharacters::BlipText
    );
    assert_eq!(
        schema.permitted_characters(Some("b")),
        PermittedCharacters::Any
    );
    assert_eq!(schema.permitted_characters(None), PermittedCharacters::None);
    assert_eq!(
        schema.permitted_characters(Some("")),
        PermittedCharacters::None
    );

    assert!(schema.required_initial_children(Some("a")).is_empty());
    schema.add_required_initial("a", &["b"]);
    assert_eq!(schema.required_initial_children(Some("a")), ["b"]);
    assert!(schema.required_initial_children(None).is_empty());
}

#[test]
fn value_predicates() {
    assert!(is_valid_integer("-5", -5));
    assert!(!is_valid_integer("-6", -5));
    assert!(is_valid_integer("+7", 0));
    assert!(!is_valid_integer("7.0", 0));
    assert!(!is_valid_integer("2147483648", 0));
    assert!(is_non_negative_integer("0"));
    assert!(!is_non_negative_integer("-1"));
    assert!(is_positive_integer("1"));
    assert!(!is_positive_integer("0"));
    assert!(is_wavelet_id("anything at all"));
    assert!(is_long("-9223372036854775808"));
    assert!(!is_long("9223372036854775808"));
    assert!(!is_long("1.5"));
    for good in [
        "1", "-1.5", " 2e3 ", ".5", "5.", "0x1p-2", "NaN", "Infinity", "1d", "1e999",
    ] {
        assert!(is_double(good), "{good}");
    }
    for bad in ["", " ", "one", "1e", "0x1", "infinity", "1 2", "++1"] {
        assert!(!is_double(bad), "{bad}");
    }
}

#[test]
fn lookup() {
    let schemas = SchemaCollection::standard();
    let is = |found: Option<&dyn DocumentSchema>, expected: &XmlSchemaConstraints| {
        let found = found.expect("a schema") as *const dyn DocumentSchema;
        std::ptr::addr_eq(found, expected as *const XmlSchemaConstraints)
    };
    let root = wid("conv+root");
    let udw = wid("user+alice@example.com");
    assert!(is(
        schemas.schema_for_id(&root, "b+abc"),
        &BLIP_SCHEMA_CONSTRAINTS
    ));
    assert!(is(
        schemas.schema_for_id(&root, "conversation"),
        &MANIFEST_SCHEMA_CONSTRAINTS
    ));
    assert!(is(
        schemas.schema_for_id(&root, "roles"),
        &account::ROLES_SCHEMA_CONSTRAINTS
    ));
    assert!(schemas.schema_for_id(&root, "tags").is_none());
    assert!(schemas.schema_for_id(&root, "m/read").is_none());
    // A legacy wavelet id without prefix is conversational.
    assert!(is(
        schemas.schema_for_id(&wid("legacy"), "b+abc"),
        &BLIP_SCHEMA_CONSTRAINTS
    ));
    for (doc, schema) in [
        ("m/read", &user_data::UDW_READ),
        ("m/presentation", &user_data::UDW_PRESENTATION),
        ("m/folder", &user_data::UDW_FOLDER),
        ("m/archiving", &user_data::UDW_ARCHIVE),
        ("m/muted", &user_data::UDW_MUTE),
        ("m/cleared", &user_data::UDW_CLEARED),
        ("m/abuse", &user_data::UDW_ABUSE),
        ("m/seen", &user_data::UDW_SEEN),
        ("m/gadgets", &user_data::UDW_GADGET),
    ] {
        assert!(is(schemas.schema_for_id(&udw, doc), schema), "{doc}");
        assert!(schemas.schema_for_id(&root, doc).is_none(), "{doc}");
    }
    assert!(schemas.schema_for_id(&udw, "b+abc").is_none());
    assert!(schemas.schema_for_id(&udw, "conversation").is_none());
    assert!(schemas.schema_for_id(&wid("other+x"), "b+abc").is_none());

    // Without a schema, anything goes.
    let none = schemas.schema_for_id_or_default(&udw, "m/other");
    assert!(none.permits_child(None, "anything"));
    assert!(none.permits_attribute_value("anything", "x", "y"));
    assert_eq!(none.permitted_characters(None), PermittedCharacters::Any);

    let empty = SchemaCollection::new();
    assert!(empty.schema_for_id(&root, "b+abc").is_none());

    // The first provider with a schema wins.
    struct Everything;
    impl SchemaProvider for Everything {
        fn schema_for_id(&self, _: &WaveletId, _: &str) -> Option<&dyn DocumentSchema> {
            Some(&*MANIFEST_SCHEMA_CONSTRAINTS)
        }
    }
    let mut both = SchemaCollection::new();
    both.add(ConversationSchemas);
    both.add(Everything);
    assert!(is(
        both.schema_for_id(&root, "b+abc"),
        &BLIP_SCHEMA_CONSTRAINTS
    ));
    assert!(is(
        both.schema_for_id(&udw, "m/read"),
        &MANIFEST_SCHEMA_CONSTRAINTS
    ));
}

/// The server's use: validate a document against the schema found for it.
#[test]
fn validation() {
    let schemas = SchemaCollection::standard();
    let udw = wid("user+alice@example.com");
    let schema = schemas.schema_for_id_or_default(&udw, "m/read");
    let read_state = |version: &str| {
        DocOpBuilder::new()
            .element_start(
                "wavelet",
                crate::docop::Attributes::from_pairs([("i", "example.com/conv+root")]).unwrap(),
            )
            .element_start(
                "all",
                crate::docop::Attributes::from_pairs([("v", version)]).unwrap(),
            )
            .element_end()
            .element_end()
            .build_initialization()
            .unwrap()
    };
    assert_eq!(
        validate_initialization(None, schema, &read_state("12")),
        ValidationResult::Valid
    );
    assert_eq!(
        validate_initialization(None, schema, &read_state("-12")),
        ValidationResult::InvalidSchema
    );
    // The same content is not a blip.
    let blip = schemas.schema_for_id_or_default(&wid("conv+root"), "b+abc");
    assert_eq!(
        validate_initialization(None, blip, &read_state("12")),
        ValidationResult::InvalidSchema
    );
}
