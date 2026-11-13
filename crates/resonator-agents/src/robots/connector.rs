//! Calls to external robots (`RobotConnector`): event bundles posted to a
//! robot's URL, capabilities fetched from it, and the stream that robots
//! without a URL listen to instead.
//!
//! Every call is signed ([`super::sign`]), goes only to addresses the
//! operator allows ([`super::ssrf`]), follows no redirects, has timeouts,
//! and reads at most `robot_max_response_bytes` of the answer.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use resonator_model::id::ParticipantId;
use resonator_robots::api::{OperationRequest, deserialize_operations};
use resonator_robots::events::{EventDelivery, EventSink, SinkError};
use resonator_robots::ops::CapabilitiesFetcher;
use resonator_server::account::{AccountData, RobotAccountData, RobotCapabilities};
use resonator_server::config::Config;
use resonator_server::storage::{AccountStore, RobotStore};
use tokio::sync::Notify;

use super::capabilities::parse_capabilities;
use super::sign;
use super::ssrf::{AddressPolicy, GuardedResolver};

/// Where the original's robots take event bundles (`Robot.RPC_URL`).
pub const RPC_PATH: &str = "/_wave/robot/jsonrpc";
/// Where the original's robots publish their capabilities
/// (`Robot.CAPABILITIES_URL`).
pub const CAPABILITIES_PATH: &str = "/_wave/capabilities.xml";

/// The settings of calls to robots.
#[derive(Debug, Clone)]
pub struct ConnectorSettings {
    pub policy: Arc<AddressPolicy>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
}

impl ConnectorSettings {
    pub fn from_config(config: &Config) -> Self {
        ConnectorSettings {
            policy: Arc::new(AddressPolicy::from_config(config)),
            connect_timeout: Duration::from_millis(config.robot_connect_timeout_ms.max(1)),
            request_timeout: Duration::from_secs(config.robot_request_timeout_secs.max(1)),
            max_response_bytes: config.robot_max_response_bytes.max(1024),
        }
    }
}

/// The HTTP side of talking to robots.
#[derive(Clone)]
pub struct RobotConnector {
    client: reqwest::Client,
    settings: ConnectorSettings,
    robots: RobotStore,
}

impl std::fmt::Debug for RobotConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RobotConnector")
            .field("settings", &self.settings)
            .finish()
    }
}

/// Failure of a call to a robot; the message is safe to show the robot's
/// owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CallError(pub String);

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl RobotConnector {
    pub fn new(settings: ConnectorSettings, robots: RobotStore) -> Result<Self, CallError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(GuardedResolver::new(Arc::clone(&settings.policy))))
            .connect_timeout(settings.connect_timeout)
            .timeout(settings.request_timeout)
            .user_agent("resonator-robot-connector")
            .build()
            .map_err(|error| CallError(format!("cannot build the HTTP client: {error}")))?;
        Ok(RobotConnector {
            client,
            settings,
            robots,
        })
    }

    pub fn policy(&self) -> &AddressPolicy {
        &self.settings.policy
    }

    /// Makes a signed call to `base + path` and returns the body, at most
    /// `max_response_bytes` of it.
    async fn call(
        &self,
        robot: &ParticipantId,
        base: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<String, CallError> {
        let url = format!("{}{path}", base.trim_end_matches('/'));
        let parsed = self
            .settings
            .policy
            .check_url(&url)
            .map_err(|error| CallError(error.to_string()))?;
        let secret = match self.robots.registration(robot).await {
            Ok(Some(registration)) => Some(registration.webhook_secret),
            Ok(None) => None,
            Err(error) => return Err(CallError(format!("storage error: {error}"))),
        };
        let timestamp = unix_seconds();
        let bytes = body.clone().unwrap_or_default();
        let mut request = match body {
            Some(body) => self
                .client
                .post(parsed)
                .header("content-type", "application/json; charset=utf-8")
                .body(body),
            None => self.client.get(parsed),
        };
        request = request
            .header(sign::ROBOT_HEADER, robot.address())
            .header(sign::TIMESTAMP_HEADER, timestamp.to_string());
        if let Some(secret) = secret {
            request = request.header(
                sign::SIGNATURE_HEADER,
                sign::sign(&secret, timestamp, &bytes),
            );
        }
        let response = request.send().await.map_err(|error| {
            let mut message = error.to_string();
            // The resolver's refusal is in the source chain.
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                message = cause.to_string();
                source = cause.source();
            }
            CallError(format!("cannot call the robot: {message}"))
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(CallError(format!(
                "the robot answered HTTP {}",
                status.as_u16()
            )));
        }
        let max = self.settings.max_response_bytes;
        if response
            .content_length()
            .is_some_and(|length| length as usize > max)
        {
            return Err(CallError(format!("the robot's answer is over {max} bytes")));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| CallError(format!("reading the answer: {error}")))?;
            if body.len() + chunk.len() > max {
                return Err(CallError(format!("the robot's answer is over {max} bytes")));
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body).map_err(|_| CallError("the robot's answer is not UTF-8".to_owned()))
    }

    /// Posts a bundle to a robot and returns the operations it answers
    /// with (`RobotConnector.sendMessageBundle`).
    pub async fn post_bundle(
        &self,
        robot: &RobotAccountData,
        delivery: &EventDelivery,
    ) -> Result<Vec<OperationRequest>, CallError> {
        let Some(url) = robot.url() else {
            return Err(CallError("the robot has no URL".to_owned()));
        };
        let body = serde_json::to_vec(&delivery.bundle)
            .map_err(|error| CallError(format!("cannot serialise the bundle: {error}")))?;
        let answer = self.call(&robot.id, url, RPC_PATH, Some(body)).await?;
        if answer.trim().is_empty() {
            return Ok(Vec::new());
        }
        deserialize_operations(&answer)
            .map_err(|error| CallError(format!("the robot's answer is not operations: {error}")))
    }

    /// Fetches a robot's `capabilities.xml`.
    pub async fn fetch(&self, robot: &RobotAccountData) -> Result<RobotCapabilities, CallError> {
        let Some(url) = robot.url() else {
            return Err(CallError("the robot has no URL".to_owned()));
        };
        let xml = self.call(&robot.id, url, CAPABILITIES_PATH, None).await?;
        parse_capabilities(&xml).map_err(|error| CallError(error.to_string()))
    }
}

#[async_trait]
impl CapabilitiesFetcher for RobotConnector {
    async fn fetch_capabilities(
        &self,
        account: &RobotAccountData,
    ) -> Result<RobotCapabilities, String> {
        self.fetch(account).await.map_err(|error| error.0)
    }
}

/// One listener on `/robot/events`.
#[derive(Debug, Default)]
pub struct StreamListener {
    queue: Mutex<VecDeque<String>>,
    notify: Notify,
    capacity: usize,
    dropped: std::sync::atomic::AtomicU64,
}

impl StreamListener {
    /// The next bundle, as JSON; waits for one.
    pub async fn next(&self) -> String {
        loop {
            let notified = self.notify.notified();
            if let Some(bundle) = self
                .queue
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
            {
                return bundle;
            }
            notified.await;
        }
    }

    fn push(&self, bundle: String) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        while queue.len() >= self.capacity.max(1) {
            queue.pop_front();
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        queue.push_back(bundle);
        drop(queue);
        self.notify.notify_one();
    }
}

/// The robots listening on `/robot/events`, and the hop counts of what
/// was last delivered to each robot, for the loop guard of `/robot/rpc`.
#[derive(Debug, Default)]
pub struct EventHub {
    listeners: Mutex<HashMap<ParticipantId, Vec<Arc<StreamListener>>>>,
    hops: Mutex<HashMap<ParticipantId, (u32, Instant)>>,
    capacity: usize,
}

/// How long the hop count of a delivery counts for the robot's calls to
/// `/robot/rpc`.
const HOP_MEMORY: Duration = Duration::from_secs(60);

impl EventHub {
    pub fn new(capacity: usize) -> Self {
        EventHub {
            capacity: capacity.max(1),
            ..EventHub::default()
        }
    }

    /// Starts listening for a robot.
    pub fn listen(&self, robot: &ParticipantId) -> Arc<StreamListener> {
        let listener = Arc::new(StreamListener {
            capacity: self.capacity,
            ..StreamListener::default()
        });
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(robot.clone())
            .or_default()
            .push(Arc::clone(&listener));
        listener
    }

    /// Stops a listener.
    pub fn unlisten(&self, robot: &ParticipantId, listener: &Arc<StreamListener>) {
        let mut listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = listeners.get_mut(robot) {
            list.retain(|l| !Arc::ptr_eq(l, listener));
            if list.is_empty() {
                listeners.remove(robot);
            }
        }
    }

    /// Hands a bundle to every listener of a robot; returns how many.
    pub fn publish(&self, robot: &ParticipantId, bundle: &str) -> usize {
        let listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner());
        let Some(list) = listeners.get(robot) else {
            return 0;
        };
        for listener in list {
            listener.push(bundle.to_owned());
        }
        list.len()
    }

    pub fn note_hop(&self, robot: &ParticipantId, hop: u32) {
        let mut hops = self.hops.lock().unwrap_or_else(|e| e.into_inner());
        if hops.len() > 10_000 {
            hops.retain(|_, (_, at)| at.elapsed() < HOP_MEMORY);
        }
        hops.insert(robot.clone(), (hop, Instant::now()));
    }

    /// The hop count of the last delivery to a robot, if recent.
    pub fn recent_hop(&self, robot: &ParticipantId) -> u32 {
        self.hops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(robot)
            .filter(|(_, at)| at.elapsed() < HOP_MEMORY)
            .map_or(0, |(hop, _)| *hop)
    }
}

/// The sink of external robots: the webhook for robots with a URL, the
/// event stream for those without.
pub struct RobotSink {
    pub connector: RobotConnector,
    pub hub: Arc<EventHub>,
    pub accounts: AccountStore,
}

#[async_trait]
impl EventSink for RobotSink {
    async fn deliver(&self, delivery: EventDelivery) -> Result<Vec<OperationRequest>, SinkError> {
        let address = delivery.robot.to_email_address();
        let id = ParticipantId::new_unchecked(&address);
        let account = match self.accounts.get_account(&id).await {
            Ok(Some(AccountData::Robot(robot))) => robot,
            Ok(_) => return Err(SinkError(format!("{address} is not a robot"))),
            Err(error) => return Err(SinkError(error.to_string())),
        };
        self.hub.note_hop(&id, delivery.hop_count);
        if account.url().is_some() {
            return self
                .connector
                .post_bundle(&account, &delivery)
                .await
                .map_err(|error| SinkError(error.0));
        }
        let body = serde_json::to_string(&delivery.bundle)
            .map_err(|error| SinkError(error.to_string()))?;
        if self.hub.publish(&id, &body) == 0 {
            return Err(SinkError(format!("nobody listens for {address}")));
        }
        // Answers come through /robot/rpc.
        Ok(Vec::new())
    }
}
