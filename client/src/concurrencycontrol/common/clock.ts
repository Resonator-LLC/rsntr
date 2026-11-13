// A source of timestamps.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.common.Clock` and
// `SystemClock` of Apache Wave, licensed under the Apache License, Version 2.0.

export interface Clock {
  /** The current timestamp, in milliseconds. */
  getCurrentTime(): number;
}

/** A clock which uses the underlying system clock. */
export class SystemClock implements Clock {
  getCurrentTime(): number {
    return Date.now();
  }
}
