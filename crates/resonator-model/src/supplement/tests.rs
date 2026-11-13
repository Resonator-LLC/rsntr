//! Ports of the tests in `org.waveprotocol.wave.model.supplement`
//! (`PrimitiveSupplementTestBase` with both of its implementations,
//! `WaveletBasedSupplementTest`, `GadgetStateCollectionTest`,
//! `SimpleWantedEvaluationSetTest`, `SupplementedWaveImplTest`,
//! `LiveSupplementedWaveImplAbuseTest`), without the listener and event
//! tests, and tests of what the port adds.
//!
//! Where the original tests build a conversation with the conversation
//! model, these write the manifest and blip documents directly.

use std::collections::BTreeMap;

use super::*;
use crate::data::WaveletData;
use crate::doc::XmlFragment;
use crate::id::{ParticipantId, WaveId, WaveletName};
use crate::wavelet::OpBasedWavelet;

const TIME: i64 = 1234567890123;

fn viewer() -> ParticipantId {
    ParticipantId::of("nobody@nowhere.com").unwrap()
}

fn wid(id: &str) -> WaveletId {
    WaveletId::of("example.com", id).unwrap()
}

/// An empty wavelet of the viewer, whose operations each raise its version
/// by one, as in the original's test wave model.
fn wavelet(id: &str) -> OpBasedWavelet {
    let name = WaveletName::of(WaveId::of("example.com", "w+wave").unwrap(), wid(id));
    let data = WaveletData::empty(&name, viewer(), HashedVersion::unsigned(0), TIME);
    let mut wavelet = OpBasedWavelet::new(data, viewer(), TIME);
    wavelet.bump_versions = true;
    wavelet
}

fn user_data() -> OpBasedWavelet {
    wavelet("user+nobody@nowhere.com")
}

fn append(wavelet: &mut OpBasedWavelet, doc: &str, xml: &str) {
    let xml = XmlFragment::parse(xml).unwrap();
    wavelet
        .edit_document(doc, |doc| doc.insert_xml(doc.size(), &xml).map(drop))
        .unwrap();
}

fn xml(wavelet: &OpBasedWavelet, doc: &str) -> String {
    wavelet.document(doc).to_xml_string()
}

// ---- PrimitiveSupplementTestBase ----

mod primitive {
    use super::*;

    fn defaults(target: &mut impl PrimitiveSupplement) {
        let wavelet1 = wid("wavelet1");
        let blip1 = "blip1";
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), NO_VERSION);
        assert_eq!(target.last_read_participants_version(&wavelet1), NO_VERSION);
        assert_eq!(target.last_read_wavelet_version(&wavelet1), NO_VERSION);
        assert!(target.folders().is_empty());
        assert_eq!(target.followed(), None);
        assert_eq!(target.archive_wavelet_version(&wavelet1), NO_VERSION);
        assert!(!target.pending_notification());
        assert!(target.gadget_state("Gadget #1").is_empty());
    }

    fn inboxing(target: &mut impl PrimitiveSupplement) {
        let (wavelet1, wavelet2) = (wid("wavelet1"), wid("wavelet2"));
        target.archive_at_version(&wavelet1, 10).unwrap();
        assert_eq!(target.archive_wavelet_version(&wavelet1), 10);
        target.archive_at_version(&wavelet2, 5).unwrap();
        assert_eq!(target.archive_wavelet_version(&wavelet2), 5);
        target.clear_archive_state().unwrap();
        assert_eq!(target.archive_wavelet_version(&wavelet1), NO_VERSION);
        assert_eq!(target.archive_wavelet_version(&wavelet2), NO_VERSION);
    }

    fn clear_read_state(target: &mut impl PrimitiveSupplement) {
        let (wavelet1, wavelet2) = (wid("wavelet1"), wid("wavelet2"));
        let (blip1, blip2) = ("blip1", "blip2");
        target
            .set_last_read_blip_version(&wavelet1, blip1, 23)
            .unwrap();
        target
            .set_last_read_blip_version(&wavelet2, blip2, 43)
            .unwrap();
        target.clear_read_state().unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), NO_VERSION);
        assert_eq!(target.last_read_blip_version(&wavelet2, blip2), NO_VERSION);
        target
            .set_last_read_blip_version(&wavelet1, blip1, 34)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 34);
        assert_eq!(target.last_read_blip_version(&wavelet2, blip2), NO_VERSION);
    }

    fn clear_blip_read_state(target: &mut impl PrimitiveSupplement) {
        let wavelet1 = wid("wavelet1");
        let (blip1, blip2) = ("blip1", "blip2");
        target
            .set_last_read_blip_version(&wavelet1, blip1, 23)
            .unwrap();
        target
            .set_last_read_blip_version(&wavelet1, blip2, 43)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 23);
        assert_eq!(target.last_read_blip_version(&wavelet1, blip2), 43);
        target.clear_blip_read_state(&wavelet1, blip1).unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), NO_VERSION);
        assert_eq!(target.last_read_blip_version(&wavelet1, blip2), 43);
        // A lesser version than the one cleared can be set.
        target
            .set_last_read_blip_version(&wavelet1, blip1, 10)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 10);
    }

    fn seen_versions(target: &mut impl PrimitiveSupplement) {
        let (wavelet1, wavelet2) = (wid("wavelet1"), wid("wavelet2"));
        let seen = HashedVersion::of(213819238, vec![1]);
        target.set_seen_version(&wavelet1, &seen).unwrap();
        assert_eq!(target.seen_version(&wavelet1), seen);
        assert_eq!(target.seen_version(&wavelet2), HashedVersion::unsigned(0));
        assert_eq!(target.seen_wavelets(), [wavelet1]);
    }

    fn random(target: &mut impl PrimitiveSupplement) {
        use ThreadState::{Collapsed, Expanded};
        let (wavelet1, wavelet2) = (wid("wavelet1"), wid("wavelet2"));
        let (blip1, blip2) = ("blip1", "blip2");
        let (thread1, thread2) = ("thread1", "thread2");

        target
            .set_last_read_blip_version(&wavelet1, blip1, 7)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 7);
        target
            .set_last_read_blip_version(&wavelet1, blip1, 5)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 7);
        target
            .set_last_read_blip_version(&wavelet1, blip2, 15)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 7);
        assert_eq!(target.last_read_blip_version(&wavelet1, blip2), 15);
        target
            .set_last_read_blip_version(&wavelet2, blip1, 8)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 7);
        assert_eq!(target.last_read_blip_version(&wavelet1, blip2), 15);
        assert_eq!(target.last_read_blip_version(&wavelet2, blip1), 8);
        target
            .set_last_read_participants_version(&wavelet1, 10)
            .unwrap();
        assert_eq!(target.last_read_participants_version(&wavelet1), 10);
        target.set_last_read_wavelet_version(&wavelet2, 5).unwrap();
        assert_eq!(target.last_read_wavelet_version(&wavelet2), 5);
        target
            .set_last_read_blip_version(&wavelet2, blip1, 13)
            .unwrap();
        assert_eq!(target.last_read_blip_version(&wavelet1, blip1), 7);
        assert_eq!(target.last_read_blip_version(&wavelet1, blip2), 15);
        assert_eq!(target.last_read_blip_version(&wavelet2, blip1), 13);

        target.add_folder(3).unwrap();
        assert_eq!(target.folders(), [3]);
        target.add_folder(5).unwrap();
        assert_eq!(target.folders(), [3, 5]);
        target.add_folder(5).unwrap();
        assert_eq!(target.folders(), [3, 5]);
        target.remove_all_folders().unwrap();
        assert!(target.folders().is_empty());
        target.add_folder(5).unwrap();
        assert_eq!(target.folders(), [5]);

        target.set_notified_version(&wavelet1, 10).unwrap();
        target.set_notified_version(&wavelet2, 20).unwrap();
        assert_eq!(
            target.notified_wavelets(),
            [wavelet1.clone(), wavelet2.clone()]
        );
        assert_eq!(target.notified_version(&wavelet1), 10);
        assert_eq!(target.notified_version(&wavelet2), 20);
        assert!(!target.pending_notification());

        target
            .set_seen_version(&wavelet1, &HashedVersion::of(213819238, vec![10]))
            .unwrap();
        target
            .set_seen_version(&wavelet2, &HashedVersion::of(213819238, vec![20]))
            .unwrap();
        assert!(!target.pending_notification());

        assert_eq!(target.thread_state(&wavelet1, thread1), None);
        assert_eq!(target.thread_state(&wavelet1, thread2), None);
        let mut set = |wavelet: &WaveletId, thread: &str, state| {
            target.set_thread_state(wavelet, thread, state).unwrap();
        };
        set(&wavelet1, thread1, Some(Expanded));
        set(&wavelet1, thread1, Some(Collapsed));
        set(&wavelet1, thread1, Some(Expanded));
        set(&wavelet1, thread2, Some(Collapsed));
        set(&wavelet2, thread1, Some(Collapsed));
        set(&wavelet2, thread2, Some(Collapsed));
        set(&wavelet2, thread2, Some(Expanded));
        assert_eq!(target.thread_state(&wavelet1, thread1), Some(Expanded));
        assert_eq!(target.thread_state(&wavelet1, thread2), Some(Collapsed));
        assert_eq!(target.thread_state(&wavelet2, thread1), Some(Collapsed));
        assert_eq!(target.thread_state(&wavelet2, thread2), Some(Expanded));
        assert_eq!(target.stateful_threads(&wavelet1), [thread1, thread2]);
        assert_eq!(
            target.wavelets_with_thread_state(),
            [wavelet1.clone(), wavelet2.clone()]
        );

        target.set_thread_state(&wavelet2, thread1, None).unwrap();
        assert_eq!(target.thread_state(&wavelet2, thread1), None);
        assert_eq!(target.thread_state(&wavelet1, thread1), Some(Expanded));
        assert_eq!(target.thread_state(&wavelet2, thread2), Some(Expanded));
        target.set_thread_state(&wavelet1, thread2, None).unwrap();
        target.set_thread_state(&wavelet2, thread2, None).unwrap();
        assert_eq!(target.thread_state(&wavelet1, thread1), Some(Expanded));
        assert_eq!(target.thread_state(&wavelet2, thread2), None);
        assert_eq!(target.thread_state(&wavelet2, thread1), None);
    }

    fn evaluation(
        wanted: bool,
        certainty: f32,
        timestamp: i64,
        agent: &str,
        ignored: bool,
    ) -> WantedEvaluation {
        WantedEvaluation {
            wavelet_id: Some(wid("wavelet1")),
            adder_address: "evilbob@evil.com".to_owned(),
            wanted,
            certainty: f64::from(certainty),
            timestamp,
            agent_identity: agent.to_owned(),
            ignored,
            comment: "no comment".to_owned(),
        }
    }

    fn abuse(target: &mut impl PrimitiveSupplement) {
        let eval1 = evaluation(true, 0.1, 1234, "agent1", false);
        let eval2 = evaluation(true, 0.8, 1256, "agent2", true);
        let eval3 = evaluation(false, 0.5, 1278, "agent1", false);
        let all = [eval1.clone(), eval2.clone(), eval3.clone()];
        assert!(target.wanted_evaluations().is_empty());
        target.add_wanted_evaluation(&eval1).unwrap();
        assert_eq!(target.wanted_evaluations().len(), 1);
        target.add_wanted_evaluation(&eval2).unwrap();
        target.add_wanted_evaluation(&eval3).unwrap();
        assert_eq!(target.wanted_evaluations(), all);
        target.add_wanted_evaluation(&eval1).unwrap();
        assert_eq!(target.wanted_evaluations(), all);
    }

    fn gadget_states(target: &mut impl PrimitiveSupplement) {
        let (gadget1, gadget2) = ("Gadget 1", "Gadget 2");
        let (state1, state2) = ("State 1", "State 2");
        assert!(target.gadget_state(gadget1).is_empty());
        assert!(target.gadget_state(gadget2).is_empty());
        let mut set = |gadget: &str, key: &str, value: Option<&str>| {
            target.set_gadget_state(gadget, key, value).unwrap();
        };
        set(gadget1, state1, Some("State 1 in gadget 1"));
        set(gadget2, state1, Some("State 1 in gadget 2"));
        set(gadget1, state2, Some("State 2 in gadget 1"));
        set(gadget2, state2, Some("State 2 in gadget 2"));
        let state = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert_eq!(
            target.gadget_state(gadget1),
            state(&[
                (state1, "State 1 in gadget 1"),
                (state2, "State 2 in gadget 1")
            ])
        );
        assert_eq!(
            target.gadget_state(gadget2),
            state(&[
                (state1, "State 1 in gadget 2"),
                (state2, "State 2 in gadget 2")
            ])
        );
        target.set_gadget_state(gadget1, state1, None).unwrap();
        target.set_gadget_state(gadget2, state2, None).unwrap();
        assert_eq!(
            target.gadget_state(gadget1),
            state(&[(state2, "State 2 in gadget 1")])
        );
        assert_eq!(
            target.gadget_state(gadget2),
            state(&[(state1, "State 1 in gadget 2")])
        );
    }

    macro_rules! both {
        ($($test:ident),*) => {
            mod in_memory {
                $(#[test]
                fn $test() {
                    super::$test(&mut super::PrimitiveSupplementImpl::new());
                })*
            }

            mod wavelet_based {
                $(#[test]
                fn $test() {
                    let mut udw = super::user_data();
                    let mut target = super::WaveletBasedSupplement::create(&mut udw).unwrap();
                    super::$test(&mut target);
                })*
            }

            /// Through a `&mut` and the partitioning wrapper in its
            /// pass-through configuration.
            mod partitioned {
                $(#[test]
                fn $test() {
                    let mut real = super::PrimitiveSupplementImpl::new();
                    let mut target =
                        super::PartitioningPrimitiveSupplement::new(&mut real, true, true);
                    super::$test(&mut target);
                })*
            }
        };
    }

    both!(
        defaults,
        inboxing,
        clear_read_state,
        clear_blip_read_state,
        seen_versions,
        random,
        abuse,
        gadget_states
    );
}

// ---- WaveletBasedSupplementTest ----

#[test]
fn load_on_domainless_wavelet_ids_does_not_fail() {
    let mut udw = user_data();
    append(
        &mut udw,
        READSTATE_DOCUMENT,
        "<wavelet i=\"conversation/root\"></wavelet>",
    );
    append(
        &mut udw,
        ARCHIVING_DOCUMENT,
        "<archive i=\"conversation/root\"></archive>",
    );
    append(
        &mut udw,
        SEEN_DOCUMENT,
        "<seen i=\"conversation/root\"></seen>",
    );
    udw.take_ops();
    let supplement = WaveletBasedSupplement::create(&mut udw).unwrap();
    // The id reads as wavelet "root" in the domain "conversation".
    let id = WaveletId::of("conversation", "root").unwrap();
    assert_eq!(supplement.read_wavelets(), std::slice::from_ref(&id));
    assert_eq!(supplement.last_read_wavelet_version(&id), NO_VERSION);
    assert_eq!(supplement.archive_wavelet_version(&id), NO_VERSION);
    assert_eq!(supplement.seen_version(&id), HashedVersion::unsigned(0));
    assert!(udw.ops().is_empty());
}

/// A conversation wavelet with one blip.
fn conversation(id: &str) -> OpBasedWavelet {
    let mut wavelet = wavelet(id);
    append(
        &mut wavelet,
        "conversation",
        "<conversation><blip id=\"b+1\"></blip></conversation>",
    );
    append(&mut wavelet, "b+1", "<body><line></line></body>");
    wavelet
}

#[test]
fn copy_snapshot_does_not_cause_writes() {
    let mut udw = user_data();
    let mut root = conversation("conv+root");
    root.add_participant(&viewer()).unwrap();
    {
        let view = WaveletDataView::new(viewer(), [root.data()]);
        let substrate = WaveletBasedSupplement::create(&mut udw).unwrap();
        let mut swave = SupplementedWaveImpl::new(substrate, view, DefaultFollow::Always);
        swave.mark_as_read().unwrap();
    }
    let version = udw.data().version();
    assert_eq!(udw.take_ops().len(), 2);
    let substrate = WaveletBasedSupplement::create(&mut udw).unwrap();
    let copy = PrimitiveSupplementImpl::copy_of(&substrate);
    assert_eq!(copy.last_read_wavelet_version(&wid("conv+root")), 3);
    assert_eq!(udw.data().version(), version);
    assert!(udw.ops().is_empty());
}

// ---- GadgetStateCollectionTest ----

#[test]
fn set_gadget_state() {
    let mut udw = user_data();
    let mut states = WaveletBasedSupplement::create(&mut udw).unwrap();
    let (gadget1, gadget2) = ("1", "2");
    assert!(states.gadget_state(gadget1).is_empty());
    assert!(states.gadget_state(gadget2).is_empty());
    states
        .set_gadget_state(gadget1, "key1", Some("value1 in 1"))
        .unwrap();
    assert_eq!(states.gadget_state(gadget1).len(), 1);
    assert_eq!(states.gadget_state(gadget2).len(), 0);
    assert_eq!(states.gadget_state(gadget1)["key1"], "value1 in 1");
    states
        .set_gadget_state(gadget2, "key1", Some("value1 in 2"))
        .unwrap();
    assert_eq!(states.gadget_state(gadget1)["key1"], "value1 in 1");
    assert_eq!(states.gadget_state(gadget2)["key1"], "value1 in 2");
    states.set_gadget_state(gadget1, "key1", None).unwrap();
    states
        .set_gadget_state(gadget2, "key2", Some("value2 in 2"))
        .unwrap();
    assert_eq!(states.gadget_state(gadget1).len(), 0);
    assert_eq!(states.gadget_state(gadget2).len(), 2);
    assert_eq!(states.gadget_state(gadget2)["key1"], "value1 in 2");
    assert_eq!(states.gadget_state(gadget2)["key2"], "value2 in 2");
    assert_eq!(
        xml(&udw, GADGETS_DOCUMENT),
        "<gadget i=\"1\"/><gadget i=\"2\"><state name=\"key2\" value=\"value2 in 2\"/>\
         <state name=\"key1\" value=\"value1 in 2\"/></gadget>"
    );
}

// ---- SimpleWantedEvaluationSetTest, LiveSupplementedWaveImplAbuseTest ----

mod wanted {
    use super::*;

    fn evaluation(
        wavelet: &str,
        wanted: bool,
        certainty: f32,
        timestamp: i64,
        agent: &str,
        ignored: bool,
        comment: &str,
    ) -> WantedEvaluation {
        WantedEvaluation {
            wavelet_id: Some(wid(wavelet)),
            adder_address: "badhorse@evilleagueofevil.com".to_owned(),
            wanted,
            certainty: f64::from(certainty),
            timestamp,
            agent_identity: agent.to_owned(),
            ignored,
            comment: comment.to_owned(),
        }
    }

    #[test]
    fn basic() {
        let id = wid("wavelet1");
        let evaluation1 = evaluation("wavelet1", true, 0.2, 1000, "agent", false, "");
        let evaluation2 = evaluation("wavelet1", false, 0.2, 1010, "test2", false, "");
        let evaluation3 = evaluation("wavelet1", true, 0.3, 1011, "agent", true, "");

        let set = WantedEvaluationSet::new(id.clone(), []);
        assert_eq!(set.wavelet_id(), &id);
        assert!(set.is_wanted());
        let default = set.most_certain();
        assert!(default.wanted);
        assert!(default.certainty < 0.01);
        assert!(set.evaluations().is_empty());

        let set = WantedEvaluationSet::new(id.clone(), [evaluation1.clone(), evaluation2.clone()]);
        assert_eq!(set.most_certain(), evaluation2);
        assert!(!set.is_wanted());
        assert!(!set.is_ignored());

        let all = [evaluation1, evaluation2, evaluation3.clone()];
        let set = WantedEvaluationSet::new(id, all.clone());
        assert_eq!(set.most_certain(), evaluation3);
        assert!(set.is_wanted());
        assert!(set.is_ignored());
        assert_eq!(set.evaluations(), all);
    }

    #[test]
    fn duplicates_are_dropped_and_ties_go_to_the_hash_code() {
        let a = evaluation("wavelet1", true, 0.5, 7, "a", false, "");
        let b = evaluation("wavelet1", false, 0.5, 7, "b", true, "");
        let forward = WantedEvaluationSet::new(wid("wavelet1"), [a.clone(), b.clone(), a.clone()]);
        assert_eq!(forward.evaluations().len(), 2);
        let backward = WantedEvaluationSet::new(wid("wavelet1"), [b.clone(), a.clone()]);
        assert_eq!(forward.most_certain(), backward.most_certain());
        let expected = if a.java_hash_code() > b.java_hash_code() {
            a
        } else {
            b
        };
        assert_eq!(forward.most_certain(), expected);
    }

    /// The fixture of `LiveSupplementedWaveImplAbuseTest`.
    fn fixture() -> (
        OpBasedWavelet,
        OpBasedWavelet,
        OpBasedWavelet,
        [WantedEvaluation; 3],
    ) {
        let evaluations = [
            evaluation("conv+root", true, 0.1, 1234, "agent0", true, "no comment"),
            evaluation("conv+other", true, 0.8, 1256, "agent2", false, "no comment"),
            evaluation("conv+root", false, 0.5, 1278, "agent1", true, "no comment"),
        ];
        (
            user_data(),
            conversation("conv+root"),
            conversation("conv+other"),
            evaluations,
        )
    }

    #[test]
    fn empty_evaluation_set() {
        let (mut udw, root, other, _) = fixture();
        let view = WaveletDataView::new(viewer(), [root.data(), other.data()]);
        let primitive = WaveletBasedSupplement::create(&mut udw).unwrap();
        let supplemented = SupplementedWaveImpl::new(primitive, view, DefaultFollow::Always);
        let set = supplemented.wanted_evaluation_set(&wid("conv+root"));
        assert!(set.is_wanted());
        assert!(set.evaluations().is_empty());
        let default = set.most_certain();
        assert!(default.certainty < 0.01);
        assert!(default.wanted);
    }

    #[test]
    fn invalid_evaluations_ignored() {
        let (mut udw, root, other, [_, eval2, _]) = fixture();
        // An evaluation without a wavelet id.
        append(
            &mut udw,
            ABUSE_DOCUMENT,
            "<wanted agent=\"invalid agent\" certainty=\"0.0\" timestamp=\"1234\" \
             comment=\"no comment\" wanted=\"true\"></wanted>",
        );
        let view = WaveletDataView::new(viewer(), [root.data(), other.data()]);
        let primitive = WaveletBasedSupplement::create(&mut udw).unwrap();
        let mut supplemented = SupplementedWaveImpl::new(primitive, view, DefaultFollow::Always);
        supplemented.add_wanted_evaluation(&eval2).unwrap();
        let set = supplemented.wanted_evaluation_set(&wid("conv+other"));
        assert_eq!(set.evaluations(), [eval2]);
        let all = supplemented.supplement().primitive().wanted_evaluations();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].wavelet_id, None);
        assert_eq!(all[0].agent_identity, "invalid agent");
        assert_eq!(all[0].adder_address, "");
    }

    #[test]
    fn multiple_evaluation_sets() {
        let (mut udw, root, other, [eval1, eval2, eval3]) = fixture();
        let view = WaveletDataView::new(viewer(), [root.data(), other.data()]);
        let primitive = WaveletBasedSupplement::create(&mut udw).unwrap();
        let mut supplemented = SupplementedWaveImpl::new(primitive, view, DefaultFollow::Always);
        for evaluation in [&eval1, &eval2, &eval3] {
            supplemented.add_wanted_evaluation(evaluation).unwrap();
        }
        let set1 = supplemented.wanted_evaluation_set(&wid("conv+root"));
        assert_eq!(set1.evaluations(), [eval1, eval3]);
        let set2 = supplemented.wanted_evaluation_set(&wid("conv+other"));
        assert_eq!(set2.evaluations(), [eval2]);
    }

    #[test]
    fn evaluation_without_wavelet_id_cannot_be_stored() {
        let mut udw = user_data();
        let mut primitive = WaveletBasedSupplement::create(&mut udw).unwrap();
        let mut evaluation = evaluation("conv+root", true, 0.5, 1, "a", false, "");
        evaluation.wavelet_id = None;
        assert!(matches!(
            primitive.add_wanted_evaluation(&evaluation),
            Err(SupplementError::IllegalState(_))
        ));
        assert!(udw.ops().is_empty());
    }
}

// ---- SupplementedWaveImplTest ----

mod supplemented_wave {
    use super::*;

    const ROOT_VERSION: i64 = 30;
    const ROOT_BLIP1_VERSION: i64 = 28;
    const W1_VERSION: i64 = 20;
    const W1_BLIP1_VERSION: i64 = 15;
    const ROOT_BLIP1: &str = "b+rootblip";
    const W1_BLIP1: &str = "b+w1blip";

    fn root() -> WaveletId {
        wid("conv+root")
    }

    fn w1() -> WaveletId {
        wid("conv+w1")
    }

    /// `StubSupplementWaveView`.
    #[derive(Debug, Clone, Default)]
    struct StubView {
        versions: BTreeMap<WaveletId, i64>,
        signatures: BTreeMap<WaveletId, HashedVersion>,
        blip_versions: BTreeMap<WaveletId, BTreeMap<String, i64>>,
        is_explicit_participant: bool,
    }

    impl StubView {
        fn put(&mut self, id: WaveletId, version: i64) {
            self.versions.insert(id, version);
        }

        fn put_blip_version(&mut self, id: WaveletId, blip: &str, version: i64) {
            let blips = self.blip_versions.entry(id).or_default();
            blips.insert(blip.to_owned(), version);
        }

        fn touch(&mut self, id: &WaveletId) {
            if let Some(version) = self.versions.get_mut(id) {
                *version += 1;
            }
        }
    }

    impl SupplementWaveView for StubView {
        fn wavelets(&self) -> Vec<WaveletId> {
            self.versions.keys().cloned().collect()
        }

        fn version(&self, wavelet_id: &WaveletId) -> i64 {
            self.versions
                .get(wavelet_id)
                .copied()
                .unwrap_or(i64::from(NO_VERSION))
        }

        fn signature(&self, wavelet_id: &WaveletId) -> HashedVersion {
            self.signatures
                .get(wavelet_id)
                .cloned()
                .unwrap_or_else(|| HashedVersion::unsigned(0))
        }

        fn blip_versions(&self, wavelet_id: &WaveletId) -> BTreeMap<String, i64> {
            self.blip_versions
                .get(wavelet_id)
                .cloned()
                .unwrap_or_default()
        }

        fn is_explicit_participant(&self) -> bool {
            self.is_explicit_participant
        }
    }

    type Target = SupplementedWaveImpl<PrimitiveSupplementImpl, StubView>;

    /// The original's `setUp`.
    fn target() -> Target {
        let mut wave = StubView::default();
        wave.put(root(), ROOT_VERSION);
        wave.put(w1(), W1_VERSION);
        wave.put_blip_version(root(), ROOT_BLIP1, ROOT_BLIP1_VERSION);
        wave.put_blip_version(w1(), W1_BLIP1, W1_BLIP1_VERSION);
        SupplementedWaveImpl::new(PrimitiveSupplementImpl::new(), wave, DefaultFollow::Always)
    }

    fn substrate(supplement: &mut Target) -> &mut PrimitiveSupplementImpl {
        supplement.supplement_mut().primitive_mut()
    }

    fn touch(supplement: Target, id: &WaveletId) -> Target {
        let mut wave = supplement.wave().clone();
        wave.touch(id);
        SupplementedWaveImpl::new(supplement.into_primitive(), wave, DefaultFollow::Always)
    }

    fn archive_versions(supplement: &Target) -> BTreeMap<WaveletId, i32> {
        let substrate = supplement.supplement().primitive();
        substrate
            .archive_wavelets()
            .into_iter()
            .map(|id| {
                let version = substrate.archive_wavelet_version(&id);
                (id, version)
            })
            .collect()
    }

    fn expected_archive_versions() -> BTreeMap<WaveletId, i32> {
        [(root(), ROOT_VERSION as i32), (w1(), W1_VERSION as i32)].into()
    }

    fn assert_is_inbox(supplement: &Target) {
        assert!(supplement.is_inbox());
        assert!(!supplement.is_archived());
        assert!(!supplement.is_mute());
        assert_eq!(supplement.inbox_state(), InboxState::Inbox);
    }

    fn assert_is_archived(supplement: &Target) {
        assert!(!supplement.is_inbox());
        assert!(supplement.is_archived());
        assert!(!supplement.is_mute());
        assert_eq!(supplement.inbox_state(), InboxState::Archive);
    }

    fn assert_is_unfollowed(supplement: &Target) {
        assert!(!supplement.is_inbox());
        assert!(!supplement.is_archived());
        assert!(supplement.is_mute());
        assert_eq!(supplement.inbox_state(), InboxState::Mute);
    }

    /// The original throws for a view that lists a wavelet that is not
    /// conversational; here such a wavelet is disregarded.
    #[test]
    fn incorrect_external_wave_view_is_disregarded() {
        let mut supplement = target();
        let mut wave = supplement.wave().clone();
        let udw = wid("user+foo@bar.com");
        wave.put(udw.clone(), 20);
        supplement =
            SupplementedWaveImpl::new(supplement.into_primitive(), wave, DefaultFollow::Always);
        supplement.archive().unwrap();
        supplement.mark_as_read().unwrap();
        supplement.see().unwrap();
        supplement.mark_as_notified().unwrap();
        assert_eq!(archive_versions(&supplement), expected_archive_versions());
        let substrate = supplement.supplement().primitive();
        assert_eq!(substrate.read_wavelets(), [root(), w1()]);
        assert_eq!(substrate.seen_wavelets(), [root(), w1()]);
        assert_eq!(substrate.notified_wavelets(), [root(), w1()]);
        assert_is_archived(&supplement);
    }

    #[test]
    fn stub_blips_are_unread_until_read() {
        let mut supplement = target();
        assert!(supplement.is_unread(&root(), ROOT_BLIP1));
        assert!(supplement.is_unread(&w1(), W1_BLIP1));
        assert!(!supplement.is_unread(&root(), "b+unknown"));
        supplement.mark_blip_as_read(&root(), ROOT_BLIP1).unwrap();
        supplement.mark_blip_as_read(&root(), "b+unknown").unwrap();
        assert!(!supplement.is_unread(&root(), ROOT_BLIP1));
        assert!(supplement.is_unread(&w1(), W1_BLIP1));
        // Read at the version of the wavelet, not of the blip.
        let substrate = supplement.supplement().primitive();
        assert_eq!(
            substrate.last_read_blip_version(&root(), ROOT_BLIP1),
            ROOT_VERSION as i32
        );
        assert_eq!(substrate.read_blips(&root()), [ROOT_BLIP1]);
    }

    /// The original's `setUpWithWaveModel`: a root conversation over the
    /// wave model and an in-memory supplement.
    struct Model {
        root: OpBasedWavelet,
        substrate: PrimitiveSupplementImpl,
        blips: usize,
    }

    impl Model {
        fn new() -> Self {
            let mut root = wavelet("conv+root");
            append(&mut root, "conversation", "<conversation></conversation>");
            Model {
                root,
                substrate: PrimitiveSupplementImpl::new(),
                blips: 0,
            }
        }

        /// `getRootThread().appendBlip()`: a manifest entry and a blip
        /// document, two operations.
        fn append_blip(&mut self) -> String {
            self.blips += 1;
            let id = format!("b+{}", self.blips);
            self.root
                .edit_document("conversation", |doc| {
                    let location = doc.size() - 1;
                    doc.create_element(location, "blip", &[("id", id.as_str())])
                        .map(drop)
                })
                .unwrap();
            append(&mut self.root, &id, "<body><line></line></body>");
            id
        }

        fn version(&self) -> i32 {
            self.root.data().version() as i32
        }

        fn supplement(
            &mut self,
        ) -> SupplementedWaveImpl<&mut PrimitiveSupplementImpl, WaveletDataView<'_>> {
            let view = WaveletDataView::new(viewer(), [self.root.data()]);
            SupplementedWaveImpl::new(&mut self.substrate, view, DefaultFollow::Always)
        }
    }

    #[test]
    fn new_blip_is_unread() {
        let mut model = Model::new();
        let b = model.append_blip();
        assert!(model.supplement().is_unread(&root(), &b));
    }

    #[test]
    fn mark_blip_as_read_affects_blip_read_state() {
        let mut model = Model::new();
        let b = model.append_blip();
        let mut supplement = model.supplement();
        supplement.mark_blip_as_read(&root(), &b).unwrap();
        assert!(!supplement.is_unread(&root(), &b));
    }

    #[test]
    fn mark_blip_is_idempotent() {
        let mut model = Model::new();
        let b = model.append_blip();
        model.supplement().mark_blip_as_read(&root(), &b).unwrap();
        let blip_read_version = model.substrate.last_read_blip_version(&root(), &b);
        let wavelet_version = model.version();
        assert_eq!(wavelet_version, blip_read_version);

        model.append_blip();
        assert!(model.version() > wavelet_version);
        model.supplement().mark_blip_as_read(&root(), &b).unwrap();
        assert_eq!(
            model.substrate.last_read_blip_version(&root(), &b),
            blip_read_version
        );
    }

    #[test]
    fn marking_blip_as_read_after_wave_unread_marks_at_wavelet_version() {
        let mut model = Model::new();
        let b = model.append_blip();
        model.supplement().mark_blip_as_read(&root(), &b).unwrap();
        model.supplement().mark_as_unread().unwrap();
        model.append_blip();
        model.supplement().mark_blip_as_read(&root(), &b).unwrap();
        assert_eq!(
            model.substrate.last_read_blip_version(&root(), &b),
            model.version()
        );
    }

    #[test]
    fn marking_blip_as_read_after_blip_modified_marks_at_wavelet_version() {
        let mut model = Model::new();
        let b = model.append_blip();
        model.supplement().mark_blip_as_read(&root(), &b).unwrap();
        model.supplement().mark_as_unread().unwrap();
        append(&mut model.root, &b, "<body><line></line></body>");
        model.append_blip();
        model.supplement().mark_blip_as_read(&root(), &b).unwrap();
        assert_eq!(
            model.substrate.last_read_blip_version(&root(), &b),
            model.version()
        );
    }

    #[test]
    fn unmuted_unarchived_is_inbox() {
        assert_is_inbox(&target());
    }

    #[test]
    fn empty_wave_state() {
        let supplement = SupplementedWaveImpl::new(
            PrimitiveSupplementImpl::new(),
            StubView::default(),
            DefaultFollow::Always,
        );
        assert_is_archived(&supplement);
    }

    #[test]
    fn unmuted_some_archived_is_inbox() {
        let mut supplement = target();
        substrate(&mut supplement)
            .archive_at_version(&root(), ROOT_VERSION as i32)
            .unwrap();
        assert_is_inbox(&supplement);
    }

    #[test]
    fn unmuted_archived_is_not_inbox() {
        let mut supplement = target();
        supplement.archive().unwrap();
        assert_is_archived(&supplement);
    }

    #[test]
    fn unmuted_remote_archived_is_not_inbox() {
        let mut supplement = target();
        substrate(&mut supplement)
            .archive_at_version(&root(), ROOT_VERSION as i32)
            .unwrap();
        substrate(&mut supplement)
            .archive_at_version(&w1(), W1_VERSION as i32)
            .unwrap();
        assert_is_archived(&supplement);
    }

    #[test]
    fn unfollowed_is_not_inbox() {
        let mut supplement = target();
        supplement.unfollow().unwrap();
        assert_is_unfollowed(&supplement);
    }

    #[test]
    fn remote_unfollowed_is_not_inbox() {
        let mut supplement = target();
        substrate(&mut supplement).unfollow().unwrap();
        assert_is_unfollowed(&supplement);
    }

    #[test]
    fn archive_archives_all_wavelets() {
        let mut supplement = target();
        supplement.archive().unwrap();
        assert_eq!(archive_versions(&supplement), expected_archive_versions());
        assert_is_archived(&supplement);
    }

    #[test]
    fn change_after_archive_is_inbox() {
        let mut supplement = target();
        supplement.archive().unwrap();
        let supplement = touch(supplement, &root());
        assert_is_inbox(&supplement);
    }

    #[test]
    fn change_after_unfollow_is_not_inbox() {
        let mut supplement = target();
        supplement.unfollow().unwrap();
        let supplement = touch(supplement, &root());
        assert_is_unfollowed(&supplement);
    }

    #[test]
    fn unfollow_unfollows_substrate() {
        let mut supplement = target();
        supplement.unfollow().unwrap();
        assert_eq!(substrate(&mut supplement).followed(), Some(false));
        assert_is_unfollowed(&supplement);
    }

    #[test]
    fn mute_clears_archive_versions() {
        let mut supplement = target();
        supplement.archive().unwrap();
        supplement.mute().unwrap();
        assert!(archive_versions(&supplement).is_empty());
    }

    #[test]
    fn archive_does_not_clear_mute_nor_sets_archive_versions() {
        let mut supplement = target();
        supplement.unfollow().unwrap();
        supplement.archive().unwrap();
        assert_eq!(substrate(&mut supplement).followed(), Some(false));
        assert!(archive_versions(&supplement).is_empty());
    }

    #[test]
    fn move_to_folder_archives() {
        let mut supplement = target();
        supplement.move_to_folder(100).unwrap();
        assert_eq!(archive_versions(&supplement), expected_archive_versions());
    }

    #[test]
    fn move_to_folder_removes_existing_folders() {
        let mut supplement = target();
        substrate(&mut supplement).add_folder(100).unwrap();
        substrate(&mut supplement).add_folder(101).unwrap();
        supplement.move_to_folder(102).unwrap();
        assert_eq!(supplement.folders(), [102]);
        assert_eq!(substrate(&mut supplement).folders(), [102]);
    }

    #[test]
    fn move_to_folder_is_not_inbox() {
        let mut supplement = target();
        supplement.move_to_folder(100).unwrap();
        assert!(!supplement.is_inbox());
    }

    #[test]
    fn move_to_folder_preserves_unfollow() {
        let mut supplement = target();
        supplement.unfollow().unwrap();
        supplement.move_to_folder(100).unwrap();
        assert_eq!(substrate(&mut supplement).followed(), Some(false));
    }

    #[test]
    fn move_to_inbox_folder_moves_to_inbox() {
        let mut supplement = target();
        supplement.move_to_folder(INBOX_FOLDER).unwrap();
        assert!(supplement.is_inbox());
        assert!(substrate(&mut supplement).folders().is_empty());
    }

    #[test]
    fn move_to_inbox_folder_removes_existing_folders() {
        let mut supplement = target();
        supplement.move_to_folder(100).unwrap();
        supplement.move_to_folder(INBOX_FOLDER).unwrap();
        assert!(supplement.folders().is_empty());
    }

    #[test]
    fn inbox_moves_to_inbox() {
        let mut supplement = target();
        supplement.inbox().unwrap();
        assert_is_inbox(&supplement);
    }

    #[test]
    fn inbox_clears_archive_state() {
        let mut supplement = target();
        supplement.archive().unwrap();
        supplement.inbox().unwrap();
        assert!(archive_versions(&supplement).is_empty());
    }

    #[test]
    fn move_to_folder_all_archives_and_clears_folder_state() {
        let mut supplement = target();
        supplement.move_to_folder(100).unwrap();
        supplement.move_to_folder(ALL_FOLDER).unwrap();
        assert_is_archived(&supplement);
        assert!(supplement.folders().is_empty());
        assert_eq!(archive_versions(&supplement), expected_archive_versions());
        assert!(substrate(&mut supplement).folders().is_empty());
    }

    #[test]
    fn move_to_folder_all_preserves_mute() {
        let mut supplement = target();
        supplement.move_to_folder(100).unwrap();
        supplement.unfollow().unwrap();
        supplement.move_to_folder(ALL_FOLDER).unwrap();
        assert_eq!(substrate(&mut supplement).followed(), Some(false));
    }

    #[test]
    fn mute_preserves_folders() {
        let mut supplement = target();
        supplement.move_to_folder(100).unwrap();
        supplement.mute().unwrap();
        assert_eq!(supplement.folders(), [100]);
    }

    #[test]
    fn participant_read_state() {
        let mut model = Model::new();
        let mut supplement = model.supplement();
        assert!(!supplement.have_participants_ever_been_read(&root()));
        assert!(supplement.is_participants_unread(&root()));
        supplement.mark_participants_as_read(&root()).unwrap();
        assert!(supplement.have_participants_ever_been_read(&root()));
        assert!(!supplement.is_participants_unread(&root()));
    }

    #[test]
    fn participant_read_state_with_blip() {
        let mut model = Model::new();
        model.append_blip();
        let mut supplement = model.supplement();
        supplement.mark_participants_as_read(&root()).unwrap();
        assert!(!supplement.is_participants_unread(&root()));
    }

    #[test]
    fn participant_read_state_affected_by_wavelet_read_state() {
        let mut model = Model::new();
        let mut supplement = model.supplement();
        assert!(!supplement.have_participants_ever_been_read(&root()));
        assert!(supplement.is_participants_unread(&root()));
        supplement.mark_as_read().unwrap();
        assert!(supplement.have_participants_ever_been_read(&root()));
        assert!(!supplement.is_participants_unread(&root()));
    }

    /// A view of two wavelets with signatures.
    fn signed() -> (WaveletId, WaveletId, HashedVersion, HashedVersion, Target) {
        let id1 = wid("somewaveletid1");
        let id2 = wid("somewaveletid2");
        let signature1 = HashedVersion::of(234, vec![1]);
        let signature2 = HashedVersion::of(12828, vec![2]);
        let mut wave = StubView::default();
        wave.put(id1.clone(), 234);
        wave.put(id2.clone(), 12828);
        wave.signatures.insert(id1.clone(), signature1.clone());
        wave.signatures.insert(id2.clone(), signature2.clone());
        let supplement =
            SupplementedWaveImpl::new(PrimitiveSupplementImpl::new(), wave, DefaultFollow::Always);
        (id1, id2, signature1, signature2, supplement)
    }

    #[test]
    fn see_marks_as_seen_at_version() {
        let (id1, id2, signature1, signature2, mut supplement) = signed();
        supplement.see().unwrap();
        assert_eq!(supplement.seen_version(&id1), signature1);
        assert_eq!(supplement.seen_version(&id2), signature2);
        assert_eq!(
            supplement.supplement().primitive().seen_wavelets(),
            [id1, id2]
        );
    }

    #[test]
    fn see_wavelet_marks_one_wavelet() {
        let (id1, id2, signature1, _, mut supplement) = signed();
        supplement.see_wavelet(&id1).unwrap();
        assert_eq!(supplement.seen_version(&id1), signature1);
        assert_eq!(supplement.seen_version(&id2), HashedVersion::unsigned(0));
    }

    #[test]
    fn trashed_if_wave_in_trash() {
        let mut supplement = target();
        substrate(&mut supplement).add_folder(TRASH_FOLDER).unwrap();
        assert!(supplement.is_trashed());
    }

    #[test]
    fn not_trashed_if_wave_is_not_in_trash() {
        let mut supplement = target();
        substrate(&mut supplement).add_folder(INBOX_FOLDER).unwrap();
        assert!(!supplement.is_trashed());
    }

    #[test]
    fn has_been_seen() {
        let (_, _, _, _, mut supplement) = signed();
        assert!(!supplement.has_been_seen());
        supplement.see().unwrap();
        assert!(supplement.has_been_seen());
    }

    #[test]
    fn read_state_affected_by_tags_state() {
        let mut model = Model::new();
        assert!(model.supplement().is_tags_unread(&root()));
        // The original adds a tags document here, which changes nothing.
        let mut supplement = model.supplement();
        assert!(supplement.is_tags_unread(&root()));
        supplement.mark_as_read().unwrap();
        assert!(!supplement.is_tags_unread(&root()));
        supplement.mark_as_unread().unwrap();
        assert!(supplement.is_tags_unread(&root()));
        supplement.mark_tags_as_read(&root()).unwrap();
        assert!(!supplement.is_tags_unread(&root()));
    }

    #[test]
    fn participant_read_state_affected_by_wavelet_read_state_with_blip() {
        let mut model = Model::new();
        let b = model.append_blip();
        let mut supplement = model.supplement();
        supplement.mark_as_read().unwrap();
        assert!(!supplement.is_unread(&root(), &b));
        assert!(!supplement.is_participants_unread(&root()));
    }

    #[test]
    fn new_wave_has_no_pending_notification() {
        assert!(!target().has_pending_notification());
    }

    #[test]
    fn reports_pending_notification() {
        let mut supplement = target();
        substrate(&mut supplement)
            .set_notified_version(&w1(), W1_VERSION as i32)
            .unwrap();
        assert!(supplement.has_pending_notification());
    }

    #[test]
    fn seeing_clears_pending_notification() {
        let (_, _, _, _, mut supplement) = signed();
        supplement.mark_as_notified().unwrap();
        assert!(supplement.has_pending_notification());
        supplement.see().unwrap();
        assert!(!supplement.has_pending_notification());
    }

    #[test]
    fn seeing_clears_pending_notification_pending_notification_flag_set() {
        // The pending flag can only come from a user-data wavelet.
        let mut udw = user_data();
        append(
            &mut udw,
            SEEN_DOCUMENT,
            "<notification pending=\"true\"></notification>",
        );
        let id1 = wid("somewaveletid1");
        let mut wave = StubView::default();
        wave.put(id1.clone(), 234);
        wave.signatures.insert(id1, HashedVersion::of(234, vec![1]));
        let primitive = WaveletBasedSupplement::create(&mut udw).unwrap();
        let mut supplement = SupplementedWaveImpl::new(primitive, wave, DefaultFollow::Always);
        assert!(supplement.has_pending_notification());
        supplement.mark_as_notified().unwrap();
        assert!(supplement.has_pending_notification());
        supplement.see().unwrap();
        assert!(!supplement.has_pending_notification());
        assert!(!supplement.supplement().primitive().pending_notification());
    }

    #[test]
    fn default_follow_policies() {
        let mut wave = StubView::default();
        assert!(DefaultFollow::Always.is_followed(&wave));
        assert!(!DefaultFollow::WhenParticipant.is_followed(&wave));
        wave.is_explicit_participant = true;
        assert!(DefaultFollow::WhenParticipant.is_followed(&wave));

        let mut wave = StubView::default();
        wave.put(root(), 1);
        let mut supplement = SupplementedWaveImpl::new(
            PrimitiveSupplementImpl::new(),
            wave,
            DefaultFollow::WhenParticipant,
        );
        assert!(supplement.is_mute());
        // Not followed: archiving does nothing.
        supplement.archive().unwrap();
        assert!(archive_versions(&supplement).is_empty());
        supplement.follow().unwrap();
        assert!(supplement.is_followed());
        supplement
            .supplement_mut()
            .primitive_mut()
            .clear_follow()
            .unwrap();
        assert!(!supplement.is_followed());
    }

    #[test]
    fn versions_are_narrowed_to_int_as_in_java() {
        let mut wave = StubView::default();
        wave.put(root(), (1i64 << 32) + 5);
        let mut supplement =
            SupplementedWaveImpl::new(PrimitiveSupplementImpl::new(), wave, DefaultFollow::Always);
        supplement.mark_as_read().unwrap();
        let substrate = supplement.supplement().primitive();
        assert_eq!(substrate.last_read_wavelet_version(&root()), 5);
    }
}

// ---- what the port adds ----

mod port {
    use super::*;

    #[test]
    fn wavelet_data_view_takes_conversations_only() {
        let mut root = conversation("conv+root");
        root.add_participant(&viewer()).unwrap();
        root.data_mut()
            .set_hashed_version(HashedVersion::of(9, vec![1, 2]));
        let other = conversation("conv+other");
        let no_manifest = wavelet("conv+empty");
        let mut bad_manifest = wavelet("conv+bad");
        append(
            &mut bad_manifest,
            "conversation",
            "<x></x><conversation></conversation>",
        );
        let udw = user_data();
        let mut not_conversational = wavelet("other+data");
        append(
            &mut not_conversational,
            "conversation",
            "<conversation></conversation>",
        );
        let all = [
            udw.data(),
            other.data(),
            no_manifest.data(),
            root.data(),
            bad_manifest.data(),
            not_conversational.data(),
        ];
        let view = WaveletDataView::new(viewer(), all);
        assert_eq!(view.wavelets(), [wid("conv+other"), wid("conv+root")]);
        assert_eq!(view.version(&wid("conv+root")), 3);
        assert_eq!(view.version(&wid("conv+empty")), -1);
        assert_eq!(
            view.signature(&wid("conv+root")),
            HashedVersion::of(9, vec![1, 2])
        );
        assert_eq!(
            view.signature(&wid("conv+empty")),
            HashedVersion::unsigned(0)
        );
        assert_eq!(view.blip_version(&wid("conv+root"), "b+1"), Some(2));
        assert_eq!(view.blip_version(&wid("conv+root"), "b+2"), None);
        assert_eq!(view.blip_version(&wid("conv+empty"), "b+1"), None);
        let blips: BTreeMap<String, i64> = [("b+1".to_owned(), 2)].into();
        assert_eq!(view.blip_versions(&wid("conv+root")), blips);
        assert!(view.blip_versions(&wid("conv+empty")).is_empty());
        assert!(view.is_explicit_participant());
        let stranger = ParticipantId::of("stranger@example.com").unwrap();
        assert!(!WaveletDataView::new(stranger, all).is_explicit_participant());
    }

    /// The use the server makes of the supplement for digests: count the
    /// unread blips of a wave without writing anything.
    #[test]
    fn read_only_digest() {
        let mut root = conversation("conv+root");
        append(&mut root, "b+2", "<body><line></line></body>");
        let mut udw = user_data();
        {
            let view = WaveletDataView::new(viewer(), [root.data()]);
            let primitive = WaveletBasedSupplement::create(&mut udw).unwrap();
            let mut supplemented =
                SupplementedWaveImpl::new(primitive, view, DefaultFollow::Always);
            supplemented
                .mark_blip_as_read(&wid("conv+root"), "b+1")
                .unwrap();
            supplemented.move_to_folder(7).unwrap();
        }
        // The legacy folders the original removes on creation.
        append(
            &mut udw,
            FOLDERS_DOCUMENT,
            "<folder i=\"1\"></folder><folder i=\"3\"></folder>",
        );
        let udw = udw.into_data();
        let root = root.into_data();

        let view = WaveletDataView::new(viewer(), [&root, &udw]);
        let state = UserDataSupplement::new(&udw);
        assert_eq!(state.folders(), [7]);
        assert!(!state.is_in_folder(3));
        let supplemented = SupplementedWaveImpl::new(state, &view, DefaultFollow::Always);
        fn unread<P: ReadablePrimitiveSupplement, V: SupplementWaveView>(
            supplemented: &SupplementedWaveImpl<P, V>,
        ) -> usize {
            ["b+1", "b+2"]
                .iter()
                .filter(|blip| supplemented.is_unread(&wid("conv+root"), blip))
                .count()
        }
        assert_eq!(unread(&supplemented), 1);
        assert!(supplemented.is_archived());
        assert!(!supplemented.is_inbox());

        // The same from a snapshot, which answers without reading the
        // documents again.
        let snapshot = PrimitiveSupplementImpl::copy_of(&state);
        let supplemented = SupplementedWaveImpl::new(&snapshot, &view, DefaultFollow::Always);
        assert_eq!(unread(&supplemented), 1);
        assert_eq!(supplemented.folders(), [7]);
        assert!(supplemented.is_archived());

        // Without a user-data wavelet everything is unread and in the inbox.
        let nothing = PrimitiveSupplementImpl::new();
        let supplemented = SupplementedWaveImpl::new(&nothing, &view, DefaultFollow::Always);
        assert_eq!(unread(&supplemented), 2);
        assert!(supplemented.is_inbox());
    }

    /// The use the server makes of the supplement for robots' folder
    /// actions: the operations to submit are those the user-data wavelet
    /// recorded.
    #[test]
    fn mutations_are_recorded_as_operations() {
        let root = conversation("conv+root");
        let mut udw = user_data();
        let view = WaveletDataView::new(viewer(), [root.data()]);
        let primitive = WaveletBasedSupplement::create(&mut udw).unwrap();
        let mut supplemented = SupplementedWaveImpl::new(primitive, view, DefaultFollow::Always);
        supplemented.mark_as_read().unwrap();
        supplemented.archive().unwrap();
        let delta = udw.take_delta().unwrap();
        assert_eq!(delta.ops().len(), 3);
        assert_eq!(
            xml(&udw, READSTATE_DOCUMENT),
            "<wavelet i=\"example.com/conv+root\"><all v=\"2\"/></wavelet>"
        );
        assert_eq!(
            xml(&udw, ARCHIVING_DOCUMENT),
            "<archive i=\"example.com/conv+root\" v=\"2\"/>"
        );
    }

    #[test]
    fn create_removes_legacy_folders_and_reader_hides_them() {
        let mut udw = user_data();
        append(
            &mut udw,
            FOLDERS_DOCUMENT,
            "<folder i=\"3\"></folder><folder i=\"8\"></folder><folder i=\"1\"></folder>",
        );
        udw.take_ops();
        assert_eq!(UserDataSupplement::new(udw.data()).folders(), [8]);
        assert!(udw.ops().is_empty());
        let supplement = WaveletBasedSupplement::create(&mut udw).unwrap();
        assert_eq!(supplement.folders(), [8]);
        assert_eq!(supplement.user_data().ops().len(), 2);
        assert_eq!(xml(&udw, FOLDERS_DOCUMENT), "<folder i=\"8\"/>");
    }

    #[test]
    fn malformed_state_is_ignored() {
        let mut udw = user_data();
        append(
            &mut udw,
            READSTATE_DOCUMENT,
            "<wavelet i=\"not an id\"><all v=\"5\"></all></wavelet>\
             <wavelet i=\"example.com/conv+root\"><all v=\"x\"></all><all></all>\
             <blip v=\"3\"></blip><blip i=\"b+1\" v=\"\"></blip><blip i=\"b+2\" v=\"4\"></blip>\
             text</wavelet>",
        );
        append(
            &mut udw,
            PRESENTATION_DOCUMENT,
            "<conversation i=\"example.com/conv+root\">\
             <thread i=\"t\" state=\"collapsed\"></thread></conversation>",
        );
        append(
            &mut udw,
            SEEN_DOCUMENT,
            "<seen i=\"example.com/conv+root\" signature=\"x:y\"></seen>\
             <notified i=\"example.com/conv+root\" v=\"1.5\"></notified>",
        );
        append(
            &mut udw,
            ABUSE_DOCUMENT,
            "<wanted wavelet_id=\"example.com/conv+root\" certainty=\"high\"></wanted>",
        );
        append(
            &mut udw,
            FOLDERS_DOCUMENT,
            "<folder></folder><folder i=\"x\"></folder>",
        );
        let state = UserDataSupplement::new(udw.data());
        let root = wid("conv+root");
        assert_eq!(state.read_wavelets(), std::slice::from_ref(&root));
        assert_eq!(state.last_read_wavelet_version(&root), NO_VERSION);
        assert_eq!(state.read_blips(&root), ["b+2"]);
        assert_eq!(state.last_read_blip_version(&root, "b+1"), NO_VERSION);
        assert_eq!(state.last_read_blip_version(&root, "b+2"), 4);
        assert_eq!(state.thread_state(&root, "t"), None);
        assert!(state.stateful_threads(&root).is_empty());
        assert_eq!(
            state.wavelets_with_thread_state(),
            std::slice::from_ref(&root)
        );
        assert_eq!(state.seen_version(&root), HashedVersion::unsigned(0));
        assert!(state.seen_wavelets().is_empty());
        assert_eq!(state.notified_version(&root), NO_VERSION);
        assert!(state.wanted_evaluations().is_empty());
        assert!(state.folders().is_empty());
    }

    #[test]
    fn clear_blip_read_state_of_unknown_wavelet_does_nothing() {
        let mut udw = user_data();
        let mut supplement = WaveletBasedSupplement::create(&mut udw).unwrap();
        supplement
            .clear_blip_read_state(&wid("conv+root"), "b+1")
            .unwrap();
        assert!(udw.ops().is_empty());
    }

    #[test]
    fn in_memory_wavelet_version_quirk() {
        let mut target = PrimitiveSupplementImpl::new();
        let id = wid("conv+root");
        target.set_last_read_wavelet_version(&id, 5).unwrap();
        // Set, and no participants version to compare with: the original
        // throws a NullPointerException.
        assert!(matches!(
            target.set_last_read_wavelet_version(&id, 9),
            Err(SupplementError::IllegalState(_))
        ));
        assert_eq!(target.last_read_wavelet_version(&id), 5);
        // Replaced only while less than the participants version.
        target.set_last_read_participants_version(&id, 7).unwrap();
        target.set_last_read_wavelet_version(&id, 9).unwrap();
        assert_eq!(target.last_read_wavelet_version(&id), 9);
        target.set_last_read_wavelet_version(&id, 12).unwrap();
        assert_eq!(target.last_read_wavelet_version(&id), 9);
    }

    #[test]
    fn partitioning_keeps_thread_and_gadget_state_apart() {
        let id = wid("conv+root");
        let collapsed = Some(ThreadState::Collapsed);
        let mut target =
            PartitioningPrimitiveSupplement::new(PrimitiveSupplementImpl::new(), false, false);
        target.set_thread_state(&id, "t", collapsed).unwrap();
        assert_eq!(target.thread_state(&id, "t"), collapsed);
        assert_eq!(target.stateful_threads(&id), ["t"]);
        assert_eq!(
            target.wavelets_with_thread_state(),
            std::slice::from_ref(&id)
        );
        assert_eq!(target.real().thread_state(&id, "t"), None);
        target.set_gadget_state("g", "k", Some("v")).unwrap();
        assert!(target.gadget_state("g").is_empty());
        assert!(target.real().gadget_state("g").is_empty());
        target.add_folder(4).unwrap();
        assert_eq!(target.real().folders(), [4]);
        target.stop_partitioning();
        assert_eq!(target.thread_state(&id, "t"), None);

        let mut target =
            PartitioningPrimitiveSupplement::new(PrimitiveSupplementImpl::new(), true, true);
        target.set_thread_state(&id, "t", collapsed).unwrap();
        target.set_gadget_state("g", "k", Some("v")).unwrap();
        target.stop_partitioning();
        assert_eq!(target.thread_state(&id, "t"), collapsed);
        let real = target.into_real();
        assert_eq!(real.thread_state(&id, "t"), collapsed);
        assert_eq!(real.gadget_state("g")["k"], "v");
    }

    #[test]
    fn thread_state_names() {
        assert_eq!(ThreadState::Collapsed.name(), "COLLAPSED");
        assert_eq!(ThreadState::Expanded.name(), "EXPANDED");
        assert_eq!(
            <ThreadState as crate::adt::Serializable>::from_attribute("EXPANDED"),
            Some(ThreadState::Expanded)
        );
        assert_eq!(
            <ThreadState as crate::adt::Serializable>::from_attribute("expanded"),
            None
        );
    }
}
