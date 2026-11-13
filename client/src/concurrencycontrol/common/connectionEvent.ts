// Concurrency control state change events.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.common.ConnectionEvent`
// of Apache Wave, licensed under the Apache License, Version 2.0.

export const ConnectionEvent = {
  /** Signals that the listener shall call receive on the concurrency control. */
  READABLE: "READABLE",
  /** Attempt to recover from reconnection to server has failed. */
  FAILED_RECOVERY: "FAILED_RECOVERY",
  /** Channel or CC state has failed, the channel is now unusable. */
  FAILED_CORRUPT_STATE: "FAILED_CORRUPT_STATE",
  /** Channel has closed permanently, either successfully or due to error. */
  CLOSED: "CLOSED",
} as const;

export type ConnectionEvent = (typeof ConnectionEvent)[keyof typeof ConnectionEvent];
