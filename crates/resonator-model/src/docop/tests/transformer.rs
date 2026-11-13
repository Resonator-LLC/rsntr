//! Port of `DocOpTransformerTest` and `TransformerTest`: expected results of
//! transforming specific pairs of operations. The cases were translated
//! mechanically from the Java source.

use super::{attrs, begin_annotation, finish_annotation};
use crate::docop::testing::creator;
use crate::docop::*;

fn single_test(
    client: &DocOp,
    server: &DocOp,
    transformed_client: &DocOp,
    transformed_server: &DocOp,
) {
    let pair = transform(client, server).expect("transform failed");
    assert_eq!(
        to_concise_string(transformed_client),
        to_concise_string(&pair.client_op)
    );
    assert_eq!(
        to_concise_string(transformed_server),
        to_concise_string(&pair.server_op)
    );
}

fn single(client: DocOp, server: DocOp, transformed_client: DocOp, transformed_server: DocOp) {
    single_test(&client, &server, &transformed_client, &transformed_server);
}

/// Also checks the transformation with client and server swapped.
fn reversible(client: DocOp, server: DocOp, transformed_client: DocOp, transformed_server: DocOp) {
    single_test(&client, &server, &transformed_client, &transformed_server);
    single_test(&server, &client, &transformed_server, &transformed_client);
}

fn sample_structural(size: usize, location: usize) -> DocOp {
    DocOpBuilder::new()
        .retain(location)
        .element_start("sampleElement", Attributes::new())
        .characters("sample text")
        .element_end()
        .retain(size - location)
        .build()
        .unwrap()
}

// TransformerTest.testClientOpLongerThanServerOp
#[test]
fn client_op_longer_than_server_op() {
    let client = DocOpBuilder::new().retain(1).build().unwrap();
    let server = DocOpBuilder::new().build().unwrap();
    assert!(matches!(
        transform(&client, &server),
        Err(TransformError::RanOut { .. })
    ));
}

// Performs tests for transforming text deletions against text deletions.
#[test]
fn delete_vs_delete() {
    // A's deletion spatially before B's deletion
    reversible(
        creator::delete_characters(20, 1, "abcde"),
        creator::delete_characters(20, 7, "fg"),
        creator::delete_characters(18, 1, "abcde"),
        creator::delete_characters(15, 2, "fg"),
    );
    // A's deletion spatially adjacent to and before B's deletion
    reversible(
        creator::delete_characters(20, 1, "abcde"),
        creator::delete_characters(20, 6, "fg"),
        creator::delete_characters(18, 1, "abcde"),
        creator::delete_characters(15, 1, "fg"),
    );
    // A's deletion overlaps B's deletion
    reversible(
        creator::delete_characters(20, 1, "abcde"),
        creator::delete_characters(20, 3, "cdefghi"),
        creator::delete_characters(13, 1, "ab"),
        creator::delete_characters(15, 1, "fghi"),
    );
    // A's deletion a subset of B's deletion
    reversible(
        creator::delete_characters(20, 1, "abcdefg"),
        creator::delete_characters(20, 3, "cd"),
        creator::delete_characters(18, 1, "abefg"),
        creator::identity(13),
    );
    // A's deletion identical to B's deletion
    reversible(
        creator::delete_characters(20, 1, "abcdefg"),
        creator::delete_characters(20, 1, "abcdefg"),
        creator::identity(13),
        creator::identity(13),
    );
}
// Performs tests for transforming text insertions against text deletions.
#[test]
fn insert_vs_delete() {
    // A's insertion spatially before B's deletion
    reversible(
        creator::insert_characters(20, 1, "abc"),
        creator::delete_characters(20, 2, "de"),
        creator::insert_characters(18, 1, "abc"),
        creator::delete_characters(23, 5, "de"),
    );
    // A's insertion spatially inside B's deletion
    reversible(
        creator::insert_characters(20, 2, "abc"),
        creator::delete_characters(20, 1, "ce"),
        creator::insert_characters(18, 1, "abc"),
        DocOpBuilder::new()
            .retain(1)
            .delete_characters("c")
            .retain(3)
            .delete_characters("e")
            .retain(17)
            .build()
            .unwrap(),
    );
    // A's insertion spatially at the start of B's deletion
    reversible(
        creator::insert_characters(20, 1, "abc"),
        creator::delete_characters(20, 1, "de"),
        creator::insert_characters(18, 1, "abc"),
        creator::delete_characters(23, 4, "de"),
    );
    // A's insertion spatially at the end of B's deletion
    reversible(
        creator::insert_characters(20, 3, "abc"),
        creator::delete_characters(20, 1, "de"),
        creator::insert_characters(18, 1, "abc"),
        creator::delete_characters(23, 1, "de"),
    );
    // A's insertion spatially after B's deletion
    reversible(
        creator::insert_characters(20, 4, "abc"),
        creator::delete_characters(20, 1, "de"),
        creator::insert_characters(18, 2, "abc"),
        creator::delete_characters(23, 1, "de"),
    );
}
// Performs tests for transforming text insertions against text insertions.
#[test]
fn insert_vs_insert() {
    // A's insertion spatially before B's insertion
    reversible(
        creator::insert_characters(20, 1, "a"),
        creator::insert_characters(20, 2, "1"),
        creator::insert_characters(21, 1, "a"),
        creator::insert_characters(21, 3, "1"),
    );
    // client's insertion spatially at the same location as server's insertion
    single(
        creator::insert_characters(20, 2, "abc"),
        creator::insert_characters(20, 2, "123"),
        creator::insert_characters(23, 2, "abc"),
        creator::insert_characters(23, 5, "123"),
    );
}
// Performs tests for transforming element insertions against text insertions.
#[test]
fn structural_vs_insert() {
    // A's insertion spatially before B's insertion
    reversible(
        creator::insert_characters(20, 1, "a"),
        sample_structural(20, 2),
        creator::insert_characters(33, 1, "a"),
        sample_structural(21, 3),
    );
    // A's insertion spatially after B's insertion
    reversible(
        creator::insert_characters(20, 2, "a"),
        sample_structural(20, 1),
        creator::insert_characters(33, 15, "a"),
        sample_structural(21, 1),
    );
    // client's insertion spatially at the same location as server's insertion
    single(
        creator::insert_characters(20, 2, "a"),
        sample_structural(20, 2),
        creator::insert_characters(33, 2, "a"),
        sample_structural(21, 3),
    );
    // client's insertion spatially at the same location as server's insertion
    single(
        sample_structural(20, 2),
        creator::insert_characters(20, 2, "a"),
        sample_structural(21, 2),
        creator::insert_characters(33, 15, "a"),
    );
}
// Performs tests for transforming element insertions against element
// insertions.
#[test]
fn structural_vs_structural() {
    // A's insertion spatially before B's insertion
    reversible(
        sample_structural(20, 1),
        sample_structural(20, 2),
        sample_structural(33, 1),
        sample_structural(33, 15),
    );
    // client's insertion spatially at the same location as server's insertion
    single(
        sample_structural(20, 2),
        sample_structural(20, 2),
        sample_structural(33, 2),
        sample_structural(33, 15),
    );
}
// Performs tests for transforming attribute settings against text deletions.
#[test]
fn attributes_vs_delete() {
    // A's attributes replacement spatially before B's deletion
    reversible(
        creator::replace_attributes(
            20,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::replace_attributes(
            17,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
    // A's attributes replacement spatially adjacent to and before B's deletion
    reversible(
        creator::replace_attributes(
            20,
            4,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::replace_attributes(
            17,
            4,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
    // A's attributes replacement spatially after B's deletion
    reversible(
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::replace_attributes(
            17,
            6,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
    // A's attributes replacement adjacent to and after B's deletion
    reversible(
        creator::replace_attributes(
            20,
            8,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::replace_attributes(
            17,
            5,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
}
// Performs tests for transforming attribute settings against text insertions.
#[test]
fn attributes_vs_insert() {
    // A's attributes replacement spatially before B's insertion
    reversible(
        creator::replace_attributes(
            20,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 6, "hello"),
        creator::replace_attributes(
            25,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 6, "hello"),
    );
    // A's attributes replacement spatially after B's insertion
    reversible(
        creator::replace_attributes(
            20,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 2, "hello"),
        creator::replace_attributes(
            25,
            8,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 2, "hello"),
    );
    // A's attributes replacement spatially adjacent to and after B's insertion
    reversible(
        creator::replace_attributes(
            20,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 3, "hello"),
        creator::replace_attributes(
            25,
            8,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 3, "hello"),
    );
    // A's attributes replacement spatially adjacent to and before B's insertion
    reversible(
        creator::replace_attributes(
            20,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 4, "hello"),
        creator::replace_attributes(
            25,
            3,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::insert_characters(20, 4, "hello"),
    );
}
// Performs tests for transforming attribute settings against attribute
// settings.
#[test]
fn attributes_vs_attributes() {
    // A's attributes replacement spatially before B's attributes replacement
    reversible(
        creator::replace_attributes(
            20,
            5,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
        creator::replace_attributes(
            20,
            5,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
    );
    // client's attributes replacement coincides with server's attributes replacement
    single(
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["name", "Google!"]),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "Google!"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::identity(20),
    );
    // client's attributes replacement coincides with server's attributes replacement
    single(
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["href", "http://www.yahoo.com/"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::identity(20),
    );
}
// Performs tests for transforming an attribute setting against an element
// deletion.
#[test]
fn attributes_vs_element_deletion() {
    // A's attributes replacement coincides with B's element deletion.
    reversible(
        creator::replace_attributes(
            20,
            6,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.google.com/"]),
        ),
        creator::delete_element(20, 6, "type", attrs(&["name", "value"])),
        creator::identity(18),
        creator::delete_element(20, 6, "type", attrs(&["href", "http://www.google.com/"])),
    );
}
// Performs tests for transforming attribute settings against text deletions.
#[test]
fn attribute_vs_delete() {
    // A's attribute update spatially before B's deletion.
    reversible(
        creator::set_attribute(
            20,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::set_attribute(
            17,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
    // A's attribute update spatially adjacent to and before B's deletion.
    reversible(
        creator::set_attribute(
            20,
            4,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::set_attribute(
            17,
            4,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
    // A's attribute update spatially after B's deletion.
    reversible(
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::set_attribute(
            17,
            6,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
    // A's attribute update spatially adjacent to and after B's deletion.
    reversible(
        creator::set_attribute(
            20,
            8,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
        creator::set_attribute(
            17,
            5,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_characters(20, 5, "abc"),
    );
}
// Performs tests for transforming attribute settings against text insertions.
#[test]
fn attribute_vs_insert() {
    // A's attribute update spatially before B's insertion.
    reversible(
        creator::set_attribute(
            20,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 6, "hello"),
        creator::set_attribute(
            25,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 6, "hello"),
    );
    // A's attribute update spatially adjacent to and before B's insertion.
    reversible(
        creator::set_attribute(
            20,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 4, "hello"),
        creator::set_attribute(
            25,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 4, "hello"),
    );
    // A's attribute update spatially after B's insertion.
    reversible(
        creator::set_attribute(
            20,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 2, "hello"),
        creator::set_attribute(
            25,
            8,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 2, "hello"),
    );
    // A's attribute update spatially adjacent to and after B's insertion.
    reversible(
        creator::set_attribute(
            20,
            3,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 3, "hello"),
        creator::set_attribute(
            25,
            8,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::insert_characters(20, 3, "hello"),
    );
}
// Performs tests for transforming attribute settings against attribute
// settings.
#[test]
fn attribute_vs_attributes() {
    // A's attribute update spatially before B's attributes replacement.
    reversible(
        creator::set_attribute(
            20,
            5,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
        creator::set_attribute(
            20,
            5,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
    );
    // A's attribute update spatially after B's attributes replacement.
    reversible(
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::replace_attributes(
            20,
            5,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::replace_attributes(
            20,
            5,
            attrs(&["name", "value"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
    );
    // A's attribute update coincides with B's attributes replacement.
    reversible(
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["href", "initial"]),
            attrs(&["name", "Google!"]),
        ),
        creator::identity(20),
        creator::replace_attributes(
            20,
            9,
            attrs(&["href", "http://www.google.com/"]),
            attrs(&["name", "Google!"]),
        ),
    );
    // A's attribute update coincides with B's attributes replacement.
    reversible(
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::replace_attributes(
            20,
            9,
            attrs(&["href", "initial"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
        creator::identity(20),
        creator::replace_attributes(
            20,
            9,
            attrs(&["href", "http://www.google.com/"]),
            attrs(&["href", "http://www.yahoo.com/"]),
        ),
    );
}
// Performs tests for transforming attribute settings against attribute
// settings.
#[test]
fn attribute_vs_attribute() {
    // A's attribute update spatially before B's attribute update.
    reversible(
        creator::set_attribute(
            20,
            5,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.yahoo.com/"),
        ),
        creator::set_attribute(
            20,
            5,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.yahoo.com/"),
        ),
    );
    // A's attribute update has different key than B's attribute update.
    reversible(
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::set_attribute(20, 9, "name", Some("initial"), Some("Google!")),
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::set_attribute(20, 9, "name", Some("initial"), Some("Google!")),
    );
    // client's attribute update has same key as server's attribute update.
    single(
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::set_attribute(
            20,
            9,
            "href",
            Some("initial"),
            Some("http://www.yahoo.com/"),
        ),
        creator::set_attribute(
            20,
            9,
            "href",
            Some("http://www.yahoo.com/"),
            Some("http://www.google.com/"),
        ),
        DocOpBuilder::new()
            .retain(9)
            .update_attributes(AttributesUpdate::new())
            .retain(10)
            .build()
            .unwrap(),
    );
}
// Performs tests for transforming an attribute setting against an element
// deletion.
#[test]
fn attribute_vs_element_deletion() {
    // A's attribute update coincides with B's element deletion
    reversible(
        creator::set_attribute(
            20,
            6,
            "href",
            Some("initial"),
            Some("http://www.google.com/"),
        ),
        creator::delete_element(20, 6, "type", attrs(&["href", "initial"])),
        creator::identity(18),
        creator::delete_element(20, 6, "type", attrs(&["href", "http://www.google.com/"])),
    );
}
// Performs tests for transforming a structural deletion against a text
// insertion.
#[test]
fn structural_deletion_vs_insert() {
    // A's deletion engulfs B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::insert_characters(20, 6, "hello"),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("hellocdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::identity(5),
    );
    // A's deletion engulfs B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::insert_characters(20, 7, "hello"),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("chellodefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::identity(5),
    );
    // A's deletion engulfs B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::insert_characters(20, 16, "hello"),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_characters("hello")
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::identity(5),
    );
    // A's deletion spatially adjacent to and before B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::insert_characters(20, 17, "hello"),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(8)
            .build()
            .unwrap(),
        creator::insert_characters(5, 2, "hello"),
    );
    // A's deletion spatially before B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::insert_characters(20, 18, "hello"),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(8)
            .build()
            .unwrap(),
        creator::insert_characters(5, 3, "hello"),
    );
}
// Performs tests for transforming a structural deletion against an element
// insertion.
#[test]
fn structural_deletion_vs_structural() {
    // A's deletion engulfs B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        sample_structural(20, 6),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_element_start("sampleElement", Attributes::new())
            .delete_characters("sample text")
            .delete_element_end()
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::identity(5),
    );
    // A's deletion engulfs B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        sample_structural(20, 7),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("c")
            .delete_element_start("sampleElement", Attributes::new())
            .delete_characters("sample text")
            .delete_element_end()
            .delete_characters("defg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::identity(5),
    );
    // A's deletion engulfs B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        sample_structural(20, 16),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_start("sampleElement", Attributes::new())
            .delete_characters("sample text")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::identity(5),
    );
    // A's deletion spatially adjacent to and before B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        sample_structural(20, 17),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(16)
            .build()
            .unwrap(),
        sample_structural(5, 2),
    );
    // A's deletion spatially before B's insertion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        sample_structural(20, 18),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(16)
            .build()
            .unwrap(),
        sample_structural(5, 3),
    );
}
// Performs tests for transforming structural deletions.
#[test]
fn structural_deletion_transformations() {
    // A's deletion engulfs B's deletion
    reversible(
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        DocOpBuilder::new()
            .retain(5)
            .delete_element_start("type", Attributes::new())
            .delete_characters("cdefg")
            .delete_element_end()
            .retain(8)
            .build()
            .unwrap(),
        DocOpBuilder::new()
            .retain(2)
            .delete_element_start("type", Attributes::new())
            .delete_characters("ab")
            .delete_element_start("type", Attributes::new())
            .delete_characters("hi")
            .delete_element_end()
            .delete_element_end()
            .retain(3)
            .build()
            .unwrap(),
        creator::identity(5),
    );
}
// Performs tests for transforming annotation modifications against deletions.
#[test]
fn annotation_vs_delete() {
    // A's annotation spatially before B's deletion
    reversible(
        creator::set_annotation(20, 4, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 7, "ab"),
        creator::set_annotation(18, 4, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 7, "ab"),
    );
    // A's annotation spatially after B's deletion
    reversible(
        creator::set_annotation(20, 4, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 1, "ab"),
        creator::set_annotation(18, 2, 4, "hello", None, Some("world")),
        creator::delete_characters(20, 1, "ab"),
    );
    // A's annotation spatially adjacent to and before B's deletion
    reversible(
        creator::set_annotation(20, 4, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 6, "abc"),
        creator::set_annotation(17, 4, 6, "hello", None, Some("world")),
        DocOpBuilder::new()
            .retain(6)
            .annotation_boundary(begin_annotation("hello", None, Some("world")))
            .delete_characters("abc")
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
    );
    // A's annotation spatially adjacent to and after B's deletion
    reversible(
        creator::set_annotation(20, 4, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 1, "abc"),
        creator::set_annotation(17, 1, 3, "hello", None, Some("world")),
        creator::delete_characters(20, 1, "abc"),
    );
    // A's annotation overlaps B's deletion
    reversible(
        creator::set_annotation(20, 4, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 1, "abcd"),
        creator::set_annotation(16, 1, 2, "hello", None, Some("world")),
        DocOpBuilder::new()
            .retain(1)
            .delete_characters("abc")
            .annotation_boundary(begin_annotation("hello", Some("world"), None))
            .delete_characters("d")
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(15)
            .build()
            .unwrap(),
    );
    // A's annotation overlaps B's deletion
    reversible(
        creator::set_annotation(20, 4, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 5, "abcd"),
        creator::set_annotation(16, 4, 5, "hello", None, Some("world")),
        DocOpBuilder::new()
            .retain(5)
            .delete_characters("a")
            .annotation_boundary(begin_annotation("hello", None, Some("world")))
            .delete_characters("bcd")
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
    );
    // A's annotation encloses B's deletion
    reversible(
        creator::set_annotation(20, 2, 8, "hello", None, Some("world")),
        creator::delete_characters(20, 5, "ab"),
        creator::set_annotation(18, 2, 6, "hello", None, Some("world")),
        creator::delete_characters(20, 5, "ab"),
    );
    // A's annotation inside B's deletion
    reversible(
        creator::set_annotation(20, 5, 7, "hello", None, Some("world")),
        creator::delete_characters(20, 2, "abcdef"),
        creator::identity(14),
        DocOpBuilder::new()
            .retain(2)
            .delete_characters("abc")
            .annotation_boundary(begin_annotation("hello", Some("world"), None))
            .delete_characters("de")
            .annotation_boundary(finish_annotation(&["hello"]))
            .delete_characters("f")
            .retain(12)
            .build()
            .unwrap(),
    );
    // A's operation clears an annotation
    reversible(
        creator::set_annotation(20, 4, 6, "hello", Some("world"), None),
        creator::delete_characters(20, 6, "abc"),
        creator::set_annotation(17, 4, 6, "hello", Some("world"), None),
        DocOpBuilder::new()
            .retain(6)
            .annotation_boundary(begin_annotation("hello", Some("world"), None))
            .delete_characters("abc")
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
    );
}
// Performs tests for transforming annotation modifications against
// insertions.
#[test]
fn annotation_vs_insert() {
    // A's annotation spatially after B's insertion
    reversible(
        creator::set_annotation(20, 3, 5, "hello", None, Some("world")),
        creator::insert_characters(20, 2, "abcd"),
        creator::set_annotation(24, 7, 9, "hello", None, Some("world")),
        creator::insert_characters(20, 2, "abcd"),
    );
    // A's annotation spatially before B's insertion
    reversible(
        creator::set_annotation(20, 3, 5, "hello", None, Some("world")),
        creator::insert_characters(20, 6, "abcd"),
        creator::set_annotation(24, 3, 5, "hello", None, Some("world")),
        creator::insert_characters(20, 6, "abcd"),
    );
    // A's annotation encloses B's insertion
    reversible(
        creator::set_annotation(20, 3, 5, "hello", None, Some("world")),
        creator::insert_characters(20, 4, "abcd"),
        creator::set_annotation(24, 3, 9, "hello", None, Some("world")),
        creator::insert_characters(20, 4, "abcd"),
    );
    // A's annotation spatially adjacent to and after B's insertion
    reversible(
        creator::set_annotation(20, 3, 5, "hello", None, Some("world")),
        creator::insert_characters(20, 3, "abcd"),
        creator::set_annotation(24, 7, 9, "hello", None, Some("world")),
        creator::insert_characters(20, 3, "abcd"),
    );
    // A's annotation spatially adjacent to and before B's insertion
    reversible(
        creator::set_annotation(20, 3, 5, "hello", None, Some("world")),
        creator::insert_characters(20, 5, "abcd"),
        creator::set_annotation(24, 3, 9, "hello", None, Some("world")),
        creator::insert_characters(20, 5, "abcd"),
    );
    // A's operation clears an annotation
    reversible(
        creator::set_annotation(20, 3, 5, "hello", Some("world"), None),
        creator::insert_characters(20, 4, "abcd"),
        creator::set_annotation(24, 3, 9, "hello", Some("world"), None),
        creator::insert_characters(20, 4, "abcd"),
    );
}
// Performs tests for transforming annotation modifications against annotation
// modifications.
#[test]
fn annotation_vs_annotation() {
    // A's annotation overlaps B's annotation and has different key
    reversible(
        creator::set_annotation(20, 2, 6, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 9, "hi", Some("initial"), Some("there")),
        creator::set_annotation(20, 2, 6, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 9, "hi", Some("initial"), Some("there")),
    );
    // A's annotation overlaps B's annotation and has different key
    reversible(
        creator::set_annotation(20, 2, 9, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 7, "hi", Some("initial"), Some("there")),
        creator::set_annotation(20, 2, 9, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 7, "hi", Some("initial"), Some("there")),
    );
    // A's annotation spatially before B's annotation
    reversible(
        creator::set_annotation(20, 2, 5, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 6, 9, "hello", Some("initial"), Some("there")),
        creator::set_annotation(20, 2, 5, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 6, 9, "hello", Some("initial"), Some("there")),
    );
    // A's annotation spatially adjacent to and before B's annotation
    reversible(
        creator::set_annotation(20, 2, 5, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 9, "hello", Some("initial"), Some("there")),
        creator::set_annotation(20, 2, 5, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 9, "hello", Some("initial"), Some("there")),
    );
    // client's annotation overlaps server's annotation
    single(
        creator::set_annotation(20, 2, 6, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 9, "hello", Some("initial"), Some("there")),
        DocOpBuilder::new()
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(3)
            .annotation_boundary(begin_annotation("hello", Some("there"), Some("world")))
            .retain(1)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(14)
            .build()
            .unwrap(),
        creator::set_annotation(20, 6, 9, "hello", Some("initial"), Some("there")),
    );
    // client's annotation overlaps server's annotation
    single(
        creator::set_annotation(20, 5, 9, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 2, 6, "hello", Some("initial"), Some("there")),
        DocOpBuilder::new()
            .retain(5)
            .annotation_boundary(begin_annotation("hello", Some("there"), Some("world")))
            .retain(1)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(3)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
        creator::set_annotation(20, 2, 5, "hello", Some("initial"), Some("there")),
    );
    // client's annotation encloses server's annotation
    single(
        creator::set_annotation(20, 2, 9, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 5, 7, "hello", Some("initial"), Some("there")),
        DocOpBuilder::new()
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(3)
            .annotation_boundary(begin_annotation("hello", Some("there"), Some("world")))
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(2)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
        creator::identity(20),
    );
    // client's annotation inside server's annotation
    single(
        creator::set_annotation(20, 5, 7, "hello", Some("initial"), Some("world")),
        creator::set_annotation(20, 2, 9, "hello", Some("initial"), Some("there")),
        creator::set_annotation(20, 5, 7, "hello", Some("there"), Some("world")),
        DocOpBuilder::new()
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("there")))
            .retain(3)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("there")))
            .retain(2)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
    );
    // client's annotation overlaps server's incontiguous annotation
    single(
        creator::set_annotation(20, 4, 8, "hello", Some("initial"), Some("world")),
        DocOpBuilder::new()
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("there")))
            .retain(3)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("there")))
            .retain(2)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
        DocOpBuilder::new()
            .retain(4)
            .annotation_boundary(begin_annotation("hello", Some("there"), Some("world")))
            .retain(1)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("there"), Some("world")))
            .retain(1)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(12)
            .build()
            .unwrap(),
        DocOpBuilder::new()
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("there")))
            .retain(2)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(4)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("there")))
            .retain(1)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
    );
    // client's incontiguous annotation overlaps server's annotation
    single(
        DocOpBuilder::new()
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(3)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(2)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
        creator::set_annotation(20, 4, 8, "hello", Some("initial"), Some("there")),
        DocOpBuilder::new()
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("there"), Some("world")))
            .retain(1)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(2)
            .annotation_boundary(begin_annotation("hello", Some("there"), Some("world")))
            .retain(1)
            .annotation_boundary(begin_annotation("hello", Some("initial"), Some("world")))
            .retain(1)
            .annotation_boundary(finish_annotation(&["hello"]))
            .retain(11)
            .build()
            .unwrap(),
        creator::set_annotation(20, 5, 7, "hello", Some("initial"), Some("there")),
    );
}
