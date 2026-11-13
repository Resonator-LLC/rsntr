// The server side core of the concurrency control, for simulated servers
// and tests.
//
// A TypeScript port of `org.waveprotocol.wave.concurrencycontrol.server` of
// Apache Wave (plus the `SimpleDeltaHistory` test helper), licensed under
// the Apache License, Version 2.0.

export { ConcurrencyControlCore, ReOpenInfo } from "./concurrencyControlCore";
export type { DeltaHistory } from "./deltaHistory";
export { SimpleDeltaHistory } from "./simpleDeltaHistory";
