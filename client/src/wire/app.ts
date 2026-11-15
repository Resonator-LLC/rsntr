// Wave app messages of the client socket, with their JSON codecs: opening
// an app's view (a stream of `AppUpdate`s, ended by `CancelRpc`) and sending
// it UI events.
//
// Port of `crates/resonator-model/src/proto/app.rs`. Not in the original
// protocol.

import { type MessageCodec, defineMessage, field } from "./json";

export interface AppOpenRequest {
  /** The serialised wave id. */
  readonly waveId: string;
  readonly gadgetId: string;
  /** The view to render; 0 if missing. */
  readonly view?: number;
}

export const AppOpenRequest: MessageCodec<AppOpenRequest> = defineMessage("AppOpenRequest", [
  field(1, "waveId", "required", "string"),
  field(2, "gadgetId", "required", "string"),
  field(3, "view", "optional", "int32"),
]);

export interface AppPair {
  readonly name: string;
  readonly value: string;
}

export const AppPair: MessageCodec<AppPair> = defineMessage("AppPair", [
  field(1, "name", "required", "string"),
  field(2, "value", "required", "string"),
]);

/** One node of a rendered view, in document order. */
export interface AppUiNode {
  /** The index of the parent node; missing for a root. */
  readonly parent?: number;
  readonly tag: string;
  readonly text?: string;
  readonly attrs: readonly AppPair[];
  /** The events the app wants from this node. */
  readonly on: readonly string[];
}

export const AppUiNode: MessageCodec<AppUiNode> = defineMessage("AppUiNode", [
  field(1, "parent", "optional", "int32"),
  field(2, "tag", "required", "string"),
  field(3, "text", "optional", "string"),
  field(4, "attrs", "repeated", AppPair),
  field(5, "on", "repeated", "string"),
]);

export type AppStatus = "needs-grant" | "loading" | "running" | "failed" | "stopped";

/** A receiving track of the viewer's WebRTC connection, and the app channel it carries. */
export interface AppRtcSlot {
  readonly mid: string;
  readonly channel: number;
}

export const AppRtcSlot: MessageCodec<AppRtcSlot> = defineMessage("AppRtcSlot", [
  field(1, "mid", "required", "string"),
  field(2, "channel", "required", "int32"),
]);

export interface AppUpdate {
  readonly status: string;
  readonly reason?: string;
  /** `sha256:<hex>`. */
  readonly appId?: string;
  readonly name?: string;
  readonly permissions: readonly string[];
  readonly nodes: readonly AppUiNode[];
  /** With status `media`: frames for the speaker (base64; a channel byte, then Opus). */
  readonly media: readonly string[];
  /** With status `media`: video for the screen (base64; a channel byte, a flags byte, then a frame). */
  readonly video: readonly string[];
  /** With status `slots`: which receiving track carries which channel, all of them. */
  readonly slots: readonly AppRtcSlot[];
}

export const AppUpdate: MessageCodec<AppUpdate> = defineMessage("AppUpdate", [
  field(1, "status", "required", "string"),
  field(2, "reason", "optional", "string"),
  field(3, "appId", "optional", "string"),
  field(4, "name", "optional", "string"),
  field(5, "permissions", "repeated", "string"),
  field(6, "nodes", "repeated", AppUiNode),
  field(7, "media", "repeated", "string"),
  field(8, "video", "repeated", "string"),
  field(9, "slots", "repeated", AppRtcSlot),
]);

/** The viewer's WebRTC offer for an app open on this connection. */
export interface AppRtcRequest {
  readonly waveId: string;
  readonly gadgetId: string;
  /** The SDP offer. */
  readonly offer: string;
}

export const AppRtcRequest: MessageCodec<AppRtcRequest> = defineMessage("AppRtcRequest", [
  field(1, "waveId", "required", "string"),
  field(2, "gadgetId", "required", "string"),
  field(3, "offer", "required", "string"),
]);

export interface AppRtcResponse {
  /** The SDP answer, unless refused. */
  readonly answer?: string;
  readonly errorMessage?: string;
}

export const AppRtcResponse: MessageCodec<AppRtcResponse> = defineMessage("AppRtcResponse", [
  field(1, "answer", "optional", "string"),
  field(2, "errorMessage", "optional", "string"),
]);

/**
 * A frame of the viewer's microphone (one Opus frame, base64), or with
 * `video` of their camera (a flags byte, then an encoded frame).
 */
export interface AppMediaRequest {
  readonly waveId: string;
  readonly gadgetId: string;
  readonly data: string;
  readonly video?: boolean;
}

export const AppMediaRequest: MessageCodec<AppMediaRequest> = defineMessage("AppMediaRequest", [
  field(1, "waveId", "required", "string"),
  field(2, "gadgetId", "required", "string"),
  field(3, "data", "required", "string"),
  field(4, "video", "optional", "bool"),
]);

export interface AppEventRequest {
  readonly waveId: string;
  readonly gadgetId: string;
  readonly view?: number;
  /** The index of the node in the last render. */
  readonly node: number;
  readonly name: string;
  readonly value?: string;
  /** The fields of a submitted form. */
  readonly fields: readonly AppPair[];
}

export const AppEventRequest: MessageCodec<AppEventRequest> = defineMessage("AppEventRequest", [
  field(1, "waveId", "required", "string"),
  field(2, "gadgetId", "required", "string"),
  field(3, "view", "optional", "int32"),
  field(4, "node", "required", "int32"),
  field(5, "name", "required", "string"),
  field(6, "value", "optional", "string"),
  field(7, "fields", "repeated", AppPair),
]);

export interface AppEventResponse {
  readonly errorMessage?: string;
}

export const AppEventResponse: MessageCodec<AppEventResponse> = defineMessage("AppEventResponse", [
  field(1, "errorMessage", "optional", "string"),
]);
