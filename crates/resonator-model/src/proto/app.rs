//! Wave app messages of the client socket: opening an app's view (a
//! stream of [`AppUpdate`]s, ended by `CancelRpc`) and sending it UI
//! events. Not in the original protocol.

use super::wave_message;

/// Opens the view of a gadget's app. Answered by a stream of
/// [`AppUpdate`]s on the same sequence number.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AppOpenRequest {
    /// Required. The serialised wave id.
    #[prost(string, optional, tag = "1")]
    pub wave_id: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "2")]
    pub gadget_id: Option<String>,
    /// The view to render; 0 if missing.
    #[prost(int32, optional, tag = "3")]
    pub view: Option<i32>,
}
wave_message!(AppOpenRequest {
    required: [wave_id, gadget_id],
});

/// A name and a value.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AppPair {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "2")]
    pub value: Option<String>,
}
wave_message!(AppPair {
    required: [name, value],
});

/// One node of a rendered view, in document order.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AppUiNode {
    /// The index of the parent node; missing for a root.
    #[prost(int32, optional, tag = "1")]
    pub parent: Option<i32>,
    /// Required.
    #[prost(string, optional, tag = "2")]
    pub tag: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub text: Option<String>,
    #[prost(message, repeated, tag = "4")]
    pub attrs: Vec<AppPair>,
    /// The events the app wants from this node.
    #[prost(string, repeated, tag = "5")]
    pub on: Vec<String>,
}
wave_message!(AppUiNode {
    required: [tag],
    repeated: [attrs],
});

/// The state of an open app and, when it is running, its view.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AppUpdate {
    /// Required: `needs-grant`, `loading`, `running`, `failed` or
    /// `stopped`.
    #[prost(string, optional, tag = "1")]
    pub status: Option<String>,
    /// Why it failed or does not run.
    #[prost(string, optional, tag = "2")]
    pub reason: Option<String>,
    /// The app id (`sha256:<hex>`).
    #[prost(string, optional, tag = "3")]
    pub app_id: Option<String>,
    /// The manifest's name.
    #[prost(string, optional, tag = "4")]
    pub name: Option<String>,
    /// The manifest's permissions (for the grant prompt).
    #[prost(string, repeated, tag = "5")]
    pub permissions: Vec<String>,
    /// The view, when running.
    #[prost(message, repeated, tag = "6")]
    pub nodes: Vec<AppUiNode>,
}
wave_message!(AppUpdate {
    required: [status],
    repeated: [nodes],
});

/// A UI event for an open app.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AppEventRequest {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wave_id: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "2")]
    pub gadget_id: Option<String>,
    #[prost(int32, optional, tag = "3")]
    pub view: Option<i32>,
    /// Required. The index of the node in the last render.
    #[prost(int32, optional, tag = "4")]
    pub node: Option<i32>,
    /// Required. `click`, `input`, `change`, `submit`, ...
    #[prost(string, optional, tag = "5")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "6")]
    pub value: Option<String>,
    /// The fields of a submitted form.
    #[prost(message, repeated, tag = "7")]
    pub fields: Vec<AppPair>,
}
wave_message!(AppEventRequest {
    required: [wave_id, gadget_id, node, name],
    repeated: [fields],
});

/// The answer to an [`AppEventRequest`].
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AppEventResponse {
    /// Set if the event was not delivered.
    #[prost(string, optional, tag = "1")]
    pub error_message: Option<String>,
}
wave_message!(AppEventResponse {});
