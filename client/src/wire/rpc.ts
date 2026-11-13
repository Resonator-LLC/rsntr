// Messages of `box/server/rpc/rpc.proto` (package `rpc`): the control
// messages of the client RPC channel, with their JSON codecs.
//
// Port of `crates/resonator-model/src/proto/rpc.rs`; derived from Apache Wave,
// licensed under the Apache License, Version 2.0.

import { type MessageCodec, defineMessage, field } from "./json";

/**
 * Sent by the client to ask that an RPC, streaming or not, be cancelled.
 * The server still finishes the RPC in the standard manner.
 */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type
export interface CancelRpc {}

export const CancelRpc: MessageCodec<CancelRpc> = defineMessage("CancelRpc", []);

/**
 * Sent by the server when a streaming RPC has finished (failed or not) or
 * when a normal RPC has failed.
 */
export interface RpcFinished {
  readonly failed: boolean;
  readonly errorText?: string;
}

export const RpcFinished: MessageCodec<RpcFinished> = defineMessage("RpcFinished", [
  field(1, "failed", "required", "bool"),
  field(2, "errorText", "optional", "string"),
]);

/** A streaming RPC that ended normally. */
export function rpcFinishedSuccess(): RpcFinished {
  return { failed: false };
}

/** An RPC that failed with `errorText`. */
export function rpcFinishedFailure(errorText: string): RpcFinished {
  return { failed: true, errorText };
}
