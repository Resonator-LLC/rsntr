//! The resonator node as a library: the wave server with the robots,
//! tools and agents of `resonator-agents`, federation
//! (`resonator-federation`) and the embedded web client, assembled,
//! started and stopped in-process.
//!
//! Two ways in:
//! - [`launch`] / [`launch_with`] inside a tokio runtime the caller owns
//!   (the `resonator` binary, tests): returns a [`Node`], stopped with
//!   [`Node::shutdown`].
//! - [`start`] / [`start_with`] from any thread outside a runtime (the
//!   FFI of `resonator-ffi`, apps): the node gets a tokio runtime of its
//!   own and the blocking [`Handle`].
//!
//! The assembly, in order:
//! 1. device mode: loopback listening by default, and a person key on the
//!    first start of a device that is its person's first ([`launch_with`]);
//! 2. person mode: the domain becomes the person domain;
//! 3. federation set up (key loaded or created) before the wave server
//!    starts, so that every delta is signed;
//! 4. the wave server and its services (`App::prepare_with`), subscribed
//!    to by federation before anything can write;
//! 5. robots, Data API, MCP and agents (`Integrations::mount`);
//! 6. the HTTP server with the web client's routes;
//! 7. the federation endpoint, last.
//!
//! [`Node::shutdown`] stops them the other way round: federation, then
//! the agents (cancelling responses being written), then the HTTP and
//! wave servers, flushing and closing the database.
//!
//! Device mode (`mode = "device"`, see
//! [`resonator_server::config::DeviceConfig`]) is what an app embeds: one
//! account, signed in from loopback with [`Node::token`] by loading
//! [`Node::signed_in_url`] in a WebView. [`Node::enter_background`] and
//! [`Node::enter_foreground`] are the app's lifecycle hooks.

mod handle;
pub mod push;
mod web;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use resonator_agents::{Integrations, MountError};
use resonator_federation::dns::TxtResolver;
use resonator_federation::{
    Federation, FederationSetup, FileKeyStore, KeyStore, PersonKey, SetupError,
};
use resonator_server::app::{self, App, AppError};
use resonator_server::config::{Config, ConfigError};
use resonator_server::http::AppState;
use resonator_server::waveserver::{WaveServer, WaveServerOptions};
use thiserror::Error;
use tokio::time::Instant;

pub use handle::{Handle, start, start_with};
pub use resonator_federation::{LeaseState, PublicKey, Tuning};

/// Failure to start or use a node.
#[derive(Debug, Error)]
pub enum NodeError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Federation(#[from] SetupError),
    #[error(transparent)]
    App(#[from] AppError),
    #[error(transparent)]
    Integrations(#[from] MountError),
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the node is stopped")]
    Stopped,
    /// Pairing, recovery or another person operation failed.
    #[error("{0}")]
    Person(String),
}

/// What [`launch_with`] may replace (tests shorten federation's timings
/// and give it a DNS resolver of their own).
#[derive(Clone, Default)]
pub struct LaunchOptions {
    pub tuning: Option<Tuning>,
    pub resolver: Option<Arc<dyn TxtResolver>>,
}

impl std::fmt::Debug for LaunchOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchOptions")
            .field("tuning", &self.tuning)
            .finish_non_exhaustive()
    }
}

/// The largest attachment a device takes by default: 4 GiB.
const DEVICE_ATTACHMENT_MAX_SIZE: i64 = 4 * 1024 * 1024 * 1024;

/// The configuration of a device node keeping its files in `data_dir`:
/// `mode = "device"`, HTTP on `127.0.0.1:0`, the database
/// `resonator.db` there, federation and person mode on, and the device an
/// admin device (it holds the person key, made on the first start), and
/// wave apps on.
/// `overrides` is TOML laid over these defaults, key by key (a `[person]`
/// table replaces only the keys it names); it is checked like a config
/// file.
pub fn device_config(data_dir: &Path, overrides: &str) -> Result<Config, NodeError> {
    let mut defaults = toml::Table::new();
    let set = |table: &mut toml::Table, key: &str, value: toml::Value| {
        table.insert(key.to_owned(), value);
    };
    let path = |path: &Path| toml::Value::String(path.display().to_string());
    set(&mut defaults, "mode", "device".into());
    set(
        &mut defaults,
        "listen",
        toml::Value::Array(vec!["127.0.0.1:0".into()]),
    );
    set(&mut defaults, "data_dir", path(data_dir));
    set(
        &mut defaults,
        "database_path",
        path(&data_dir.join("resonator.db")),
    );
    // A device stores only its own person's uploads, and uploads, serving
    // and federation transfers all stream from disk: no reason for the
    // server's 25 MB. Disk space is the real limit.
    set(
        &mut defaults,
        "attachment_max_size",
        toml::Value::Integer(DEVICE_ATTACHMENT_MAX_SIZE),
    );
    let mut federation = toml::Table::new();
    set(&mut federation, "enabled", true.into());
    set(&mut defaults, "federation", federation.into());
    let mut person = toml::Table::new();
    set(&mut person, "enabled", true.into());
    set(&mut person, "admin", true.into());
    set(&mut defaults, "person", person.into());
    let mut apps = toml::Table::new();
    set(&mut apps, "enabled", true.into());
    set(&mut defaults, "apps", apps.into());
    let overrides: toml::Table = toml::from_str(overrides)
        .map_err(|error| NodeError::Invalid(format!("device config overrides: {error}")))?;
    merge(&mut defaults, overrides);
    let text = toml::to_string(&defaults).map_err(|error| NodeError::Invalid(error.to_string()))?;
    let config = Config::from_toml(&text)
        .map_err(|error| NodeError::Invalid(format!("device config: {error}")))?;
    config.validate()?;
    Ok(config)
}

/// Lays `over` onto `base`, table by table.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(over)) => merge(base, over),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Device mode, before anything starts: listens on loopback (any free
/// port) unless told otherwise, and makes the person key on the first
/// start of an admin device that has neither a key nor a certificate (the
/// person's first device; others are paired). The key's recovery phrase
/// is never logged: `resonator person show --phrase` shows it.
fn prepare_device(config: &mut Config) -> Result<(), NodeError> {
    if !config.is_device() {
        return Ok(());
    }
    if !config.device.allow_remote && config.listen == Config::default().listen {
        config.listen = vec!["127.0.0.1:0".to_owned()];
    }
    std::fs::create_dir_all(config.data_directory())?;
    let person = &config.person;
    if config.federation.enabled
        && person.enabled
        && person.admin
        && person.cert.is_empty()
        && !config.device_cert_path().exists()
    {
        let store = FileKeyStore::new(config.person_key_path());
        let stored = store.load().map_err(SetupError::from)?;
        if stored.is_none() {
            let (key, phrase) = PersonKey::generate();
            store.store(&phrase).map_err(SetupError::from)?;
            tracing::info!(
                domain = %key.domain(),
                path = %store.path().display(),
                "made a person key for this device (show its recovery phrase with `resonator \
                 person show --phrase`)"
            );
        }
    }
    Ok(())
}

/// A running node.
pub struct Node {
    config: Config,
    app: App,
    integrations: Integrations,
    federation: Option<Federation>,
    /// For [`Node::relaunch`].
    options: LaunchOptions,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("domain", &self.config.domain)
            .field("listen", &self.app.local_addrs())
            .finish_non_exhaustive()
    }
}

/// Starts a node on `config` in the current tokio runtime.
pub async fn launch(config: Config) -> Result<Node, NodeError> {
    launch_with(config, LaunchOptions::default()).await
}

/// [`launch`] with options.
pub async fn launch_with(mut config: Config, options: LaunchOptions) -> Result<Node, NodeError> {
    let launch_options = options.clone();
    prepare_device(&mut config)?;
    // Person mode: the domain is the person's.
    FederationSetup::configure_person(&mut config)?;
    // Federation disabled: no key, no signer, options as before.
    let mut federation_setup = FederationSetup::load(&config)?.map(|mut setup| {
        if let Some(tuning) = &options.tuning {
            setup = setup.with_tuning(tuning.clone());
        }
        if let Some(resolver) = &options.resolver {
            setup = setup.with_resolver(Arc::clone(resolver));
        }
        setup
    });
    let mut server_options = WaveServerOptions::from_config(&config);
    if let Some(setup) = &federation_setup {
        server_options.delta_signer = Some(setup.delta_signer());
    }
    let mut prepared = App::prepare_with(&config, server_options).await?;
    if let Some(setup) = &mut federation_setup {
        // Before anything (agents, clients) can write: every delta from
        // here on is fanned out.
        setup.subscribe(prepared.server());
        setup.serve_app(prepared.state());
    }
    let (integrations, routes) = match Integrations::mount(&mut prepared).await {
        Ok(mounted) => mounted,
        Err(error) => {
            prepared.abandon().await;
            return Err(error.into());
        }
    };
    let app = match prepared.serve(routes.merge(web::routes())).await {
        Ok(app) => app,
        Err(error) => {
            integrations.shutdown().await;
            return Err(error.into());
        }
    };
    let federation = match federation_setup {
        Some(setup) => match setup.start(app.server()).await {
            Ok(federation) => Some(federation),
            Err(error) => {
                integrations.shutdown().await;
                app.shutdown().await;
                return Err(error.into());
            }
        },
        None => None,
    };
    // Push (`[push] relay` set): the token the app gave last time is
    // sealed and advertised (nothing happens without push).
    if let Some(federation) = &federation {
        federation.set_push_token(push::load(&config.data_directory()).map(push::raw));
    }
    tracing::info!(
        domain = %config.domain,
        mode = if config.is_device() { "device" } else { "server" },
        database = %config.database_path.display(),
        listen = ?app.local_addrs(),
        robots = config.robot_api_enabled,
        mcp = config.mcp_enabled,
        agents = integrations.agents().map_or(0, |a| a.addresses().len()),
        "resonator ready"
    );
    Ok(Node {
        config,
        app,
        integrations,
        federation,
        options: launch_options,
    })
}

/// What [`Node::enter_background`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundReport {
    /// The device (`<z32 key>`) this device handed its person's host
    /// lease to, if it held it.
    pub handed_over_to: Option<String>,
    /// Whether that device had taken the lease within the budget (it
    /// first catches up with this device).
    pub handover_complete: bool,
    /// Queued submissions answered by their host.
    pub pending_sent: usize,
    /// Queued submissions still waiting for their host.
    pub pending_left: usize,
}

/// What [`Node::enter_foreground`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForegroundReport {
    /// Own devices synced with.
    pub devices_synced: usize,
    /// Wavelets that moved on here by syncing.
    pub wavelets_updated: usize,
    /// Queued submissions answered by their host.
    pub pending_sent: usize,
    /// Queued submissions still waiting for their host.
    pub pending_left: usize,
    /// The host lease after the look (person mode).
    pub lease: Option<LeaseState>,
}

impl Node {
    /// The effective configuration (in person mode with the person
    /// domain; in device mode with the listen address used).
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The push token the app got for this device (`None` forgets it).
    /// Kept in the data directory; never logged. With `[push] relay` set,
    /// it is sealed by the relay and the sealed token advertised so that
    /// other nodes can wake this device.
    pub fn set_push_token(&self, token: Option<&push::PushToken>) -> Result<(), NodeError> {
        push::store(&self.config.data_directory(), token)?;
        if let Some(federation) = &self.federation {
            federation.set_push_token(token.cloned().map(push::raw));
        }
        Ok(())
    }

    /// The push token kept for this device, if any.
    pub fn push_token(&self) -> Option<push::PushToken> {
        push::load(&self.config.data_directory())
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn server(&self) -> &WaveServer {
        self.app.server()
    }

    pub fn state(&self) -> &AppState {
        self.app.state()
    }

    pub fn integrations(&self) -> &Integrations {
        &self.integrations
    }

    /// Federation, if enabled.
    pub fn federation(&self) -> Option<&Federation> {
        self.federation.as_ref()
    }

    /// The addresses the HTTP server is bound to.
    pub fn local_addrs(&self) -> &[SocketAddr] {
        self.app.local_addrs()
    }

    /// `http://<host>:<port>` of the HTTP server (a wildcard address as
    /// loopback).
    pub fn local_url(&self) -> String {
        self.app.local_url().unwrap_or_default()
    }

    /// Device mode: the token that signs the device's account in from
    /// loopback (`GET /?token=`), new at every start. Never log it.
    pub fn token(&self) -> Option<&str> {
        self.app.state().device_token()
    }

    /// Device mode: `<local_url>/?token=<token>`, the page a WebView
    /// loads to be signed in. Never log it.
    pub fn signed_in_url(&self) -> Option<String> {
        self.app.device_url()
    }

    /// Person mode: the host lease as this device sees it.
    pub fn lease_state(&self) -> Option<LeaseState> {
        self.federation.as_ref().map(Federation::lease_state)
    }

    async fn pending_left(&self) -> usize {
        self.server()
            .database()
            .pending_store()
            .all()
            .await
            .map_or(0, |pending| pending.len())
    }

    /// The app goes to the background (iOS gives it some 5 to 30 s):
    /// within `budget`, hands the person's host lease to another device of
    /// the person if this one holds it (an anchor first, then a device that
    /// answered lately) and waits for that device to take it, sends what
    /// waits in the pending queue, and flushes the database.
    pub async fn enter_background(&self, budget: Duration) -> BackgroundReport {
        let deadline = Instant::now() + budget;
        let mut report = BackgroundReport::default();
        if let Some(federation) = &self.federation
            && federation.lease_state().held_here
        {
            for device in federation.own_devices() {
                if Instant::now() >= deadline {
                    break;
                }
                match tokio::time::timeout_at(deadline, federation.handover(device.key)).await {
                    Ok(Ok(())) => {
                        report.handed_over_to = Some(device.key.to_z32());
                        // It catches up with this device, then claims the
                        // next term: wait for its lease.
                        while Instant::now() < deadline {
                            if federation
                                .lease()
                                .is_some_and(|lease| lease.device == device.key)
                            {
                                report.handover_complete = true;
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        break;
                    }
                    Ok(Err(error)) => {
                        tracing::info!(device = %device.key.fmt_short(), %error, "cannot hand the host lease over");
                    }
                    Err(_) => break,
                }
            }
        }
        self.app.state().stop_apps();
        if let Ok(sent) = tokio::time::timeout_at(deadline, self.server().drain_pending()).await {
            report.pending_sent = sent;
        }
        report.pending_left = self.pending_left().await;
        if let Err(error) = self.server().flush().await {
            tracing::warn!(%error, "flushing the database for the background failed");
        }
        tracing::info!(
            handed_over = report.handed_over_to.is_some(),
            complete = report.handover_complete,
            pending_left = report.pending_left,
            "entered the background"
        );
        report
    }

    /// The app comes back to the foreground: within `budget`, tells the
    /// endpoint the network may have changed, syncs with every other
    /// device of the person, sends the pending queue and has the lease
    /// looked at again.
    pub async fn enter_foreground(&self, budget: Duration) -> ForegroundReport {
        let deadline = Instant::now() + budget;
        let mut report = ForegroundReport::default();
        if let Some(federation) = &self.federation {
            let _ = tokio::time::timeout_at(deadline, federation.network_change()).await;
            federation.recheck_lease();
            for device in federation.own_devices() {
                match tokio::time::timeout_at(deadline, federation.sync_with(device.key)).await {
                    Ok(Ok(updated)) => {
                        report.devices_synced += 1;
                        report.wavelets_updated += updated;
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(device = %device.key.fmt_short(), %error, "no sync");
                    }
                    Err(_) => break,
                }
            }
        }
        if let Ok(sent) = tokio::time::timeout_at(deadline, self.server().drain_pending()).await {
            report.pending_sent = sent;
        }
        report.pending_left = self.pending_left().await;
        if let Some(federation) = &self.federation {
            federation.recheck_lease();
            report.lease = Some(federation.lease_state());
        }
        tracing::info!(
            devices = report.devices_synced,
            wavelets = report.wavelets_updated,
            pending_left = report.pending_left,
            "entered the foreground"
        );
        report
    }

    /// Resolves once the node asks to be restarted: it became a device of
    /// another person by pairing or recovery (see [`Node::relaunch`]).
    pub async fn restart_requested(&self) {
        self.state().restart_requested().await;
    }

    /// Stops the node and starts it again on the same configuration, at
    /// the same HTTP address and (device mode) with the same sign-in
    /// token: after pairing or recovery, which change who the device is.
    pub async fn relaunch(self) -> Result<Node, NodeError> {
        let mut config = self.config.clone();
        let addrs: Vec<String> = self.local_addrs().iter().map(ToString::to_string).collect();
        if !addrs.is_empty() {
            config.listen = addrs;
        }
        config.device_token = self
            .token()
            .map(|token| resonator_server::config::DeviceToken(token.to_owned()));
        let options = self.options.clone();
        self.shutdown().await;
        tracing::info!("starting again");
        launch_with(config, options).await
    }

    /// Person mode: pairs this device by a pairing code
    /// (`482913-ocean-tiger`) or a `resonator:pair/...` link (made by
    /// `pair_invite` on an admin device of the person). It is a device of
    /// that person after [`Node::relaunch`]. `wait` bounds the wait for
    /// the inviting device to confirm an admin pairing.
    pub async fn pair_join(
        &self,
        code_or_link: &str,
        name: &str,
        wait: Duration,
    ) -> Result<resonator_federation::control::JoinSummary, NodeError> {
        let federation = self
            .federation
            .as_ref()
            .ok_or_else(|| NodeError::Invalid("federation is off".to_owned()))?;
        federation
            .person()
            .join(code_or_link, name, wait)
            .await
            .map_err(|error| NodeError::Person(error.to_string()))
    }

    /// Person mode, an admin device: a new pairing invitation; its link
    /// and code (each lets a device in once: show them only to the
    /// person, never log them).
    pub async fn pair_invite(
        &self,
        admin: bool,
    ) -> Result<resonator_federation::pairing::NewInvitation, NodeError> {
        let federation = self
            .federation
            .as_ref()
            .ok_or_else(|| NodeError::Invalid("federation is off".to_owned()))?;
        federation
            .person()
            .invite(admin)
            .await
            .map_err(|error| NodeError::Person(error.to_string()))
    }

    /// Stops everything in order: federation, the agents, the HTTP and
    /// wave servers; the database is flushed and closed.
    pub async fn shutdown(self) {
        tracing::info!("shutting down");
        if let Some(federation) = self.federation {
            federation.shutdown().await;
        }
        self.integrations.shutdown().await;
        self.app.shutdown().await;
        tracing::info!("database closed, bye");
    }
}

/// Writes a consistent copy of the configured database to `destination`.
pub fn backup(config: &Config, destination: &Path) -> Result<(), NodeError> {
    Ok(app::backup(config, destination)?)
}
