// Listener for operations from the server arriving from a ServerConnection.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.client.ServerConnectionListener`
// of Apache Wave, licensed under the Apache License, Version 2.0.
//
// Where the original declares `TransformException` / `OperationException`,
// the methods here throw errors for which `isTransformException` /
// `isOperationException` (see `common/errors.ts`) hold.

import type { TransformedWaveletDelta } from "../../model/operation";
import type { HashedVersion } from "../../model/version";

export interface ServerConnectionListener {
  /**
   * Called when the wave is (re-)opened and before receiving deltas from the
   * server. Throws `ChannelException`.
   *
   * @param connectVersion the signature of the wave at the version at which
   *        the wave is opened.
   * @param currentVersion the signature of the wave at the server's current
   *        version.
   */
  onOpen(connectVersion: HashedVersion, currentVersion: HashedVersion): void;

  /** Called when a delta is received from the server. */
  onServerDelta(delta: TransformedWaveletDelta): void;

  /**
   * Called when deltas are received from the server. With a list, client
   * deltas that need resending are only sent after all the transformation
   * rather than once per server delta.
   */
  onServerDeltas(deltas: readonly TransformedWaveletDelta[]): void;

  /**
   * Called when acknowledgement is received from the server for an operation
   * issued by this client.
   *
   * @param operationsApplied number of operations to ack.
   * @param signature the signature after the client operations being applied
   *        on the server.
   */
  onSuccess(operationsApplied: number, signature: HashedVersion): void;

  /**
   * Called when the server advertises that the delta at some version has
   * been committed to persistent storage. The client can clear any cache
   * that is used for recovery before that version.
   */
  onCommit(version: number): void;
}
