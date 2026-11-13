//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::sync::Arc;

use resonator_model::id::{IdGeneratorImpl, ParticipantId, WaveletName};
use resonator_robots::api::{
    BlipData, JsonRpcResponse, OperationRequest, OperationType, ParamsProperty, WaveletData,
};
use resonator_robots::ops::{OperationExecutor, Registry};
use resonator_server::clock::ManualClock;
use resonator_server::storage::{Database, DatabaseOptions};
use resonator_server::waveserver::{WaveServer, WaveServerOptions, WaveletProvider};
use serde_json::Value;
use tempfile::TempDir;

pub const DOMAIN: &str = "example.com";

pub fn participant(address: &str) -> ParticipantId {
    ParticipantId::of(address).expect("participant")
}

pub fn alice() -> ParticipantId {
    participant("alice@example.com")
}

pub fn bob() -> ParticipantId {
    participant("bob@example.com")
}

/// A wave server on a temporary database, with a manual clock.
pub struct Fixture {
    pub dir: TempDir,
    pub db: Database,
    pub server: WaveServer,
    pub clock: Arc<ManualClock>,
}

/// Ids `<namespace>+s<n>` in order of creation: deterministic.
pub fn sequential_ids() -> IdGeneratorImpl {
    IdGeneratorImpl::new(DOMAIN, || "s".to_owned())
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Database::open(dir.path().join("test.db"), DatabaseOptions::default())
            .expect("open database");
        let clock = Arc::new(ManualClock::new(1_000_000));
        let server =
            WaveServer::with_clock(db.clone(), WaveServerOptions::new(DOMAIN), clock.clone());
        Fixture {
            dir,
            db,
            server,
            clock,
        }
    }

    /// An executor with deterministic ids and the fixture's clock.
    pub fn executor(&self) -> OperationExecutor {
        OperationExecutor::new(self.server.clone())
            .with_clock(self.clock.clone())
            .with_id_generator(sequential_ids())
    }

    /// The current XML of a document of a wavelet.
    pub async fn doc_xml(&self, name: &WaveletName, doc_id: &str) -> String {
        self.server
            .get_snapshot(name)
            .await
            .expect("snapshot")
            .expect("wavelet exists")
            .snapshot
            .document(doc_id)
            .map(|doc| doc.content().to_xml_string())
            .unwrap_or_default()
    }

    pub async fn participants(&self, name: &WaveletName) -> Vec<String> {
        self.server
            .get_snapshot(name)
            .await
            .expect("snapshot")
            .expect("wavelet exists")
            .snapshot
            .participants()
            .iter()
            .map(|p| p.address().to_owned())
            .collect()
    }
}

/// A `robot.createWavelet` request with temporary ids `TBD_wave` and
/// `TBD_blip`.
pub fn create_wavelet_request(id: &str, participants: &[&str]) -> OperationRequest {
    let mut data = WaveletData::new(
        "example.com/TBD_wave",
        "example.com/conv+root",
        "TBD_blip",
        None,
    );
    data.participants = participants.iter().map(|p| (*p).to_owned()).collect();
    OperationRequest::of(OperationType::RobotCreateWavelet, id)
        .with(ParamsProperty::WaveletData, data)
}

/// The blip data a client sends for a new blip.
pub fn new_blip_data(wave_id: &str, wavelet_id: &str, temp_id: &str, content: &str) -> BlipData {
    BlipData::new(wave_id, wavelet_id, temp_id, content)
}

pub fn data_string(response: &JsonRpcResponse, property: ParamsProperty) -> String {
    assert!(
        !response.is_error(),
        "unexpected error: {:?}",
        response.error_message()
    );
    response
        .string(property)
        .unwrap_or_else(|| panic!("no {property} in {response:?}"))
        .to_owned()
}

/// Creates a wave as `creator` and returns (wave id, wavelet id, root blip
/// id) as the API writes them, and the wavelet name.
pub async fn create_wave(
    executor: &OperationExecutor,
    creator: &ParticipantId,
    participants: &[&str],
) -> (String, String, String, WaveletName) {
    let responses = executor
        .execute(
            creator,
            Registry::DataApi,
            vec![create_wavelet_request("create", participants)],
        )
        .await;
    let wave_id = data_string(&responses[0], ParamsProperty::WaveId);
    let wavelet_id = data_string(&responses[0], ParamsProperty::WaveletId);
    let blip_id = data_string(&responses[0], ParamsProperty::BlipId);
    let name = WaveletName::of(
        resonator_robots::convert::deserialise_wave_id(&wave_id).expect("wave id"),
        resonator_robots::convert::deserialise_wavelet_id(&wavelet_id).expect("wavelet id"),
    );
    (wave_id, wavelet_id, blip_id, name)
}

pub fn to_json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("json")
}

// An in-process agent for the tests.

use std::sync::Mutex;

use async_trait::async_trait;
use resonator_robots::agent::RobotAgent;
use resonator_robots::api::{Capability, Context, EventProperties, EventType};
use resonator_robots::events::EventDelivery;

/// Greets when added to a wave and answers every blip someone else
/// creates with a reply to it. Records the deliveries it gets.
pub struct EchoAgent {
    pub id: String,
    pub deliveries: Mutex<Vec<EventDelivery>>,
}

impl EchoAgent {
    pub fn new(id: &str) -> Arc<Self> {
        Arc::new(EchoAgent {
            id: id.to_owned(),
            deliveries: Mutex::new(Vec::new()),
        })
    }

    pub fn deliveries(&self) -> Vec<EventDelivery> {
        self.deliveries.lock().unwrap().clone()
    }

    pub fn event_types(&self) -> Vec<Vec<EventType>> {
        self.deliveries()
            .iter()
            .map(|d| d.bundle.events.iter().map(|e| e.event_type()).collect())
            .collect()
    }
}

#[async_trait]
impl RobotAgent for EchoAgent {
    fn robot_id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::with_contexts(EventType::WaveletSelfAdded, vec![Context::Root]),
            Capability::with_contexts(EventType::WaveletBlipCreated, vec![Context::All]),
            Capability::new(EventType::WaveletParticipantsChanged),
        ]
    }

    async fn on_events(&self, delivery: EventDelivery) -> Vec<OperationRequest> {
        self.deliveries.lock().unwrap().push(delivery.clone());
        let bundle = &delivery.bundle;
        let Some(wavelet) = &bundle.wavelet else {
            return Vec::new();
        };
        let wave_id = wavelet.wave_id.clone().unwrap_or_default();
        let wavelet_id = wavelet.wavelet_id.clone().unwrap_or_default();
        let mut operations = Vec::new();
        for (i, event) in bundle.events.iter().enumerate() {
            match &event.properties {
                EventProperties::WaveletSelfAdded => {
                    operations.push(
                        OperationRequest::for_wavelet(
                            OperationType::WaveletAppendBlip,
                            format!("greet{i}"),
                            &wave_id,
                            &wavelet_id,
                        )
                        .with(
                            ParamsProperty::BlipData,
                            new_blip_data(
                                &wave_id,
                                &wavelet_id,
                                &format!("TBD_greet{i}"),
                                &format!("\nhello from {}", self.id),
                            ),
                        ),
                    );
                }
                EventProperties::WaveletBlipCreated {
                    new_blip_id: Some(new_blip_id),
                } => {
                    let said = bundle
                        .blips
                        .get(new_blip_id)
                        .map(|blip| blip.content.trim().to_owned())
                        .unwrap_or_default();
                    operations.push(
                        OperationRequest::for_blip(
                            OperationType::BlipCreateChild,
                            format!("echo{i}"),
                            &wave_id,
                            &wavelet_id,
                            new_blip_id,
                        )
                        .with(
                            ParamsProperty::BlipData,
                            new_blip_data(
                                &wave_id,
                                &wavelet_id,
                                &format!("TBD_echo{i}"),
                                &format!("\n{} heard: {said}", self.id),
                            ),
                        ),
                    );
                }
                _ => {}
            }
        }
        operations
    }
}

/// The paths at which two JSON values differ, with both values.
pub fn json_diff(expected: &Value, actual: &Value) -> Vec<String> {
    fn walk(path: &str, expected: &Value, actual: &Value, out: &mut Vec<String>) {
        match (expected, actual) {
            (Value::Object(a), Value::Object(b)) => {
                let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
                for key in keys {
                    walk(
                        &format!("{path}.{key}"),
                        a.get(key).unwrap_or(&Value::Null),
                        b.get(key).unwrap_or(&Value::Null),
                        out,
                    );
                }
            }
            (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
                for (i, (a, b)) in a.iter().zip(b).enumerate() {
                    walk(&format!("{path}[{i}]"), a, b, out);
                }
            }
            (a, b) if a != b => out.push(format!("{path}: expected {a}, got {b}")),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk("", expected, actual, &mut out);
    out
}
