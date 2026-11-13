//! Events for robots: what happened to a wavelet, told to the robots on
//! it, and their replies applied.
//!
//! Derived from `org.waveprotocol.box.server.robots.passive` of Apache
//! Wave (`EventGenerator`, `WaveletAndDeltas`, `WaveletOperationInverter`,
//! `Robot`, `RobotsGateway`, `RobotOperationApplicator`, `RobotConnector`),
//! licensed under the Apache License, Version 2.0.
//!
//! * [`EventGenerator`] turns a wavelet's state before some deltas, and
//!   the deltas, into the [`EventMessageBundle`](crate::api
//!   ::EventMessageBundle) for one robot.
//! * [`RobotDispatcher`] listens to the wave bus, keeps a bounded queue of
//!   updates per robot, delivers bundles through an [`EventSink`] and
//!   executes the operations the sink returns.
//!
//! Two things are added for agents that are language models, and are not
//! in the original: a loop guard ([`DispatcherOptions::max_hops`],
//! [`EventDelivery::hop_count`], [`Reactor`]) and a summary of whether the
//! robot was addressed ([`AgentTriggers`]).

mod dispatcher;
mod generator;
mod wavelet_and_deltas;

pub use dispatcher::{
    AccountRobotDirectory, DispatcherHandle, DispatcherOptions, DispatcherStats, EventDelivery,
    EventSink, Reactor, RobotDirectory, RobotDispatcher, RobotEndpoint, SinkError,
};
pub use generator::{AgentTriggers, EventGenerator, GeneratedEvents, MentionTrigger};
pub use wavelet_and_deltas::{EventError, WaveletAndDeltas, invert_operation, rollback};
