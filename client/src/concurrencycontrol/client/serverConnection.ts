// Receives deltas to send to the server.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.client.ServerConnection`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveletDelta } from "../../model/operation";

export interface ServerConnection {
  /**
   * Sends the given delta on the wire. All ops in the delta must have the
   * same author. {@link ServerConnection.isOpen} must be true before sending.
   */
  send(delta: WaveletDelta): void;

  /**
   * Whether the connection is ready to accept deltas. Needed because the
   * connection can be closed but we haven't reconnected to the server for
   * recovery.
   */
  isOpen(): boolean;

  /** Debug string that details profile information regarding data being sent. */
  debugGetProfilingInfo(): string;
}
