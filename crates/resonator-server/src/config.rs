//! Server configuration: built-in defaults, overridden by one TOML file,
//! overridden by `RESONATOR_*` environment variables, overridden by command
//! line flags.
//!
//! Covers the settings of Apache Wave's `reference.conf` that still apply
//! (the wave domain, listen addresses, cache bounds, registration, admin
//! user, welcome wave and WebSocket limits) plus the storage, session and
//! rate-limit settings of this server.
//!
//! TLS SEAM: the server speaks plain HTTP; put a TLS-terminating reverse
//! proxy in front and list it in `trusted_proxies`. Native TLS
//! (`security.enable_ssl` in the original) would add certificate settings
//! here and wrap the listeners in `http::serve`.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use resonator_model::id::{ParticipantId, is_valid_domain};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::storage::{DatabaseOptions, Synchronous};

mod apps;
mod device;
mod federation;
mod integrations;
mod person;
mod push;
mod webrtc;

pub use apps::AppsConfig;
pub use device::{DeviceConfig, Mode};
pub use federation::{
    DEFAULT_PAIRING_RENDEZVOUS, FederationConfig, FederationMode, FederationPeer, RelaySetting,
};
pub use person::PersonConfig;
pub use push::PushConfig;
pub use webrtc::WebRtcConfig;

pub use integrations::{
    AgentTools, AiAgentConfig, AiConfig, AiProviderConfig, ExtraParams, REDACTED, Secret,
    is_sensitive_header, is_valid_robot_id,
};

/// The environment variable prefix of every setting.
pub const ENV_PREFIX: &str = "RESONATOR_";

/// The config file read when none is named, if it exists.
pub const DEFAULT_CONFIG_FILE: &str = "resonator.toml";

/// A configuration that cannot be loaded or is invalid.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// The server configuration. Every field has a default, so an empty file
/// (or none) is valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Domain name of the wave server; wavelets of this domain are hosted
    /// here (`core.wave_server_domain`).
    pub domain: String,
    /// `server` (the default: accounts of many people on one domain) or
    /// `device` (one person's device; see `[device]`).
    #[serde(skip_serializing_if = "Mode::is_server")]
    pub mode: Mode,
    /// `host:port` addresses to listen on for HTTP and WebSocket
    /// connections (`core.http_frontend_addresses`).
    pub listen: Vec<String>,
    /// The `host:port` clients are told to connect to; empty for the first
    /// listen address (`core.http_frontend_public_address`).
    pub public_address: String,
    /// Path of the SQLite database file.
    pub database_path: PathBuf,
    /// SQLite `synchronous` mode: `off`, `normal`, `full` or `extra`.
    pub synchronous: Synchronous,
    /// Number of read-only database connections.
    pub read_connections: usize,
    /// A wavelet snapshot is stored every this many deltas; 0 disables
    /// snapshots.
    pub snapshot_interval: u64,
    /// Number of recent deltas each loaded wavelet keeps in memory for
    /// transforming stale submissions without reading the database.
    pub delta_cache_size: usize,
    /// Number of waves kept in memory (`core.wave_cache_size`).
    pub wave_cache_size: usize,
    /// Seconds an unused wave stays in memory (`core.wave_cache_expire`).
    pub wave_cache_expire_secs: u64,
    /// Whether anyone may register an account
    /// (the inverse of `administration.disable_registration`).
    pub registration_enabled: bool,
    /// Address of the admin user. The default `@` is not a registrable
    /// address, so there is no admin (`administration.admin_user`).
    pub admin_user: String,
    /// Id, without domain, of the wave copied into every new user's inbox;
    /// empty for none (`administration.welcome_wave_id`).
    pub welcome_wave_id: String,
    /// Domains of other servers where people may register instead
    /// (`other_servers = ["resonator.network"]`): the registration screen
    /// links to `https://<domain>/` for each. Empty (the default) for none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_servers: Vec<String>,
    /// Seconds a session lives without being used; every use renews it.
    pub session_lifetime_secs: u64,
    /// Seconds after sign-in at which a session ends however much it is
    /// used; 0 for no such limit.
    pub session_absolute_lifetime_secs: u64,
    /// Whether the session cookie gets the `Secure` attribute: `auto`
    /// (when the request arrived over HTTPS according to a trusted proxy),
    /// `always` or `never`.
    pub cookie_secure: CookieSecure,
    /// Addresses or CIDR blocks of reverse proxies whose `X-Forwarded-For`,
    /// `X-Forwarded-Proto` and `X-Forwarded-Host` headers are believed.
    pub trusted_proxies: Vec<String>,
    /// Origins (`scheme://host[:port]`) besides the server's own that may
    /// make state-changing requests and open the WebSocket with cookies.
    pub allowed_origins: Vec<String>,
    /// Shortest password accepted at registration (the original accepts
    /// any, including none).
    pub password_min_length: usize,
    /// Failed sign-ins per account and client address before further
    /// attempts are delayed; 0 disables sign-in rate limiting.
    pub login_max_failures: u32,
    /// First delay in seconds once `login_max_failures` is reached; it
    /// doubles with every further failure.
    pub login_backoff_base_secs: u64,
    /// Longest sign-in delay in seconds; failures older than twice this
    /// are forgotten.
    pub login_backoff_max_secs: u64,
    /// Registrations accepted per client address per hour; 0 for no limit.
    pub register_max_per_hour: u32,
    /// Largest HTTP request body in bytes.
    pub http_max_body_size: usize,
    /// Largest WebSocket message accepted, in bytes
    /// (`network.websocket_max_message_size`, there in megabytes).
    pub websocket_max_message_size: usize,
    /// Messages that may wait to be written to one WebSocket; a client that
    /// falls further behind is disconnected.
    pub websocket_send_queue: usize,
    /// Bytes (roughly) that may wait to be written to one WebSocket before
    /// the client is disconnected. A single message counts for at most
    /// half of this, so that large snapshots can still be sent.
    pub websocket_send_queue_bytes: usize,
    /// Seconds between WebSocket pings.
    pub websocket_ping_interval_secs: u64,
    /// Seconds without any frame from the client (pongs count) after which
    /// a WebSocket is closed (`network.websocket_max_idle_time`); 0 for
    /// never.
    pub websocket_idle_timeout_secs: u64,
    /// Wave views one WebSocket may have open at a time.
    pub websocket_max_subscriptions: usize,
    /// Whether signed-in users can list the accounts of this server
    /// (`/directory`), which is what suggests people to add to a wave.
    pub directory_enabled: bool,
    /// Whether `/metrics` is served.
    pub metrics_enabled: bool,
    /// If not empty, `/metrics` requires `Authorization: Bearer` with this
    /// token.
    pub metrics_token: String,
    /// Directory for the server's files other than the database; empty for
    /// the directory of the database file.
    pub data_dir: PathBuf,
    /// Directory of the attachment files
    /// (`core.attachment_store_directory`); empty for `attachments` in the
    /// data directory.
    pub attachments_dir: PathBuf,
    /// Largest attachment accepted, in bytes.
    pub attachment_max_size: u64,
    /// Whether waves are indexed for free-text search. Without it, queries
    /// with free-text terms find nothing.
    pub search_fulltext_enabled: bool,
    /// Milliseconds a changed wavelet waits before its text is indexed
    /// again; changes within that time are indexed together.
    pub search_index_debounce_ms: u64,
    /// Number of wave digests kept in memory for search results.
    pub search_digest_cache_size: usize,
    /// Most results one search request may ask for.
    pub search_max_results: usize,
    /// Whether a search starting with `SELECT ` runs as SQL over the
    /// user's own data (sessions, and tokens with the `sql` scope).
    pub search_sql_enabled: bool,
    /// Most bytes of attachments one user may have uploaded; 0 for no
    /// limit.
    pub attachment_user_quota: u64,
    /// Whether the robot endpoints are served: the Data API
    /// (`/robot/dataapi`), the active robot API (`/robot/rpc`), robot
    /// registration (`/robot/register`) and the robot event stream
    /// (`/robot/events`).
    pub robot_api_enabled: bool,
    /// Whether any signed-in user may register robots. If not, only the
    /// admin may.
    pub robot_registration_enabled: bool,
    /// Most robots one user may have registered.
    pub robot_max_per_user: usize,
    /// Whether robots may have URLs that resolve to loopback, link-local,
    /// private, carrier-grade NAT (which includes tailnets) or otherwise
    /// non-public addresses. Off by default: a robot URL is supplied by a
    /// user and the server calls it, which must not become a way into the
    /// server's own network.
    pub robot_allow_private_addresses: bool,
    /// Exceptions to the rule above: addresses or CIDR blocks a robot URL
    /// may resolve to, and host names (exact, or `*.suffix`) whose
    /// addresses are accepted whatever they are.
    pub robot_allowed_addresses: Vec<String>,
    /// Milliseconds to wait for the connection to a robot.
    pub robot_connect_timeout_ms: u64,
    /// Seconds a robot may take to answer a call.
    pub robot_request_timeout_secs: u64,
    /// Largest response accepted from a robot, in bytes.
    pub robot_max_response_bytes: usize,
    /// The loop guard: events caused by a chain of more than this many
    /// robot or agent reactions are not delivered.
    pub robot_max_hops: u32,
    /// Event bundles that may wait for one robot listening on
    /// `/robot/events`; the oldest are dropped beyond that.
    pub robot_event_stream_buffer: usize,
    /// Whether the Model Context Protocol endpoint `/mcp` is served.
    pub mcp_enabled: bool,
    /// Most characters one MCP tool result may have; longer results are
    /// cut with a marker.
    pub mcp_max_result_chars: usize,
    /// Model providers and built-in agents.
    pub ai: AiConfig,
    /// Server-to-server federation; off by default.
    pub federation: FederationConfig,
    /// Person mode (this node is one device of a person); off by default.
    pub person: PersonConfig,
    /// Device mode settings (`mode = "device"`).
    #[serde(skip_serializing_if = "DeviceConfig::is_default")]
    pub device: DeviceConfig,
    /// Waking sleeping devices through a push relay (`[push]`); off
    /// while `push.relay` is empty.
    #[serde(skip_serializing_if = "PushConfig::is_default")]
    pub push: PushConfig,
    /// Wave apps (`[apps]`); off by default, on in device mode's defaults.
    #[serde(skip_serializing_if = "AppsConfig::is_default")]
    pub apps: AppsConfig,
    /// App media over WebRTC (`[webrtc]`); off by default.
    #[serde(skip_serializing_if = "WebRtcConfig::is_default")]
    pub webrtc: WebRtcConfig,
    /// Device mode: the sign-in token to keep when the node restarts in
    /// the same process (after pairing or recovery); never in a file.
    #[serde(skip)]
    pub device_token: Option<DeviceToken>,
}

/// A device sign-in token kept across a restart. Its `Debug` form hides
/// it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken(pub String);

impl std::fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceToken(<redacted>)")
    }
}

/// When the session cookie is marked `Secure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CookieSecure {
    /// When a trusted proxy says the request arrived over HTTPS.
    #[default]
    Auto,
    Always,
    Never,
}

impl std::str::FromStr for CookieSecure {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(CookieSecure::Auto),
            "always" => Ok(CookieSecure::Always),
            "never" => Ok(CookieSecure::Never),
            _ => Err(format!(
                "invalid cookie_secure '{s}' (expected auto, always or never)"
            )),
        }
    }
}

/// An address or CIDR block, as listed in `trusted_proxies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    network: IpAddr,
    prefix: u8,
}

impl IpNet {
    pub fn contains(&self, address: IpAddr) -> bool {
        fn bits(address: IpAddr) -> (u128, u8) {
            match address {
                IpAddr::V4(v4) => (u128::from(u32::from(v4)) << 96, 32),
                IpAddr::V6(v6) => (u128::from(v6), 128),
            }
        }
        // An IPv4 peer may show up as an IPv4-mapped IPv6 address.
        let address = match address {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
            v4 => v4,
        };
        let (network, width) = bits(self.network);
        let (address, address_width) = bits(address);
        if width != address_width {
            return false;
        }
        let mask = match self.prefix {
            0 => 0,
            prefix => u128::MAX << (128 - u32::from(prefix)),
        };
        network & mask == address & mask
    }
}

impl std::str::FromStr for IpNet {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let error = || format!("'{s}' is not an IP address or CIDR block");
        let (address, prefix) = match s.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (s, None),
        };
        let network: IpAddr = address.trim().parse().map_err(|_| error())?;
        let width = if network.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(prefix) => prefix.trim().parse::<u8>().map_err(|_| error())?,
            None => width,
        };
        if prefix > width {
            return Err(error());
        }
        Ok(IpNet { network, prefix })
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            domain: "local.net".to_owned(),
            mode: Mode::Server,
            listen: vec!["0.0.0.0:9898".to_owned()],
            public_address: String::new(),
            database_path: PathBuf::from("resonator.db"),
            synchronous: Synchronous::Full,
            read_connections: 4,
            snapshot_interval: 100,
            delta_cache_size: 256,
            wave_cache_size: 1000,
            wave_cache_expire_secs: 60 * 60,
            registration_enabled: true,
            admin_user: "@".to_owned(),
            welcome_wave_id: String::new(),
            other_servers: Vec::new(),
            session_lifetime_secs: 14 * 24 * 60 * 60,
            session_absolute_lifetime_secs: 90 * 24 * 60 * 60,
            cookie_secure: CookieSecure::Auto,
            trusted_proxies: Vec::new(),
            allowed_origins: Vec::new(),
            password_min_length: 8,
            login_max_failures: 5,
            login_backoff_base_secs: 5,
            login_backoff_max_secs: 15 * 60,
            register_max_per_hour: 60,
            http_max_body_size: 1024 * 1024,
            websocket_max_message_size: 2 * 1024 * 1024,
            websocket_send_queue: 1024,
            websocket_send_queue_bytes: 16 * 1024 * 1024,
            websocket_ping_interval_secs: 30,
            websocket_idle_timeout_secs: 90,
            websocket_max_subscriptions: 64,
            directory_enabled: true,
            metrics_enabled: true,
            metrics_token: String::new(),
            data_dir: PathBuf::new(),
            attachments_dir: PathBuf::new(),
            attachment_max_size: 25 * 1024 * 1024,
            search_fulltext_enabled: true,
            search_index_debounce_ms: 500,
            search_digest_cache_size: 10_000,
            search_max_results: 1000,
            search_sql_enabled: true,
            attachment_user_quota: 1024 * 1024 * 1024,
            robot_api_enabled: true,
            robot_registration_enabled: true,
            robot_max_per_user: 20,
            robot_allow_private_addresses: false,
            robot_allowed_addresses: Vec::new(),
            robot_connect_timeout_ms: 5000,
            robot_request_timeout_secs: 20,
            robot_max_response_bytes: 1024 * 1024,
            robot_max_hops: 4,
            robot_event_stream_buffer: 256,
            mcp_enabled: true,
            mcp_max_result_chars: 20_000,
            ai: AiConfig::default(),
            federation: FederationConfig::default(),
            person: PersonConfig::default(),
            device: DeviceConfig::default(),
            push: PushConfig::default(),
            apps: AppsConfig::default(),
            webrtc: WebRtcConfig::default(),
            device_token: None,
        }
    }
}

/// Settings given on the command line or in the environment; each one set
/// replaces the value from the config file. Flattened into the binary's
/// command line.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct Overrides {
    /// Domain name of the wave server.
    #[arg(long, env = "RESONATOR_DOMAIN", value_name = "DOMAIN")]
    pub domain: Option<String>,
    /// Addresses to listen on (host:port), comma-separated or repeated.
    #[arg(
        long,
        env = "RESONATOR_LISTEN",
        value_name = "HOST:PORT",
        value_delimiter = ','
    )]
    pub listen: Option<Vec<String>>,
    /// The host:port clients are told to connect to.
    #[arg(long, env = "RESONATOR_PUBLIC_ADDRESS", value_name = "HOST:PORT")]
    pub public_address: Option<String>,
    /// Path of the SQLite database file.
    #[arg(long, env = "RESONATOR_DATABASE_PATH", value_name = "PATH")]
    pub database_path: Option<PathBuf>,
    /// SQLite synchronous mode: off, normal, full or extra.
    #[arg(long, env = "RESONATOR_SYNCHRONOUS", value_name = "MODE")]
    pub synchronous: Option<Synchronous>,
    /// Number of read-only database connections.
    #[arg(long, env = "RESONATOR_READ_CONNECTIONS", value_name = "N")]
    pub read_connections: Option<usize>,
    /// Store a wavelet snapshot every N deltas (0 disables).
    #[arg(long, env = "RESONATOR_SNAPSHOT_INTERVAL", value_name = "N")]
    pub snapshot_interval: Option<u64>,
    /// Recent deltas kept in memory per loaded wavelet.
    #[arg(long, env = "RESONATOR_DELTA_CACHE_SIZE", value_name = "N")]
    pub delta_cache_size: Option<usize>,
    /// Number of waves kept in memory.
    #[arg(long, env = "RESONATOR_WAVE_CACHE_SIZE", value_name = "N")]
    pub wave_cache_size: Option<usize>,
    /// Seconds an unused wave stays in memory.
    #[arg(long, env = "RESONATOR_WAVE_CACHE_EXPIRE_SECS", value_name = "SECONDS")]
    pub wave_cache_expire_secs: Option<u64>,
    /// Whether anyone may register an account.
    #[arg(long, env = "RESONATOR_REGISTRATION_ENABLED", value_name = "BOOL")]
    pub registration_enabled: Option<bool>,
    /// Address of the admin user.
    #[arg(long, env = "RESONATOR_ADMIN_USER", value_name = "ADDRESS")]
    pub admin_user: Option<String>,
    /// Id (without domain) of the welcome wave.
    #[arg(long, env = "RESONATOR_WELCOME_WAVE_ID", value_name = "ID")]
    pub welcome_wave_id: Option<String>,
    /// Seconds an unused session lives.
    #[arg(long, env = "RESONATOR_SESSION_LIFETIME_SECS", value_name = "SECONDS")]
    pub session_lifetime_secs: Option<u64>,
    /// Seconds after sign-in at which a session ends regardless (0: never).
    #[arg(
        long,
        env = "RESONATOR_SESSION_ABSOLUTE_LIFETIME_SECS",
        value_name = "SECONDS"
    )]
    pub session_absolute_lifetime_secs: Option<u64>,
    /// Mark the session cookie Secure: auto, always or never.
    #[arg(long, env = "RESONATOR_COOKIE_SECURE", value_name = "MODE")]
    pub cookie_secure: Option<CookieSecure>,
    /// Reverse proxies (address or CIDR) whose X-Forwarded-* headers are
    /// believed, comma-separated or repeated.
    #[arg(
        long,
        env = "RESONATOR_TRUSTED_PROXIES",
        value_name = "CIDR",
        value_delimiter = ','
    )]
    pub trusted_proxies: Option<Vec<String>>,
    /// Additional origins allowed to make state-changing requests,
    /// comma-separated or repeated.
    #[arg(
        long,
        env = "RESONATOR_ALLOWED_ORIGINS",
        value_name = "ORIGIN",
        value_delimiter = ','
    )]
    pub allowed_origins: Option<Vec<String>>,
    /// Shortest password accepted at registration.
    #[arg(long, env = "RESONATOR_PASSWORD_MIN_LENGTH", value_name = "N")]
    pub password_min_length: Option<usize>,
    /// Failed sign-ins before further attempts are delayed (0: no limit).
    #[arg(long, env = "RESONATOR_LOGIN_MAX_FAILURES", value_name = "N")]
    pub login_max_failures: Option<u32>,
    /// First sign-in delay in seconds; doubles with each further failure.
    #[arg(
        long,
        env = "RESONATOR_LOGIN_BACKOFF_BASE_SECS",
        value_name = "SECONDS"
    )]
    pub login_backoff_base_secs: Option<u64>,
    /// Longest sign-in delay in seconds.
    #[arg(long, env = "RESONATOR_LOGIN_BACKOFF_MAX_SECS", value_name = "SECONDS")]
    pub login_backoff_max_secs: Option<u64>,
    /// Registrations per client address per hour (0: no limit).
    #[arg(long, env = "RESONATOR_REGISTER_MAX_PER_HOUR", value_name = "N")]
    pub register_max_per_hour: Option<u32>,
    /// Largest HTTP request body in bytes.
    #[arg(long, env = "RESONATOR_HTTP_MAX_BODY_SIZE", value_name = "BYTES")]
    pub http_max_body_size: Option<usize>,
    /// Largest WebSocket message in bytes.
    #[arg(
        long,
        env = "RESONATOR_WEBSOCKET_MAX_MESSAGE_SIZE",
        value_name = "BYTES"
    )]
    pub websocket_max_message_size: Option<usize>,
    /// Messages that may wait to be written to one WebSocket.
    #[arg(long, env = "RESONATOR_WEBSOCKET_SEND_QUEUE", value_name = "N")]
    pub websocket_send_queue: Option<usize>,
    /// Bytes that may wait to be written to one WebSocket.
    #[arg(
        long,
        env = "RESONATOR_WEBSOCKET_SEND_QUEUE_BYTES",
        value_name = "BYTES"
    )]
    pub websocket_send_queue_bytes: Option<usize>,
    /// Seconds between WebSocket pings.
    #[arg(
        long,
        env = "RESONATOR_WEBSOCKET_PING_INTERVAL_SECS",
        value_name = "SECONDS"
    )]
    pub websocket_ping_interval_secs: Option<u64>,
    /// Seconds of client silence after which a WebSocket is closed (0:
    /// never).
    #[arg(
        long,
        env = "RESONATOR_WEBSOCKET_IDLE_TIMEOUT_SECS",
        value_name = "SECONDS"
    )]
    pub websocket_idle_timeout_secs: Option<u64>,
    /// Wave views one WebSocket may have open.
    #[arg(long, env = "RESONATOR_WEBSOCKET_MAX_SUBSCRIPTIONS", value_name = "N")]
    pub websocket_max_subscriptions: Option<usize>,
    /// Whether signed-in users can list this server's accounts.
    #[arg(long, env = "RESONATOR_DIRECTORY_ENABLED", value_name = "BOOL")]
    pub directory_enabled: Option<bool>,
    /// Whether /metrics is served.
    #[arg(long, env = "RESONATOR_METRICS_ENABLED", value_name = "BOOL")]
    pub metrics_enabled: Option<bool>,
    /// Bearer token required for /metrics (empty: none).
    #[arg(long, env = "RESONATOR_METRICS_TOKEN", value_name = "TOKEN")]
    pub metrics_token: Option<String>,
    /// Directory for files other than the database (empty: beside it).
    #[arg(long, env = "RESONATOR_DATA_DIR", value_name = "PATH")]
    pub data_dir: Option<PathBuf>,
    /// Directory of the attachment files (empty: attachments in the data
    /// directory).
    #[arg(long, env = "RESONATOR_ATTACHMENTS_DIR", value_name = "PATH")]
    pub attachments_dir: Option<PathBuf>,
    /// Largest attachment accepted, in bytes.
    #[arg(long, env = "RESONATOR_ATTACHMENT_MAX_SIZE", value_name = "BYTES")]
    pub attachment_max_size: Option<u64>,
    /// Whether waves are indexed for free-text search.
    #[arg(long, env = "RESONATOR_SEARCH_FULLTEXT_ENABLED", value_name = "BOOL")]
    pub search_fulltext_enabled: Option<bool>,
    /// Milliseconds a changed wavelet waits before its text is indexed.
    #[arg(
        long,
        env = "RESONATOR_SEARCH_INDEX_DEBOUNCE_MS",
        value_name = "MILLISECONDS"
    )]
    pub search_index_debounce_ms: Option<u64>,
    /// Wave digests kept in memory for search results.
    #[arg(long, env = "RESONATOR_SEARCH_DIGEST_CACHE_SIZE", value_name = "N")]
    pub search_digest_cache_size: Option<usize>,
    /// Most results one search request may ask for.
    #[arg(long, env = "RESONATOR_SEARCH_MAX_RESULTS", value_name = "N")]
    pub search_max_results: Option<usize>,
    /// Whether searches starting with `SELECT ` run as SQL.
    #[arg(long, env = "RESONATOR_SEARCH_SQL_ENABLED", value_name = "BOOL")]
    pub search_sql_enabled: Option<bool>,
    /// Most bytes of attachments one user may have uploaded (0: no limit).
    #[arg(long, env = "RESONATOR_ATTACHMENT_USER_QUOTA", value_name = "BYTES")]
    pub attachment_user_quota: Option<u64>,
    /// Whether the robot endpoints (/robot/...) are served.
    #[arg(long, env = "RESONATOR_ROBOT_API_ENABLED", value_name = "BOOL")]
    pub robot_api_enabled: Option<bool>,
    /// Whether any signed-in user may register robots.
    #[arg(
        long,
        env = "RESONATOR_ROBOT_REGISTRATION_ENABLED",
        value_name = "BOOL"
    )]
    pub robot_registration_enabled: Option<bool>,
    /// Most robots one user may have registered.
    #[arg(long, env = "RESONATOR_ROBOT_MAX_PER_USER", value_name = "N")]
    pub robot_max_per_user: Option<usize>,
    /// Whether robot URLs may resolve to non-public addresses.
    #[arg(
        long,
        env = "RESONATOR_ROBOT_ALLOW_PRIVATE_ADDRESSES",
        value_name = "BOOL"
    )]
    pub robot_allow_private_addresses: Option<bool>,
    /// Addresses, CIDR blocks or host names robot URLs may use although
    /// they are not public, comma-separated or repeated.
    #[arg(
        long,
        env = "RESONATOR_ROBOT_ALLOWED_ADDRESSES",
        value_name = "CIDR|HOST",
        value_delimiter = ','
    )]
    pub robot_allowed_addresses: Option<Vec<String>>,
    /// Milliseconds to wait for the connection to a robot.
    #[arg(
        long,
        env = "RESONATOR_ROBOT_CONNECT_TIMEOUT_MS",
        value_name = "MILLISECONDS"
    )]
    pub robot_connect_timeout_ms: Option<u64>,
    /// Seconds a robot may take to answer a call.
    #[arg(
        long,
        env = "RESONATOR_ROBOT_REQUEST_TIMEOUT_SECS",
        value_name = "SECONDS"
    )]
    pub robot_request_timeout_secs: Option<u64>,
    /// Largest response accepted from a robot, in bytes.
    #[arg(long, env = "RESONATOR_ROBOT_MAX_RESPONSE_BYTES", value_name = "BYTES")]
    pub robot_max_response_bytes: Option<usize>,
    /// Longest chain of robot reactions whose events are still delivered.
    #[arg(long, env = "RESONATOR_ROBOT_MAX_HOPS", value_name = "N")]
    pub robot_max_hops: Option<u32>,
    /// Event bundles that may wait for one robot on /robot/events.
    #[arg(long, env = "RESONATOR_ROBOT_EVENT_STREAM_BUFFER", value_name = "N")]
    pub robot_event_stream_buffer: Option<usize>,
    /// Whether the MCP endpoint /mcp is served.
    #[arg(long, env = "RESONATOR_MCP_ENABLED", value_name = "BOOL")]
    pub mcp_enabled: Option<bool>,
    /// Most characters of one MCP tool result.
    #[arg(long, env = "RESONATOR_MCP_MAX_RESULT_CHARS", value_name = "N")]
    pub mcp_max_result_chars: Option<usize>,
    /// Whether the configured agents run ([ai] enabled).
    #[arg(long, env = "RESONATOR_AI_ENABLED", value_name = "BOOL")]
    pub ai_enabled: Option<bool>,
    /// Whether wave apps run here ([apps] enabled).
    #[arg(long, env = "RESONATOR_APPS_ENABLED", value_name = "BOOL")]
    pub apps_enabled: Option<bool>,
    /// Whether app media may go over WebRTC ([webrtc] enabled).
    #[arg(long, env = "RESONATOR_WEBRTC_ENABLED", value_name = "BOOL")]
    pub webrtc_enabled: Option<bool>,
    /// The UDP address for WebRTC ([webrtc] listen).
    #[arg(long, env = "RESONATOR_WEBRTC_LISTEN", value_name = "ADDR")]
    pub webrtc_listen: Option<std::net::SocketAddr>,
    /// The address browsers send WebRTC to ([webrtc] advertise).
    #[arg(long, env = "RESONATOR_WEBRTC_ADVERTISE", value_name = "ADDR")]
    pub webrtc_advertise: Option<std::net::SocketAddr>,
    /// Whether this server federates with others ([federation] enabled).
    #[arg(long, env = "RESONATOR_FEDERATION_ENABLED", value_name = "BOOL")]
    pub federation_enabled: Option<bool>,
    /// Which servers may federate: open (the default) or allowlist.
    #[arg(long, env = "RESONATOR_FEDERATION_MODE", value_name = "MODE")]
    pub federation_mode: Option<FederationMode>,
    /// Domains or key domains allowed to federate, comma-separated or
    /// repeated.
    #[arg(
        long,
        env = "RESONATOR_FEDERATION_ALLOW",
        value_name = "DOMAIN",
        value_delimiter = ','
    )]
    pub federation_allow: Option<Vec<String>>,
    /// Relay for federation: default, off or a URL.
    #[arg(long, env = "RESONATOR_FEDERATION_RELAY", value_name = "RELAY")]
    pub federation_relay: Option<String>,
    /// Address lookup for federation: default or off.
    #[arg(long, env = "RESONATOR_FEDERATION_DISCOVERY", value_name = "MODE")]
    pub federation_discovery: Option<String>,
    /// UDP host:port or port of the federation endpoint.
    #[arg(long, env = "RESONATOR_FEDERATION_LISTEN", value_name = "ADDR")]
    pub federation_listen: Option<String>,
    /// server (many accounts) or device (one person's device).
    #[arg(long, env = "RESONATOR_MODE", value_name = "MODE")]
    pub mode: Option<Mode>,
    /// Device mode: the name part of the one account.
    #[arg(long, env = "RESONATOR_DEVICE_ACCOUNT", value_name = "NAME")]
    pub device_account: Option<String>,
    /// Device mode: whether other machines are served (with sign-in).
    #[arg(long, env = "RESONATOR_DEVICE_ALLOW_REMOTE", value_name = "BOOL")]
    pub device_allow_remote: Option<bool>,
    /// The push relay ([push] relay) that wakes sleeping devices.
    #[arg(long, env = "RESONATOR_PUSH_RELAY", value_name = "URL")]
    pub push_relay: Option<String>,
}

impl Config {
    /// Parses a TOML document; absent keys keep their defaults.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Loads the configuration: `file` if given (it must exist), otherwise
    /// [`DEFAULT_CONFIG_FILE`] if present, otherwise the defaults; then
    /// applies `overrides` and validates.
    pub fn load(file: Option<&Path>, overrides: &Overrides) -> Result<Self, ConfigError> {
        let path = match file {
            Some(path) => Some(path.to_path_buf()),
            None => Some(PathBuf::from(DEFAULT_CONFIG_FILE)).filter(|path| path.exists()),
        };
        let mut config = match path {
            Some(path) => {
                let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
                    path: path.clone(),
                    source,
                })?;
                Self::from_toml(&text).map_err(|source| ConfigError::Parse {
                    path,
                    source: Box::new(source),
                })?
            }
            None => Config::default(),
        };
        config.apply(overrides);
        config.validate()?;
        Ok(config)
    }

    /// Replaces every setting that `overrides` sets.
    pub fn apply(&mut self, overrides: &Overrides) {
        macro_rules! take {
            ($($field:ident),*) => {
                $(if let Some(value) = &overrides.$field {
                    self.$field = value.clone();
                })*
            };
        }
        take!(
            domain,
            listen,
            public_address,
            database_path,
            synchronous,
            read_connections,
            snapshot_interval,
            delta_cache_size,
            wave_cache_size,
            wave_cache_expire_secs,
            registration_enabled,
            admin_user,
            welcome_wave_id,
            session_lifetime_secs,
            session_absolute_lifetime_secs,
            cookie_secure,
            trusted_proxies,
            allowed_origins,
            password_min_length,
            login_max_failures,
            login_backoff_base_secs,
            login_backoff_max_secs,
            register_max_per_hour,
            http_max_body_size,
            websocket_max_message_size,
            websocket_send_queue,
            websocket_send_queue_bytes,
            websocket_ping_interval_secs,
            websocket_idle_timeout_secs,
            websocket_max_subscriptions,
            directory_enabled,
            metrics_enabled,
            metrics_token,
            data_dir,
            attachments_dir,
            attachment_max_size,
            search_fulltext_enabled,
            search_index_debounce_ms,
            search_digest_cache_size,
            search_max_results,
            search_sql_enabled,
            attachment_user_quota,
            robot_api_enabled,
            robot_registration_enabled,
            robot_max_per_user,
            robot_allow_private_addresses,
            robot_allowed_addresses,
            robot_connect_timeout_ms,
            robot_request_timeout_secs,
            robot_max_response_bytes,
            robot_max_hops,
            robot_event_stream_buffer,
            mcp_enabled,
            mcp_max_result_chars
        );
        if let Some(enabled) = overrides.ai_enabled {
            self.ai.enabled = enabled;
        }
        let federation = &mut self.federation;
        if let Some(enabled) = overrides.apps_enabled {
            self.apps.enabled = enabled;
        }
        if let Some(enabled) = overrides.webrtc_enabled {
            self.webrtc.enabled = enabled;
        }
        if let Some(listen) = overrides.webrtc_listen {
            self.webrtc.listen = listen;
        }
        if let Some(advertise) = overrides.webrtc_advertise {
            self.webrtc.advertise = Some(advertise);
        }
        if let Some(enabled) = overrides.federation_enabled {
            federation.enabled = enabled;
        }
        if let Some(mode) = overrides.federation_mode {
            federation.mode = mode;
        }
        if let Some(allow) = &overrides.federation_allow {
            federation.allow = allow.clone();
        }
        if let Some(relay) = &overrides.federation_relay {
            federation.relay = relay.clone();
        }
        if let Some(discovery) = &overrides.federation_discovery {
            federation.discovery = discovery.clone();
        }
        if let Some(listen) = &overrides.federation_listen {
            federation.listen = listen.clone();
        }
        if let Some(mode) = overrides.mode {
            self.mode = mode;
        }
        if let Some(account) = &overrides.device_account {
            self.device.account = account.clone();
        }
        if let Some(allow) = overrides.device_allow_remote {
            self.device.allow_remote = allow;
        }
        if let Some(relay) = &overrides.push_relay {
            self.push.relay = relay.clone();
        }
    }

    /// Checks the settings that have constraints.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |message: String| Err(ConfigError::Invalid(message));
        if !is_valid_domain(0, &self.domain) {
            return invalid(format!(
                "domain '{}' is not a valid domain name",
                self.domain
            ));
        }
        if self.listen.is_empty() {
            return invalid("listen must name at least one address".to_owned());
        }
        for other in &self.other_servers {
            if !is_valid_domain(0, other) || other == &self.domain {
                return invalid(format!(
                    "other_servers: '{other}' is not another server's domain name"
                ));
            }
        }
        for address in self
            .listen
            .iter()
            .chain(Some(&self.public_address).filter(|address| !address.is_empty()))
        {
            let port = address
                .rsplit_once(':')
                .map(|(host, port)| (host, port.parse::<u16>()));
            if !matches!(port, Some((host, Ok(_))) if !host.is_empty()) {
                return invalid(format!("address '{address}' is not of the form host:port"));
            }
        }
        if self.read_connections == 0 {
            return invalid("read_connections must be at least 1".to_owned());
        }
        if self.wave_cache_size == 0 {
            return invalid("wave_cache_size must be at least 1".to_owned());
        }
        if self.database_path.as_os_str().is_empty() {
            return invalid("database_path must not be empty".to_owned());
        }
        if self.session_lifetime_secs == 0 {
            return invalid("session_lifetime_secs must be at least 1".to_owned());
        }
        if let Err(message) = self.trusted_proxy_nets() {
            return invalid(format!("trusted_proxies: {message}"));
        }
        for origin in &self.allowed_origins {
            let authority = origin
                .strip_prefix("http://")
                .or_else(|| origin.strip_prefix("https://"));
            if !matches!(authority, Some(authority) if !authority.is_empty() && !authority.contains('/'))
            {
                return invalid(format!(
                    "allowed origin '{origin}' is not of the form scheme://host[:port]"
                ));
            }
        }
        for (name, value) in [
            ("http_max_body_size", self.http_max_body_size),
            (
                "websocket_max_message_size",
                self.websocket_max_message_size,
            ),
            ("websocket_send_queue", self.websocket_send_queue),
            (
                "websocket_max_subscriptions",
                self.websocket_max_subscriptions,
            ),
        ] {
            if value == 0 {
                return invalid(format!("{name} must be at least 1"));
            }
        }
        if self.websocket_send_queue_bytes < 1024 {
            return invalid("websocket_send_queue_bytes must be at least 1024".to_owned());
        }
        if self.websocket_ping_interval_secs == 0 {
            return invalid("websocket_ping_interval_secs must be at least 1".to_owned());
        }
        if self.attachment_max_size == 0 {
            return invalid("attachment_max_size must be at least 1".to_owned());
        }
        if self.search_max_results == 0 {
            return invalid("search_max_results must be at least 1".to_owned());
        }
        for (name, value) in [
            ("robot_connect_timeout_ms", self.robot_connect_timeout_ms),
            (
                "robot_request_timeout_secs",
                self.robot_request_timeout_secs,
            ),
            (
                "robot_max_response_bytes",
                self.robot_max_response_bytes as u64,
            ),
            (
                "robot_event_stream_buffer",
                self.robot_event_stream_buffer as u64,
            ),
            ("mcp_max_result_chars", self.mcp_max_result_chars as u64),
        ] {
            if value == 0 {
                return invalid(format!("{name} must be at least 1"));
            }
        }
        for entry in &self.robot_allowed_addresses {
            // An address, a CIDR block, or a host name (`*.` for a suffix).
            let host = entry.strip_prefix("*.").unwrap_or(entry);
            let is_host = !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            if entry.parse::<IpNet>().is_err() && !is_host {
                return invalid(format!(
                    "robot_allowed_addresses: '{entry}' is not an address, a CIDR block or a \
                     host name"
                ));
            }
        }
        if let Err(message) = self.ai.validate() {
            return invalid(message);
        }
        if let Err(message) = self.federation.validate() {
            return invalid(message);
        }
        if let Err(message) = self.person.validate() {
            return invalid(message);
        }
        if let Err(message) = self.push.validate() {
            return invalid(message);
        }
        if let Err(message) = self.apps.validate() {
            return invalid(message);
        }
        if let Err(message) = self.webrtc.validate() {
            return invalid(message);
        }
        if self.person.enabled && !self.federation.enabled {
            return invalid("person mode needs [federation] enabled = true".to_owned());
        }
        if self.is_device()
            && let Err(message) = self.device.validate(&self.domain)
        {
            return invalid(message);
        }
        Ok(())
    }

    /// Whether this node is one person's device (`mode = "device"`).
    pub fn is_device(&self) -> bool {
        self.mode == Mode::Device
    }

    /// Device mode: the one account, `<device.account>@<domain>`.
    pub fn device_address(&self) -> Option<ParticipantId> {
        if !self.is_device() {
            return None;
        }
        crate::auth::registration::check_new_username(&self.domain, Some(&self.device.account)).ok()
    }

    /// Whether anyone may register: `registration_enabled`, never in
    /// device mode.
    pub fn registration_open(&self) -> bool {
        self.registration_enabled && !self.is_device()
    }

    /// Whether `/directory` is served: `directory_enabled`, never in
    /// device mode (contacts take its place).
    pub fn directory_open(&self) -> bool {
        self.directory_enabled && !self.is_device()
    }

    /// The directory for the server's files other than the database:
    /// `data_dir`, or the directory of the database file.
    pub fn data_directory(&self) -> PathBuf {
        if !self.data_dir.as_os_str().is_empty() {
            return self.data_dir.clone();
        }
        match self.database_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        }
    }

    /// The directory of the attachment files: `attachments_dir`, or
    /// `attachments` in the data directory.
    pub fn attachments_directory(&self) -> PathBuf {
        if self.attachments_dir.as_os_str().is_empty() {
            self.data_directory().join("attachments")
        } else {
            self.attachments_dir.clone()
        }
    }

    /// The file holding the federation server key:
    /// `federation.key` in the data directory.
    pub fn federation_key_path(&self) -> PathBuf {
        self.data_directory().join("federation.key")
    }

    /// The file holding the person key on an admin device:
    /// `person.key_path`, or `person.key` in the data directory.
    pub fn person_key_path(&self) -> PathBuf {
        if self.person.key_path.is_empty() {
            self.data_directory().join("person.key")
        } else {
            PathBuf::from(&self.person.key_path)
        }
    }

    /// The file holding this device's certificate when `person.cert` is
    /// empty: `device.cert` in the data directory.
    pub fn device_cert_path(&self) -> PathBuf {
        self.data_directory().join("device.cert")
    }

    /// The welcome wave, if one is configured and its id is valid.
    pub fn welcome_wave(&self) -> Option<resonator_model::id::WaveId> {
        if self.welcome_wave_id.is_empty() {
            return None;
        }
        resonator_model::id::WaveId::of(&self.domain, &self.welcome_wave_id).ok()
    }

    /// The parsed `trusted_proxies`.
    pub fn trusted_proxy_nets(&self) -> Result<Vec<IpNet>, String> {
        self.trusted_proxies
            .iter()
            .map(|proxy| proxy.parse())
            .collect()
    }

    /// The `host:port` clients are told to connect the WebSocket to: the
    /// public address if set, otherwise the first listen address
    /// (`WaveClientServlet`).
    pub fn websocket_address(&self) -> &str {
        if self.public_address.is_empty() {
            self.listen.first().map_or("", String::as_str)
        } else {
            &self.public_address
        }
    }

    /// The configured admin, if `admin_user` is a real address.
    pub fn admin(&self) -> Option<ParticipantId> {
        ParticipantId::of(&self.admin_user)
            .ok()
            .filter(|id| !id.name().is_empty())
    }

    pub fn wave_cache_expire(&self) -> Duration {
        Duration::from_secs(self.wave_cache_expire_secs)
    }

    /// The database options these settings describe.
    pub fn database_options(&self) -> DatabaseOptions {
        DatabaseOptions {
            synchronous: self.synchronous,
            read_connections: self.read_connections,
            ..DatabaseOptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        overrides: Overrides,
    }

    #[test]
    fn other_servers_must_be_other_domains() {
        let mut config = Config {
            other_servers: vec!["resonator.network".to_owned()],
            ..Config::default()
        };
        config.validate().unwrap();
        for bad in ["https://resonator.network", "a/b", "", "local.net"] {
            config.other_servers = vec![bad.to_owned()];
            assert!(config.validate().is_err(), "{bad}");
        }
        // Empty: not in the printed configuration.
        assert!(
            !toml::to_string(&Config::default())
                .unwrap()
                .contains("other_servers")
        );
    }

    #[test]
    fn defaults_are_valid_and_listen_on_all_interfaces() {
        let config = Config::default();
        config.validate().unwrap();
        assert_eq!(config.listen, ["0.0.0.0:9898"]);
        assert_eq!(config.synchronous, Synchronous::Full);
        assert_eq!(config.wave_cache_size, 1000);
        assert_eq!(config.wave_cache_expire(), Duration::from_secs(3600));
        assert!(config.registration_enabled);
        assert_eq!(config.admin(), None);
    }

    #[test]
    fn file_overrides_defaults_and_keeps_the_rest() {
        let config = Config::from_toml(
            "domain = \"example.com\"\nsynchronous = \"full\"\nsnapshot_interval = 7\n",
        )
        .unwrap();
        assert_eq!(config.domain, "example.com");
        assert_eq!(config.synchronous, Synchronous::Full);
        assert_eq!(config.snapshot_interval, 7);
        assert_eq!(config.listen, Config::default().listen);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::from_toml("domian = \"example.com\"\n").is_err());
    }

    #[test]
    fn default_config_round_trips_through_toml() {
        let text = toml::to_string(&Config::default()).unwrap();
        assert_eq!(Config::from_toml(&text).unwrap(), Config::default());
    }

    #[test]
    fn flags_override_the_file() {
        let cli = TestCli::parse_from([
            "test",
            "--domain",
            "flag.example",
            "--listen",
            "0.0.0.0:1,100.64.0.1:2",
            "--synchronous",
            "FULL",
            "--registration-enabled",
            "false",
        ]);
        let mut config =
            Config::from_toml("domain = \"file.example\"\nadmin_user = \"a@b.c\"\n").unwrap();
        config.apply(&cli.overrides);
        config.validate().unwrap();
        assert_eq!(config.domain, "flag.example");
        assert_eq!(config.listen, ["0.0.0.0:1", "100.64.0.1:2"]);
        assert_eq!(config.synchronous, Synchronous::Full);
        assert!(!config.registration_enabled);
        assert_eq!(config.admin(), Some(ParticipantId::of("a@b.c").unwrap()));
    }

    #[test]
    fn invalid_settings_are_reported() {
        let check = |text: &str| Config::from_toml(text).unwrap().validate().is_err();
        assert!(check("domain = \"Not A Domain\""));
        assert!(check("listen = []"));
        assert!(check("listen = [\"nonsense\"]"));
        assert!(check("read_connections = 0"));
        assert!(check("wave_cache_size = 0"));
    }

    #[test]
    fn front_end_settings_have_defaults_and_overrides() {
        let config = Config::default();
        assert_eq!(config.websocket_max_message_size, 2 * 1024 * 1024);
        assert_eq!(config.cookie_secure, CookieSecure::Auto);
        assert_eq!(config.websocket_address(), "0.0.0.0:9898");
        assert!(config.metrics_enabled);
        let cli = TestCli::parse_from([
            "test",
            "--cookie-secure",
            "always",
            "--trusted-proxies",
            "10.0.0.0/8,::1",
            "--session-lifetime-secs",
            "60",
            "--public-address",
            "wave.example.com:443",
        ]);
        let mut config = Config::from_toml(
            "websocket_send_queue = 8\nallowed_origins = [\"https://app.example.com\"]\n",
        )
        .unwrap();
        config.apply(&cli.overrides);
        config.validate().unwrap();
        assert_eq!(config.cookie_secure, CookieSecure::Always);
        assert_eq!(config.trusted_proxies, ["10.0.0.0/8", "::1"]);
        assert_eq!(config.session_lifetime_secs, 60);
        assert_eq!(config.websocket_send_queue, 8);
        assert_eq!(config.websocket_address(), "wave.example.com:443");
        let check = |text: &str| Config::from_toml(text).unwrap().validate().is_err();
        assert!(check("trusted_proxies = [\"10.0.0.0/33\"]"));
        assert!(check("trusted_proxies = [\"proxy.example.com\"]"));
        assert!(check("allowed_origins = [\"app.example.com\"]"));
        assert!(check(
            "allowed_origins = [\"https://app.example.com/path\"]"
        ));
        assert!(check("session_lifetime_secs = 0"));
        assert!(check("websocket_send_queue = 0"));
        assert!(check("websocket_send_queue_bytes = 100"));
        assert!(Config::from_toml("cookie_secure = \"sometimes\"").is_err());
    }

    #[test]
    fn storage_and_search_settings() {
        let config = Config::default();
        assert_eq!(config.attachment_max_size, 25 * 1024 * 1024);
        assert_eq!(config.data_directory(), PathBuf::from("."));
        assert_eq!(
            config.attachments_directory(),
            PathBuf::from("./attachments")
        );
        assert!(config.search_fulltext_enabled);
        assert_eq!(config.welcome_wave(), None);
        let cli = TestCli::parse_from([
            "test",
            "--database-path",
            "/var/lib/wave/wave.db",
            "--attachment-max-size",
            "1024",
            "--search-fulltext-enabled",
            "false",
            "--welcome-wave-id",
            "w+welcome",
        ]);
        let mut config = Config::default();
        config.apply(&cli.overrides);
        config.validate().unwrap();
        assert_eq!(config.data_directory(), PathBuf::from("/var/lib/wave"));
        assert_eq!(
            config.attachments_directory(),
            PathBuf::from("/var/lib/wave/attachments")
        );
        assert_eq!(config.attachment_max_size, 1024);
        assert!(!config.search_fulltext_enabled);
        assert_eq!(
            config.welcome_wave().unwrap().serialise().unwrap(),
            "local.net/w+welcome"
        );
        config.data_dir = PathBuf::from("/data");
        assert_eq!(
            config.attachments_directory(),
            PathBuf::from("/data/attachments")
        );
        config.attachments_dir = PathBuf::from("/files");
        assert_eq!(config.attachments_directory(), PathBuf::from("/files"));
        let check = |text: &str| Config::from_toml(text).unwrap().validate().is_err();
        assert!(check("attachment_max_size = 0"));
        assert!(check("search_max_results = 0"));
    }

    #[test]
    fn ip_nets_match_addresses() {
        let net: IpNet = "10.1.0.0/16".parse().unwrap();
        assert!(net.contains("10.1.2.3".parse().unwrap()));
        assert!(!net.contains("10.2.0.1".parse().unwrap()));
        assert!(net.contains("::ffff:10.1.2.3".parse().unwrap()));
        assert!(!net.contains("::1".parse().unwrap()));
        let single: IpNet = "127.0.0.1".parse().unwrap();
        assert!(single.contains("127.0.0.1".parse().unwrap()));
        assert!(!single.contains("127.0.0.2".parse().unwrap()));
        let v6: IpNet = "fd7a:115c:a1e0::/48".parse().unwrap();
        assert!(v6.contains("fd7a:115c:a1e0::1".parse().unwrap()));
        assert!(!v6.contains("fd7a:115c:a1e1::1".parse().unwrap()));
        let all: IpNet = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("8.8.8.8".parse().unwrap()));
        assert!("10.0.0.0/33".parse::<IpNet>().is_err());
        assert!("nonsense".parse::<IpNet>().is_err());
    }

    #[test]
    fn integration_settings_have_defaults_overrides_and_checks() {
        let config = Config::default();
        assert_eq!(config.attachment_user_quota, 1 << 30);
        assert!(config.robot_api_enabled && config.mcp_enabled);
        assert!(!config.robot_allow_private_addresses);
        assert!(config.robot_allowed_addresses.is_empty());
        assert!(!config.ai.is_active());

        let mut config = Config::from_toml(
            r#"
            attachment_user_quota = 0
            robot_allowed_addresses = ["10.1.0.0/16", "robot.internal", "*.bots.example"]
            mcp_enabled = false

            [ai]
            reply_debounce_ms = 1000
            [ai.providers.local]
            base_url = "http://127.0.0.1:11434/v1"
            api_key = "sk-very-secret"
            [[ai.agents]]
            name = "assistant"
            provider = "local"
            model = "llama3.2"
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.attachment_user_quota, 0);
        assert!(!config.mcp_enabled);
        assert!(config.ai.is_active());
        assert_eq!(config.ai.agents[0].tools, AgentTools::Wave);

        // What `resonator config` prints has no secrets and parses again.
        let printed = toml::to_string(&config).unwrap();
        assert!(!printed.contains("sk-very-secret"), "{printed}");
        assert!(printed.contains(REDACTED));
        let reparsed = Config::from_toml(&printed).unwrap();
        assert_eq!(reparsed.ai.agents, config.ai.agents);

        let cli = TestCli::parse_from([
            "test",
            "--robot-allow-private-addresses",
            "true",
            "--robot-allowed-addresses",
            "192.168.0.0/16,fd00::/8",
            "--attachment-user-quota",
            "5",
            "--ai-enabled",
            "false",
        ]);
        config.apply(&cli.overrides);
        assert!(config.robot_allow_private_addresses);
        assert_eq!(
            config.robot_allowed_addresses,
            ["192.168.0.0/16", "fd00::/8"]
        );
        assert_eq!(config.attachment_user_quota, 5);
        assert!(!config.ai.is_active());

        for (text, expected) in [
            (
                "robot_allowed_addresses = [\"not a host\"]",
                "robot_allowed_addresses",
            ),
            (
                "robot_request_timeout_secs = 0",
                "robot_request_timeout_secs",
            ),
            (
                "[[ai.agents]]\nname = \"a\"\nprovider = \"nope\"\nmodel = \"m\"",
                "no provider 'nope'",
            ),
        ] {
            let error = Config::from_toml(text).unwrap().validate().unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn federation_section_has_defaults_overrides_and_checks() {
        let config = Config::default();
        assert!(!config.federation.enabled);
        assert_eq!(config.federation.mode, FederationMode::Open);
        assert_eq!(
            config.federation_key_path(),
            PathBuf::from("./federation.key")
        );
        let mut config = Config::from_toml(
            "[federation]\nenabled = true\nmode = \"open\"\nallow = [\"example.com\"]\n\
             relay = \"off\"\ndiscovery = \"off\"\nlisten = \"7777\"\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert!(config.federation.enabled);
        assert_eq!(config.federation.mode, FederationMode::Open);
        assert_eq!(config.federation.allow, ["example.com"]);
        let cli = TestCli::parse_from([
            "test",
            "--federation-enabled",
            "false",
            "--federation-mode",
            "allowlist",
            "--federation-allow",
            "a.example,b.example",
            "--federation-listen",
            "127.0.0.1:9",
        ]);
        config.apply(&cli.overrides);
        config.validate().unwrap();
        assert!(!config.federation.enabled);
        assert_eq!(config.federation.mode, FederationMode::Allowlist);
        assert_eq!(config.federation.allow, ["a.example", "b.example"]);
        assert_eq!(config.federation.listen, "127.0.0.1:9");
        assert!(Config::from_toml("[federation]\nmode = \"closed\"").is_err());
        assert!(Config::from_toml("[federation]\nunknown = 1").is_err());
        let error = Config::from_toml("[federation]\nrelay = \"nope\"")
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(error.to_string().contains("federation.relay"), "{error}");
    }

    #[test]
    fn missing_named_file_is_an_error() {
        let result = Config::load(
            Some(Path::new("/nonexistent/resonator.toml")),
            &Overrides::default(),
        );
        assert!(matches!(result, Err(ConfigError::Read { .. })));
    }
}
