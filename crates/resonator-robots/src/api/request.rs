//! JSON-RPC requests and responses of the Robots API (`OperationRequest`,
//! `OperationType`, `JsonRpcConstant.ParamsProperty`, `JsonRpcResponse`,
//! their Gson adaptors, and `RobotSerializer.deserializeOperations`).

use std::collections::BTreeMap;
use std::fmt;

use serde::de::DeserializeOwned;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use super::lenient::{self, ByteArrays};
use super::types::{
    Annotation, BlipData, DocumentModifyAction, DocumentModifyQuery, Element, FetchProfilesRequest,
    Range, RawAttachmentData, WaveletData,
};
use resonator_server::account::ProtocolVersion;

macro_rules! operation_types {
    ($($variant:ident => $method:literal),* $(,)?) => {
        /// The operations of the API (`OperationType`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum OperationType {
            $($variant),*
        }

        impl OperationType {
            pub const ALL: &'static [OperationType] = &[$(OperationType::$variant),*];

            /// The JSON-RPC method name.
            pub fn method(self) -> &'static str {
                match self {
                    $(OperationType::$variant => $method),*
                }
            }

            /// `OperationType.fromMethodName`: unknown names give
            /// [`OperationType::Unknown`].
            pub fn from_method_name(name: &str) -> OperationType {
                match name {
                    $($method => OperationType::$variant,)*
                    _ => OperationType::Unknown,
                }
            }
        }
    };
}

operation_types! {
    Unknown => "unknown",
    WaveletAppendBlip => "wavelet.appendBlip",
    WaveletCreate => "wavelet.create",
    WaveletRemoveSelf => "wavelet.removeSelf",
    WaveletSetTitle => "wavelet.setTitle",
    WaveletAddParticipantNewsyntax => "wavelet.addParticipant",
    WaveletRemoveParticipantNewsyntax => "wavelet.removeParticipant",
    WaveletAppendDatadoc => "wavelet.appendDatadoc",
    WaveletSetDatadoc => "wavelet.setDatadoc",
    WaveletModifyTag => "wavelet.modifyTag",
    WaveletModifyParticipantRole => "wavelet.modifyParticipantRole",
    BlipContinueThread => "blip.continueThread",
    BlipCreateChild => "blip.createChild",
    BlipDelete => "blip.delete",
    BlipSetAuthor => "blip.setAuthor",
    BlipSetCreationTime => "blip.setCreationTime",
    DocumentDeleteAnnotation => "document.deleteAnnotation",
    DocumentSetAnnotation => "document.setAnnotation",
    DocumentSetAnnotationNorange => "document.setAnnotationNoRange",
    DocumentAppend => "document.append",
    DocumentAppendMarkup => "document.appendMarkup",
    DocumentAppendStyledText => "document.appendStyledText",
    DocumentDelete => "document.delete",
    DocumentInsert => "document.insert",
    DocumentModify => "document.modify",
    DocumentReplace => "document.replace",
    DocumentAppendElement => "document.appendElement",
    DocumentDeleteElement => "document.deleteElement",
    DocumentInsertElement => "document.insertElement",
    DocumentInsertElementAfter => "document.insertElementAfter",
    DocumentInsertElementBefore => "document.insertElementBefore",
    DocumentModifyElementAttrs => "document.modifyElementAttrs",
    DocumentReplaceElement => "document.replaceElement",
    DocumentAppendInlineBlip => "document.appendInlineBlip",
    DocumentInsertInlineBlip => "document.insertInlineBlip",
    DocumentInsertInlineBlipAfterElement => "document.insertInlineBlipAfterElement",
    RobotFolderAction => "robot.folderAction",
    RobotCreateWavelet => "robot.createWavelet",
    RobotFetchMyProfile => "robot.fetchMyProfile",
    RobotFetchProfiles => "robot.fetchProfiles",
    RobotFetchWave => "robot.fetchWave",
    RobotNotify => "robot.notify",
    RobotSearch => "robot.search",
    RobotExportSnapshot => "robot.exportSnapshot",
    RobotExportDeltas => "robot.exportDeltas",
    RobotExportAttachment => "robot.exportAttachment",
    RobotImportSnapshot => "robot.importSnapshot",
    RobotImportDeltas => "robot.importDeltas",
    RobotImportAttachment => "robot.importAttachment",
    // Deprecated since protocol 0.21.
    WaveletAddParticipant => "wavelet.participant.add",
    WaveletRemoveParticipant => "wavelet.participant.remove",
    WaveletDatadocAppend => "wavelet.datadoc.append",
    WaveletDatadocSet => "wavelet.datadoc.set",
    DocumentAnnotationDelete => "document.annotation.delete",
    DocumentAnnotationSet => "document.annotation.set",
    DocumentAnnotationSetNorange => "document.annotation.setNoRange",
    DocumentElementAppend => "document.element.append",
    DocumentElementDelete => "document.element.delete",
    DocumentElementInsert => "document.element.insert",
    DocumentElementInsertAfter => "document.element.insertAfter",
    DocumentElementInsertBefore => "document.element.insertBefore",
    DocumentElementModifyAttrs => "document.element.modifyAttrs",
    DocumentElementReplace => "document.element.replace",
    DocumentInlineBlipAppend => "document.inlineBlip.append",
    DocumentInlineBlipInsert => "document.inlineBlip.insert",
    DocumentInlineBlipInsertAfterElement => "document.inlineBlip.insertAfterElement",
    RobotNotifyCapabilitiesHash => "robot.notifyCapabilitiesHash",
}

impl fmt::Display for OperationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.method())
    }
}

/// The JSON type of a parameter, i.e. the class Gson deserializes it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    String,
    Integer,
    Long,
    Boolean,
    /// Base64 bytes.
    Bytes,
    /// A list of base64 byte strings.
    BytesList,
    /// A list of any JSON values.
    List,
    /// A JSON object with any content.
    Map,
    Annotation,
    BlipData,
    Element,
    FetchProfilesRequest,
    ModifyAction,
    ModifyQuery,
    Range,
    WaveletData,
    AttachmentData,
    /// A JSON object the server only produces, never reads.
    Object,
}

macro_rules! params_properties {
    ($($variant:ident => ($name:literal, $key:literal, $kind:ident)),* $(,)?) => {
        /// The parameters of requests and the properties of responses
        /// (`JsonRpcConstant.ParamsProperty`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum ParamsProperty {
            $($variant),*
        }

        impl ParamsProperty {
            pub const ALL: &'static [ParamsProperty] = &[$(ParamsProperty::$variant),*];

            /// The JSON key.
            pub fn key(self) -> &'static str {
                match self {
                    $(ParamsProperty::$variant => $key),*
                }
            }

            /// The name of the original's enum constant, which error
            /// messages show.
            pub fn name(self) -> &'static str {
                match self {
                    $(ParamsProperty::$variant => $name),*
                }
            }

            pub fn kind(self) -> ParamKind {
                match self {
                    $(ParamsProperty::$variant => ParamKind::$kind),*
                }
            }

            /// `ParamsProperty.fromKey`.
            pub fn from_key(key: &str) -> Option<ParamsProperty> {
                match key {
                    $($key => Some(ParamsProperty::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

params_properties! {
    WaveId => ("WAVE_ID", "waveId", String),
    WaveletId => ("WAVELET_ID", "waveletId", String),
    BlipId => ("BLIP_ID", "blipId", String),
    AttachmentId => ("ATTACHMENT_ID", "attachmentId", String),
    Annotation => ("ANNOTATION", "annotation", Annotation),
    BlipAuthor => ("BLIP_AUTHOR", "blipAuthor", String),
    BlipCreationTime => ("BLIP_CREATION_TIME", "blipCreationTime", Long),
    BlipData => ("BLIP_DATA", "blipData", BlipData),
    Blips => ("BLIPS", "blips", Map),
    CapabilitiesHash => ("CAPABILITIES_HASH", "capabilitiesHash", String),
    ChildBlipId => ("CHILD_BLIP_ID", "childBlipId", String),
    Content => ("CONTENT", "content", String),
    DatadocName => ("DATADOC_NAME", "datadocName", String),
    DatadocValue => ("DATADOC_VALUE", "datadocValue", String),
    DatadocWriteback => ("DATADOC_WRITEBACK", "datadocWriteback", String),
    Element => ("ELEMENT", "element", Element),
    FetchProfilesRequest => ("FETCH_PROFILES_REQUEST", "fetchProfilesRequest", FetchProfilesRequest),
    FetchProfilesResult => ("FETCH_PROFILES_RESULT", "fetchProfilesResult", Object),
    Index => ("INDEX", "index", Integer),
    Language => ("LANGUAGE", "language", String),
    Message => ("MESSAGE", "message", String),
    ModifyAction => ("MODIFY_ACTION", "modifyAction", ModifyAction),
    ModifyHow => ("MODIFY_HOW", "modifyHow", String),
    ModifyQuery => ("MODIFY_QUERY", "modifyQuery", ModifyQuery),
    Name => ("NAME", "name", String),
    NewBlipId => ("NEW_BLIP_ID", "newBlipId", String),
    NumResults => ("NUM_RESULTS", "numResults", Integer),
    ParticipantId => ("PARTICIPANT_ID", "participantId", String),
    ParticipantProfile => ("PARTICIPANT_PROFILE", "participantProfile", Object),
    ParticipantRole => ("PARTICIPANT_ROLE", "participantRole", String),
    ParticipantsAdded => ("PARTICIPANTS_ADDED", "participantsAdded", List),
    ParticipantsRemoved => ("PARTICIPANTS_REMOVED", "participantsRemoved", List),
    ProtocolVersion => ("PROTOCOL_VERSION", "protocolVersion", String),
    ProxyingFor => ("PROXYING_FOR", "proxyingFor", String),
    Query => ("QUERY", "query", String),
    Range => ("RANGE", "range", Range),
    SearchResults => ("SEARCH_RESULTS", "searchResults", Object),
    StyleType => ("STYLE_TYPE", "styleType", String),
    Threads => ("THREADS", "threads", Map),
    WaveletData => ("WAVELET_DATA", "waveletData", WaveletData),
    WaveletTitle => ("WAVELET_TITLE", "waveletTitle", String),
    ReturnWaveletIds => ("RETURN_WAVELET_IDS", "returnWaveletIds", Boolean),
    WaveletIds => ("WAVELET_IDS", "waveletIds", List),
    RawSnapshot => ("RAW_SNAPSHOT", "rawSnapshot", String),
    FromVersion => ("FROM_VERSION", "fromVersion", Bytes),
    ToVersion => ("TO_VERSION", "toVersion", Bytes),
    RawDeltas => ("RAW_DELTAS", "rawDeltas", BytesList),
    TargetVersion => ("TARGET_VERSION", "targetVersion", Bytes),
    AttachmentData => ("ATTACHMENT_DATA", "attachmentData", AttachmentData),
    ImportedFromVersion => ("IMPORTED_FROM_VERSION", "importedFromVersion", Long),
}

impl fmt::Display for ParamsProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl ParamKind {
    /// The JSON Gson would write after deserializing `value` into the
    /// parameter's class, or `None` if it would fail to. This is how a
    /// parameter is kept: a number sent as a string becomes a number, a
    /// structure gets all its fields.
    pub fn canonicalize(self, value: &Value) -> Option<Value> {
        fn typed<T: DeserializeOwned + Serialize>(value: &Value) -> Option<Value> {
            serde_json::to_value(T::deserialize(value).ok()?).ok()
        }
        let structured = !matches!(
            self,
            ParamKind::String
                | ParamKind::Integer
                | ParamKind::Long
                | ParamKind::Boolean
                | ParamKind::Bytes
                | ParamKind::BytesList
                | ParamKind::List
        );
        if structured && !value.is_object() {
            return None;
        }
        match self {
            ParamKind::String => lenient::value_as_string(value).map(Value::String),
            ParamKind::Integer => lenient::value_as_i64(value)
                .filter(|v| i32::try_from(*v).is_ok())
                .map(Value::from),
            ParamKind::Long => lenient::value_as_i64(value).map(Value::from),
            ParamKind::Boolean => lenient::value_as_bool(value).map(Value::Bool),
            ParamKind::Bytes => value.as_str().map(|s| {
                Value::String(lenient::base64_bytes::encode(
                    &lenient::base64_bytes::decode(s),
                ))
            }),
            ParamKind::BytesList => typed::<ByteArrays>(value),
            ParamKind::List => value.is_array().then(|| value.clone()),
            ParamKind::Map | ParamKind::Object => value.is_object().then(|| value.clone()),
            ParamKind::Annotation => typed::<Annotation>(value),
            ParamKind::BlipData => typed::<BlipData>(value),
            ParamKind::Element => typed::<Element>(value),
            ParamKind::FetchProfilesRequest => typed::<FetchProfilesRequest>(value),
            ParamKind::ModifyAction => typed::<DocumentModifyAction>(value),
            ParamKind::ModifyQuery => typed::<DocumentModifyQuery>(value),
            ParamKind::Range => typed::<Range>(value),
            ParamKind::WaveletData => typed::<WaveletData>(value),
            ParamKind::AttachmentData => typed::<RawAttachmentData>(value),
        }
    }
}

/// A request that cannot be read or executed (`InvalidRequestException`).
/// The message is what the client gets as the operation's error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct InvalidRequest {
    pub message: String,
}

impl InvalidRequest {
    pub fn new(message: impl Into<String>) -> Self {
        InvalidRequest {
            message: message.into(),
        }
    }
}

/// One operation of a request batch (`OperationRequest`).
///
/// Parameters are kept as JSON and read with typed accessors. As in the
/// original, parameters with unknown keys are dropped when a request is
/// read from JSON, and absent and `null` parameters are the same thing.
/// A request read from JSON holds its parameters as Gson would write them
/// back, see [`ParamKind::canonicalize`].
#[derive(Debug, Clone, PartialEq)]
pub struct OperationRequest {
    pub method: String,
    pub id: String,
    params: BTreeMap<ParamsProperty, Value>,
}

impl OperationRequest {
    pub fn new(method: impl Into<String>, id: impl Into<String>) -> Self {
        OperationRequest {
            method: method.into(),
            id: id.into(),
            params: BTreeMap::new(),
        }
    }

    /// A request of an operation type.
    pub fn of(operation: OperationType, id: impl Into<String>) -> Self {
        Self::new(operation.method(), id)
    }

    /// A request of an operation type addressed to a wavelet.
    pub fn for_wavelet(
        operation: OperationType,
        id: impl Into<String>,
        wave_id: &str,
        wavelet_id: &str,
    ) -> Self {
        Self::of(operation, id)
            .with(ParamsProperty::WaveId, wave_id)
            .with(ParamsProperty::WaveletId, wavelet_id)
    }

    /// A request of an operation type addressed to a blip.
    pub fn for_blip(
        operation: OperationType,
        id: impl Into<String>,
        wave_id: &str,
        wavelet_id: &str,
        blip_id: &str,
    ) -> Self {
        Self::for_wavelet(operation, id, wave_id, wavelet_id).with(ParamsProperty::BlipId, blip_id)
    }

    /// Adds a parameter (builder style). A value that serializes to `null`
    /// removes the parameter.
    pub fn with(mut self, property: ParamsProperty, value: impl Serialize) -> Self {
        self.set(property, value);
        self
    }

    /// Sets a parameter (`addParameter`). A value that cannot be turned
    /// into JSON, or serializes to `null`, removes the parameter.
    pub fn set(&mut self, property: ParamsProperty, value: impl Serialize) {
        match serde_json::to_value(value) {
            Ok(value) if !value.is_null() => {
                self.params.insert(property, value);
            }
            _ => {
                self.params.remove(&property);
            }
        }
    }

    /// Sets a `byte[]` parameter.
    pub fn set_bytes(&mut self, property: ParamsProperty, bytes: &[u8]) {
        self.set(property, lenient::base64_bytes::encode(bytes));
    }

    pub fn remove(&mut self, property: ParamsProperty) -> Option<Value> {
        self.params.remove(&property)
    }

    /// The raw JSON of a parameter.
    pub fn raw(&self, property: ParamsProperty) -> Option<&Value> {
        self.params.get(&property).filter(|v| !v.is_null())
    }

    pub fn params(&self) -> impl Iterator<Item = (ParamsProperty, &Value)> {
        self.params.iter().map(|(k, v)| (*k, v))
    }

    /// The operation type of the method, without the `wave.` namespace
    /// some clients prefix (`OperationUtil.getOperationType`).
    pub fn operation_type(&self) -> OperationType {
        let method = self.method.strip_prefix("wave.").unwrap_or(&self.method);
        OperationType::from_method_name(method)
    }

    pub fn wave_id(&self) -> Option<String> {
        self.string(ParamsProperty::WaveId)
    }

    pub fn wavelet_id(&self) -> Option<String> {
        self.string(ParamsProperty::WaveletId)
    }

    pub fn blip_id(&self) -> Option<String> {
        self.string(ParamsProperty::BlipId)
    }

    /// A string parameter, if present and string-like.
    pub fn string(&self, property: ParamsProperty) -> Option<String> {
        lenient::value_as_string(self.raw(property)?)
    }

    pub fn integer(&self, property: ParamsProperty) -> Option<i32> {
        i32::try_from(lenient::value_as_i64(self.raw(property)?)?).ok()
    }

    pub fn long(&self, property: ParamsProperty) -> Option<i64> {
        lenient::value_as_i64(self.raw(property)?)
    }

    pub fn boolean(&self, property: ParamsProperty) -> Option<bool> {
        lenient::value_as_bool(self.raw(property)?)
    }

    pub fn bytes(&self, property: ParamsProperty) -> Option<Vec<u8>> {
        Some(lenient::base64_bytes::decode(self.raw(property)?.as_str()?))
    }

    /// A structured parameter, if present and well-formed
    /// (`OperationUtil.getOptionalParameter`).
    pub fn typed<T: DeserializeOwned>(&self, property: ParamsProperty) -> Option<T> {
        T::deserialize(self.raw(property)?).ok()
    }

    fn not_found(property: ParamsProperty) -> InvalidRequest {
        InvalidRequest::new(format!("property {property} not found"))
    }

    /// `OperationUtil.getRequiredParameter` for a string.
    pub fn required_string(&self, property: ParamsProperty) -> Result<String, InvalidRequest> {
        self.string(property)
            .ok_or_else(|| Self::not_found(property))
    }

    pub fn required_integer(&self, property: ParamsProperty) -> Result<i32, InvalidRequest> {
        self.integer(property)
            .ok_or_else(|| Self::not_found(property))
    }

    pub fn required_bytes(&self, property: ParamsProperty) -> Result<Vec<u8>, InvalidRequest> {
        self.bytes(property)
            .ok_or_else(|| Self::not_found(property))
    }

    /// `OperationUtil.getRequiredParameter` for a structured parameter.
    pub fn required<T: DeserializeOwned>(
        &self,
        property: ParamsProperty,
    ) -> Result<T, InvalidRequest> {
        self.typed(property)
            .ok_or_else(|| Self::not_found(property))
    }

    /// Reads a request from JSON like `OperationRequestGsonAdaptor`: the
    /// object must have `method`, `id` and `params`; parameters with
    /// unknown keys are dropped; a known parameter of the wrong type is an
    /// error.
    pub fn from_value(value: &Value) -> Result<OperationRequest, InvalidRequest> {
        let Some(object) = value.as_object() else {
            return Err(InvalidRequest::new(format!(
                "The incoming JSON is not a JSON object: {value}"
            )));
        };
        let missing: String = ["method", "id", "params"]
            .into_iter()
            .filter(|key| !object.contains_key(*key))
            .collect();
        if !missing.is_empty() {
            return Err(InvalidRequest::new(format!(
                "Missing required properties {missing}operation: {value}"
            )));
        }
        let invalid =
            || InvalidRequest::new(format!("Couldn't deserialize incoming operations: {value}"));
        let method = lenient::value_as_string(&object["method"]).ok_or_else(invalid)?;
        let id = lenient::value_as_string(&object["id"]).ok_or_else(invalid)?;
        let params = object["params"].as_object().ok_or_else(invalid)?;
        let mut request = OperationRequest::new(method, id);
        for (key, value) in params {
            let Some(property) = ParamsProperty::from_key(key) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            let canonical = property.kind().canonicalize(value).ok_or_else(invalid)?;
            request.params.insert(property, canonical);
        }
        Ok(request)
    }

    /// The JSON of the request, with a namespace before the method name
    /// (`OperationRequestGsonAdaptor(namespace)`; clients of the active API
    /// use `wave`).
    pub fn to_value_with_namespace(&self, namespace: &str) -> Value {
        let method = if namespace.is_empty() || namespace.ends_with('.') {
            format!("{namespace}{}", self.method)
        } else {
            format!("{namespace}.{}", self.method)
        };
        let params: serde_json::Map<String, Value> = self
            .params
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.key().to_owned(), v.clone()))
            .collect();
        let mut object = serde_json::Map::new();
        object.insert("method".to_owned(), Value::String(method));
        object.insert("id".to_owned(), Value::String(self.id.clone()));
        object.insert("params".to_owned(), Value::Object(params));
        Value::Object(object)
    }

    pub fn to_value(&self) -> Value {
        self.to_value_with_namespace("")
    }
}

impl Serialize for OperationRequest {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for OperationRequest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        OperationRequest::from_value(&value).map_err(serde::de::Error::custom)
    }
}

/// Reads a request body: one operation or an array of them; an empty body
/// is no operations (`RobotSerializer.deserializeOperations`).
pub fn deserialize_operations(json: &str) -> Result<Vec<OperationRequest>, InvalidRequest> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(json).map_err(|_| {
        InvalidRequest::new(format!("Couldn't deserialize incoming operations: {json}"))
    })?;
    match &value {
        Value::Array(items) => items.iter().map(OperationRequest::from_value).collect(),
        other => Ok(vec![OperationRequest::from_value(other)?]),
    }
}

/// The protocol version a batch of operations declares: that of a leading
/// `robot.notify`, or the default (`OperationUtil.getProtocolVersion`).
pub fn protocol_version_of(operations: &[OperationRequest]) -> ProtocolVersion {
    let Some(first) = operations.first() else {
        return ProtocolVersion::DEFAULT;
    };
    if first.method != OperationType::RobotNotify.method() {
        return ProtocolVersion::DEFAULT;
    }
    first
        .string(ParamsProperty::ProtocolVersion)
        .and_then(|version| version.parse().ok())
        .unwrap_or(ProtocolVersion::DEFAULT)
}

/// The response to one operation (`JsonRpcResponse`): data or an error
/// message.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonRpcResponse {
    pub id: String,
    result: Result<BTreeMap<ParamsProperty, Value>, String>,
}

impl JsonRpcResponse {
    pub fn error(id: impl Into<String>, message: impl Into<String>) -> Self {
        JsonRpcResponse {
            id: id.into(),
            result: Err(message.into()),
        }
    }

    pub fn result(id: impl Into<String>, data: BTreeMap<ParamsProperty, Value>) -> Self {
        JsonRpcResponse {
            id: id.into(),
            result: Ok(data),
        }
    }

    pub fn is_error(&self) -> bool {
        self.result.is_err()
    }

    pub fn error_message(&self) -> Option<&str> {
        self.result.as_ref().err().map(String::as_str)
    }

    /// The data of a successful response. A property can be present with
    /// the value `null`.
    pub fn data(&self) -> Option<&BTreeMap<ParamsProperty, Value>> {
        self.result.as_ref().ok()
    }

    /// One property of the data; `None` if absent or `null`.
    pub fn get(&self, property: ParamsProperty) -> Option<&Value> {
        self.data()?.get(&property).filter(|v| !v.is_null())
    }

    pub fn string(&self, property: ParamsProperty) -> Option<&str> {
        self.get(property)?.as_str()
    }

    /// One property of the data as a typed value.
    pub fn typed<T: DeserializeOwned>(&self, property: ParamsProperty) -> Option<T> {
        T::deserialize(self.get(property)?).ok()
    }

    pub fn from_value(value: &Value) -> Result<JsonRpcResponse, InvalidRequest> {
        let invalid = || InvalidRequest::new(format!("Couldn't deserialize response: {value}"));
        let object = value.as_object().ok_or_else(invalid)?;
        let id = object
            .get("id")
            .and_then(lenient::value_as_string)
            .ok_or_else(invalid)?;
        if let Some(error) = object.get("error") {
            let message = error
                .get("message")
                .and_then(lenient::value_as_string)
                .ok_or_else(invalid)?;
            return Ok(JsonRpcResponse::error(id, message));
        }
        let mut data = BTreeMap::new();
        if let Some(Value::Object(properties)) = object.get("data") {
            for (key, value) in properties {
                if let Some(property) = ParamsProperty::from_key(key) {
                    data.insert(property, value.clone());
                }
            }
        }
        Ok(JsonRpcResponse::result(id, data))
    }
}

impl Serialize for JsonRpcResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        struct Data<'a>(&'a BTreeMap<ParamsProperty, Value>);
        impl Serialize for Data<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let mut map = s.serialize_map(Some(self.0.len()))?;
                for (key, value) in self.0 {
                    map.serialize_entry(key.key(), value)?;
                }
                map.end()
            }
        }
        #[derive(Serialize)]
        struct ErrorBody<'a> {
            message: &'a str,
        }
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("id", &self.id)?;
        match &self.result {
            Ok(data) => map.serialize_entry("data", &Data(data))?,
            Err(message) => map.serialize_entry("error", &ErrorBody { message })?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for JsonRpcResponse {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        JsonRpcResponse::from_value(&value).map_err(serde::de::Error::custom)
    }
}
