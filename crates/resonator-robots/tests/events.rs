//! Event generation and delivery against a real wave server. Includes
//! ports of the original's `EventGeneratorTest`, `WaveletAndDeltasTest`
//! and of what `RobotTest`, `RobotsGatewayTest` and
//! `RobotOperationApplicatorTest` check; the bundles themselves are
//! compared with the original's in `golden.rs`.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::*;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{
    TransformedWaveletDelta, WaveletDelta, WaveletOperation, WaveletOperationContext,
};
use resonator_model::version::{HashedVersion, HashedVersionFactory};
use resonator_robots::agent::{AgentError, RobotAgent, register_agent};
use resonator_robots::api::*;
use resonator_robots::convert::EventDataConverter;
use resonator_robots::events::*;
use resonator_robots::ops::{CapabilitiesFetcher, OperationExecutor, Registry};
use resonator_server::account::{AccountData, HumanAccountData, RobotAccountData};
use resonator_server::waveserver::WaveletProvider;
use tokio::sync::Semaphore;

const ROBOT: &str = "robot@example.com";

fn robot() -> ParticipantId {
    participant(ROBOT)
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

impl Wave {
    fn op(&self, operation: OperationType, id: &str) -> OperationRequest {
        OperationRequest::for_wavelet(operation, id, &self.wave_id, &self.wavelet_id)
    }

    fn add(&self, address: &str) -> OperationRequest {
        self.op(OperationType::WaveletAddParticipantNewsyntax, "add")
            .with(ParamsProperty::ParticipantId, address)
    }

    fn remove(&self, address: &str) -> OperationRequest {
        self.op(OperationType::WaveletRemoveParticipantNewsyntax, "remove")
            .with(ParamsProperty::ParticipantId, address)
    }

    fn append(&self, content: &str) -> OperationRequest {
        self.op(OperationType::WaveletAppendBlip, "append").with(
            ParamsProperty::BlipData,
            new_blip_data(&self.wave_id, &self.wavelet_id, "TBD_new", content),
        )
    }

    fn reply(&self, blip_id: &str, content: &str) -> OperationRequest {
        self.op(OperationType::BlipCreateChild, "reply")
            .with(ParamsProperty::BlipId, blip_id)
            .with(
                ParamsProperty::BlipData,
                new_blip_data(&self.wave_id, &self.wavelet_id, "TBD_reply", content),
            )
    }

    fn insert(&self, blip_id: &str, text: &str) -> OperationRequest {
        self.op(OperationType::DocumentModify, "insert")
            .with(ParamsProperty::BlipId, blip_id)
            .with(
                ParamsProperty::ModifyAction,
                DocumentModifyAction::with_values(ModifyHow::InsertAfter, [text]),
            )
    }

    /// Marks `start..end` of a blip's text as an `@`-mention of `address`.
    fn mention(&self, blip_id: &str, start: i32, end: i32, address: &str) -> OperationRequest {
        self.op(OperationType::DocumentModify, "mention")
            .with(ParamsProperty::BlipId, blip_id)
            .with(
                ParamsProperty::ModifyAction,
                DocumentModifyAction::annotation(
                    ModifyHow::Annotate,
                    "mention/user",
                    Some(address),
                ),
            )
            .with(ParamsProperty::Range, Range::new(start, end))
    }

    fn delete(&self, blip_id: &str) -> OperationRequest {
        self.op(OperationType::BlipDelete, "delete")
            .with(ParamsProperty::BlipId, blip_id)
    }
}

async fn run(
    executor: &OperationExecutor,
    actor: &ParticipantId,
    requests: Vec<OperationRequest>,
) -> Vec<JsonRpcResponse> {
    let result = executor
        .execute_detailed(actor, Registry::DataApi, &requests, None)
        .await;
    for response in &result.responses {
        assert!(!response.is_error(), "{response:?}");
    }
    assert!(result.all_submitted(), "{:?}", result.submissions);
    result.responses
}

async fn version(fixture: &Fixture, name: &WaveletName) -> HashedVersion {
    fixture
        .server
        .get_snapshot(name)
        .await
        .unwrap()
        .unwrap()
        .snapshot
        .hashed_version()
        .clone()
}

async fn deltas_since(
    fixture: &Fixture,
    name: &WaveletName,
    from: &HashedVersion,
) -> Vec<TransformedWaveletDelta> {
    let to = version(fixture, name).await;
    if *from == to {
        return Vec::new();
    }
    fixture.server.get_history(name, from, &to).await.unwrap()
}

/// The wavelet now with the deltas since a version.
async fn wavelet_and_deltas(
    fixture: &Fixture,
    name: &WaveletName,
    from: &HashedVersion,
) -> WaveletAndDeltas {
    let after = fixture
        .server
        .get_snapshot(name)
        .await
        .unwrap()
        .unwrap()
        .snapshot;
    WaveletAndDeltas::create(&after, deltas_since(fixture, name, from).await).unwrap()
}

fn all_capabilities() -> BTreeMap<EventType, Capability> {
    [
        EventType::WaveletBlipCreated,
        EventType::WaveletBlipRemoved,
        EventType::WaveletParticipantsChanged,
        EventType::WaveletSelfAdded,
        EventType::WaveletSelfRemoved,
        EventType::DocumentChanged,
        EventType::AnnotatedTextChanged,
        EventType::FormButtonClicked,
        EventType::GadgetStateChanged,
    ]
    .into_iter()
    .map(|event_type| (event_type, Capability::new(event_type)))
    .collect()
}

/// The events for the robot of everything since a version.
async fn events_since(fixture: &Fixture, wave: &Wave, from: &HashedVersion) -> GeneratedEvents {
    events_for(fixture, wave, from, ROBOT, &all_capabilities()).await
}

async fn events_for(
    fixture: &Fixture,
    wave: &Wave,
    from: &HashedVersion,
    robot: &str,
    capabilities: &BTreeMap<EventType, Capability>,
) -> GeneratedEvents {
    let wavelet_and_deltas = wavelet_and_deltas(fixture, &wave.name, from).await;
    EventGenerator::new(RobotName::from_address(robot).unwrap())
        .generate(
            wavelet_and_deltas.snapshot_before_deltas(),
            wavelet_and_deltas.deltas(),
            capabilities,
            EventDataConverter::V22,
        )
        .unwrap()
}

fn types(bundle: &EventMessageBundle) -> Vec<EventType> {
    bundle.events.iter().map(Event::event_type).collect()
}

// ---- EventGeneratorTest ----

#[tokio::test]
async fn participants_changed_and_self_events() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[ROBOT]).await;
    let start = version(&fixture, &wave.name).await;
    fixture.clock.set(1_500_000);
    run(&executor, &alice(), vec![wave.add("bob@example.com")]).await;
    let generated = events_since(&fixture, &wave, &start).await;
    let bundle = &generated.bundle;
    assert_eq!(vec![EventType::WaveletParticipantsChanged], types(bundle));
    let event = &bundle.events[0];
    assert_eq!(Some("alice@example.com"), event.modified_by.as_deref());
    // Events carry the time of their delta.
    assert_eq!(1_500_000, event.timestamp);
    assert_eq!(Some(wave.root_blip.as_str()), event.blip_id.as_deref());
    assert_eq!(
        EventProperties::WaveletParticipantsChanged {
            participants_added: vec!["bob@example.com".to_owned()],
            participants_removed: vec![],
        },
        event.properties
    );
    // The bundle shows the wavelet after the deltas and the root blip.
    assert_eq!(ROBOT, bundle.robot_address);
    assert_eq!(None, bundle.proxying_for);
    let wavelet = bundle.wavelet.as_ref().unwrap();
    assert_eq!(3, wavelet.participants.len());
    assert_eq!(
        vec![&wave.root_blip],
        bundle.blips.keys().collect::<Vec<_>>()
    );
    assert!(!generated.triggers.any());

    // Removal of someone else, then of the robot: after the event that
    // says so, nothing more.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.remove("bob@example.com")]).await;
    run(&executor, &alice(), vec![wave.remove(ROBOT)]).await;
    run(&executor, &alice(), vec![wave.append("\nunseen")]).await;
    let bundle = events_since(&fixture, &wave, &start).await.bundle;
    assert_eq!(
        vec![
            EventType::WaveletParticipantsChanged,
            EventType::WaveletSelfRemoved
        ],
        types(&bundle)
    );
    assert_eq!(
        EventProperties::WaveletParticipantsChanged {
            participants_added: vec![],
            participants_removed: vec!["bob@example.com".to_owned()],
        },
        bundle.events[0].properties
    );

    // Not a participant: nothing, until it is added.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.append("\nstill unseen")]).await;
    let generated = events_since(&fixture, &wave, &start).await;
    assert!(generated.bundle.events.is_empty());
    assert_eq!(None, generated.bundle.wavelet);
    assert!(generated.bundle.blips.is_empty());
    run(&executor, &alice(), vec![wave.add(ROBOT)]).await;
    run(&executor, &alice(), vec![wave.append("\nseen")]).await;
    let generated = events_since(&fixture, &wave, &start).await;
    assert_eq!(
        vec![
            EventType::WaveletSelfAdded,
            EventType::WaveletParticipantsChanged,
            EventType::WaveletBlipCreated,
            EventType::DocumentChanged,
        ],
        types(&generated.bundle)
    );
    assert!(generated.triggers.self_added);
}

#[tokio::test]
async fn blips_created_removed_and_changed() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[ROBOT]).await;
    let start = version(&fixture, &wave.name).await;
    let responses = run(&executor, &alice(), vec![wave.append("\nnew blip")]).await;
    let new_blip = data_string(&responses[0], ParamsProperty::NewBlipId);
    let bundle = events_since(&fixture, &wave, &start).await.bundle;
    // The content of the new blip arrives after it joined the
    // conversation, so the document changes too.
    assert_eq!(
        vec![EventType::WaveletBlipCreated, EventType::DocumentChanged],
        types(&bundle)
    );
    assert_eq!(
        EventProperties::WaveletBlipCreated {
            new_blip_id: Some(new_blip.clone()),
        },
        bundle.events[0].properties
    );
    assert_eq!(
        Some(wave.root_blip.as_str()),
        bundle.events[0].blip_id.as_deref()
    );
    assert_eq!(Some(new_blip.as_str()), bundle.events[1].blip_id.as_deref());
    assert_eq!("\nnew blip", bundle.blips[&new_blip].content);

    // A document changes many times: one event per blip and bundle.
    let start = version(&fixture, &wave.name).await;
    for text in ["a", "b", "c"] {
        run(
            &executor,
            &alice(),
            vec![wave.insert(&wave.root_blip, text)],
        )
        .await;
    }
    run(
        &executor,
        &bob_on(&executor, &wave).await,
        vec![wave.insert(&new_blip, "!")],
    )
    .await;
    let bundle = events_since(&fixture, &wave, &start).await.bundle;
    assert_eq!(
        vec![
            EventType::DocumentChanged,
            EventType::WaveletParticipantsChanged,
            EventType::DocumentChanged
        ],
        types(&bundle)
    );
    assert_eq!(
        Some(wave.root_blip.as_str()),
        bundle.events[0].blip_id.as_deref()
    );
    assert_eq!("\nabc", bundle.blips[&wave.root_blip].content);

    // Removed.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.delete(&new_blip)]).await;
    let bundle = events_since(&fixture, &wave, &start).await.bundle;
    assert_eq!(
        EventProperties::WaveletBlipRemoved {
            removed_blip_id: Some(new_blip.clone()),
        },
        bundle.events[0].properties
    );
    assert!(!bundle.blips.contains_key(&new_blip));
}

/// Adds Bob to the wave and returns him.
async fn bob_on(executor: &OperationExecutor, wave: &Wave) -> ParticipantId {
    run(executor, &alice(), vec![wave.add("bob@example.com")]).await;
    bob()
}

#[tokio::test]
async fn annotated_text_changed() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[ROBOT]).await;
    run(
        &executor,
        &alice(),
        vec![wave.insert(&wave.root_blip, "some text")],
    )
    .await;
    let start = version(&fixture, &wave.name).await;
    let annotate = wave
        .op(OperationType::DocumentModify, "annotate")
        .with(ParamsProperty::BlipId, &wave.root_blip)
        .with(
            ParamsProperty::ModifyAction,
            DocumentModifyAction::annotation(ModifyHow::Annotate, "style/color", Some("red")),
        )
        .with(ParamsProperty::Range, Range::new(1, 5));
    run(&executor, &alice(), vec![annotate]).await;
    let capabilities = BTreeMap::from([(
        EventType::AnnotatedTextChanged,
        Capability::new(EventType::AnnotatedTextChanged),
    )]);
    let bundle = events_for(&fixture, &wave, &start, ROBOT, &capabilities)
        .await
        .bundle;
    assert_eq!(vec![EventType::AnnotatedTextChanged], types(&bundle));
    assert_eq!(
        EventProperties::AnnotatedTextChanged {
            name: Some("style/color".to_owned()),
            value: Some("red".to_owned()),
        },
        bundle.events[0].properties
    );
    assert_eq!(
        vec![Annotation::new("style/color", "red", 1, 5)],
        bundle.blips[&wave.root_blip].annotations
    );
    // Without the capability, no event; an annotation alone is no
    // document change either.
    let capabilities = BTreeMap::from([(
        EventType::DocumentChanged,
        Capability::new(EventType::DocumentChanged),
    )]);
    let bundle = events_for(&fixture, &wave, &start, ROBOT, &capabilities)
        .await
        .bundle;
    assert!(bundle.events.is_empty());
}

#[tokio::test]
async fn mentions_of_the_robot_are_triggers() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    run(
        &executor,
        &alice(),
        vec![wave.insert(&wave.root_blip, "Hi robot and bob")],
    )
    .await;
    let mentioned_by_alice = vec![MentionTrigger {
        blip_id: wave.root_blip.clone(),
        by: "alice@example.com".to_owned(),
    }];

    // Not on the wave: a mention alone is nothing.
    let start = version(&fixture, &wave.name).await;
    run(
        &executor,
        &alice(),
        vec![wave.mention(&wave.root_blip, 4, 9, ROBOT)],
    )
    .await;
    let generated = events_since(&fixture, &wave, &start).await;
    assert!(!generated.triggers.any());

    // Mentioned and then added (the web client adds whoever is
    // mentioned): both triggers, whatever the order.
    run(
        &executor,
        &alice(),
        vec![wave.mention(&wave.root_blip, 4, 9, ROBOT), wave.add(ROBOT)],
    )
    .await;
    let generated = events_since(&fixture, &wave, &start).await;
    assert!(generated.triggers.self_added);
    assert_eq!(mentioned_by_alice, generated.triggers.mentions);

    // On the wave: someone else's edit of a blip that mentions it is one
    // (as an edit of a direct reply is); a mention of somebody else is not.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.insert(&wave.root_blip, "?")]).await;
    let generated = events_since(&fixture, &wave, &start).await;
    assert_eq!(mentioned_by_alice, generated.triggers.mentions);
    assert!(generated.triggers.any() && !generated.triggers.self_added);
    let other = wave.append("\nA note for bob");
    run(&executor, &alice(), vec![other]).await;
    let note = events_since(&fixture, &wave, &start)
        .await
        .bundle
        .blips
        .keys()
        .find(|id| **id != wave.root_blip)
        .cloned()
        .unwrap();
    let start = version(&fixture, &wave.name).await;
    run(
        &executor,
        &alice(),
        vec![wave.mention(&note, 12, 15, "bob@example.com")],
    )
    .await;
    assert!(
        events_since(&fixture, &wave, &start)
            .await
            .triggers
            .mentions
            .is_empty()
    );

    // Annotating text alone makes no document-changed event, but it is a
    // mention all the same.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.mention(&note, 1, 2, ROBOT)]).await;
    let capabilities = BTreeMap::from([(
        EventType::DocumentChanged,
        Capability::new(EventType::DocumentChanged),
    )]);
    let generated = events_for(&fixture, &wave, &start, ROBOT, &capabilities).await;
    assert!(generated.bundle.events.is_empty());
    assert_eq!(
        vec![MentionTrigger {
            blip_id: note.clone(),
            by: "alice@example.com".to_owned(),
        }],
        generated.triggers.mentions
    );

    // Its own edits never are.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &robot(), vec![wave.insert(&note, " ok")]).await;
    assert!(
        events_since(&fixture, &wave, &start)
            .await
            .triggers
            .mentions
            .is_empty()
    );
}

#[tokio::test]
async fn a_robot_gets_no_events_for_its_own_deltas() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[ROBOT]).await;
    // The robot's deltas first, then a person's.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &robot(), vec![wave.append("\nby the robot")]).await;
    run(&executor, &robot(), vec![wave.add("bob@example.com")]).await;
    let own = events_since(&fixture, &wave, &start).await;
    assert!(own.bundle.events.is_empty());
    run(
        &executor,
        &alice(),
        vec![wave.insert(&wave.root_blip, "by alice")],
    )
    .await;
    let bundle = events_since(&fixture, &wave, &start).await.bundle;
    assert_eq!(vec![EventType::DocumentChanged], types(&bundle));
    assert_eq!(
        Some("alice@example.com"),
        bundle.events[0].modified_by.as_deref()
    );
    // A person's delta first, then the robot's.
    let start = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.append("\nby alice")]).await;
    run(
        &executor,
        &robot(),
        vec![wave.insert(&wave.root_blip, "by the robot")],
    )
    .await;
    let bundle = events_since(&fixture, &wave, &start).await.bundle;
    assert_eq!(
        vec![EventType::WaveletBlipCreated, EventType::DocumentChanged],
        types(&bundle)
    );
    assert!(
        bundle
            .events
            .iter()
            .all(|e| e.modified_by.as_deref() == Some("alice@example.com"))
    );
    // Its proxy is someone else: each gets the other's events.
    let proxy = "robot+proxy@example.com";
    run(&executor, &alice(), vec![wave.add(proxy)]).await;
    let start = version(&fixture, &wave.name).await;
    run(&executor, &robot(), vec![wave.insert(&wave.root_blip, "!")]).await;
    assert!(
        events_since(&fixture, &wave, &start)
            .await
            .bundle
            .events
            .is_empty()
    );
    let bundle = events_for(&fixture, &wave, &start, proxy, &all_capabilities())
        .await
        .bundle;
    assert_eq!(vec![EventType::DocumentChanged], types(&bundle));
    assert_eq!(ROBOT, bundle.robot_address);
    assert_eq!(Some("proxy"), bundle.proxying_for.as_deref());
    assert_eq!(
        Some("proxy"),
        serde_json::to_value(&bundle).unwrap()["proxyingFor"].as_str()
    );
}

#[tokio::test]
async fn a_robot_on_a_new_wave_learns_that_it_was_added() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    // The delta that creates the wave also adds the robot. (The original
    // generates no events at all for a wavelet that had no conversation
    // before the deltas.)
    let wave = new_wave(&executor, &alice(), &[ROBOT]).await;
    let version_zero = HashedVersionFactory::create_version_zero(&wave.name);
    let generated = events_since(&fixture, &wave, &version_zero).await;
    assert_eq!(
        vec![
            EventType::WaveletSelfAdded,
            EventType::WaveletParticipantsChanged
        ],
        types(&generated.bundle)
    );
    assert!(generated.triggers.self_added);
    assert_eq!(
        Some(wave.root_blip.clone()),
        generated.bundle.wavelet.unwrap().root_blip_id
    );
}

#[tokio::test]
async fn contexts_choose_the_blips_of_a_bundle() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[ROBOT]).await;
    let root = wave.root_blip.clone();
    let responses = run(
        &executor,
        &alice(),
        vec![wave.reply(&root, "\nreply"), wave.append("\nsecond")],
    )
    .await;
    let reply = data_string(&responses[0], ParamsProperty::NewBlipId);
    let second = data_string(&responses[1], ParamsProperty::NewBlipId);
    let responses = run(&executor, &alice(), vec![wave.reply(&reply, "\nnested")]).await;
    let nested = data_string(&responses[0], ParamsProperty::NewBlipId);
    let start = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.insert(&reply, "!")]).await;
    let blips_with = |contexts: Vec<Context>| {
        let capabilities = BTreeMap::from([(
            EventType::DocumentChanged,
            Capability::with_contexts(EventType::DocumentChanged, contexts),
        )]);
        let fixture = &fixture;
        let wave = &wave;
        let start = &start;
        async move {
            let bundle = events_for(fixture, wave, start, ROBOT, &capabilities)
                .await
                .bundle;
            let mut ids: Vec<String> = bundle.blips.keys().cloned().collect();
            ids.sort();
            ids
        }
    };
    let sorted = |ids: &[&String]| {
        let mut ids: Vec<String> = ids.iter().map(|id| (*id).clone()).collect();
        ids.sort();
        ids
    };
    assert_eq!(sorted(&[&reply]), blips_with(vec![Context::SelfOnly]).await);
    assert_eq!(
        sorted(&[&reply, &root]),
        blips_with(vec![Context::Root]).await
    );
    assert_eq!(
        sorted(&[&reply, &root]),
        blips_with(vec![Context::Parent]).await
    );
    assert_eq!(
        sorted(&[&reply, &nested]),
        blips_with(vec![Context::Children]).await
    );
    assert_eq!(sorted(&[&reply]), blips_with(vec![Context::Siblings]).await);
    assert_eq!(
        sorted(&[&reply, &root, &nested]),
        blips_with(Capability::DEFAULT_CONTEXTS.to_vec()).await
    );
    assert_eq!(
        sorted(&[&reply, &root, &nested, &second]),
        blips_with(vec![Context::All]).await
    );
}

// ---- WaveletAndDeltasTest ----

#[tokio::test]
async fn wavelet_before_and_after_deltas() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[]).await;
    let created = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap()
        .snapshot;
    let start = created.hashed_version().clone();
    run(&executor, &alice(), vec![wave.append("\none")]).await;
    let middle = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap()
        .snapshot;
    let first = deltas_since(&fixture, &wave.name, &start).await;
    run(&executor, &alice(), vec![wave.add("bob@example.com")]).await;
    let end = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap()
        .snapshot;
    let second = deltas_since(&fixture, &wave.name, middle.hashed_version()).await;

    let mut both = WaveletAndDeltas::create(&middle, first.clone()).unwrap();
    assert_eq!(&wave.name, both.wavelet_name());
    // The state before is the state after with the deltas undone.
    let before = both.snapshot_before_deltas();
    assert_eq!(created.version(), before.version());
    assert_eq!(created.participants(), before.participants());
    assert_eq!(
        created.document("conversation").unwrap().content(),
        before.document("conversation").unwrap().content()
    );
    assert_eq!(&middle, both.snapshot_after_deltas());
    assert_eq!(middle.hashed_version(), &both.version_after_deltas());
    assert_eq!(first.len(), both.deltas().len());
    // Deltas that follow can be appended, others not.
    assert!(both.are_contiguous_to_current_version(&second));
    assert!(!both.are_contiguous_to_current_version(&first));
    assert!(both.append_deltas(&end, first.clone()).is_err());
    assert!(both.append_deltas(&middle, second.clone()).is_err());
    assert!(both.append_deltas(&end, Vec::new()).is_err());
    both.append_deltas(&end, second.clone()).unwrap();
    assert_eq!(&end, both.snapshot_after_deltas());
    assert_eq!(end.hashed_version(), &both.version_after_deltas());
    assert_eq!(first.len() + second.len(), both.deltas().len());
    assert_eq!(created.version(), both.snapshot_before_deltas().version());
    // A snapshot that is not the state after the deltas is refused.
    assert!(WaveletAndDeltas::create(&end, first).is_err());
    // Without deltas, before and after are the same.
    let none = WaveletAndDeltas::create(&end, Vec::new()).unwrap();
    assert_eq!(none.snapshot_before_deltas(), none.snapshot_after_deltas());
    assert_eq!(end.hashed_version(), &none.version_after_deltas());
    // Undoing by hand.
    let mut undone = end.clone();
    rollback(&mut undone, &second).unwrap();
    assert_eq!(middle.participants(), undone.participants());
    assert_eq!(middle.version(), undone.version());
    let op = &second[0].ops()[0];
    let inverse = invert_operation(op).unwrap();
    assert_eq!(-1, inverse.context().version_increment());
}

// ---- The dispatcher with in-process agents ----

struct Setup {
    fixture: Fixture,
    executor: Arc<OperationExecutor>,
    dispatcher: RobotDispatcher,
    _handle: DispatcherHandle,
}

fn setup(options: DispatcherOptions) -> Setup {
    let fixture = Fixture::new();
    let executor = Arc::new(fixture.executor());
    let dispatcher = RobotDispatcher::new(executor.clone(), options);
    let handle = dispatcher.start();
    Setup {
        fixture,
        executor,
        dispatcher,
        _handle: handle,
    }
}

impl Setup {
    async fn agent(&self, id: &str) -> (Arc<EchoAgent>, ParticipantId) {
        let agent = EchoAgent::new(id);
        let address = register_agent(
            &self.dispatcher,
            &self.fixture.db.account_store(),
            agent.clone(),
        )
        .await
        .unwrap();
        (agent, address)
    }

    /// The blips of a wavelet with their authors and text, in id order.
    async fn blips(&self, name: &WaveletName) -> Vec<(String, String, String)> {
        let snapshot = self
            .fixture
            .server
            .get_snapshot(name)
            .await
            .unwrap()
            .unwrap();
        snapshot
            .snapshot
            .documents()
            .filter(|doc| doc.id().starts_with("b+"))
            .map(|doc| {
                let view = resonator_model::doc::DocView::new(doc.content());
                (
                    doc.id().to_owned(),
                    doc.author().address().to_owned(),
                    view.text_content(view.root()),
                )
            })
            .collect()
    }
}

#[tokio::test]
async fn an_agent_is_added_gets_the_event_and_replies() {
    let setup = setup(DispatcherOptions::default());
    let (echo, echo_id) = setup.agent("echo").await;
    assert_eq!("echo@example.com", echo_id.address());
    // Registering made it a robot account.
    let account = setup
        .fixture
        .db
        .account_store()
        .get_account(&echo_id)
        .await
        .unwrap();
    assert!(matches!(&account, Some(AccountData::Robot(robot)) if robot.is_agent()));
    assert!(setup.dispatcher.is_registered(&echo_id));

    let wave = new_wave(&setup.executor, &alice(), &[]).await;
    setup.dispatcher.idle().await;
    assert!(echo.deliveries().is_empty());
    let before = version(&setup.fixture, &wave.name).await;
    run(&setup.executor, &alice(), vec![wave.add(echo_id.address())]).await;
    setup.dispatcher.idle().await;

    // The agent got one bundle: it was added.
    let deliveries = echo.deliveries();
    assert_eq!(1, deliveries.len());
    let delivery = &deliveries[0];
    assert_eq!(
        vec![
            EventType::WaveletSelfAdded,
            EventType::WaveletParticipantsChanged
        ],
        types(&delivery.bundle)
    );
    assert_eq!("echo@example.com", delivery.robot.to_participant_address());
    assert_eq!(wave.name, delivery.wavelet_name);
    assert_eq!(0, delivery.hop_count);
    assert!(delivery.triggers.self_added);
    assert_eq!(ProtocolVersion::V2_2, delivery.protocol_version);
    assert_eq!(
        Some(wave.root_blip.as_str()),
        delivery.bundle.events[0].blip_id.as_deref()
    );
    assert!(delivery.bundle.blips.contains_key(&wave.root_blip));

    // Its reply is on the wave, in a delta authored by the agent.
    let deltas = deltas_since(&setup.fixture, &wave.name, &before).await;
    assert_eq!(2, deltas.len());
    assert_eq!(&alice(), deltas[0].author());
    assert_eq!(&echo_id, deltas[1].author());
    let blips = setup.blips(&wave.name).await;
    assert_eq!(2, blips.len());
    assert_eq!(
        ("echo@example.com", "hello from echo"),
        (blips[1].1.as_str(), blips[1].2.as_str())
    );
    // It got no events for its own delta.
    assert_eq!(
        DispatcherStats {
            bundles_delivered: 1,
            ..DispatcherStats::default()
        },
        setup.dispatcher.stats()
    );
}

#[tokio::test]
async fn an_agent_answers_blips_but_not_its_own() {
    let setup = setup(DispatcherOptions::default());
    let (echo, echo_id) = setup.agent("echo").await;
    let wave = new_wave(&setup.executor, &alice(), &[echo_id.address()]).await;
    setup.dispatcher.idle().await;
    // Added by the delta that created the wave: greeted.
    assert_eq!(1, echo.deliveries().len());
    assert_eq!(2, setup.blips(&wave.name).await.len());

    run(
        &setup.executor,
        &alice(),
        vec![wave.reply(&wave.root_blip, "\nping")],
    )
    .await;
    setup.dispatcher.idle().await;
    let blips = setup.blips(&wave.name).await;
    let texts: Vec<&str> = blips.iter().map(|b| b.2.as_str()).collect();
    assert_eq!(
        vec!["", "hello from echo", "ping", "echo heard: ping"],
        texts
    );
    assert_eq!("echo@example.com", blips[3].1);
    // Two bundles in all; none for the blips the agent made itself.
    let event_types = echo.event_types();
    assert_eq!(2, event_types.len(), "{event_types:?}");
    assert_eq!(vec![EventType::WaveletBlipCreated], event_types[1]);
    for delivery in echo.deliveries() {
        for event in &delivery.bundle.events {
            assert_ne!(Some("echo@example.com"), event.modified_by.as_deref());
        }
    }
    // Alice replied to the root blip, which is hers: no trigger. A reply
    // to the agent's blip is one.
    assert!(!echo.deliveries()[1].triggers.any());
    let agents_blip = blips[3].0.clone();
    let responses = run(
        &setup.executor,
        &alice(),
        vec![wave.reply(&agents_blip, "\nthanks")],
    )
    .await;
    let thanks = data_string(&responses[0], ParamsProperty::NewBlipId);
    setup.dispatcher.idle().await;
    let last = echo.deliveries().pop().unwrap();
    assert_eq!(vec![thanks.clone()], last.triggers.reply_blips_created);
    assert!(last.triggers.any() && !last.triggers.self_added);
    // And so is a change to that reply.
    let count = echo.deliveries().len();
    run(
        &setup.executor,
        &alice(),
        vec![wave.insert(&thanks, " a lot")],
    )
    .await;
    setup.dispatcher.idle().await;
    // The echo agent does not ask for document changes, so no bundle.
    assert_eq!(count, echo.deliveries().len());
    let start = version(&setup.fixture, &wave.name).await;
    run(&setup.executor, &alice(), vec![wave.insert(&thanks, "!")]).await;
    let generated = events_for(
        &setup.fixture,
        &wave,
        &start,
        "echo@example.com",
        &all_capabilities(),
    )
    .await;
    assert_eq!(vec![thanks], generated.triggers.reply_blips_changed);
}

#[tokio::test]
async fn the_loop_guard_stops_two_echo_agents() {
    let setup = setup(DispatcherOptions {
        max_hops: Some(3),
        ..DispatcherOptions::default()
    });
    let (ping, ping_id) = setup.agent("ping").await;
    let (pong, pong_id) = setup.agent("pong").await;
    let wave = new_wave(&setup.executor, &alice(), &[]).await;
    // Each greets when added; each answers the other's greeting, and the
    // answer to that, and so on: without the guard this never ends.
    run(
        &setup.executor,
        &alice(),
        vec![wave.add(ping_id.address()), wave.add(pong_id.address())],
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), setup.dispatcher.idle())
        .await
        .expect("the agents stop answering each other");
    let stats = setup.dispatcher.stats();
    assert!(stats.bundles_suppressed >= 1, "{stats:?}");
    let hops = |agent: &EchoAgent| -> Vec<u32> {
        agent.deliveries().iter().map(|d| d.hop_count).collect()
    };
    let (ping_hops, pong_hops) = (hops(&ping), hops(&pong));
    assert_eq!(0, ping_hops[0]);
    assert_eq!(0, pong_hops[0]);
    // Chains of answers were followed up to the limit and no further.
    let max = ping_hops.iter().chain(&pong_hops).copied().max().unwrap();
    assert_eq!(3, max, "{ping_hops:?} {pong_hops:?}");
    assert!(ping_hops.len() + pong_hops.len() >= 4);
    // The wave is quiet now, and a person's blip is answered again, by
    // both, with the count starting over.
    let before = setup.blips(&wave.name).await.len();
    let delivered = stats.bundles_delivered;
    run(
        &setup.executor,
        &alice(),
        vec![wave.append("\nhello, both")],
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), setup.dispatcher.idle())
        .await
        .expect("the agents stop again");
    assert!(setup.blips(&wave.name).await.len() >= before + 3);
    assert!(setup.dispatcher.stats().bundles_delivered >= delivered + 2);
    assert!(hops(&ping).contains(&0) && hops(&ping)[ping_hops.len()..].contains(&0));
}

/// An agent that answers nothing but acts later, through the reactor.
struct LateAgent {
    deliveries: Mutex<Vec<EventDelivery>>,
}

#[async_trait]
impl RobotAgent for LateAgent {
    fn robot_id(&self) -> &str {
        "late"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new(EventType::WaveletSelfAdded)]
    }

    async fn on_events(&self, delivery: EventDelivery) -> Vec<OperationRequest> {
        self.deliveries.lock().unwrap().push(delivery);
        Vec::new()
    }
}

#[tokio::test]
async fn acting_later_through_the_reactor_counts_as_a_reaction() {
    let setup = setup(DispatcherOptions::default());
    let late = Arc::new(LateAgent {
        deliveries: Mutex::new(Vec::new()),
    });
    let late_id = register_agent(
        &setup.dispatcher,
        &setup.fixture.db.account_store(),
        late.clone(),
    )
    .await
    .unwrap();
    let (echo, echo_id) = setup.agent("echo").await;
    let wave = new_wave(&setup.executor, &alice(), &[echo_id.address()]).await;
    setup.dispatcher.idle().await;
    let echo_count = echo.deliveries().len();
    run(&setup.executor, &alice(), vec![wave.add(late_id.address())]).await;
    setup.dispatcher.idle().await;
    let delivery = late.deliveries.lock().unwrap().pop().unwrap();
    assert_eq!(0, delivery.hop_count);
    assert_eq!(&late_id, delivery.reactor.account_id());
    // The echo agent saw the late agent being added (hop 0).
    let seen = echo.deliveries();
    assert_eq!(echo_count + 1, seen.len());
    // Long after its reply, the late agent streams a blip: created empty,
    // then filled piece by piece.
    let result = delivery
        .reactor
        .execute(vec![wave.reply(&wave.root_blip, "\n")])
        .await;
    assert!(result.all_submitted());
    let blip = data_string(&result.responses[0], ParamsProperty::NewBlipId);
    for piece in ["Thinking", " about", " it."] {
        let result = delivery
            .reactor
            .execute(vec![wave.insert(&blip, piece)])
            .await;
        assert!(result.all_submitted() && !result.responses[0].is_error());
    }
    setup.dispatcher.idle().await;
    assert_eq!(
        "<body><line/>Thinking about it.</body>",
        setup.fixture.doc_xml(&wave.name, &blip).await
    );
    // The echo agent was told of the new blip as the reaction it is: one
    // hop on.
    let later: Vec<EventDelivery> = echo.deliveries().split_off(echo_count + 1);
    assert!(!later.is_empty());
    assert!(later.iter().all(|d| d.hop_count == 1), "{later:?}");
    // What the late agent does outside any delivery is no reaction.
    let count = echo.deliveries().len();
    run(&setup.executor, &late_id, vec![wave.append("\non my own")]).await;
    setup.dispatcher.idle().await;
    let own: Vec<EventDelivery> = echo.deliveries().split_off(count);
    assert!(own.iter().any(|d| d.hop_count == 0), "{own:?}");
}

#[tokio::test]
async fn registering_agents() {
    let setup = setup(DispatcherOptions::default());
    let accounts = setup.fixture.db.account_store();
    accounts
        .put_account(AccountData::Human(HumanAccountData::new(participant(
            "human@example.com",
        ))))
        .await
        .unwrap();
    let human = EchoAgent::new("human");
    assert!(matches!(
        register_agent(&setup.dispatcher, &accounts, human).await,
        Err(AgentError::NotARobot(_))
    ));
    let invalid = EchoAgent::new("not valid");
    assert!(matches!(
        register_agent(&setup.dispatcher, &accounts, invalid).await,
        Err(AgentError::InvalidAddress(_))
    ));
    // Registering again is fine; unregistering stops deliveries.
    let (echo, echo_id) = setup.agent("echo").await;
    let (_, again) = setup.agent("echo").await;
    assert_eq!(echo_id, again);
    setup.dispatcher.unregister(&echo_id);
    assert!(!setup.dispatcher.is_registered(&echo_id));
    let wave = new_wave(&setup.executor, &alice(), &[echo_id.address()]).await;
    setup.dispatcher.idle().await;
    assert!(echo.deliveries().is_empty());
    assert_eq!(1, setup.blips(&wave.name).await.len());
}

// ---- The dispatcher with other sinks ----

/// A sink that records, optionally waits for a permit before answering,
/// and answers with fixed operations or an error.
struct TestSink {
    deliveries: Mutex<Vec<EventDelivery>>,
    gate: Option<Arc<Semaphore>>,
    reply: Mutex<Vec<OperationRequest>>,
    fail: bool,
}

impl TestSink {
    fn new(gate: Option<Arc<Semaphore>>) -> Arc<Self> {
        Arc::new(TestSink {
            deliveries: Mutex::new(Vec::new()),
            gate,
            reply: Mutex::new(Vec::new()),
            fail: false,
        })
    }

    fn deliveries(&self) -> Vec<EventDelivery> {
        self.deliveries.lock().unwrap().clone()
    }
}

#[async_trait]
impl EventSink for TestSink {
    async fn deliver(&self, delivery: EventDelivery) -> Result<Vec<OperationRequest>, SinkError> {
        self.deliveries.lock().unwrap().push(delivery);
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        if self.fail {
            return Err(SinkError("connection refused".to_owned()));
        }
        Ok(std::mem::take(&mut *self.reply.lock().unwrap()))
    }
}

/// Waits until the sink has been handed a number of bundles.
async fn wait_for_deliveries(sink: &TestSink, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while sink.deliveries().len() < count {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the bundle is delivered");
}

fn endpoint(sink: Arc<TestSink>, events: &[EventType]) -> RobotEndpoint {
    RobotEndpoint {
        account_id: robot(),
        capabilities: RobotCapabilities::new(
            events.iter().map(|&e| Capability::new(e)),
            "hash",
            ProtocolVersion::V2_2,
        ),
        sink,
        registry: Registry::Passive,
    }
}

#[tokio::test]
async fn a_slow_robot_gets_merged_bundles_and_does_not_hold_up_the_server() {
    let setup = setup(DispatcherOptions::default());
    let gate = Arc::new(Semaphore::new(0));
    let sink = TestSink::new(Some(gate.clone()));
    setup.dispatcher.register(endpoint(
        sink.clone(),
        &[EventType::WaveletSelfAdded, EventType::WaveletBlipCreated],
    ));
    let wave = new_wave(&setup.executor, &alice(), &[ROBOT]).await;
    wait_for_deliveries(&sink, 1).await;
    // The robot is stuck in its first delivery; the server goes on.
    for text in ["\none", "\ntwo", "\nthree"] {
        run(&setup.executor, &alice(), vec![wave.append(text)]).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(1, sink.deliveries().len());
    assert_eq!(4, setup.blips(&wave.name).await.len());
    // Released, it gets the three updates it missed as one bundle.
    gate.add_permits(10);
    setup.dispatcher.idle().await;
    let deliveries = sink.deliveries();
    assert_eq!(2, deliveries.len());
    assert_eq!(
        vec![EventType::WaveletBlipCreated; 3],
        types(&deliveries[1].bundle)
    );
    assert_eq!(
        4,
        deliveries[1]
            .bundle
            .wavelet
            .as_ref()
            .unwrap()
            .root_thread
            .as_ref()
            .unwrap()
            .blip_ids
            .len()
    );
}

#[tokio::test]
async fn a_full_queue_drops_the_oldest_updates() {
    let setup = setup(DispatcherOptions {
        max_pending_deltas: 2,
        ..DispatcherOptions::default()
    });
    let gate = Arc::new(Semaphore::new(0));
    let sink = TestSink::new(Some(gate.clone()));
    setup
        .dispatcher
        .register(endpoint(sink.clone(), &[EventType::WaveletSelfAdded]));
    // The first wave's update is being delivered; four more wait.
    let mut waves = Vec::new();
    for i in 0..5 {
        waves.push(new_wave(&setup.executor, &alice(), &[ROBOT]).await);
        if i == 0 {
            wait_for_deliveries(&sink, 1).await;
        }
    }
    // Let the dispatcher queue what the server announced.
    tokio::time::sleep(Duration::from_millis(300)).await;
    gate.add_permits(10);
    setup.dispatcher.idle().await;
    // The first, and the two newest of the waiting ones.
    let delivered: Vec<WaveletName> = sink
        .deliveries()
        .iter()
        .map(|d| d.wavelet_name.clone())
        .collect();
    assert_eq!(
        vec![
            waves[0].name.clone(),
            waves[3].name.clone(),
            waves[4].name.clone()
        ],
        delivered
    );
    assert_eq!(2, setup.dispatcher.stats().deltas_dropped);
}

#[tokio::test]
async fn failed_and_late_deliveries_are_given_up() {
    let setup = setup(DispatcherOptions {
        delivery_timeout: Some(Duration::from_millis(50)),
        ..DispatcherOptions::default()
    });
    // One robot fails, one never answers.
    let failing = Arc::new(TestSink {
        deliveries: Mutex::new(Vec::new()),
        gate: None,
        reply: Mutex::new(Vec::new()),
        fail: true,
    });
    setup
        .dispatcher
        .register(endpoint(failing.clone(), &[EventType::WaveletSelfAdded]));
    let stuck = TestSink::new(Some(Arc::new(Semaphore::new(0))));
    setup.dispatcher.register(RobotEndpoint {
        account_id: participant("stuck@example.com"),
        ..endpoint(stuck.clone(), &[EventType::WaveletSelfAdded])
    });
    let wave = new_wave(&setup.executor, &alice(), &[ROBOT, "stuck@example.com"]).await;
    setup.dispatcher.idle().await;
    assert_eq!(1, failing.deliveries().len());
    assert_eq!(1, stuck.deliveries().len());
    let stats = setup.dispatcher.stats();
    assert_eq!((2, 2), (stats.bundles_delivered, stats.deliveries_failed));
    // Nothing is retried, and later events still come.
    assert_eq!(1, setup.blips(&wave.name).await.len());
}

#[tokio::test]
async fn robots_without_the_capability_or_account_get_nothing() {
    let setup = setup(DispatcherOptions::default());
    let sink = TestSink::new(None);
    setup
        .dispatcher
        .register(endpoint(sink.clone(), &[EventType::WaveletBlipRemoved]));
    // On the wave, but nothing it asked for happens; and an address that
    // is no robot's is ignored.
    let wave = new_wave(&setup.executor, &alice(), &[ROBOT, "nobody@example.com"]).await;
    run(&setup.executor, &alice(), vec![wave.append("\ntext")]).await;
    setup.dispatcher.idle().await;
    assert!(sink.deliveries().is_empty());
    assert_eq!(DispatcherStats::default(), setup.dispatcher.stats());
}

struct FixedCapabilities(RobotCapabilities);

#[async_trait]
impl CapabilitiesFetcher for FixedCapabilities {
    async fn fetch_capabilities(&self, _: &RobotAccountData) -> Result<RobotCapabilities, String> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn external_robots_of_the_account_store() {
    let setup = setup(DispatcherOptions::default());
    let accounts = setup.fixture.db.account_store();
    let capabilities = RobotCapabilities::new(
        [Capability::new(EventType::WaveletSelfAdded)],
        "hash",
        ProtocolVersion::V2_1,
    );
    let robot_account = |address: &str, capabilities: Option<RobotCapabilities>, verified| {
        AccountData::Robot(
            RobotAccountData::robot(
                participant(address),
                "http://robot.example",
                None,
                capabilities,
                verified,
            )
            .unwrap(),
        )
    };
    accounts
        .put_account(robot_account(ROBOT, Some(capabilities.clone()), true))
        .await
        .unwrap();
    accounts
        .put_account(robot_account("fresh@example.com", None, true))
        .await
        .unwrap();
    accounts
        .put_account(robot_account(
            "unverified@example.com",
            Some(capabilities.clone()),
            false,
        ))
        .await
        .unwrap();
    accounts
        .put_account(AccountData::Human(HumanAccountData::new(bob())))
        .await
        .unwrap();
    let sink = TestSink::new(None);
    setup
        .dispatcher
        .set_directory(Some(Arc::new(AccountRobotDirectory::new(
            accounts.clone(),
            sink.clone(),
            Some(Arc::new(FixedCapabilities(capabilities.clone()))),
        ))));
    let wave = new_wave(
        &setup.executor,
        &alice(),
        &[
            ROBOT,
            "fresh@example.com",
            "unverified@example.com",
            "bob@example.com",
        ],
    )
    .await;
    // The robot's reply: passive robots answer with operations.
    *sink.reply.lock().unwrap() = vec![
        wave.append("\nfrom a passive robot"),
        // Not for passive robots.
        wave.op(OperationType::RobotFetchWave, "fetch"),
    ];
    setup.dispatcher.idle().await;
    let mut robots: Vec<String> = sink
        .deliveries()
        .iter()
        .map(|d| d.robot.to_participant_address())
        .collect();
    robots.sort();
    assert_eq!(vec!["fresh@example.com", ROBOT], robots);
    // Protocol 0.21: the bundle has no threads.
    let delivery = &sink.deliveries()[0];
    assert_eq!(ProtocolVersion::V2_1, delivery.protocol_version);
    assert_eq!(None, delivery.bundle.wavelet.as_ref().unwrap().root_thread);
    // The capabilities of the robot that had none were fetched and kept.
    match accounts
        .get_account(&participant("fresh@example.com"))
        .await
        .unwrap()
    {
        Some(AccountData::Robot(robot)) => assert_eq!(Some(capabilities), robot.capabilities),
        other => panic!("{other:?}"),
    }
    // The reply of whichever robot was served first is on the wave,
    // authored by that robot.
    let blips = setup.blips(&wave.name).await;
    assert_eq!(2, blips.len());
    assert_eq!("from a passive robot", blips[1].2);
    assert!(robots.contains(&blips[1].1));
}

#[tokio::test]
async fn updates_can_be_fed_by_hand() {
    // Without subscribing to the bus.
    let fixture = Fixture::new();
    let executor = Arc::new(fixture.executor());
    let dispatcher = RobotDispatcher::new(executor.clone(), DispatcherOptions::default());
    let sink = TestSink::new(None);
    dispatcher.register(endpoint(sink.clone(), &[EventType::WaveletSelfAdded]));
    let wave = new_wave(&executor, &alice(), &[ROBOT]).await;
    dispatcher.idle().await;
    assert!(sink.deliveries().is_empty());
    let snapshot = fixture
        .server
        .get_snapshot(&wave.name)
        .await
        .unwrap()
        .unwrap();
    let records = fixture
        .server
        .get_history_records(
            &wave.name,
            &HashedVersionFactory::create_version_zero(&wave.name),
            snapshot.snapshot.hashed_version(),
            None,
        )
        .await
        .unwrap();
    dispatcher
        .wavelet_update(
            wave.name.clone(),
            records,
            snapshot.snapshot.participants().to_vec(),
        )
        .await;
    dispatcher.idle().await;
    assert_eq!(1, sink.deliveries().len());
    assert_eq!(4, dispatcher.options().max_hops.unwrap());
}

// ---- Finished editing: a no-op wavelet operation ----

/// Submits a delta of one no-op by `author`, as a client does when a
/// person ends an edit session.
async fn no_op(fixture: &Fixture, name: &WaveletName, author: &ParticipantId) {
    let target = version(fixture, name).await;
    let delta = WaveletDelta::new(
        author.clone(),
        target,
        vec![WaveletOperation::no_op(WaveletOperationContext::new(
            author.clone(),
            1_000_000,
            1,
        ))],
    );
    fixture.server.submit_delta(name, &delta).await.unwrap();
}

#[tokio::test]
async fn a_no_op_tells_who_finished_editing_unless_they_wrote_again() {
    let fixture = Fixture::new();
    let executor = fixture.executor();
    let wave = new_wave(&executor, &alice(), &[ROBOT, bob().address()]).await;
    let from = version(&fixture, &wave.name).await;
    // Alice finishes, then writes again: she has not finished.
    no_op(&fixture, &wave.name, &alice()).await;
    run(&executor, &alice(), vec![wave.append("\nmore")]).await;
    let generated = events_since(&fixture, &wave, &from).await;
    assert!(generated.triggers.finished_by.is_empty());
    assert!(!generated.bundle.events.is_empty());
    // Alice writes, then she, Bob and the robot itself send a no-op: the
    // robot's own does not count.
    let from = version(&fixture, &wave.name).await;
    run(&executor, &alice(), vec![wave.append("\nlast")]).await;
    no_op(&fixture, &wave.name, &alice()).await;
    no_op(&fixture, &wave.name, &bob()).await;
    no_op(&fixture, &wave.name, &robot()).await;
    no_op(&fixture, &wave.name, &alice()).await;
    let generated = events_since(&fixture, &wave, &from).await;
    assert_eq!(
        vec!["alice@example.com".to_owned(), "bob@example.com".to_owned()],
        generated.triggers.finished_by
    );
    // A no-op alone makes no event.
    let from = version(&fixture, &wave.name).await;
    no_op(&fixture, &wave.name, &bob()).await;
    let generated = events_since(&fixture, &wave, &from).await;
    assert!(generated.bundle.events.is_empty());
    assert_eq!(
        vec!["bob@example.com".to_owned()],
        generated.triggers.finished_by
    );
    assert!(!generated.triggers.any());
}

#[tokio::test]
async fn only_agents_in_the_server_hear_of_no_ops_and_only_from_people() {
    let setup = setup(DispatcherOptions::default());
    let (echo, echo_id) = setup.agent("echo").await;
    let (other, other_id) = setup.agent("other").await;
    // An external robot that asks for every event.
    let sink = TestSink::new(None);
    let events: Vec<EventType> = all_capabilities().into_keys().collect();
    setup.dispatcher.register(endpoint(sink.clone(), &events));
    let wave = new_wave(
        &setup.executor,
        &alice(),
        &[echo_id.address(), other_id.address(), ROBOT],
    )
    .await;
    setup.dispatcher.idle().await;
    let (echo_count, other_count, sink_count) = (
        echo.deliveries().len(),
        other.deliveries().len(),
        sink.deliveries().len(),
    );
    // A person's no-op: the agents get a delivery without events that
    // says who finished; the external robot gets nothing.
    no_op(&setup.fixture, &wave.name, &alice()).await;
    setup.dispatcher.idle().await;
    for agent in [&echo, &other] {
        let deliveries = agent.deliveries();
        let delivery = deliveries.last().unwrap();
        assert!(delivery.bundle.events.is_empty());
        assert_eq!(
            vec!["alice@example.com".to_owned()],
            delivery.triggers.finished_by
        );
        assert_eq!(wave.name, delivery.wavelet_name);
        assert_eq!(0, delivery.hop_count);
    }
    assert_eq!(echo_count + 1, echo.deliveries().len());
    assert_eq!(other_count + 1, other.deliveries().len());
    assert_eq!(sink_count, sink.deliveries().len());
    // No-ops by agents and robots are no signal, to anyone.
    for author in [&other_id, &echo_id, &robot()] {
        no_op(&setup.fixture, &wave.name, author).await;
    }
    setup.dispatcher.idle().await;
    assert_eq!(echo_count + 1, echo.deliveries().len());
    assert_eq!(other_count + 1, other.deliveries().len());
    assert_eq!(sink_count, sink.deliveries().len());
    // With events, an external robot's bundle is as before and its
    // delivery says nothing of who finished.
    run(&setup.executor, &alice(), vec![wave.append("\nhello")]).await;
    no_op(&setup.fixture, &wave.name, &alice()).await;
    setup.dispatcher.idle().await;
    let delivered = sink.deliveries();
    assert!(delivered.len() > sink_count);
    assert!(delivered.iter().all(|d| d.triggers.finished_by.is_empty()));
    assert!(
        delivered[sink_count..]
            .iter()
            .all(|d| !d.bundle.events.is_empty())
    );
}
