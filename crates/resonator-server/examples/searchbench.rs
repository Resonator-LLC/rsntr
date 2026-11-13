//! Search benchmark: fills a database with conversation waves, then times
//! inbox and full-text queries through the library interface, after a
//! restart (nothing in memory) and warm.
//!
//! ```text
//! cargo run --release -p resonator-server --example searchbench -- \
//!     --dir /path/on/local/disk/tmp-search --waves 5000 --blips 10
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use resonator_model::conversation::{WaveletBasedConversation, make_wavelet_conversational};
use resonator_model::data::WaveletData;
use resonator_model::docop::{Attributes, DocInitialization, DocOpBuilder};
use resonator_model::id::util::build_user_data_wavelet_id;
use resonator_model::id::{IdGeneratorImpl, ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::supplement::{
    DefaultFollow, SupplementedWaveImpl, WaveletBasedSupplement, WaveletDataView,
};
use resonator_model::version::HashedVersionFactory;
use resonator_model::wavelet::{OpBasedWavelet, WaveView};
use resonator_server::search::{SearchProvider, SearchService, SearchSettings};
use resonator_server::storage::{Database, DatabaseOptions, Synchronous};
use resonator_server::waveserver::{WaveServer, WaveServerOptions, WaveletProvider};

const DOMAIN: &str = "example.com";

#[derive(Parser, Debug)]
struct Args {
    /// Directory for the database; created, and removed afterwards unless
    /// --keep.
    #[arg(long)]
    dir: PathBuf,
    /// Waves the benchmark user is a participant of.
    #[arg(long, default_value_t = 5000)]
    waves: usize,
    /// Blips per wave.
    #[arg(long, default_value_t = 10)]
    blips: usize,
    /// Words per blip.
    #[arg(long, default_value_t = 40)]
    words: usize,
    /// Times each query is repeated for the warm numbers.
    #[arg(long, default_value_t = 200)]
    repeats: usize,
    #[arg(long, default_value = "full")]
    synchronous: Synchronous,
    #[arg(long)]
    keep: bool,
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// A word of a vocabulary of 20,000, common ones much more often.
    fn word(&mut self) -> String {
        let uniform = (self.next() % 1_000_000) as f64 / 1_000_000.0;
        word_at((uniform * uniform * uniform * 20_000.0) as u64)
    }
}

/// The word of the vocabulary with this rank; low ranks are common.
fn word_at(index: u64) -> String {
    const SYLLABLES: [&str; 16] = [
        "ka", "lo", "mi", "ne", "ru", "sa", "to", "vu", "be", "di", "fo", "ga", "hu", "je", "pi",
        "zo",
    ];
    let mut word = String::new();
    let mut rest = index + 16;
    while rest > 0 {
        word.push_str(SYLLABLES[(rest % 16) as usize]);
        rest /= 16;
    }
    word
}

fn blip(text: &str) -> DocInitialization {
    DocOpBuilder::new()
        .element_start("body", Attributes::new())
        .element_start("line", Attributes::new())
        .element_end()
        .characters(text)
        .element_end()
        .build_initialization()
        .expect("blip")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

fn root_name(wave: &WaveId) -> WaveletName {
    WaveletName::of(
        wave.clone(),
        WaveletId::of(DOMAIN, "conv+root").expect("id"),
    )
}

async fn create_wave(
    server: &WaveServer,
    random: &mut Random,
    args: &Args,
    index: usize,
    author: &ParticipantId,
    others: &[&ParticipantId],
) -> WaveId {
    let wave = WaveId::of(DOMAIN, &format!("w+bench{index}")).expect("wave id");
    let seed = format!("s{index}x");
    let mut ids = IdGeneratorImpl::new(DOMAIN, move || seed.clone());
    let mut view = WaveView::new(wave.clone(), author.clone(), now());
    let root = view.create_root().expect("root");
    make_wavelet_conversational(root).expect("manifest");
    let mut conversation = WaveletBasedConversation::new(root, &mut ids).expect("conversation");
    for blip_index in 0..args.blips {
        let mut text = if blip_index == 0 {
            format!("Wave number {index} marker{index}x. ")
        } else {
            String::new()
        };
        for _ in 0..args.words {
            text.push_str(&random.word());
            text.push(' ');
        }
        if index % 100 == 7 && blip_index == 3 {
            text.push_str("quarterly budget review ");
        }
        conversation
            .append_blip_with_content("", Some(&blip(&text)))
            .expect("blip");
    }
    conversation.update_implicit_title().expect("title");
    for other in others {
        conversation.add_participant(other).expect("participant");
    }
    for (name, delta) in view.take_deltas() {
        server.submit_delta(&name, &delta).await.expect("submit");
    }
    wave
}

async fn archive(server: &WaveServer, user: &ParticipantId, wave: &WaveId) {
    let root = server
        .get_snapshot(&root_name(wave))
        .await
        .expect("snapshot")
        .expect("wavelet")
        .snapshot;
    let name = WaveletName::of(
        wave.clone(),
        build_user_data_wavelet_id(user).expect("udw id"),
    );
    let data = WaveletData::empty(
        &name,
        user.clone(),
        HashedVersionFactory::create_version_zero(&name),
        now(),
    );
    let mut udw = OpBasedWavelet::new(data, user.clone(), now());
    udw.add_participant(user).expect("participant");
    {
        let view = WaveletDataView::new(user.clone(), [&root]);
        let state = WaveletBasedSupplement::create(&mut udw).expect("supplement");
        let mut supplement = SupplementedWaveImpl::new(state, view, DefaultFollow::Always);
        supplement.mark_as_read().expect("read");
        supplement.archive().expect("archive");
    }
    if let Some(delta) = udw.take_delta() {
        server.submit_delta(&name, &delta).await.expect("submit");
    }
}

fn open(args: &Args) -> (WaveServer, SearchService) {
    let options = DatabaseOptions {
        synchronous: args.synchronous,
        ..DatabaseOptions::default()
    };
    let db = Database::open(args.dir.join("bench.db"), options).expect("open database");
    let server = WaveServer::new(db, WaveServerOptions::new(DOMAIN));
    let search = SearchService::new(server.clone(), SearchSettings::default());
    (server, search)
}

async fn close(server: WaveServer, search: SearchService) {
    search.shutdown();
    server.flush().await.expect("flush");
    server.database().close().await;
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

async fn time_query(
    search: &SearchService,
    user: &ParticipantId,
    query: &str,
    start_at: usize,
    num_results: usize,
    repeats: usize,
) {
    let started = Instant::now();
    let first = search
        .search(user, query, start_at, num_results)
        .await
        .expect("search");
    let cold = started.elapsed();
    let mut times = Vec::with_capacity(repeats);
    for _ in 0..repeats {
        let started = Instant::now();
        let result = search
            .search(user, query, start_at, num_results)
            .await
            .expect("search");
        times.push(started.elapsed());
        assert_eq!(result.digests.len(), first.digests.len());
    }
    times.sort();
    let at = |fraction: f64| millis(times[((times.len() - 1) as f64 * fraction) as usize]);
    println!(
        "{:<44} [{:>4}+{:<3}] {:>5} of {:>5}  first {:>8.2} ms  p50 {:>7.2}  p95 {:>7.2}  max {:>7.2}",
        format!("'{query}'"),
        start_at,
        num_results,
        first.digests.len(),
        first.total_results,
        millis(cold),
        at(0.5),
        at(0.95),
        at(1.0),
    );
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    std::fs::create_dir_all(&args.dir).expect("create directory");
    let user = ParticipantId::of("bench@example.com").expect("participant");
    let friend = ParticipantId::of("friend@example.com").expect("participant");
    let stranger = ParticipantId::of("stranger@example.com").expect("participant");

    // ---- Populate ----
    let (server, search) = open(&args);
    let mut random = Random(42);
    let started = Instant::now();
    for index in 0..args.waves {
        // Every tenth wave is the friend's, with the user on it; the friend
        // is on every fourth wave of the user's.
        let wave = if index % 10 == 3 {
            create_wave(&server, &mut random, &args, index, &friend, &[&user]).await
        } else if index % 4 == 0 {
            create_wave(&server, &mut random, &args, index, &user, &[&friend]).await
        } else {
            create_wave(&server, &mut random, &args, index, &user, &[]).await
        };
        if index % 10 == 5 {
            archive(&server, &user, &wave).await;
        }
    }
    // Waves the user is not on.
    for index in args.waves..args.waves + args.waves / 10 {
        create_wave(&server, &mut random, &args, index, &stranger, &[]).await;
    }
    server.flush().await.expect("flush");
    let populated = started.elapsed();
    let started = Instant::now();
    search.flush().await;
    println!(
        "populated {} waves x {} blips x {} words in {:.1} s; index caught up {:.1} s later; {} wavelets indexed",
        args.waves + args.waves / 10,
        args.blips,
        args.words,
        populated.as_secs_f64(),
        started.elapsed().as_secs_f64(),
        search.indexed_wavelet_count().await.expect("count"),
    );
    close(server, search).await;
    let size = |name: &str| {
        std::fs::metadata(args.dir.join(name))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    println!(
        "database {:.1} MiB",
        (size("bench.db") + size("bench.db-wal")) as f64 / (1024.0 * 1024.0)
    );

    // ---- Queries after a restart: no wave in memory, no digest cached ----
    let (server, search) = open(&args);
    let started = Instant::now();
    search.flush().await;
    println!(
        "restart: index checked against {} wavelets in {:.0} ms",
        search.indexed_wavelet_count().await.expect("count"),
        millis(started.elapsed())
    );
    println!("\nquery                                        [start+n ] found of total");
    let repeats = args.repeats;
    let (common, medium, rare) = (word_at(3), word_at(4_000), word_at(19_000));
    let queries: Vec<(String, usize, usize)> = vec![
        ("in:inbox".to_owned(), 0, 20),
        ("in:inbox".to_owned(), 0, 100),
        ("in:inbox".to_owned(), args.waves / 2, 20),
        ("in:inbox orderby:createdasc".to_owned(), 0, 20),
        ("in:archive".to_owned(), 0, 20),
        (String::new(), 0, 20),
        ("in:inbox with:friend".to_owned(), 0, 20),
        ("in:inbox creator:friend".to_owned(), 0, 20),
        // Full text: one wave; about one wave in a hundred; a rare, a
        // medium and a common word; several words; a prefix; no hit.
        ("marker1234x".to_owned(), 0, 20),
        ("\"quarterly budget review\"".to_owned(), 0, 20),
        ("budget in:inbox".to_owned(), 0, 20),
        (rare.clone(), 0, 20),
        (medium.clone(), 0, 20),
        (common.clone(), 0, 20),
        (format!("{common} {medium}"), 0, 20),
        (format!("{medium} {rare}"), 0, 20),
        (format!("in:inbox {medium} creator:friend"), 0, 20),
        (format!("{}*", &medium[..medium.len() - 2]), 0, 20),
        ("wave number".to_owned(), 0, 100),
        ("nosuchwordanywhere".to_owned(), 0, 20),
    ];
    for (query, start_at, num_results) in &queries {
        time_query(&search, &user, query, *start_at, *num_results, repeats).await;
    }
    println!("as someone who is on none of the waves:");
    let nobody = ParticipantId::of("nobody@example.com").expect("participant");
    time_query(&search, &nobody, "in:inbox", 0, 20, repeats).await;
    time_query(&search, &nobody, &common, 0, 20, repeats).await;
    close(server, search).await;

    // ---- Rebuilding the index from nothing ----
    {
        let conn = rusqlite::Connection::open(args.dir.join("bench.db")).expect("open");
        conn.execute_batch("DELETE FROM search_wavelets; DELETE FROM search_text;")
            .expect("clear index");
    }
    let (server, search) = open(&args);
    let started = Instant::now();
    search.flush().await;
    println!(
        "\nindex rebuilt from the wavelets: {} wavelets in {:.1} s",
        search.indexed_wavelet_count().await.expect("count"),
        started.elapsed().as_secs_f64()
    );
    time_query(&search, &user, "in:inbox", 0, 20, repeats).await;
    close(server, search).await;

    if !args.keep {
        std::fs::remove_dir_all(&args.dir).expect("remove directory");
    }
}
