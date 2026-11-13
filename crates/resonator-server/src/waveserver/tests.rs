//! Tests that need to hold up the database writer, which only code inside
//! the crate can do.

use std::sync::{Arc, Mutex, mpsc};

use resonator_model::data::WaveletData;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{
    NO_TIMESTAMP, WaveletDelta, WaveletOperation, WaveletOperationContext,
};
use resonator_model::proto::serialize_delta;
use resonator_model::version::HashedVersion;
use tempfile::TempDir;

use super::{WaveBusSubscriber, WaveServer, WaveServerOptions, WaveletProvider};
use crate::clock::ManualClock;
use crate::storage::{Database, DatabaseOptions, WaveletDeltaRecord};

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<String>>,
}

impl Recorder {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl WaveBusSubscriber for Recorder {
    fn wavelet_update(&self, wavelet: &WaveletData, _: &[Arc<WaveletDeltaRecord>]) {
        let event = format!("update {}", wavelet.version());
        self.events.lock().unwrap().push(event);
    }

    fn wavelet_committed(&self, _: &WaveletName, version: &HashedVersion) {
        let event = format!("committed {}", version.version());
        self.events.lock().unwrap().push(event);
    }
}

struct Fixture {
    _dir: TempDir,
    db: Database,
    server: WaveServer,
    recorder: Arc<Recorder>,
    user: ParticipantId,
}

fn fixture(options: WaveServerOptions) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("test.db"), DatabaseOptions::default()).unwrap();
    let server = WaveServer::with_clock(db.clone(), options, Arc::new(ManualClock::new(1000)));
    let recorder = Arc::new(Recorder::default());
    server.bus().subscribe(recorder.clone());
    Fixture {
        _dir: dir,
        db,
        server,
        recorder,
        user: ParticipantId::of("user@example.com").unwrap(),
    }
}

fn name(wave: &str) -> WaveletName {
    WaveletName::of_parts("example.com", wave, "example.com", "conv+root").unwrap()
}

impl Fixture {
    /// Makes the writer thread wait until the returned sender is used or
    /// dropped.
    async fn block_writer(&self) -> mpsc::Sender<()> {
        let (release, blocked) = mpsc::channel::<()>();
        let _ = self
            .db
            .enqueue_write(move |_| {
                let _ = blocked.recv();
                Ok(())
            })
            .await
            .unwrap();
        release
    }

    async fn add_self(&self, wavelet: &WaveletName, target: &HashedVersion) -> HashedVersion {
        let context = WaveletOperationContext::new(self.user.clone(), NO_TIMESTAMP, 1);
        let op = if target.version() == 0 {
            WaveletOperation::add_participant(context, self.user.clone())
        } else {
            WaveletOperation::no_op(context)
        };
        let delta = WaveletDelta::new(self.user.clone(), target.clone(), vec![op]);
        self.server
            .submit_request(wavelet, serialize_delta(&delta).unwrap())
            .await
            .unwrap()
            .hashed_version_after_application
    }
}

#[tokio::test]
async fn deltas_are_applied_and_announced_before_they_are_committed() {
    let f = fixture(WaveServerOptions::new("example.com"));
    let wavelet = name("w");
    let v0 = f.server.version_zero(&wavelet);
    let release = f.block_writer().await;

    // Both submissions return while nothing can have been written.
    let v1 = f.add_self(&wavelet, &v0).await;
    let v2 = f.add_self(&wavelet, &v1).await;
    assert_eq!(f.recorder.events(), ["update 1", "update 2"]);
    let snapshot = f.server.get_snapshot(&wavelet).await.unwrap().unwrap();
    assert_eq!(snapshot.snapshot.hashed_version(), &v2);
    assert_eq!(snapshot.committed_version, v0);
    assert_eq!(
        f.server.get_versions(&wavelet).await.unwrap(),
        Some((v2.clone(), v0.clone()))
    );
    // History is served from memory meanwhile.
    assert_eq!(
        f.server
            .get_history(&wavelet, &v0, &v2)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(
        f.db.delta_store()
            .open(wavelet.clone())
            .is_empty()
            .await
            .unwrap()
    );
    // Applied but uncommitted waves are listed.
    assert_eq!(
        f.server.get_wave_ids().await.unwrap(),
        std::slice::from_ref(&wavelet.wave_id)
    );

    drop(release);
    assert_eq!(f.server.wait_committed(&wavelet, 2).await.unwrap(), v2);
    // The bus hears of a commit just after the committed version moves.
    for _ in 0..500 {
        if f.recorder.events().len() == 4 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        f.recorder.events(),
        ["update 1", "update 2", "committed 1", "committed 2"]
    );
    let snapshot = f.server.get_snapshot(&wavelet).await.unwrap().unwrap();
    assert_eq!(snapshot.committed_version, v2);
    assert_eq!(f.db.delta_store().delta_count().await.unwrap(), 2);
}

#[tokio::test]
async fn waves_with_writes_in_flight_are_not_evicted() {
    let f = fixture(WaveServerOptions {
        wave_cache_size: 1,
        ..WaveServerOptions::new("example.com")
    });
    let release = f.block_writer().await;
    let names = [name("w1"), name("w2"), name("w3")];
    let mut versions = Vec::new();
    for wavelet in &names {
        let v0 = f.server.version_zero(wavelet);
        versions.push(f.add_self(wavelet, &v0).await);
    }
    // Over the bound, but every wave holds an unpersisted delta.
    assert_eq!(f.server.loaded_wave_count(), 3);
    assert_eq!(f.server.unload_all_waves(), 0);

    drop(release);
    for wavelet in &names {
        f.server.wait_committed(wavelet, 1).await.unwrap();
    }
    assert_eq!(f.server.unload_all_waves(), 3);
    // Nothing was lost.
    for (wavelet, version) in names.iter().zip(&versions) {
        let snapshot = f.server.get_snapshot(wavelet).await.unwrap().unwrap();
        assert_eq!(snapshot.snapshot.hashed_version(), version);
    }
    assert_eq!(f.server.loaded_wave_count(), 1);
}

#[tokio::test]
async fn shutdown_flushes_queued_writes() {
    let f = fixture(WaveServerOptions::new("example.com"));
    let wavelet = name("w");
    let v0 = f.server.version_zero(&wavelet);
    let release = f.block_writer().await;
    let mut version = v0;
    for _ in 0..20 {
        version = f.add_self(&wavelet, &version).await;
    }
    drop(release);
    let path = f.db.path().to_path_buf();
    crate::app::shutdown(f.server.clone()).await;

    let db = Database::open(path, DatabaseOptions::default()).unwrap();
    assert_eq!(
        db.delta_store().open(wavelet).end_version().await.unwrap(),
        Some(version)
    );
    db.close().await;
}
