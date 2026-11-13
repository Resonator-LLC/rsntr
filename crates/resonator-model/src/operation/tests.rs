//! Ports of the Apache Wave unit tests for wavelet operations:
//! `AddParticipantTest`, `BlipContentOperationTest`, `BlipOperationTest`,
//! `BlipOperationVisitorTest`, `OperationEqualityTest`,
//! `ParticipantTransformTest`, `VersionUpdateOpTest`, `WaveletOperationTest`
//! and `WorthyChangeCheckerTest` (`wave.model.operation.wave`),
//! `DeltaPairTest` (`wave.concurrencycontrol.client`),
//! `OpComparatorsLargeTest` (`wave.model.operation`) and
//! `WaveletDataUtilTest` (`box.server.util`), plus tests of the submitted
//! delta transformation taken from `WaveletContainerImpl`.
//!
//! Blip operations are applied through wavelet operations on a real
//! [`WaveletData`]; the original applies them to blips of test doubles.

use super::*;
use crate::data::WaveletData;
use crate::docop::{Attributes, DocOpBuilder};
use crate::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use crate::version::HashedVersion;

fn participant(address: &str) -> ParticipantId {
    ParticipantId::of(address).unwrap()
}

fn wavelet_name() -> WaveletName {
    WaveletName::of(
        WaveId::of("example.com", "w+wave").unwrap(),
        WaveletId::of("example.com", "conv+root").unwrap(),
    )
}

/// The wavelet `WaveletDataFactory.create()` gives: empty, at unsigned
/// version zero and time zero.
fn empty_wavelet() -> WaveletData {
    WaveletData::empty(
        &wavelet_name(),
        participant("fake@example.com"),
        HashedVersion::unsigned(0),
        0,
    )
}

/// `ModelTestUtils.createContent`.
fn create_content(text: &str) -> DocInitialization {
    let mut builder = DocOpBuilder::new()
        .element_start("body", Attributes::new())
        .element_start("line", Attributes::new())
        .element_end();
    if !text.is_empty() {
        builder = builder.characters(text);
    }
    builder.element_end().build_initialization().unwrap()
}

/// `DeltaTestUtil.insert`: an insertion into the document "blip id".
fn insert(
    author: &ParticipantId,
    pos: usize,
    text: &str,
    remaining: usize,
    resulting_version: Option<HashedVersion>,
) -> WaveletOperation {
    let mut builder = DocOpBuilder::new().retain(pos).characters(text);
    if remaining > 0 {
        builder = builder.retain(remaining);
    }
    let context = match resulting_version {
        Some(version) => {
            WaveletOperationContext::with_hashed_version(author.clone(), 0, 1, version)
        }
        None => WaveletOperationContext::new(author.clone(), 0, 1),
    };
    WaveletOperation::blip_content(context, "blip id", builder.build().unwrap())
}

/// Asserts that `operation` is the insertion `DeltaTestUtil.insert` builds.
fn check_insert(operation: &WaveletOperation, location: usize, content: &str, remaining: usize) {
    let WaveletOperationKind::Blip {
        op: BlipOperation::Content { op, .. },
        ..
    } = operation.kind()
    else {
        panic!("Did not get an insertion operation.");
    };
    let mut builder = DocOpBuilder::new().retain(location).characters(content);
    if remaining > 0 {
        builder = builder.retain(remaining);
    }
    assert!(syntactic_identity(&builder.build().unwrap(), op));
}

mod add_participant_test {
    use super::*;

    fn creator() -> ParticipantId {
        participant("abc@example.com")
    }

    fn another() -> ParticipantId {
        participant("def@example.com")
    }

    fn a_third() -> ParticipantId {
        participant("xyz@example.com")
    }

    fn wavelet() -> WaveletData {
        WaveletData::new(
            WaveletId::of("example.com", "conv+root").unwrap(),
            creator(),
            0,
            0,
            HashedVersion::unsigned(0),
            0,
            WaveId::of("example.com", "c+123").unwrap(),
        )
    }

    fn create_context() -> WaveletOperationContext {
        WaveletOperationContext::new(creator(), NO_TIMESTAMP, 1)
    }

    fn add_at(participant: ParticipantId, position: usize) -> WaveletOperation {
        WaveletOperation::new(
            create_context(),
            WaveletOperationKind::AddParticipant {
                participant,
                position: Some(position),
            },
        )
    }

    #[test]
    fn test_add_many_participants() {
        let mut wavelet = wavelet();
        let mut participants: Vec<ParticipantId> = Vec::new();
        assert_eq!(participants, wavelet.participants());

        WaveletOperation::add_participant(create_context(), creator())
            .apply(&mut wavelet)
            .unwrap();
        participants.push(creator());
        assert_eq!(participants, wavelet.participants());

        for i in 0..10 {
            let p = participant(&format!("abc{i}@example.com"));
            participants.push(p.clone());
            WaveletOperation::add_participant(create_context(), p)
                .apply(&mut wavelet)
                .unwrap();
        }
        assert_eq!(participants, wavelet.participants());
    }

    #[test]
    fn test_cannot_add_same_participant_twice() {
        let mut wavelet = wavelet();
        add_at(creator(), 0).apply(&mut wavelet).unwrap();
        assert_eq!(wavelet.participants(), [creator()]);
        assert_eq!(
            add_at(creator(), 0).apply(&mut wavelet),
            Err(WaveletOperationError::DuplicateParticipant(
                creator().to_string()
            ))
        );
        // Adding another is ok.
        add_at(another(), 0).apply(&mut wavelet).unwrap();
    }

    #[test]
    fn test_reverse_of_add_participant_is_remove_participant() {
        let mut wavelet = wavelet();
        assert_eq!(
            vec![WaveletOperation::remove_participant(
                create_context(),
                creator()
            )],
            add_at(creator(), 0)
                .apply_and_return_reverse(&mut wavelet)
                .unwrap()
        );
    }

    #[test]
    fn test_cannot_remove_non_participant() {
        let mut wavelet = wavelet();
        let remove = WaveletOperation::remove_participant(create_context(), creator());
        let missing = Err(WaveletOperationError::MissingParticipant(
            creator().to_string(),
        ));
        assert_eq!(remove.apply(&mut wavelet), missing);

        add_at(creator(), 0).apply(&mut wavelet).unwrap();
        // Now it's ok to remove.
        remove.apply(&mut wavelet).unwrap();
        assert!(wavelet.participants().is_empty());

        assert_eq!(remove.apply(&mut wavelet), missing);
    }

    #[test]
    fn test_reverse_of_remove_participant_is_add_participant_with_position() {
        let mut wavelet = wavelet();
        let participants = vec![creator(), another(), a_third()];
        for p in &participants {
            wavelet.add_participant(p.clone());
        }
        assert_eq!(participants, wavelet.participants());

        // The reverse of removing any participant is an add at its position
        // which, when applied, restores the participant list.
        for (i, p) in participants.iter().enumerate() {
            let reverse = WaveletOperation::remove_participant(create_context(), p.clone())
                .apply_and_return_reverse(&mut wavelet)
                .unwrap();
            assert_eq!(vec![add_at(p.clone(), i)], reverse);
            reverse[0].apply(&mut wavelet).unwrap();
            assert_eq!(participants, wavelet.participants());
        }
    }
}

/// Fixture of `OperationTestBase`.
mod operation_test_base {
    use super::*;

    pub const CREATION_TIMESTAMP: i64 = 100;
    pub const LAST_MODIFIED_TIMESTAMP: i64 = CREATION_TIMESTAMP + 10;
    pub const CONTEXT_TIMESTAMP: i64 = LAST_MODIFIED_TIMESTAMP + 5;
    pub const CONTEXT_VERSION: u64 = 4;

    pub fn creator() -> ParticipantId {
        participant("lars@example.com")
    }

    pub fn fred() -> ParticipantId {
        participant("fred@example.com")
    }

    pub fn jane() -> ParticipantId {
        participant("jane@example.com")
    }

    pub fn context_hashed_version() -> HashedVersion {
        HashedVersion::of(CONTEXT_VERSION, [4u8, 4, 4, 4])
    }

    pub fn context() -> WaveletOperationContext {
        WaveletOperationContext::new(fred(), CONTEXT_TIMESTAMP, 1)
    }
}

mod blip_content_operation_test {
    use super::operation_test_base::*;
    use super::*;

    fn doc_op() -> DocOp {
        DocOpBuilder::new().characters("Hello").build().unwrap()
    }

    #[test]
    fn test_apply() {
        let mut wavelet = empty_wavelet();
        wavelet
            .create_document("root", jane(), [], &DocInitialization::default(), 0, 0)
            .unwrap();
        WaveletOperation::blip_content(context(), "root", doc_op())
            .apply(&mut wavelet)
            .unwrap();
        let blip = wavelet.document("root").unwrap();
        // The op reached the document.
        assert_eq!(&doc_op(), blip.content().as_operation().as_op());
        // Editing the document makes the op creator a blip contributor.
        assert_eq!(blip.contributors(), [fred()]);
    }

    #[test]
    fn test_reverse_restores_content() {
        let mut wavelet = empty_wavelet();
        wavelet
            .create_document("root", fred(), [], &DocInitialization::default(), 0, 0)
            .unwrap();
        let reverse_ops = WaveletOperation::blip_content(context(), "root", doc_op())
            .apply_and_return_reverse(&mut wavelet)
            .unwrap();
        assert_eq!(
            wavelet.document("root").unwrap().content().to_xml_string(),
            "Hello"
        );
        for reverse in &reverse_ops {
            reverse.apply(&mut wavelet).unwrap();
        }
        let blip = wavelet.document("root").unwrap();
        assert_eq!(blip.content().to_xml_string(), "");
        assert!(blip.contributors().is_empty());
    }
}

mod blip_operation_test {
    use super::operation_test_base::*;
    use super::*;

    const BLIP_ID: &str = "blipid";

    fn jane_context() -> WaveletOperationContext {
        WaveletOperationContext::new(jane(), 42, 1)
    }

    /// An op that doesn't change the document but still is worthy.
    fn sample_content_operation_in(
        context: WaveletOperationContext,
        method: UpdateContributorMethod,
    ) -> WaveletOperation {
        let op = DocOpBuilder::new()
            .retain(1)
            .delete_element_start("line", Attributes::new())
            .delete_element_end()
            .element_start("line", Attributes::new())
            .element_end()
            .retain(1)
            .build()
            .unwrap();
        assert!(is_worthy(&op));
        WaveletOperation::blip(context, BLIP_ID, BlipOperation::Content { op, method })
    }

    fn sample_content_operation(method: UpdateContributorMethod) -> WaveletOperation {
        sample_content_operation_in(context(), method)
    }

    /// A wavelet with the blip `createBlipData()` makes.
    fn wavelet_with_blip() -> WaveletData {
        let mut wavelet = empty_wavelet();
        wavelet
            .create_document(BLIP_ID, fred(), [], &create_content(""), 0, 0)
            .unwrap();
        wavelet
    }

    fn contributors(wavelet: &WaveletData) -> &[ParticipantId] {
        wavelet.document(BLIP_ID).unwrap().contributors()
    }

    #[test]
    fn test_get_context() {
        let op = WaveletOperation::blip(context(), BLIP_ID, BlipOperation::Submit);
        assert_eq!(op.context(), &context());
    }

    #[test]
    fn test_apply_updates_timestamp() {
        let mut wavelet = wavelet_with_blip();
        let op = sample_content_operation(UpdateContributorMethod::Add);
        let old_wavelet_version = wavelet.version();
        assert_ne!(
            context().timestamp(),
            wavelet.document(BLIP_ID).unwrap().last_modified_time()
        );
        op.apply(&mut wavelet).unwrap();
        let blip = wavelet.document(BLIP_ID).unwrap();
        // The last modified time is now the context time.
        assert_eq!(context().timestamp(), blip.last_modified_time());
        // The blip version is the wavelet version before the op plus one.
        assert_eq!(old_wavelet_version + 1, blip.last_modified_version());
    }

    #[test]
    fn test_none_contributor_method_leaves_contributors() {
        let mut wavelet = wavelet_with_blip();
        sample_content_operation(UpdateContributorMethod::None)
            .apply(&mut wavelet)
            .unwrap();
        assert!(contributors(&wavelet).is_empty());
    }

    #[test]
    fn test_add_contributor_method_adds_new_contributor() {
        let mut wavelet = wavelet_with_blip();
        sample_content_operation(UpdateContributorMethod::Add)
            .apply(&mut wavelet)
            .unwrap();
        assert!(contributors(&wavelet).contains(&fred()));

        sample_content_operation_in(jane_context(), UpdateContributorMethod::Add)
            .apply(&mut wavelet)
            .unwrap();
        assert!(contributors(&wavelet).contains(&fred()));
        assert!(contributors(&wavelet).contains(&jane()));
    }

    #[test]
    fn test_add_contributor_method_doesnt_duplicate_contributors() {
        let mut wavelet = wavelet_with_blip();
        sample_content_operation(UpdateContributorMethod::Add)
            .apply(&mut wavelet)
            .unwrap();
        sample_content_operation(UpdateContributorMethod::Add)
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(contributors(&wavelet), [fred()]);
    }

    #[test]
    fn test_reverse_add_contributor_removes_contributor() {
        let mut wavelet = wavelet_with_blip();
        // Adds fred.
        let reverse_ops = sample_content_operation(UpdateContributorMethod::Add)
            .apply_and_return_reverse(&mut wavelet)
            .unwrap();
        assert_eq!(contributors(&wavelet), [fred()]);
        for reverse in &reverse_ops {
            reverse.apply(&mut wavelet).unwrap();
        }
        assert!(contributors(&wavelet).is_empty());
    }
}

/// The visitor is replaced by matching on [`BlipOperation`]; these check
/// that each variant is told apart.
mod blip_operation_visitor_test {
    use super::*;

    #[test]
    fn test_blip_content_operation_visitor() {
        let doc_op = create_content("Hello").into_op();
        let op = BlipOperation::content(doc_op.clone());
        match &op {
            BlipOperation::Content { op: visited, .. } => assert_eq!(visited, &doc_op),
            BlipOperation::Submit => panic!("visited as a submit"),
        }
    }

    #[test]
    fn test_submit_blip_visitor() {
        let op = BlipOperation::Submit;
        assert!(matches!(op, BlipOperation::Submit));
    }
}

mod operation_equality_test {
    use super::operation_test_base::*;
    use super::*;

    fn create_wavelet_operations() -> Vec<WaveletOperation> {
        vec![
            WaveletOperation::add_participant(context(), fred()),
            WaveletOperation::add_participant(context(), jane()),
            WaveletOperation::blip_content(context(), "b", create_content("Hello").into_op()),
            WaveletOperation::blip_content(context(), "b", create_content("World").into_op()),
            WaveletOperation::no_op(context()),
            WaveletOperation::remove_participant(context(), fred()),
            WaveletOperation::remove_participant(context(), jane()),
            WaveletOperation::blip(context(), "b", BlipOperation::Submit),
            WaveletOperation::version_update(fred(), 1, None, Some("id1".to_owned()), None),
            WaveletOperation::version_update(fred(), 1, None, Some("id2".to_owned()), None),
        ]
    }

    /// The blip operations of the original list, unwrapped.
    fn create_blip_operations() -> Vec<BlipOperation> {
        vec![
            BlipOperation::content(create_content("Hello").into_op()),
            BlipOperation::content(create_content("World").into_op()),
            BlipOperation::Submit,
        ]
    }

    fn check_pairwise<T: PartialEq + std::fmt::Debug>(operations1: &[T], operations2: &[T]) {
        for (i, a) in operations1.iter().enumerate() {
            for (j, b) in operations2.iter().enumerate() {
                assert_eq!(
                    i == j,
                    a == b,
                    "failed pair's equals [i:{i}, {a:?}] [j:{j}, {b:?}]"
                );
            }
        }
    }

    #[test]
    fn test_equality() {
        check_pairwise(&create_wavelet_operations(), &create_wavelet_operations());
        check_pairwise(&create_blip_operations(), &create_blip_operations());
    }
}

mod participant_transform_test {
    use super::*;

    struct Ops {
        remove1a: WaveletOperation,
        remove2a: WaveletOperation,
        remove2b: WaveletOperation,
        add1a: WaveletOperation,
        add2a: WaveletOperation,
        add2b: WaveletOperation,
        noop1: WaveletOperation,
        noop2: WaveletOperation,
        mutation: WaveletOperation,
    }

    fn ops() -> Ops {
        let a = participant("a@google.com");
        let b = participant("b@google.com");
        let context1 = WaveletOperationContext::new(participant("p1@google.com"), 1, 1);
        let context2 = WaveletOperationContext::new(participant("p2@google.com"), 1, 1);
        let context_a = WaveletOperationContext::new(a.clone(), 1, 1);
        Ops {
            remove1a: WaveletOperation::remove_participant(context1.clone(), a.clone()),
            remove2a: WaveletOperation::remove_participant(context2.clone(), a.clone()),
            remove2b: WaveletOperation::remove_participant(context2.clone(), b.clone()),
            add1a: WaveletOperation::add_participant(context1.clone(), a.clone()),
            add2a: WaveletOperation::add_participant(context2.clone(), a),
            add2b: WaveletOperation::add_participant(context2.clone(), b),
            noop1: WaveletOperation::no_op(context1),
            noop2: WaveletOperation::no_op(context2),
            mutation: WaveletOperation::blip_content(
                context_a,
                "dummy",
                DocOpBuilder::new().characters("x").build().unwrap(),
            ),
        }
    }

    fn check_transform(
        client_operation: &WaveletOperation,
        server_operation: &WaveletOperation,
        transformed_client_operation: &WaveletOperation,
        transformed_server_operation: &WaveletOperation,
    ) {
        let pair = transform(client_operation, server_operation).expect("Unexpected error");
        assert_eq!(transformed_client_operation, &pair.client_op);
        assert_eq!(transformed_server_operation, &pair.server_op);
        // Not in the original: the contexts stay with their sides.
        assert_eq!(client_operation.context(), pair.client_op.context());
        assert_eq!(server_operation.context(), pair.server_op.context());
    }

    fn check_identity_transform(
        client_operation: &WaveletOperation,
        server_operation: &WaveletOperation,
    ) {
        check_transform(
            client_operation,
            server_operation,
            client_operation,
            server_operation,
        );
    }

    /// A removed participant issuing an operation is a distinct error.
    #[test]
    fn test_removed_author_exception() {
        let o = ops();
        assert_eq!(
            transform(&o.mutation, &o.remove2a),
            Err(WaveTransformError::RemovedAuthor("a@google.com".to_owned()))
        );
    }

    #[test]
    fn test_no_exception() {
        let o = ops();
        check_identity_transform(&o.remove2a, &o.mutation);
        check_identity_transform(&o.mutation, &o.remove2b);
        check_identity_transform(&o.remove2b, &o.mutation);
    }

    #[test]
    fn test_addition_vs_addition() {
        let o = ops();
        check_transform(&o.add1a, &o.add2a, &o.noop1, &o.noop2);
        check_identity_transform(&o.add1a, &o.add2b);
    }

    #[test]
    fn test_addition_vs_removal() {
        let o = ops();
        let involving_a = Err(WaveTransformError::Participant("a@google.com".to_owned()));
        assert_eq!(transform(&o.add1a, &o.remove2a), involving_a);
        assert_eq!(transform(&o.remove2a, &o.add1a), involving_a);
        check_identity_transform(&o.add1a, &o.remove2b);
        check_identity_transform(&o.remove2b, &o.add1a);
    }

    #[test]
    fn test_removal_vs_removal() {
        let o = ops();
        check_transform(&o.remove1a, &o.remove2a, &o.noop1, &o.noop2);
        check_identity_transform(&o.remove1a, &o.remove2b);
    }
}

mod version_update_op_test {
    use super::*;

    const ROOT_BLIP: &str = "root";
    const SIGNATURE: [u8; 4] = [4, 4, 4, 4];
    const SIGNATURE2: [u8; 4] = [8, 8, 8, 8];

    fn creator() -> ParticipantId {
        participant("lars@gwave.com")
    }

    fn fred() -> ParticipantId {
        participant("fred@gwave.com")
    }

    /// A context typical of a locally generated op; doesn't update metadata.
    fn local_context() -> WaveletOperationContext {
        WaveletOperationContext::new(creator(), NO_TIMESTAMP, 0)
    }

    fn assert_blip_version(wavelet: &WaveletData, blip_id: &str, last_modified_version: u64) {
        assert_eq!(
            last_modified_version,
            wavelet.document(blip_id).unwrap().last_modified_version()
        );
    }

    fn assert_wavelet_metadata_is_initial_state(wavelet: &WaveletData) {
        assert_eq!(0, wavelet.version());
        assert_eq!(&HashedVersion::unsigned(0), wavelet.hashed_version());
        if wavelet.document(ROOT_BLIP).is_some() {
            assert_blip_version(wavelet, ROOT_BLIP, 0);
        }
    }

    /// Applies an op that adds fred and returns it.
    fn add_participant(wavelet: &mut WaveletData) -> WaveletOperation {
        let op = WaveletOperation::add_participant(local_context(), fred());
        op.apply(wavelet).unwrap();
        op
    }

    /// Applies an empty document op to a blip and returns it.
    fn touch_blip(wavelet: &mut WaveletData, blip_id: &str) -> WaveletOperation {
        let op = WaveletOperation::blip_content(local_context(), blip_id, DocOp::default());
        op.apply(wavelet).unwrap();
        op
    }

    /// Applies a document mutation to a blip and returns it.
    fn mutate_blip(wavelet: &mut WaveletData, blip_id: &str) -> WaveletOperation {
        let op = WaveletOperation::blip_content(
            local_context(),
            blip_id,
            create_content("Hello").into_op(),
        );
        op.apply(wavelet).unwrap();
        op
    }

    #[test]
    fn test_version_update_op_from_wavelet_op_ack() {
        let mut wavelet = empty_wavelet();
        assert_wavelet_metadata_is_initial_state(&wavelet);

        // Apply an op that has no version information.
        let op = add_participant(&mut wavelet);
        assert_wavelet_metadata_is_initial_state(&wavelet);

        // Create version update op from ack.
        let update_op = op.create_version_update_op(5, Some(HashedVersion::of(5, SIGNATURE)));
        update_op.apply(&mut wavelet).unwrap();
        assert_eq!(5, wavelet.version());
        assert_eq!(&HashedVersion::of(5, SIGNATURE), wavelet.hashed_version());
    }

    #[test]
    fn test_version_update_op_from_blip_no_op_ack() {
        let mut wavelet = empty_wavelet();
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let wavelet_op = touch_blip(&mut wavelet, ROOT_BLIP);
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let update_op =
            wavelet_op.create_version_update_op(1, Some(HashedVersion::of(1, SIGNATURE)));
        update_op.apply(&mut wavelet).unwrap();
        assert_eq!(1, wavelet.version());
        assert_eq!(&HashedVersion::of(1, SIGNATURE), wavelet.hashed_version());
        assert_blip_version(&wavelet, ROOT_BLIP, 0);
    }

    #[test]
    fn test_version_update_op_from_blip_content_operation_ack() {
        let mut wavelet = empty_wavelet();
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let wavelet_op = mutate_blip(&mut wavelet, ROOT_BLIP);
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let update_op =
            wavelet_op.create_version_update_op(1, Some(HashedVersion::of(1, SIGNATURE)));
        update_op.apply(&mut wavelet).unwrap();
        assert_eq!(1, wavelet.version());
        assert_eq!(&HashedVersion::of(1, SIGNATURE), wavelet.hashed_version());
        assert_blip_version(&wavelet, ROOT_BLIP, 1);
    }

    #[test]
    fn test_version_update_op_from_submit_ack() {
        let mut wavelet = empty_wavelet();
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let wavelet_op = WaveletOperation::blip(local_context(), ROOT_BLIP, BlipOperation::Submit);
        wavelet_op.apply(&mut wavelet).unwrap();
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let update_op =
            wavelet_op.create_version_update_op(1, Some(HashedVersion::of(1, SIGNATURE)));
        update_op.apply(&mut wavelet).unwrap();
        assert_eq!(1, wavelet.version());
        assert_eq!(&HashedVersion::of(1, SIGNATURE), wavelet.hashed_version());
        assert_blip_version(&wavelet, ROOT_BLIP, 0);
    }

    #[test]
    fn test_multiple_version_updates() {
        let mut wavelet = empty_wavelet();
        assert_wavelet_metadata_is_initial_state(&wavelet);

        // First add participant.
        let add_participant_op = add_participant(&mut wavelet);
        add_participant_op
            .create_version_update_op(1, Some(HashedVersion::of(1, SIGNATURE)))
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(1, wavelet.version());
        assert_eq!(&HashedVersion::of(1, SIGNATURE), wavelet.hashed_version());

        // Then mutate blip.
        let mutate_blip_op = mutate_blip(&mut wavelet, ROOT_BLIP);
        mutate_blip_op
            .create_version_update_op(1, Some(HashedVersion::of(2, SIGNATURE2)))
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(2, wavelet.version());
        assert_eq!(&HashedVersion::of(2, SIGNATURE2), wavelet.hashed_version());
        assert_blip_version(&wavelet, ROOT_BLIP, 2);
    }

    #[test]
    fn test_reverse() {
        let mut wavelet = empty_wavelet();
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let wavelet_op = mutate_blip(&mut wavelet, ROOT_BLIP);
        assert_wavelet_metadata_is_initial_state(&wavelet);

        let update_op =
            wavelet_op.create_version_update_op(1, Some(HashedVersion::of(1, SIGNATURE)));
        let reverse = update_op.apply_and_return_reverse(&mut wavelet).unwrap();
        assert_eq!(1, wavelet.version());
        assert_eq!(&HashedVersion::of(1, SIGNATURE), wavelet.hashed_version());
        assert_blip_version(&wavelet, ROOT_BLIP, 1);

        // Applying the reverse restores the versions.
        assert_eq!(1, reverse.len());
        reverse[0].apply(&mut wavelet).unwrap();
        assert_wavelet_metadata_is_initial_state(&wavelet);
    }
}

mod wavelet_operation_test {
    use super::operation_test_base::*;
    use super::*;

    #[test]
    fn test_op_updates_timestamp() {
        let mut wavelet = empty_wavelet();
        let context = WaveletOperationContext::new(creator(), CONTEXT_TIMESTAMP, 0);
        WaveletOperation::no_op(context)
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(CONTEXT_TIMESTAMP, wavelet.last_modified_time());
    }

    #[test]
    fn test_op_without_timestamp_doesnt_update_timestamp() {
        let mut wavelet = empty_wavelet();
        let context = WaveletOperationContext::new(creator(), NO_TIMESTAMP, 0);
        let old_timestamp = wavelet.last_modified_time();
        WaveletOperation::no_op(context)
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(old_timestamp, wavelet.last_modified_time());
    }

    #[test]
    fn test_op_updates_version() {
        let mut wavelet = empty_wavelet();
        let context = WaveletOperationContext::new(creator(), NO_TIMESTAMP, 1);
        let old_version = wavelet.version();
        WaveletOperation::no_op(context)
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(old_version + 1, wavelet.version());
    }

    #[test]
    fn test_op_updates_signature() {
        let mut wavelet = empty_wavelet();
        let context = WaveletOperationContext::with_hashed_version(
            creator(),
            NO_TIMESTAMP,
            0,
            context_hashed_version(),
        );
        WaveletOperation::no_op(context)
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(&context_hashed_version(), wavelet.hashed_version());
    }

    #[test]
    fn test_op_without_signature_doesnt_update_signature() {
        let mut wavelet = empty_wavelet();
        let old_hashed_version = wavelet.hashed_version().clone();
        let context = WaveletOperationContext::new(creator(), NO_TIMESTAMP, 0);
        WaveletOperation::no_op(context)
            .apply(&mut wavelet)
            .unwrap();
        assert_eq!(&old_hashed_version, wavelet.hashed_version());
    }

    /// `createReverseContext` is not public; the reverse context is read
    /// off the reverse of a no-op.
    #[test]
    fn test_create_reverse_context_reverses_context() {
        let mut wavelet = empty_wavelet();
        let old_timestamp = wavelet.last_modified_time();
        let old_hashed_version = wavelet.hashed_version().clone();
        let context = WaveletOperationContext::with_hashed_version(
            creator(),
            CONTEXT_TIMESTAMP,
            1,
            context_hashed_version(),
        );
        let reverse_ops = WaveletOperation::no_op(context.clone())
            .apply_and_return_reverse(&mut wavelet)
            .unwrap();
        assert_eq!(1, reverse_ops.len());
        let reverse = reverse_ops[0].context();
        assert_eq!(context.creator(), reverse.creator());
        assert_eq!(-context.version_increment(), reverse.version_increment());
        assert_eq!(old_timestamp, reverse.timestamp());
        assert_eq!(Some(&old_hashed_version), reverse.hashed_version());
    }
}

mod worthy_change_checker_test {
    use super::*;
    use crate::docop::{AnnotationBoundaryMap, BootstrapDocument};
    use crate::operation::worthy::THREAD_INLINE_ANCHOR_TAGNAME;

    /// The original derives the deletion from a `Nindo` against the
    /// document; here it is written out and checked to apply.
    #[test]
    fn test_anchor_removal_is_unworthy1() {
        let initialization = DocOpBuilder::new()
            .annotation_boundary(
                AnnotationBoundaryMap::changes_only([("a", None, Some("b"))]).unwrap(),
            )
            .characters("a")
            .annotation_boundary(AnnotationBoundaryMap::ends(["a"]).unwrap())
            .characters("a")
            .element_start(THREAD_INLINE_ANCHOR_TAGNAME, Attributes::new())
            .element_end()
            .build_initialization()
            .unwrap();
        let mut d = BootstrapDocument::new();
        d.consume(initialization.as_op()).unwrap();

        let op = DocOpBuilder::new()
            .retain(2)
            .delete_element_start(THREAD_INLINE_ANCHOR_TAGNAME, Attributes::new())
            .delete_element_end()
            .build()
            .unwrap();
        d.consume(&op).unwrap();
        assert!(!is_worthy(&op));
    }
}

mod delta_pair_test {
    use super::*;

    fn client() -> ParticipantId {
        participant("client@example.com")
    }

    fn server() -> ParticipantId {
        participant("server@example.com")
    }

    fn check_version_update(
        operation: &WaveletOperation,
        version_increment: i64,
        distinct_version: Option<&HashedVersion>,
    ) {
        assert!(matches!(
            operation.kind(),
            WaveletOperationKind::VersionUpdate { .. }
        ));
        assert_eq!(version_increment, operation.context().version_increment());
        assert_eq!(distinct_version, operation.context().hashed_version());
    }

    #[test]
    fn test_multiple_client_server_ops() {
        // Client inserts ".A.B".
        let client_ops = vec![
            insert(&client(), 1, "A", 1, None),
            insert(&client(), 3, "B", 0, None),
        ];
        // Server inserts ".2.1".
        let server_ops = vec![
            insert(&server(), 2, "1", 0, None),
            insert(&server(), 1, "2", 2, None),
        ];

        let pair = DeltaPair::new(client_ops, server_ops).transform().unwrap();

        // With the same insertion point, the client op goes to the left of
        // the server op. The client inserts ".A..B.".
        assert_eq!(2, pair.client.len());
        check_insert(&pair.client[0], 1, "A", 3);
        check_insert(&pair.client[1], 4, "B", 1);

        // The server inserts "..2..1".
        assert_eq!(2, pair.server.len());
        check_insert(&pair.server[0], 4, "1", 0);
        check_insert(&pair.server[1], 2, "2", 3);
    }

    #[test]
    fn test_is_same() {
        let client_ops = vec![
            insert(&client(), 1, "A", 1, None),
            insert(&client(), 3, "B", 0, None),
        ];
        // The server has the same ops by the same author.
        let resulting_version = HashedVersion::of(1, [1u8, 2, 3, 4]);
        let server_ops = vec![
            insert(&client(), 1, "A", 1, None),
            insert(&client(), 3, "B", 0, Some(resulting_version.clone())),
        ];

        // Deltas with the same ops are the same; other info is ignored.
        assert!(DeltaPair::are_same(&client_ops, &server_ops));

        // Transforming leaves only version update server ops.
        let pair = DeltaPair::new(client_ops, server_ops).transform().unwrap();
        assert_eq!(0, pair.client.len());
        assert_eq!(2, pair.server.len());
        check_version_update(&pair.server[0], 1, None);
        check_version_update(&pair.server[1], 1, Some(&resulting_version));
    }
}

mod op_comparators_large_test {
    use super::*;
    use crate::docop::testing::{Parameters, RandomProviderImpl, generate_random_op};
    use crate::docop::{AnnotationBoundaryMap, BootstrapDocument};

    #[test]
    fn test_doc_op() {
        let a1 = DocOpBuilder::new().characters("a").build().unwrap();
        let a2 = DocOpBuilder::new().characters("a").build().unwrap();
        let b1 = DocOpBuilder::new().delete_characters("a").build().unwrap();
        let b2 = DocOpBuilder::new().delete_characters("a").build().unwrap();

        for (x, y) in [(&a1, &a1), (&a1, &a2), (&a2, &a1), (&a2, &a2)] {
            assert!(syntactic_identity(x, y));
            assert_eq!(x, y);
        }
        for (x, y) in [(&b1, &b1), (&b1, &b2), (&b2, &b1), (&b2, &b2)] {
            assert!(syntactic_identity(x, y));
            assert_eq!(x, y);
        }
        for (x, y) in [(&a1, &b1), (&a1, &b2), (&a2, &b1), (&a2, &b2)] {
            assert!(!syntactic_identity(x, y));
            assert_ne!(x, y);
        }
    }

    /// Two documents with annotations "x", `joined` and "y" that differ in
    /// whether "x" and "y" or `joined` end first.
    fn docs_ending_in_different_order(joined: &str) -> (DocInitialization, DocInitialization) {
        let starts = || {
            AnnotationBoundaryMap::changes_only([
                ("x", None, Some("1")),
                (joined, None, Some("3")),
                ("y", None, Some("2")),
            ])
            .unwrap()
        };
        let ends_both = || AnnotationBoundaryMap::ends(["x", "y"]).unwrap();
        let ends_joined = || AnnotationBoundaryMap::ends([joined]).unwrap();
        let doc1 = DocOpBuilder::new()
            .annotation_boundary(starts())
            .characters("m")
            .annotation_boundary(ends_both())
            .characters("n")
            .annotation_boundary(ends_joined())
            .build_initialization()
            .unwrap();
        let doc2 = DocOpBuilder::new()
            .annotation_boundary(starts())
            .characters("m")
            .annotation_boundary(ends_joined())
            .characters("n")
            .annotation_boundary(ends_both())
            .build_initialization()
            .unwrap();
        (doc1, doc2)
    }

    fn check_distinct(doc1: &DocInitialization, doc2: &DocInitialization) {
        assert!(
            !syntactic_identity(doc1.as_op(), doc2.as_op()),
            "\ndoc1: {doc1}\ndoc2: {doc2}"
        );
        assert_ne!(doc1, doc2);
        assert!(
            !equal_documents(doc1, doc2),
            "\ndoc1: {:?}\ndoc2: {:?}",
            to_xml_string(doc1),
            to_xml_string(doc2)
        );
        // Not in the original: both documents do have an XML form, so the
        // inequality is not that of a failed conversion.
        assert!(to_xml_string(doc1).is_ok());
        assert!(to_xml_string(doc2).is_ok());
        assert!(equal_documents(doc1, doc1));
        assert!(equal_documents(doc2, doc2));
    }

    /// Ending the single annotation "x y" is distinct from ending the two
    /// annotations "x" and "y".
    #[test]
    fn test_equal_handles_spaces_in_annotation_keys() {
        let (doc1, doc2) = docs_ending_in_different_order("x y");
        check_distinct(&doc1, &doc2);
    }

    /// Annotation keys may contain double quotes without ambiguity.
    #[test]
    fn test_equal_handles_quotes_in_annotation_keys() {
        let (doc1, doc2) = docs_ending_in_different_order("x\" \"y");
        check_distinct(&doc1, &doc2);
    }

    #[test]
    fn test_random_doc_ops() {
        let p = Parameters::default();
        for i in 0..200 {
            let mut doc = BootstrapDocument::new();
            for j in 0..20 {
                let mut ra = RandomProviderImpl::of_seed(i * 20 + j);
                let mut rb = RandomProviderImpl::of_seed(i * 20 + j + 1);
                let a = generate_random_op(&mut ra, &p, &doc);
                let b = generate_random_op(&mut rb, &p, &doc);
                doc.consume(&a).unwrap();
                assert!(syntactic_identity(&a, &a));
                // Not guaranteed by the generator, but true for these seeds.
                assert!(!syntactic_identity(&a, &b), "seeds for i={i} j={j}");
            }
        }
    }
}

mod wavelet_data_util_test {
    use super::*;

    fn creator() -> ParticipantId {
        participant("creator@example.com")
    }

    fn joe() -> ParticipantId {
        participant("joe@example.com")
    }

    fn name() -> WaveletName {
        WaveletName::of(
            WaveId::of("example.com", "w+wave").unwrap(),
            WaveletId::of("example.com", "wavelet").unwrap(),
        )
    }

    fn op_context(timestamp: i64, version: HashedVersion) -> WaveletOperationContext {
        WaveletOperationContext::with_hashed_version(creator(), timestamp, 1, version)
    }

    fn add_participant(user: ParticipantId, time: i64, version: HashedVersion) -> WaveletOperation {
        WaveletOperation::add_participant(op_context(time, version), user)
    }

    fn add_blip(id: &str, time: i64, version: HashedVersion) -> WaveletOperation {
        WaveletOperation::blip_content(op_context(time, version), id, DocOp::default())
    }

    /// A delta with the author, version and timestamp of its last op.
    fn delta(ops: Vec<WaveletOperation>) -> TransformedWaveletDelta {
        let context = ops.last().unwrap().context().clone();
        TransformedWaveletDelta::new(
            context.creator().clone(),
            context.hashed_version().unwrap().clone(),
            context.timestamp(),
            ops,
        )
        .unwrap()
    }

    #[test]
    fn test_build_wavelet_from_one_delta() {
        let deltas = [delta(vec![add_participant(
            creator(),
            1093,
            HashedVersion::unsigned(1),
        )])];
        let wavelet = build_wavelet_from_deltas(&name(), &deltas).unwrap();
        assert_eq!(name(), wavelet.wavelet_name());
        assert_eq!(&creator(), wavelet.creator());
        assert_eq!(1093, wavelet.creation_time());
        assert_eq!(1093, wavelet.last_modified_time());
        assert_eq!(&HashedVersion::unsigned(1), wavelet.hashed_version());
        assert_eq!(0, wavelet.document_ids().count());
        assert_eq!(wavelet.participants(), [creator()]);
    }

    #[test]
    fn test_build_wavelet_from_three_deltas() {
        let deltas = [
            delta(vec![add_participant(
                creator(),
                1093,
                HashedVersion::unsigned(1),
            )]),
            delta(vec![add_participant(
                joe(),
                1492,
                HashedVersion::unsigned(2),
            )]),
            delta(vec![add_blip("blipid", 2010, HashedVersion::unsigned(3))]),
        ];
        let wavelet = build_wavelet_from_deltas(&name(), &deltas).unwrap();
        assert_eq!(name(), wavelet.wavelet_name());
        assert_eq!(&creator(), wavelet.creator());
        assert_eq!(1093, wavelet.creation_time());
        assert_eq!(2010, wavelet.last_modified_time());
        assert_eq!(&HashedVersion::unsigned(3), wavelet.hashed_version());
        assert_eq!(vec!["blipid"], wavelet.document_ids().collect::<Vec<_>>());
        assert_eq!(wavelet.participants(), [creator(), joe()]);
    }
}

/// Tests of `transform_submitted_delta` and `transform_ops`, after
/// `WaveletContainerImpl.maybeTransformSubmittedDelta`, `transformSubmittedDelta`
/// and `transformOps`. The original has no unit tests of its own for these.
mod submitted_delta_transform_test {
    use super::*;

    fn alice() -> ParticipantId {
        participant("alice@example.com")
    }

    fn bob() -> ParticipantId {
        participant("bob@example.com")
    }

    fn carol() -> ParticipantId {
        participant("carol@example.com")
    }

    /// A version with a hash that depends on the version number.
    fn version(n: u64) -> HashedVersion {
        HashedVersion::of(n, [n as u8; 4])
    }

    fn context(author: &ParticipantId) -> WaveletOperationContext {
        WaveletOperationContext::new(author.clone(), NO_TIMESTAMP, 1)
    }

    /// A server delta by `author` resulting in `resulting`.
    fn server_delta(
        author: &ParticipantId,
        resulting: &HashedVersion,
        ops: &[WaveletOperation],
    ) -> TransformedWaveletDelta {
        TransformedWaveletDelta::clone_operations(author.clone(), resulting.clone(), 1234, ops)
            .unwrap()
    }

    #[test]
    fn test_submit_at_head_returns_delta_unchanged() {
        let delta = WaveletDelta::new(alice(), version(2), vec![insert(&alice(), 2, "A", 0, None)]);
        // The history is not looked at.
        let unrelated = server_delta(&bob(), &version(1), &[insert(&bob(), 1, "1", 1, None)]);
        for history in [vec![], vec![unrelated]] {
            let result = transform_submitted_delta(&delta, &version(2), &history).unwrap();
            assert_eq!(delta, result);
            assert_eq!(delta[0].context(), result[0].context());
        }
    }

    #[test]
    fn test_same_version_number_with_different_hash_is_invalid_hash() {
        let target = HashedVersion::of(2, [9u8, 9, 9, 9]);
        let delta = WaveletDelta::new(
            alice(),
            target.clone(),
            vec![insert(&alice(), 2, "A", 0, None)],
        );
        let history = [server_delta(
            &bob(),
            &version(2),
            &[insert(&bob(), 1, "1", 1, None)],
        )];
        for history in [&history[..], &[]] {
            assert_eq!(
                transform_submitted_delta(&delta, &version(2), history),
                Err(SubmitTransformError::InvalidHash {
                    expected: version(2),
                    target: target.clone(),
                })
            );
        }
    }

    #[test]
    fn test_transform_over_one_server_delta() {
        // Alice appends to a document of two items; Bob has inserted before.
        let delta = WaveletDelta::new(alice(), version(0), vec![insert(&alice(), 2, "A", 0, None)]);
        let history = [server_delta(
            &bob(),
            &version(1),
            &[insert(&bob(), 1, "1", 1, None)],
        )];
        let result = transform_submitted_delta(&delta, &version(1), &history).unwrap();
        assert_eq!(&alice(), result.author());
        assert_eq!(&version(1), result.target_version());
        assert_eq!(1, result.len());
        check_insert(&result[0], 3, "A", 0);
        // The client op keeps its context.
        assert_eq!(delta[0].context(), result[0].context());
    }

    #[test]
    fn test_transform_over_two_server_deltas() {
        let delta = WaveletDelta::new(
            alice(),
            version(0),
            vec![
                insert(&alice(), 2, "A", 0, None),
                WaveletOperation::add_participant(context(&alice()), carol()),
            ],
        );
        let history = [
            server_delta(&bob(), &version(1), &[insert(&bob(), 1, "1", 1, None)]),
            server_delta(
                &bob(),
                &version(3),
                &[
                    insert(&bob(), 1, "2", 2, None),
                    WaveletOperation::no_op(context(&bob())),
                ],
            ),
        ];
        let result = transform_submitted_delta(&delta, &version(3), &history).unwrap();
        assert_eq!(&alice(), result.author());
        assert_eq!(&version(3), result.target_version());
        assert_eq!(2, result.len());
        check_insert(&result[0], 4, "A", 0);
        assert_eq!(delta[1], result[1]);
    }

    #[test]
    fn test_duplicate_of_server_delta_returns_old_target_version() {
        let ops = vec![
            insert(&alice(), 2, "A", 0, None),
            WaveletOperation::add_participant(context(&alice()), carol()),
        ];
        let delta = WaveletDelta::new(alice(), version(1), ops.clone());
        let history = [
            // The delta as the server applied it the first time.
            server_delta(&alice(), &version(3), &ops),
            server_delta(&bob(), &version(4), &[insert(&bob(), 1, "1", 2, None)]),
        ];
        let result = transform_submitted_delta(&delta, &version(4), &history).unwrap();
        assert_eq!(delta, result);
        assert_eq!(&version(1), result.target_version());
    }

    #[test]
    fn test_duplicate_after_other_server_delta_returns_version_before_duplicate() {
        // Alice's delta was transformed over Bob's before it was applied.
        let delta = WaveletDelta::new(alice(), version(0), vec![insert(&alice(), 2, "A", 0, None)]);
        let history = [
            server_delta(&bob(), &version(1), &[insert(&bob(), 1, "1", 1, None)]),
            server_delta(&alice(), &version(2), &[insert(&alice(), 3, "A", 0, None)]),
            server_delta(&bob(), &version(3), &[insert(&bob(), 1, "2", 3, None)]),
        ];
        let result = transform_submitted_delta(&delta, &version(3), &history).unwrap();
        assert_eq!(&version(1), result.target_version());
        assert_eq!(1, result.len());
        check_insert(&result[0], 3, "A", 0);
    }

    #[test]
    fn test_same_ops_by_other_author_are_not_a_duplicate() {
        let add_carol =
            |author: &ParticipantId| WaveletOperation::add_participant(context(author), carol());
        let delta = WaveletDelta::new(alice(), version(0), vec![add_carol(&alice())]);
        let history = [server_delta(&bob(), &version(1), &[add_carol(&bob())])];
        let result = transform_submitted_delta(&delta, &version(1), &history).unwrap();
        // Transformed to the current version, where the addition is a no-op.
        assert_eq!(&version(1), result.target_version());
        assert_eq!(result.ops(), [WaveletOperation::no_op(context(&alice()))]);
        assert_eq!(&alice(), result[0].context().creator());
    }

    #[test]
    fn test_delta_without_ops_returns_old_target_version() {
        // A delta that has transformed to nothing is returned at the
        // version where that was noticed, not at the current version.
        let delta = WaveletDelta::new(alice(), version(0), vec![]);
        let history = [
            server_delta(&bob(), &version(1), &[insert(&bob(), 1, "1", 1, None)]),
            server_delta(&bob(), &version(2), &[insert(&bob(), 1, "2", 2, None)]),
        ];
        let result = transform_submitted_delta(&delta, &version(2), &history).unwrap();
        assert_eq!(&alice(), result.author());
        assert_eq!(&version(0), result.target_version());
        assert!(result.is_empty());
    }

    #[test]
    fn test_removed_author_fails() {
        let delta = WaveletDelta::new(alice(), version(0), vec![insert(&alice(), 2, "A", 0, None)]);
        let history = [
            server_delta(&bob(), &version(1), &[insert(&bob(), 1, "1", 1, None)]),
            server_delta(
                &bob(),
                &version(2),
                &[WaveletOperation::remove_participant(
                    context(&bob()),
                    alice(),
                )],
            ),
        ];
        assert_eq!(
            transform_submitted_delta(&delta, &version(2), &history),
            Err(SubmitTransformError::Transform(
                WaveTransformError::RemovedAuthor("alice@example.com".to_owned())
            ))
        );
    }

    #[test]
    fn test_empty_history_fails() {
        let delta = WaveletDelta::new(alice(), version(0), vec![insert(&alice(), 2, "A", 0, None)]);
        assert_eq!(
            transform_submitted_delta(&delta, &version(1), &[]),
            Err(SubmitTransformError::History {
                target: version(0),
                current: version(1),
            })
        );
    }

    #[test]
    fn test_history_not_ending_at_current_version_fails() {
        let delta = WaveletDelta::new(alice(), version(0), vec![insert(&alice(), 2, "A", 0, None)]);
        let history = [server_delta(
            &bob(),
            &version(1),
            &[insert(&bob(), 1, "1", 1, None)],
        )];
        // The history stops short of the current version.
        assert_eq!(
            transform_submitted_delta(&delta, &version(2), &history),
            Err(SubmitTransformError::History {
                target: version(1),
                current: version(2),
            })
        );
        // The history ends at the current version number with another hash.
        let current = HashedVersion::of(1, [9u8, 9, 9, 9]);
        assert_eq!(
            transform_submitted_delta(&delta, &current, &history),
            Err(SubmitTransformError::History {
                target: version(1),
                current,
            })
        );
    }

    #[test]
    fn test_transform_ops_without_server_ops_is_identity() {
        let client_ops = vec![
            insert(&alice(), 2, "A", 0, None),
            WaveletOperation::add_participant(context(&alice()), carol()),
        ];
        assert_eq!(transform_ops(&client_ops, &[]), Ok(client_ops.clone()));
        assert_eq!(transform_ops(&[], &client_ops), Ok(vec![]));
    }

    #[test]
    fn test_transform_ops_transforms_each_client_op_over_all_server_ops() {
        let client_ops = vec![
            insert(&alice(), 2, "A", 0, None),
            WaveletOperation::add_participant(context(&alice()), carol()),
            WaveletOperation::remove_participant(context(&alice()), bob()),
        ];
        let server_ops = vec![
            insert(&bob(), 1, "1", 1, None),
            WaveletOperation::add_participant(context(&bob()), carol()),
            insert(&bob(), 1, "2", 2, None),
        ];
        let transformed = transform_ops(&client_ops, &server_ops).unwrap();
        assert_eq!(3, transformed.len());
        check_insert(&transformed[0], 4, "A", 0);
        // Carol was added concurrently.
        assert_eq!(transformed[1], WaveletOperation::no_op(context(&alice())));
        assert_eq!(transformed[2], client_ops[2]);
        for (client_op, transformed_op) in client_ops.iter().zip(&transformed) {
            assert_eq!(client_op.context(), transformed_op.context());
        }
    }

    #[test]
    fn test_transform_ops_fails_on_concurrent_add_and_remove() {
        let client_ops = vec![
            insert(&alice(), 2, "A", 0, None),
            WaveletOperation::remove_participant(context(&alice()), carol()),
        ];
        let server_ops = vec![WaveletOperation::add_participant(context(&bob()), carol())];
        assert_eq!(
            transform_ops(&client_ops, &server_ops),
            Err(WaveTransformError::Participant(
                "carol@example.com".to_owned()
            ))
        );
    }
}
