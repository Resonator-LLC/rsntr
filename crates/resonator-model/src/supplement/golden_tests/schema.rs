//! Scenarios of the schemas (`schemaScenarios` in the generator). The
//! golden records hold both the questions and the answers.

use super::{golden_scenario, list, nullable, result_name, unescape, wid};
use crate::doc::XmlFragment;
use crate::docop::{
    DocInitialization, DocumentSchema, PermittedCharacters, ViolationCollector,
    validate_initialization,
};
use crate::schema::{SchemaCollection, SchemaProvider};

/// The fields of the `Q` records of a scenario.
fn questions(scenario: &str) -> Vec<Vec<String>> {
    golden_scenario(scenario)
        .into_iter()
        .map(|line| {
            let mut fields = line.split('\t').map(unescape);
            assert_eq!(fields.next().as_deref(), Some("Q"), "{line}");
            fields.collect()
        })
        .collect()
}

fn characters(permitted: PermittedCharacters) -> &'static str {
    match permitted {
        PermittedCharacters::None => "NONE",
        PermittedCharacters::BlipText => "BLIP_TEXT",
        PermittedCharacters::Any => "ANY",
    }
}

#[test]
fn schema_lookup() {
    let schemas = SchemaCollection::standard();
    let records = questions("schema-lookup");
    assert!(records.len() > 20);
    for record in records {
        let found = schemas.schema_for_id(&wid(&record[0]), &record[1]);
        let schema = schemas.schema_for_id_or_default(&wid(&record[0]), &record[1]);
        let mut actual = vec![found.is_some()];
        for child in ["body", "conversation", "assign", "wavelet", "anything"] {
            actual.push(schema.permits_child(None, child));
        }
        let actual: Vec<String> = actual.iter().map(bool::to_string).collect();
        assert_eq!(&record[2..], &actual[..], "{record:?}");
    }
}

/// Each record is about a parent element type: the text it permits, its
/// required initial children, then each child type followed by whether it
/// is permitted.
#[test]
fn schema_structure() {
    let schemas = SchemaCollection::standard();
    let records = questions("schema-structure");
    assert!(records.len() > 200);
    for record in records {
        let schema = schemas.schema_for_id_or_default(&wid(&record[0]), &record[1]);
        let parent = (record[2] != "null").then_some(record[2].as_str());
        let mut actual = vec![
            characters(schema.permitted_characters(parent)).to_owned(),
            list(schema.required_initial_children(parent)),
        ];
        for pair in record[5..].chunks(2) {
            actual.push(pair[0].clone());
            actual.push(schema.permits_child(parent, &pair[0]).to_string());
        }
        assert_eq!(&record[3..], &actual[..], "{record:?}");
    }
}

/// Each record is about an attribute of an element type: whether it is
/// permitted, then each value followed by whether it is permitted.
#[test]
fn schema_attributes() {
    let schemas = SchemaCollection::standard();
    let records = questions("schema-attributes");
    assert!(records.len() > 300);
    let mut exceptions = 0;
    for record in records {
        let schema = schemas.schema_for_id_or_default(&wid(&record[0]), &record[1]);
        let (tag, attr) = (&record[2], &record[3]);
        assert_eq!(
            record[4],
            schema.permits_attribute(tag, attr).to_string(),
            "{record:?}"
        );
        for pair in record[5..].chunks(2) {
            // Where the original throws, the port answers false.
            let expected = match pair[1].as_str() {
                "false (exception)" => {
                    exceptions += 1;
                    "false"
                }
                other => other,
            };
            assert_eq!(
                expected,
                schema
                    .permits_attribute_value(tag, attr, &pair[0])
                    .to_string(),
                "{tag} {attr}={:?} in {} of {}",
                pair[0],
                record[1],
                record[0]
            );
        }
    }
    assert!(exceptions > 0);
}

fn initialization(xml: &str) -> DocInitialization {
    let mut b = crate::docop::DocOpBuilder::new();
    for item in XmlFragment::parse(xml).unwrap().items() {
        b = match item {
            crate::doc::XmlItem::Text(text) => b.characters(text.clone()),
            crate::doc::XmlItem::Start { tag, attributes } => {
                b.element_start(tag.clone(), attributes.clone())
            }
            crate::doc::XmlItem::End => b.element_end(),
        };
    }
    b.build_initialization().unwrap()
}

#[test]
fn schema_documents() {
    let schemas = SchemaCollection::standard();
    let records = questions("schema-documents");
    assert!(records.len() > 60);
    for record in records {
        let schema: &dyn DocumentSchema =
            schemas.schema_for_id_or_default(&wid(&record[0]), &record[1]);
        let mut violations = ViolationCollector::new();
        let result =
            validate_initialization(Some(&mut violations), schema, &initialization(&record[2]));
        let actual = [
            result_name(result).to_owned(),
            nullable(violations.first_description()),
        ];
        assert_eq!(&record[3..], &actual[..], "{record:?}");
    }
}
