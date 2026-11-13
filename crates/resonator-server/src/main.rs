//! The `resonator` binary.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use resonator_server::app;
use resonator_server::config::{Config, Overrides};

#[derive(Parser)]
#[command(name = "resonator", version, about = "A single-binary wave server")]
struct Cli {
    /// Path of the TOML config file [default: resonator.toml if present].
    #[arg(
        long,
        short,
        global = true,
        env = "RESONATOR_CONFIG",
        value_name = "FILE"
    )]
    config: Option<PathBuf>,
    #[command(flatten)]
    overrides: Overrides,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server until Ctrl-C.
    Serve,
    /// Write a consistent copy of the database to PATH; safe while the
    /// server is running.
    Backup {
        /// Where to write the copy; must not exist.
        path: PathBuf,
    },
    /// Print the effective configuration as TOML.
    Config,
}

/// Runs the server until Ctrl-C. With `RESONATOR_E2E_HOOKS=1` (the
/// end-to-end tests only) it also serves `POST /e2e/rewind`.
async fn serve(config: Config) -> Result<(), app::AppError> {
    if std::env::var("RESONATOR_E2E_HOOKS").as_deref() != Ok("1") {
        return app::serve(config).await;
    }
    tracing::warn!("end-to-end test hooks enabled: POST /e2e/rewind");
    let routes = resonator_server::http::placeholder_routes()
        .route("/e2e/rewind", axum::routing::post(e2e::rewind));
    let app = app::App::prepare(&config).await?.serve(routes).await?;
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "cannot listen for Ctrl-C; stopping");
    }
    app.shutdown().await;
    Ok(())
}

/// Hooks for the end-to-end tests of the web client.
mod e2e {
    use axum::Json;
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use resonator_model::id::{IdSerialiser, ModernIdSerialiser};
    use resonator_server::http::AppState;

    #[derive(serde::Deserialize)]
    pub struct Rewind {
        /// The wavelet name as the client writes it.
        wavelet: String,
        /// How many of the last deltas to take back.
        back: usize,
    }

    /// Rewinds a wavelet to a version and replays what was taken out, as
    /// a change of host would (`WaveServer::rewind_and_replay`).
    pub async fn rewind(
        State(state): State<AppState>,
        Query(request): Query<Rewind>,
    ) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
        let name = ModernIdSerialiser
            .deserialise_wavelet_name(&request.wavelet)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        let server = &state.server;
        let failed = |e: &dyn std::fmt::Display| (StatusCode::CONFLICT, e.to_string());
        let current = server
            .current_version(&name)
            .await
            .map_err(|e| failed(&e))?;
        let records = server
            .get_history_records(&name, &server.version_zero(&name), &current, None)
            .await
            .map_err(|e| failed(&e))?;
        let Some(first) = records
            .len()
            .checked_sub(request.back)
            .and_then(|i| records.get(i))
        else {
            return Err((StatusCode::BAD_REQUEST, "not that many deltas".to_owned()));
        };
        let version = first.applied_at_version.version();
        let outcome = server
            .rewind_and_replay(&name, version)
            .await
            .map_err(|e| failed(&e))?;
        Ok(Json(serde_json::json!({
            "fork": version,
            "applied": outcome.applied,
            "skipped": outcome.skipped,
            "version": outcome.version.version(),
        })))
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load(cli.config.as_deref(), &cli.overrides)?;
    match cli.command {
        Command::Serve => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(serve(config))?;
        }
        Command::Backup { path } => {
            app::backup(&config, &path)?;
            println!("{}", path.display());
        }
        Command::Config => print!("{}", toml::to_string(&config)?),
    }
    Ok(())
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("resonator: {error}");
            ExitCode::FAILURE
        }
    }
}
