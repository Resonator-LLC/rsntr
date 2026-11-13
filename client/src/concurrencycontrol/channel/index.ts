// The client's channel stack: view channel, per-wavelet delta and operation
// channels, and the multiplexer that ties them to one view.
//
// A TypeScript port of `org.waveprotocol.wave.concurrencycontrol.channel` of
// Apache Wave, licensed under the Apache License, Version 2.0. Nested Java
// interfaces are top-level here with the outer name as prefix (for example
// `ViewChannel.Listener` is `ViewChannelListener`).

export { Accessibility } from "./accessibility";
export type { InternalOperationChannel } from "./internalOperationChannel";
export type {
  OperationChannel,
  OperationChannelListener,
} from "./operationChannel";
export { OperationChannelImpl } from "./operationChannelImpl";
export {
  KnownWavelet,
  type OperationChannelMultiplexer,
  type OperationChannelMultiplexerListener,
} from "./operationChannelMultiplexer";
export {
  LoggerContext,
  OperationChannelMultiplexerImpl,
  createEmptyWaveletSnapshot,
  signaturesFromWavelets,
  type DeltaChannelFactory,
  type EmptyWaveletFactory,
  type OperationChannelFactory,
  type OperationChannelMultiplexerOptions,
  type VersionZeroFactory,
} from "./operationChannelMultiplexerImpl";
export {
  ImmediateExecutionScheduler,
  type Scheduler,
  type SchedulerCommand,
} from "./scheduler";
export type { SubmitCallback } from "./submitCallback";
export type { ViewChannel, ViewChannelListener } from "./viewChannel";
export type { ViewChannelFactory } from "./viewChannelFactory";
export { ViewChannelImpl } from "./viewChannelImpl";
export type {
  CloseCallback,
  OpenCallback,
  WaveViewService,
  WaveViewServiceSubmitCallback,
  WaveViewServiceUpdate,
} from "./waveViewService";
export type { WaveletChannel, WaveletChannelListener } from "./waveletChannel";
export {
  ClientMessage,
  type WaveletDeltaChannel,
  type WaveletDeltaChannelReceiver,
  type WaveletDeltaChannelTransmitter,
} from "./waveletDeltaChannel";
export {
  WaveletDeltaChannelImpl,
  type MultiplexedDeltaChannel,
} from "./waveletDeltaChannelImpl";
export { WaveletIdMap, type KnownWaveletVersions } from "./waveletIdMap";
