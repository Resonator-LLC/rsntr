// Op-based wavelets and the wave view: the live, mutable model of the
// wavelets of a wave.
//
// A TypeScript port of the Rust crate `resonator-model` (module `wavelet`),
// itself derived from `org.waveprotocol.wave.model.wave.opbased`
// (`OpBasedWavelet`, `OpBasedBlip`, `WaveViewImpl`) and
// `org.waveprotocol.wave.model.wave` (`ParticipationHelper`,
// `WaveletListener`, `ObservableWaveView`) of Apache Wave, licensed under
// the Apache License, Version 2.0.
//
// Differences from the Rust port, which is server-side:
//
// * `OpBasedWavelet.events` and `WaveView.events` report every change,
//   local or remote; `OpBasedWavelet.consumeRemote` applies the server's
//   operations; an `output` sink receives local operations.
// * The time of operations comes from a clock unless a fixed timestamp is
//   set.
// * Operation and document errors are thrown as they are
//   (`WaveletOperationError`, `DocError`), not wrapped in `WaveletError`.
// * `takeDelta` targets the hashed version at which the oldest recorded
//   operation was made, not the one at construction.

export { WaveletError, type WaveletErrorDetail } from "./error";
export {
  OpBasedWavelet,
  WaveletDocument,
  type OpBasedWaveletOptions,
  type ParticipationHelper,
  type WaveletEvents,
} from "./opBasedWavelet";
export { WaveView, type WaveViewEvents, type WaveViewOptions } from "./waveView";
