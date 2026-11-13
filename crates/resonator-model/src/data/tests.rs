//! Ports `WaveletDataTestBase` and `BlipDataTestBase` (as instantiated by
//! `WaveletDataWithWaveletDataImplTest` and `BlipDataWithBlipDataImplTest`)
//! of Apache Wave. Listener assertions and the disabled `testSetTitle` are
//! not ported.

use super::*;
use crate::docop::{Attributes, DocOpBuilder, to_xml_string};

fn pid(address: &str) -> ParticipantId {
    ParticipantId::of(address).unwrap()
}

/// `WaveletDataFactory.create`: an empty wavelet at version zero and time
/// zero created by `fake@example.com`.
fn create_wavelet_data() -> WaveletData {
    WaveletData::new(
        WaveletId::of("example.com", "conv+root").unwrap(),
        pid("fake@example.com"),
        0,
        0,
        HashedVersion::unsigned(0),
        0,
        WaveId::of("example.com", "w+1").unwrap(),
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

mod wavelet_data_test_base {
    use super::*;

    #[test]
    fn new_wave_has_creator() {
        let target = create_wavelet_data();
        assert_eq!(target.creator(), &pid("fake@example.com"));
    }

    #[test]
    fn new_wave_has_no_participants() {
        // The creator is not automatically a participant.
        let target = create_wavelet_data();
        assert!(target.participants().is_empty());
    }

    #[test]
    fn new_wave_has_no_documents() {
        let target = create_wavelet_data();
        assert_eq!(target.document_ids().count(), 0);
        assert_eq!(target.documents().count(), 0);
    }

    #[test]
    fn create_blip() {
        let mut target = create_wavelet_data();
        let creator = target.creator().clone();
        target
            .create_document(
                "root",
                creator.clone(),
                [creator.clone()],
                &create_content("test"),
                42,
                42,
            )
            .unwrap();

        // The root blip is retrievable by id.
        let blip = target.document("root").unwrap();
        assert_eq!(target.document_ids().collect::<Vec<_>>(), ["root"]);

        assert_eq!(blip.id(), "root");
        assert_eq!(blip.author(), target.creator());
        assert_eq!(
            to_xml_string(&blip.content().as_operation()).unwrap(),
            "<body><line/>test</body>"
        );
        assert_eq!(blip.last_modified_time(), 42);
    }

    #[test]
    fn can_not_create_multiple_blips_with_same_id() {
        let mut target = create_wavelet_data();
        let creator = target.creator().clone();
        target
            .create_document(
                "DOCUMENT_NAME",
                creator.clone(),
                [creator.clone()],
                &create_content("test"),
                42,
                42,
            )
            .unwrap();
        let result = target
            .create_document(
                "DOCUMENT_NAME",
                creator.clone(),
                [creator],
                &create_content("test"),
                42,
                42,
            )
            .map(|_| ());
        assert_eq!(
            result,
            Err(DataError::DuplicateDocument("DOCUMENT_NAME".to_owned()))
        );
        assert_eq!(target.documents().count(), 1);
    }

    #[test]
    fn get_non_existent_blip_fails() {
        let target = create_wavelet_data();
        assert!(target.document("not a real blip id").is_none());
    }

    #[test]
    fn add_participant() {
        let mut target = create_wavelet_data();
        assert!(target.add_participant(pid("fred@gwave.com")));
        assert!(target.add_participant(pid("jane@gwave.com")));
    }

    #[test]
    fn add_participant_at_position() {
        let mut target = create_wavelet_data();
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        assert_eq!(target.add_participant_at(fred.clone(), 0), Ok(true));
        assert_eq!(target.participants(), std::slice::from_ref(&fred));
        assert_eq!(target.add_participant_at(fred.clone(), 0), Ok(false));
        assert_eq!(target.add_participant_at(jane.clone(), 0), Ok(true));
        assert_eq!(target.participants(), [jane.clone(), fred]);
        assert_eq!(target.add_participant_at(jane, 0), Ok(false));
    }

    #[test]
    fn add_participant_at_position_past_end() {
        let mut target = create_wavelet_data();
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        assert_eq!(
            target.add_participant_at(fred.clone(), 1),
            Err(DataError::PositionOutOfBounds {
                position: 1,
                size: 0
            })
        );
        assert_eq!(target.add_participant_at(fred.clone(), 0), Ok(true));
        assert_eq!(
            target.add_participant_at(jane, 2),
            Err(DataError::PositionOutOfBounds {
                position: 2,
                size: 1
            })
        );
        assert_eq!(target.participants(), [fred]);
    }

    #[test]
    fn remove_participant() {
        let mut target = create_wavelet_data();
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        target.add_participant(fred.clone());
        target.add_participant(jane.clone());
        assert!(target.remove_participant(&fred));
        assert_eq!(target.participants(), [jane]);
    }

    #[test]
    fn add_duplicate_participant_returns_false() {
        let mut target = create_wavelet_data();
        let fred = pid("fred@gwave.com");
        target.add_participant(fred.clone());
        assert!(!target.add_participant(fred.clone()));
        assert_eq!(target.participants(), [fred]);
    }

    #[test]
    fn remove_invalid_participant_returns_false() {
        let mut target = create_wavelet_data();
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        target.add_participant(fred.clone());
        assert!(!target.remove_participant(&jane));
        assert_eq!(target.participants(), [fred]);
    }

    #[test]
    fn add_participants_retrieved_in_order() {
        let mut target = create_wavelet_data();
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        target.add_participant(fred.clone());
        target.add_participant(jane.clone());
        let result: Vec<ParticipantId> = target.participants().to_vec();
        assert_eq!(result, [fred, jane]);
    }

    #[test]
    fn removal_of_multiply_added_participant() {
        let mut target = create_wavelet_data();
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        target.add_participant(fred.clone());
        target.add_participant(fred.clone());
        target.add_participant(jane.clone());
        assert!(target.remove_participant(&fred));
        assert_eq!(target.participants(), [jane]);
    }

    #[test]
    fn set_version() {
        let mut target = create_wavelet_data();
        target.set_version(1234);
        assert_eq!(target.version(), 1234);
    }

    #[test]
    fn set_last_modified_time() {
        let mut target = create_wavelet_data();
        target.set_last_modified_time(4321);
        assert_eq!(target.last_modified_time(), 4321);
    }
}

mod blip_data_test_base {
    use super::*;

    /// `BlipDataWithBlipDataImplTest.createBlipData`: an empty document
    /// `root` without contributors in a new wavelet.
    fn create_blip_data() -> WaveletData {
        let mut wavelet = create_wavelet_data();
        let creator = wavelet.creator().clone();
        let contributors = wavelet.participants().to_vec();
        wavelet
            .create_document(
                "root",
                creator,
                contributors,
                &DocOpBuilder::new().build_initialization().unwrap(),
                42,
                42,
            )
            .unwrap();
        wavelet
    }

    fn target(wavelet: &mut WaveletData) -> &mut BlipData {
        wavelet.document_mut("root").unwrap()
    }

    #[test]
    fn get_wave() {
        // A blip has no back reference; it is reachable from its wavelet.
        let mut wavelet = create_blip_data();
        let blip = target(&mut wavelet).clone();
        assert_eq!(wavelet.document(blip.id()), Some(&blip));
    }

    #[test]
    fn blip_has_id() {
        let mut wavelet = create_blip_data();
        assert_eq!(target(&mut wavelet).id(), "root");
    }

    #[test]
    fn blip_author() {
        let mut wavelet = create_blip_data();
        assert_eq!(target(&mut wavelet).author(), &pid("fake@example.com"));
    }

    #[test]
    fn new_blip_has_no_contributors() {
        let mut wavelet = create_blip_data();
        assert_eq!(target(&mut wavelet).contributors().len(), 0);
    }

    #[test]
    fn add_contributors_retreived_in_order() {
        let mut wavelet = create_blip_data();
        let target = target(&mut wavelet);
        let contributors = vec![pid("fred@gwave.com"), pid("jane@gwave.com")];
        for p in &contributors {
            target.add_contributor(p.clone());
        }
        let result: Vec<ParticipantId> = target.contributors().to_vec();
        assert_eq!(result, contributors);
    }

    #[test]
    fn remove_contributors() {
        let mut wavelet = create_blip_data();
        let target = target(&mut wavelet);
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        target.add_contributor(fred.clone());
        // Added twice, should be completely removed.
        target.add_contributor(fred.clone());
        target.add_contributor(jane.clone());

        target.remove_contributor(&fred);
        assert_eq!(target.contributors(), std::slice::from_ref(&jane));
        target.remove_contributor(&jane);
        assert!(target.contributors().is_empty());
    }

    #[test]
    fn remove_invalid_contributor_ignored() {
        let mut wavelet = create_blip_data();
        let target = target(&mut wavelet);
        let fred = pid("fred@gwave.com");
        let jane = pid("jane@gwave.com");
        target.add_contributor(fred.clone());
        target.remove_contributor(&jane);
        assert_eq!(target.contributors(), [fred]);
    }

    #[test]
    fn set_last_modified_time() {
        let mut wavelet = create_blip_data();
        let target = target(&mut wavelet);
        let lmt = target.last_modified_time() + 42;
        target.set_last_modified_time(lmt);
        assert_eq!(target.last_modified_time(), lmt);
    }

    #[test]
    fn last_modified_version() {
        let mut wavelet = create_blip_data();
        let target = target(&mut wavelet);
        let lmv = target.last_modified_version() + 23;
        target.set_last_modified_version(lmv);
        assert_eq!(target.last_modified_version(), lmv);
    }

    #[test]
    fn empty_blip_has_content() {
        let mut wavelet = create_blip_data();
        let target = target(&mut wavelet);
        assert!(target.content().as_operation().is_empty());
        assert_eq!(target.content(), &BootstrapDocument::new());
    }
}
