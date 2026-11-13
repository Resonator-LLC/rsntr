// The wave protocol transport over the WebSocket RPC channel.
//
// Corresponds to `WaveWebSocketClient` of
// `org.waveprotocol.box.webclient.client` in Apache Wave, licensed under the
// Apache License, Version 2.0.

import { ClosedError, DisconnectedError, type RpcChannel, RpcFailedError } from "../net";
import type { Unsubscribe } from "../util/events";
import {
  ProtocolOpenRequest,
  ProtocolSubmitRequest,
  ProtocolSubmitResponse,
  ProtocolWaveletUpdate,
} from "../wire";
import type { ViewStream, ViewStreamHandler, WaveRpcTransport } from "./waveRpc";

export class RpcWaveTransport implements WaveRpcTransport {
  constructor(private readonly channel: RpcChannel) {}

  isConnected(): boolean {
    return this.channel.state === "connected";
  }

  onConnectionChange(listener: (connected: boolean) => void): Unsubscribe {
    return this.channel.events.on("state", ({ state, previous }) => {
      const now = state === "connected";
      if (now !== (previous === "connected")) {
        listener(now);
      }
    });
  }

  openView(request: ProtocolOpenRequest, handler: ViewStreamHandler): ViewStream {
    const stream = this.channel.openStream(ProtocolOpenRequest, request, ProtocolWaveletUpdate, {
      onMessage: (update) => handler.onUpdate(update),
      onFinished: (error) => handler.onFinished(error),
      onDisconnected: (info) => handler.onDisconnected(info.reason || `connection closed (${info.code})`),
    });
    return { cancel: () => stream.cancel() };
  }

  submit(
    request: ProtocolSubmitRequest,
    onResponse: (response: ProtocolSubmitResponse) => void,
    onFailure: (reason: string) => void,
    onPending?: () => void,
  ): void {
    try {
      this.channel.callWithCallback(
        ProtocolSubmitRequest,
        request,
        ProtocolSubmitResponse,
        onResponse,
        (error) => {
          if (error instanceof RpcFailedError) {
            // The server refused the request as such: an answer, not a
            // network failure, so retrying the same delta will not help.
            onResponse({ operationsApplied: 0, errorMessage: error.errorText });
          } else {
            onFailure(error.message);
          }
        },
        (response) => {
          if (response.pending !== true) {
            return false;
          }
          onPending?.();
          return true;
        },
      );
    } catch (error) {
      if (error instanceof DisconnectedError || error instanceof ClosedError) {
        onFailure(error.message);
      } else {
        // Not encodable (e.g. an unpaired surrogate): sending it again
        // would fail the same way.
        const message = error instanceof Error ? error.message : String(error);
        onResponse({ operationsApplied: 0, errorMessage: `Delta cannot be sent: ${message}` });
      }
    }
  }
}
