//! Operations not tied to a blip (`NotifyOperationService`,
//! `FetchWaveService`, `SearchService`, `FolderActionService`,
//! `FetchProfilesService`).

use std::collections::BTreeMap;

use resonator_model::conversation::Conversation;
use resonator_model::id::util::build_user_data_wavelet_id;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::supplement::{
    DefaultFollow, SupplementedWaveImpl, WaveletBasedSupplement, WaveletDataView,
};
use resonator_server::account::AccountData;
use resonator_server::waveserver::WaveletProvider;
use serde_json::json;

use super::context::{OperationContext, check_conversational};
use crate::api::{
    ApiWaveletId, Event, EventMessageBundle, EventProperties, FetchProfilesRequest,
    FetchProfilesResult, InvalidRequest, OperationRequest, ParamsProperty, RobotName,
};
use crate::convert::{add_all_blips, deserialise_wave_id, root_blip_id};

/// `robot.notify`: the robot says what its capabilities hash is; if it
/// differs from the one on record its capabilities are fetched again.
pub(super) async fn notify(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let capabilities_hash = operation.required_string(ParamsProperty::CapabilitiesHash)?;
    let robot_account_id = RobotName::from_address(participant.address())
        .map(|name| ParticipantId::new_unchecked(&name.to_email_address()))
        .unwrap_or_else(|| participant.clone());
    let accounts = context.executor.server.database().account_store();
    let account = match accounts.get_account(&robot_account_id).await {
        Ok(account) => account,
        Err(_) => {
            context.construct_error_response(operation, "Unable to retrieve account data");
            return Ok(());
        }
    };
    let Some(AccountData::Robot(mut robot)) = account else {
        return Err(InvalidRequest::new(format!(
            "Can't exectute robot.notify for unknown robot {robot_account_id}"
        )));
    };
    let unchanged = robot
        .capabilities
        .as_ref()
        .is_some_and(|c| c.capabilities_hash == capabilities_hash);
    if !unchanged {
        match context
            .executor
            .capabilities
            .fetch_capabilities(&robot)
            .await
        {
            Ok(capabilities) => robot.capabilities = Some(capabilities),
            Err(_) => {
                context.construct_error_response(operation, "Unable to retrieve new capabilities");
                return Ok(());
            }
        }
        if accounts
            .put_account(AccountData::Robot(robot))
            .await
            .is_err()
        {
            context.construct_error_response(operation, "Unable to update account data");
            return Ok(());
        }
    }
    context.construct_response(operation, BTreeMap::new());
    Ok(())
}

/// `robot.fetchWave`: a wavelet with all its blips and threads, or with
/// `returnWaveletIds` the ids of the wave's wavelets the participant may
/// access.
pub(super) async fn fetch_wave(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    if operation
        .boolean(ParamsProperty::ReturnWaveletIds)
        .unwrap_or(false)
    {
        // getVisibleWaveletIds
        let wave_id = operation.required_string(ParamsProperty::WaveId)?;
        let wave_id =
            deserialise_wave_id(&wave_id).ok_or_else(|| InvalidRequest::new("Invalid id"))?;
        let mut visible = Vec::new();
        let ids = context
            .executor
            .server
            .get_wavelet_ids(&wave_id)
            .await
            .unwrap_or_default();
        for wavelet_id in ids {
            let name = WaveletName::of(wave_id.clone(), wavelet_id.clone());
            if context.check_access(&name, participant).await {
                visible.push(ApiWaveletId {
                    domain: wavelet_id.domain().to_owned(),
                    id: wavelet_id.id().to_owned(),
                });
            }
        }
        let data = BTreeMap::from([(ParamsProperty::WaveletIds, json!(visible))]);
        context.construct_response(operation, data);
        return Ok(());
    }
    let name = context
        .open_operation_wavelet(operation, participant)
        .await?;
    let converter = context.converter();
    let event = {
        let wavelet = context.wavelet(&name, participant)?;
        check_conversational(wavelet.data())?;
        let conversation = Conversation::read(wavelet.data())
            .ok_or_else(|| InvalidRequest::new("the wavelet has no conversation manifest"))?;
        // mapWaveletToMessageBundle
        let mut bundle = EventMessageBundle::new(participant.address(), "");
        let wavelet_data = converter.to_wavelet_data(wavelet.data(), &conversation);
        add_all_blips(&mut bundle, &conversation, converter);
        Event::new(
            participant.address(),
            context.now(),
            Some(&root_blip_id(&conversation)),
            EventProperties::WaveletFetched {
                message: operation.string(ParamsProperty::Message),
                wavelet_data: Some(Box::new(wavelet_data)),
                blips: bundle.blips,
                threads: bundle.threads,
            },
        )
    };
    context.process_event(operation, &event);
    Ok(())
}

/// The number of results of a search that does not say
/// (`DEFAULT_NUMBER_SEARCH_RESULTS`).
const DEFAULT_NUMBER_SEARCH_RESULTS: i32 = 10;

/// `robot.search`.
pub(super) async fn search(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let query = operation.required_string(ParamsProperty::Query)?;
    let index = operation.integer(ParamsProperty::Index).unwrap_or(0);
    let num_results = operation
        .integer(ParamsProperty::NumResults)
        .unwrap_or(DEFAULT_NUMBER_SEARCH_RESULTS);
    let result = context
        .executor
        .search
        .search(participant, &query, index, num_results)
        .await?;
    let data = BTreeMap::from([(ParamsProperty::SearchResults, json!(result))]);
    context.construct_response(operation, data);
    Ok(())
}

/// `robot.folderAction`: marks a wave, or one blip of it, as read, or the
/// wave as unread, in the participant's user-data wavelet.
pub(super) async fn folder_action(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let modify_how = operation.required_string(ParamsProperty::ModifyHow)?;
    let blip_id = operation
        .string(ParamsProperty::BlipId)
        .filter(|id| !id.is_empty());
    // buildSupplement
    let (wave_id, wavelet_id) = context.wavelet_ids_of(operation)?;
    let name = context
        .open_wavelet(&wave_id, &wavelet_id, participant)
        .await?;
    check_conversational(context.wavelet(&name, participant)?.data())?;
    let user_data_id =
        build_user_data_wavelet_id(participant).map_err(|e| InvalidRequest::new(e.to_string()))?;
    let user_data_name = context
        .open_wavelet(&wave_id, &user_data_id, participant)
        .await?;
    let mut user_data = context.take_wavelet(&user_data_name, participant)?;
    let result = (|| {
        let conversation = context.wavelet(&name, participant)?.data();
        let view = WaveletDataView::new(participant.clone(), [conversation]);
        let invalid =
            |e: resonator_model::supplement::SupplementError| InvalidRequest::new(e.to_string());
        let state = WaveletBasedSupplement::create(&mut user_data).map_err(invalid)?;
        let mut supplement = SupplementedWaveImpl::new(state, view, DefaultFollow::Always);
        match modify_how.as_str() {
            "markAsRead" => match &blip_id {
                None => supplement.mark_as_read().map_err(invalid),
                Some(blip_id) => supplement
                    .mark_blip_as_read(&name.wavelet_id, blip_id)
                    .map_err(invalid),
            },
            "markAsUnread" => supplement.mark_as_unread().map_err(invalid),
            other => Err(InvalidRequest::new(format!(
                "Unsupported folder action: {other}"
            ))),
        }
    })();
    context.restore_wavelet(user_data);
    result?;
    context.construct_response(operation, BTreeMap::new());
    Ok(())
}

/// `robot.fetchProfiles`.
pub(super) async fn fetch_profiles(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
) -> Result<(), InvalidRequest> {
    let request: FetchProfilesRequest = operation.required(ParamsProperty::FetchProfilesRequest)?;
    let mut profiles = Vec::new();
    for address in request.participant_ids.unwrap_or_default() {
        profiles.push(context.executor.profiles.fetch_profile(&address).await);
    }
    let result = FetchProfilesResult {
        profiles: Some(profiles),
    };
    let data = BTreeMap::from([(ParamsProperty::FetchProfilesResult, json!(result))]);
    context.construct_response(operation, data);
    Ok(())
}
