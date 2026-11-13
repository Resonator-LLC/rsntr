//! Robots that run inside the server.
//!
//! Derived from the idea of `org.waveprotocol.box.server.robots.agent
//! .AbstractBaseRobotAgent` of Apache Wave (Apache License, Version 2.0):
//! a robot implemented in the server registers itself as a robot account
//! and is then treated like any other robot. The original's agents are
//! servlets that the server calls over HTTP on its own address; here an
//! agent is a trait object that the dispatcher calls directly.

use std::sync::Arc;

use async_trait::async_trait;
use resonator_model::id::ParticipantId;
use resonator_server::account::{
    AccountData, Capability, ProtocolVersion, RobotAccountData, RobotCapabilities,
};
use resonator_server::storage::AccountStore;
use thiserror::Error;

use crate::api::{OperationRequest, RobotName};
use crate::events::{EventDelivery, EventSink, RobotDispatcher, RobotEndpoint, SinkError};
use crate::ops::Registry;

/// A robot that runs inside the server: a participant with an address in
/// the server's domain that is told what happens on the waves it is on
/// and answers with operations.
///
/// Built-in AI agents are robot agents: they see a wave as the bundles
/// show it and act through the same operations as any API client.
#[async_trait]
pub trait RobotAgent: Send + Sync + 'static {
    /// The local part of the agent's address, e.g. `welcome-bot`
    /// (`getRobotId`).
    fn robot_id(&self) -> &str;

    /// The events the agent wants and the blips it wants with them.
    fn capabilities(&self) -> Vec<Capability>;

    /// The shape of the bundles the agent gets.
    fn protocol_version(&self) -> ProtocolVersion {
        ProtocolVersion::DEFAULT
    }

    /// Called with each bundle of events, one at a time. The operations
    /// returned are executed as the agent, against the wavelet as the
    /// bundle shows it, with the operations of [`Registry::Agent`].
    ///
    /// An agent that needs time should return quickly and act later
    /// through `delivery.reactor`.
    async fn on_events(&self, delivery: EventDelivery) -> Vec<OperationRequest>;
}

/// Failure to register an agent.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("invalid agent address: {0}")]
    InvalidAddress(String),
    #[error("{0} is the account of a human")]
    NotARobot(ParticipantId),
    #[error(transparent)]
    Storage(#[from] resonator_server::storage::StorageError),
}

struct AgentSink {
    agent: Arc<dyn RobotAgent>,
}

#[async_trait]
impl EventSink for AgentSink {
    async fn deliver(&self, delivery: EventDelivery) -> Result<Vec<OperationRequest>, SinkError> {
        Ok(self.agent.on_events(delivery).await)
    }
}

/// The hash recorded for an agent's capabilities; agents do not have a
/// `capabilities.xml` to hash.
const AGENT_CAPABILITIES_HASH: &str = "agent";

/// Makes an agent a participant others can add to waves: stores (or
/// updates) its robot account `<robot id>@<domain>` with its capabilities
/// and registers it with the dispatcher (`ensureRegistered`). Returns the
/// agent's address.
pub async fn register_agent(
    dispatcher: &RobotDispatcher,
    accounts: &AccountStore,
    agent: Arc<dyn RobotAgent>,
) -> Result<ParticipantId, AgentError> {
    let domain = dispatcher.executor().server().domain().to_owned();
    let address = format!("{}@{domain}", agent.robot_id());
    if !RobotName::is_well_formed_address(&address) {
        return Err(AgentError::InvalidAddress(address));
    }
    let id = ParticipantId::of(&address).map_err(|_| AgentError::InvalidAddress(address))?;
    let capabilities = RobotCapabilities::new(
        agent.capabilities(),
        AGENT_CAPABILITIES_HASH,
        agent.protocol_version(),
    );
    let mut account = match accounts.get_account(&id).await? {
        Some(AccountData::Human(_)) => return Err(AgentError::NotARobot(id)),
        Some(AccountData::Robot(robot)) if robot.is_agent() => robot,
        _ => RobotAccountData::agent(id.clone()),
    };
    if account.capabilities.as_ref() != Some(&capabilities) {
        account.capabilities = Some(capabilities.clone());
        accounts.put_account(AccountData::Robot(account)).await?;
    }
    dispatcher.register(RobotEndpoint {
        account_id: id.clone(),
        capabilities,
        sink: Arc::new(AgentSink { agent }),
        registry: Registry::Agent,
    });
    Ok(id)
}
