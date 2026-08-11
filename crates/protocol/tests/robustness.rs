//! Malformed, hostile, and forward-compatible input: errors, never panics;
//! unknown rsntr classes decode as Generic (v3).

use bytes::{BufMut, BytesMut};
use proptest::prelude::*;
use resonator_protocol::{
    EnvelopeObject, EnvelopeParser, ProtocolError, Statement, Value, decode_envelope,
    decode_frame_eof, encode_envelope,
};

#[test]
fn unknown_predicates_on_known_classes_are_ignored() {
    let doc = r#"[] a rsntr:Query ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:futureFeature "from a newer minor version" ;
   rsntr:anotherThing 42 ;
   rdfs:label "even a foreign vocabulary predicate" ;
   rsntr:signal "SELECT 1" .
"#;
    let got = EnvelopeObject::from_turtle(doc).expect("parse");
    assert_eq!(
        got,
        EnvelopeObject::Query(Statement {
            id: "01J9V3Z8K7Q2M4X6P8R0T2V4W6".into(),
            modulation: "sql-sqlite".into(),
            signal: "SELECT 1".into(),
            params: vec![],
            database: None,
            row_limit: None,
            byte_limit: None,
            timeout_ms: None,
        })
    );
}

#[test]
fn unknown_predicates_on_rows_are_ignored() {
    let doc = "[] a rsntr:Row ; rsntr:seq 0 ; rsntr:col_x 1 ; rsntr:rowChecksum \"abc\" .\n";
    let got = EnvelopeObject::from_turtle(doc).expect("parse");
    assert_eq!(
        got,
        EnvelopeObject::Row(vec![resonator_protocol::Row {
            seq: 0,
            cells: vec![("x".into(), Value::Integer(1))],
        }])
    );
}

/// v3: an unknown rsntr class is not an error; it decodes as Generic with
/// the class name and the subject's properties in document order.
#[test]
fn unknown_rsntr_class_decodes_as_generic() {
    let doc = "[] a rsntr:Frobnicate ; rsntr:id \"x\" ; rsntr:level 3 .\n";
    let got = EnvelopeObject::from_turtle(doc).expect("parse");
    let EnvelopeObject::Generic(gnrc) = got else {
        panic!("expected Generic, got {got:?}");
    };
    assert_eq!(gnrc.class, "Frobnicate");
    assert_eq!(gnrc.props.len(), 2);
    assert_eq!(
        gnrc.props[0].0.as_str(),
        "http://resonator.network/v3/rsntr#id"
    );
    assert_eq!(
        gnrc.props[1].0.as_str(),
        "http://resonator.network/v3/rsntr#level"
    );
    // And it re-serializes to an equivalent frame.
    let rewritten = EnvelopeObject::Generic(gnrc.clone())
        .to_turtle()
        .expect("serialize");
    let back = EnvelopeObject::from_turtle(&rewritten).expect("reparse");
    assert_eq!(back, EnvelopeObject::Generic(gnrc));
}

/// A frame typed with a foreign (non-rsntr) class alone is still refused:
/// Generic passthrough covers the rsntr namespace only.
#[test]
fn foreign_class_alone_still_errors() {
    let doc = "<urn:x:thing> a <http://example.org/vocab#Thing> .\n";
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn two_typed_subjects_error() {
    let doc = r#"[] a rsntr:Knock ; rsntr:message "hi" .
[] a rsntr:Presence ; rsntr:at "2026-07-23T09:15:00" .
"#;
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn row_mixed_with_other_object_errors() {
    let doc = r#"[] a rsntr:Row ; rsntr:seq 0 .
[] a rsntr:Done ; rsntr:id "x" ; rsntr:truncated false .
"#;
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn two_graph_subjects_error() {
    let doc = r#"[] a rsntr:Graph ; rsntr:id "x" ; rsntr:seq 0 .
[] a rsntr:Graph ; rsntr:id "x" ; rsntr:seq 1 .
"#;
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn empty_document_errors() {
    assert!(matches!(
        EnvelopeObject::from_turtle(""),
        Err(ProtocolError::Malformed(_))
    ));
    // Valid Turtle, but no envelope object in it.
    assert!(matches!(
        EnvelopeObject::from_turtle("# just a comment\n"),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn untyped_subject_errors() {
    let doc = "[] rsntr:message \"typeless\" .\n";
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn missing_required_property_errors() {
    // Query without rsntr:signal.
    let doc = "[] a rsntr:Query ; rsntr:id \"x\" ; rsntr:mod \"sql-sqlite\" .\n";
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
    // Row without rsntr:seq.
    let doc = "[] a rsntr:Row ; rsntr:col_x 1 .\n";
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
    // Graph without rsntr:seq.
    let doc = "[] a rsntr:Graph ; rsntr:id \"x\" .\n";
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn unsupported_param_datatype_errors() {
    let doc = r#"[] a rsntr:Query ;
   rsntr:id "x" ; rsntr:mod "m" ; rsntr:signal "t" ;
   rsntr:params (true) .
"#;
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn out_of_range_integer_errors() {
    let doc = r#"[] a rsntr:Query ;
   rsntr:id "x" ; rsntr:mod "m" ; rsntr:signal "t" ;
   rsntr:params (9223372036854775808) .
"#;
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn broken_list_errors() {
    // rdf:rest missing on a hand-built list node.
    let doc = r#"[] a rsntr:Query ;
   rsntr:id "x" ; rsntr:mod "m" ; rsntr:signal "t" ;
   rsntr:params _:l .
_:l rdf:first "a" .
"#;
    assert!(matches!(
        EnvelopeObject::from_turtle(doc),
        Err(ProtocolError::Malformed(_))
    ));
}

#[test]
fn chunked_push_parses_byte_at_a_time() {
    let doc = r#"[] a rsntr:Query ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal "SELECT name, mtime FROM notes WHERE mtime > ?" ;
   rsntr:params ("2026-07-01" rsntr:null 42) ;
   rsntr:rowLimit 1000 .
"#;
    let mut parser = EnvelopeParser::new();
    for b in doc.as_bytes() {
        parser.push(std::slice::from_ref(b)).expect("push");
    }
    let got = parser.finish().expect("finish");
    let EnvelopeObject::Query(st) = got else {
        panic!("not a query");
    };
    assert_eq!(
        st.params,
        vec![
            Value::Text("2026-07-01".into()),
            Value::Null,
            Value::Integer(42)
        ]
    );
}

#[test]
fn multiple_frames_decode_in_sequence() {
    let a = EnvelopeObject::Knock(resonator_protocol::Knock {
        id: None,
        message: "first".into(),
    });
    let b = EnvelopeObject::Presence(resonator_protocol::Presence {
        at: "2026-07-23T09:15:00".into(),
        status: None,
        endpoint: None,
    });
    let mut buf = BytesMut::new();
    encode_envelope(&a, &mut buf).expect("encode a");
    encode_envelope(&b, &mut buf).expect("encode b");
    assert_eq!(decode_envelope(&mut buf).expect("a").expect("some"), a);
    assert_eq!(decode_envelope(&mut buf).expect("b").expect("some"), b);
    assert!(decode_envelope(&mut buf).expect("end").is_none());
}

#[test]
fn blank_node_labels_do_not_leak_between_frames() {
    // Two frames each using the label _:x must not interfere.
    let doc1 = "_:x a rsntr:Knock ; rsntr:message \"one\" .\n";
    let doc2 = "_:x a rsntr:Knock ; rsntr:message \"two\" .\n";
    let a = EnvelopeObject::from_turtle(doc1).expect("frame 1");
    let b = EnvelopeObject::from_turtle(doc2).expect("frame 2");
    assert_ne!(a, b);
}

proptest! {
    /// Arbitrary bytes through the full decode path: any outcome is a clean
    /// Ok/Err, never a panic.
    #[test]
    fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let mut buf = BytesMut::from(&data[..]);
        loop {
            match decode_envelope(&mut buf) {
                Ok(Some(_)) => continue,
                Ok(None) => {
                    // Leftover partial frame must error (not panic) at EOF.
                    let _ = decode_frame_eof(&mut buf);
                    break;
                }
                Err(_) => break,
            }
        }
    }

    /// Arbitrary text through the Turtle envelope parser: clean Ok/Err only.
    #[test]
    fn arbitrary_text_never_panics(text in ".{0,512}") {
        let _ = EnvelopeObject::from_turtle(&text);
    }
}

#[test]
fn oversized_frame_via_put_prefix_errors() {
    let mut buf = BytesMut::new();
    buf.put_u32_le(u32::MAX);
    assert!(matches!(
        decode_envelope(&mut buf),
        Err(ProtocolError::FrameTooLarge { .. })
    ));
}
