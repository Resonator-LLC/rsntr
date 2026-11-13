//! Events and the bundle they are sent to a robot in (`com.google.wave.api
//! .event.*`, `EventSerializer`, `EventMessageBundle` and its Gson adaptor).

use std::collections::BTreeMap;

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::lenient;
use super::request::ParamsProperty;
use super::types::{BlipData, BlipThread, WaveletData};
use resonator_server::account::{EventType, ProtocolVersion};

/// What an event says beyond its common fields: the declared fields of the
/// original's event class.
#[derive(Debug, Clone, PartialEq)]
pub enum EventProperties {
    WaveletBlipCreated {
        new_blip_id: Option<String>,
    },
    WaveletBlipRemoved {
        removed_blip_id: Option<String>,
    },
    WaveletParticipantsChanged {
        participants_added: Vec<String>,
        participants_removed: Vec<String>,
    },
    WaveletSelfAdded,
    WaveletSelfRemoved,
    WaveletTitleChanged {
        title: Option<String>,
    },
    WaveletCreated {
        message: Option<String>,
        wave_id: Option<String>,
        wavelet_id: Option<String>,
    },
    WaveletFetched {
        message: Option<String>,
        wavelet_data: Option<Box<WaveletData>>,
        blips: BTreeMap<String, BlipData>,
        threads: BTreeMap<String, BlipThread>,
    },
    WaveletTagsChanged,
    BlipContributorsChanged {
        contributors_added: Vec<String>,
        contributors_removed: Vec<String>,
    },
    BlipSubmitted,
    DocumentChanged,
    FormButtonClicked {
        button_name: Option<String>,
    },
    GadgetStateChanged {
        index: Option<i32>,
        old_state: BTreeMap<String, Option<String>>,
    },
    AnnotatedTextChanged {
        name: Option<String>,
        value: Option<String>,
    },
    OperationError {
        operation_id: Option<String>,
        message: Option<String>,
    },
}

/// Something that happened to a wavelet (`Event`).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The address of who caused the event.
    pub modified_by: Option<String>,
    pub timestamp: i64,
    /// The blip the event is about; for wavelet events the root blip.
    pub blip_id: Option<String>,
    pub properties: EventProperties,
}

impl Event {
    pub fn new(
        modified_by: &str,
        timestamp: i64,
        blip_id: Option<&str>,
        properties: EventProperties,
    ) -> Self {
        Event {
            modified_by: Some(modified_by.to_owned()),
            timestamp,
            blip_id: blip_id.map(str::to_owned),
            properties,
        }
    }

    pub fn event_type(&self) -> EventType {
        match &self.properties {
            EventProperties::WaveletBlipCreated { .. } => EventType::WaveletBlipCreated,
            EventProperties::WaveletBlipRemoved { .. } => EventType::WaveletBlipRemoved,
            EventProperties::WaveletParticipantsChanged { .. } => {
                EventType::WaveletParticipantsChanged
            }
            EventProperties::WaveletSelfAdded => EventType::WaveletSelfAdded,
            EventProperties::WaveletSelfRemoved => EventType::WaveletSelfRemoved,
            EventProperties::WaveletTitleChanged { .. } => EventType::WaveletTitleChanged,
            EventProperties::WaveletCreated { .. } => EventType::WaveletCreated,
            EventProperties::WaveletFetched { .. } => EventType::WaveletFetched,
            EventProperties::WaveletTagsChanged => EventType::WaveletTagsChanged,
            EventProperties::BlipContributorsChanged { .. } => EventType::BlipContributorsChanged,
            EventProperties::BlipSubmitted => EventType::BlipSubmitted,
            EventProperties::DocumentChanged => EventType::DocumentChanged,
            EventProperties::FormButtonClicked { .. } => EventType::FormButtonClicked,
            EventProperties::GadgetStateChanged { .. } => EventType::GadgetStateChanged,
            EventProperties::AnnotatedTextChanged { .. } => EventType::AnnotatedTextChanged,
            EventProperties::OperationError { .. } => EventType::OperationError,
        }
    }

    /// The event's own properties and its blip id, keyed like the fields
    /// of the original's event class
    /// (`EventSerializer.extractPropertiesToStringMap`). Absent values are
    /// `null`.
    pub fn property_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("blipId".to_owned(), json!(self.blip_id));
        let mut put = |key: &str, value: Value| {
            map.insert(key.to_owned(), value);
        };
        match &self.properties {
            EventProperties::WaveletBlipCreated { new_blip_id } => {
                put("newBlipId", json!(new_blip_id));
            }
            EventProperties::WaveletBlipRemoved { removed_blip_id } => {
                put("removedBlipId", json!(removed_blip_id));
            }
            EventProperties::WaveletParticipantsChanged {
                participants_added,
                participants_removed,
            } => {
                put("participantsAdded", json!(participants_added));
                put("participantsRemoved", json!(participants_removed));
            }
            EventProperties::WaveletTitleChanged { title } => put("title", json!(title)),
            EventProperties::WaveletCreated {
                message,
                wave_id,
                wavelet_id,
            } => {
                put("message", json!(message));
                put("waveId", json!(wave_id));
                put("waveletId", json!(wavelet_id));
            }
            EventProperties::WaveletFetched {
                message,
                wavelet_data,
                blips,
                threads,
            } => {
                put("message", json!(message));
                put("waveletData", json!(wavelet_data));
                put("blips", json!(blips));
                put("threads", json!(threads));
            }
            EventProperties::BlipContributorsChanged {
                contributors_added,
                contributors_removed,
            } => {
                put("contributorsAdded", json!(contributors_added));
                put("contributorsRemoved", json!(contributors_removed));
            }
            EventProperties::FormButtonClicked { button_name } => {
                put("buttonName", json!(button_name));
            }
            EventProperties::GadgetStateChanged { index, old_state } => {
                put("index", json!(index));
                put("oldState", json!(old_state));
            }
            EventProperties::AnnotatedTextChanged { name, value } => {
                put("name", json!(name));
                put("value", json!(value));
            }
            EventProperties::OperationError {
                operation_id,
                message,
            } => {
                put("operationId", json!(operation_id));
                put("message", json!(message));
            }
            EventProperties::WaveletSelfAdded
            | EventProperties::WaveletSelfRemoved
            | EventProperties::WaveletTagsChanged
            | EventProperties::BlipSubmitted
            | EventProperties::DocumentChanged => {}
        }
        map
    }

    /// The properties as response data
    /// (`EventSerializer.extractPropertiesToParamsPropertyMap`). Properties
    /// that are not a [`ParamsProperty`] are left out (the original files
    /// them under a null key, which fails when the response is written).
    pub fn response_data(&self) -> BTreeMap<ParamsProperty, Value> {
        self.property_map()
            .into_iter()
            .filter_map(|(key, value)| Some((ParamsProperty::from_key(&key)?, value)))
            .collect()
    }

    /// The JSON of the event (`EventSerializer.serialize`).
    pub fn to_value(&self) -> Value {
        json!({
            "type": self.event_type().as_str(),
            "modifiedBy": self.modified_by,
            "timestamp": self.timestamp,
            "properties": self.property_map(),
        })
    }

    /// Reads an event (`EventSerializer.deserialize`). Missing properties
    /// read as absent.
    pub fn from_value(value: &Value) -> Result<Event, String> {
        let type_name = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or("event without type")?;
        let event_type: EventType = type_name
            .to_uppercase()
            .parse()
            .map_err(|_| format!("Trying to deserialize event JSON with unknown type: {value}"))?;
        let unable = || format!("Unable to deserialize event JSON: {value}");
        let modified_by = value.get("modifiedBy").and_then(lenient::value_as_string);
        let timestamp = value
            .get("timestamp")
            .and_then(lenient::value_as_i64)
            .ok_or_else(unable)?;
        let empty = Map::new();
        let properties = match value.get("properties") {
            Some(Value::Object(properties)) => properties,
            _ => &empty,
        };
        let string = |key: &str| properties.get(key).and_then(lenient::value_as_string);
        fn typed<T: serde::de::DeserializeOwned + Default>(
            properties: &Map<String, Value>,
            key: &str,
        ) -> Result<T, String> {
            match properties.get(key) {
                None | Some(Value::Null) => Ok(T::default()),
                Some(value) => T::deserialize(value)
                    .map_err(|e| format!("Unable to deserialize event JSON: {key}: {e}")),
            }
        }
        let event_properties = match event_type {
            EventType::WaveletBlipCreated => EventProperties::WaveletBlipCreated {
                new_blip_id: string("newBlipId"),
            },
            EventType::WaveletBlipRemoved => EventProperties::WaveletBlipRemoved {
                removed_blip_id: string("removedBlipId"),
            },
            EventType::WaveletParticipantsChanged => EventProperties::WaveletParticipantsChanged {
                participants_added: typed(properties, "participantsAdded")?,
                participants_removed: typed(properties, "participantsRemoved")?,
            },
            EventType::WaveletSelfAdded => EventProperties::WaveletSelfAdded,
            EventType::WaveletSelfRemoved => EventProperties::WaveletSelfRemoved,
            EventType::WaveletTitleChanged => EventProperties::WaveletTitleChanged {
                title: string("title"),
            },
            EventType::WaveletCreated => EventProperties::WaveletCreated {
                message: string("message"),
                wave_id: string("waveId"),
                wavelet_id: string("waveletId"),
            },
            EventType::WaveletFetched => EventProperties::WaveletFetched {
                message: string("message"),
                wavelet_data: typed::<Option<Box<WaveletData>>>(properties, "waveletData")?,
                blips: typed(properties, "blips")?,
                threads: typed(properties, "threads")?,
            },
            EventType::WaveletTagsChanged => EventProperties::WaveletTagsChanged,
            EventType::BlipContributorsChanged => EventProperties::BlipContributorsChanged {
                contributors_added: typed(properties, "contributorsAdded")?,
                contributors_removed: typed(properties, "contributorsRemoved")?,
            },
            EventType::BlipSubmitted => EventProperties::BlipSubmitted,
            EventType::DocumentChanged => EventProperties::DocumentChanged,
            EventType::FormButtonClicked => EventProperties::FormButtonClicked {
                button_name: string("buttonName"),
            },
            EventType::GadgetStateChanged => EventProperties::GadgetStateChanged {
                index: properties
                    .get("index")
                    .and_then(lenient::value_as_i64)
                    .and_then(|i| i32::try_from(i).ok()),
                old_state: typed(properties, "oldState")?,
            },
            EventType::AnnotatedTextChanged => EventProperties::AnnotatedTextChanged {
                name: string("name"),
                value: string("value"),
            },
            EventType::OperationError => EventProperties::OperationError {
                operation_id: string("operationId"),
                message: string("message"),
            },
            EventType::Unknown => {
                return Err(format!(
                    "Trying to deserialize event JSON with unknown type: {value}"
                ));
            }
        };
        Ok(Event {
            modified_by,
            timestamp,
            blip_id: string("blipId"),
            properties: event_properties,
        })
    }
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        Event::from_value(&value).map_err(serde::de::Error::custom)
    }
}

/// The events of a wavelet for one robot, with the wavelet and the blips
/// and threads the events are about (`EventMessageBundle`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventMessageBundle {
    pub events: Vec<Event>,
    pub wavelet: Option<WaveletData>,
    pub blips: BTreeMap<String, BlipData>,
    pub threads: BTreeMap<String, BlipThread>,
    /// The robot's address without proxy and version parts.
    pub robot_address: String,
    /// Set if the events are for a proxying robot address
    /// (`robot+<proxyingFor>@domain`).
    pub proxying_for: Option<String>,
    pub rpc_server_url: String,
}

impl EventMessageBundle {
    pub fn new(robot_address: &str, rpc_server_url: &str) -> Self {
        EventMessageBundle {
            robot_address: robot_address.to_owned(),
            rpc_server_url: rpc_server_url.to_owned(),
            ..Default::default()
        }
    }

    /// `hasMessages`.
    pub fn has_messages(&self) -> bool {
        !self.events.is_empty()
    }

    pub fn has_event(&self, event_type: EventType) -> bool {
        self.events.iter().any(|e| e.event_type() == event_type)
    }

    pub fn has_blip_id(&self, id: &str) -> bool {
        self.blips.contains_key(id)
    }

    pub fn has_thread_id(&self, id: &str) -> bool {
        self.threads.contains_key(id)
    }

    /// The bundle as a given protocol version writes it: before 0.21,
    /// attachments are written as images (`ElementGsonAdaptorV2`).
    pub fn for_protocol(mut self, version: ProtocolVersion) -> Self {
        if version < ProtocolVersion::V2_1 {
            let adapt = |blips: &mut BTreeMap<String, BlipData>| {
                for blip in blips.values_mut() {
                    for element in blip.elements.values_mut() {
                        *element = element.to_v2();
                    }
                }
            };
            adapt(&mut self.blips);
            for event in &mut self.events {
                if let EventProperties::WaveletFetched { blips, .. } = &mut event.properties {
                    adapt(blips);
                }
            }
        }
        self
    }
}

impl Serialize for EventMessageBundle {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("events", &self.events)?;
        map.serialize_entry("wavelet", &self.wavelet)?;
        map.serialize_entry("blips", &self.blips)?;
        map.serialize_entry("threads", &self.threads)?;
        map.serialize_entry("robotAddress", &self.robot_address)?;
        if let Some(proxying_for) = self.proxying_for.as_deref().filter(|p| !p.is_empty()) {
            map.serialize_entry("proxyingFor", proxying_for)?;
        }
        if !self.rpc_server_url.is_empty() {
            map.serialize_entry("rpcServerUrl", &self.rpc_server_url)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for EventMessageBundle {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            events: Vec<Event>,
            #[serde(default)]
            wavelet: Option<WaveletData>,
            #[serde(default, deserialize_with = "lenient::null_default")]
            blips: BTreeMap<String, BlipData>,
            #[serde(default, deserialize_with = "lenient::null_default")]
            threads: BTreeMap<String, BlipThread>,
            robot_address: String,
            #[serde(default)]
            proxying_for: Option<String>,
            #[serde(default)]
            rpc_server_url: Option<String>,
        }
        let raw = Raw::deserialize(d)?;
        Ok(EventMessageBundle {
            events: raw.events,
            wavelet: raw.wavelet,
            blips: raw.blips,
            threads: raw.threads,
            robot_address: raw.robot_address,
            proxying_for: raw.proxying_for,
            rpc_server_url: raw.rpc_server_url.unwrap_or_default(),
        })
    }
}
