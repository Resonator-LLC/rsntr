// A clock whose time is set by the test.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.testing.FakeClock`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { Clock } from "../common";

export class FakeClock implements Clock {
  private currentTime: number;

  constructor(currentTime: number) {
    this.currentTime = currentTime;
  }

  setCurrentTime(currentTime: number): void {
    this.currentTime = currentTime;
  }

  getCurrentTime(): number {
    return this.currentTime;
  }
}
