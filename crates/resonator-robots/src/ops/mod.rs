//! Execution of Robots API operations against the wave server.
//!
//! Derived from `org.waveprotocol.box.server.robots` of Apache Wave
//! (`OperationContextImpl`, `OperationResults`, `RobotWaveletData`,
//! `AbstractOperationServiceRegistry` and the active, Data API and passive
//! registries, `operations.*`, `util.OperationUtil`,
//! `util.ConversationUtil`, `util.RobotsUtil`, and the request handling of
//! `dataapi.BaseApiServlet` and `passive.RobotOperationApplicator`),
//! licensed under the Apache License, Version 2.0.
//!
//! A batch of operations runs against one [`OperationContext`]: wavelets
//! are fetched from the server when an operation first needs them, the
//! operations edit local copies and record wavelet operations, things
//! created under temporary ids (`TBD_...`) can be referred to by later
//! operations of the batch, and at the end the recorded operations are
//! submitted to the wave server as deltas, one per wavelet and author.
//!
//! Authentication is the caller's business: the caller says who acts.
//!
//! Differences from the original, besides those the items document:
//!
//! * An exception that is not an `InvalidRequestException` (a null
//!   pointer, an illegal argument, an unsupported operation) aborts the
//!   whole request in the original with an HTTP 500. Here every failure is
//!   the error response of its operation.
//! * Operations that the original restricts to the root conversation
//!   wavelet by accident (they fail with a null pointer elsewhere) work on
//!   any conversational wavelet.
//! * `wavelet.setTitle` gets an empty successful response; the original
//!   forgets to respond, which puts a `null` into the response list.
//! * Two operations of a batch with the same id each get their own
//!   response; the original fails.
//! * Importing deltas and attachments is off unless enabled, see
//!   [`OperationExecutor::with_import_enabled`].

mod blip;
mod context;
mod document_modify;
mod porting;
mod robot;
mod wavelet;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use resonator_model::data::WaveletData;
use resonator_model::id::{IdGeneratorImpl, ParticipantId, TokenGenerator, WaveletName};
use resonator_model::operation::WaveletDelta;
use resonator_server::account::{RobotAccountData, RobotCapabilities};
use resonator_server::clock::{Clock, SystemClock};
use resonator_server::waveserver::{SubmitResult, WaveServer};

pub use context::{OpenConversation, OperationContext, RobotWavelet};
pub use porting::coerce_blip_text;

use crate::api::{
    InvalidRequest, JsonRpcResponse, OperationRequest, OperationType, ParamsProperty,
    ParticipantProfile, RawAttachmentData, RobotName, SearchResult, protocol_version_of,
};
use crate::convert::EventDataConverter;

/// Which operations a caller may use (`OperationServiceRegistry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Registry {
    /// The active robot API (`ActiveApiOperationServiceRegistry`).
    Active,
    /// The Data API, for programs acting as a user
    /// (`DataApiOperationServiceRegistry`): like the active API, except
    /// that `robot.notify` does nothing.
    DataApi,
    /// The reply of a passive robot to events
    /// (`passive.OperationServiceRegistryImpl`): no fetching, searching,
    /// profiles, export or import; new waves are created with
    /// `wavelet.create` instead of `robot.createWavelet`.
    Passive,
    /// Agents running inside the server. Not in the original: the
    /// operations of the Data API plus `wavelet.create`, so that an agent
    /// can use the same operations whether it replies to events or acts
    /// on its own.
    Agent,
}

/// What executes an operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    Notify,
    DoNothing,
    Participant,
    Blip,
    CreateWavelet,
    FetchWave,
    DocumentModify,
    Search,
    SetTitle,
    FolderAction,
    FetchProfiles,
    ExportSnapshot,
    ExportDeltas,
    ExportAttachment,
    ImportDeltas,
    ImportAttachment,
}

impl Registry {
    /// The operation types the registry has a service for.
    pub fn operations(self) -> Vec<OperationType> {
        OperationType::ALL
            .iter()
            .copied()
            .filter(|&operation| self.service_for(operation).is_some())
            .collect()
    }

    pub fn supports(self, operation: OperationType) -> bool {
        self.service_for(operation).is_some()
    }

    fn service_for(self, operation: OperationType) -> Option<Service> {
        use OperationType as T;
        let passive = self == Registry::Passive;
        Some(match operation {
            T::RobotNotify | T::RobotNotifyCapabilitiesHash => match self {
                Registry::Active | Registry::Passive => Service::Notify,
                Registry::DataApi | Registry::Agent => Service::DoNothing,
            },
            T::WaveletAddParticipantNewsyntax | T::WaveletRemoveParticipantNewsyntax => {
                Service::Participant
            }
            T::WaveletAppendBlip
            | T::BlipContinueThread
            | T::BlipCreateChild
            | T::BlipDelete
            | T::DocumentAppendInlineBlip
            | T::DocumentAppendMarkup
            | T::DocumentInsertInlineBlip
            | T::DocumentInsertInlineBlipAfterElement => Service::Blip,
            T::DocumentModify => Service::DocumentModify,
            T::WaveletSetTitle => Service::SetTitle,
            T::RobotFolderAction => Service::FolderAction,
            T::WaveletCreate if passive || self == Registry::Agent => Service::CreateWavelet,
            T::RobotCreateWavelet if !passive => Service::CreateWavelet,
            T::RobotFetchWave if !passive => Service::FetchWave,
            T::RobotSearch if !passive => Service::Search,
            T::RobotFetchProfiles if !passive => Service::FetchProfiles,
            T::RobotExportSnapshot if !passive => Service::ExportSnapshot,
            T::RobotExportDeltas if !passive => Service::ExportDeltas,
            T::RobotExportAttachment if !passive => Service::ExportAttachment,
            T::RobotImportDeltas if !passive => Service::ImportDeltas,
            T::RobotImportAttachment if !passive => Service::ImportAttachment,
            _ => return None,
        })
    }
}

/// Searches the waves of a participant for `robot.search`
/// (`SearchProvider`).
#[async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(
        &self,
        participant: &ParticipantId,
        query: &str,
        start_at: i32,
        num_results: i32,
    ) -> Result<SearchResult, InvalidRequest>;
}

/// The search provider used until one is given: every search fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSearch;

#[async_trait]
impl SearchProvider for NoSearch {
    async fn search(
        &self,
        _: &ParticipantId,
        _: &str,
        _: i32,
        _: i32,
    ) -> Result<SearchResult, InvalidRequest> {
        Err(InvalidRequest::new("Search is not available"))
    }
}

/// Supplies profiles for `robot.fetchProfiles`
/// (`FetchProfilesService.ProfilesFetcher`).
#[async_trait]
pub trait ProfilesFetcher: Send + Sync {
    async fn fetch_profile(&self, address: &str) -> ParticipantProfile;
}

/// The image of an unknown participant (`ProfilesFetcher.UNKNOWN_IMAGE`).
pub const UNKNOWN_IMAGE: &str = "/static/images/unknown.jpg";

/// A display name made from an address: the part before the `@`, split at
/// dots and underscores, each piece capitalized; the address itself if
/// that part is empty (`SIMPLE_PROFILES_FETCHER.buildNames`).
pub fn name_from_address(address: &str) -> String {
    let name = address.split('@').next().unwrap_or("");
    if name.is_empty() {
        return address.to_owned();
    }
    name.split(['.', '_'])
        .filter(|fragment| !fragment.is_empty())
        .map(|fragment| {
            let mut chars = fragment.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Profiles with a name made from the address and no image of their own
/// (`ProfilesFetcher.SIMPLE_PROFILES_FETCHER`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleProfilesFetcher;

#[async_trait]
impl ProfilesFetcher for SimpleProfilesFetcher {
    async fn fetch_profile(&self, address: &str) -> ParticipantProfile {
        ParticipantProfile::new(address, &name_from_address(address), UNKNOWN_IMAGE, "")
    }
}

/// Profiles with a name made from the address and an avatar of initials
/// served by the server (`InitialsProfilesFetcher`). The default.
#[derive(Debug, Clone, Copy, Default)]
pub struct InitialsProfilesFetcher;

impl InitialsProfilesFetcher {
    pub fn image_url(address: &str) -> String {
        format!("/iniavatars/100x100/{address}")
    }
}

#[async_trait]
impl ProfilesFetcher for InitialsProfilesFetcher {
    async fn fetch_profile(&self, address: &str) -> ParticipantProfile {
        ParticipantProfile::new(
            address,
            &name_from_address(address),
            &Self::image_url(address),
            "",
        )
    }
}

/// Attachment storage for `robot.exportAttachment` and
/// `robot.importAttachment` (`AttachmentService`). Errors are the
/// operation's error message.
#[async_trait]
pub trait AttachmentService: Send + Sync {
    /// The attachment with an id: file name, creator and content.
    async fn export_attachment(&self, attachment_id: &str) -> Result<RawAttachmentData, String>;

    /// Stores an attachment of a wavelet under an id.
    async fn import_attachment(
        &self,
        attachment_id: &str,
        wavelet_name: &WaveletName,
        file_name: &str,
        creator: &ParticipantId,
        data: &[u8],
    ) -> Result<(), String>;
}

/// The attachment service used until one is given: there are no
/// attachments and none can be stored.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAttachments;

#[async_trait]
impl AttachmentService for NoAttachments {
    async fn export_attachment(&self, _: &str) -> Result<RawAttachmentData, String> {
        Err("Attachment storage is not available".to_owned())
    }

    async fn import_attachment(
        &self,
        _: &str,
        _: &WaveletName,
        _: &str,
        _: &ParticipantId,
        _: &[u8],
    ) -> Result<(), String> {
        Err("Attachment storage is not available".to_owned())
    }
}

/// Fetches the capabilities of an external robot for `robot.notify`
/// (`RobotConnector.fetchCapabilities`).
#[async_trait]
pub trait CapabilitiesFetcher: Send + Sync {
    async fn fetch_capabilities(
        &self,
        account: &RobotAccountData,
    ) -> Result<RobotCapabilities, String>;
}

/// The capabilities fetcher used until one is given: fetching fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCapabilitiesFetcher;

#[async_trait]
impl CapabilitiesFetcher for NoCapabilitiesFetcher {
    async fn fetch_capabilities(&self, _: &RobotAccountData) -> Result<RobotCapabilities, String> {
        Err("no capabilities fetcher".to_owned())
    }
}

/// A wavelet as a passive robot saw it when it was sent events; its reply
/// is applied to that state (`RobotOperationApplicator.applyOperations`).
#[derive(Debug, Clone)]
pub struct BoundWavelet {
    pub snapshot: WaveletData,
}

/// What became of one delta a batch produced.
#[derive(Debug)]
pub struct Submission {
    pub wavelet_name: WaveletName,
    pub author: ParticipantId,
    /// The indices, in the batch, of the operations that used this wavelet
    /// as this author.
    pub operation_indices: Vec<usize>,
    /// The server's verdict; the error is its message.
    pub result: Result<SubmitResult, String>,
}

/// The outcome of a batch: the responses, in the order of the requests,
/// and what became of the deltas.
#[derive(Debug)]
pub struct BatchResult {
    pub responses: Vec<JsonRpcResponse>,
    pub submissions: Vec<Submission>,
}

impl BatchResult {
    /// Whether every delta was applied.
    pub fn all_submitted(&self) -> bool {
        self.submissions.iter().all(|s| s.result.is_ok())
    }

    /// The responses, with those of operations whose changes the server
    /// rejected turned into errors.
    ///
    /// The original answers before it knows whether the deltas apply, and
    /// only logs failures; [`BatchResult::responses`] is that answer. Use
    /// this where the caller should learn that its change was not made.
    pub fn responses_with_submit_errors(&self) -> Vec<JsonRpcResponse> {
        let mut responses = self.responses.clone();
        for submission in &self.submissions {
            let Err(message) = &submission.result else {
                continue;
            };
            for &index in &submission.operation_indices {
                if let Some(response) = responses.get_mut(index)
                    && !response.is_error()
                {
                    *response = JsonRpcResponse::error(
                        response.id.clone(),
                        format!(
                            "Failed to apply the changes to {}: {message}",
                            submission.wavelet_name
                        ),
                    );
                }
            }
        }
        responses
    }
}

/// A delta a batch produced and has not submitted.
#[derive(Debug, Clone)]
pub struct PendingDelta {
    pub wavelet_name: WaveletName,
    pub delta: WaveletDelta,
    /// The indices, in the batch, of the operations that used this wavelet
    /// as the delta's author.
    pub operation_indices: Vec<usize>,
}

/// The outcome of a batch that was executed but not submitted.
#[derive(Debug, Clone)]
pub struct AppliedBatch {
    pub responses: Vec<JsonRpcResponse>,
    /// One delta per wavelet and author, in order of first use.
    pub deltas: Vec<PendingDelta>,
}

/// Executes batches of operations against a wave server.
pub struct OperationExecutor {
    pub(crate) server: WaveServer,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) search: Arc<dyn SearchProvider>,
    pub(crate) profiles: Arc<dyn ProfilesFetcher>,
    pub(crate) attachments: Arc<dyn AttachmentService>,
    pub(crate) capabilities: Arc<dyn CapabilitiesFetcher>,
    pub(crate) import_enabled: bool,
    ids: Mutex<IdGeneratorImpl>,
}

impl std::fmt::Debug for OperationExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationExecutor")
            .field("server", &self.server)
            .field("import_enabled", &self.import_enabled)
            .finish()
    }
}

/// A random seed for ids: nine characters of web-safe base 64.
fn random_seed() -> String {
    let mut tokens = TokenGenerator::new(|bytes: &mut [u8]| {
        if getrandom::fill(bytes).is_err() {
            // No entropy source: fall back to the clock, which still gives
            // ids that differ between calls.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = (nanos >> (8 * (i % 16))) as u8;
            }
        }
    });
    tokens.generate_token(9).unwrap_or_default()
}

impl OperationExecutor {
    /// An executor on a wave server, with the system clock, random ids,
    /// initials profiles, and no search, attachments or capabilities
    /// fetching.
    pub fn new(server: WaveServer) -> Self {
        let ids = IdGeneratorImpl::new(server.domain().to_owned(), random_seed);
        OperationExecutor {
            server,
            clock: Arc::new(SystemClock),
            search: Arc::new(NoSearch),
            profiles: Arc::new(InitialsProfilesFetcher),
            attachments: Arc::new(NoAttachments),
            capabilities: Arc::new(NoCapabilitiesFetcher),
            import_enabled: false,
            ids: Mutex::new(ids),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_search(mut self, search: Arc<dyn SearchProvider>) -> Self {
        self.search = search;
        self
    }

    pub fn with_profiles(mut self, profiles: Arc<dyn ProfilesFetcher>) -> Self {
        self.profiles = profiles;
        self
    }

    pub fn with_attachments(mut self, attachments: Arc<dyn AttachmentService>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn with_capabilities_fetcher(mut self, fetcher: Arc<dyn CapabilitiesFetcher>) -> Self {
        self.capabilities = fetcher;
        self
    }

    /// Replaces the source of ids for new waves and blips, e.g. with a
    /// deterministic one for tests.
    pub fn with_id_generator(mut self, ids: IdGeneratorImpl) -> Self {
        self.ids = Mutex::new(ids);
        self
    }

    /// Allows `robot.importDeltas` and `robot.importAttachment`.
    ///
    /// Importing submits deltas under the authors recorded in them and
    /// does not check that the caller may access the wavelet, so it lets
    /// the caller write as anyone. The original offers it to every API
    /// user; here it has to be enabled, and should be only for trusted
    /// callers (an administrator's migration tool).
    pub fn with_import_enabled(mut self, enabled: bool) -> Self {
        self.import_enabled = enabled;
        self
    }

    pub fn server(&self) -> &WaveServer {
        &self.server
    }

    pub(crate) fn ids(&self) -> MutexGuard<'_, IdGeneratorImpl> {
        self.ids.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Executes a batch of operations as `actor` and submits the resulting
    /// deltas. Returns one response per request, in order.
    ///
    /// As in the original, a response says what the operation did to the
    /// batch's copy of the wavelet; whether the server then accepted the
    /// delta is not in it. See [`OperationExecutor::execute_detailed`].
    pub async fn execute(
        &self,
        actor: &ParticipantId,
        registry: Registry,
        requests: Vec<OperationRequest>,
    ) -> Vec<JsonRpcResponse> {
        self.execute_detailed(actor, registry, &requests, None)
            .await
            .responses
    }

    /// Executes a batch of operations as `actor`, optionally bound to the
    /// state of a wavelet as a robot saw it, and submits the resulting
    /// deltas (`BaseApiServlet.processOpsRequest`,
    /// `RobotOperationApplicator.applyOperations`).
    pub async fn execute_detailed(
        &self,
        actor: &ParticipantId,
        registry: Registry,
        requests: &[OperationRequest],
        bound: Option<BoundWavelet>,
    ) -> BatchResult {
        let applied = self.apply(actor, registry, requests, bound).await;
        let mut submissions = Vec::new();
        for pending in applied.deltas {
            let result = self
                .server
                .submit_delta(&pending.wavelet_name, &pending.delta)
                .await;
            if let Err(error) = &result {
                tracing::warn!(
                    wavelet = %pending.wavelet_name,
                    author = %pending.delta.author(),
                    "robot operation delta rejected: {error}"
                );
            }
            submissions.push(Submission {
                wavelet_name: pending.wavelet_name,
                author: pending.delta.author().clone(),
                operation_indices: pending.operation_indices,
                result: result.map_err(|e| e.to_string()),
            });
        }
        BatchResult {
            responses: applied.responses,
            submissions,
        }
    }

    /// Executes a batch of operations without submitting anything: the
    /// responses, and the deltas that [`OperationExecutor::execute`] would
    /// submit. Operations that act on the server directly (imports,
    /// `robot.notify`) still do.
    pub async fn apply(
        &self,
        actor: &ParticipantId,
        registry: Registry,
        requests: &[OperationRequest],
        bound: Option<BoundWavelet>,
    ) -> AppliedBatch {
        let converter = EventDataConverter::for_version(protocol_version_of(requests));
        let mut context = OperationContext::new(
            self,
            converter,
            requests.len(),
            bound.map(|bound| bound.snapshot),
        );
        for (index, operation) in requests.iter().enumerate() {
            context.set_current(index);
            if let Err(error) = self
                .execute_operation(operation, registry, &mut context, actor)
                .await
            {
                context.construct_error_response(operation, &error.message);
            }
            if !context.has_response() {
                context.construct_response(operation, BTreeMap::new());
            }
        }
        let (responses, opened, touches) = context.into_parts();
        let mut deltas = Vec::new();
        for mut wavelet in opened {
            let wavelet_name = wavelet.wavelet_name();
            for delta in wavelet.take_deltas() {
                let mut operation_indices: Vec<usize> = touches
                    .iter()
                    .filter(|t| t.wavelet_name == wavelet_name && &t.author == delta.author())
                    .map(|t| t.operation_index)
                    .collect();
                operation_indices.dedup();
                deltas.push(PendingDelta {
                    wavelet_name: wavelet_name.clone(),
                    delta,
                    operation_indices,
                });
            }
        }
        AppliedBatch {
            responses: responses
                .into_iter()
                .zip(requests)
                .map(|(response, request)| {
                    response.unwrap_or_else(|| {
                        JsonRpcResponse::result(request.id.clone(), BTreeMap::new())
                    })
                })
                .collect(),
            deltas,
        }
    }

    /// `OperationUtil.executeOperation`.
    async fn execute_operation(
        &self,
        operation: &OperationRequest,
        registry: Registry,
        context: &mut OperationContext<'_>,
        actor: &ParticipantId,
    ) -> Result<(), InvalidRequest> {
        let operation_type = operation.operation_type();
        let Some(service) = registry.service_for(operation_type) else {
            return Err(InvalidRequest::new(format!(
                "No OperationService found for {}",
                operation_type_name(operation_type)
            )));
        };
        let participant = compute_participant(operation, actor)?;
        match service {
            Service::Notify => robot::notify(operation, context, &participant).await,
            Service::DoNothing => {
                context.construct_response(operation, BTreeMap::new());
                Ok(())
            }
            Service::Participant => {
                wavelet::participant(operation, operation_type, context, &participant).await
            }
            Service::Blip => blip::execute(operation, operation_type, context, &participant).await,
            Service::CreateWavelet => wavelet::create(operation, context, &participant).await,
            Service::FetchWave => robot::fetch_wave(operation, context, &participant).await,
            Service::DocumentModify => {
                document_modify::execute(operation, context, &participant).await
            }
            Service::Search => robot::search(operation, context, &participant).await,
            Service::SetTitle => wavelet::set_title(operation, context, &participant).await,
            Service::FolderAction => robot::folder_action(operation, context, &participant).await,
            Service::FetchProfiles => robot::fetch_profiles(operation, context).await,
            Service::ExportSnapshot => {
                porting::export_snapshot(operation, context, &participant).await
            }
            Service::ExportDeltas => porting::export_deltas(operation, context, &participant).await,
            Service::ExportAttachment => porting::export_attachment(operation, context).await,
            Service::ImportDeltas => porting::import_deltas(operation, context).await,
            Service::ImportAttachment => porting::import_attachment(operation, context).await,
        }
    }
}

/// The name of an operation type's constant in the original, which the
/// "no service" error shows.
fn operation_type_name(operation: OperationType) -> String {
    // The constant names are the variant names in upper snake case.
    let variant = format!("{operation:?}");
    let mut name = String::new();
    for (i, c) in variant.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            name.push('_');
        }
        name.extend(c.to_uppercase());
    }
    name
}

/// The participant an actor acts as when proxying for someone:
/// `actor+proxyFor@domain` (`OperationUtil.toProxyParticipant`). Without a
/// proxy the actor itself.
pub fn to_proxy_participant(
    participant: &ParticipantId,
    proxy_for: Option<&str>,
) -> Result<ParticipantId, InvalidRequest> {
    let Some(proxy_for) = proxy_for.filter(|p| !p.is_empty()) else {
        return Ok(participant.clone());
    };
    let invalid = |address: &str| {
        InvalidRequest::new(format!(
            "{address} is not a valid robot name, the proxy is likely to be wrong"
        ))
    };
    let mut robot_name = RobotName::from_address(participant.address())
        .ok_or_else(|| invalid(participant.address()))?;
    robot_name.set_proxy_for(proxy_for);
    let address = robot_name.to_participant_address();
    if !RobotName::is_well_formed_address(&address) {
        return Err(invalid(&address));
    }
    ParticipantId::of(&address).map_err(|_| invalid(&address))
}

/// The participant an operation is executed as: the actor, or its proxy
/// if the operation has a `proxyingFor` parameter
/// (`OperationUtil.computeParticipant`).
pub fn compute_participant(
    operation: &OperationRequest,
    actor: &ParticipantId,
) -> Result<ParticipantId, InvalidRequest> {
    let proxy_for = operation.string(ParamsProperty::ProxyingFor);
    to_proxy_participant(actor, proxy_for.as_deref()).map_err(|_| {
        // The original's message, operator precedence slip included: with
        // a proxy it is just the address.
        InvalidRequest::new(match &proxy_for {
            Some(proxy) => format!("{}+{proxy}", actor.address()),
            None => format!("{} is not a valid participant address", actor.address()),
        })
    })
}
