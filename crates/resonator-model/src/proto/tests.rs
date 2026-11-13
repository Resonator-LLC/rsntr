//! Ports `WaveletOperationSerializerTest` and `SnapshotSerializerTest`
//! (with `TestDataUtil`) of the Apache Wave server, and adds tests of the
//! wire encoding against hand-computed bytes, of decoder and converter
//! validation, and of round trips through bytes.

use std::fmt::Debug;

use sha2::{Digest, Sha256};

use super::delta_store::ProtoTransformedWaveletDelta;
use super::federation::protocol_document_operation::Component;
use super::federation::protocol_document_operation::component::{
    AnnotationBoundary, ElementStart, KeyValuePair, KeyValueUpdate, ReplaceAttributes,
    UpdateAttributes,
};
use super::federation::protocol_wavelet_operation::MutateDocument;
use super::federation::{
    ProtocolAppliedWaveletDelta, ProtocolDocumentOperation, ProtocolHashedVersion,
    ProtocolSignature, ProtocolSignedDelta, ProtocolWaveletDelta, ProtocolWaveletOperation,
};
use super::waveclient_rpc::{DocumentSnapshot, WaveletSnapshot};
use super::*;
use crate::data::{BlipData, WaveletData};
use crate::docop::testing::{Parameters, RandomProviderImpl, generate_random_op};
use crate::docop::{
    AnnotationBoundaryMap, Attributes, AttributesUpdate, BootstrapDocument, DocOp, DocOpBuilder,
    DocOpComponent,
};
use crate::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use crate::operation::{
    BlipOperation, NO_TIMESTAMP, TransformedWaveletDelta, WaveletDelta, WaveletOperation,
    WaveletOperationContext, WaveletOperationKind,
};
use crate::version::{HashedVersion, HashedVersionFactory};

fn pid(address: &str) -> ParticipantId {
    ParticipantId::of(address).unwrap()
}

/// A context for ops that don't have one (`OP_CONTEXT`).
fn op_context() -> WaveletOperationContext {
    WaveletOperationContext::new(ParticipantId::new_unchecked("test@example.com"), 0, 1)
}

fn attrs(pairs: &[(&str, &str)]) -> Attributes {
    Attributes::from_pairs(pairs.iter().copied()).unwrap()
}

fn hashed_version(version: i64, hash: &[u8]) -> ProtocolHashedVersion {
    ProtocolHashedVersion {
        version: Some(version),
        history_hash: Some(hash.to_vec()),
    }
}

fn kvp(key: &str, value: &str) -> KeyValuePair {
    KeyValuePair {
        key: Some(key.to_owned()),
        value: Some(value.to_owned()),
    }
}

fn kvu(key: &str, old_value: Option<&str>, new_value: Option<&str>) -> KeyValueUpdate {
    KeyValueUpdate {
        key: Some(key.to_owned()),
        old_value: old_value.map(str::to_owned),
        new_value: new_value.map(str::to_owned),
    }
}

fn element(tag: &str, attributes: Vec<KeyValuePair>) -> ElementStart {
    ElementStart {
        r#type: Some(tag.to_owned()),
        attribute: attributes,
    }
}

fn characters(text: &str) -> Component {
    Component {
        characters: Some(text.to_owned()),
        ..Default::default()
    }
}

fn retain(item_count: i32) -> Component {
    Component {
        retain_item_count: Some(item_count),
        ..Default::default()
    }
}

fn element_start(tag: &str, attributes: Vec<KeyValuePair>) -> Component {
    Component {
        element_start: Some(element(tag, attributes)),
        ..Default::default()
    }
}

fn element_end() -> Component {
    Component {
        element_end: Some(true),
        ..Default::default()
    }
}

fn annotation_boundary(boundary: AnnotationBoundary) -> Component {
    Component {
        annotation_boundary: Some(boundary),
        ..Default::default()
    }
}

fn update_attributes(updates: Vec<KeyValueUpdate>) -> Component {
    Component {
        update_attributes: Some(UpdateAttributes {
            empty: None,
            attribute_update: updates,
        }),
        ..Default::default()
    }
}

fn doc_op_message(components: Vec<Component>) -> ProtocolDocumentOperation {
    ProtocolDocumentOperation {
        component: components,
    }
}

fn no_op_message() -> ProtocolWaveletOperation {
    ProtocolWaveletOperation {
        no_op: Some(true),
        ..Default::default()
    }
}

fn add_participant_message(address: &str) -> ProtocolWaveletOperation {
    ProtocolWaveletOperation {
        add_participant: Some(address.to_owned()),
        ..Default::default()
    }
}

fn is_missing<T: Debug>(result: Result<T, ProtoError>, field: &str) -> bool {
    matches!(&result, Err(ProtoError::MissingField(name)) if name.ends_with(field))
}

fn is_invalid<T: Debug>(result: Result<T, ProtoError>, field: &str) -> bool {
    matches!(&result, Err(ProtoError::InvalidValue { field: name, .. }) if name.ends_with(field))
}

// WaveletOperationSerializerTest.

mod wavelet_operation_serializer_test {
    use super::*;

    fn assert_deep_equals(a: &WaveletDelta, b: &WaveletDelta) {
        assert_eq!(a.author(), b.author());
        assert_eq!(a.len(), b.len());
        for (a_op, b_op) in a.iter().zip(b.iter()) {
            assert_eq!(a_op, b_op);
        }
    }

    /// Asserts that an operation is unchanged when serialised then
    /// deserialised, alone and in a delta.
    fn assert_reversible(op: WaveletOperation) {
        assert_eq!(
            deserialize_op(&serialize_op(&op).unwrap(), op_context()).unwrap(),
            op
        );

        let ops = vec![op.clone(), op.clone(), op];
        let author = pid("kalman@google.com");
        let hashed_version = HashedVersion::unsigned(0);
        let delta = WaveletDelta::new(author, hashed_version.clone(), ops);
        let serialized = serialize_delta(&delta).unwrap();
        let deserialized = deserialize_delta(&serialized).unwrap();
        let serialized_version = serialized.hashed_version.as_ref().unwrap();
        assert_eq!(
            serialized_version.version,
            Some(hashed_version.version() as i64)
        );
        assert_eq!(
            serialized_version.history_hash.as_deref(),
            Some(hashed_version.history_hash())
        );
        assert_deep_equals(&delta, &deserialized);

        // Not in the original: the same holds through the wire bytes.
        let decoded: ProtocolWaveletDelta = decode(&encode(&serialized).unwrap()).unwrap();
        assert_eq!(decoded, serialized);
        assert_eq!(deserialize_delta(&decoded).unwrap(), delta);
    }

    fn make_blip_op(blip_id: &str, mutation: DocOp) -> WaveletOperation {
        WaveletOperation::blip_content(op_context(), blip_id, mutation)
    }

    fn a() -> Attributes {
        attrs(&[("a1", "1"), ("a2", "2")])
    }

    fn b() -> Attributes {
        Attributes::new()
    }

    fn c() -> Attributes {
        attrs(&[("c1", "1"), ("c2", "2"), ("c3", "3")])
    }

    #[test]
    fn no_op() {
        assert_reversible(WaveletOperation::no_op(op_context()));
    }

    #[test]
    fn add_participant() {
        assert_reversible(WaveletOperation::add_participant(
            op_context(),
            pid("kalman@google.com"),
        ));
    }

    #[test]
    fn remove_participant() {
        assert_reversible(WaveletOperation::remove_participant(
            op_context(),
            pid("kalman@google.com"),
        ));
    }

    #[test]
    fn empty_document_mutation() {
        assert_reversible(make_blip_op("empty", DocOpBuilder::new().build().unwrap()));
    }

    #[test]
    fn single_characters() {
        let m = DocOpBuilder::new().characters("hello");
        assert_reversible(make_blip_op("single", m.build().unwrap()));
    }

    #[test]
    fn many_characters() {
        let m = DocOpBuilder::new()
            .characters("hello")
            .characters("world")
            .characters("foo")
            .characters("bar");
        assert_reversible(make_blip_op("many", m.build().unwrap()));
    }

    #[test]
    fn retain() {
        let m = DocOpBuilder::new()
            .characters("hello")
            .retain(5)
            .characters("world")
            .retain(10)
            .characters("foo")
            .retain(13)
            .characters("bar")
            .retain(16);
        assert_reversible(make_blip_op("retain", m.build().unwrap()));
    }

    #[test]
    fn delete_characters() {
        let m = DocOpBuilder::new()
            .characters("hello")
            .retain(1)
            .delete_characters("ab")
            .characters("world")
            .retain(2)
            .delete_characters("cd");
        assert_reversible(make_blip_op("deleteCharacters", m.build().unwrap()));
    }

    #[test]
    fn elements() {
        let m = DocOpBuilder::new()
            .element_start("a", a())
            .element_start("b", b())
            .element_start("c", c())
            .element_end()
            .element_end()
            .element_end();
        assert_reversible(make_blip_op("elements", m.build().unwrap()));
    }

    #[test]
    fn characters_and_elements() {
        let m = DocOpBuilder::new()
            .element_start("a", a())
            .characters("hello")
            .element_start("b", b())
            .characters("world")
            .element_start("c", c())
            .element_end()
            .characters("blah")
            .element_end()
            .element_end();
        assert_reversible(make_blip_op("charactersAndElements", m.build().unwrap()));
    }

    #[test]
    fn delete_elements() {
        let m = DocOpBuilder::new()
            .delete_element_start("a", a())
            .delete_element_start("b", b())
            .delete_element_start("c", c())
            .delete_element_end()
            .delete_element_end()
            .delete_element_end();
        assert_reversible(make_blip_op("deleteElements", m.build().unwrap()));
    }

    #[test]
    fn delete_characters_and_elements() {
        let m = DocOpBuilder::new()
            .delete_element_start("a", a())
            .delete_characters("hello")
            .delete_element_start("b", b())
            .delete_characters("world")
            .delete_element_start("c", c())
            .delete_element_end()
            .delete_characters("blah")
            .delete_element_end()
            .delete_element_end();
        assert_reversible(make_blip_op(
            "deleteCharactersAndElements",
            m.build().unwrap(),
        ));
    }

    #[test]
    fn annotation_boundary() {
        let map_a = AnnotationBoundaryMap::changes_only([("a", None, Some("b"))]).unwrap();
        let map_b = AnnotationBoundaryMap::changes_only([("a", Some("b"), None)]).unwrap();
        let map_c = AnnotationBoundaryMap::ends(["a"]).unwrap();
        let m = DocOpBuilder::new()
            .element_start("a", a())
            .annotation_boundary(map_a)
            .characters("test")
            .annotation_boundary(map_b)
            .characters("text")
            .annotation_boundary(map_c)
            .element_end();
        assert_reversible(make_blip_op("annotationBoundary", m.build().unwrap()));
    }

    #[test]
    fn empty_annotation_boundary() {
        let m = DocOpBuilder::new()
            .element_start("a", a())
            .annotation_boundary(AnnotationBoundaryMap::empty())
            .characters("text")
            .annotation_boundary(AnnotationBoundaryMap::empty())
            .element_end();
        assert_reversible(make_blip_op("emptyAnnotationBoundary", m.build().unwrap()));
    }

    #[test]
    fn replace_attributes() {
        let old_a = attrs(&[("a1", "1"), ("a2", "2")]);
        let new_a = attrs(&[("a1", "3"), ("a2", "4")]);
        let m = DocOpBuilder::new()
            .retain(4)
            .replace_attributes(old_a, new_a)
            .retain(4);
        assert_reversible(make_blip_op("replaceAttributes", m.build().unwrap()));
    }

    #[test]
    fn empty_replace_attributes() {
        let m = DocOpBuilder::new()
            .retain(4)
            .replace_attributes(Attributes::new(), Attributes::new())
            .retain(4);
        assert_reversible(make_blip_op("emptyReplaceAttributes", m.build().unwrap()));
    }

    #[test]
    fn update_attributes() {
        let u = AttributesUpdate::from_triples([("a", None, Some("2")), ("b", Some("1"), None)])
            .unwrap();
        let m = DocOpBuilder::new().retain(4).update_attributes(u).retain(4);
        assert_reversible(make_blip_op("updateAttributes", m.build().unwrap()));
    }

    #[test]
    fn empty_update_attributes() {
        let m = DocOpBuilder::new()
            .retain(4)
            .update_attributes(AttributesUpdate::new())
            .retain(4);
        assert_reversible(make_blip_op("emptyUpdateAttributes", m.build().unwrap()));
    }
}

// SnapshotSerializerTest.

mod snapshot_serializer_test {
    use super::*;

    /// `TestDataUtil.createSimpleWaveletData`.
    fn create_simple_wavelet_data() -> WaveletData {
        let name = WaveletName::of(
            WaveId::of("example.com", "w+abc123").unwrap(),
            WaveletId::of("example.com", "conv+root").unwrap(),
        );
        let creator = ParticipantId::new_unchecked("sam@example.com");
        let time = 1234567890;

        let mut wavelet =
            WaveletData::empty(&name, creator.clone(), HashedVersion::unsigned(0), time);

        let content = DocOpBuilder::new()
            .characters("Hello there")
            .build_initialization()
            .unwrap();
        wavelet
            .create_document("b+abc123", creator, [], &content, time, 0)
            .unwrap();
        wavelet
    }

    /// `TestDataUtil.checkSerializedDocument`.
    fn check_serialized_document(expected: &BlipData, actual: &BlipData) {
        assert_eq!(expected.id(), actual.id());
        assert_eq!(
            expected.content().to_xml_string(),
            actual.content().to_xml_string()
        );
        assert_eq!(expected.author(), actual.author());
        assert_eq!(expected.contributors(), actual.contributors());
        assert_eq!(expected.last_modified_time(), actual.last_modified_time());
        assert_eq!(
            expected.last_modified_version(),
            actual.last_modified_version()
        );
    }

    /// `TestDataUtil.checkSerializedWavelet`.
    fn check_serialized_wavelet(expected: &WaveletData, actual: &WaveletData) {
        assert_eq!(expected.wave_id(), actual.wave_id());
        assert_eq!(expected.participants(), actual.participants());
        assert_eq!(expected.version(), actual.version());
        assert_eq!(expected.last_modified_time(), actual.last_modified_time());
        assert_eq!(expected.creator(), actual.creator());
        assert_eq!(expected.creation_time(), actual.creation_time());

        // The documents the wavelets contain are also the same.
        assert_eq!(
            expected.document_ids().collect::<Vec<_>>(),
            actual.document_ids().collect::<Vec<_>>()
        );
        for doc_id in expected.document_ids() {
            check_serialized_document(
                expected.document(doc_id).unwrap(),
                actual.document(doc_id).unwrap(),
            );
        }
    }

    #[test]
    fn wavelet_roundtrip() {
        let expected = create_simple_wavelet_data();
        let name = WaveletName::of(expected.wave_id().clone(), expected.wavelet_id().clone());
        let version = HashedVersionFactory::create_version_zero(&name);

        let snapshot = serialize_wavelet(&expected, &version).unwrap();
        let actual = deserialize_wavelet(&snapshot, expected.wave_id()).unwrap();

        check_serialized_wavelet(&expected, &actual);
    }
}

// Hand-computed wire bytes. Field numbers are those of the .proto files;
// a tag is `field_number << 3 | wire_type` (0 varint, 2 length-delimited).

mod wire_bytes {
    use super::*;

    /// Asserts that `message` encodes to exactly `bytes` and that `bytes`
    /// decode to `message`.
    fn assert_wire<M: WaveMessage + PartialEq + Debug>(message: &M, bytes: &[u8]) {
        assert_eq!(
            hex::encode(encode(message).unwrap()),
            hex::encode(bytes),
            "encoding of {message:?}"
        );
        assert_eq!(&decode::<M>(bytes).unwrap(), message);
    }

    #[test]
    fn hashed_version_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x08, 0xAC, 0x02,       // version (1, varint) = 300
            0x12, 0x02, 0xDE, 0xAD, // history_hash (2, bytes), length 2
        ];
        let message = hashed_version(300, &[0xDE, 0xAD]);
        assert_wire(&message, &bytes);
        assert_eq!(
            serialize_hashed_version(&HashedVersion::of(300, vec![0xDE, 0xAD])).unwrap(),
            message
        );
    }

    #[test]
    fn hashed_version_zero_and_empty_hash_bytes() {
        // Required fields are written even when they hold default values.
        #[rustfmt::skip]
        let bytes = [
            0x08, 0x00, // version (1, varint) = 0
            0x12, 0x00, // history_hash (2, bytes), empty
        ];
        assert_wire(&hashed_version(0, &[]), &bytes);
        assert_eq!(
            encode(&serialize_hashed_version(&HashedVersion::unsigned(0)).unwrap()).unwrap(),
            bytes
        );
    }

    #[test]
    fn hashed_version_large_version_bytes() {
        #[rustfmt::skip]
        let bytes = [
            // version (1, varint) = 2^63 - 1: nine bytes
            0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F,
            0x12, 0x01, 0x07, // history_hash (2, bytes) = 07
        ];
        assert_wire(&hashed_version(i64::MAX, &[7]), &bytes);
    }

    #[test]
    fn add_participant_op_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x05, b'a', b'@', b'b', b'.', b'c', // add_participant (1, string)
        ];
        let message = add_participant_message("a@b.c");
        assert_wire(&message, &bytes);
        let op = WaveletOperation::add_participant(op_context(), pid("a@b.c"));
        assert_eq!(serialize_op(&op).unwrap(), message);
    }

    #[test]
    fn remove_participant_op_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x12, 0x05, b'a', b'@', b'b', b'.', b'c', // remove_participant (2, string)
        ];
        let message = ProtocolWaveletOperation {
            remove_participant: Some("a@b.c".to_owned()),
            ..Default::default()
        };
        assert_wire(&message, &bytes);
        let op = WaveletOperation::remove_participant(op_context(), pid("a@b.c"));
        assert_eq!(serialize_op(&op).unwrap(), message);
    }

    #[test]
    fn mutate_document_op_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x1A, 0x0B,             // mutate_document (3, message), length 11
            0x0A, 0x01, b'b',       //   document_id (1, string) = "b"
            0x12, 0x06,             //   document_operation (2, message), length 6
            0x0A, 0x04,             //     component (1, message), length 4
            0x12, 0x02, b'h', b'i', //       characters (2, string) = "hi"
        ];
        let message = ProtocolWaveletOperation {
            mutate_document: Some(MutateDocument {
                document_id: Some("b".to_owned()),
                document_operation: Some(doc_op_message(vec![characters("hi")])),
            }),
            ..Default::default()
        };
        assert_wire(&message, &bytes);
        let op = WaveletOperation::blip_content(
            op_context(),
            "b",
            DocOpBuilder::new().characters("hi").build().unwrap(),
        );
        assert_eq!(serialize_op(&op).unwrap(), message);
    }

    #[test]
    fn no_op_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x20, 0x01, // no_op (4, varint) = true
        ];
        let message = no_op_message();
        assert_wire(&message, &bytes);
        assert_eq!(
            serialize_op(&WaveletOperation::no_op(op_context())).unwrap(),
            message
        );
    }

    #[test]
    fn document_operation_bytes() {
        // A well-formed operation with one component of each kind.
        #[rustfmt::skip]
        let bytes = [
            // Annotation boundary starting k: (absent) -> "n".
            0x0A, 0x0A,       // component (1, message), length 10
            0x0A, 0x08,       //   annotation_boundary (1, message), length 8
            0x1A, 0x06,       //     change (3, message), length 6
            0x0A, 0x01, b'k', //       key (1, string) = "k"
            0x1A, 0x01, b'n', //       new_value (3, string) = "n"; old_value absent
            // Characters.
            0x0A, 0x04,             // component, length 4
            0x12, 0x02, b'a', b'b', //   characters (2, string) = "ab"
            // Element start.
            0x0A, 0x0D,       // component, length 13
            0x1A, 0x0B,       //   element_start (3, message), length 11
            0x0A, 0x01, b'p', //     type (1, string) = "p"
            0x12, 0x06,       //     attribute (2, message), length 6
            0x0A, 0x01, b'k', //       key (1, string) = "k"
            0x12, 0x01, b'v', //       value (2, string) = "v"
            // Element end.
            0x0A, 0x02, // component, length 2
            0x20, 0x01, //   element_end (4, varint) = true
            // Retain.
            0x0A, 0x03,       // component, length 3
            0x28, 0xAC, 0x02, //   retain_item_count (5, varint) = 300
            // Delete characters.
            0x0A, 0x03,       // component, length 3
            0x32, 0x01, b'x', //   delete_characters (6, string) = "x"
            // Delete element start.
            0x0A, 0x05,       // component, length 5
            0x3A, 0x03,       //   delete_element_start (7, message), length 3
            0x0A, 0x01, b'q', //     type (1, string) = "q"; no attributes
            // Delete element end.
            0x0A, 0x02, // component, length 2
            0x40, 0x01, //   delete_element_end (8, varint) = true
            // Replace attributes.
            0x0A, 0x12,       // component, length 18
            0x4A, 0x10,       //   replace_attributes (9, message), length 16
            0x12, 0x06,       //     old_attribute (2, message), length 6
            0x0A, 0x01, b'a', //       key (1, string) = "a"
            0x12, 0x01, b'1', //       value (2, string) = "1"
            0x1A, 0x06,       //     new_attribute (3, message), length 6
            0x0A, 0x01, b'a', //       key (1, string) = "a"
            0x12, 0x01, b'2', //       value (2, string) = "2"
            // Update attributes.
            0x0A, 0x0A,       // component, length 10
            0x52, 0x08,       //   update_attributes (10, message), length 8
            0x12, 0x06,       //     attribute_update (2, message), length 6
            0x0A, 0x01, b'u', //       key (1, string) = "u"
            0x12, 0x01, b'o', //       old_value (2, string) = "o"; new_value absent
            // Annotation boundary ending k.
            0x0A, 0x05,       // component, length 5
            0x0A, 0x03,       //   annotation_boundary (1, message), length 3
            0x12, 0x01, b'k', //     end (2, string) = "k"
        ];
        let op = DocOpBuilder::new()
            .annotation_boundary(
                AnnotationBoundaryMap::changes_only([("k", None, Some("n"))]).unwrap(),
            )
            .characters("ab")
            .element_start("p", attrs(&[("k", "v")]))
            .element_end()
            .retain(300)
            .delete_characters("x")
            .delete_element_start("q", Attributes::new())
            .delete_element_end()
            .replace_attributes(attrs(&[("a", "1")]), attrs(&[("a", "2")]))
            .update_attributes(AttributesUpdate::single("u", Some("o"), None))
            .annotation_boundary(AnnotationBoundaryMap::ends(["k"]).unwrap())
            .build()
            .unwrap();
        let message = serialize_doc_op(&op).unwrap();
        assert_wire(&message, &bytes);
        assert_eq!(deserialize_doc_op(&decode(&bytes).unwrap()).unwrap(), op);
    }

    #[test]
    fn document_operation_empty_flags_and_optional_values_bytes() {
        #[rustfmt::skip]
        let bytes = [
            // Empty annotation boundary.
            0x0A, 0x04, // component (1, message), length 4
            0x0A, 0x02, //   annotation_boundary (1, message), length 2
            0x08, 0x01, //     empty (1, varint) = true
            // Empty replace attributes.
            0x0A, 0x04, // component, length 4
            0x4A, 0x02, //   replace_attributes (9, message), length 2
            0x08, 0x01, //     empty (1, varint) = true
            // Empty update attributes.
            0x0A, 0x04, // component, length 4
            0x52, 0x02, //   update_attributes (10, message), length 2
            0x08, 0x01, //     empty (1, varint) = true
            // Annotation boundary with an end and a change without values.
            0x0A, 0x0A,       // component, length 10
            0x0A, 0x08,       //   annotation_boundary (1, message), length 8
            0x12, 0x01, b'e', //     end (2, string) = "e"
            0x1A, 0x03,       //     change (3, message), length 3
            0x0A, 0x01, b'k', //       key (1, string) = "k"; no old or new value
            // Update with both an old and a new value.
            0x0A, 0x0D,       // component, length 13
            0x52, 0x0B,       //   update_attributes (10, message), length 11
            0x12, 0x09,       //     attribute_update (2, message), length 9
            0x0A, 0x01, b'u', //       key (1, string) = "u"
            0x12, 0x01, b'o', //       old_value (2, string) = "o"
            0x1A, 0x01, b'n', //       new_value (3, string) = "n"
            // Update with empty strings, which differ from absent values.
            0x0A, 0x0B,       // component, length 11
            0x52, 0x09,       //   update_attributes (10, message), length 9
            0x12, 0x07,       //     attribute_update (2, message), length 7
            0x0A, 0x01, b'u', //       key (1, string) = "u"
            0x12, 0x00,       //       old_value (2, string) = ""
            0x1A, 0x00,       //       new_value (3, string) = ""
        ];
        let message = doc_op_message(vec![
            annotation_boundary(AnnotationBoundary {
                empty: Some(true),
                ..Default::default()
            }),
            Component {
                replace_attributes: Some(ReplaceAttributes {
                    empty: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Component {
                update_attributes: Some(UpdateAttributes {
                    empty: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            annotation_boundary(AnnotationBoundary {
                empty: None,
                end: vec!["e".to_owned()],
                change: vec![kvu("k", None, None)],
            }),
            update_attributes(vec![kvu("u", Some("o"), Some("n"))]),
            update_attributes(vec![kvu("u", Some(""), Some(""))]),
        ]);
        assert_wire(&message, &bytes);

        // The converters produce the empty flags.
        assert_eq!(
            serialize_doc_op(&DocOp::new_unchecked(vec![
                DocOpComponent::AnnotationBoundary(AnnotationBoundaryMap::empty()),
                DocOpComponent::ReplaceAttributes {
                    old_attributes: Attributes::new(),
                    new_attributes: Attributes::new(),
                },
                DocOpComponent::UpdateAttributes(AttributesUpdate::new()),
            ]))
            .unwrap()
            .component,
            message.component[..3]
        );
    }

    #[test]
    fn wavelet_delta_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x06,                               // hashed_version (1, message), length 6
            0x08, 0x07,                               //   version (1, varint) = 7
            0x12, 0x02, 0x01, 0x02,                   //   history_hash (2, bytes) = 01 02
            0x12, 0x05, b'a', b'@', b'b', b'.', b'c', // author (2, string)
            0x1A, 0x02,                               // operation (3, message), length 2
            0x20, 0x01,                               //   no_op (4, varint) = true
            0x1A, 0x07,                               // operation (3, message), length 7
            0x0A, 0x05, b'x', b'@', b'y', b'.', b'z', //   add_participant (1, string)
        ];
        let mut message = ProtocolWaveletDelta {
            hashed_version: Some(hashed_version(7, &[1, 2])),
            author: Some("a@b.c".to_owned()),
            operation: vec![no_op_message(), add_participant_message("x@y.z")],
            address_path: vec![],
        };
        assert_wire(&message, &bytes);

        let author = pid("a@b.c");
        let context = WaveletOperationContext::new(author.clone(), NO_TIMESTAMP, 1);
        let delta = WaveletDelta::new(
            author,
            HashedVersion::of(7, vec![1, 2]),
            vec![
                WaveletOperation::no_op(context.clone()),
                WaveletOperation::add_participant(context, pid("x@y.z")),
            ],
        );
        assert_eq!(serialize_delta(&delta).unwrap(), message);
        assert_eq!(deserialize_delta(&decode(&bytes).unwrap()).unwrap(), delta);

        // The address path follows the operations.
        let mut with_path = bytes.to_vec();
        #[rustfmt::skip]
        with_path.extend([
            0x22, 0x05, b'p', b'@', b'q', b'.', b'r', // address_path (4, string)
        ]);
        message.address_path.push("p@q.r".to_owned());
        assert_wire(&message, &with_path);
    }

    #[test]
    fn stored_delta_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x05, b'a', b'@', b'b', b'.', b'c', // author (1, string)
            0x12, 0x05,                               // resulting_version (2, message), length 5
            0x08, 0x01,                               //   version (1, varint) = 1
            0x12, 0x01, 0x09,                         //   history_hash (2, bytes) = 09
            0x18, 0xE8, 0x07,                         // application_timestamp (3, varint) = 1000
            0x22, 0x02,                               // operation (4, message), length 2
            0x20, 0x01,                               //   no_op (4, varint) = true
        ];
        let message = ProtoTransformedWaveletDelta {
            author: Some("a@b.c".to_owned()),
            resulting_version: Some(hashed_version(1, &[9])),
            application_timestamp: Some(1000),
            operation: vec![no_op_message()],
        };
        assert_wire(&message, &bytes);

        let author = pid("a@b.c");
        let delta = TransformedWaveletDelta::clone_operations(
            author.clone(),
            HashedVersion::of(1, vec![9]),
            1000,
            &[WaveletOperation::no_op(op_context())],
        )
        .unwrap();
        assert_eq!(serialize_stored_delta(&delta).unwrap(), message);
        assert_eq!(
            deserialize_stored_delta(&decode(&bytes).unwrap()).unwrap(),
            delta
        );
    }

    #[test]
    fn applied_delta_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x0E,             // signed_original_delta (1, message), length 14
            0x0A, 0x02, 0xAA, 0xBB, //   delta (1, bytes) = AA BB
            0x12, 0x08,             //   signature (2, message), length 8
            0x0A, 0x01, 0x01,       //     signature_bytes (1, bytes) = 01
            0x12, 0x01, 0x02,       //     signer_id (2, bytes) = 02
            0x18, 0x01,             //     signature_algorithm (3, varint) = SHA1_RSA
            0x12, 0x04,             // hashed_version_applied_at (2, message), length 4
            0x08, 0x02,             //   version (1, varint) = 2
            0x12, 0x00,             //   history_hash (2, bytes), empty
            0x18, 0x01,             // operations_applied (3, varint) = 1
            0x20, 0x05,             // application_timestamp (4, varint) = 5
        ];
        let signed = ProtocolSignedDelta {
            delta: Some(vec![0xAA, 0xBB]),
            signature: vec![ProtocolSignature {
                signature_bytes: Some(vec![1]),
                signer_id: Some(vec![2]),
                signature_algorithm: Some(1),
            }],
        };
        let message = ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(signed.clone()),
            hashed_version_applied_at: Some(hashed_version(2, &[])),
            operations_applied: Some(1),
            application_timestamp: Some(5),
            epoch: None,
            rebased_operations: None,
        };
        assert_wire(&message, &bytes);

        let built = build_applied_delta(signed, &HashedVersion::unsigned(2), 1, 5).unwrap();
        assert_eq!(built.bytes(), bytes);
        assert_eq!(built.message(), &message);
    }

    #[test]
    fn wavelet_snapshot_bytes() {
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x05, b'b', b'.', b'c', b'/', b'r', // wavelet_id (1, string) = "b.c/r"
            0x12, 0x05, b'a', b'@', b'b', b'.', b'c', // participant_id (2, string)
            0x1A, 0x1D,                               // document (3, message), length 29
            0x0A, 0x01, b'b',                         //   document_id (1, string) = "b"
            0x12, 0x06,                               //   document_operation (2, message)
            0x0A, 0x04,                               //     component (1, message), length 4
            0x12, 0x02, b'h', b'i',                   //       characters (2, string) = "hi"
            0x1A, 0x05, b'a', b'@', b'b', b'.', b'c', //   author (3, string)
            0x22, 0x05, b'x', b'@', b'y', b'.', b'z', //   contributor (4, string)
            0x28, 0x03,                               //   last_modified_version (5, varint) = 3
            0x30, 0x04,                               //   last_modified_time (6, varint) = 4
            0x22, 0x05,                               // version (4, message), length 5
            0x08, 0x03,                               //   version (1, varint) = 3
            0x12, 0x01, 0x07,                         //   history_hash (2, bytes) = 07
            0x28, 0x04,                               // last_modified_time (5, varint) = 4
            0x32, 0x05, b'a', b'@', b'b', b'.', b'c', // creator (6, string)
            0x38, 0x02,                               // creation_time (7, varint) = 2
        ];
        let message = WaveletSnapshot {
            wavelet_id: Some("b.c/r".to_owned()),
            participant_id: vec!["a@b.c".to_owned()],
            document: vec![DocumentSnapshot {
                document_id: Some("b".to_owned()),
                document_operation: Some(doc_op_message(vec![characters("hi")])),
                author: Some("a@b.c".to_owned()),
                contributor: vec!["x@y.z".to_owned()],
                last_modified_version: Some(3),
                last_modified_time: Some(4),
            }],
            version: Some(hashed_version(3, &[7])),
            last_modified_time: Some(4),
            creator: Some("a@b.c".to_owned()),
            creation_time: Some(2),
        };
        assert_wire(&message, &bytes);

        let wave_id = WaveId::of("b.c", "w").unwrap();
        let version = HashedVersion::of(3, vec![7]);
        let wavelet = deserialize_wavelet(&message, &wave_id).unwrap();
        assert_eq!(wavelet.wave_id(), &wave_id);
        assert_eq!(wavelet.wavelet_id(), &WaveletId::of("b.c", "r").unwrap());
        assert_eq!(wavelet.creator(), &pid("a@b.c"));
        assert_eq!(wavelet.creation_time(), 2);
        assert_eq!(wavelet.last_modified_time(), 4);
        assert_eq!(wavelet.version(), 3);
        assert_eq!(wavelet.hashed_version(), &version);
        assert_eq!(wavelet.participants(), [pid("a@b.c")]);
        let document = wavelet.document("b").unwrap();
        assert_eq!(document.author(), &pid("a@b.c"));
        assert_eq!(document.contributors(), [pid("x@y.z")]);
        assert_eq!(document.last_modified_version(), 3);
        assert_eq!(document.last_modified_time(), 4);
        assert_eq!(document.content().to_xml_string(), "hi");
        assert_eq!(serialize_wavelet(&wavelet, &version).unwrap(), message);
    }
}

// Decoder and converter validation.

mod validation {
    use super::*;

    #[test]
    fn decode_rejects_truncated_bytes() {
        // version = 300, history_hash = DE AD.
        let bytes = [0x08, 0xAC, 0x02, 0x12, 0x02, 0xDE, 0xAD];
        assert!(decode::<ProtocolHashedVersion>(&bytes).is_ok());
        for len in 0..bytes.len() {
            let result = decode::<ProtocolHashedVersion>(&bytes[..len]);
            assert!(result.is_err(), "prefix of length {len}: {result:?}");
        }
        // Cut inside a varint, after a tag and inside a length-delimited
        // value.
        for len in [1, 2, 4, 5, 6] {
            assert!(
                matches!(
                    decode::<ProtocolHashedVersion>(&bytes[..len]),
                    Err(ProtoError::Decode(_))
                ),
                "prefix of length {len}"
            );
        }
        // Cut between fields: well-formed, but a required field is missing.
        assert!(is_missing(
            decode::<ProtocolHashedVersion>(&bytes[..3]),
            "history_hash"
        ));
        assert!(is_missing(decode::<ProtocolHashedVersion>(&[]), "version"));
    }

    #[test]
    fn decode_rejects_truncated_nested_message() {
        let delta = ProtocolWaveletDelta {
            hashed_version: Some(hashed_version(7, &[1, 2])),
            author: Some("a@b.c".to_owned()),
            operation: vec![add_participant_message("x@y.z")],
            address_path: vec![],
        };
        let bytes = encode(&delta).unwrap();
        assert_eq!(decode::<ProtocolWaveletDelta>(&bytes).unwrap(), delta);
        // Cut inside the last operation.
        for cut in 1..8 {
            assert!(
                matches!(
                    decode::<ProtocolWaveletDelta>(&bytes[..bytes.len() - cut]),
                    Err(ProtoError::Decode(_))
                ),
                "cut {cut}"
            );
        }
    }

    #[test]
    fn decode_rejects_malformed_bytes() {
        // version (1) with wire type 2 instead of varint.
        assert!(matches!(
            decode::<ProtocolHashedVersion>(&[0x0A, 0x01, 0x00, 0x12, 0x00]),
            Err(ProtoError::Decode(_))
        ));
        // A varint of more than ten bytes.
        let mut bytes = vec![0x08];
        bytes.extend([0xFF; 10]);
        bytes.extend([0x01, 0x12, 0x00]);
        assert!(matches!(
            decode::<ProtocolHashedVersion>(&bytes),
            Err(ProtoError::Decode(_))
        ));
        // Field number zero.
        assert!(matches!(
            decode::<ProtocolHashedVersion>(&[0x00, 0x00]),
            Err(ProtoError::Decode(_))
        ));
        // add_participant (1, string) that is not UTF-8.
        assert!(matches!(
            decode::<ProtocolWaveletOperation>(&[0x0A, 0x02, 0xC3, 0x28]),
            Err(ProtoError::Decode(_))
        ));
    }

    #[test]
    fn decode_rejects_missing_required_field_at_top_level() {
        // A delta with a hashed version (version 0, empty hash) but no
        // author.
        assert!(is_missing(
            decode::<ProtocolWaveletDelta>(&[0x0A, 0x04, 0x08, 0x00, 0x12, 0x00]),
            "ProtocolWaveletDelta.author"
        ));
        // A delta with an author but no hashed version.
        assert!(is_missing(
            decode::<ProtocolWaveletDelta>(&[0x12, 0x01, b'a']),
            "ProtocolWaveletDelta.hashed_version"
        ));
        // A hashed version without history_hash, and one without version.
        assert!(is_missing(
            decode::<ProtocolHashedVersion>(&[0x08, 0x01]),
            "ProtocolHashedVersion.history_hash"
        ));
        assert!(is_missing(
            decode::<ProtocolHashedVersion>(&[0x12, 0x00]),
            "ProtocolHashedVersion.version"
        ));
        // A stored delta without application_timestamp.
        assert!(is_missing(
            decode::<ProtoTransformedWaveletDelta>(&[
                0x0A, 0x01, b'a', // author
                0x12, 0x04, 0x08, 0x01, 0x12, 0x00, // resulting_version
            ]),
            "application_timestamp"
        ));
        // An applied delta without operations_applied.
        assert!(is_missing(
            decode::<ProtocolAppliedWaveletDelta>(&[
                0x0A, 0x02, 0x0A, 0x00, // signed_original_delta { delta: "" }
                0x20, 0x05, // application_timestamp
            ]),
            "operations_applied"
        ));
    }

    #[test]
    fn decode_rejects_missing_required_field_in_nested_message() {
        // A delta whose hashed version lacks history_hash.
        assert!(is_missing(
            decode::<ProtocolWaveletDelta>(&[
                0x0A, 0x02, 0x08, 0x01, // hashed_version { version: 1 }
                0x12, 0x01, b'a', // author
            ]),
            "ProtocolHashedVersion.history_hash"
        ));
        // MutateDocument without document_operation.
        assert!(is_missing(
            decode::<ProtocolWaveletOperation>(&[
                0x1A, 0x03, // mutate_document
                0x0A, 0x01, b'b', // document_id
            ]),
            "MutateDocument.document_operation"
        ));
        // MutateDocument without document_id.
        assert!(is_missing(
            decode::<ProtocolWaveletOperation>(&[
                0x1A, 0x02, // mutate_document
                0x12, 0x00, // document_operation, empty
            ]),
            "MutateDocument.document_id"
        ));
        // The same inside an operation of a delta.
        assert!(is_missing(
            decode::<ProtocolWaveletDelta>(&[
                0x0A, 0x04, 0x08, 0x00, 0x12, 0x00, // hashed_version
                0x12, 0x01, b'a', // author
                0x1A, 0x05, // operation
                0x1A, 0x03, // mutate_document
                0x0A, 0x01, b'b', // document_id
            ]),
            "MutateDocument.document_operation"
        ));
        // KeyValuePair without value, in an element start.
        assert!(is_missing(
            decode::<ProtocolDocumentOperation>(&[
                0x0A, 0x0A, // component
                0x1A, 0x08, // element_start
                0x0A, 0x01, b'p', // type
                0x12, 0x03, // attribute
                0x0A, 0x01, b'k', // key
            ]),
            "KeyValuePair.value"
        ));
        // KeyValuePair without key, in the new attributes of a replace.
        assert!(is_missing(
            decode::<ProtocolDocumentOperation>(&[
                0x0A, 0x07, // component
                0x4A, 0x05, // replace_attributes
                0x1A, 0x03, // new_attribute
                0x12, 0x01, b'v', // value
            ]),
            "KeyValuePair.key"
        ));
        // ElementStart without type, in a delete element start.
        assert!(is_missing(
            decode::<ProtocolDocumentOperation>(&[
                0x0A, 0x02, // component
                0x3A, 0x00, // delete_element_start, empty
            ]),
            "type"
        ));
        // KeyValueUpdate without key, in an annotation boundary and in an
        // attributes update.
        assert!(is_missing(
            decode::<ProtocolDocumentOperation>(&[
                0x0A, 0x07, // component
                0x0A, 0x05, // annotation_boundary
                0x1A, 0x03, // change
                0x1A, 0x01, b'n', // new_value
            ]),
            "KeyValueUpdate.key"
        ));
        assert!(is_missing(
            decode::<ProtocolDocumentOperation>(&[
                0x0A, 0x07, // component
                0x52, 0x05, // update_attributes
                0x12, 0x03, // attribute_update
                0x12, 0x01, b'o', // old_value
            ]),
            "KeyValueUpdate.key"
        ));
        // A signature without signature_algorithm in an applied delta.
        assert!(is_missing(
            decode::<ProtocolAppliedWaveletDelta>(&[
                0x0A, 0x0A, // signed_original_delta
                0x0A, 0x00, // delta
                0x12, 0x06, // signature
                0x0A, 0x01, 0x01, // signature_bytes
                0x12, 0x01, 0x02, // signer_id
                0x18, 0x01, // operations_applied
                0x20, 0x05, // application_timestamp
            ]),
            "signature_algorithm"
        ));
        // A document without author in a wavelet snapshot.
        let mut snapshot = WaveletSnapshot {
            wavelet_id: Some("b.c/r".to_owned()),
            participant_id: vec![],
            document: vec![DocumentSnapshot {
                document_id: Some("b".to_owned()),
                document_operation: Some(doc_op_message(vec![])),
                author: None,
                contributor: vec![],
                last_modified_version: Some(0),
                last_modified_time: Some(0),
            }],
            version: Some(hashed_version(0, &[])),
            last_modified_time: Some(0),
            creator: Some("a@b.c".to_owned()),
            creation_time: Some(0),
        };
        let bytes = prost::Message::encode_to_vec(&snapshot);
        assert!(is_missing(
            decode::<WaveletSnapshot>(&bytes),
            "DocumentSnapshot.author"
        ));
        snapshot.document[0].author = Some("a@b.c".to_owned());
        let bytes = prost::Message::encode_to_vec(&snapshot);
        assert_eq!(decode::<WaveletSnapshot>(&bytes).unwrap(), snapshot);
    }

    #[test]
    fn encode_rejects_missing_required_field() {
        assert!(is_missing(
            encode(&ProtocolHashedVersion {
                version: Some(1),
                history_hash: None,
            }),
            "history_hash"
        ));
        assert!(is_missing(
            encode(&ProtocolWaveletDelta {
                hashed_version: Some(hashed_version(0, &[])),
                author: None,
                operation: vec![],
                address_path: vec![],
            }),
            "author"
        ));
        // Nested in a repeated field.
        assert!(is_missing(
            encode(&ProtocolWaveletDelta {
                hashed_version: Some(hashed_version(0, &[])),
                author: Some("a@b.c".to_owned()),
                operation: vec![ProtocolWaveletOperation {
                    mutate_document: Some(MutateDocument {
                        document_id: Some("b".to_owned()),
                        document_operation: None,
                    }),
                    ..Default::default()
                }],
                address_path: vec![],
            }),
            "document_operation"
        ));
        assert!(is_missing(
            encode(&doc_op_message(vec![element_start(
                "p",
                vec![KeyValuePair {
                    key: Some("k".to_owned()),
                    value: None,
                }],
            )])),
            "KeyValuePair.value"
        ));
    }

    #[test]
    fn decode_skips_unknown_fields() {
        #[rustfmt::skip]
        let bytes = [
            0x08, 0x01,                               // version (1, varint) = 1
            0x78, 0x05,                               // unknown field 15, varint
            0x82, 0x01, 0x02, 0xAA, 0xBB,             // unknown field 16, length-delimited
            0x8D, 0x01, 0x01, 0x02, 0x03, 0x04,       // unknown field 17, fixed32
            0x91, 0x01, 1, 2, 3, 4, 5, 6, 7, 8,       // unknown field 18, fixed64
            0x12, 0x01, 0x09,                         // history_hash (2, bytes) = 09
        ];
        let message: ProtocolHashedVersion = decode(&bytes).unwrap();
        assert_eq!(message, hashed_version(1, &[9]));
        // The unknown fields are dropped on re-encoding.
        assert_eq!(encode(&message).unwrap(), [0x08, 0x01, 0x12, 0x01, 0x09]);
    }

    #[test]
    fn decode_skips_unknown_fields_in_nested_message() {
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x06,             // hashed_version (1, message), length 6
            0x08, 0x00,             //   version = 0
            0x78, 0x05,             //   unknown field 15, varint
            0x12, 0x00,             //   history_hash, empty
            0x12, 0x01, b'a',       // author (2, string) = "a"
            0x2A, 0x02, 0x08, 0x01, // unknown field 5, length-delimited
            0x1A, 0x04,             // operation (3, message), length 4
            0x20, 0x01,             //   no_op = true
            0x48, 0x07,             //   unknown field 9, varint
        ];
        let message: ProtocolWaveletDelta = decode(&bytes).unwrap();
        assert_eq!(
            message,
            ProtocolWaveletDelta {
                hashed_version: Some(hashed_version(0, &[])),
                author: Some("a".to_owned()),
                operation: vec![no_op_message()],
                address_path: vec![],
            }
        );
    }

    #[test]
    fn decode_rejects_truncated_unknown_field() {
        // Unknown field 16 announces two bytes but only one follows.
        assert!(matches!(
            decode::<ProtocolHashedVersion>(&[0x08, 0x01, 0x12, 0x00, 0x82, 0x01, 0x02, 0xAA]),
            Err(ProtoError::Decode(_))
        ));
    }

    #[test]
    fn decode_takes_last_of_repeated_singular_field_and_merges_messages() {
        // version given twice: the last one wins.
        assert_eq!(
            decode::<ProtocolHashedVersion>(&[0x08, 0x01, 0x08, 0x02, 0x12, 0x00]).unwrap(),
            hashed_version(2, &[])
        );
        // hashed_version given in two parts: they are merged.
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x02, 0x08, 0x05, // hashed_version { version: 5 }
            0x12, 0x01, b'a',       // author
            0x0A, 0x02, 0x12, 0x00, // hashed_version { history_hash: "" }
        ];
        assert_eq!(
            decode::<ProtocolWaveletDelta>(&bytes)
                .unwrap()
                .hashed_version,
            Some(hashed_version(5, &[]))
        );
    }

    #[test]
    fn deserialize_doc_op_rejects_negative_retain_count() {
        #[rustfmt::skip]
        let bytes = [
            0x0A, 0x0B, // component (1, message), length 11
            // retain_item_count (5, varint) = -1, sign-extended to 64 bits
            0x28, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
        ];
        let message: ProtocolDocumentOperation = decode(&bytes).unwrap();
        assert_eq!(message, doc_op_message(vec![retain(-1)]));
        assert!(is_invalid(
            deserialize_doc_op(&message),
            "retain_item_count"
        ));
        assert!(is_invalid(
            deserialize_doc_op(&doc_op_message(vec![retain(i32::MIN)])),
            "retain_item_count"
        ));
    }

    #[test]
    fn deserialize_doc_op_rejects_zero_retain_count() {
        assert!(matches!(
            deserialize_doc_op(&doc_op_message(vec![retain(0)])),
            Err(ProtoError::Operation(_))
        ));
    }

    #[test]
    fn serialize_doc_op_rejects_retain_count_above_int32() {
        let op = DocOp::new_unchecked(vec![DocOpComponent::Retain(i32::MAX as usize + 1)]);
        assert!(is_invalid(serialize_doc_op(&op), "retain_item_count"));
        let op = DocOp::new_unchecked(vec![DocOpComponent::Retain(i32::MAX as usize)]);
        assert_eq!(
            serialize_doc_op(&op).unwrap(),
            doc_op_message(vec![retain(i32::MAX)])
        );
    }

    #[test]
    fn deserialize_doc_op_rejects_unclosed_element() {
        let message = doc_op_message(vec![element_start("p", vec![]), characters("a")]);
        assert!(matches!(
            deserialize_doc_op(&message),
            Err(ProtoError::Operation(_))
        ));
        // Closed, it is accepted.
        let message = doc_op_message(vec![
            element_start("p", vec![]),
            characters("a"),
            element_end(),
        ]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new()
                .element_start("p", Attributes::new())
                .characters("a")
                .element_end()
                .build()
                .unwrap()
        );
    }

    #[test]
    fn deserialize_doc_op_rejects_unmatched_element_end() {
        let message = doc_op_message(vec![characters("a"), element_end()]);
        assert!(matches!(
            deserialize_doc_op(&message),
            Err(ProtoError::Operation(_))
        ));
        let message = doc_op_message(vec![Component {
            delete_element_end: Some(true),
            ..Default::default()
        }]);
        assert!(matches!(
            deserialize_doc_op(&message),
            Err(ProtoError::Operation(_))
        ));
    }

    #[test]
    fn deserialize_doc_op_rejects_empty_characters() {
        for message in [
            doc_op_message(vec![characters("")]),
            doc_op_message(vec![Component {
                delete_characters: Some(String::new()),
                ..Default::default()
            }]),
        ] {
            assert!(matches!(
                deserialize_doc_op(&message),
                Err(ProtoError::Operation(_))
            ));
        }
    }

    #[test]
    fn deserialize_doc_op_rejects_unclosed_annotation() {
        let message = doc_op_message(vec![
            annotation_boundary(AnnotationBoundary {
                empty: None,
                end: vec![],
                change: vec![kvu("k", None, Some("n"))],
            }),
            characters("a"),
        ]);
        assert!(matches!(
            deserialize_doc_op(&message),
            Err(ProtoError::Operation(_))
        ));
    }

    #[test]
    fn deserialize_doc_op_rejects_annotation_boundary_with_duplicate_key() {
        let boundaries = [
            // The same key changed twice.
            AnnotationBoundary {
                empty: None,
                end: vec![],
                change: vec![kvu("k", None, Some("a")), kvu("k", None, Some("b"))],
            },
            // The same key ended twice.
            AnnotationBoundary {
                empty: None,
                end: vec!["k".to_owned(), "k".to_owned()],
                change: vec![],
            },
            // The same key ended and changed.
            AnnotationBoundary {
                empty: None,
                end: vec!["k".to_owned()],
                change: vec![kvu("k", None, Some("a"))],
            },
        ];
        for boundary in boundaries {
            let message = doc_op_message(vec![
                annotation_boundary(AnnotationBoundary {
                    empty: None,
                    end: vec![],
                    change: vec![kvu("k", None, Some("x"))],
                }),
                characters("a"),
                annotation_boundary(boundary.clone()),
                characters("b"),
                annotation_boundary(AnnotationBoundary {
                    empty: None,
                    end: vec!["k".to_owned()],
                    change: vec![],
                }),
            ]);
            assert!(
                matches!(deserialize_doc_op(&message), Err(ProtoError::Operation(_))),
                "{boundary:?}"
            );
        }
    }

    #[test]
    fn deserialize_doc_op_rejects_annotation_boundary_with_invalid_key() {
        for key in ["", "a?b", "a@b"] {
            let message = doc_op_message(vec![
                annotation_boundary(AnnotationBoundary {
                    empty: None,
                    end: vec![],
                    change: vec![kvu(key, None, Some("x"))],
                }),
                characters("a"),
                annotation_boundary(AnnotationBoundary {
                    empty: None,
                    end: vec![key.to_owned()],
                    change: vec![],
                }),
            ]);
            assert!(
                matches!(deserialize_doc_op(&message), Err(ProtoError::Operation(_))),
                "key {key:?}"
            );
        }
    }

    #[test]
    fn deserialize_doc_op_rejects_unsorted_annotation_boundary_keys() {
        let boundary = |changes: Vec<KeyValueUpdate>| {
            doc_op_message(vec![
                annotation_boundary(AnnotationBoundary {
                    empty: None,
                    end: vec![],
                    change: changes,
                }),
                characters("a"),
                annotation_boundary(AnnotationBoundary {
                    empty: None,
                    end: vec!["a".to_owned(), "b".to_owned()],
                    change: vec![],
                }),
            ])
        };
        let sorted = boundary(vec![kvu("a", None, Some("1")), kvu("b", None, Some("2"))]);
        assert!(deserialize_doc_op(&sorted).is_ok());
        let unsorted = boundary(vec![kvu("b", None, Some("2")), kvu("a", None, Some("1"))]);
        assert!(matches!(
            deserialize_doc_op(&unsorted),
            Err(ProtoError::Operation(_))
        ));
    }

    #[test]
    fn deserialize_doc_op_rejects_duplicate_attributes_update_keys() {
        let message = doc_op_message(vec![update_attributes(vec![
            kvu("a", None, Some("1")),
            kvu("a", None, Some("2")),
        ])]);
        assert!(matches!(
            deserialize_doc_op(&message),
            Err(ProtoError::Operation(_))
        ));
    }

    #[test]
    fn deserialize_doc_op_sorts_unsorted_attributes_update_keys() {
        // As in the original, where the update is built from triples in any
        // order.
        let message = doc_op_message(vec![update_attributes(vec![
            kvu("b", Some("1"), None),
            kvu("a", None, Some("2")),
        ])]);
        let op = deserialize_doc_op(&message).unwrap();
        assert_eq!(
            op,
            DocOpBuilder::new()
                .update_attributes(
                    AttributesUpdate::from_triples([
                        ("a", None, Some("2")),
                        ("b", Some("1"), None)
                    ])
                    .unwrap()
                )
                .build()
                .unwrap()
        );
        let DocOpComponent::UpdateAttributes(update) = &op.components()[0] else {
            panic!("not an update: {op}");
        };
        let names: Vec<&str> = update.updates().iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn deserialize_doc_op_accepts_duplicate_attribute_keys_with_last_wins() {
        let pairs = vec![kvp("k", "first"), kvp("a", "1"), kvp("k", "last")];
        let expected = attrs(&[("a", "1"), ("k", "last")]);
        let message = doc_op_message(vec![
            element_start("p", pairs.clone()),
            element_end(),
            Component {
                delete_element_start: Some(element("q", pairs.clone())),
                ..Default::default()
            },
            Component {
                delete_element_end: Some(true),
                ..Default::default()
            },
            Component {
                replace_attributes: Some(ReplaceAttributes {
                    empty: None,
                    old_attribute: pairs.clone(),
                    new_attribute: pairs,
                }),
                ..Default::default()
            },
        ]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new()
                .element_start("p", expected.clone())
                .element_end()
                .delete_element_start("q", expected.clone())
                .delete_element_end()
                .replace_attributes(expected.clone(), expected)
                .build()
                .unwrap()
        );
    }

    #[test]
    fn deserialize_doc_op_sorts_attributes() {
        let message = doc_op_message(vec![
            element_start("p", vec![kvp("c", "3"), kvp("a", "1"), kvp("b", "2")]),
            element_end(),
        ]);
        let op = deserialize_doc_op(&message).unwrap();
        let DocOpComponent::ElementStart { attributes, .. } = &op.components()[0] else {
            panic!("not an element start: {op}");
        };
        assert_eq!(
            attributes.iter().collect::<Vec<_>>(),
            [("a", "1"), ("b", "2"), ("c", "3")]
        );
    }

    #[test]
    fn deserialize_doc_op_rejects_messages_missing_required_fields() {
        // Messages built in memory are not checked by `decode`.
        let no_type = doc_op_message(vec![Component {
            element_start: Some(ElementStart {
                r#type: None,
                attribute: vec![],
            }),
            ..Default::default()
        }]);
        assert!(is_missing(deserialize_doc_op(&no_type), "type"));
        let no_value = doc_op_message(vec![
            element_start(
                "p",
                vec![KeyValuePair {
                    key: Some("k".to_owned()),
                    value: None,
                }],
            ),
            element_end(),
        ]);
        assert!(is_missing(
            deserialize_doc_op(&no_value),
            "KeyValuePair.value"
        ));
        let no_key = doc_op_message(vec![update_attributes(vec![KeyValueUpdate {
            key: None,
            old_value: None,
            new_value: Some("n".to_owned()),
        }])]);
        assert!(is_missing(
            deserialize_doc_op(&no_key),
            "KeyValueUpdate.key"
        ));
    }

    #[test]
    fn deserialize_doc_op_skips_component_without_fields() {
        let message = doc_op_message(vec![
            Component::default(),
            characters("a"),
            Component::default(),
        ]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new().characters("a").build().unwrap()
        );
    }

    #[test]
    fn deserialize_doc_op_counts_false_booleans_as_present() {
        let message = doc_op_message(vec![
            element_start("p", vec![]),
            Component {
                element_end: Some(false),
                ..Default::default()
            },
            Component {
                delete_element_start: Some(element("q", vec![])),
                ..Default::default()
            },
            Component {
                delete_element_end: Some(false),
                ..Default::default()
            },
        ]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new()
                .element_start("p", Attributes::new())
                .element_end()
                .delete_element_start("q", Attributes::new())
                .delete_element_end()
                .build()
                .unwrap()
        );
    }

    #[test]
    fn deserialize_doc_op_reads_first_set_field_of_component() {
        let message = doc_op_message(vec![Component {
            characters: Some("a".to_owned()),
            retain_item_count: Some(3),
            delete_characters: Some("b".to_owned()),
            ..Default::default()
        }]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new().characters("a").build().unwrap()
        );
        let message = doc_op_message(vec![Component {
            retain_item_count: Some(3),
            delete_characters: Some("b".to_owned()),
            ..Default::default()
        }]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new().retain(3).build().unwrap()
        );
    }

    #[test]
    fn deserialize_doc_op_ignores_contents_when_empty_flag_is_set() {
        let message = doc_op_message(vec![
            Component {
                replace_attributes: Some(ReplaceAttributes {
                    empty: Some(true),
                    old_attribute: vec![kvp("a", "1")],
                    new_attribute: vec![kvp("a", "2")],
                }),
                ..Default::default()
            },
            Component {
                update_attributes: Some(UpdateAttributes {
                    empty: Some(true),
                    attribute_update: vec![kvu("a", None, Some("1"))],
                }),
                ..Default::default()
            },
        ]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new()
                .replace_attributes(Attributes::new(), Attributes::new())
                .update_attributes(AttributesUpdate::new())
                .build()
                .unwrap()
        );
        // A false flag is the same as an absent one.
        let message = doc_op_message(vec![Component {
            update_attributes: Some(UpdateAttributes {
                empty: Some(false),
                attribute_update: vec![kvu("a", None, Some("1"))],
            }),
            ..Default::default()
        }]);
        assert_eq!(
            deserialize_doc_op(&message).unwrap(),
            DocOpBuilder::new()
                .update_attributes(AttributesUpdate::single("a", None, Some("1")))
                .build()
                .unwrap()
        );
    }

    #[test]
    fn deserialize_hashed_version_rejects_negative_version() {
        #[rustfmt::skip]
        let bytes = [
            // version (1, varint) = -1
            0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
            0x12, 0x00, // history_hash, empty
        ];
        let message: ProtocolHashedVersion = decode(&bytes).unwrap();
        assert_eq!(message, hashed_version(-1, &[]));
        assert!(is_invalid(
            deserialize_hashed_version(&message),
            "ProtocolHashedVersion.version"
        ));
        assert!(is_invalid(
            deserialize_hashed_version(&hashed_version(i64::MIN, &[1])),
            "ProtocolHashedVersion.version"
        ));
        assert_eq!(
            deserialize_hashed_version(&hashed_version(i64::MAX, &[1])).unwrap(),
            HashedVersion::of(i64::MAX as u64, vec![1])
        );
    }

    #[test]
    fn deserialize_hashed_version_rejects_missing_fields() {
        assert!(is_missing(
            deserialize_hashed_version(&ProtocolHashedVersion {
                version: None,
                history_hash: Some(vec![]),
            }),
            "version"
        ));
        assert!(is_missing(
            deserialize_hashed_version(&ProtocolHashedVersion {
                version: Some(1),
                history_hash: None,
            }),
            "history_hash"
        ));
    }

    #[test]
    fn serialize_hashed_version_rejects_versions_above_int64() {
        assert!(is_invalid(
            serialize_hashed_version(&HashedVersion::unsigned(i64::MAX as u64 + 1)),
            "ProtocolHashedVersion.version"
        ));
        assert!(is_invalid(
            serialize_hashed_version(&HashedVersion::of(u64::MAX, vec![1])),
            "ProtocolHashedVersion.version"
        ));
        assert_eq!(
            serialize_hashed_version(&HashedVersion::of(i64::MAX as u64, vec![1])).unwrap(),
            hashed_version(i64::MAX, &[1])
        );
    }

    #[test]
    fn deserialize_op_rejects_empty_operation() {
        assert!(matches!(
            deserialize_op(&ProtocolWaveletOperation::default(), op_context()),
            Err(ProtoError::Unsupported(_))
        ));
        // An empty operation decodes: it has no required fields.
        let message: ProtocolWaveletOperation = decode(&[]).unwrap();
        assert!(deserialize_op(&message, op_context()).is_err());
    }

    #[test]
    fn deserialize_op_reads_false_no_op_as_no_op() {
        let message = ProtocolWaveletOperation {
            no_op: Some(false),
            ..Default::default()
        };
        let op = deserialize_op(&message, op_context()).unwrap();
        assert_eq!(op.kind(), &WaveletOperationKind::NoOp);
        // From bytes: no_op (4, varint) = false.
        let message: ProtocolWaveletOperation = decode(&[0x20, 0x00]).unwrap();
        assert_eq!(message.no_op, Some(false));
        assert_eq!(
            deserialize_op(&message, op_context()).unwrap().kind(),
            &WaveletOperationKind::NoOp
        );
    }

    #[test]
    fn deserialize_op_prefers_no_op_then_add_then_remove_then_mutate() {
        let mutate = MutateDocument {
            document_id: Some("b".to_owned()),
            document_operation: Some(doc_op_message(vec![characters("a")])),
        };
        let mut message = ProtocolWaveletOperation {
            add_participant: Some("a@b.c".to_owned()),
            remove_participant: Some("d@e.f".to_owned()),
            mutate_document: Some(mutate),
            no_op: Some(true),
        };
        let kind = |message: &ProtocolWaveletOperation| {
            deserialize_op(message, op_context())
                .unwrap()
                .kind()
                .clone()
        };
        assert_eq!(kind(&message), WaveletOperationKind::NoOp);
        message.no_op = None;
        assert_eq!(
            kind(&message),
            WaveletOperationKind::AddParticipant {
                participant: pid("a@b.c"),
                position: None,
            }
        );
        message.add_participant = None;
        assert_eq!(
            kind(&message),
            WaveletOperationKind::RemoveParticipant {
                participant: pid("d@e.f"),
            }
        );
        message.remove_participant = None;
        assert_eq!(
            deserialize_op(&message, op_context()).unwrap(),
            WaveletOperation::blip_content(
                op_context(),
                "b",
                DocOpBuilder::new().characters("a").build().unwrap()
            )
        );
    }

    #[test]
    fn deserialize_op_keeps_context_and_does_not_validate_addresses() {
        let context = WaveletOperationContext::with_hashed_version(
            pid("a@b.c"),
            1234,
            1,
            HashedVersion::of(5, vec![1]),
        );
        let op =
            deserialize_op(&add_participant_message("Not An Address"), context.clone()).unwrap();
        assert_eq!(op.context(), &context);
        // Lower-cased but otherwise taken as is.
        assert_eq!(
            op.kind(),
            &WaveletOperationKind::AddParticipant {
                participant: ParticipantId::new_unchecked("not an address"),
                position: None,
            }
        );
    }

    #[test]
    fn deserialize_op_rejects_invalid_document_operation() {
        let message = ProtocolWaveletOperation {
            mutate_document: Some(MutateDocument {
                document_id: Some("b".to_owned()),
                document_operation: Some(doc_op_message(vec![retain(-1)])),
            }),
            ..Default::default()
        };
        assert!(is_invalid(
            deserialize_op(&message, op_context()),
            "retain_item_count"
        ));
        let message = ProtocolWaveletOperation {
            mutate_document: Some(MutateDocument {
                document_id: Some("b".to_owned()),
                document_operation: None,
            }),
            ..Default::default()
        };
        assert!(is_missing(
            deserialize_op(&message, op_context()),
            "MutateDocument.document_operation"
        ));
    }

    #[test]
    fn serialize_op_rejects_submit_and_version_update() {
        let submit = WaveletOperation::blip(op_context(), "b", BlipOperation::Submit);
        assert!(matches!(
            serialize_op(&submit),
            Err(ProtoError::Unsupported(_))
        ));
        let version_update = WaveletOperation::version_update(
            pid("a@b.c"),
            1,
            Some(HashedVersion::unsigned(1)),
            None,
            None,
        );
        assert!(matches!(
            serialize_op(&version_update),
            Err(ProtoError::Unsupported(_))
        ));
        let version_update =
            WaveletOperation::version_update(pid("a@b.c"), 1, None, Some("b".to_owned()), Some(3));
        assert!(matches!(
            serialize_op(&version_update),
            Err(ProtoError::Unsupported(_))
        ));

        // A delta containing one cannot be serialised either.
        let delta = WaveletDelta::new(
            pid("a@b.c"),
            HashedVersion::unsigned(0),
            vec![WaveletOperation::no_op(op_context()), submit],
        );
        assert!(matches!(
            serialize_delta(&delta),
            Err(ProtoError::Unsupported(_))
        ));
    }

    fn delta_message(
        author: &str,
        operations: Vec<ProtocolWaveletOperation>,
    ) -> ProtocolWaveletDelta {
        ProtocolWaveletDelta {
            hashed_version: Some(hashed_version(3, &[1, 2])),
            author: Some(author.to_owned()),
            operation: operations,
            address_path: vec![],
        }
    }

    #[test]
    fn deserialize_delta_sets_contexts() {
        let message = delta_message(
            "A@b.c",
            vec![no_op_message(), add_participant_message("x@y.z")],
        );
        let delta = deserialize_delta(&message).unwrap();
        // The author is lower-cased.
        assert_eq!(delta.author(), &pid("a@b.c"));
        assert_eq!(delta.target_version(), &HashedVersion::of(3, vec![1, 2]));
        assert_eq!(delta.len(), 2);
        for op in delta.iter() {
            assert_eq!(
                op.context(),
                &WaveletOperationContext::new(pid("a@b.c"), NO_TIMESTAMP, 1)
            );
        }
    }

    #[test]
    fn deserialize_delta_validates_author_only_with_operations() {
        // As in the original, where the author is validated when building
        // the context of each operation.
        let empty = deserialize_delta(&delta_message("nobody", vec![])).unwrap();
        assert_eq!(empty.author(), &ParticipantId::new_unchecked("nobody"));
        assert!(empty.is_empty());
        assert!(matches!(
            deserialize_delta(&delta_message("nobody", vec![no_op_message()])),
            Err(ProtoError::ParticipantId(_))
        ));
    }

    #[test]
    fn deserialize_delta_rejects_invalid_content() {
        let mut message = delta_message("a@b.c", vec![no_op_message()]);
        message.hashed_version = Some(hashed_version(-1, &[]));
        assert!(is_invalid(deserialize_delta(&message), "version"));
        message.hashed_version = None;
        assert!(is_missing(deserialize_delta(&message), "hashed_version"));
        let mut message = delta_message("a@b.c", vec![no_op_message()]);
        message.author = None;
        assert!(is_missing(deserialize_delta(&message), "author"));
        let message = delta_message("a@b.c", vec![ProtocolWaveletOperation::default()]);
        assert!(matches!(
            deserialize_delta(&message),
            Err(ProtoError::Unsupported(_))
        ));
    }

    #[test]
    fn deserialize_transformed_delta_rejects_empty_delta() {
        assert!(is_invalid(
            deserialize_transformed_delta(
                &delta_message("a@b.c", vec![]),
                HashedVersion::of(4, vec![9]),
                1000
            ),
            "ProtocolWaveletDelta.operation"
        ));
    }

    #[test]
    fn deserialize_transformed_delta_rejects_invalid_author_address() {
        for author in ["nobody", "a@", "a@b@c", ""] {
            assert!(
                matches!(
                    deserialize_transformed_delta(
                        &delta_message(author, vec![no_op_message()]),
                        HashedVersion::of(4, vec![9]),
                        1000
                    ),
                    Err(ProtoError::ParticipantId(_))
                ),
                "author {author:?}"
            );
        }
    }

    #[test]
    fn deserialize_transformed_delta_sets_contexts_and_ignores_message_version() {
        let message = delta_message(
            "a@b.c",
            vec![no_op_message(), add_participant_message("x@y.z")],
        );
        let resulting_version = HashedVersion::of(10, vec![9]);
        let delta =
            deserialize_transformed_delta(&message, resulting_version.clone(), 1000).unwrap();
        assert_eq!(delta.author(), &pid("a@b.c"));
        assert_eq!(delta.resulting_version(), &resulting_version);
        assert_eq!(delta.applied_at_version(), 8);
        assert_eq!(delta.application_timestamp(), 1000);
        assert_eq!(
            delta.ops()[0].context(),
            &WaveletOperationContext::new(pid("a@b.c"), 1000, 1)
        );
        assert_eq!(
            delta.ops()[1].context(),
            &WaveletOperationContext::with_hashed_version(pid("a@b.c"), 1000, 1, resulting_version)
        );
    }

    #[test]
    fn deserialize_transformed_delta_rejects_resulting_version_below_operation_count() {
        let message = delta_message("a@b.c", vec![no_op_message(), no_op_message()]);
        assert!(matches!(
            deserialize_transformed_delta(&message, HashedVersion::of(1, vec![9]), 1000),
            Err(ProtoError::Delta(_))
        ));
    }

    fn stored_delta_message() -> ProtoTransformedWaveletDelta {
        ProtoTransformedWaveletDelta {
            author: Some("a@b.c".to_owned()),
            resulting_version: Some(hashed_version(5, &[9])),
            application_timestamp: Some(1000),
            operation: vec![no_op_message(), add_participant_message("x@y.z")],
        }
    }

    #[test]
    fn deserialize_stored_delta_sets_contexts() {
        let delta = deserialize_stored_delta(&stored_delta_message()).unwrap();
        let resulting_version = HashedVersion::of(5, vec![9]);
        assert_eq!(delta.author(), &pid("a@b.c"));
        assert_eq!(delta.resulting_version(), &resulting_version);
        assert_eq!(delta.applied_at_version(), 3);
        assert_eq!(delta.application_timestamp(), 1000);
        assert_eq!(
            delta.ops()[0].context(),
            &WaveletOperationContext::new(pid("a@b.c"), 1000, 1)
        );
        assert_eq!(
            delta.ops()[1].context(),
            &WaveletOperationContext::with_hashed_version(pid("a@b.c"), 1000, 1, resulting_version)
        );
    }

    #[test]
    fn deserialize_stored_delta_rejects_invalid_content() {
        let mut message = stored_delta_message();
        message.author = Some("nobody".to_owned());
        assert!(matches!(
            deserialize_stored_delta(&message),
            Err(ProtoError::ParticipantId(_))
        ));
        let mut message = stored_delta_message();
        message.resulting_version = Some(hashed_version(-5, &[9]));
        assert!(is_invalid(deserialize_stored_delta(&message), "version"));
        let mut message = stored_delta_message();
        message.resulting_version = Some(hashed_version(1, &[9]));
        assert!(matches!(
            deserialize_stored_delta(&message),
            Err(ProtoError::Delta(_))
        ));
        let mut message = stored_delta_message();
        message.operation.push(ProtocolWaveletOperation::default());
        assert!(matches!(
            deserialize_stored_delta(&message),
            Err(ProtoError::Unsupported(_))
        ));
        let mut message = stored_delta_message();
        message.application_timestamp = None;
        assert!(is_missing(
            deserialize_stored_delta(&message),
            "application_timestamp"
        ));
    }

    fn snapshot_message() -> WaveletSnapshot {
        WaveletSnapshot {
            wavelet_id: Some("example.com/conv+root".to_owned()),
            participant_id: vec!["a@example.com".to_owned(), "b@example.com".to_owned()],
            document: vec![DocumentSnapshot {
                document_id: Some("b+1".to_owned()),
                document_operation: Some(doc_op_message(vec![characters("hi")])),
                author: Some("a@example.com".to_owned()),
                contributor: vec!["b@example.com".to_owned()],
                last_modified_version: Some(3),
                last_modified_time: Some(4),
            }],
            version: Some(hashed_version(3, &[7])),
            last_modified_time: Some(4),
            creator: Some("a@example.com".to_owned()),
            creation_time: Some(2),
        }
    }

    fn wave_id() -> WaveId {
        WaveId::of("example.com", "w+abc").unwrap()
    }

    #[test]
    fn deserialize_wavelet_accepts_valid_snapshot() {
        let wavelet = deserialize_wavelet(&snapshot_message(), &wave_id()).unwrap();
        assert_eq!(
            wavelet.participants(),
            [pid("a@example.com"), pid("b@example.com")]
        );
        assert_eq!(wavelet.document_ids().collect::<Vec<_>>(), ["b+1"]);
    }

    #[test]
    fn deserialize_wavelet_rejects_invalid_participant_addresses() {
        // Participant.
        let mut message = snapshot_message();
        message.participant_id.push("nobody".to_owned());
        assert!(matches!(
            deserialize_wavelet(&message, &wave_id()),
            Err(ProtoError::ParticipantId(_))
        ));
        // Creator.
        let mut message = snapshot_message();
        message.creator = Some("a@".to_owned());
        assert!(matches!(
            deserialize_wavelet(&message, &wave_id()),
            Err(ProtoError::ParticipantId(_))
        ));
        // Document author.
        let mut message = snapshot_message();
        message.document[0].author = Some("a@b@c".to_owned());
        assert!(matches!(
            deserialize_wavelet(&message, &wave_id()),
            Err(ProtoError::ParticipantId(_))
        ));
        // Document contributor.
        let mut message = snapshot_message();
        message.document[0].contributor.push(String::new());
        assert!(matches!(
            deserialize_wavelet(&message, &wave_id()),
            Err(ProtoError::ParticipantId(_))
        ));
    }

    #[test]
    fn deserialize_wavelet_rejects_negative_last_modified_version() {
        let mut message = snapshot_message();
        message.document[0].last_modified_version = Some(-1);
        assert!(is_invalid(
            deserialize_wavelet(&message, &wave_id()),
            "DocumentSnapshot.last_modified_version"
        ));
    }

    #[test]
    fn deserialize_wavelet_rejects_negative_version() {
        let mut message = snapshot_message();
        message.version = Some(hashed_version(-1, &[7]));
        assert!(is_invalid(
            deserialize_wavelet(&message, &wave_id()),
            "ProtocolHashedVersion.version"
        ));
    }

    #[test]
    fn deserialize_wavelet_rejects_invalid_wavelet_id() {
        for id in ["", "conv+root", "example.com/conv+root/x", "example.com/"] {
            let mut message = snapshot_message();
            message.wavelet_id = Some(id.to_owned());
            assert!(
                matches!(
                    deserialize_wavelet(&message, &wave_id()),
                    Err(ProtoError::Id(_))
                ),
                "wavelet id {id:?}"
            );
        }
    }

    #[test]
    fn deserialize_wavelet_rejects_invalid_documents() {
        // Content that is not an initialization.
        let mut message = snapshot_message();
        message.document[0].document_operation = Some(doc_op_message(vec![retain(1)]));
        assert!(matches!(
            deserialize_wavelet(&message, &wave_id()),
            Err(ProtoError::Operation(_))
        ));
        // Ill-formed content.
        let mut message = snapshot_message();
        message.document[0].document_operation =
            Some(doc_op_message(vec![element_start("p", vec![])]));
        assert!(matches!(
            deserialize_wavelet(&message, &wave_id()),
            Err(ProtoError::Operation(_))
        ));
        // Two documents with the same id.
        let mut message = snapshot_message();
        message.document.push(message.document[0].clone());
        assert!(matches!(
            deserialize_wavelet(&message, &wave_id()),
            Err(ProtoError::Data(_))
        ));
        // A missing required field in a message built in memory.
        let mut message = snapshot_message();
        message.document[0].document_id = None;
        assert!(is_missing(
            deserialize_wavelet(&message, &wave_id()),
            "DocumentSnapshot.document_id"
        ));
    }

    #[test]
    fn deserialize_wavelet_drops_duplicate_participants_and_contributors() {
        let mut message = snapshot_message();
        message.participant_id.push("a@example.com".to_owned());
        message.document[0]
            .contributor
            .push("b@example.com".to_owned());
        let wavelet = deserialize_wavelet(&message, &wave_id()).unwrap();
        assert_eq!(
            wavelet.participants(),
            [pid("a@example.com"), pid("b@example.com")]
        );
        assert_eq!(
            wavelet.document("b+1").unwrap().contributors(),
            [pid("b@example.com")]
        );
    }

    #[test]
    fn serialize_wavelet_rejects_versions_above_int64() {
        let mut wavelet = deserialize_wavelet(&snapshot_message(), &wave_id()).unwrap();
        assert!(is_invalid(
            serialize_wavelet(&wavelet, &HashedVersion::unsigned(u64::MAX)),
            "ProtocolHashedVersion.version"
        ));
        wavelet
            .document_mut("b+1")
            .unwrap()
            .set_last_modified_version(u64::MAX);
        assert!(is_invalid(
            serialize_wavelet(&wavelet, &HashedVersion::unsigned(3)),
            "DocumentSnapshot.last_modified_version"
        ));
    }
}

// Round trips through bytes.

mod round_trips {
    use super::*;

    const DOC_IDS: [&str; 3] = ["b+1", "b+2", "conversation"];

    /// Random operations that apply in sequence to an empty document, and
    /// the resulting document.
    fn random_ops(seed: i32, count: usize) -> (Vec<DocOp>, BootstrapDocument) {
        let p = Parameters::default();
        let mut r = RandomProviderImpl::of_seed(seed);
        let mut doc = BootstrapDocument::new();
        let mut ops = Vec::with_capacity(count);
        for _ in 0..count {
            let op = generate_random_op(&mut r, &p, &doc);
            doc.consume(&op).unwrap();
            ops.push(op);
        }
        (ops, doc)
    }

    /// Operations of every serialisable kind by `author`.
    fn random_wavelet_ops(seed: i32, author: &ParticipantId) -> Vec<WaveletOperation> {
        let context = WaveletOperationContext::new(author.clone(), NO_TIMESTAMP, 1);
        let mut ops = vec![
            WaveletOperation::add_participant(context.clone(), pid("x@example.com")),
            WaveletOperation::no_op(context.clone()),
        ];
        for (i, op) in random_ops(seed, 4).0.into_iter().enumerate() {
            ops.push(WaveletOperation::blip_content(
                context.clone(),
                DOC_IDS[i % DOC_IDS.len()],
                op,
            ));
        }
        ops.push(WaveletOperation::remove_participant(
            context,
            pid("x@example.com"),
        ));
        ops
    }

    fn assert_same_contexts(a: &[WaveletOperation], b: &[WaveletOperation]) {
        assert_eq!(a.len(), b.len());
        for (a_op, b_op) in a.iter().zip(b) {
            assert_eq!(a_op.context(), b_op.context());
        }
    }

    #[test]
    fn random_doc_ops_survive_round_trip() {
        let mut components = 0;
        for seed in 0..50 {
            for op in random_ops(seed, 6).0 {
                let message = serialize_doc_op(&op).unwrap();
                assert_eq!(message.component.len(), op.len());
                let bytes = encode(&message).unwrap();
                let decoded: ProtocolDocumentOperation = decode(&bytes).unwrap();
                assert_eq!(decoded, message);
                assert_eq!(deserialize_doc_op(&decoded).unwrap(), op, "seed {seed}");
                components += op.len();
            }
        }
        // The generator produced something to convert.
        assert!(components > 500, "{components} components");
    }

    #[test]
    fn random_doc_ops_with_supplementary_characters_survive_round_trip() {
        let p = Parameters {
            supplementary_characters: true,
            ..Parameters::default()
        };
        for seed in 0..20 {
            let mut r = RandomProviderImpl::of_seed(seed);
            let mut doc = BootstrapDocument::new();
            for _ in 0..6 {
                let op = generate_random_op(&mut r, &p, &doc);
                doc.consume(&op).unwrap();
                let bytes = encode(&serialize_doc_op(&op).unwrap()).unwrap();
                assert_eq!(
                    deserialize_doc_op(&decode(&bytes).unwrap()).unwrap(),
                    op,
                    "seed {seed}"
                );
            }
        }
    }

    #[test]
    fn deltas_survive_round_trip() {
        for seed in 0..20 {
            let author = pid("author@example.com");
            let delta = WaveletDelta::new(
                author.clone(),
                HashedVersion::of(seed as u64 * 1000, vec![seed as u8; 20]),
                random_wavelet_ops(seed, &author),
            );
            let message = serialize_delta(&delta).unwrap();
            let bytes = encode(&message).unwrap();
            let decoded: ProtocolWaveletDelta = decode(&bytes).unwrap();
            assert_eq!(decoded, message);
            let parsed = deserialize_delta(&decoded).unwrap();
            assert_eq!(parsed, delta, "seed {seed}");
            assert_same_contexts(&parsed, &delta);
        }
    }

    #[test]
    fn empty_delta_survives_round_trip() {
        let delta = WaveletDelta::new(pid("a@example.com"), HashedVersion::unsigned(0), vec![]);
        let bytes = encode(&serialize_delta(&delta).unwrap()).unwrap();
        assert_eq!(deserialize_delta(&decode(&bytes).unwrap()).unwrap(), delta);
    }

    #[test]
    fn transformed_deltas_survive_round_trip() {
        for seed in 0..20 {
            let author = pid("author@example.com");
            let ops = random_wavelet_ops(seed, &author);
            let resulting_version =
                HashedVersion::of(seed as u64 * 1000 + ops.len() as u64, vec![seed as u8; 20]);
            let timestamp = 1_000_000 + i64::from(seed);
            let delta = TransformedWaveletDelta::clone_operations(
                author,
                resulting_version.clone(),
                timestamp,
                &ops,
            )
            .unwrap();

            let message = serialize_transformed_delta(&delta).unwrap();
            // The message carries the applied-at version without a hash.
            assert_eq!(
                message.hashed_version,
                Some(hashed_version(seed as i64 * 1000, &[]))
            );
            let bytes = encode(&message).unwrap();
            let parsed = deserialize_transformed_delta(
                &decode(&bytes).unwrap(),
                resulting_version,
                timestamp,
            )
            .unwrap();
            assert_eq!(parsed, delta, "seed {seed}");
            assert_same_contexts(&parsed, &delta);
        }
    }

    #[test]
    fn stored_deltas_survive_round_trip() {
        for seed in 0..20 {
            let author = pid("author@example.com");
            let ops = random_wavelet_ops(seed, &author);
            let delta = TransformedWaveletDelta::clone_operations(
                author,
                HashedVersion::of(seed as u64 * 1000 + ops.len() as u64, vec![seed as u8; 20]),
                1_000_000 + i64::from(seed),
                &ops,
            )
            .unwrap();

            let message = serialize_stored_delta(&delta).unwrap();
            let bytes = encode(&message).unwrap();
            let decoded: ProtoTransformedWaveletDelta = decode(&bytes).unwrap();
            assert_eq!(decoded, message);
            let parsed = deserialize_stored_delta(&decoded).unwrap();
            assert_eq!(parsed, delta, "seed {seed}");
            assert_same_contexts(&parsed, &delta);
        }
    }

    #[test]
    fn empty_stored_delta_survives_round_trip() {
        let delta = TransformedWaveletDelta::clone_operations(
            pid("a@example.com"),
            HashedVersion::of(4, vec![1]),
            1000,
            &[],
        )
        .unwrap();
        let bytes = encode(&serialize_stored_delta(&delta).unwrap()).unwrap();
        assert_eq!(
            deserialize_stored_delta(&decode(&bytes).unwrap()).unwrap(),
            delta
        );
    }

    #[test]
    fn wavelet_snapshots_survive_round_trip() {
        for seed in 0..20 {
            let name = WaveletName::of(
                WaveId::of("example.com", "w+abc123").unwrap(),
                WaveletId::of("other.example.com", "conv+root").unwrap(),
            );
            let version = HashedVersion::of(seed as u64 + 17, vec![seed as u8; 20]);
            let creator = pid("creator@example.com");
            let mut wavelet = WaveletData::empty(&name, creator.clone(), version.clone(), 1234);
            wavelet.add_participant(pid("b@example.com"));
            wavelet.add_participant(creator.clone());
            wavelet.add_participant(pid("a@example.com"));
            // Created out of id order.
            for (i, doc_id) in DOC_IDS.iter().rev().enumerate() {
                let (_, doc) = random_ops(seed * 10 + i as i32, 3);
                wavelet
                    .create_document(
                        doc_id,
                        pid("a@example.com"),
                        [pid("b@example.com"), creator.clone()],
                        &doc.as_operation(),
                        5000 + i as i64,
                        seed as u64 + i as u64,
                    )
                    .unwrap();
            }
            wavelet.set_last_modified_time(9999);

            let message = serialize_wavelet(&wavelet, &version).unwrap();
            let ids: Vec<&str> = message
                .document
                .iter()
                .map(|d| d.document_id.as_deref().unwrap())
                .collect();
            assert_eq!(ids, DOC_IDS);
            let bytes = encode(&message).unwrap();
            let decoded: WaveletSnapshot = decode(&bytes).unwrap();
            assert_eq!(decoded, message);
            let parsed = deserialize_wavelet(&decoded, &name.wave_id).unwrap();
            assert_eq!(parsed, wavelet, "seed {seed}");
            // Serialising again gives the same bytes.
            assert_eq!(
                encode(&serialize_wavelet(&parsed, &version).unwrap()).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn serialized_wavelet_carries_given_version() {
        // The snapshot version is the argument, not the wavelet's own.
        let name = WaveletName::of(
            WaveId::of("example.com", "w+abc123").unwrap(),
            WaveletId::of("example.com", "conv+root").unwrap(),
        );
        let wavelet =
            WaveletData::empty(&name, pid("a@example.com"), HashedVersion::unsigned(0), 7);
        let version = HashedVersion::of(42, vec![1, 2, 3]);
        let message = serialize_wavelet(&wavelet, &version).unwrap();
        assert_eq!(message.version, Some(hashed_version(42, &[1, 2, 3])));
        assert_eq!(message.wavelet_id.as_deref(), Some("example.com/conv+root"));
        let parsed = deserialize_wavelet(&message, &name.wave_id).unwrap();
        assert_eq!(parsed.version(), 42);
        assert_eq!(parsed.hashed_version(), &version);
        assert_eq!(parsed.creation_time(), 7);
        assert_eq!(parsed.last_modified_time(), 7);
    }

    #[test]
    fn byte_string_message_keeps_serialized_bytes() {
        let message = hashed_version(300, &[0xDE, 0xAD]);
        let wrapped = ByteStringMessage::serialize_message(message.clone()).unwrap();
        assert_eq!(wrapped.message(), &message);
        assert_eq!(wrapped.bytes(), [0x08, 0xAC, 0x02, 0x12, 0x02, 0xDE, 0xAD]);
        let parsed =
            ByteStringMessage::<ProtocolHashedVersion>::parse_from(wrapped.bytes()).unwrap();
        assert_eq!(parsed, wrapped);
        assert_eq!(parsed.message(), &message);
        assert_eq!(
            wrapped.into_bytes(),
            [0x08, 0xAC, 0x02, 0x12, 0x02, 0xDE, 0xAD]
        );
    }

    #[test]
    fn byte_string_message_keeps_original_bytes_with_unknown_fields() {
        #[rustfmt::skip]
        let bytes = [
            0x08, 0x01,       // version (1, varint) = 1
            0x78, 0x05,       // unknown field 15, varint
            0x12, 0x01, 0x09, // history_hash (2, bytes) = 09
        ];
        let parsed = ByteStringMessage::<ProtocolHashedVersion>::parse_from(bytes).unwrap();
        assert_eq!(parsed.message(), &hashed_version(1, &[9]));
        assert_eq!(parsed.bytes(), bytes);
        // Re-encoding the message would not reproduce the bytes.
        let reencoded = encode(parsed.message()).unwrap();
        assert_ne!(reencoded, bytes);
        assert_eq!(reencoded, [0x08, 0x01, 0x12, 0x01, 0x09]);
        assert_eq!(parsed.clone().into_bytes(), bytes);
    }

    #[test]
    fn byte_string_message_keeps_original_bytes_in_other_field_order() {
        // history_hash before version: valid, but not the canonical order.
        let bytes = [0x12, 0x01, 0x09, 0x08, 0x01];
        let parsed = ByteStringMessage::<ProtocolHashedVersion>::parse_from(bytes).unwrap();
        assert_eq!(parsed.message(), &hashed_version(1, &[9]));
        assert_eq!(parsed.bytes(), bytes);
        assert_ne!(encode(parsed.message()).unwrap(), bytes);
    }

    #[test]
    fn byte_string_message_equality_is_by_bytes() {
        let canonical = [0x08, 0x01, 0x12, 0x01, 0x09];
        let with_unknown = [0x08, 0x01, 0x78, 0x05, 0x12, 0x01, 0x09];
        let a = ByteStringMessage::<ProtocolHashedVersion>::parse_from(canonical).unwrap();
        let b = ByteStringMessage::<ProtocolHashedVersion>::parse_from(with_unknown).unwrap();
        // The same message from different bytes.
        assert_eq!(a.message(), b.message());
        assert_ne!(a, b);
        let c = ByteStringMessage::serialize_message(hashed_version(1, &[9])).unwrap();
        assert_eq!(a, c);
        assert_ne!(b, c);
        let d = ByteStringMessage::<ProtocolHashedVersion>::parse_from(with_unknown).unwrap();
        assert_eq!(b, d);
    }

    #[test]
    fn byte_string_message_rejects_invalid_messages() {
        assert!(matches!(
            ByteStringMessage::<ProtocolHashedVersion>::parse_from([0x08]),
            Err(ProtoError::Decode(_))
        ));
        assert!(is_missing(
            ByteStringMessage::<ProtocolHashedVersion>::parse_from([0x08, 0x01]),
            "history_hash"
        ));
        assert!(is_missing(
            ByteStringMessage::serialize_message(ProtocolHashedVersion {
                version: None,
                history_hash: Some(vec![]),
            }),
            "version"
        ));
    }

    fn sample_delta(target_version: &HashedVersion) -> WaveletDelta {
        let author = pid("author@example.com");
        let context = WaveletOperationContext::new(author.clone(), NO_TIMESTAMP, 1);
        WaveletDelta::new(
            author,
            target_version.clone(),
            vec![
                WaveletOperation::add_participant(context.clone(), pid("x@example.com")),
                WaveletOperation::no_op(context.clone()),
                WaveletOperation::blip_content(
                    context,
                    "b+1",
                    DocOpBuilder::new().characters("hi").build().unwrap(),
                ),
            ],
        )
    }

    fn signed(delta: &WaveletDelta) -> ProtocolSignedDelta {
        ProtocolSignedDelta {
            delta: Some(encode(&serialize_delta(delta).unwrap()).unwrap()),
            signature: vec![ProtocolSignature {
                signature_bytes: Some(vec![1, 2, 3]),
                signer_id: Some(vec![4, 5]),
                signature_algorithm: Some(1),
            }],
        }
    }

    /// `SHA-256(hash ++ bytes)` truncated to 20 bytes.
    fn next_hash(hash: &[u8], bytes: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(hash);
        hasher.update(bytes);
        hasher.finalize()[..20].to_vec()
    }

    #[test]
    fn build_applied_delta_records_applied_at_version() {
        let target = HashedVersion::of(10, vec![1; 20]);
        let applied_at = HashedVersion::of(12, vec![2; 20]);
        let delta = sample_delta(&target);
        let signed_delta = signed(&delta);
        let applied = build_applied_delta(signed_delta.clone(), &applied_at, 3, 1234).unwrap();

        let expected = ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(signed_delta),
            hashed_version_applied_at: Some(hashed_version(12, &[2; 20])),
            operations_applied: Some(3),
            application_timestamp: Some(1234),
            epoch: None,
            rebased_operations: None,
        };
        assert_eq!(applied.message(), &expected);
        assert_eq!(applied.bytes(), encode(&expected).unwrap());
        // The bytes parse back to an equal message.
        assert_eq!(
            ByteStringMessage::<ProtocolAppliedWaveletDelta>::parse_from(applied.bytes()).unwrap(),
            applied
        );
    }

    #[test]
    fn build_applied_delta_rejects_unrepresentable_values() {
        let delta = sample_delta(&HashedVersion::unsigned(0));
        assert!(is_invalid(
            build_applied_delta(signed(&delta), &HashedVersion::unsigned(0), u32::MAX, 0),
            "operations_applied"
        ));
        assert!(is_invalid(
            build_applied_delta(signed(&delta), &HashedVersion::unsigned(u64::MAX), 1, 0),
            "version"
        ));
        assert!(is_missing(
            build_applied_delta(
                ProtocolSignedDelta {
                    delta: None,
                    signature: vec![],
                },
                &HashedVersion::unsigned(0),
                1,
                0
            ),
            "ProtocolSignedDelta.delta"
        ));
    }

    #[test]
    fn get_hashed_version_applied_at_with_version_present() {
        let target = HashedVersion::of(10, vec![1; 20]);
        let applied_at = HashedVersion::of(12, vec![2; 20]);
        let applied =
            build_applied_delta(signed(&sample_delta(&target)), &applied_at, 3, 1234).unwrap();
        assert_eq!(get_hashed_version_applied_at(&applied).unwrap(), applied_at);

        // The original delta is not inspected when the version is present.
        let garbage = ProtocolSignedDelta {
            delta: Some(vec![0xFF, 0xFF]),
            signature: vec![],
        };
        let applied = build_applied_delta(garbage, &applied_at, 3, 1234).unwrap();
        assert_eq!(get_hashed_version_applied_at(&applied).unwrap(), applied_at);
    }

    #[test]
    fn get_hashed_version_applied_at_without_version_present() {
        let target = HashedVersion::of(10, vec![1; 20]);
        let applied = ByteStringMessage::serialize_message(ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(signed(&sample_delta(&target))),
            hashed_version_applied_at: None,
            operations_applied: Some(3),
            application_timestamp: Some(1234),
            epoch: None,
            rebased_operations: None,
        })
        .unwrap();
        // Falls back to the target version of the original delta.
        assert_eq!(get_hashed_version_applied_at(&applied).unwrap(), target);

        // Then the original delta must parse.
        let applied = ByteStringMessage::serialize_message(ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(ProtocolSignedDelta {
                delta: Some(vec![0xFF, 0xFF]),
                signature: vec![],
            }),
            hashed_version_applied_at: None,
            operations_applied: Some(3),
            application_timestamp: Some(1234),
            epoch: None,
            rebased_operations: None,
        })
        .unwrap();
        assert!(matches!(
            get_hashed_version_applied_at(&applied),
            Err(ProtoError::Decode(_))
        ));
        // An empty original delta lacks its required fields.
        let applied = ByteStringMessage::serialize_message(ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(ProtocolSignedDelta {
                delta: Some(vec![]),
                signature: vec![],
            }),
            hashed_version_applied_at: None,
            operations_applied: Some(3),
            application_timestamp: Some(1234),
            epoch: None,
            rebased_operations: None,
        })
        .unwrap();
        assert!(matches!(
            get_hashed_version_applied_at(&applied),
            Err(ProtoError::MissingField(_))
        ));
    }

    #[test]
    fn get_hashed_version_applied_at_rejects_negative_version() {
        let applied = ByteStringMessage::serialize_message(ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(signed(&sample_delta(&HashedVersion::unsigned(0)))),
            hashed_version_applied_at: Some(hashed_version(-1, &[])),
            operations_applied: Some(3),
            application_timestamp: Some(1234),
            epoch: None,
            rebased_operations: None,
        })
        .unwrap();
        assert!(is_invalid(
            get_hashed_version_applied_at(&applied),
            "version"
        ));
    }

    #[test]
    fn calculate_resulting_hashed_version_extends_hash_chain() {
        let target = HashedVersion::of(10, vec![1; 20]);
        let applied_at = HashedVersion::of(12, vec![2; 20]);
        let applied =
            build_applied_delta(signed(&sample_delta(&target)), &applied_at, 3, 1234).unwrap();
        let resulting = calculate_resulting_hashed_version(&applied).unwrap();
        assert_eq!(resulting.version(), 15);
        assert_eq!(
            resulting.history_hash(),
            next_hash(applied_at.history_hash(), applied.bytes())
        );
        assert_eq!(
            resulting,
            HashedVersionFactory::create(applied.bytes(), &applied_at, 3)
        );
    }

    #[test]
    fn calculate_resulting_hashed_version_without_version_present() {
        let target = HashedVersion::of(10, vec![1; 20]);
        let applied = ByteStringMessage::serialize_message(ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(signed(&sample_delta(&target))),
            hashed_version_applied_at: None,
            operations_applied: Some(2),
            application_timestamp: Some(1234),
            epoch: None,
            rebased_operations: None,
        })
        .unwrap();
        let resulting = calculate_resulting_hashed_version(&applied).unwrap();
        assert_eq!(resulting.version(), 12);
        assert_eq!(
            resulting.history_hash(),
            next_hash(target.history_hash(), applied.bytes())
        );
    }

    #[test]
    fn calculate_resulting_hashed_version_uses_original_bytes() {
        // An unknown field changes the bytes and so the hash, though the
        // message is the same.
        let applied_at = HashedVersion::of(12, vec![2; 20]);
        let built = build_applied_delta(
            signed(&sample_delta(&HashedVersion::unsigned(12))),
            &applied_at,
            3,
            1234,
        )
        .unwrap();
        let mut bytes = built.bytes().to_vec();
        bytes.extend([0x78, 0x05]); // unknown field 15, varint
        let parsed =
            ByteStringMessage::<ProtocolAppliedWaveletDelta>::parse_from(bytes.clone()).unwrap();
        assert_eq!(parsed.message(), built.message());
        let resulting = calculate_resulting_hashed_version(&parsed).unwrap();
        assert_eq!(
            resulting.history_hash(),
            next_hash(applied_at.history_hash(), &bytes)
        );
        assert_ne!(
            resulting,
            calculate_resulting_hashed_version(&built).unwrap()
        );
    }

    #[test]
    fn calculate_resulting_hashed_version_rejects_negative_operation_count() {
        let applied = ByteStringMessage::serialize_message(ProtocolAppliedWaveletDelta {
            signed_original_delta: Some(signed(&sample_delta(&HashedVersion::unsigned(0)))),
            hashed_version_applied_at: Some(hashed_version(0, &[])),
            operations_applied: Some(-1),
            application_timestamp: Some(1234),
            epoch: None,
            rebased_operations: None,
        })
        .unwrap();
        assert!(is_invalid(
            calculate_resulting_hashed_version(&applied),
            "operations_applied"
        ));
    }

    #[test]
    fn build_transformed_delta_computes_resulting_version() {
        let target = HashedVersion::of(10, vec![1; 20]);
        let applied_at = HashedVersion::of(12, vec![2; 20]);
        let original = sample_delta(&target);
        let transformed = sample_delta(&applied_at);
        let applied = build_applied_delta(signed(&original), &applied_at, 3, 1234).unwrap();

        let delta = build_transformed_delta(&applied, &transformed).unwrap();
        let resulting = calculate_resulting_hashed_version(&applied).unwrap();
        assert_eq!(delta.author(), transformed.author());
        assert_eq!(delta.resulting_version(), &resulting);
        assert_eq!(delta.applied_at_version(), 12);
        assert_eq!(delta.application_timestamp(), 1234);
        assert_eq!(delta.ops(), transformed.ops());
        let author = transformed.author().clone();
        assert_eq!(
            delta.ops()[0].context(),
            &WaveletOperationContext::new(author.clone(), 1234, 1)
        );
        assert_eq!(
            delta.ops()[2].context(),
            &WaveletOperationContext::with_hashed_version(author, 1234, 1, resulting)
        );
    }

    #[test]
    fn build_transformed_delta_rejects_mismatched_target_version() {
        let applied_at = HashedVersion::of(12, vec![2; 20]);
        let applied =
            build_applied_delta(signed(&sample_delta(&applied_at)), &applied_at, 3, 1234).unwrap();
        // Another version number.
        let transformed = sample_delta(&HashedVersion::of(13, vec![2; 20]));
        assert!(is_invalid(
            build_transformed_delta(&applied, &transformed),
            "hashed_version_applied_at"
        ));
        // The same version number with another hash.
        let transformed = sample_delta(&HashedVersion::of(12, vec![3; 20]));
        assert!(is_invalid(
            build_transformed_delta(&applied, &transformed),
            "hashed_version_applied_at"
        ));
    }

    #[test]
    fn build_transformed_delta_rejects_mismatched_operation_count() {
        let applied_at = HashedVersion::of(12, vec![2; 20]);
        let transformed = sample_delta(&applied_at);
        for count in [2, 4] {
            let applied =
                build_applied_delta(signed(&transformed), &applied_at, count, 1234).unwrap();
            assert!(
                is_invalid(
                    build_transformed_delta(&applied, &transformed),
                    "operations_applied"
                ),
                "count {count}"
            );
        }
    }
}
