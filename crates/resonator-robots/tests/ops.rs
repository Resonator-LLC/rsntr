//! The operation executor against a real wave server on a temporary
//! database. Includes ports of the original's `OperationContextImplTest`,
//! `OperationUtilTest`, `OperationServiceRegistryTest`, `RobotWaveletTest`
//! and of the tests of the operation services; what the services do to
//! documents, operation for operation, is checked against the original in
//! `golden.rs`.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::*;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::WaveletOperationKind;
use resonator_model::proto::federation::ProtocolHashedVersion;
use resonator_model::proto::{
    decode, deserialize_hashed_version, encode, serialize_hashed_version,
};
use resonator_model::version::HashedVersionFactory;
use resonator_robots::api::*;
use resonator_robots::ops::*;
use resonator_server::account::{AccountData, HumanAccountData, RobotAccountData};
use resonator_server::waveserver::WaveletProvider;
use serde_json::{Value, json};

fn op(operation: OperationType, id: &str, wave: &Wave) -> OperationRequest {
    OperationRequest::for_wavelet(operation, id, &wave.wave_id, &wave.wavelet_id)
}

fn blip_op(operation: OperationType, id: &str, wave: &Wave, blip_id: &str) -> OperationRequest {
    OperationRequest::for_blip(operation, id, &wave.wave_id, &wave.wavelet_id, blip_id)
}

fn new_blip(wave: &Wave, temp_id: &str, content: &str) -> BlipData {
    new_blip_data(&wave.wave_id, &wave.wavelet_id, temp_id, content)
}

fn modify(wave: &Wave, id: &str, blip_id: &str, action: DocumentModifyAction) -> OperationRequest {
    blip_op(OperationType::DocumentModify, id, wave, blip_id)
        .with(ParamsProperty::ModifyAction, action)
}

struct Wave {
    wave_id: String,
    wavelet_id: String,
    root_blip: String,
    name: WaveletName,
}

async fn new_wave(executor: &OperationExecutor, creator: &ParticipantId, others: &[&str]) -> Wave {
    let (wave_id, wavelet_id, root_blip, name) = create_wave(executor, creator, others).await;
    Wave {
        wave_id,
        wavelet_id,
        root_blip,
        name,
    }
}

fn error_of(response: &JsonRpcResponse) -> &str {
    response
        .error_message()
        .unwrap_or_else(|| panic!("expected an error, got {response:?}"))
}

fn assert_ok(responses: &[JsonRpcResponse]) {
    for response in responses {
        assert!(!response.is_error(), "{response:?}");
    }
}

// ---- End to end ----

#[tokio::test]
async fn create_wave_with_operations() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let requests = vec![
        create_wavelet_request("op1", &["bob@example.com"]).with(ParamsProperty::Message, "hi"),
        // Later operations of the batch use the temporary ids.
        OperationRequest::for_blip(
            OperationType::DocumentModify,
            "op2",
            "example.com/TBD_wave",
            "example.com/conv+root",
            "TBD_blip",
        )
        .with(
            ParamsProperty::ModifyAction,
            DocumentModifyAction::with_values(ModifyHow::Insert, ["Hello"]),
        )
        .with(ParamsProperty::Index, 1),
        OperationRequest::for_wavelet(
            OperationType::WaveletSetTitle,
            "op3",
            "example.com/TBD_wave",
            "example.com/conv+root",
        )
        .with(ParamsProperty::WaveletTitle, "Greeting"),
    ];
    let result = executor
        .execute_detailed(&alice(), Registry::DataApi, &requests, None)
        .await;
    assert_ok(&result.responses);
    assert_eq!(
        json!({"id": "op1", "data": {
            "blipId": "b+sB", "message": "hi",
            "waveId": "example.com/w+sA", "waveletId": "example.com/conv+root"}}),
        to_json(&result.responses[0])
    );
    // setTitle answers with success (the original forgets to answer).
    assert_eq!(
        json!({"id": "op3", "data": {}}),
        to_json(&result.responses[2])
    );
    // One delta, by the creator, for the whole batch.
    assert_eq!(1, result.submissions.len());
    let submission = &result.submissions[0];
    assert_eq!(alice(), submission.author);
    assert_eq!(vec![0, 1, 2], submission.operation_indices);
    assert!(result.all_submitted());
    let name = &submission.wavelet_name;
    assert_eq!(
        vec!["alice@example.com", "bob@example.com"],
        fixture.participants(name).await
    );
    assert_eq!(
        "<conversation><blip id=\"b+sB\"/></conversation>",
        fixture.doc_xml(name, "conversation").await
    );
    assert_eq!(
        "<?a \"conv/title\"=\"Greeting\"?><body><?a \"conv/title\"?><line/>Hello</body>",
        fixture.doc_xml(name, "b+sB").await
    );
    // Every delta is authored by the actor.
    let snapshot = fixture.server.get_snapshot(name).await.unwrap().unwrap();
    let history = fixture
        .server
        .get_history(
            name,
            &HashedVersionFactory::create_version_zero(name),
            snapshot.snapshot.hashed_version(),
        )
        .await
        .unwrap();
    assert_eq!(1, history.len());
    assert_eq!(&alice(), history[0].author());
}

#[tokio::test]
async fn append_reply_inline_reply_and_delete() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &["bob@example.com"]).await;
    let root = wave.root_blip.clone();
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                modify(
                    &wave,
                    "text",
                    &root,
                    DocumentModifyAction::with_values(ModifyHow::Insert, ["Hello world"]),
                )
                .with(ParamsProperty::Index, 1),
            ],
        )
        .await;
    assert_ok(&responses);
    // Bob appends a blip, replies to the root blip, continues that
    // thread, and replies inline twice.
    let requests = vec![
        op(OperationType::WaveletAppendBlip, "append", &wave).with(
            ParamsProperty::BlipData,
            new_blip(&wave, "TBD_a", "\nappended"),
        ),
        blip_op(OperationType::BlipCreateChild, "child", &wave, &root).with(
            ParamsProperty::BlipData,
            new_blip(&wave, "TBD_c", "\nchild"),
        ),
        blip_op(
            OperationType::BlipContinueThread,
            "continue",
            &wave,
            "TBD_c",
        )
        .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_n", "\nnext")),
        blip_op(
            OperationType::DocumentInsertInlineBlip,
            "inline",
            &wave,
            &root,
        )
        .with(
            ParamsProperty::BlipData,
            new_blip(&wave, "TBD_i", "\ninline"),
        )
        .with(ParamsProperty::Index, 6),
        blip_op(
            OperationType::DocumentAppendInlineBlip,
            "inline-end",
            &wave,
            &root,
        )
        .with(
            ParamsProperty::BlipData,
            new_blip(&wave, "TBD_e", "\nat the end"),
        ),
        blip_op(
            OperationType::DocumentAppendMarkup,
            "markup",
            &wave,
            "TBD_a",
        )
        .with(ParamsProperty::Content, "<b>bold</b> text"),
    ];
    let responses = executor.execute(&bob(), Registry::DataApi, requests).await;
    assert_ok(&responses);
    let new_id = |i: usize| data_string(&responses[i], ParamsProperty::NewBlipId);
    let (appended, child, next, inline, inline_end) =
        (new_id(0), new_id(1), new_id(2), new_id(3), new_id(4));
    // Each answer names the root blip and the new blip.
    assert_eq!(root, data_string(&responses[0], ParamsProperty::BlipId));
    assert_eq!(json!({"id": "markup", "data": {}}), to_json(&responses[5]));
    let manifest = fixture.doc_xml(&wave.name, "conversation").await;
    assert_eq!(
        format!(
            "<conversation><blip id=\"{root}\">\
             <thread id=\"{child}\"><blip id=\"{child}\"/><blip id=\"{next}\"/></thread>\
             <thread id=\"{inline}\" inline=\"true\"><blip id=\"{inline}\"/></thread>\
             <thread id=\"{inline_end}\" inline=\"true\"><blip id=\"{inline_end}\"/></thread>\
             </blip><blip id=\"{appended}\"/></conversation>"
        ),
        manifest
    );
    assert_eq!(
        format!(
            "<body><line/>Hello<reply id=\"{inline}\"/> world<line/>\
             <reply id=\"{inline_end}\"/></body>"
        ),
        fixture.doc_xml(&wave.name, &root).await
    );
    assert_eq!(
        "<body><line/>appended<line/><b>bold</b> text</body>",
        fixture.doc_xml(&wave.name, &appended).await
    );
    assert_eq!(
        "<body><line/>next</body>",
        fixture.doc_xml(&wave.name, &next).await
    );
    // Deleting the inline reply removes its thread and its anchor;
    // deleting the child keeps the thread for the blip after it.
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                blip_op(OperationType::BlipDelete, "d1", &wave, &inline),
                blip_op(OperationType::BlipDelete, "d2", &wave, &child),
                blip_op(OperationType::BlipDelete, "d3", &wave, &child),
            ],
        )
        .await;
    assert_eq!(json!({"id": "d1", "data": {}}), to_json(&responses[0]));
    assert!(!responses[1].is_error());
    assert_eq!(
        format!("Blip with id {child} does not exist or has been deleted"),
        error_of(&responses[2])
    );
    assert_eq!(
        format!(
            "<conversation><blip id=\"{root}\">\
             <thread id=\"{child}\"><blip id=\"{next}\"/></thread>\
             <thread id=\"{inline_end}\" inline=\"true\"><blip id=\"{inline_end}\"/></thread>\
             </blip><blip id=\"{appended}\"/></conversation>"
        ),
        fixture.doc_xml(&wave.name, "conversation").await
    );
    assert_eq!(
        format!("<body><line/>Hello world<line/><reply id=\"{inline_end}\"/></body>"),
        fixture.doc_xml(&wave.name, &root).await
    );
    assert_eq!("", fixture.doc_xml(&wave.name, &inline).await);
}

#[tokio::test]
async fn inline_blip_errors() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let root = &wave.root_blip;
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                blip_op(OperationType::DocumentInsertInlineBlip, "zero", &wave, root)
                    .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_1", "\nx"))
                    .with(ParamsProperty::Index, 0),
                blip_op(
                    OperationType::DocumentInsertInlineBlipAfterElement,
                    "missing",
                    &wave,
                    root,
                )
                .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_2", "\nx"))
                .with(ParamsProperty::Element, Element::gadget("http://nowhere/")),
                blip_op(
                    OperationType::DocumentInsertInlineBlipAfterElement,
                    "after-line",
                    &wave,
                    root,
                )
                .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_3", "\nx"))
                .with(ParamsProperty::Element, Element::line()),
                blip_op(
                    OperationType::DocumentAppendMarkup,
                    "bad-markup",
                    &wave,
                    root,
                )
                .with(ParamsProperty::Content, "<b>unclosed"),
                blip_op(OperationType::BlipCreateChild, "no-data", &wave, root),
                op(OperationType::WaveletAppendBlip, "no-blip", &wave)
                    .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_4", "\nx"))
                    .with(ParamsProperty::WaveletId, "nonsense"),
            ],
        )
        .await;
    assert_eq!(
        "Can't inline a blip on position <= 0, got 0",
        error_of(&responses[0])
    );
    assert_eq!("Requested element not found", error_of(&responses[1]));
    assert!(!responses[2].is_error());
    assert!(responses[3].is_error());
    assert_eq!("property BLIP_DATA not found", error_of(&responses[4]));
    assert_eq!("Invalid id", error_of(&responses[5]));
    let inline = data_string(&responses[2], ParamsProperty::NewBlipId);
    assert_eq!(
        format!("<body><line/><reply id=\"{inline}\"/></body>"),
        fixture.doc_xml(&wave.name, root).await
    );
}

#[tokio::test]
async fn document_modify_variants() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let root = wave.root_blip.clone();
    let actor = alice();
    let run =
        |requests: Vec<OperationRequest>| executor.execute(&actor, Registry::DataApi, requests);
    let xml = || fixture.doc_xml(&wave.name, &root);

    // Insert at a position, over two lines.
    let responses = run(vec![
        modify(
            &wave,
            "insert",
            &root,
            DocumentModifyAction::with_values(ModifyHow::Insert, ["one two\nthree two"]),
        )
        .with(ParamsProperty::Index, 1),
    ])
    .await;
    assert_ok(&responses);
    assert_eq!(json!({"id": "insert", "data": {}}), to_json(&responses[0]));
    assert_eq!("<body><line/>one two<line/>three two</body>", xml().await);

    // Replace a range; insert after a range.
    let responses = run(vec![
        modify(
            &wave,
            "replace",
            &root,
            DocumentModifyAction::with_values(ModifyHow::Replace, ["1"]),
        )
        .with(ParamsProperty::Range, Range::new(1, 4)),
        modify(
            &wave,
            "after",
            &root,
            DocumentModifyAction::with_values(ModifyHow::InsertAfter, ["!"]),
        )
        .with(ParamsProperty::Range, Range::new(1, 6)),
    ])
    .await;
    assert_ok(&responses);
    assert_eq!("<body><line/>1 two!<line/>three two</body>", xml().await);

    // Every occurrence of a text: annotate, then replace with values in
    // turn. (The original has no working text query.)
    let responses = run(vec![
        modify(
            &wave,
            "annotate",
            &root,
            DocumentModifyAction::annotation(ModifyHow::Annotate, "style/fontWeight", Some("bold")),
        )
        .with(
            ParamsProperty::ModifyQuery,
            DocumentModifyQuery::text("two", -1),
        ),
        modify(
            &wave,
            "replace-all",
            &root,
            DocumentModifyAction::with_values(ModifyHow::Replace, ["2", "zwei"]),
        )
        .with(
            ParamsProperty::ModifyQuery,
            DocumentModifyQuery::text("two", -1),
        ),
    ])
    .await;
    assert_ok(&responses);
    // The replacement text is typed in front of the bold text and so is
    // not bold itself.
    assert_eq!("<body><line/>1 2!<line/>three zwei</body>", xml().await);

    // Delete matches of a text, at most one; clear the annotation
    // everywhere; insert with annotations of its own.
    let responses = run(vec![
        modify(
            &wave,
            "delete",
            &root,
            DocumentModifyAction::new(ModifyHow::Delete),
        )
        .with(
            ParamsProperty::ModifyQuery,
            DocumentModifyQuery::text("e", 1),
        ),
        modify(
            &wave,
            "clear",
            &root,
            DocumentModifyAction::annotation(ModifyHow::ClearAnnotation, "style/fontWeight", None),
        ),
        modify(
            &wave,
            "bundled",
            &root,
            DocumentModifyAction {
                bundled_annotations: Some(vec![BundledAnnotation::new("link/manual", "http://x/")]),
                ..DocumentModifyAction::with_values(ModifyHow::Insert, ["link"])
            },
        )
        .with(ParamsProperty::Index, 2),
    ])
    .await;
    assert_ok(&responses);
    assert_eq!(
        "<body><line/>1<?a \"link/manual\"=\"http://x/\"?>link<?a \"link/manual\"?> 2!<line/>\
         thre zwei</body>",
        xml().await
    );

    // Elements: a line and an image at a position (an extension), a
    // gadget and a button (appended on lines of their own, as in the
    // original).
    let mut gadget = Element::gadget("http://g/g.xml");
    gadget.set_property("count", "1");
    let responses = run(vec![
        modify(
            &wave,
            "elements",
            &root,
            DocumentModifyAction::with_elements(
                ModifyHow::Insert,
                [Element::line(), Element::image("http://i/", 1, 2, "pic")],
            ),
        )
        .with(
            ParamsProperty::ModifyQuery,
            DocumentModifyQuery::text("2", 2),
        ),
        modify(
            &wave,
            "gadget",
            &root,
            DocumentModifyAction::with_elements(ModifyHow::Insert, [gadget.clone()]),
        )
        .with(ParamsProperty::Index, 1),
        modify(
            &wave,
            "button",
            &root,
            DocumentModifyAction::with_elements(
                ModifyHow::Insert,
                [Element::form(ElementType::Button, "go", "Go", "Go")],
            ),
        )
        .with(ParamsProperty::Index, 1),
    ])
    .await;
    assert_ok(&responses);
    assert_eq!(
        "<body><line/>1<?a \"link/manual\"=\"http://x/\"?>link<?a \"link/manual\"?> <line/>2!\
         <line/>thre zwei<line/>\
         <gadget author=\"\" prefs=\"\" state=\"\" title=\"\" url=\"http://g/g.xml\">\
         <state name=\"count\" value=\"1\"/><state name=\"url\" value=\"http://g/g.xml\"/>\
         </gadget><line/><button name=\"go\"><caption>Go</caption><events/></button></body>"
            .replace(" author=\"\"", ""),
        xml().await
    );

    // Update the gadget found by a query; delete an element by query.
    let mut update = Element::new(ElementType::Gadget);
    update.set_property("count", "2");
    update.set_property("extra", "x");
    let responses = run(vec![
        modify(
            &wave,
            "update",
            &root,
            DocumentModifyAction::with_elements(ModifyHow::UpdateElement, [update]),
        )
        .with(
            ParamsProperty::ModifyQuery,
            DocumentModifyQuery::element(ElementType::Gadget, BTreeMap::new(), -1),
        ),
        modify(
            &wave,
            "delete-button",
            &root,
            DocumentModifyAction::new(ModifyHow::Delete),
        )
        .with(
            ParamsProperty::ModifyQuery,
            DocumentModifyQuery::element(
                ElementType::Button,
                BTreeMap::from([("name".to_owned(), "go".to_owned())]),
                -1,
            ),
        ),
    ])
    .await;
    assert_ok(&responses);
    let after = xml().await;
    assert!(
        after.contains(
            "<state name=\"count\" value=\"2\"/><state name=\"url\" value=\"http://g/g.xml\"/>\
             <state name=\"extra\" value=\"x\"/></gadget><line/></body>"
        ),
        "{after}"
    );

    // Errors.
    let responses = run(vec![
        modify(
            &wave,
            "both",
            &root,
            DocumentModifyAction::new(ModifyHow::Delete),
        )
        .with(ParamsProperty::Index, 1)
        .with(ParamsProperty::Range, Range::new(1, 2)),
        modify(
            &wave,
            "empty-range",
            &root,
            DocumentModifyAction::new(ModifyHow::Delete),
        )
        .with(ParamsProperty::Range, Range::new(3, 3)),
        modify(
            &wave,
            "out-of-bounds",
            &root,
            DocumentModifyAction::annotation(ModifyHow::Annotate, "k", Some("v")),
        )
        .with(ParamsProperty::Range, Range::new(1, 1000)),
        blip_op(OperationType::DocumentModify, "no-action", &wave, &root),
        modify(
            &wave,
            "no-blip",
            "b+none",
            DocumentModifyAction::new(ModifyHow::Delete),
        ),
        modify(
            &wave,
            "inline-blip-element",
            &root,
            DocumentModifyAction::with_elements(
                ModifyHow::Insert,
                [Element::new(ElementType::InlineBlip)],
            ),
        ),
    ])
    .await;
    assert_eq!(
        "At most one parameter out of RANGE, INDEX and MODIFY_QUERY must be specified",
        error_of(&responses[0])
    );
    assert_eq!("Invalid range specified, 3>=3", error_of(&responses[1]));
    assert!(
        error_of(&responses[2]).starts_with("Can't set annotation for out of bounds indices"),
        "{:?}",
        responses[2]
    );
    assert_eq!("property MODIFY_ACTION not found", error_of(&responses[3]));
    assert_eq!(
        "Blip with id b+none does not exist or has been deleted",
        error_of(&responses[4])
    );
    assert!(responses[5].is_error());
    assert_eq!(after, xml().await);
}

#[tokio::test]
async fn text_with_characters_outside_the_bmp() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let root = wave.root_blip.clone();
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                modify(
                    &wave,
                    "insert",
                    &root,
                    DocumentModifyAction::with_values(ModifyHow::Insert, ["a\u{1F600}b"]),
                )
                .with(ParamsProperty::Index, 1),
                // Positions are UTF-16 code units: the emoji takes two.
                modify(
                    &wave,
                    "after-emoji",
                    &root,
                    DocumentModifyAction::with_values(ModifyHow::Insert, ["X"]),
                )
                .with(ParamsProperty::Index, 4),
                op(OperationType::WaveletAppendBlip, "append", &wave).with(
                    ParamsProperty::BlipData,
                    new_blip(&wave, "TBD_1", "\ntab\there \u{1F680}"),
                ),
                op(OperationType::RobotFetchWave, "fetch", &wave),
            ],
        )
        .await;
    assert_ok(&responses);
    assert_eq!(
        "<body><line/>a\u{1F600}Xb</body>",
        fixture.doc_xml(&wave.name, &root).await
    );
    let appended = data_string(&responses[2], ParamsProperty::NewBlipId);
    assert_eq!(
        "<body><line/>tab    here \u{1F680}</body>",
        fixture.doc_xml(&wave.name, &appended).await
    );
    let blips: BTreeMap<String, BlipData> = responses[3].typed(ParamsProperty::Blips).unwrap();
    assert_eq!("\na\u{1F600}Xb", blips[&root].content);
}

#[tokio::test]
async fn set_title_and_add_participant() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let requests = vec![
        op(OperationType::WaveletSetTitle, "title", &wave)
            .with(ParamsProperty::WaveletTitle, "The title"),
        op(OperationType::WaveletAddParticipantNewsyntax, "add", &wave)
            .with(ParamsProperty::ParticipantId, "bob@example.com"),
        op(
            OperationType::WaveletAddParticipantNewsyntax,
            "again",
            &wave,
        )
        .with(ParamsProperty::ParticipantId, "bob@example.com"),
        op(
            OperationType::WaveletAddParticipantNewsyntax,
            "invalid",
            &wave,
        )
        .with(ParamsProperty::ParticipantId, "not an address"),
        op(
            OperationType::WaveletRemoveParticipantNewsyntax,
            "stranger",
            &wave,
        )
        .with(ParamsProperty::ParticipantId, "carol@example.com"),
        op(
            OperationType::WaveletAddParticipantNewsyntax,
            "missing",
            &wave,
        ),
    ];
    let responses = executor
        .execute(&alice(), Registry::DataApi, requests)
        .await;
    assert_eq!(json!({"id": "title", "data": {}}), to_json(&responses[0]));
    assert_eq!(
        json!({"id": "add", "data": {
            "blipId": wave.root_blip,
            "participantsAdded": ["bob@example.com"],
            "participantsRemoved": []}}),
        to_json(&responses[1])
    );
    assert_eq!(
        "bob@example.com is already a member of wavelet",
        error_of(&responses[2])
    );
    assert_eq!(
        "Target ParticipantId not an address is not valid",
        error_of(&responses[3])
    );
    assert_eq!(
        "carol@example.com is not a member of wavelet",
        error_of(&responses[4])
    );
    assert_eq!("property PARTICIPANT_ID not found", error_of(&responses[5]));
    assert_eq!(
        vec!["alice@example.com", "bob@example.com"],
        fixture.participants(&wave.name).await
    );
    assert!(
        fixture
            .doc_xml(&wave.name, &wave.root_blip)
            .await
            .starts_with("<?a \"conv/title\"=\"The title\"?><body>")
    );
    // Bob, now a participant, removes Alice.
    let responses = executor
        .execute(
            &bob(),
            Registry::DataApi,
            vec![
                op(
                    OperationType::WaveletRemoveParticipantNewsyntax,
                    "remove",
                    &wave,
                )
                .with(ParamsProperty::ParticipantId, "alice@example.com"),
            ],
        )
        .await;
    assert_eq!(
        json!({"id": "remove", "data": {
            "blipId": wave.root_blip,
            "participantsAdded": [],
            "participantsRemoved": ["alice@example.com"]}}),
        to_json(&responses[0])
    );
    assert_eq!(
        vec!["bob@example.com"],
        fixture.participants(&wave.name).await
    );
}

#[tokio::test]
async fn fetch_wave_returns_the_expected_json() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &["bob@example.com"]).await;
    let root = wave.root_blip.clone();
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                modify(
                    &wave,
                    "text",
                    &root,
                    DocumentModifyAction::with_values(ModifyHow::Insert, ["Hello world"]),
                )
                .with(ParamsProperty::Index, 1),
                op(OperationType::WaveletSetTitle, "title", &wave)
                    .with(ParamsProperty::WaveletTitle, "Hello"),
                blip_op(
                    OperationType::DocumentInsertInlineBlip,
                    "inline",
                    &wave,
                    &root,
                )
                .with(
                    ParamsProperty::BlipData,
                    new_blip(&wave, "TBD_i", "\ninline"),
                )
                .with(ParamsProperty::Index, 6),
            ],
        )
        .await;
    assert_ok(&responses);
    let inline = data_string(&responses[2], ParamsProperty::NewBlipId);
    fixture.clock.set(2_000_000);
    let responses = executor
        .execute(
            &bob(),
            Registry::DataApi,
            vec![
                op(OperationType::RobotFetchWave, "fetch", &wave)
                    .with(ParamsProperty::Message, "please"),
            ],
        )
        .await;
    let line = json!({"type": "LINE", "properties": {}});
    let expected = json!({"id": "fetch", "data": {
        "blipId": root,
        "message": "please",
        "waveletData": {
            "creationTime": 1_000_000,
            "lastModifiedTime": 1_000_000,
            "version": 12,
            "participants": ["alice@example.com", "bob@example.com"],
            "participantRoles": {"alice@example.com": "FULL", "bob@example.com": "FULL"},
            "dataDocuments": {},
            "tags": [],
            "creator": "alice@example.com",
            "rootBlipId": root,
            "title": "Hello",
            "waveId": wave.wave_id,
            "waveletId": wave.wavelet_id,
            "rootThread": {"id": "", "location": -1, "blipIds": [root]},
        },
        "blips": {
            root.clone(): {
                "annotations": [{
                    "name": "conv/title", "value": "Hello", "range": {"start": 0, "end": 0}}],
                "elements": {
                    "0": line,
                    "6": {"type": "INLINE_BLIP", "properties": {"id": inline}}},
                "blipId": root,
                "childBlipIds": [inline],
                "contributors": ["alice@example.com"],
                "creator": "alice@example.com",
                "content": "\nHello  world",
                "lastModifiedTime": 1_000_000,
                "parentBlipId": null,
                // Anchoring a reply does not count as modifying the blip.
                "version": 7,
                "waveId": wave.wave_id,
                "waveletId": wave.wavelet_id,
                "replyThreadIds": [inline],
                "threadId": "",
            },
            inline.clone(): {
                "annotations": [],
                "elements": {"0": line},
                "blipId": inline,
                "childBlipIds": [],
                "contributors": ["alice@example.com"],
                "creator": "alice@example.com",
                "content": "\ninline",
                "lastModifiedTime": 1_000_000,
                "parentBlipId": root,
                "version": 12,
                "waveId": wave.wave_id,
                "waveletId": wave.wavelet_id,
                "replyThreadIds": [],
                "threadId": inline,
            },
        },
        "threads": {
            inline.clone(): {"id": inline, "location": 6, "blipIds": [inline]},
        },
    }});
    let actual = to_json(&responses[0]);
    assert_eq!(Vec::<String>::new(), json_diff(&expected, &actual));

    // Protocol 0.21, declared by a leading robot.notify: no threads.
    let responses = executor
        .execute(
            &bob(),
            Registry::DataApi,
            vec![
                OperationRequest::of(OperationType::RobotNotify, "notify")
                    .with(ParamsProperty::CapabilitiesHash, "x")
                    .with(ParamsProperty::ProtocolVersion, "0.21"),
                op(OperationType::RobotFetchWave, "fetch", &wave),
            ],
        )
        .await;
    assert_eq!(json!({"id": "notify", "data": {}}), to_json(&responses[0]));
    let data = to_json(&responses[1])["data"].clone();
    assert_eq!(json!({}), data["threads"]);
    assert_eq!(Value::Null, data["waveletData"]["rootThread"]);
    assert_eq!(Value::Null, data["blips"][&root]["threadId"]);
    assert_eq!(json!(root), data["blips"][&inline]["parentBlipId"]);

    // The ids of the wave's wavelets the caller may see.
    let responses = executor
        .execute(
            &bob(),
            Registry::DataApi,
            vec![
                op(OperationType::RobotFetchWave, "ids", &wave)
                    .with(ParamsProperty::ReturnWaveletIds, true),
                OperationRequest::of(OperationType::RobotFetchWave, "missing"),
            ],
        )
        .await;
    assert_eq!(
        json!({"id": "ids", "data": {"waveletIds": [
            {"domain": "example.com", "id": "conv+root"}]}}),
        to_json(&responses[0])
    );
    assert_eq!("property WAVE_ID not found", error_of(&responses[1]));
}

#[tokio::test]
async fn access_denied_for_non_participants() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let before = fixture.doc_xml(&wave.name, "conversation").await;
    let requests = vec![
        op(OperationType::RobotFetchWave, "fetch", &wave),
        op(OperationType::WaveletAppendBlip, "append", &wave).with(
            ParamsProperty::BlipData,
            new_blip(&wave, "TBD_1", "\nintruder"),
        ),
        modify(
            &wave,
            "modify",
            &wave.root_blip,
            DocumentModifyAction::with_values(ModifyHow::Insert, ["x"]),
        ),
        op(OperationType::WaveletSetTitle, "title", &wave).with(ParamsProperty::WaveletTitle, "x"),
        op(OperationType::WaveletAddParticipantNewsyntax, "add", &wave)
            .with(ParamsProperty::ParticipantId, "bob@example.com"),
        blip_op(OperationType::BlipDelete, "delete", &wave, &wave.root_blip),
        op(OperationType::RobotFolderAction, "folder", &wave)
            .with(ParamsProperty::ModifyHow, "markAsRead"),
        op(OperationType::RobotExportSnapshot, "snapshot", &wave),
        op(OperationType::RobotFetchWave, "ids", &wave)
            .with(ParamsProperty::ReturnWaveletIds, true),
    ];
    let result = executor
        .execute_detailed(&bob(), Registry::DataApi, &requests, None)
        .await;
    for response in &result.responses[..8] {
        assert_eq!("Access rejected", error_of(response), "{}", response.id);
    }
    // Wavelets the caller may not see are not listed.
    assert_eq!(
        json!({"id": "ids", "data": {"waveletIds": []}}),
        to_json(&result.responses[8])
    );
    assert!(result.submissions.is_empty());
    assert_eq!(before, fixture.doc_xml(&wave.name, "conversation").await);
    assert_eq!(
        vec!["alice@example.com"],
        fixture.participants(&wave.name).await
    );
    // A wave that does not exist looks the same.
    let responses = executor
        .execute(
            &bob(),
            Registry::DataApi,
            vec![OperationRequest::for_wavelet(
                OperationType::RobotFetchWave,
                "nowhere",
                "example.com/w+none",
                "example.com/conv+root",
            )],
        )
        .await;
    assert_eq!("Access rejected", error_of(&responses[0]));
}

#[tokio::test]
async fn a_rejected_delta_can_be_reported() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &["bob@example.com"]).await;
    // Bob reads the wavelet as a robot would get it in a bundle, is then
    // removed, and replies against the old state.
    let seen = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap()
        .snapshot;
    let removal = vec![
        op(
            OperationType::WaveletRemoveParticipantNewsyntax,
            "remove",
            &wave,
        )
        .with(ParamsProperty::ParticipantId, "bob@example.com"),
    ];
    assert_ok(&executor.execute(&alice(), Registry::DataApi, removal).await);
    let requests = vec![op(OperationType::WaveletAppendBlip, "append", &wave).with(
        ParamsProperty::BlipData,
        new_blip(&wave, "TBD_1", "\ntoo late"),
    )];
    let result = executor
        .execute_detailed(
            &bob(),
            Registry::Passive,
            &requests,
            Some(BoundWavelet { snapshot: seen }),
        )
        .await;
    // As in the original the response says what was done locally...
    assert!(!result.responses[0].is_error());
    // ...and the server's verdict is separate.
    assert!(!result.all_submitted());
    assert_eq!(vec![0], result.submissions[0].operation_indices);
    let reported = result.responses_with_submit_errors();
    assert!(
        error_of(&reported[0]).starts_with("Failed to apply the changes to "),
        "{reported:?}"
    );
}

// ---- OperationContextImplTest ----

#[tokio::test]
async fn temporary_ids_resolve_within_a_batch_only() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let temp_blip_op = |id: &str| {
        OperationRequest::for_blip(
            OperationType::BlipCreateChild,
            id,
            "example.com/TBD_wave",
            "example.com/conv+root",
            "TBD_blip",
        )
        .with(
            ParamsProperty::BlipData,
            new_blip_data(
                "example.com/TBD_wave",
                "example.com/conv+root",
                "TBD_child",
                "\nx",
            ),
        )
    };
    // Before the wave is created the temporary ids mean nothing.
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                temp_blip_op("early"),
                create_wavelet_request("create", &[]),
                temp_blip_op("child"),
                OperationRequest::for_blip(
                    OperationType::BlipDelete,
                    "delete",
                    "example.com/TBD_wave",
                    "example.com/conv+root",
                    "TBD_child",
                ),
                OperationRequest::for_blip(
                    OperationType::BlipDelete,
                    "unknown",
                    "example.com/TBD_wave",
                    "example.com/conv+root",
                    "TBD_never",
                ),
            ],
        )
        .await;
    assert!(
        error_of(&responses[0]).contains("couldn't be retrieved"),
        "{:?}",
        responses[0]
    );
    assert_ok(&responses[1..4]);
    assert_eq!(
        "Blip with id TBD_never does not exist or has been deleted",
        error_of(&responses[4])
    );
    // In the next batch they are gone.
    let responses = executor
        .execute(&alice(), Registry::DataApi, vec![temp_blip_op("later")])
        .await;
    assert!(responses[0].is_error());
    // A created wavelet whose client id is not temporary is reachable
    // only by its real id.
    let mut request = create_wavelet_request("create", &[]);
    let mut data: WaveletData = request.typed(ParamsProperty::WaveletData).unwrap();
    data.wave_id = Some("example.com/w+mine".to_owned());
    request.set(ParamsProperty::WaveletData, data);
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                request,
                OperationRequest::for_wavelet(
                    OperationType::RobotFetchWave,
                    "by-client-id",
                    "example.com/w+mine",
                    "example.com/conv+root",
                ),
            ],
        )
        .await;
    let real_wave_id = data_string(&responses[0], ParamsProperty::WaveId);
    assert_ne!("example.com/w+mine", real_wave_id);
    assert_eq!("Access rejected", error_of(&responses[1]));
}

#[tokio::test]
async fn wavelets_are_opened_once_per_batch() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &["bob@example.com"]).await;
    // Two operations of a batch see each other's changes, and the fetch
    // sees both: they work on one copy of the wavelet.
    let requests = vec![
        op(OperationType::WaveletAppendBlip, "a", &wave)
            .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_a", "\none")),
        op(OperationType::WaveletAppendBlip, "b", &wave)
            .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_b", "\ntwo")),
        op(OperationType::RobotFetchWave, "fetch", &wave),
    ];
    let applied = executor
        .apply(&alice(), Registry::DataApi, &requests, None)
        .await;
    assert_ok(&applied.responses);
    let wavelet_data: WaveletData = applied.responses[2]
        .typed(ParamsProperty::WaveletData)
        .unwrap();
    assert_eq!(3, wavelet_data.root_thread.unwrap().blip_ids.len());
    // Nothing was submitted: one delta against the version first seen.
    assert_eq!(1, applied.deltas.len());
    let delta = &applied.deltas[0];
    assert_eq!(wave.name, delta.wavelet_name);
    assert_eq!(vec![0, 1, 2], delta.operation_indices);
    let server_version = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap()
        .snapshot
        .hashed_version()
        .clone();
    assert_eq!(&server_version, delta.delta.target_version());
    assert_eq!(&alice(), delta.delta.author());
    assert_eq!(
        "<conversation><blip id=\"b+sB\"/></conversation>",
        fixture.doc_xml(&wave.name, "conversation").await
    );
}

#[tokio::test]
async fn bound_batches_use_the_wavelet_given() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &["bob@example.com"]).await;
    let seen = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap()
        .snapshot;
    // The wavelet moves on.
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![op(OperationType::WaveletAppendBlip, "a", &wave).with(
                ParamsProperty::BlipData,
                new_blip(&wave, "TBD_a", "\nlater"),
            )],
        )
        .await;
    let later = data_string(&responses[0], ParamsProperty::NewBlipId);
    // A reply bound to the old state does not see the new blip, but its
    // delta is transformed and applies.
    let requests = vec![
        blip_op(OperationType::BlipDelete, "unseen", &wave, &later),
        op(OperationType::WaveletAppendBlip, "append", &wave).with(
            ParamsProperty::BlipData,
            new_blip(&wave, "TBD_b", "\nreply"),
        ),
        // The passive registry has no fetchWave.
        op(OperationType::RobotFetchWave, "fetch", &wave),
    ];
    let result = executor
        .execute_detailed(
            &bob(),
            Registry::Passive,
            &requests,
            Some(BoundWavelet {
                snapshot: seen.clone(),
            }),
        )
        .await;
    assert!(result.responses[0].is_error());
    let reply = data_string(&result.responses[1], ParamsProperty::NewBlipId);
    assert_eq!(
        "No OperationService found for ROBOT_FETCH_WAVE",
        error_of(&result.responses[2])
    );
    assert!(result.all_submitted());
    assert_eq!(
        // The reply was made without knowing of the later blip; the
        // server's transformation puts it first.
        format!(
            "<conversation><blip id=\"{}\"/><blip id=\"{reply}\"/><blip id=\"{later}\"/>\
             </conversation>",
            wave.root_blip
        ),
        fixture.doc_xml(&wave.name, "conversation").await
    );
}

// ---- OperationUtilTest, OperationServiceRegistryTest ----

#[tokio::test]
async fn unknown_operations_and_registries() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let requests = vec![
        OperationRequest::new("no.such.operation", "unknown"),
        op(OperationType::WaveletAppendDatadoc, "unsupported", &wave),
        op(OperationType::WaveletCreate, "passive-only", &wave),
    ];
    let responses = executor
        .execute(&alice(), Registry::DataApi, requests)
        .await;
    assert_eq!(
        "No OperationService found for UNKNOWN",
        error_of(&responses[0])
    );
    assert_eq!(
        "No OperationService found for WAVELET_APPEND_DATADOC",
        error_of(&responses[1])
    );
    assert_eq!(
        "No OperationService found for WAVELET_CREATE",
        error_of(&responses[2])
    );
    // What each registry offers.
    let active = Registry::Active.operations();
    assert_eq!(active, Registry::DataApi.operations());
    assert_eq!(24, active.len());
    let passive = Registry::Passive.operations();
    assert_eq!(16, passive.len());
    assert!(passive.contains(&OperationType::WaveletCreate));
    assert!(!passive.contains(&OperationType::RobotCreateWavelet));
    assert!(!passive.contains(&OperationType::RobotFetchWave));
    let agent = Registry::Agent.operations();
    assert_eq!(25, agent.len());
    assert!(agent.contains(&OperationType::WaveletCreate));
    assert!(Registry::Agent.supports(OperationType::RobotSearch));
    assert!(!Registry::Passive.supports(OperationType::RobotSearch));
    // Passive robots create waves with wavelet.create.
    let mut create = create_wavelet_request("create", &[]);
    create.method = OperationType::WaveletCreate.method().to_owned();
    let responses = executor
        .execute(&alice(), Registry::Passive, vec![create])
        .await;
    assert_ok(&responses);
    // The namespace of active API clients is understood.
    let mut namespaced = op(OperationType::RobotFetchWave, "ns", &wave);
    namespaced.method = "wave.robot.fetchWave".to_owned();
    assert_ok(
        &executor
            .execute(&alice(), Registry::Active, vec![namespaced])
            .await,
    );
}

#[test]
fn proxy_participants() {
    let robot = participant("robot@example.com");
    assert_eq!(Ok(robot.clone()), to_proxy_participant(&robot, None));
    assert_eq!(Ok(robot.clone()), to_proxy_participant(&robot, Some("")));
    assert_eq!(
        Ok(participant("robot+proxy@example.com")),
        to_proxy_participant(&robot, Some("proxy"))
    );
    assert!(to_proxy_participant(&robot, Some("invalid@")).is_err());
    assert!(to_proxy_participant(&robot, Some("with space")).is_err());

    let with_proxy = |proxy: &str| {
        OperationRequest::of(OperationType::RobotFetchWave, "op1")
            .with(ParamsProperty::ProxyingFor, proxy)
    };
    assert_eq!(
        Ok(participant("robot+proxy@example.com")),
        compute_participant(&with_proxy("proxy"), &robot)
    );
    assert_eq!(
        Ok(robot.clone()),
        compute_participant(
            &OperationRequest::of(OperationType::RobotFetchWave, "x"),
            &robot
        )
    );
    assert_eq!(
        "robot@example.com+invalid@",
        compute_participant(&with_proxy("invalid@"), &robot)
            .unwrap_err()
            .message
    );
}

#[tokio::test]
async fn proxying_for_acts_as_another_participant() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let robot = participant("robot@example.com");
    let wave = new_wave(&executor, &alice(), &["robot@example.com"]).await;
    let append = |id: &str, temp: &str| {
        op(OperationType::WaveletAppendBlip, id, &wave)
            .with(
                ParamsProperty::BlipData,
                new_blip(&wave, temp, "\nvia proxy"),
            )
            .with(ParamsProperty::ProxyingFor, "carol")
    };
    // The proxy is a participant of its own: it has to be on the wave.
    let responses = executor
        .execute(&robot, Registry::Active, vec![append("early", "TBD_1")])
        .await;
    assert_eq!("Access rejected", error_of(&responses[0]));
    // The robot adds its proxy, which then acts under its own address.
    // Robot and proxy each work on their own copy of the wavelet and
    // submit their own delta.
    let result = executor
        .execute_detailed(
            &robot,
            Registry::Active,
            &[
                op(OperationType::WaveletAddParticipantNewsyntax, "add", &wave)
                    .with(ParamsProperty::ParticipantId, "robot+carol@example.com"),
                op(OperationType::WaveletAppendBlip, "own", &wave).with(
                    ParamsProperty::BlipData,
                    new_blip(&wave, "TBD_0", "\nas robot"),
                ),
            ],
            None,
        )
        .await;
    assert!(result.all_submitted());
    let result = executor
        .execute_detailed(
            &robot,
            Registry::Active,
            &[
                append("proxied", "TBD_1"),
                op(OperationType::WaveletAppendBlip, "own", &wave).with(
                    ParamsProperty::BlipData,
                    new_blip(&wave, "TBD_2", "\nas robot"),
                ),
                op(OperationType::WaveletAppendBlip, "bad-proxy", &wave)
                    .with(ParamsProperty::BlipData, new_blip(&wave, "TBD_3", "\nx"))
                    .with(ParamsProperty::ProxyingFor, "not valid"),
            ],
            None,
        )
        .await;
    assert_ok(&result.responses[..2]);
    assert_eq!(
        "robot@example.com+not valid",
        error_of(&result.responses[2])
    );
    assert_eq!(2, result.submissions.len());
    assert!(result.all_submitted());
    let authors: Vec<&str> = result
        .submissions
        .iter()
        .map(|s| s.author.address())
        .collect();
    assert_eq!(
        vec!["robot+carol@example.com", "robot@example.com"],
        authors
    );
    let proxied = data_string(&result.responses[0], ParamsProperty::NewBlipId);
    let snapshot = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        "robot+carol@example.com",
        snapshot
            .snapshot
            .document(&proxied)
            .unwrap()
            .author()
            .address()
    );
    // A person cannot proxy unless their address would do for a robot.
    let odd = participant("o'brien@example.com");
    let request =
        op(OperationType::RobotFetchWave, "x", &wave).with(ParamsProperty::ProxyingFor, "carol");
    let responses = executor
        .execute(&odd, Registry::DataApi, vec![request])
        .await;
    assert_eq!("o'brien@example.com+carol", error_of(&responses[0]));
}

// ---- RobotWaveletTest ----

#[test]
fn robot_wavelet_deltas() {
    let name =
        resonator_model::id::WaveletName::of_parts(DOMAIN, "w+1", DOMAIN, "conv+root").unwrap();
    let mut wavelet = RobotWavelet::empty(&alice(), &name, 5);
    assert_eq!(name, wavelet.wavelet_name());
    assert_eq!(0, wavelet.snapshot_version().version());
    assert!(wavelet.take_deltas().is_empty());
    // The same author gets the same copy; another author another one.
    wavelet
        .op_based_wavelet(&alice())
        .add_participant(&alice())
        .unwrap();
    assert_eq!(1, wavelet.op_based_wavelet(&alice()).participants().len());
    assert_eq!(0, wavelet.op_based_wavelet(&bob()).participants().len());
    wavelet
        .op_based_wavelet(&bob())
        .add_participant(&bob())
        .unwrap();
    wavelet
        .op_based_wavelet(&alice())
        .add_participant(&bob())
        .unwrap();
    // One delta per author, in order of first use, against the snapshot.
    let deltas = wavelet.take_deltas();
    assert_eq!(2, deltas.len());
    assert_eq!((&alice(), 2), (deltas[0].author(), deltas[0].len()));
    assert_eq!((&bob(), 1), (deltas[1].author(), deltas[1].len()));
    for delta in &deltas {
        assert_eq!(wavelet.snapshot_version(), delta.target_version());
    }
    assert!(matches!(
        deltas[0].ops()[0].kind(),
        WaveletOperationKind::AddParticipant { .. }
    ));
    assert!(wavelet.take_deltas().is_empty());
    assert!(wavelet.snapshot().participants().is_empty());
}

// ---- Services with injected collaborators ----

#[tokio::test]
async fn fetch_profiles() {
    let fixture = Fixture::new();
    let request = |ids: Option<Vec<&str>>| {
        OperationRequest::of(OperationType::RobotFetchProfiles, "op1").with(
            ParamsProperty::FetchProfilesRequest,
            FetchProfilesRequest {
                participant_ids: ids.map(|ids| ids.into_iter().map(str::to_owned).collect()),
                language: None,
            },
        )
    };
    // The default: a name from the address, an avatar of initials.
    let responses = fixture
        .executor()
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                request(Some(vec!["john.smith@example.com", "@example.com"])),
                request(None),
                OperationRequest::of(OperationType::RobotFetchProfiles, "missing"),
            ],
        )
        .await;
    assert_eq!(
        json!({"id": "op1", "data": {"fetchProfilesResult": {"profiles": [
            {"address": "john.smith@example.com", "name": "John Smith",
             "imageUrl": "/iniavatars/100x100/john.smith@example.com", "profileUrl": ""},
            {"address": "@example.com", "name": "@example.com",
             "imageUrl": "/iniavatars/100x100/@example.com", "profileUrl": ""},
        ]}}}),
        to_json(&responses[0])
    );
    assert_eq!(
        json!({"fetchProfilesResult": {"profiles": []}}),
        to_json(&responses[1])["data"]
    );
    assert_eq!(
        "property FETCH_PROFILES_REQUEST not found",
        error_of(&responses[2])
    );
    // The simple fetcher of the original.
    let executor = fixture
        .executor()
        .with_profiles(Arc::new(SimpleProfilesFetcher));
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![request(Some(vec!["a_b.c@example.com"]))],
        )
        .await;
    let result: FetchProfilesResult = responses[0]
        .typed(ParamsProperty::FetchProfilesResult)
        .unwrap();
    assert_eq!(
        vec![ParticipantProfile::new(
            "a_b.c@example.com",
            "A B C",
            UNKNOWN_IMAGE,
            ""
        )],
        result.profiles.unwrap()
    );
    assert_eq!("John Smith", name_from_address("john_smith@example.com"));
    assert_eq!("John", name_from_address("john..@example.com"));
}

struct RecordingSearch {
    calls: Mutex<Vec<(String, String, i32, i32)>>,
}

#[async_trait]
impl SearchProvider for RecordingSearch {
    async fn search(
        &self,
        participant: &ParticipantId,
        query: &str,
        start_at: i32,
        num_results: i32,
    ) -> Result<SearchResult, InvalidRequest> {
        self.calls.lock().unwrap().push((
            participant.address().to_owned(),
            query.to_owned(),
            start_at,
            num_results,
        ));
        let mut result = SearchResult::new(query);
        result.add_digest(SearchDigest {
            title: Some("A wave".to_owned()),
            snippet: Some("snippet".to_owned()),
            wave_id: Some("example.com/w+1".to_owned()),
            last_modified: 2,
            created: 1,
            unread_count: 3,
            blip_count: 4,
            participants: vec!["alice@example.com".to_owned()],
        });
        Ok(result)
    }
}

#[tokio::test]
async fn search() {
    let fixture = Fixture::new();
    let search = Arc::new(RecordingSearch {
        calls: Mutex::new(Vec::new()),
    });
    let executor = fixture.executor().with_search(search.clone());
    let query = |id: &str| {
        OperationRequest::of(OperationType::RobotSearch, id).with(ParamsProperty::Query, "in:inbox")
    };
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                query("defaults"),
                query("paged")
                    .with(ParamsProperty::Index, 5)
                    .with(ParamsProperty::NumResults, 7),
                OperationRequest::of(OperationType::RobotSearch, "no-query"),
            ],
        )
        .await;
    assert_eq!(
        json!({"id": "defaults", "data": {"searchResults": {
            "query": "in:inbox", "numResults": 1, "digests": [{
                "title": "A wave", "snippet": "snippet", "waveId": "example.com/w+1",
                "lastModified": 2, "created": 1, "unreadCount": 3, "blipCount": 4,
                "participants": ["alice@example.com"]}]}}}),
        to_json(&responses[0])
    );
    assert_eq!("property QUERY not found", error_of(&responses[2]));
    let me = "alice@example.com".to_owned();
    assert_eq!(
        vec![
            (me.clone(), "in:inbox".to_owned(), 0, 10),
            (me, "in:inbox".to_owned(), 5, 7)
        ],
        *search.calls.lock().unwrap()
    );
    // Without a search provider.
    let responses = fixture
        .executor()
        .execute(&alice(), Registry::DataApi, vec![query("none")])
        .await;
    assert_eq!("Search is not available", error_of(&responses[0]));
}

struct FixedCapabilities(Option<RobotCapabilities>);

#[async_trait]
impl CapabilitiesFetcher for FixedCapabilities {
    async fn fetch_capabilities(&self, _: &RobotAccountData) -> Result<RobotCapabilities, String> {
        self.0.clone().ok_or_else(|| "unreachable".to_owned())
    }
}

#[tokio::test]
async fn robot_notify() {
    let fixture = Fixture::new();
    let accounts = fixture.db.account_store();
    let robot = participant("robot@example.com");
    let old = RobotCapabilities::new(
        [Capability::new(EventType::DocumentChanged)],
        "old-hash",
        ProtocolVersion::V2_2,
    );
    let new = RobotCapabilities::new(
        [Capability::new(EventType::WaveletSelfAdded)],
        "new-hash",
        ProtocolVersion::V2_2,
    );
    let account = RobotAccountData::robot(
        robot.clone(),
        "http://robot.example",
        None,
        Some(old.clone()),
        true,
    )
    .unwrap();
    accounts
        .put_account(AccountData::Robot(account))
        .await
        .unwrap();
    accounts
        .put_account(AccountData::Human(HumanAccountData::new(alice())))
        .await
        .unwrap();
    let notify = |hash: Option<&str>| {
        let request = OperationRequest::of(OperationType::RobotNotify, "op1");
        match hash {
            Some(hash) => request.with(ParamsProperty::CapabilitiesHash, hash),
            None => request,
        }
    };
    let stored = || async {
        match accounts.get_account(&robot).await.unwrap() {
            Some(AccountData::Robot(robot)) => robot.capabilities,
            other => panic!("{other:?}"),
        }
    };
    let executor = fixture
        .executor()
        .with_capabilities_fetcher(Arc::new(FixedCapabilities(Some(new.clone()))));
    // The same hash: nothing to do.
    let responses = executor
        .execute(&robot, Registry::Active, vec![notify(Some("old-hash"))])
        .await;
    assert_eq!(json!({"id": "op1", "data": {}}), to_json(&responses[0]));
    assert_eq!(Some(old.clone()), stored().await);
    // Another hash: the capabilities are fetched and stored. The
    // deprecated name of the operation works too.
    let mut deprecated = notify(Some("new-hash"));
    deprecated.method = OperationType::RobotNotifyCapabilitiesHash
        .method()
        .to_owned();
    let responses = executor
        .execute(&robot, Registry::Passive, vec![deprecated])
        .await;
    assert_ok(&responses);
    assert_eq!(Some(new.clone()), stored().await);
    // Fetching fails.
    let failing = fixture
        .executor()
        .with_capabilities_fetcher(Arc::new(FixedCapabilities(None)));
    let responses = failing
        .execute(&robot, Registry::Active, vec![notify(Some("other-hash"))])
        .await;
    assert_eq!(
        "Unable to retrieve new capabilities",
        error_of(&responses[0])
    );
    assert_eq!(Some(new), stored().await);
    // Not a robot; no hash.
    let responses = executor
        .execute(&alice(), Registry::Active, vec![notify(Some("x"))])
        .await;
    assert_eq!(
        "Can't exectute robot.notify for unknown robot alice@example.com",
        error_of(&responses[0])
    );
    let responses = executor
        .execute(&robot, Registry::Active, vec![notify(None)])
        .await;
    assert_eq!(
        "property CAPABILITIES_HASH not found",
        error_of(&responses[0])
    );
    // For the Data API and for agents the operation does nothing
    // (`DoNothingService`).
    for registry in [Registry::DataApi, Registry::Agent] {
        let responses = executor
            .execute(&alice(), registry, vec![notify(None)])
            .await;
        assert_eq!(json!({"id": "op1", "data": {}}), to_json(&responses[0]));
    }
}

#[tokio::test]
async fn folder_action_marks_read_in_the_user_data_wavelet() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &["bob@example.com"]).await;
    let folder = |id: &str, how: &str| {
        op(OperationType::RobotFolderAction, id, &wave).with(ParamsProperty::ModifyHow, how)
    };
    let result = executor
        .execute_detailed(
            &bob(),
            Registry::DataApi,
            &[
                folder("read", "markAsRead"),
                folder("bad", "archive"),
                op(OperationType::RobotFolderAction, "missing", &wave),
            ],
            None,
        )
        .await;
    assert_eq!(
        json!({"id": "read", "data": {}}),
        to_json(&result.responses[0])
    );
    assert_eq!(
        "Unsupported folder action: archive",
        error_of(&result.responses[1])
    );
    assert_eq!(
        "property MODIFY_HOW not found",
        error_of(&result.responses[2])
    );
    // Bob's user-data wavelet was created by the delta.
    assert_eq!(1, result.submissions.len());
    assert!(result.all_submitted());
    let user_data = &result.submissions[0].wavelet_name;
    assert_eq!("user+bob@example.com", user_data.wavelet_id.id());
    assert_eq!(wave.name.wave_id, user_data.wave_id);
    let read_state = fixture.doc_xml(user_data, "m/read").await;
    assert!(read_state.contains("<all v=\"5\"/>"), "{read_state}");
    assert_eq!(
        vec!["bob@example.com"],
        fixture.participants(user_data).await
    );
    // One blip, then everything unread again; the wavelet exists now.
    let result = executor
        .execute_detailed(
            &bob(),
            Registry::DataApi,
            &[
                folder("unread", "markAsUnread"),
                folder("blip", "markAsRead").with(ParamsProperty::BlipId, &wave.root_blip),
            ],
            None,
        )
        .await;
    assert_ok(&result.responses);
    assert!(result.all_submitted());
    let read_state = fixture.doc_xml(user_data, "m/read").await;
    assert!(
        read_state.contains(&format!("<blip i=\"{}\"", wave.root_blip)),
        "{read_state}"
    );
    // Alice cannot open Bob's user-data wavelet.
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![OperationRequest::for_wavelet(
                OperationType::RobotExportSnapshot,
                "peek",
                &wave.wave_id,
                "example.com/user+bob@example.com",
            )],
        )
        .await;
    assert_eq!("Access rejected", error_of(&responses[0]));
}

struct MemoryAttachments {
    stored: Mutex<BTreeMap<String, (WaveletName, RawAttachmentData)>>,
}

#[async_trait]
impl AttachmentService for MemoryAttachments {
    async fn export_attachment(&self, attachment_id: &str) -> Result<RawAttachmentData, String> {
        self.stored
            .lock()
            .unwrap()
            .get(attachment_id)
            .map(|(_, data)| data.clone())
            .ok_or_else(|| format!("java.io.IOException: no attachment {attachment_id}"))
    }

    async fn import_attachment(
        &self,
        attachment_id: &str,
        wavelet_name: &WaveletName,
        file_name: &str,
        creator: &ParticipantId,
        data: &[u8],
    ) -> Result<(), String> {
        self.stored.lock().unwrap().insert(
            attachment_id.to_owned(),
            (
                wavelet_name.clone(),
                RawAttachmentData {
                    file_name: Some(file_name.to_owned()),
                    creator: Some(creator.address().to_owned()),
                    data: data.to_vec(),
                },
            ),
        );
        Ok(())
    }
}

#[tokio::test]
async fn export_and_import() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &["bob@example.com"]).await;
    let responses = executor
        .execute(
            &bob(),
            Registry::DataApi,
            vec![op(OperationType::WaveletAppendBlip, "a", &wave).with(
                ParamsProperty::BlipData,
                new_blip(&wave, "TBD_a", "\nby bob"),
            )],
        )
        .await;
    assert_ok(&responses);
    let snapshot = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap();
    let current = snapshot.snapshot.hashed_version().clone();
    let version_zero = HashedVersionFactory::create_version_zero(&wave.name);
    let bytes = |version: &resonator_model::version::HashedVersion| {
        encode(&serialize_hashed_version(version).unwrap()).unwrap()
    };
    let export = |id: &str, from: &[u8], to: &[u8]| {
        let mut request = op(OperationType::RobotExportDeltas, id, &wave);
        request.set_bytes(ParamsProperty::FromVersion, from);
        request.set_bytes(ParamsProperty::ToVersion, to);
        request
    };
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                op(OperationType::RobotExportSnapshot, "snapshot", &wave),
                export("deltas", &bytes(&version_zero), &bytes(&current)),
                export("nothing", &bytes(&current), &bytes(&current)),
                export("garbage", &[0xff, 0xff], &bytes(&current)),
                export("backwards", &bytes(&current), &bytes(&version_zero)),
                op(OperationType::RobotExportDeltas, "missing", &wave),
            ],
        )
        .await;
    // The snapshot is the JSON of a WaveletSnapshot message, in a string.
    let raw: Value =
        serde_json::from_str(&data_string(&responses[0], ParamsProperty::RawSnapshot)).unwrap();
    assert_eq!("example.com/conv+root", raw["1"]);
    assert_eq!(json!(["alice@example.com", "bob@example.com"]), raw["2"]);
    let ByteArrays(raw_deltas) = responses[1].typed(ParamsProperty::RawDeltas).unwrap();
    assert_eq!(2, raw_deltas.len());
    let target = |response: &JsonRpcResponse| {
        let encoded = response.string(ParamsProperty::TargetVersion).unwrap();
        let message: ProtocolHashedVersion = decode(&base64_bytes::decode(encoded)).unwrap();
        deserialize_hashed_version(&message).unwrap()
    };
    assert_eq!(current, target(&responses[1]));
    assert_eq!(
        Some(ByteArrays(vec![])),
        responses[2].typed(ParamsProperty::RawDeltas)
    );
    assert_eq!(current, target(&responses[2]));
    assert_eq!("Invalid version fromVersion", error_of(&responses[3]));
    assert!(responses[4].is_error());
    assert_eq!("property FROM_VERSION not found", error_of(&responses[5]));

    // Importing is off unless enabled.
    let import = |id: &str, wave_id: &str, deltas: &[Vec<u8>]| {
        OperationRequest::for_wavelet(
            OperationType::RobotImportDeltas,
            id,
            wave_id,
            "other.org/conv+root",
        )
        .with(ParamsProperty::RawDeltas, ByteArrays(deltas.to_vec()))
    };
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![import("off", "other.org/w+copy", &raw_deltas)],
        )
        .await;
    assert_eq!("robot.importDeltas is not enabled", error_of(&responses[0]));
    // Enabled: the deltas go, under their authors, into the wavelet of
    // the same id in this server's domain.
    let attachments = Arc::new(MemoryAttachments {
        stored: Mutex::new(BTreeMap::new()),
    });
    let importer = fixture
        .executor()
        .with_import_enabled(true)
        .with_attachments(attachments.clone());
    let responses = importer
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                import("first", "other.org/w+copy", &raw_deltas[..1]),
                // Repeating what is there is skipped; the rest goes on.
                import("rest", "other.org/w+copy", &raw_deltas),
                import("again", "other.org/w+copy", &raw_deltas),
                import("empty", "other.org/w+copy", &[]),
                import("garbage", "other.org/w+copy", &[vec![0xff, 0xff, 0xff]]),
            ],
        )
        .await;
    let imported_from = |response: &JsonRpcResponse| {
        response
            .get(ParamsProperty::ImportedFromVersion)
            .and_then(Value::as_i64)
    };
    assert_eq!(Some(0), imported_from(&responses[0]));
    let first_delta_end = imported_from(&responses[1]).unwrap();
    assert!(first_delta_end > 0);
    assert_eq!(Some(-1), imported_from(&responses[2]));
    assert_eq!(Some(-1), imported_from(&responses[3]));
    assert_eq!("Parse delta", error_of(&responses[4]));
    let copy = WaveletName::of_parts(DOMAIN, "w+copy", DOMAIN, "conv+root").unwrap();
    assert_eq!(
        fixture.doc_xml(&wave.name, "conversation").await,
        fixture.doc_xml(&copy, "conversation").await
    );
    let copied = fixture.server.get_snapshot(&copy).await.unwrap().unwrap();
    assert_eq!(current.version(), copied.snapshot.version());
    assert_eq!(
        snapshot.snapshot.participants(),
        copied.snapshot.participants()
    );

    // Attachments go through the attachment service.
    let attachment = RawAttachmentData {
        file_name: Some("file.txt".to_owned()),
        creator: Some("alice@example.com".to_owned()),
        data: vec![1, 2, 3],
    };
    let import_attachment = |id: &str, data: RawAttachmentData| {
        op(OperationType::RobotImportAttachment, id, &wave)
            .with(ParamsProperty::AttachmentId, "example.com/att1")
            .with(ParamsProperty::AttachmentData, data)
    };
    let export_attachment = |id: &str, attachment_id: &str| {
        OperationRequest::of(OperationType::RobotExportAttachment, id)
            .with(ParamsProperty::AttachmentId, attachment_id)
    };
    let responses = importer
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                import_attachment("import", attachment.clone()),
                export_attachment("export", "example.com/att1"),
                export_attachment("unknown", "example.com/none"),
                export_attachment("invalid", "a/b/c"),
                import_attachment(
                    "bad-creator",
                    RawAttachmentData {
                        creator: Some("nobody".to_owned()),
                        ..attachment.clone()
                    },
                ),
            ],
        )
        .await;
    assert_eq!(json!({"id": "import", "data": {}}), to_json(&responses[0]));
    assert_eq!(
        json!({"id": "export", "data": {"attachmentData": {
            "fileName": "file.txt", "creator": "alice@example.com", "data": "AQID"}}}),
        to_json(&responses[1])
    );
    assert_eq!(
        "java.io.IOException: no attachment example.com/none",
        error_of(&responses[2])
    );
    assert_eq!("Invalid id", error_of(&responses[3]));
    assert_eq!("Invalid participant nobody", error_of(&responses[4]));
    assert_eq!(
        wave.name,
        attachments.stored.lock().unwrap()["example.com/att1"].0
    );
    // Without attachment storage, and without imports enabled.
    let responses = executor
        .execute(
            &alice(),
            Registry::DataApi,
            vec![
                export_attachment("export", "example.com/att1"),
                import_attachment("import", attachment),
            ],
        )
        .await;
    assert_eq!(
        "Attachment storage is not available",
        error_of(&responses[0])
    );
    assert_eq!(
        "robot.importAttachment is not enabled",
        error_of(&responses[1])
    );
}

#[tokio::test]
async fn malformed_requests_are_errors_not_panics() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let root = &wave.root_blip;
    let raw = |params: Value| {
        let mut params = params;
        params["waveId"] = json!(wave.wave_id);
        params["waveletId"] = json!(wave.wavelet_id);
        params["blipId"] = json!(root);
        OperationRequest::from_value(&json!({
            "id": "x", "method": "document.modify", "params": params}))
        .unwrap()
    };
    let requests = vec![
        raw(
            json!({"modifyAction": {"modifyHow": "INSERT", "values": ["x"]},
            "index": -5}),
        ),
        raw(json!({"modifyAction": {"modifyHow": "DELETE"}, "range": {"start": -3, "end": 2}})),
        raw(json!({"modifyAction": {"modifyHow": "DELETE"}, "range": {"start": 5, "end": 2}})),
        raw(json!({"modifyAction": {"modifyHow": "DELETE"}, "range": {"start": 1, "end": 99}})),
        raw(
            json!({"modifyAction": {"modifyHow": "REPLACE", "values": ["x"]},
            "range": {"start": 9, "end": 3}}),
        ),
        raw(json!({"modifyAction": {"modifyHow": "ANNOTATE"}, "index": 0})),
        raw(
            json!({"modifyAction": {"modifyHow": "ANNOTATE", "annotationKey": "k"},
            "range": {"start": -1, "end": 1}}),
        ),
        raw(json!({"modifyAction": {}})),
        raw(
            json!({"modifyAction": {"modifyHow": "INSERT", "values": ["x"]},
            "modifyQuery": {}}),
        ),
        raw(json!({"modifyAction": {"modifyHow": "UPDATE_ELEMENT",
            "elements": [{"type": "GADGET", "properties": {}}]}, "index": 0})),
        raw(
            json!({"modifyAction": {"modifyHow": "INSERT", "values": ["x"]},
            "index": 2147483647}),
        ),
    ];
    let count = requests.len();
    let result = executor
        .execute_detailed(&alice(), Registry::DataApi, &requests, None)
        .await;
    assert_eq!(count, result.responses.len());
    let errors: Vec<usize> = (0..count)
        .filter(|&i| result.responses[i].is_error())
        .collect();
    // Ranges that reach past the end are cut off, a range that ends
    // before it starts is empty, and a position past the end appends, as
    // in the original; the rest is refused.
    assert_eq!(vec![0, 1, 2, 5, 7, 8, 9], errors, "{:?}", result.responses);
    let xml = fixture.doc_xml(&wave.name, root).await;
    assert!(xml.ends_with("<line/>xx</body>"), "{xml}");
}
