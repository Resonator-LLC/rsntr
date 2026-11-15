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

export interface AppUpdate {
  readonly status: string;
  readonly reason?: string;
  /** `sha256:<hex>`. */
  readonly appId?: string;
  readonly name?: string;
  readonly permissions: readonly string[];
  readonly nodes: readonly AppUiNode[];
}

export const AppUpdate: MessageCodec<AppUpdate> = defineMessage("AppUpdate", [
  field(1, "status", "required", "string"),
  field(2, "reason", "optional", "string"),
  field(3, "appId", "optional", "string"),
  field(4, "name", "optional", "string"),
  field(5, "permissions", "repeated", "string"),
  field(6, "nodes", "repeated", AppUiNode),
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
