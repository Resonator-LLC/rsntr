// The client side concurrency control: the per-wavelet OT state machine and
// its connection interfaces.
//
// A TypeScript port of `org.waveprotocol.wave.concurrencycontrol.client` of
// Apache Wave, licensed under the Apache License, Version 2.0.

export { ConcurrencyControl, type ConnectionListener } from "./concurrencyControl";
export { MergingSequence } from "./mergingSequence";
export { OperationQueue, type Transformer } from "./operationQueue";
export type { ServerConnection } from "./serverConnection";
export type { ServerConnectionListener } from "./serverConnectionListener";
