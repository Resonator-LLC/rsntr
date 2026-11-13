//! `resonator-push`: the push relay. See `docs/push-relay.md`.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use resonator_push::{Config, Relay};

/// The resonator push relay: seals device push tokens and wakes
/// sleeping devices with silent pushes.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// A TOML config file; `RESONATOR_PUSH_*` variables override it.
    #[arg(long, short, env = "RESONATOR_PUSH_CONFIG")]
    config: Option<PathBuf>,
    /// Check the configuration and keys, then exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let config = match Config::load(args.config.as_deref()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("resonator-push: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    let relay = match Relay::new(config) {
        Ok(relay) => Arc::new(relay),
        Err(error) => {
            eprintln!("resonator-push: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    if args.check {
        println!("{relay:?}");
        return std::process::ExitCode::SUCCESS;
    }
    let listen = relay.config().listen.clone();
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("resonator-push: cannot listen on {listen}: {error}");
            return std::process::ExitCode::from(1);
        }
    };
    tracing::info!(%listen, relay = ?relay, "push relay ready");
    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("stopping");
    };
    match relay.serve(listener, shutdown).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("resonator-push: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
