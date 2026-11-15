//! The server process: start-up, readiness and clean shutdown. The binary
//! is a thin wrapper around this module.

use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use thiserror::Error;

use crate::config::{Config, ConfigError};
use crate::http::{AppState, HttpServer};
use crate::storage::{self, Database, SCHEMA_VERSION, StorageError};
use crate::waveserver::{WaveServer, WaveServerOptions};

/// Meta key recording the wave domain a database was created for.
pub const META_DOMAIN: &str = "wave_server_domain";

/// How often idle waves are evicted from memory.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// Failure to start or run the server.
#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(
        "database {path} belongs to wave domain '{stored}' but the configured domain is \
         '{configured}'"
    )]
    DomainMismatch {
        path: String,
        stored: String,
        configured: String,
    },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// A file in the data directory that lets the next start move a
/// database without waves to the configured domain: left by pairing and
/// recovery, which make the device another person's.
pub const REBIND_MARKER: &str = "domain-rebind";

/// Opens and migrates the database named by `config`, checks that it
/// belongs to the configured wave domain, and starts the wave server on
/// it. Must be called within a tokio runtime.
pub async fn start(config: &Config) -> Result<WaveServer, AppError> {
    start_with(config, WaveServerOptions::from_config(config)).await
}

/// [`start`] with the wave server options given rather than derived from
/// `config`, for instance with a delta signer.
pub async fn start_with(
    config: &Config,
    options: WaveServerOptions,
) -> Result<WaveServer, AppError> {
    config.validate()?;
    let path = config.database_path.clone();
    let database_options = config.database_options();
    let db = tokio::task::spawn_blocking(move || Database::open(path, database_options))
        .await
        .map_err(|_| StorageError::Closed)??;
    match db.meta(META_DOMAIN).await? {
        None => db.set_meta(META_DOMAIN, &config.domain).await?,
        Some(stored) if stored == config.domain => {}
        // A device paired or recovered as another person's: its
        // database, which holds no waves, moves to the person's domain.
        Some(stored)
            if config.data_directory().join(REBIND_MARKER).exists()
                && db.delta_store().delta_count().await? == 0 =>
        {
            tracing::info!(from = %stored, to = %config.domain, "the database moves to the person's domain");
            db.set_meta(META_DOMAIN, &config.domain).await?;
            let _ = std::fs::remove_file(config.data_directory().join(REBIND_MARKER));
        }
        Some(stored) => {
            db.close().await;
            return Err(AppError::DomainMismatch {
                path: config.database_path.display().to_string(),
                stored,
                configured: config.domain.clone(),
            });
        }
    }
    Ok(WaveServer::new(db, options))
}

/// Flushes every queued write and closes the database.
pub async fn shutdown(server: WaveServer) {
    if let Err(error) = server.flush().await {
        tracing::error!(%error, "flushing the database on shutdown failed");
    }
    server.database().close().await;
}

/// How often expired sessions are purged.
const SESSION_PURGE_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// How often the attachments directory is swept for files nothing owns.
const ATTACHMENT_SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// How often the write-ahead log is checkpointed explicitly.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Starts a task that checkpoints the write-ahead log every `interval`,
/// until the database is closed.
fn spawn_checkpoints(db: Database, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match db.checkpoint().await {
                Ok((log, checkpointed)) => {
                    tracing::debug!(log, checkpointed, "checkpointed the write-ahead log");
                }
                Err(error) => tracing::warn!(%error, "checkpoint failed"),
            }
        }
    })
}

/// The running server: the wave server and its HTTP front end.
pub struct App {
    server: WaveServer,
    http: HttpServer,
    maintenance: Vec<tokio::task::JoinHandle<()>>,
}

/// The server started up to the point where it would begin to listen:
/// the wave server, the services and the maintenance tasks. What is built
/// on top of the server (robots, agents) takes what it needs from
/// [`PreparedApp::state`] and adds its routes with [`PreparedApp::serve`].
pub struct PreparedApp {
    server: WaveServer,
    state: AppState,
    maintenance: Vec<tokio::task::JoinHandle<()>>,
}

impl PreparedApp {
    pub fn server(&self) -> &WaveServer {
        &self.server
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Has a task aborted when the server shuts down.
    pub fn add_task(&mut self, task: tokio::task::JoinHandle<()>) {
        self.maintenance.push(task);
    }

    /// Binds the configured listen addresses and starts serving the
    /// server's routes and the `extra` ones (see
    /// [`crate::http::router_with`]).
    pub async fn serve(self, extra: axum::Router<AppState>) -> Result<App, AppError> {
        let PreparedApp {
            server,
            state,
            maintenance,
        } = self;
        let http = match HttpServer::bind_with(state.clone(), extra).await {
            Ok(http) => http,
            Err(error) => {
                PreparedApp {
                    server,
                    state,
                    maintenance,
                }
                .abandon()
                .await;
                return Err(AppError::Io(error));
            }
        };
        let app = App {
            server,
            http,
            maintenance,
        };
        app.write_device_url();
        Ok(app)
    }

    /// Stops what was started, without ever having listened.
    pub async fn abandon(self) {
        for task in &self.maintenance {
            task.abort();
        }
        self.state.frontend.shutdown();
        self.state.search.shutdown();
        shutdown(self.server).await;
    }
}

impl App {
    /// Starts the wave server on the configured database and the HTTP
    /// server on the configured listen addresses.
    pub async fn start(config: &Config) -> Result<App, AppError> {
        Self::prepare(config)
            .await?
            .serve(crate::http::placeholder_routes())
            .await
    }

    /// Starts everything but the HTTP server.
    pub async fn prepare(config: &Config) -> Result<PreparedApp, AppError> {
        Self::prepare_with(config, WaveServerOptions::from_config(config)).await
    }

    /// [`App::prepare`] with the given wave server options.
    pub async fn prepare_with(
        config: &Config,
        options: WaveServerOptions,
    ) -> Result<PreparedApp, AppError> {
        let server = start_with(config, options).await?;
        let state = match AppState::new(config.clone(), server.clone()) {
            Ok(state) => state,
            Err(error) => {
                shutdown(server).await;
                return Err(AppError::Io(error));
            }
        };
        if let Some(participant) = state.device_account() {
            // Device mode: the one account, without a password; the
            // loopback token signs it in.
            let account = crate::account::HumanAccountData::new(participant.clone());
            match state.auth.accounts().create_account(account.into()).await {
                Ok(true) => tracing::info!(%participant, "created the device's account"),
                Ok(false) => {}
                Err(error) => {
                    state.frontend.shutdown();
                    state.search.shutdown();
                    shutdown(server).await;
                    return Err(AppError::Storage(error));
                }
            }
        }
        let mut maintenance = vec![
            server.spawn_maintenance(MAINTENANCE_INTERVAL),
            state.auth.spawn_maintenance(SESSION_PURGE_INTERVAL),
            state
                .attachments
                .spawn_maintenance(ATTACHMENT_SWEEP_INTERVAL),
            spawn_checkpoints(server.database().clone(), CHECKPOINT_INTERVAL),
        ];
        if let Some(welcome) = state.welcome.clone() {
            // Reserve the welcome robot's address before anyone registers.
            maintenance.push(tokio::spawn(async move {
                if let Err(error) = welcome.ensure_robot_account().await {
                    tracing::error!(%error, "welcome waves will not be created");
                }
            }));
        }
        Ok(PreparedApp {
            server,
            state,
            maintenance,
        })
    }

    pub fn server(&self) -> &WaveServer {
        &self.server
    }

    /// The state shared by the HTTP handlers: the authentication service,
    /// the client front end and the metrics.
    pub fn state(&self) -> &AppState {
        self.http.state()
    }

    /// The addresses the HTTP server is bound to.
    pub fn local_addrs(&self) -> &[SocketAddr] {
        self.http.local_addrs()
    }

    /// Device mode: the URL that opens the client signed in,
    /// `http://<loopback>:<port>/?token=<token>`. `None` in server mode.
    /// It carries the token: never log it.
    pub fn device_url(&self) -> Option<String> {
        let token = self.state().device_token()?;
        Some(format!("{}/?token={token}", self.local_url()?))
    }

    /// `http://<host>:<port>` of the first listen address, with a
    /// wildcard address replaced by loopback.
    pub fn local_url(&self) -> Option<String> {
        let address = *self.local_addrs().first()?;
        let ip = match address.ip() {
            std::net::IpAddr::V4(v4) if v4.is_unspecified() => {
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            }
            std::net::IpAddr::V6(v6) if v6.is_unspecified() => {
                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            }
            ip => ip,
        };
        Some(format!(
            "http://{}",
            std::net::SocketAddr::new(ip, address.port())
        ))
    }

    /// The file holding [`App::device_url`] while the server runs:
    /// `device.url` in the data directory (mode 600), for launchers on
    /// the same machine (a desktop shortcut, the end-to-end tests).
    pub fn device_url_path(&self) -> std::path::PathBuf {
        self.state().config.data_directory().join("device.url")
    }

    fn write_device_url(&self) {
        let Some(url) = self.device_url() else {
            return;
        };
        let path = self.device_url_path();
        if let Err(error) = write_private(&path, &url) {
            tracing::warn!(path = %path.display(), %error, "cannot write the device URL file");
        } else {
            tracing::info!(path = %path.display(), "device mode: the sign-in URL is in this file");
        }
    }

    /// Closes the connections, flushes every queued write and closes the
    /// database.
    pub async fn shutdown(self) {
        if self.state().device_token().is_some() {
            let _ = std::fs::remove_file(self.device_url_path());
        }
        for task in &self.maintenance {
            task.abort();
        }
        #[cfg(feature = "apps")]
        if let Some(apps) = &self.state().apps {
            apps.shutdown();
        }
        self.http.shutdown().await;
        shutdown(self.server).await;
    }
}

/// Runs the server until `stop` resolves, then shuts down cleanly.
pub async fn serve_until(config: Config, stop: impl Future<Output = ()>) -> Result<(), AppError> {
    let app = App::start(&config).await?;
    let waves = crate::waveserver::WaveletProvider::get_wave_ids(app.server())
        .await
        .map(|ids| ids.len())
        .unwrap_or(0);
    tracing::info!(
        domain = %config.domain,
        database = %config.database_path.display(),
        schema_version = SCHEMA_VERSION,
        synchronous = %config.synchronous,
        waves,
        attachments = %config.attachments_directory().display(),
        listen = ?app.local_addrs(),
        "resonator wave server ready"
    );
    stop.await;
    tracing::info!("shutting down");
    app.shutdown().await;
    tracing::info!("database closed, bye");
    Ok(())
}

/// Runs the server until Ctrl-C.
pub async fn serve(config: Config) -> Result<(), AppError> {
    serve_until(config, async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "cannot listen for Ctrl-C; stopping");
        }
    })
    .await
}

/// Writes `text` to `path`, readable by the owner only (replacing the
/// file).
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// Writes a consistent copy of the configured database to `destination`.
/// Safe while a server is running on the database. Blocking.
pub fn backup(config: &Config, destination: &Path) -> Result<(), AppError> {
    if !config.database_path.exists() {
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("database {} does not exist", config.database_path.display()),
        )));
    }
    storage::backup(&config.database_path, destination)?;
    Ok(())
}
