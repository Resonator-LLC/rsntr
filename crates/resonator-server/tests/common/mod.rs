//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use resonator_model::data::WaveletData;
use resonator_model::docop::{DocOp, DocOpBuilder};
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{
    NO_TIMESTAMP, WaveletDelta, WaveletOperation, WaveletOperationContext,
};
use resonator_model::proto::federation::ProtocolWaveletDelta;
use resonator_model::proto::serialize_delta;
use resonator_model::version::HashedVersion;
use resonator_server::clock::ManualClock;
use resonator_server::storage::{Database, DatabaseOptions};
use resonator_server::waveserver::{WaveServer, WaveServerOptions};
use tempfile::TempDir;

pub const DOMAIN: &str = "example.com";

/// A database in a temporary directory that lives as long as this value.
pub struct TestDb {
    pub dir: TempDir,
    pub path: PathBuf,
}

impl TestDb {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("resonator.db");
        TestDb { dir, path }
    }

    pub fn open(&self) -> Database {
        Database::open(&self.path, DatabaseOptions::default()).expect("open database")
    }
}

pub fn participant(address: &str) -> ParticipantId {
    ParticipantId::of(address).expect("participant")
}

pub fn wavelet_name(wave: &str, wavelet: &str) -> WaveletName {
    WaveletName::of_parts(DOMAIN, wave, DOMAIN, wavelet).expect("wavelet name")
}

pub fn context(author: &ParticipantId) -> WaveletOperationContext {
    WaveletOperationContext::new(author.clone(), NO_TIMESTAMP, 1)
}

pub fn add_participant(author: &ParticipantId, who: &ParticipantId) -> WaveletOperation {
    WaveletOperation::add_participant(context(author), who.clone())
}

pub fn remove_participant(author: &ParticipantId, who: &ParticipantId) -> WaveletOperation {
    WaveletOperation::remove_participant(context(author), who.clone())
}

pub fn no_op(author: &ParticipantId) -> WaveletOperation {
    WaveletOperation::no_op(context(author))
}

/// A document operation inserting `text` at `position` of a document of
/// `size` items.
pub fn insert_doc_op(size: usize, position: usize, text: &str) -> DocOp {
    let mut builder = DocOpBuilder::new();
    if position > 0 {
        builder = builder.retain(position);
    }
    builder = builder.characters(text);
    if size > position {
        builder = builder.retain(size - position);
    }
    builder.build().expect("doc op")
}

pub fn insert(
    author: &ParticipantId,
    blip: &str,
    size: usize,
    position: usize,
    text: &str,
) -> WaveletOperation {
    WaveletOperation::blip_content(context(author), blip, insert_doc_op(size, position, text))
}

pub fn delta(
    author: &ParticipantId,
    target: &HashedVersion,
    ops: Vec<WaveletOperation>,
) -> ProtocolWaveletDelta {
    serialize_delta(&WaveletDelta::new(author.clone(), target.clone(), ops)).expect("delta")
}

/// The XML content of a document of a wavelet.
pub fn doc_xml(wavelet: &WaveletData, blip: &str) -> String {
    wavelet
        .document(blip)
        .unwrap_or_else(|| panic!("no document {blip}"))
        .content()
        .to_xml_string()
}

pub fn options() -> WaveServerOptions {
    WaveServerOptions::new(DOMAIN)
}

/// A wave server on `db` with a manual clock starting at `1_000_000`.
pub fn server_with(db: Database, options: WaveServerOptions) -> (WaveServer, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(1_000_000));
    (WaveServer::with_clock(db, options, clock.clone()), clock)
}

pub fn server(db: Database) -> (WaveServer, Arc<ManualClock>) {
    server_with(db, options())
}
