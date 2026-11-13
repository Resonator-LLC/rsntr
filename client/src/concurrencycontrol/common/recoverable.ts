// Whether a channel failure is recoverable by reconnection.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.common.Recoverable`
// of Apache Wave, licensed under the Apache License, Version 2.0.

export const Recoverable = {
  RECOVERABLE: "RECOVERABLE",
  NOT_RECOVERABLE: "NOT_RECOVERABLE",
} as const;

export type Recoverable = (typeof Recoverable)[keyof typeof Recoverable];
