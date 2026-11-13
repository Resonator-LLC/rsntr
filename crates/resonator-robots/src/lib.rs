//! The Wave Robots API and Data API, as a library without a transport:
//! the JSON-RPC operation set with its executor, and the generation and
//! delivery of events.
//!
//! Derived from `com.google.wave.api` and
//! `org.waveprotocol.box.server.robots` of Apache Wave (Apache License
//! 2.0); the modules say what they were ported from.
//!
//! Everything that acts on waves on someone's behalf without being the
//! web client goes through here: classic robots, Data API clients, the
//! agents built into the server, and tool adapters for language models.
//! They all see a wave the same way, as [`api::WaveletData`] and
//! [`api::BlipData`], and change it with the same operations.
//!
//! - [`api`]: the data model and its JSON: requests, responses, events,
//!   wavelets, blips, elements.
//! - [`ops`]: [`ops::OperationExecutor`] executes batches of
//!   [`api::OperationRequest`]s as a participant against the wave server.
//! - [`events`]: [`events::EventGenerator`] says what happened to a
//!   wavelet from one robot's point of view; [`events::RobotDispatcher`]
//!   delivers that to robots and applies their replies.
//! - [`agent`]: robots that run inside the server.
//! - [`view`] and [`convert`]: how a blip document becomes text, elements
//!   and annotations, and back.
//!
//! Authentication is not in here: the caller says who acts.
//!
//! # Executing operations
//!
//! ```no_run
//! use resonator_model::id::ParticipantId;
//! use resonator_robots::api::{JsonRpcResponse, deserialize_operations};
//! use resonator_robots::ops::{OperationExecutor, Registry};
//!
//! /// The body of a Data API endpoint: JSON in, JSON out. `user` is who
//! /// the request was authenticated as.
//! async fn data_api(executor: &OperationExecutor, user: &ParticipantId, body: &str) -> String {
//!     let requests = match deserialize_operations(body) {
//!         Ok(requests) => requests,
//!         Err(error) => return format!("400 {error}"),
//!     };
//!     let responses: Vec<JsonRpcResponse> =
//!         executor.execute(user, Registry::DataApi, requests).await;
//!     serde_json::to_string(&responses).unwrap_or_default()
//! }
//! ```
//!
//! # An agent inside the server
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use async_trait::async_trait;
//! use resonator_robots::agent::{RobotAgent, register_agent};
//! use resonator_robots::api::{
//!     BlipData, Capability, DocumentModifyAction, EventType, ModifyHow, OperationRequest,
//!     OperationType, ParamsProperty,
//! };
//! use resonator_robots::events::{EventDelivery, RobotDispatcher};
//!
//! struct Assistant;
//!
//! #[async_trait]
//! impl RobotAgent for Assistant {
//!     fn robot_id(&self) -> &str {
//!         "assistant"
//!     }
//!
//!     fn capabilities(&self) -> Vec<Capability> {
//!         vec![
//!             Capability::new(EventType::WaveletSelfAdded),
//!             Capability::new(EventType::WaveletBlipCreated),
//!         ]
//!     }
//!
//!     async fn on_events(&self, delivery: EventDelivery) -> Vec<OperationRequest> {
//!         // Only when added to a wave or directly replied to.
//!         if !delivery.triggers.any() {
//!             return Vec::new();
//!         }
//!         let Some(wavelet) = delivery.bundle.wavelet.clone() else {
//!             return Vec::new();
//!         };
//!         let (wave_id, wavelet_id) = (
//!             wavelet.wave_id.unwrap_or_default(),
//!             wavelet.wavelet_id.unwrap_or_default(),
//!         );
//!         let reply_to = delivery
//!             .triggers
//!             .reply_blips_created
//!             .first()
//!             .cloned()
//!             .or(wavelet.root_blip_id)
//!             .unwrap_or_default();
//!         // Answer at leisure, through the reactor: an empty reply blip
//!         // first, then text as it comes.
//!         tokio::spawn(async move {
//!             let create = OperationRequest::for_blip(
//!                 OperationType::BlipCreateChild,
//!                 "create",
//!                 &wave_id,
//!                 &wavelet_id,
//!                 &reply_to,
//!             )
//!             .with(
//!                 ParamsProperty::BlipData,
//!                 BlipData::new(&wave_id, &wavelet_id, "TBD_reply", ""),
//!             );
//!             let created = delivery.reactor.execute(vec![create]).await;
//!             let Some(blip_id) = created.responses[0].string(ParamsProperty::NewBlipId) else {
//!                 return;
//!             };
//!             for chunk in ["Let me ", "think", "..."] {
//!                 // Without a position, INSERT_AFTER appends to the blip.
//!                 let append = OperationRequest::for_blip(
//!                     OperationType::DocumentModify,
//!                     "append",
//!                     &wave_id,
//!                     &wavelet_id,
//!                     blip_id,
//!                 )
//!                 .with(
//!                     ParamsProperty::ModifyAction,
//!                     DocumentModifyAction::with_values(ModifyHow::InsertAfter, [chunk]),
//!                 );
//!                 delivery.reactor.execute(vec![append]).await;
//!             }
//!         });
//!         Vec::new()
//!     }
//! }
//!
//! # async fn start(dispatcher: &RobotDispatcher) {
//! let accounts = dispatcher.executor().server().database().account_store();
//! let address = register_agent(dispatcher, &accounts, Arc::new(Assistant)).await;
//! # }
//! ```

pub mod agent;
pub mod api;
pub mod convert;
pub mod events;
pub mod ops;
pub mod view;
