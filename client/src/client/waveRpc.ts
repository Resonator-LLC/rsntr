// The wave protocol as the view service needs it: open a view stream, submit
// a delta, learn about the connection. Implemented over the WebSocket RPC
// channel (`rpcWaveTransport.ts`) and by the simulated server of the tests.
//
// Corresponds to `WaveWebSocketClient` / `WaveWebSocketCallback` of
// `org.waveprotocol.box.webclient.client` in Apache Wave, licensed under the
// Apache License, Version 2.0.

import type { Unsubscribe } from "../util/events";
import type {
  ProtocolOpenRequest,
  ProtocolSubmitRequest,
  ProtocolSubmitResponse,
  ProtocolWaveletUpdate,
} from "../wire";

/** Receives the messages of one open view stream, in arrival order. */
export interface ViewStreamHandler {
  onUpdate(update: ProtocolWaveletUpdate): void;
  /**
   * The server ended the stream: `error` is its text, or undefined after a
   * cancel. Nothing follows.
   */
  onFinished(error: string | undefined): void;
  /** The connection dropped; the stream is gone. Nothing follows. */
  onDisconnected(reason: string): void;
}

export interface ViewStream {
  /** Asks the server to end the stream. Idempotent. */
  cancel(): void;
}

/**
 * Message transport of the wave protocol. All callbacks are invoked
 * synchronously, one incoming frame at a time, in the order the frames
 * arrived: the concurrency control above depends on a submit response and
 * the view updates around it keeping their order.
 */
export interface WaveRpcTransport {
  isConnected(): boolean;
  /** Notifies of every change of {@link isConnected}. */
  onConnectionChange(listener: (connected: boolean) => void): Unsubscribe;
  /** Opens a view stream. Must only be called while connected. */
  openView(request: ProtocolOpenRequest, handler: ViewStreamHandler): ViewStream;
  /**
   * Submits a delta. Exactly one of the callbacks is called, once:
   * `onFailure` if the connection drops first or the request is refused.
   * Before that, `onPending` (if given) is called when the server answers
   * that the wavelet's host cannot be reached now and it keeps the delta
   * to send later: the final answer is still to come.
   */
  submit(
    request: ProtocolSubmitRequest,
    onResponse: (response: ProtocolSubmitResponse) => void,
    onFailure: (reason: string) => void,
    onPending?: () => void,
  ): void;
}
