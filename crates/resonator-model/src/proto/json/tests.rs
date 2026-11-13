//! Tests of the JSON form of the protocol messages.
//!
//! `testdata/json_golden.txt` was produced by
//! `testdata/JsonGoldenGenerator.java` with the Java protobuf runtime, Gson
//! and the helper classes of Apache Wave (see that file for what is the
//! original code and what is transcribed from the generator templates).
//! The other tests are hand-written from the templates.

use super::*;
use crate::docop::testing::{Parameters, RandomProviderImpl, generate_random_op};
use crate::docop::{BootstrapDocument, DocOp};
use crate::proto::serialize_doc_op;

const GOLDEN: &str = include_str!("../../../testdata/json_golden.txt");

fn hashed(version: i64, hash: &[u8]) -> ProtocolHashedVersion {
    ProtocolHashedVersion {
        version: Some(version),
        history_hash: Some(hash.to_vec()),
    }
}

fn random_doc_ops(seed: i32, count: usize, supplementary: bool) -> Vec<DocOp> {
    let p = Parameters {
        supplementary_characters: supplementary,
        ..Parameters::default()
    };
    let mut r = RandomProviderImpl::of_seed(seed);
    let mut doc = BootstrapDocument::new();
    let mut ops = Vec::with_capacity(count);
    for _ in 0..count {
        let op = generate_random_op(&mut r, &p, &doc);
        doc.consume(&op).unwrap();
        ops.push(op);
    }
    ops
}

const STRINGS: [&str; 8] = [
    "",
    "plain",
    "quote \" backslash \\ slash / angle <b>&amp;</b> = '",
    "tab\tbackspace\u{8}newline\nreturn\rformfeed\u{c}nul\u{0}unit\u{1f}del\u{7f}",
    "line\u{2028}paragraph\u{2029}separators",
    "caf\u{e9} \u{4e2d}\u{6587} \u{1F600} \u{10FFFF}",
    "\u{feff}bom and \u{fffd} replacement",
    "a@example.com",
];

const INT52: [i64; 16] = [
    0,
    1,
    7,
    -1,
    9_999_999,
    10_000_000,
    10_000_001,
    12_345_678,
    123_456_789_012,
    10_000_000_000,
    1_700_000_000_000,
    1_699_999_999_999,
    9_007_199_254_740_991,
    9_007_199_254_740_992,
    -10_000_000,
    -1_234_567_890_123,
];

const LONGS: [i64; 12] = [
    0,
    1,
    -1,
    2_147_483_647,
    2_147_483_648,
    4_294_967_295,
    4_294_967_296,
    1_700_000_000_123,
    -1_700_000_000_123,
    i64::MAX,
    i64::MIN,
    -4_294_967_296,
];

fn mutate(doc: &str, op: &DocOp) -> ProtocolWaveletOperation {
    ProtocolWaveletOperation {
        mutate_document: Some(MutateDocument {
            document_id: Some(doc.to_owned()),
            document_operation: Some(serialize_doc_op(op).unwrap()),
        }),
        ..Default::default()
    }
}

fn snapshot(seed: i32, time: i64) -> WaveletSnapshot {
    let mut doc = BootstrapDocument::new();
    let p = Parameters::default();
    let mut r = RandomProviderImpl::of_seed(seed);
    for _ in 0..5 {
        let op = generate_random_op(&mut r, &p, &doc);
        doc.consume(&op).unwrap();
    }
    WaveletSnapshot {
        wavelet_id: Some("example.com/conv+root".to_owned()),
        participant_id: vec!["a@example.com".to_owned(), "@example.com".to_owned()],
        document: vec![
            DocumentSnapshot {
                document_id: Some("b+1".to_owned()),
                document_operation: Some(serialize_doc_op(doc.as_operation().as_op()).unwrap()),
                author: Some("a@example.com".to_owned()),
                contributor: vec!["a@example.com".to_owned(), "b@example.com".to_owned()],
                last_modified_version: Some(i64::from(seed) + 3),
                last_modified_time: Some(time),
            },
            DocumentSnapshot {
                document_id: Some("conversation".to_owned()),
                document_operation: Some(ProtocolDocumentOperation::default()),
                author: Some("b@example.com".to_owned()),
                contributor: Vec::new(),
                last_modified_version: Some(0),
                last_modified_time: Some(-1),
            },
        ],
        version: Some(hashed(42, &[0xde, 0xad, 0xbe, 0xef])),
        last_modified_time: Some(time),
        creator: Some("a@example.com".to_owned()),
        creation_time: Some(time.wrapping_sub(86_400_000)),
    }
}

/// The messages the golden file covers, as `(name, bytes)`.
fn golden_inputs() -> Vec<(&'static str, Vec<u8>)> {
    fn add<M: JsonMessage>(list: &mut Vec<(&'static str, Vec<u8>)>, message: M) {
        message.check_initialized().unwrap();
        list.push((M::NAME, message.encode_to_vec()));
    }
    let mut list = Vec::new();
    let all_bytes: Vec<u8> = (0..=255).collect();

    for version in INT52 {
        add(&mut list, hashed(version, &[]));
    }
    add(&mut list, hashed(3, &all_bytes));
    add(&mut list, hashed(0, b"wave://example.com/w+abc/conv+root"));

    for s in STRINGS {
        add(
            &mut list,
            ProtocolAuthenticate {
                token: Some(s.to_owned()),
            },
        );
    }
    add(&mut list, ProtocolAuthenticationResult {});
    add(&mut list, CancelRpc {});
    add(&mut list, RpcFinished::success());
    add(&mut list, RpcFinished::failure("Not logged in"));
    add(
        &mut list,
        RpcFinished {
            failed: Some(true),
            error_text: None,
        },
    );

    add(
        &mut list,
        ProtocolOpenRequest {
            participant_id: Some("a@example.com".to_owned()),
            wave_id: Some("example.com/w+abc".to_owned()),
            wavelet_id_prefix: Vec::new(),
            known_wavelet: Vec::new(),
        },
    );
    add(
        &mut list,
        ProtocolOpenRequest {
            participant_id: Some("a@example.com".to_owned()),
            wave_id: Some("example.com/w+abc".to_owned()),
            wavelet_id_prefix: vec![String::new(), "conv".to_owned(), "user+".to_owned()],
            known_wavelet: vec![
                WaveletVersion {
                    wavelet_id: Some("example.com/conv+root".to_owned()),
                    hashed_version: Some(hashed(17, &[1, 2, 3])),
                },
                WaveletVersion {
                    wavelet_id: Some("example.com/user+a@example.com".to_owned()),
                    hashed_version: Some(hashed(0, &[])),
                },
            ],
        },
    );

    // Every component and wavelet operation kind.
    let components = vec![
        Component {
            annotation_boundary: Some(AnnotationBoundary {
                empty: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        Component {
            annotation_boundary: Some(AnnotationBoundary {
                empty: None,
                end: vec!["style/color".to_owned(), "link".to_owned()],
                change: vec![
                    KeyValueUpdate {
                        key: Some("k".to_owned()),
                        old_value: None,
                        new_value: Some("new".to_owned()),
                    },
                    KeyValueUpdate {
                        key: Some("k2".to_owned()),
                        old_value: Some("old".to_owned()),
                        new_value: None,
                    },
                    KeyValueUpdate {
                        key: Some("k3".to_owned()),
                        old_value: Some(String::new()),
                        new_value: Some(String::new()),
                    },
                ],
            }),
            ..Default::default()
        },
        Component {
            characters: Some(STRINGS[5].to_owned()),
            ..Default::default()
        },
        Component {
            element_start: Some(ElementStart {
                r#type: Some("line".to_owned()),
                attribute: vec![KeyValuePair {
                    key: Some("t".to_owned()),
                    value: Some("h1".to_owned()),
                }],
            }),
            ..Default::default()
        },
        Component {
            element_end: Some(true),
            ..Default::default()
        },
        Component {
            retain_item_count: Some(2_147_483_647),
            ..Default::default()
        },
        Component {
            delete_characters: Some(STRINGS[3].to_owned()),
            ..Default::default()
        },
        Component {
            delete_element_start: Some(ElementStart {
                r#type: Some("p".to_owned()),
                attribute: Vec::new(),
            }),
            ..Default::default()
        },
        Component {
            delete_element_end: Some(true),
            ..Default::default()
        },
        Component {
            replace_attributes: Some(ReplaceAttributes {
                empty: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        Component {
            replace_attributes: Some(ReplaceAttributes {
                empty: None,
                old_attribute: vec![KeyValuePair {
                    key: Some("a".to_owned()),
                    value: Some("1".to_owned()),
                }],
                new_attribute: vec![KeyValuePair {
                    key: Some("b".to_owned()),
                    value: Some("2".to_owned()),
                }],
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
        Component {
            update_attributes: Some(UpdateAttributes {
                empty: None,
                attribute_update: vec![KeyValueUpdate {
                    key: Some("a".to_owned()),
                    old_value: Some("1".to_owned()),
                    new_value: None,
                }],
            }),
            ..Default::default()
        },
    ];
    let every_kind = ProtocolDocumentOperation {
        component: components,
    };
    add(&mut list, every_kind.clone());
    add(&mut list, ProtocolDocumentOperation::default());

    let mut operations = vec![
        ProtocolWaveletOperation {
            add_participant: Some("b@example.com".to_owned()),
            ..Default::default()
        },
        ProtocolWaveletOperation {
            remove_participant: Some("b@example.com".to_owned()),
            ..Default::default()
        },
        ProtocolWaveletOperation {
            no_op: Some(true),
            ..Default::default()
        },
        ProtocolWaveletOperation {
            mutate_document: Some(MutateDocument {
                document_id: Some("b+1".to_owned()),
                document_operation: Some(every_kind),
            }),
            ..Default::default()
        },
    ];
    for (seed, supplementary) in [(1, false), (2, false), (3, true), (4, true)] {
        for op in random_doc_ops(seed, 6, supplementary) {
            operations.push(mutate("b+random", &op));
        }
    }
    for operation in &operations {
        add(&mut list, operation.clone());
    }
    let delta = ProtocolWaveletDelta {
        hashed_version: Some(hashed(1234, &all_bytes[100..120])),
        author: Some("a@example.com".to_owned()),
        operation: operations.clone(),
        address_path: vec!["x@example.com".to_owned()],
    };
    add(&mut list, delta.clone());
    let small_delta = ProtocolWaveletDelta {
        hashed_version: Some(hashed(0, b"wave://example.com/w+abc/conv+root")),
        author: Some("a@example.com".to_owned()),
        operation: operations[..1].to_vec(),
        address_path: Vec::new(),
    };

    add(
        &mut list,
        ProtocolSubmitRequest {
            wavelet_name: Some("example.com/w+abc/~/conv+root".to_owned()),
            delta: Some(small_delta.clone()),
            channel_id: Some("ch7".to_owned()),
        },
    );
    add(
        &mut list,
        ProtocolSubmitRequest {
            wavelet_name: Some("example.com/w+abc/~/conv+root".to_owned()),
            delta: Some(delta.clone()),
            channel_id: None,
        },
    );
    add(
        &mut list,
        ProtocolSubmitResponse {
            operations_applied: Some(3),
            error_message: None,
            hashed_version_after_application: Some(hashed(1237, &all_bytes[..20])),
            pending: None,
        },
    );
    add(
        &mut list,
        ProtocolSubmitResponse {
            operations_applied: Some(0),
            error_message: Some("Author field on delta must match logged in user".to_owned()),
            hashed_version_after_application: None,
            pending: None,
        },
    );
    add(
        &mut list,
        ProtocolSubmitResponse {
            operations_applied: Some(-1),
            error_message: None,
            hashed_version_after_application: None,
            pending: None,
        },
    );

    for time in LONGS {
        add(&mut list, snapshot(5, time));
    }
    let name = "example.com/w+abc/~/conv+root".to_owned();
    // Channel id only, marker, snapshot, deltas, commit notice.
    add(
        &mut list,
        ProtocolWaveletUpdate {
            wavelet_name: Some("example.com/w+abc/~/dummy+root".to_owned()),
            channel_id: Some("ch1".to_owned()),
            ..Default::default()
        },
    );
    add(
        &mut list,
        ProtocolWaveletUpdate {
            wavelet_name: Some("example.com/w+abc/~/dummy+root".to_owned()),
            marker: Some(true),
            ..Default::default()
        },
    );
    add(
        &mut list,
        ProtocolWaveletUpdate {
            wavelet_name: Some(name.clone()),
            marker: Some(false),
            ..Default::default()
        },
    );
    add(
        &mut list,
        ProtocolWaveletUpdate {
            wavelet_name: Some(name.clone()),
            commit_notice: Some(hashed(40, &[9; 20])),
            resulting_version: Some(hashed(42, &[0xde, 0xad, 0xbe, 0xef])),
            snapshot: Some(snapshot(9, 1_700_000_000_123)),
            channel_id: Some("ch1".to_owned()),
            ..Default::default()
        },
    );
    add(
        &mut list,
        ProtocolWaveletUpdate {
            wavelet_name: Some(name.clone()),
            applied_delta: vec![small_delta, delta],
            resulting_version: Some(hashed(1237, &[7; 20])),
            channel_id: Some("ch1".to_owned()),
            ..Default::default()
        },
    );
    add(
        &mut list,
        ProtocolWaveletUpdate {
            wavelet_name: Some(name),
            commit_notice: Some(hashed(1237, &[7; 20])),
            channel_id: Some("ch1".to_owned()),
            ..Default::default()
        },
    );

    add(
        &mut list,
        WaveViewSnapshot {
            wave_id: Some("example.com/w+abc".to_owned()),
            wavelet: vec![snapshot(11, 1_700_000_000_123), snapshot(12, 5)],
        },
    );
    add(
        &mut list,
        WaveViewSnapshot {
            wave_id: Some("example.com/w+abc".to_owned()),
            wavelet: Vec::new(),
        },
    );
    add(&mut list, ProfileResponse::default());
    add(
        &mut list,
        ProfileResponse {
            profiles: vec![
                FetchedProfile {
                    address: Some("john.smith@example.com".to_owned()),
                    name: Some("John Smith".to_owned()),
                    image_url: Some("/iniavatars/100x100/john.smith@example.com".to_owned()),
                    profile_url: Some(String::new()),
                },
                FetchedProfile {
                    address: Some("@example.com".to_owned()),
                    name: Some("@example.com".to_owned()),
                    image_url: Some("/static/images/unknown.jpg".to_owned()),
                    profile_url: None,
                },
            ],
        },
    );
    add(
        &mut list,
        ProfileRequest {
            addresses: vec!["a@example.com".to_owned()],
        },
    );
    add(
        &mut list,
        SearchRequest {
            query: Some("in:inbox with:@ caf\u{e9}".to_owned()),
            index: Some(20),
            num_results: Some(100),
        },
    );
    add(
        &mut list,
        SearchResponse {
            query: Some(String::new()),
            total_results: Some(-1),
            digests: Vec::new(),
        },
    );
    add(
        &mut list,
        SearchResponse {
            query: Some("in:inbox".to_owned()),
            total_results: Some(2),
            digests: vec![
                Digest {
                    title: Some("Hello \u{4e16}\u{754c} \u{1F600}".to_owned()),
                    snippet: Some(" a \"quoted\" <b>snippet</b>\n".to_owned()),
                    wave_id: Some("example.com/w+abc".to_owned()),
                    last_modified: Some(1_700_000_000_123),
                    unread_count: Some(3),
                    blip_count: Some(7),
                    participants: vec!["bob@example.com".to_owned(), "@example.com".to_owned()],
                    author: Some("alice@example.com".to_owned()),
                },
                Digest {
                    title: Some(String::new()),
                    snippet: Some(String::new()),
                    wave_id: Some("example.com/w+empty".to_owned()),
                    last_modified: Some(-1),
                    unread_count: Some(0),
                    blip_count: Some(0),
                    participants: Vec::new(),
                    author: Some("nobody@example.com".to_owned()),
                },
            ],
        },
    );
    add(&mut list, AttachmentsResponse::default());
    add(
        &mut list,
        AttachmentsResponse {
            attachment: vec![
                AttachmentMetadata {
                    attachment_id: Some("example.com/att+1".to_owned()),
                    wave_ref: Some("example.com/w+abc/~/conv+root".to_owned()),
                    file_name: Some("caf\u{e9} \"photo\".png".to_owned()),
                    mime_type: Some("image/png".to_owned()),
                    size: Some(5_000_000_000),
                    creator: Some("alice@example.com".to_owned()),
                    attachment_url: Some("/attachment/example.com/att+1".to_owned()),
                    thumbnail_url: Some("/thumbnail/example.com/att+1".to_owned()),
                    image_metadata: Some(ImageMetadata {
                        width: Some(1920),
                        height: Some(1080),
                    }),
                    thumbnail_metadata: Some(ImageMetadata {
                        width: Some(200),
                        height: Some(112),
                    }),
                    malware: Some(false),
                },
                AttachmentMetadata {
                    attachment_id: Some("legacy".to_owned()),
                    wave_ref: Some("example.com/w+abc/~/conv+root".to_owned()),
                    file_name: Some("notes.txt".to_owned()),
                    mime_type: Some("text/plain".to_owned()),
                    size: Some(0),
                    creator: Some(String::new()),
                    attachment_url: Some("/attachment/legacy".to_owned()),
                    thumbnail_url: Some("/thumbnail/legacy".to_owned()),
                    image_metadata: None,
                    thumbnail_metadata: Some(ImageMetadata {
                        width: Some(95),
                        height: Some(60),
                    }),
                    malware: None,
                },
            ],
        },
    );
    list
}

/// Prints the input for `JsonGoldenGenerator`:
/// `cargo test -p resonator-model print_golden_input -- --ignored --nocapture`.
#[test]
#[ignore = "prints the input of the golden generator"]
fn print_golden_input() {
    for (name, bytes) in golden_inputs() {
        println!("M\t{name}\t{}", hex::encode(bytes));
    }
}

fn unescape(field: &str) -> String {
    let mut units = Vec::new();
    let mut rest = field;
    while !rest.is_empty() {
        if let Some(escaped) = rest.strip_prefix("\\u") {
            units.push(u16::from_str_radix(&escaped[..4], 16).unwrap());
            rest = &escaped[4..];
        } else {
            let c = rest.chars().next().unwrap();
            let mut buffer = [0; 2];
            units.extend_from_slice(c.encode_utf16(&mut buffer));
            rest = &rest[c.len_utf8()..];
        }
    }
    String::from_utf16(&units).unwrap()
}

/// Checks one golden record against a message type; false if the record is
/// of another type.
fn check<M: JsonMessage>(name: &str, bytes: &[u8], json: &str, envelope_seq: Option<i32>) -> bool {
    if name != M::NAME {
        return false;
    }
    let message = crate::proto::decode::<M>(bytes).unwrap();
    match envelope_seq {
        None => {
            assert_eq!(to_json(&message).unwrap(), json, "{name}");
            let back: M = from_json(json).unwrap_or_else(|e| panic!("{name}: {e}: {json}"));
            assert_eq!(back.encode_to_vec(), bytes, "{name}: {json}");
        }
        Some(seq) => {
            assert_eq!(envelope(seq, &message).unwrap(), json, "{name}");
            let parsed = Envelope::parse(json).unwrap();
            assert_eq!(parsed.message_type, name);
            assert_eq!(parsed.sequence_number, seq);
            assert_eq!(parsed.body::<M>().unwrap().encode_to_vec(), bytes);
        }
    }
    true
}

fn check_any(name: &str, bytes: &[u8], json: &str, seq: Option<i32>) {
    let known = check::<ProtocolHashedVersion>(name, bytes, json, seq)
        || check::<ProtocolWaveletDelta>(name, bytes, json, seq)
        || check::<ProtocolWaveletOperation>(name, bytes, json, seq)
        || check::<ProtocolDocumentOperation>(name, bytes, json, seq)
        || check::<ProtocolAuthenticate>(name, bytes, json, seq)
        || check::<ProtocolAuthenticationResult>(name, bytes, json, seq)
        || check::<ProtocolOpenRequest>(name, bytes, json, seq)
        || check::<WaveletSnapshot>(name, bytes, json, seq)
        || check::<WaveViewSnapshot>(name, bytes, json, seq)
        || check::<ProtocolWaveletUpdate>(name, bytes, json, seq)
        || check::<ProtocolSubmitRequest>(name, bytes, json, seq)
        || check::<ProtocolSubmitResponse>(name, bytes, json, seq)
        || check::<CancelRpc>(name, bytes, json, seq)
        || check::<RpcFinished>(name, bytes, json, seq)
        || check::<ProfileRequest>(name, bytes, json, seq)
        || check::<ProfileResponse>(name, bytes, json, seq)
        || check::<SearchRequest>(name, bytes, json, seq)
        || check::<SearchResponse>(name, bytes, json, seq)
        || check::<AttachmentsResponse>(name, bytes, json, seq);
    assert!(known, "golden record of unknown type {name}");
}

#[test]
fn matches_the_java_encoding() {
    let mut messages = Vec::new();
    let mut envelopes = 0;
    for line in GOLDEN.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["M", name, bytes, json] => {
                let bytes = hex::decode(bytes).unwrap();
                check_any(name, &bytes, &unescape(json), None);
                messages.push((name.to_string(), bytes));
            }
            ["E", name, seq, bytes, json] => {
                let bytes = hex::decode(bytes).unwrap();
                check_any(name, &bytes, &unescape(json), Some(seq.parse().unwrap()));
                envelopes += 1;
            }
            other => panic!("unexpected golden line {other:?}"),
        }
    }
    // The golden file covers exactly the current input set.
    let expected: Vec<(String, Vec<u8>)> = golden_inputs()
        .into_iter()
        .map(|(name, bytes)| (name.to_owned(), bytes))
        .collect();
    assert_eq!(messages.len(), expected.len());
    assert!(messages == expected, "golden file is stale; regenerate it");
    assert_eq!(envelopes, expected.len());
}

// Hand-written from the templates.

#[test]
fn field_encodings() {
    assert_eq!(
        to_json(&hashed(5, &[0xab, 0x01])).unwrap(),
        r#"{"1":5.0,"2":"AB01"}"#
    );
    assert_eq!(
        to_json(&hashed(1_700_000_000_000, &[])).unwrap(),
        r#"{"1":1.7E12,"2":""}"#
    );
    let update = ProtocolWaveletUpdate {
        wavelet_name: Some("example.com/w+a/~/conv+root".to_owned()),
        marker: Some(true),
        ..Default::default()
    };
    // Repeated fields are always present; unset optional fields are absent.
    assert_eq!(
        to_json(&update).unwrap(),
        r#"{"1":"example.com/w+a/~/conv+root","2":[],"6":true}"#
    );
    let document = DocumentSnapshot {
        document_id: Some("b+1".to_owned()),
        document_operation: Some(ProtocolDocumentOperation::default()),
        author: Some("a@example.com".to_owned()),
        contributor: Vec::new(),
        last_modified_version: Some(7),
        last_modified_time: Some(1_700_000_000_123),
    };
    assert_eq!(
        to_json(&document).unwrap(),
        r#"{"1":"b+1","2":{"1":[]},"3":"a@example.com","4":[],"5":[7,0],"6":[-807049093,395]}"#
    );
}

#[test]
fn envelope_shape() {
    let text = envelope(3, &RpcFinished::failure("no \"way\"")).unwrap();
    assert_eq!(
        text,
        r#"{"messageType":"RpcFinished","sequenceNumber":3,"message":{"1":true,"2":"no \"way\""}}"#
    );
    let parsed = Envelope::parse(&text).unwrap();
    assert_eq!(parsed.message_type, "RpcFinished");
    assert_eq!(parsed.sequence_number, 3);
    assert_eq!(
        parsed.body::<RpcFinished>().unwrap(),
        RpcFinished::failure("no \"way\"")
    );
}

#[test]
fn reading_is_lenient_where_the_original_is() {
    // Plain integers and exponent forms for int52, lower-case hex, unknown
    // keys, null for absent, unsigned low word.
    let version: ProtocolHashedVersion =
        from_json(r#"{"1":12,"2":"ab0F","99":{"x":1},"3":null}"#).unwrap();
    assert_eq!(version, hashed(12, &[0xab, 0x0f]));
    let version: ProtocolHashedVersion = from_json(r#"{"1":1.7E12,"2":""}"#).unwrap();
    assert_eq!(version, hashed(1_700_000_000_000, &[]));
    let response: ProtocolSubmitResponse = from_json(r#"{"1":2.0,"2":null}"#).unwrap();
    assert_eq!(response.operations_applied, Some(2));
    assert_eq!(response.error_message, None);
    let document: DocumentSnapshot =
        from_json(r#"{"1":"d","2":{"1":[]},"3":"a@b.c","5":[4294967295,0],"6":[-1,-1]}"#).unwrap();
    assert_eq!(document.last_modified_version, Some(4_294_967_295));
    assert_eq!(document.last_modified_time, Some(-1));
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    let bad = [
        "",
        "nonsense",
        "[]",
        "3",
        r#"{"1":"five","2":""}"#,
        r#"{"1":5.5,"2":""}"#,
        r#"{"1":1e300,"2":""}"#,
        r#"{"1":5,"2":"ABC"}"#,
        r#"{"1":5,"2":"zz"}"#,
        r#"{"1":5,"2":7}"#,
        r#"{"1":[5,0],"2":""}"#,
        // Missing required fields.
        r#"{"1":5}"#,
        "{}",
    ];
    for text in bad {
        assert!(
            from_json::<ProtocolHashedVersion>(text).is_err(),
            "accepted {text}"
        );
    }
    for text in [
        r#"{"1":"d","2":{"1":[]},"3":"a","5":[1],"6":[0,0]}"#,
        r#"{"1":"d","2":{"1":[]},"3":"a","5":7,"6":[0,0]}"#,
        r#"{"1":"d","2":{"1":[]},"3":"a","5":[0,4294967296],"6":[0,0]}"#,
        r#"{"1":"d","2":{"1":{}},"3":"a","5":[0,0],"6":[0,0]}"#,
        r#"{"1":"d","2":{"1":[{"5":2147483648}]},"3":"a","5":[0,0],"6":[0,0]}"#,
        r#"{"1":"d","2":{"1":[{"4":"yes"}]},"3":"a","5":[0,0],"6":[0,0]}"#,
    ] {
        assert!(
            from_json::<DocumentSnapshot>(text).is_err(),
            "accepted {text}"
        );
    }
    for text in [
        r#"{"sequenceNumber":1,"message":{}}"#,
        r#"{"messageType":"X","message":{}}"#,
        r#"{"messageType":"X","sequenceNumber":"1","message":{}}"#,
        r#"{"messageType":"X","sequenceNumber":4294967296,"message":{}}"#,
        r#"{"messageType":7,"sequenceNumber":1,"message":{}}"#,
        "[1,2,3]",
    ] {
        assert!(Envelope::parse(text).is_err(), "accepted {text}");
    }
    // A missing body is an error for a message with required fields only.
    let envelope = Envelope::parse(r#"{"messageType":"X","sequenceNumber":1}"#).unwrap();
    assert!(envelope.body::<ProtocolAuthenticate>().is_err());
}

#[test]
fn error_paths_name_the_field() {
    let error = from_json::<ProtocolSubmitRequest>(
        r#"{"1":"n","2":{"1":{"1":0,"2":""},"2":"a@b.c","3":[{"3":{"1":"d","2":{"1":[{"5":"x"}]}}}],"4":[]}}"#,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "field 2.3.3.2.1.5: expected a 32-bit integer"
    );
}

#[test]
fn unpaired_surrogates_are_rejected() {
    for text in [
        r#"{"1":"\ud83d"}"#,
        r#"{"1":"\ude00"}"#,
        r#"{"1":"a\ud83dz"}"#,
        r#"{"1":"\ud83dA"}"#,
        r#"{"1":"\ude00\ud83d"}"#,
    ] {
        assert!(
            matches!(
                from_json::<ProtocolAuthenticate>(text),
                Err(JsonError::Syntax(_))
            ),
            "accepted {text}"
        );
    }
    // Even in a part of the envelope that is not looked at.
    assert!(
        Envelope::parse(
            r#"{"messageType":"ProtocolAuthenticate","sequenceNumber":1,"message":{"9":"\udc00"}}"#
        )
        .is_err()
    );
    // A proper pair is fine.
    let ok: ProtocolAuthenticate = from_json(r#"{"1":"😀"}"#).unwrap();
    assert_eq!(ok.token.as_deref(), Some("\u{1F600}"));
}

#[test]
fn double_formatting_follows_java() {
    let cases = [
        (0, "0.0"),
        (9_999_999, "9999999.0"),
        (10_000_000, "1.0E7"),
        (12_345_678, "1.2345678E7"),
        (-10_000_000, "-1.0E7"),
        (120_000_000_000, "1.2E11"),
        (9_007_199_254_740_993, "9.007199254740992E15"),
    ];
    for (value, expected) in cases {
        let mut out = String::new();
        write_int52(value, &mut out);
        assert_eq!(out, expected);
    }
}
