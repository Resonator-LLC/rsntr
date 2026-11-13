//! Putting it together: the executor with the server's services, the
//! dispatcher on the wave bus, the robot directory, the built-in agents
//! and the HTTP routes. [`Integrations::mount`] is the one call the
//! binary makes between `App::prepare` and `PreparedApp::serve`.

use std::sync::Arc;

use axum::Router;
use resonator_robots::events::{
    AccountRobotDirectory, DispatcherHandle, DispatcherOptions, RobotDispatcher,
};
use resonator_robots::ops::OperationExecutor;
use resonator_server::app::PreparedApp;
use resonator_server::http::AppState;
use resonator_server::metrics::write_metric;

use crate::adapters::{AttachmentAdapter, ProfilesAdapter, SearchAdapter};
use crate::agent::AgentRuntime;
use crate::http::Shared;
use crate::robots::connector::{ConnectorSettings, EventHub, RobotConnector, RobotSink};

/// Failure to set up robots and agents.
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    #[error("robots: {0}")]
    Robots(String),
    #[error("agents: {0}")]
    Agents(String),
}

/// The running robots, agents and tool endpoints.
pub struct Integrations {
    shared: Arc<Shared>,
    handle: Option<DispatcherHandle>,
    agents: Option<AgentRuntime>,
}

impl std::fmt::Debug for Integrations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Integrations").finish_non_exhaustive()
    }
}

/// The executor the robot endpoints, agents and tools act through: the
/// server's search, attachments and profiles.
pub fn executor_for(state: &AppState, connector: RobotConnector) -> OperationExecutor {
    OperationExecutor::new(state.server.clone())
        .with_clock(Arc::clone(state.auth.clock()))
        .with_search(Arc::new(SearchAdapter::new(
            state.search.clone(),
            state.config.search_max_results,
        )))
        .with_attachments(Arc::new(AttachmentAdapter::new(state.attachments.clone())))
        .with_profiles(Arc::new(ProfilesAdapter::new(
            state.auth.accounts().clone(),
        )))
        .with_capabilities_fetcher(Arc::new(connector))
}

impl Integrations {
    /// Sets everything up on a prepared server and returns the routes to
    /// serve with it. Starts the dispatcher; registers the configured
    /// agents (creating their accounts). With no `[ai]` agents configured
    /// no agent runs and no model provider is ever called.
    pub async fn mount(
        prepared: &mut PreparedApp,
    ) -> Result<(Integrations, Router<AppState>), MountError> {
        let state = prepared.state().clone();
        let config = &state.config;
        let accounts = state.auth.accounts().clone();
        let robots = state.server.database().robot_store();
        let connector = RobotConnector::new(ConnectorSettings::from_config(config), robots.clone())
            .map_err(|e| MountError::Robots(e.0))?;
        let executor = Arc::new(executor_for(&state, connector.clone()));
        let options = DispatcherOptions {
            max_hops: Some(config.robot_max_hops),
            delivery_timeout: Some(
                std::time::Duration::from_secs(config.robot_request_timeout_secs.max(1))
                    .saturating_add(std::time::Duration::from_secs(5)),
            ),
            ..DispatcherOptions::default()
        };
        let dispatcher = RobotDispatcher::new(Arc::clone(&executor), options);
        let hub = Arc::new(EventHub::new(config.robot_event_stream_buffer));
        let sink = Arc::new(RobotSink {
            connector: connector.clone(),
            hub: Arc::clone(&hub),
            accounts: accounts.clone(),
        });
        if config.robot_api_enabled {
            dispatcher.set_directory(Some(Arc::new(AccountRobotDirectory::new(
                accounts.clone(),
                sink,
                Some(Arc::new(connector.clone())),
            ))));
        }
        let handle = dispatcher.start();
        let agents = if config.ai.is_active() {
            Some(
                AgentRuntime::start(&state, &dispatcher)
                    .await
                    .map_err(MountError::Agents)?,
            )
        } else {
            None
        };
        let shared = Arc::new(Shared {
            executor,
            dispatcher: dispatcher.clone(),
            hub,
            connector,
            robots,
        });
        {
            let dispatcher = dispatcher.clone();
            let agent_metrics = agents.as_ref().map(AgentRuntime::metrics);
            state.metrics.register_extension(move |out| {
                let stats = dispatcher.stats();
                write_metric(
                    out,
                    "robot_bundles_delivered_total",
                    "counter",
                    "Event bundles handed to robots and agents.",
                    i128::from(stats.bundles_delivered),
                );
                write_metric(
                    out,
                    "robot_deliveries_failed_total",
                    "counter",
                    "Deliveries to robots that failed or timed out.",
                    i128::from(stats.deliveries_failed),
                );
                write_metric(
                    out,
                    "robot_bundles_suppressed_total",
                    "counter",
                    "Bundles withheld by the robot loop guard.",
                    i128::from(stats.bundles_suppressed),
                );
                if let Some(metrics) = &agent_metrics {
                    metrics.render(out);
                }
            });
        }
        let routes = crate::http::routes(&state, Arc::clone(&shared));
        Ok((
            Integrations {
                shared,
                handle: Some(handle),
                agents,
            },
            routes,
        ))
    }

    pub fn dispatcher(&self) -> &RobotDispatcher {
        &self.shared.dispatcher
    }

    pub fn executor(&self) -> &Arc<OperationExecutor> {
        &self.shared.executor
    }

    pub fn agents(&self) -> Option<&AgentRuntime> {
        self.agents.as_ref()
    }

    /// Stops the agents (cancelling responses being written) and the
    /// dispatcher. Call before the server shuts down.
    pub async fn shutdown(mut self) {
        if let Some(agents) = self.agents.take() {
            agents.shutdown().await;
        }
        if let Some(handle) = self.handle.take() {
            handle.stop();
        }
    }
}
