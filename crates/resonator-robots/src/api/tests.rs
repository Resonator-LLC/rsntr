//! Ports of `RobotNameTest`, `OperationRequestGsonAdaptorRobotTest`,
//! `JsonRpcResponseGsonAdaptorRobotTest` and `EventSerializerRobotTest`,
//! and tests of the JSON model. The JSON shapes themselves are checked
//! against the original in `tests/golden.rs`.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;

#[test]
fn robot_name_well_formed_address() {
    for bad in [
        "foo",
        "bar.com",
        "@bar.com",
        "foo@",
        "fo o@bar.com",
        "foo@bar_x.com",
    ] {
        assert!(!RobotName::is_well_formed_address(bad), "{bad}");
    }
    for good in [
        "foo@bar.com",
        "foo#1@bar.com",
        "foo+wave#1@bar.com",
        "foo+wave+foo#1@bar.com",
        // Not in the original: a domain without a top-level domain.
        "foo@localhost",
    ] {
        assert!(RobotName::is_well_formed_address(good), "{good}");
    }
}

#[test]
fn robot_name_parsing() {
    let name = RobotName::from_address("robot@appspot.com").unwrap();
    assert_eq!(("robot", "appspot.com"), (name.id(), name.domain()));
    assert!(!name.has_proxy_for() && !name.has_version());
    let name = RobotName::from_address("robot+proxy#version@appspot.com").unwrap();
    assert_eq!(
        ("robot", "appspot.com", "proxy", "version"),
        (name.id(), name.domain(), name.proxy_for(), name.version())
    );
    assert_eq!(None, RobotName::from_address("foo"));
}

#[test]
fn robot_name_to_address() {
    let address = |a: &str| RobotName::from_address(a).unwrap();
    assert_eq!(
        "robot@appspot.com",
        address("robot@appspot.com").to_participant_address()
    );
    assert_eq!(
        "robot+id@appspot.com",
        address("robot+id@appspot.com").to_participant_address()
    );
    assert_eq!(
        "robot+id#1@appspot.com",
        address("robot+id#1@appspot.com").to_participant_address()
    );
    assert_eq!(
        "robot@appspot.com",
        address("robot#1@appspot.com").to_email_address()
    );
    assert_eq!(
        "robot#1@appspot.com",
        address("robot#1@appspot.com").to_email_address_with_version()
    );
    assert_eq!(
        "robot@appspot.com",
        address("robot+proxy#1@appspot.com").to_email_address()
    );
    assert_eq!(
        "robot#1@appspot.com",
        address("robot+proxy#1@appspot.com").to_email_address_with_version()
    );
    let built = RobotName::new("robot", "example.com")
        .with_proxy_for("p")
        .with_version("2");
    assert_eq!("robot+p#2@example.com", built.to_string());
}

/// `OperationRequestGsonAdaptorRobotTest.testDeserialize`.
#[test]
fn request_deserialize() {
    let text = r#"{"id":"op1","method":"wavelet.setTitle","params":{
        "waveId":"1","waveletId":"2","waveletTitle":"Title","unknown":"value"}}"#;
    let request: OperationRequest = serde_json::from_str(text).unwrap();
    assert_eq!("op1", request.id);
    assert_eq!("wavelet.setTitle", request.method);
    assert_eq!(Some("1".to_owned()), request.wave_id());
    assert_eq!(Some("2".to_owned()), request.wavelet_id());
    assert_eq!(None, request.blip_id());
    assert_eq!(3, request.params().count());
    assert_eq!(
        Some("Title".to_owned()),
        request.string(ParamsProperty::WaveletTitle)
    );
    assert_eq!(OperationType::WaveletSetTitle, request.operation_type());
}

#[test]
fn request_validation() {
    let missing = deserialize_operations(r#"{"id":"op1","params":{}}"#).unwrap_err();
    assert!(
        missing
            .message
            .starts_with("Missing required properties methodoperation: "),
        "{missing}"
    );
    assert!(
        deserialize_operations("[1]")
            .unwrap_err()
            .message
            .starts_with("The incoming JSON is not a JSON object")
    );
    assert!(
        deserialize_operations("{not json")
            .unwrap_err()
            .message
            .starts_with("Couldn't deserialize incoming operations")
    );
    assert_eq!(
        Vec::<OperationRequest>::new(),
        deserialize_operations("  ").unwrap()
    );
    assert_eq!(
        Vec::<OperationRequest>::new(),
        deserialize_operations("[]").unwrap()
    );
    // A known parameter of the wrong type fails the request, as Gson does.
    for bad in [
        r#"{"index":"x"}"#,
        r#"{"index":1.5}"#,
        r#"{"range":[1,2]}"#,
        r#"{"blipData":"text"}"#,
        r#"{"modifyAction":{"modifyHow":"NOPE"}}"#,
        r#"{"element":{"type":"nope","properties":{}}}"#,
        r#"{"waveId":{}}"#,
    ] {
        let text = format!(r#"{{"id":"a","method":"m","params":{bad}}}"#);
        assert!(deserialize_operations(&text).is_err(), "{bad}");
    }
}

#[test]
fn request_parameters() {
    let mut request = OperationRequest::for_blip(
        OperationType::DocumentModify,
        "op1",
        "example.com/w+1",
        "example.com/conv+root",
        "b+1",
    )
    .with(ParamsProperty::Index, 3)
    .with(ParamsProperty::Range, Range::new(1, 2))
    .with(ParamsProperty::Message, Option::<String>::None);
    assert_eq!(Some(3), request.integer(ParamsProperty::Index));
    assert_eq!(Ok(3), request.required_integer(ParamsProperty::Index));
    assert_eq!(Some(Range::new(1, 2)), request.typed(ParamsProperty::Range));
    assert_eq!(None, request.raw(ParamsProperty::Message));
    // The messages of OperationUtil.getRequiredParameter.
    assert_eq!(
        "property MODIFY_ACTION not found",
        request
            .required::<DocumentModifyAction>(ParamsProperty::ModifyAction)
            .unwrap_err()
            .message
    );
    assert_eq!(
        "property WAVELET_TITLE not found",
        request
            .required_string(ParamsProperty::WaveletTitle)
            .unwrap_err()
            .message
    );
    request.set_bytes(ParamsProperty::FromVersion, &[1, 2, 250]);
    assert_eq!(
        Some(vec![1, 2, 250]),
        request.bytes(ParamsProperty::FromVersion)
    );
    assert!(request.remove(ParamsProperty::Index).is_some());
    assert_eq!(None, request.integer(ParamsProperty::Index));
    // The "wave." namespace of active API clients.
    let namespaced = OperationRequest::new("wave.blip.delete", "x");
    assert_eq!(OperationType::BlipDelete, namespaced.operation_type());
    assert_eq!(
        OperationType::Unknown,
        OperationRequest::new("no.such", "x").operation_type()
    );
    assert_eq!(
        json!({"id": "x", "method": "wave.blip.delete", "params": {}}),
        OperationRequest::of(OperationType::BlipDelete, "x").to_value_with_namespace("wave")
    );
}

#[test]
fn operation_types_and_properties() {
    for &operation in OperationType::ALL {
        assert_eq!(
            operation,
            OperationType::from_method_name(operation.method())
        );
    }
    assert_eq!(67, OperationType::ALL.len());
    for &property in ParamsProperty::ALL {
        assert_eq!(Some(property), ParamsProperty::from_key(property.key()));
    }
    assert_eq!(None, ParamsProperty::from_key("nope"));
    assert_eq!("WAVE_ID", ParamsProperty::WaveId.to_string());
    assert_eq!("proxyingFor", ParamsProperty::ProxyingFor.key());
}

#[test]
fn protocol_version_of_a_batch() {
    let notify = |version: Option<&str>| {
        let request = OperationRequest::of(OperationType::RobotNotify, "op1");
        match version {
            Some(version) => request.with(ParamsProperty::ProtocolVersion, version),
            None => request,
        }
    };
    assert_eq!(ProtocolVersion::DEFAULT, protocol_version_of(&[]));
    assert_eq!(
        ProtocolVersion::DEFAULT,
        protocol_version_of(&[notify(None)])
    );
    assert_eq!(
        ProtocolVersion::V2_1,
        protocol_version_of(&[notify(Some("0.21"))])
    );
    assert_eq!(
        ProtocolVersion::DEFAULT,
        protocol_version_of(&[notify(Some("9.9"))])
    );
    // Only a leading robot.notify counts.
    let other = OperationRequest::of(OperationType::RobotFetchWave, "op0")
        .with(ParamsProperty::ProtocolVersion, "0.21");
    assert_eq!(
        ProtocolVersion::DEFAULT,
        protocol_version_of(&[other, notify(Some("0.21"))])
    );
}

/// `JsonRpcResponseGsonAdaptorRobotTest`.
#[test]
fn response_deserialize() {
    let error: JsonRpcResponse =
        serde_json::from_str(r#"{"id":"op1","error":{"message":"Invalid id"}}"#).unwrap();
    assert!(error.is_error());
    assert_eq!("op1", error.id);
    assert_eq!(Some("Invalid id"), error.error_message());
    assert_eq!(None, error.data());

    let response: JsonRpcResponse =
        serde_json::from_str(r#"{"id":"op1","data":{"newBlipId":"blip_1","unknown":"value"}}"#)
            .unwrap();
    assert!(!response.is_error());
    assert_eq!(1, response.data().unwrap().len());
    assert_eq!(Some("blip_1"), response.string(ParamsProperty::NewBlipId));
    assert_eq!(
        json!({"id": "op1", "data": {"newBlipId": "blip_1"}}),
        serde_json::to_value(&response).unwrap()
    );
    assert_eq!(
        json!({"id": "x", "error": {"message": "m"}}),
        serde_json::to_value(JsonRpcResponse::error("x", "m")).unwrap()
    );
}

/// Every event type has a JSON form that reads back as the same event
/// (the original's test checks the reflection its serializer relies on).
#[test]
fn every_event_round_trips() {
    let wavelet = WaveletData::new("example.com/w+1", "example.com/conv+root", "b+1", None);
    let all = vec![
        EventProperties::WaveletBlipCreated {
            new_blip_id: Some("b+2".to_owned()),
        },
        EventProperties::WaveletBlipRemoved {
            removed_blip_id: Some("b+2".to_owned()),
        },
        EventProperties::WaveletParticipantsChanged {
            participants_added: vec!["a@example.com".to_owned()],
            participants_removed: vec![],
        },
        EventProperties::WaveletSelfAdded,
        EventProperties::WaveletSelfRemoved,
        EventProperties::WaveletTitleChanged {
            title: Some("title".to_owned()),
        },
        EventProperties::WaveletCreated {
            message: None,
            wave_id: Some("example.com/w+1".to_owned()),
            wavelet_id: Some("example.com/conv+root".to_owned()),
        },
        EventProperties::WaveletFetched {
            message: Some("msg".to_owned()),
            wavelet_data: Some(Box::new(wavelet)),
            blips: BTreeMap::from([("b+1".to_owned(), BlipData::default())]),
            threads: BTreeMap::from([("t".to_owned(), BlipThread::new("t", 3, vec![]))]),
        },
        EventProperties::WaveletTagsChanged,
        EventProperties::BlipContributorsChanged {
            contributors_added: vec!["a@example.com".to_owned()],
            contributors_removed: vec!["b@example.com".to_owned()],
        },
        EventProperties::BlipSubmitted,
        EventProperties::DocumentChanged,
        EventProperties::FormButtonClicked {
            button_name: Some("button".to_owned()),
        },
        EventProperties::GadgetStateChanged {
            index: Some(4),
            old_state: BTreeMap::from([("key".to_owned(), Some("old".to_owned()))]),
        },
        EventProperties::AnnotatedTextChanged {
            name: Some("name".to_owned()),
            value: None,
        },
        EventProperties::OperationError {
            operation_id: Some("op1".to_owned()),
            message: Some("failed".to_owned()),
        },
    ];
    let mut types = Vec::new();
    for properties in all {
        let event = Event::new("alice@example.com", 5, Some("b+1"), properties);
        let value = event.to_value();
        assert_eq!(event.event_type().as_str(), value["type"]);
        assert_eq!("alice@example.com", value["modifiedBy"]);
        assert_eq!(5, value["timestamp"]);
        assert_eq!("b+1", value["properties"]["blipId"]);
        assert_eq!(event, Event::from_value(&value).unwrap(), "{value}");
        types.push(event.event_type());
    }
    types.sort();
    types.dedup();
    // All but UNKNOWN.
    assert_eq!(16, types.len());
    let unknown = json!({"type": "NO_SUCH_EVENT", "modifiedBy": "a", "timestamp": 1});
    assert!(Event::from_value(&unknown).is_err());
    // Type names are read ignoring case.
    let lower = json!({"type": "document_changed", "modifiedBy": "a", "timestamp": 1,
        "properties": {}});
    assert_eq!(
        EventType::DocumentChanged,
        Event::from_value(&lower).unwrap().event_type()
    );
}

#[test]
fn event_as_response_data() {
    let event = Event::new(
        "alice@example.com",
        5,
        Some("b+root"),
        EventProperties::WaveletCreated {
            message: None,
            wave_id: Some("w".to_owned()),
            wavelet_id: Some("wl".to_owned()),
        },
    );
    let data = event.response_data();
    assert_eq!(
        vec![
            ParamsProperty::WaveId,
            ParamsProperty::WaveletId,
            ParamsProperty::BlipId,
            ParamsProperty::Message
        ],
        data.keys().copied().collect::<Vec<_>>()
    );
    assert_eq!(Value::Null, data[&ParamsProperty::Message]);
    // Properties that are no response parameters are left out.
    let removed = Event::new(
        "alice@example.com",
        5,
        Some("b+root"),
        EventProperties::WaveletBlipRemoved {
            removed_blip_id: Some("b+1".to_owned()),
        },
    );
    assert_eq!(1, removed.response_data().len());
}

#[test]
fn elements() {
    let element: Element = serde_json::from_value(json!({
        "type": "gadget",
        "properties": {"url": "http://g/", "count": 3, "flag": true, "nothing": null}
    }))
    .unwrap();
    assert!(element.is_gadget());
    assert_eq!(Some("3"), element.property("count"));
    assert_eq!(Some("true"), element.property("flag"));
    assert_eq!(None, element.property("nothing"));
    assert!(element.has_property("nothing"));
    assert_eq!(' ', element.text());
    assert_eq!('\n', Element::line().text());
    assert!(Element::form(ElementType::Check, "n", "d", "v").is_form_element());
    assert!(!element.is_form_element());
    for element_type in ElementType::ALL {
        assert_eq!(
            Some(element_type),
            ElementType::from_name_ignore_case(&element_type.as_str().to_lowercase())
        );
    }
    // Protocol 0.2: attachments travel as images without a URL.
    let attachment = Element::with_properties(
        ElementType::Attachment,
        [
            ("attachmentId", "a"),
            ("caption", "c"),
            ("mimeType", "image/png"),
            ("attachmentUrl", "/a"),
            ("data", "AQID"),
        ],
    );
    let old = attachment.to_v2();
    assert_eq!(ElementType::Image, old.element_type);
    assert_eq!(
        vec!["attachmentId", "caption"],
        old.properties
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(ElementType::Attachment, old.from_v2().element_type);
    let image = Element::image("http://i/", 1, 2, "c");
    assert_eq!(image, image.to_v2().from_v2());
}

#[test]
fn blip_data_defaults() {
    let blip: BlipData = serde_json::from_str("{}").unwrap();
    assert_eq!(BlipData::default(), blip);
    assert_eq!("\n", blip.content);
    assert_eq!((-1, -1), (blip.version, blip.last_modified_time));
    let blip: BlipData = serde_json::from_value(json!({
        "content": null, "annotations": null, "elements": null, "contributors": null,
        "version": "7", "lastModifiedTime": 8.0
    }))
    .unwrap();
    assert_eq!("\n", blip.content);
    assert_eq!((7, 8), (blip.version, blip.last_modified_time));
    // A client's new blip starts with a newline.
    assert_eq!("\ntext", BlipData::new("w", "wl", "TBD_1", "text").content);
    assert_eq!(
        "\ntext",
        BlipData::new("w", "wl", "TBD_1", "\ntext").content
    );
    assert_eq!("\n", BlipData::new("w", "wl", "TBD_1", "").content);
    let wavelet: WaveletData = serde_json::from_str("{}").unwrap();
    assert_eq!(WaveletData::default(), wavelet);
    let query: DocumentModifyQuery = serde_json::from_str("{}").unwrap();
    assert_eq!(-1, query.max_res);
}

#[test]
fn modify_action_values_cycle() {
    let action = DocumentModifyAction::with_values(ModifyHow::Insert, ["a", "b"]);
    assert_eq!(
        vec![Some("a"), Some("b"), Some("a")],
        (0..3).map(|i| action.value(i)).collect::<Vec<_>>()
    );
    assert!(action.has_text_at(5));
    assert_eq!(None, action.element(0));
    let none = DocumentModifyAction::new(ModifyHow::Delete);
    assert!(!none.has_text_at(0));
    let elements = DocumentModifyAction::with_elements(
        ModifyHow::Insert,
        [Element::line(), Element::gadget("u")],
    );
    assert_eq!(
        ElementType::Gadget,
        elements.element(3).unwrap().element_type
    );
    assert_eq!(
        json!("INSERT_AFTER"),
        serde_json::to_value(ModifyHow::InsertAfter).unwrap()
    );
}

#[test]
fn base64_like_commons_codec() {
    assert_eq!("AQL6", base64_bytes::encode(&[1, 2, 250]));
    assert_eq!(vec![1, 2, 250], base64_bytes::decode("AQL6"));
    // Padding optional, URL-safe alphabet and line breaks accepted.
    assert_eq!(vec![1], base64_bytes::decode("AQ=="));
    assert_eq!(vec![1], base64_bytes::decode("AQ"));
    assert_eq!(vec![251, 255], base64_bytes::decode("-_8"));
    assert_eq!(vec![1, 2, 250], base64_bytes::decode("AQ\r\nL6"));
    assert_eq!(Vec::<u8>::new(), base64_bytes::decode(""));
    let arrays: ByteArrays = serde_json::from_str(r#"["AQ==",""]"#).unwrap();
    assert_eq!(vec![vec![1], vec![]], arrays.0);
}

#[test]
fn search_result_counts_digests() {
    let mut result = SearchResult::new("in:inbox");
    result.add_digest(SearchDigest {
        title: Some("t".to_owned()),
        snippet: None,
        wave_id: Some("example.com/w+1".to_owned()),
        last_modified: 1,
        created: 2,
        unread_count: 3,
        blip_count: 4,
        participants: vec![],
    });
    assert_eq!(1, result.num_results);
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(4, value["digests"][0]["blipCount"]);
    assert_eq!(Value::Null, value["digests"][0]["snippet"]);
}
