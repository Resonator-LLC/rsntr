//! Load test: simulated users typing into wavelets.
//!
//! Each user belongs to one wavelet and types one character per delta into
//! a blip of their own, starting a new blip every `--blip-size` characters.
//! A user submits the next delta as soon as the previous one is
//! acknowledged, against the version that acknowledgement named, so with
//! several users per wavelet most deltas are submitted against a stale
//! version and go through the transform path.
//!
//! ```text
//! cargo run --release -p resonator-server --example loadtest -- \
//!     --db /path/on/local/disk/loadtest.db --users 64 --wavelets 16 --deltas 2000
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use resonator_model::docop::DocOpBuilder;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{
    NO_TIMESTAMP, WaveletDelta, WaveletOperation, WaveletOperationContext,
};
use resonator_model::proto::serialize_delta;
use resonator_server::storage::{Database, DatabaseOptions, Synchronous};
use resonator_server::waveserver::{
    WaveServer, WaveServerOptions, WaveletProvider, WaveletSettings,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Ack {
    /// A user continues when the delta is applied in memory (what a client
    /// sees as the submit response).
    Applied,
    /// A user continues when the delta is durably committed.
    Committed,
}

#[derive(Parser, Debug)]
struct Args {
    /// Database file; must not exist. Removed afterwards unless --keep.
    #[arg(long)]
    db: PathBuf,
    /// Number of simulated users.
    #[arg(long, default_value_t = 64)]
    users: usize,
    /// Number of wavelets the users are spread over.
    #[arg(long, default_value_t = 16)]
    wavelets: usize,
    /// Deltas each user submits.
    #[arg(long, default_value_t = 2000)]
    deltas: usize,
    #[arg(long, default_value = "normal")]
    synchronous: Synchronous,
    #[arg(long, value_enum, default_value = "applied")]
    ack: Ack,
    /// Characters a user types into a blip before starting a new one.
    #[arg(long, default_value_t = 400)]
    blip_size: usize,
    #[arg(long, default_value_t = 100)]
    snapshot_interval: u64,
    /// Keep the database file.
    #[arg(long)]
    keep: bool,
}

const DOMAIN: &str = "example.com";

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn files_size(db: &std::path::Path) -> u64 {
    ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| {
            let mut name = db.as_os_str().to_owned();
            name.push(suffix);
            std::fs::metadata(PathBuf::from(name)).ok()
        })
        .map(|metadata| metadata.len())
        .sum()
}

#[allow(clippy::too_many_arguments)]
async fn run_user(
    server: WaveServer,
    wavelet: WaveletName,
    user: ParticipantId,
    index: usize,
    args_deltas: usize,
    blip_size: usize,
    ack: Ack,
    start: resonator_model::version::HashedVersion,
) -> Result<Vec<u64>, Box<dyn std::error::Error + Send + Sync>> {
    let mut latencies = Vec::with_capacity(args_deltas);
    let mut known = start;
    let text = char::from(b'a' + (index % 26) as u8).to_string();
    for i in 0..args_deltas {
        let blip = format!("b+u{index}n{}", i / blip_size);
        let size = i % blip_size;
        let mut builder = DocOpBuilder::new();
        if size > 0 {
            builder = builder.retain(size);
        }
        let op = WaveletOperation::blip_content(
            WaveletOperationContext::new(user.clone(), NO_TIMESTAMP, 1),
            blip,
            builder.characters(text.clone()).build()?,
        );
        let delta = serialize_delta(&WaveletDelta::new(user.clone(), known, vec![op]))?;
        let begin = Instant::now();
        let result = match ack {
            Ack::Applied => server.submit_request(&wavelet, delta).await?,
            Ack::Committed => server.submit_request_durably(&wavelet, delta).await?,
        };
        latencies.push(begin.elapsed().as_micros() as u64);
        known = result.hashed_version_after_application;
    }
    Ok(latencies)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    if args.db.exists() {
        return Err(format!("{} already exists", args.db.display()).into());
    }
    let wavelets = args.wavelets.clamp(1, args.users.max(1));
    let db = Database::open(
        &args.db,
        DatabaseOptions {
            synchronous: args.synchronous,
            ..DatabaseOptions::default()
        },
    )?;
    let server = WaveServer::new(
        db.clone(),
        WaveServerOptions {
            wavelet: WaveletSettings {
                snapshot_interval: args.snapshot_interval,
                ..WaveletSettings::default()
            },
            ..WaveServerOptions::new(DOMAIN)
        },
    );

    // Set-up: every wavelet gets its users as participants in one delta.
    let users: Vec<ParticipantId> = (0..args.users)
        .map(|i| ParticipantId::of(&format!("user{i}@{DOMAIN}")))
        .collect::<Result<_, _>>()?;
    let mut starts = Vec::new();
    let mut names = Vec::new();
    for w in 0..wavelets {
        let name = WaveletName::of_parts(DOMAIN, &format!("w+load{w}"), DOMAIN, "conv+root")?;
        let members: Vec<&ParticipantId> = users.iter().skip(w).step_by(wavelets).collect();
        let creator = members[0].clone();
        let ops = members
            .iter()
            .map(|member| {
                WaveletOperation::add_participant(
                    WaveletOperationContext::new(creator.clone(), NO_TIMESTAMP, 1),
                    (*member).clone(),
                )
            })
            .collect();
        let delta = WaveletDelta::new(creator, server.version_zero(&name), ops);
        let result = server.submit_delta(&name, &delta).await?;
        starts.push(result.hashed_version_after_application);
        names.push(name);
    }
    server.flush().await?;

    let begin = Instant::now();
    let mut tasks = Vec::new();
    for (index, user) in users.iter().cloned().enumerate() {
        let w = index % wavelets;
        tasks.push(tokio::spawn(run_user(
            server.clone(),
            names[w].clone(),
            user,
            index,
            args.deltas,
            args.blip_size.max(1),
            args.ack,
            starts[w].clone(),
        )));
    }
    let mut latencies = Vec::with_capacity(args.users * args.deltas);
    for task in tasks {
        latencies.extend(task.await??);
    }
    let submitted = begin.elapsed();
    // Everything applied is durable only after this.
    server.flush().await?;
    let durable = begin.elapsed();

    let total = latencies.len();
    let stored = db.delta_store().delta_count().await? as usize;
    if stored != total + wavelets {
        return Err(format!(
            "expected {} stored deltas, found {stored}",
            total + wavelets
        )
        .into());
    }
    latencies.sort_unstable();
    resonator_server::app::shutdown(server).await;
    let size = files_size(&args.db);

    let rate = |elapsed: Duration| total as f64 / elapsed.as_secs_f64();
    println!(
        "users={} wavelets={} deltas/user={} total={} synchronous={} ack={:?} snapshot_interval={}",
        args.users,
        wavelets,
        args.deltas,
        total,
        args.synchronous,
        args.ack,
        args.snapshot_interval
    );
    println!(
        "  elapsed: {:.2}s submitted, {:.2}s all durable",
        submitted.as_secs_f64(),
        durable.as_secs_f64()
    );
    println!(
        "  throughput: {:.0} deltas/s submitted, {:.0} deltas/s durable",
        rate(submitted),
        rate(durable)
    );
    println!(
        "  submit latency (until {}): p50 {} us, p99 {} us, max {} us",
        match args.ack {
            Ack::Applied => "applied",
            Ack::Committed => "committed",
        },
        percentile(&latencies, 0.50),
        percentile(&latencies, 0.99),
        latencies.last().copied().unwrap_or(0)
    );
    println!(
        "  database: {:.1} MiB after checkpoint, {:.1} MiB per 100k deltas",
        size as f64 / 1048576.0,
        size as f64 / 1048576.0 * 100_000.0 / stored as f64
    );
    if !args.keep {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = args.db.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(name));
        }
    }
    Ok(())
}
