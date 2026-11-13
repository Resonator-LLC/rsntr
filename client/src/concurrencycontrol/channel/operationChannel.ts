// A client socket for a wavelet. Sends/receives operations to/from the wave
// server and transforms (outbound) client operations against (inbound)
// server operations.
//
// An operation channel can operate without network connection to the server
// by queuing outbound operations until a network connection is established
// at a later time.
//
// If the network connection fails during operation, or the wave server
// restarts or fails over, a channel will silently try to reconnect and
// recover (by selectively retransforming and retransmitting relevant client
// operations) but will notify if recovery fails.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.OperationChannel`
// of Apache Wave, licensed under the Apache License, Version 2.0.
// `OperationChannel.Listener` of the original is `OperationChannelListener`.

import type { WaveletOperation } from "../../model/operation";
import type { HashedVersion } from "../../model/version";

/** A listener for operations being received by a channel. */
export interface OperationChannelListener {
  /** Invoked when a remote operation is received by the channel. */
  onOperationReceived(): void;
}

export interface OperationChannel {
  /** Sets the channel listener. */
  setListener(listener: OperationChannelListener | null): void;

  /**
   * Sends client operations to the wave server. If the channel is not
   * connected, the operations are queued. Throws `ChannelException` if the
   * channel is corrupt or misused.
   */
  send(...operations: WaveletOperation[]): void;

  /**
   * Takes the next (transformed) server operation, if any was received from
   * the server (and made it through transformation), else null.
   */
  receive(): WaveletOperation | null;

  /** Like {@link OperationChannel.receive} without removing the operation. */
  peek(): WaveletOperation | null;

  /** The wavelet versions at which this channel could reconnect. */
  getReconnectVersions(): HashedVersion[];

  /** A dump of the state of the cc stack, for debugging. */
  getDebugString(): string;
}
