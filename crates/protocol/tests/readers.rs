//! The ordered state machines: ResponseReader (Result -> Rows -> Done, plus
//! the terminal Denied/Error/Help/Media forms and the v3 Graph/Generic
//! passthrough streams) and EntrainmentReader (Done ack -> Vibrations ->
//! end on Damp confirmation, Error, or stream close). Also the Request
//! model round-trip.

use oxrdf::{Literal, NamedNode, Term};
use resonator_protocol::{
    Denied, Done, EntrainmentEvent, EntrainmentReader, EnvelopeObject, ErrorEnvelope, Generic,
    Graph, Help, Media, Request, RequestKind, ResponseEvent, ResponseReader, ResultHeader, Row,
    Value, Vibration,
};

const RID: &str = "01J9V3Z8K7Q2M4X6P8R0T2V4W6";
const OTHER_ID: &str = "01K12M8Z4T9Q6W2E8R4T0Y6X3A";
const POINT: &str = "urn:notes:changed";

fn header(cols: &[&str]) -> EnvelopeObject {
    EnvelopeObject::Result(ResultHeader {
        id: RID.into(),
        columns: cols.iter().map(|c| c.to_string()).collect(),
        decl_types: vec![],
    })
}

fn rows(seqs: &[i64]) -> EnvelopeObject {
    EnvelopeObject::Row(
        seqs.iter()
            .map(|&seq| Row {
                seq,
                cells: vec![("a".into(), Value::Integer(seq))],
            })
            .collect(),
    )
}

fn done() -> EnvelopeObject {
    EnvelopeObject::Done(Done {
        id: RID.into(),
        row_count: None,
        affected_rows: None,
        last_insert_rowid: None,
        truncated: false,
    })
}

fn graph(seq: i64) -> EnvelopeObject {
    EnvelopeObject::Graph(Graph {
        id: RID.into(),
        seq,
        payload: vec![],
    })
}

fn generic() -> EnvelopeObject {
    EnvelopeObject::Generic(Generic {
        class: "Custom".into(),
        props: vec![(
            NamedNode::new("http://resonator.network/v3/rsntr#note").unwrap(),
            Term::Literal(Literal::new_simple_literal("hi")),
        )],
    })
}

// --- ResponseReader: rows path --------------------------------------------

#[test]
fn response_happy_path() {
    let mut r = ResponseReader::new(RID);
    assert!(matches!(
        r.accept(header(&["a"])).unwrap(),
        ResponseEvent::Header(_)
    ));
    assert!(matches!(
        r.accept(rows(&[0, 1])).unwrap(),
        ResponseEvent::Rows(_)
    ));
    assert!(matches!(
        r.accept(rows(&[2])).unwrap(),
        ResponseEvent::Rows(_)
    ));
    let ev = r.accept(done()).unwrap();
    assert!(ev.is_terminal());
    assert!(r.is_finished());
    r.finish().unwrap();
}

#[test]
fn response_choreography_errors() {
    // Row before header.
    let mut r = ResponseReader::new(RID);
    assert!(r.accept(rows(&[0])).is_err());

    // Done before anything.
    let mut r = ResponseReader::new(RID);
    assert!(r.accept(done()).is_err());

    // Duplicate header.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    assert!(r.accept(header(&["a"])).is_err());

    // Seq gap.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    r.accept(rows(&[0])).unwrap();
    assert!(r.accept(rows(&[2])).is_err());
    // Poisoned after the first error.
    assert!(r.accept(rows(&[1])).is_err());

    // Undeclared column.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["b"])).unwrap();
    assert!(r.accept(rows(&[0])).is_err());

    // Mismatched request id.
    let mut r = ResponseReader::new(RID);
    assert!(
        r.accept(EnvelopeObject::Result(ResultHeader {
            id: OTHER_ID.into(),
            columns: vec![],
            decl_types: vec![],
        }))
        .is_err()
    );

    // Frame after completion.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    r.accept(done()).unwrap();
    assert!(r.accept(rows(&[0])).is_err());

    // Truncation: no terminal frame before end of stream.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    assert!(r.finish().is_err());
}

#[test]
fn response_early_denied_and_error() {
    let mut r = ResponseReader::new(RID);
    let ev = r
        .accept(EnvelopeObject::Denied(Denied {
            id: Some(RID.into()),
            reason: None,
        }))
        .unwrap();
    assert!(ev.is_terminal());
    r.finish().unwrap();

    // Error mid-stream.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    let ev = r
        .accept(EnvelopeObject::Error(ErrorEnvelope {
            id: None,
            code: "engine-error".into(),
            reason: None,
        }))
        .unwrap();
    assert!(ev.is_terminal());

    // Denied after the header is out of choreography.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    assert!(
        r.accept(EnvelopeObject::Denied(Denied {
            id: None,
            reason: None
        }))
        .is_err()
    );
}

#[test]
fn response_help_is_a_whole_response() {
    let mut r = ResponseReader::new(RID);
    let ev = r
        .accept(EnvelopeObject::Help(Help {
            id: Some(RID.into()),
            signal: "usage...".into(),
            topics: vec!["knock".into()],
        }))
        .unwrap();
    assert_eq!(
        ev,
        ResponseEvent::Help {
            signal: "usage...".into(),
            topics: vec!["knock".into()],
        }
    );
    assert!(r.is_finished());
    r.finish().unwrap();
}

#[test]
fn response_media_is_terminal_for_frames() {
    let mut r = ResponseReader::new(RID);
    let ev = r
        .accept(EnvelopeObject::Media(Media {
            id: RID.into(),
            content_type: "video/mp2t".into(),
        }))
        .unwrap();
    assert!(matches!(ev, ResponseEvent::Media(_)));
    assert!(ev.is_terminal());
    assert!(r.is_finished());
    r.finish().unwrap();

    // Media after the header is out of choreography.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    assert!(
        r.accept(EnvelopeObject::Media(Media {
            id: RID.into(),
            content_type: "video/mp2t".into(),
        }))
        .is_err()
    );
}

// --- ResponseReader: graph passthrough (v3) -------------------------------

#[test]
fn response_graph_stream() {
    let mut r = ResponseReader::new(RID);
    assert!(matches!(
        r.accept(graph(0)).unwrap(),
        ResponseEvent::Graph(_)
    ));
    assert!(matches!(
        r.accept(graph(1)).unwrap(),
        ResponseEvent::Graph(_)
    ));
    r.accept(done()).unwrap();
    r.finish().unwrap();
}

#[test]
fn response_graph_choreography() {
    // Seq must start at 0.
    let mut r = ResponseReader::new(RID);
    assert!(r.accept(graph(1)).is_err());

    // Seq gap.
    let mut r = ResponseReader::new(RID);
    r.accept(graph(0)).unwrap();
    assert!(r.accept(graph(2)).is_err());

    // Wrong id.
    let mut r = ResponseReader::new(RID);
    assert!(
        r.accept(EnvelopeObject::Graph(Graph {
            id: OTHER_ID.into(),
            seq: 0,
            payload: vec![],
        }))
        .is_err()
    );

    // Graph after a Result header is out of choreography.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    assert!(r.accept(graph(0)).is_err());

    // Rows in a graph stream are out of choreography.
    let mut r = ResponseReader::new(RID);
    r.accept(graph(0)).unwrap();
    assert!(r.accept(rows(&[0])).is_err());

    // Truncation: graphs but no Done.
    let mut r = ResponseReader::new(RID);
    r.accept(graph(0)).unwrap();
    assert!(r.finish().is_err());
}

// --- ResponseReader: generic passthrough (v3) -----------------------------

#[test]
fn response_generic_stream() {
    let mut r = ResponseReader::new(RID);
    assert!(matches!(
        r.accept(generic()).unwrap(),
        ResponseEvent::Generic(_)
    ));
    assert!(matches!(
        r.accept(generic()).unwrap(),
        ResponseEvent::Generic(_)
    ));
    r.accept(done()).unwrap();
    r.finish().unwrap();
}

#[test]
fn response_generic_choreography() {
    // Generic after a Result header is out of choreography.
    let mut r = ResponseReader::new(RID);
    r.accept(header(&["a"])).unwrap();
    assert!(r.accept(generic()).is_err());

    // Graph in a generic stream is out of choreography.
    let mut r = ResponseReader::new(RID);
    r.accept(generic()).unwrap();
    assert!(r.accept(graph(0)).is_err());

    // Truncation: generics but no Done.
    let mut r = ResponseReader::new(RID);
    r.accept(generic()).unwrap();
    assert!(r.finish().is_err());
}

// --- EntrainmentReader ----------------------------------------------------

fn ack() -> EnvelopeObject {
    done()
}

fn vibration(seq: i64) -> EnvelopeObject {
    EnvelopeObject::Vibration(Vibration {
        id: RID.into(),
        point: POINT.into(),
        seq,
        at: None,
        payload: vec![],
    })
}

#[test]
fn entrainment_happy_path() {
    let mut r = EntrainmentReader::new(RID, POINT);
    assert_eq!(r.accept(ack()).unwrap(), EntrainmentEvent::Entrained);
    assert!(r.is_flowing());
    assert!(matches!(
        r.accept(vibration(0)).unwrap(),
        EntrainmentEvent::Vibration(_)
    ));
    assert!(matches!(
        r.accept(vibration(1)).unwrap(),
        EntrainmentEvent::Vibration(_)
    ));
    // End of stream while flowing is a normal end of entrainment.
    r.finish().unwrap();
}

#[test]
fn entrainment_damp_confirmed_by_second_done() {
    let mut r = EntrainmentReader::new(RID, POINT);
    r.accept(ack()).unwrap();
    r.accept(vibration(0)).unwrap();
    // The node confirming a Damp is a second Done: terminal, not an error.
    assert_eq!(r.accept(ack()).unwrap(), EntrainmentEvent::Damped);
    assert!(r.is_finished());
    assert!(r.accept(vibration(1)).is_err());
}

#[test]
fn entrainment_damped_by_error() {
    let mut r = EntrainmentReader::new(RID, POINT);
    r.accept(ack()).unwrap();
    r.accept(vibration(0)).unwrap();
    let ev = r
        .accept(EnvelopeObject::Error(ErrorEnvelope {
            id: Some(RID.into()),
            code: "limit-exceeded".into(),
            reason: Some("slow consumer".into()),
        }))
        .unwrap();
    assert!(matches!(ev, EntrainmentEvent::Error(_)));
    assert!(r.is_finished());
    assert!(r.accept(vibration(1)).is_err());
}

#[test]
fn entrainment_denied_before_ack() {
    let mut r = EntrainmentReader::new(RID, POINT);
    let ev = r
        .accept(EnvelopeObject::Denied(Denied {
            id: Some(RID.into()),
            reason: None,
        }))
        .unwrap();
    assert!(matches!(ev, EntrainmentEvent::Denied(_)));
    assert!(r.is_finished());
}

#[test]
fn entrainment_choreography_errors() {
    // Vibration before the acknowledgment.
    let mut r = EntrainmentReader::new(RID, POINT);
    assert!(r.accept(vibration(0)).is_err());

    // Sequence gap, then poisoned.
    let mut r = EntrainmentReader::new(RID, POINT);
    r.accept(ack()).unwrap();
    r.accept(vibration(0)).unwrap();
    assert!(r.accept(vibration(2)).is_err());
    assert!(r.accept(vibration(1)).is_err());

    // Wrong point.
    let mut r = EntrainmentReader::new(RID, POINT);
    r.accept(ack()).unwrap();
    assert!(
        r.accept(EnvelopeObject::Vibration(Vibration {
            id: RID.into(),
            point: "urn:other:point".into(),
            seq: 0,
            at: None,
            payload: vec![],
        }))
        .is_err()
    );

    // Wrong request id on the ack.
    let mut r = EntrainmentReader::new(RID, POINT);
    assert!(
        r.accept(EnvelopeObject::Done(Done {
            id: OTHER_ID.into(),
            row_count: None,
            affected_rows: None,
            last_insert_rowid: None,
            truncated: false,
        }))
        .is_err()
    );

    // Truncation: end of stream before the ack.
    let r = EntrainmentReader::new(RID, POINT);
    assert!(r.finish().is_err());
}

// --- Request model --------------------------------------------------------

#[test]
fn request_round_trips_through_the_envelope() {
    let mut req = Request::new(RequestKind::Query, "sql-sqlite", "SELECT * FROM notes");
    req.params = vec![Value::Integer(1), Value::Null];
    req.database = Some("main".into());
    req.options.row_limit = Some(100);
    let obj = req.to_envelope();
    let back = Request::from_envelope(&obj).expect("decode");
    assert_eq!(back, req);
    assert_eq!(back.id_string().len(), 26);
}

#[test]
fn request_rejects_non_request_frames_and_bad_ulids() {
    assert!(Request::from_envelope(&done()).is_err());
    assert!(Request::parse_id("not-a-ulid").is_err());
    assert_eq!(Request::parse_id(RID).unwrap().len(), 16);
}
