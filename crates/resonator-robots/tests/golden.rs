//! Differential tests against the original Java implementation.
//!
//! `testdata/api_golden.txt` is written by `testdata/ApiGoldenGenerator.java`
//! from the reference classes: the Gson serializers, the event data
//! converters with `ApiView` and `ElementSerializer`, the operation
//! services and the `EventGenerator`. The record format is described in
//! the generator.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use resonator_model::conversation::Conversation;
use resonator_model::data::WaveletData;
use resonator_model::doc::DocView;
use resonator_model::docop::{
    AnnotationBoundaryMap, AnnotationChange, AttributeUpdate, Attributes, AttributesUpdate, DocOp,
    DocOpBuilder,
};
use resonator_model::id::{IdGeneratorImpl, ParticipantId, WaveId, WaveletId};
use resonator_model::operation::{
    TransformedWaveletDelta, WaveletOperation, WaveletOperationContext, WaveletOperationKind,
};
use resonator_model::version::HashedVersion;
use resonator_robots::api::{
    ApiWaveletId, BlipData, BlipThread, ByteArrays, Capability, Context, EventMessageBundle,
    EventType, FetchProfilesResult, JsonRpcResponse, OperationRequest, OperationType,
    ParamsProperty, ProtocolVersion, RawAttachmentData, RobotName, SearchResult,
    WaveletData as ApiWaveletData, deserialize_operations,
};
use resonator_robots::convert::EventDataConverter;
use resonator_robots::events::{EventGenerator, WaveletAndDeltas};
use resonator_robots::ops::{BoundWavelet, OperationExecutor, Registry};
use resonator_robots::view::{ApiView, AttachmentContext};
use resonator_server::clock::ManualClock;
use serde_json::Value;

const GOLDEN: &str = include_str!("../testdata/api_golden.txt");
const TIME: i64 = 1234567890123;

/// Undoes the generator's `line()` escaping.
fn unescape(field: &str) -> String {
    let mut units: Vec<u16> = Vec::new();
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            assert_eq!(Some('u'), chars.next());
            let hex: String = chars.by_ref().take(4).collect();
            units.push(u16::from_str_radix(&hex, 16).unwrap());
        } else {
            let mut buffer = [0u16; 2];
            units.extend_from_slice(c.encode_utf16(&mut buffer));
        }
    }
    String::from_utf16(&units).unwrap()
}

fn records() -> Vec<Vec<String>> {
    GOLDEN
        .lines()
        .map(|line| line.split('\t').map(unescape).collect())
        .collect()
}

fn opt(part: &str) -> Option<&str> {
    (part != "\u{2}").then_some(part)
}

fn attributes(parts: &[&str]) -> Attributes {
    Attributes::from_pairs(parts.chunks(2).map(|pair| (pair[0], pair[1]))).unwrap()
}

/// A document operation from the components of an OP or D record.
fn doc_op(components: &[String]) -> DocOp {
    let mut builder = DocOpBuilder::new();
    for component in components {
        let parts: Vec<&str> = component.split('\u{1}').collect();
        builder = match parts[0] {
            "r" => builder.retain(parts[1].parse().unwrap()),
            "c" => builder.characters(parts[1]),
            "s" => builder.element_start(parts[1], attributes(&parts[2..])),
            "e" => builder.element_end(),
            "dc" => builder.delete_characters(parts[1]),
            "ds" => builder.delete_element_start(parts[1], attributes(&parts[2..])),
            "de" => builder.delete_element_end(),
            "ra" => {
                let old_size: usize = parts[1].parse().unwrap();
                let (old, new) = parts[2..].split_at(old_size * 2);
                builder.replace_attributes(attributes(old), attributes(new))
            }
            "ua" => builder.update_attributes(
                AttributesUpdate::from_sorted(
                    parts[1..]
                        .chunks(3)
                        .map(|u| AttributeUpdate::new(u[0], opt(u[1]), opt(u[2])))
                        .collect(),
                )
                .unwrap(),
            ),
            "ab" => {
                let end_size: usize = parts[1].parse().unwrap();
                let (ends, changes) = parts[2..].split_at(end_size);
                builder.annotation_boundary(
                    AnnotationBoundaryMap::new(
                        ends.iter().map(|key| (*key).to_owned()).collect(),
                        changes
                            .chunks(3)
                            .map(|c| AnnotationChange::new(c[0], opt(c[1]), opt(c[2])))
                            .collect(),
                    )
                    .unwrap(),
                )
            }
            other => panic!("unknown component {other}"),
        };
    }
    builder.build().unwrap()
}

fn participant(address: &str) -> ParticipantId {
    ParticipantId::of(address).unwrap()
}

fn participants(list: &str) -> Vec<ParticipantId> {
    list.split(' ')
        .filter(|address| !address.is_empty())
        .map(participant)
        .collect()
}

/// The wavelet operation of an OP record, in a throwaway context.
fn wavelet_op(record: &[String], author: &ParticipantId) -> WaveletOperation {
    let context = WaveletOperationContext::new(author.clone(), TIME, 1);
    match record[1].as_str() {
        "add" => WaveletOperation::add_participant(context, participant(&record[2])),
        "remove" => WaveletOperation::remove_participant(context, participant(&record[2])),
        "noop" => WaveletOperation::no_op(context),
        "doc" => WaveletOperation::blip_content(context, &record[2], doc_op(&record[3..])),
        other => panic!("unknown op {other}"),
    }
}

/// The wavelet of a W record and the D records after it.
fn wavelet(w: &[String], docs: &[&Vec<String>]) -> WaveletData {
    let version: u64 = w[3].parse().unwrap();
    let mut data = WaveletData::new(
        WaveletId::of("example.com", "conv+root").unwrap(),
        participant(&w[1]),
        w[2].parse().unwrap(),
        version,
        HashedVersion::unsigned(version),
        w[4].parse().unwrap(),
        WaveId::of("example.com", "w+wave").unwrap(),
    );
    for p in participants(&w[5]) {
        data.add_participant(p);
    }
    for doc in docs {
        let content = doc_op(&doc[6..]).try_into().unwrap();
        data.create_document(
            &doc[1],
            participant(&doc[2]),
            participants(&doc[3]),
            &content,
            doc[4].parse().unwrap(),
            // Documents that are not blips keep the original's "no
            // version", -1.
            doc[5].parse::<i64>().unwrap().max(0) as u64,
        )
        .unwrap();
    }
    data
}

/// The wavelet states of the golden file by name.
fn states(records: &[Vec<String>]) -> BTreeMap<String, WaveletData> {
    let mut states = BTreeMap::new();
    let mut i = 0;
    while i < records.len() {
        if records[i][0] == "B" {
            let name = records[i][1].clone();
            let w = &records[i + 1];
            assert_eq!("W", w[0]);
            let docs: Vec<&Vec<String>> = records[i + 2..]
                .iter()
                .take_while(|record| record[0] == "D")
                .collect();
            states.insert(name, wavelet(w, &docs));
        }
        i += 1;
    }
    states
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("bad JSON {text}: {e}"))
}

/// Removes what the typed model does not tell apart: null, an empty list
/// and an empty object, as the value of an object's property.
fn lenient(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), lenient(value)))
                .filter(|(_, value)| match value {
                    Value::Null => false,
                    Value::Array(items) => !items.is_empty(),
                    Value::Object(map) => !map.is_empty(),
                    _ => true,
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(lenient).collect()),
        other => other.clone(),
    }
}

fn assert_json(expected: &Value, actual: &Value, what: &str) {
    assert!(
        expected == actual,
        "{what}\n expected: {expected}\n   actual: {actual}"
    );
}

fn to_value(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

fn golden_json(records: &[Vec<String>], name: &str) -> (String, String) {
    let record = records
        .iter()
        .find(|record| record[0] == "J" && record[2] == name)
        .unwrap_or_else(|| panic!("no J record {name}"));
    (record[1].clone(), record[3].clone())
}

#[test]
fn requests_read_and_written_like_gson() {
    let records = records();
    let (_, text) = golden_json(&records, "requests");
    let golden = json(&text);
    let requests = deserialize_operations(&text).unwrap();
    assert_eq!(golden.as_array().unwrap().len(), requests.len());
    // Every parameter went through its typed form on the way in.
    assert_json(
        &lenient(&golden),
        &lenient(&to_value(&requests)),
        "requests",
    );
    assert_eq!(OperationType::RobotNotify, requests[0].operation_type());
    assert_eq!(
        Some("proxy".to_owned()),
        requests[1].string(ParamsProperty::ProxyingFor)
    );
    assert_eq!(
        Some(vec![0, 1, 2, 255]),
        requests
            .iter()
            .find(|r| r.id == "op10")
            .unwrap()
            .bytes(ParamsProperty::FromVersion)
    );
    // A single request is read like a list of one.
    let first = golden.as_array().unwrap()[0].to_string();
    assert_eq!(
        vec![requests[0].clone()],
        deserialize_operations(&first).unwrap()
    );

    // With the namespace active API clients use.
    let (_, namespaced) = golden_json(&records, "requests-namespaced");
    let namespaced = json(&namespaced);
    let written: Vec<Value> = requests[..3]
        .iter()
        .map(|r| r.to_value_with_namespace("wave"))
        .collect();
    assert_json(
        &lenient(&namespaced),
        &lenient(&Value::Array(written)),
        "namespaced requests",
    );
    let read = deserialize_operations(&namespaced.to_string()).unwrap();
    assert_eq!("wave.robot.notify", read[0].method);
    assert_eq!(OperationType::RobotNotify, read[0].operation_type());
    assert_eq!(OperationType::WaveletAppendBlip, read[2].operation_type());

    // JSON as other clients write it: numbers as strings, unknown
    // properties, missing fields.
    let (_, foreign) = golden_json(&records, "foreign");
    let (_, foreign_read) = golden_json(&records, "foreign-read");
    let read = deserialize_operations(&foreign).unwrap();
    assert_json(
        &lenient(&json(&foreign_read)),
        &lenient(&to_value(&read)),
        "foreign requests",
    );
    assert_eq!("7", read[0].id);
    assert_eq!(Some(3), read[0].integer(ParamsProperty::Index));
    assert_eq!(None, read[0].blip_id());
}

#[test]
fn responses_read_and_written_like_gson() {
    let records = records();
    let (_, text) = golden_json(&records, "responses");
    let golden = json(&text);
    let responses: Vec<JsonRpcResponse> = serde_json::from_str(&text).unwrap();
    assert_json(&golden, &to_value(&responses), "responses");
    assert_eq!(Some("Something <went> wrong"), responses[0].error_message());
    assert!(!responses[1].is_error());
    assert_eq!(
        Some("b+new"),
        responses[2].string(ParamsProperty::NewBlipId)
    );
    // The data, through the typed model and back.
    fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(
        response: &JsonRpcResponse,
        property: ParamsProperty,
    ) {
        let raw = response.get(property).unwrap();
        let typed: T = response
            .typed(property)
            .unwrap_or_else(|| panic!("{property} does not parse: {raw}"));
        assert_json(&lenient(raw), &lenient(&to_value(&typed)), property.name());
    }
    round_trip::<ApiWaveletData>(&responses[3], ParamsProperty::WaveletData);
    round_trip::<BTreeMap<String, BlipData>>(&responses[3], ParamsProperty::Blips);
    round_trip::<BTreeMap<String, BlipThread>>(&responses[3], ParamsProperty::Threads);
    round_trip::<Vec<String>>(&responses[4], ParamsProperty::ParticipantsAdded);
    round_trip::<SearchResult>(&responses[5], ParamsProperty::SearchResults);
    round_trip::<FetchProfilesResult>(&responses[6], ParamsProperty::FetchProfilesResult);
    round_trip::<Vec<ApiWaveletId>>(&responses[7], ParamsProperty::WaveletIds);
    round_trip::<ByteArrays>(&responses[8], ParamsProperty::RawDeltas);
    round_trip::<RawAttachmentData>(&responses[9], ParamsProperty::AttachmentData);
    let search: SearchResult = responses[5].typed(ParamsProperty::SearchResults).unwrap();
    assert_eq!(2, search.num_results);
    assert_eq!(Some("title"), search.digests[0].title.as_deref());
    let deltas: ByteArrays = responses[8].typed(ParamsProperty::RawDeltas).unwrap();
    assert_eq!(vec![vec![1, 2, 3], vec![]], deltas.0);
}

#[test]
fn bundles_read_and_written_like_gson() {
    let records = records();
    let (_, text) = golden_json(&records, "bundle");
    let golden = json(&text);
    let bundle: EventMessageBundle = serde_json::from_str(&text).unwrap();
    assert_json(&lenient(&golden), &lenient(&to_value(&bundle)), "bundle");
    assert_eq!(17, bundle.events.len());
    assert_eq!(Some("proxy"), bundle.proxying_for.as_deref());
    // Events are written exactly as by the original, nulls included.
    assert_json(&golden["events"][0], &to_value(&bundle.events[0]), "event");
    assert_json(
        &golden["events"][15],
        &to_value(&bundle.events[15]),
        "event",
    );
    assert_json(
        &golden["events"][16],
        &to_value(&bundle.events[16]),
        "event",
    );
    let types: Vec<&str> = bundle
        .events
        .iter()
        .map(|event| event.event_type().as_str())
        .collect();
    let golden_types: Vec<&str> = golden["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(golden_types, types);

    // Protocol 0.2 writes attachments as images.
    let (_, old) = golden_json(&records, "bundle-0.2");
    assert_json(
        &lenient(&json(&old)),
        &lenient(&to_value(&bundle.clone().for_protocol(ProtocolVersion::V2))),
        "bundle for protocol 0.2",
    );
    assert_json(
        &lenient(&golden),
        &lenient(&to_value(&bundle.for_protocol(ProtocolVersion::V2_1))),
        "bundle for protocol 0.21",
    );

    let (_, empty) = golden_json(&records, "bundle-empty");
    let bundle = EventMessageBundle::new("robot@example.com", "");
    assert_json(&json(&empty), &to_value(&bundle), "empty bundle");
}

#[test]
fn wavelets_and_blips_converted_like_the_original() {
    let records = records();
    let states = states(&records);
    let mut i = 0;
    let mut checked = 0;
    while i < records.len() {
        if records[i][0] != "B" || !["simple", "rich", "elements"].contains(&records[i][1].as_str())
        {
            i += 1;
            continue;
        }
        let name = &records[i][1];
        let wavelet = &states[name];
        let conversation = Conversation::read(wavelet).unwrap();
        let attachments = AttachmentContext::of(wavelet);
        let mut bundles: BTreeMap<&str, EventMessageBundle> = BTreeMap::new();
        i += 1;
        while i < records.len() && records[i][0] != "B" && records[i][0] != "SVC" {
            let record = &records[i];
            i += 1;
            match record[0].as_str() {
                "V" => {
                    let converter = match record[1].as_str() {
                        "V21" => EventDataConverter::V21,
                        _ => EventDataConverter::V22,
                    };
                    let bundle = bundles.entry(record[1].as_str()).or_default();
                    let what = format!("{name} {} {} {}", record[1], record[2], record[3]);
                    match record[2].as_str() {
                        "W" => assert_json(
                            &json(&record[3]),
                            &to_value(&converter.to_wavelet_data(wavelet, &conversation)),
                            &what,
                        ),
                        "B" => {
                            let blip = conversation.blip(&record[3]).unwrap();
                            let data = converter.to_blip_data(blip, &conversation, bundle);
                            assert_json(&json(&record[4]), &to_value(&data), &what);
                        }
                        "T" => assert_json(&json(&record[3]), &to_value(&bundle.threads), &what),
                        other => panic!("unknown V record {other}"),
                    }
                    checked += 1;
                }
                "A" | "AX" | "AT" => {
                    let doc = wavelet.document(&record[1]).unwrap().content();
                    let doc_view = DocView::new(doc);
                    let view = ApiView::new(&doc_view, &attachments);
                    let numbers = |length: usize, f: &dyn Fn(usize) -> usize| -> String {
                        (0..length)
                            .map(|i| f(i).to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    let actual = match record[0].as_str() {
                        "A" => view.api_contents(),
                        "AX" => numbers(view.len() + 2, &|i| {
                            view.transform_to_xml_offset(i).unwrap()
                        }),
                        _ => numbers(doc_view.size() + 1, &|i| view.transform_to_text_offset(i)),
                    };
                    assert_eq!(record[2], actual, "{name} {} {}", record[0], record[1]);
                    checked += 1;
                }
                _ => {}
            }
        }
    }
    assert!(checked > 50, "only {checked} records checked");
}

fn executor(fixture: &common::Fixture, seed: &'static str) -> OperationExecutor {
    OperationExecutor::new(fixture.server.clone())
        .with_clock(Arc::new(ManualClock::new(TIME + 5000)))
        .with_id_generator(IdGeneratorImpl::new("example.com", move || seed.to_owned()))
}

/// Scenarios in which this port knowingly does something else than the
/// original, with what is expected instead.
fn service_deviation(name: &str, end: &str) -> Option<&'static str> {
    match (name, end) {
        // The original cannot insert elements other than gadgets and form
        // elements.
        ("insert-image", "EXC") => Some("ok"),
        _ => None,
    }
}

/// Drops the fields that the generator's wavelet advances with every
/// operation and a real batch does not.
fn without_versions(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !["version", "lastModifiedTime"].contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), without_versions(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_versions).collect()),
        other => other.clone(),
    }
}

#[tokio::test]
async fn operations_executed_like_the_original() {
    let records = records();
    let states = states(&records);
    let fixture = common::Fixture::new();
    let mut i = 0;
    let mut scenarios = 0;
    while i < records.len() {
        if records[i][0] != "SVC" {
            i += 1;
            continue;
        }
        let name = records[i][1].clone();
        let base = &states[&records[i][2]];
        let actor = participant(&records[i][3]);
        // The operations of the scenario, with what each emitted and how
        // it ended.
        let mut requests = Vec::new();
        let mut expected_ops = Vec::new();
        let mut ends: Vec<Vec<String>> = Vec::new();
        while i < records.len() && records[i][0] == "SVC" && records[i][1] == name {
            requests.push(OperationRequest::from_value(&json(&records[i][4])).unwrap());
            i += 1;
            while records[i][0] == "OP" {
                expected_ops.push(wavelet_op(&records[i], &actor));
                i += 1;
            }
            ends.push(records[i].clone());
            i += 1;
        }
        let executor = executor(&fixture, "svc");
        let applied = executor
            .apply(
                &actor,
                Registry::DataApi,
                &requests,
                Some(BoundWavelet {
                    snapshot: base.clone(),
                }),
            )
            .await;
        let actual_ops: Vec<WaveletOperation> = applied
            .deltas
            .iter()
            .flat_map(|pending| pending.delta.ops().to_vec())
            .collect();
        let describe = |ops: &[WaveletOperation]| {
            ops.iter()
                .map(|op| format!("  {op}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        // Where the original iterates a hash map of properties, the order
        // of the elements it makes is its own; compare those as sets.
        let hash_ordered =
            ["insert-gadget", "update-gadget", "delete-root-blip"].contains(&name.as_str());
        let same = if hash_ordered {
            // The same components, retains aside, in any order.
            let components = |ops: &[WaveletOperation]| -> Vec<String> {
                let mut parts: Vec<String> = ops
                    .iter()
                    .flat_map(|op| {
                        op.to_string()
                            .trim_start_matches("document op: ")
                            .split("; ")
                            .filter(|part| !part.starts_with("__") && !part.contains(" on blip "))
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .collect();
                parts.sort();
                parts
            };
            expected_ops.len() == actual_ops.len()
                && components(&expected_ops) == components(&actual_ops)
        } else if name == "insert-image" {
            // An extension: the original refuses, this inserts the image.
            expected_ops.is_empty() && actual_ops.len() == 1
        } else {
            expected_ops == actual_ops
        };
        assert!(
            same,
            "{name}: operations differ\nexpected:\n{}\nactual:\n{}",
            describe(&expected_ops),
            describe(&actual_ops)
        );
        for (index, end) in ends.iter().enumerate() {
            let response = &applied.responses[index];
            let what = format!("{name} operation {index}");
            let kind = service_deviation(&name, &end[0]).unwrap_or(end[0].as_str());
            match kind {
                "RESP" => {
                    let expected = json(&end[1]);
                    let actual = to_value(response);
                    if name == "fetch-after-change" {
                        assert_json(
                            &without_versions(&expected),
                            &without_versions(&actual),
                            &what,
                        );
                    } else {
                        assert_json(&expected, &actual, &what);
                    }
                }
                // The original forgets to respond; here that is success.
                "NORESP" | "ok" => {
                    assert_eq!(
                        Some(&BTreeMap::new()),
                        response.data(),
                        "{what}: {response:?}"
                    )
                }
                "IRE" => assert_eq!(Some(end[1].as_str()), response.error_message(), "{what}"),
                // A crash of the original is an error here.
                "EXC" => assert!(response.is_error(), "{what}: {response:?}"),
                other => panic!("unknown end {other}"),
            }
        }
        scenarios += 1;
    }
    assert!(scenarios > 40, "only {scenarios} scenarios");
}

fn capabilities(spec: &str) -> BTreeMap<EventType, Capability> {
    spec.split(' ')
        .filter(|item| !item.is_empty())
        .map(|item| {
            let mut parts = item.split(':');
            let event_type: EventType = parts.next().unwrap().parse().unwrap();
            let contexts: Vec<Context> = parts.map(|c| c.parse().unwrap()).collect();
            (event_type, Capability::with_contexts(event_type, contexts))
        })
        .collect()
}

/// The original stamps every event with zero, having no times on its wave
/// bus; only button clicks carry their own time.
fn without_timestamps(bundle: &Value) -> Value {
    let mut bundle = bundle.clone();
    if let Some(events) = bundle["events"].as_array_mut() {
        for event in events {
            if event["type"] != "FORM_BUTTON_CLICKED" {
                event["timestamp"] = Value::from(0);
            }
        }
    }
    bundle
}

#[test]
fn events_generated_like_the_original() {
    let records = records();
    let states = states(&records);
    let mut i = 0;
    let mut scenarios = 0;
    while i < records.len() {
        if records[i][0] != "EV" {
            i += 1;
            continue;
        }
        let record = &records[i];
        let name = &record[1];
        let after = &states[&record[2]];
        let robot = RobotName::from_address(&record[3]).unwrap();
        let version: ProtocolVersion = record[4].parse().unwrap();
        let capabilities = capabilities(&record[5]);
        i += 1;
        let mut deltas = Vec::new();
        while records[i][0] == "DELTA" {
            let author = participant(&records[i][1]);
            let resulting_version: u64 = records[i][2].parse().unwrap();
            let timestamp: i64 = records[i][3].parse().unwrap();
            i += 1;
            let mut ops = Vec::new();
            while records[i][0] == "OP" {
                ops.push(wavelet_op(&records[i], &author));
                i += 1;
            }
            deltas.push(
                TransformedWaveletDelta::clone_operations(
                    author,
                    HashedVersion::unsigned(resulting_version),
                    timestamp,
                    &ops,
                )
                .unwrap(),
            );
        }
        assert_eq!("BUNDLE", records[i][0], "{name}");
        let expected = json(&records[i][1]);
        let wavelet_and_deltas = WaveletAndDeltas::create(after, deltas).unwrap();
        let bundle = EventGenerator::new(robot)
            .generate_events(
                &wavelet_and_deltas,
                &capabilities,
                EventDataConverter::for_version(version),
            )
            .unwrap()
            .for_protocol(version);
        assert_json(
            &expected,
            &without_timestamps(&to_value(&bundle)),
            &format!("events of {name}"),
        );
        scenarios += 1;
    }
    assert!(scenarios >= 18, "only {scenarios} scenarios");
}

/// The inverse of an operation undoes it, down to the participants' order
/// being the only thing that may differ.
#[test]
fn rollback_restores_the_content() {
    let records = records();
    let states = states(&records);
    let simple = &states["simple"];
    let after = &states["after-added-then-changes"];
    // The scenario's deltas, read again.
    let start = records
        .iter()
        .position(|r| r[0] == "EV" && r[1] == "added-then-changes")
        .unwrap();
    let mut deltas = Vec::new();
    let mut i = start + 1;
    while records[i][0] == "DELTA" {
        let author = participant(&records[i][1]);
        let version: u64 = records[i][2].parse().unwrap();
        let timestamp: i64 = records[i][3].parse().unwrap();
        i += 1;
        let mut ops = Vec::new();
        while records[i][0] == "OP" {
            ops.push(wavelet_op(&records[i], &author));
            i += 1;
        }
        deltas.push(
            TransformedWaveletDelta::clone_operations(
                author,
                HashedVersion::unsigned(version),
                timestamp,
                &ops,
            )
            .unwrap(),
        );
    }
    let wavelet_and_deltas = WaveletAndDeltas::create(after, deltas).unwrap();
    let before = wavelet_and_deltas.snapshot_before_deltas();
    assert_eq!(simple.version(), before.version());
    assert_eq!(simple.participants(), before.participants());
    for doc in simple.documents() {
        assert_eq!(
            doc.content().to_xml_string(),
            before.document(doc.id()).unwrap().content().to_xml_string()
        );
    }
    assert!(matches!(
        wavelet_and_deltas.deltas()[0].ops()[0].kind(),
        WaveletOperationKind::Blip { .. }
    ));
}
