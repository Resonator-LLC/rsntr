// The high-level client API: sign in, open and create waves, edit them,
// observe remote changes, connection and unsaved state.
//
// See `waveClient.ts` and `openWave.ts`. The parts underneath, for uses the
// facade does not cover: `LiveWaveView` (a wave view bound to the channel
// stack), `RemoteWaveViewService` (the wire protocol for it) and
// `WaveRpcTransport` (the message transport it needs).

export { BackoffScheduler, DEFAULT_TIMERS, type BackoffSchedulerOptions, type Timers } from "./backoffScheduler";
export {
  LiveWaveView,
  WaveFailedError,
  type LiveWaveViewEvents,
  type LiveWaveViewOptions,
  type LiveWaveViewState,
  type UnsavedState,
} from "./liveWaveView";
export {
  transformLocation,
  transformRange,
  type LocationBias,
  type LocationRange,
} from "./locations";
export { OpenWave, type ConversationTarget, type NewReply } from "./openWave";
export {
  RemoteWaveViewService,
  isRetryableOpenError,
  type RemoteWaveViewServiceOptions,
} from "./remoteWaveViewService";
export { RpcWaveTransport } from "./rpcWaveTransport";
export {
  NotSignedInError,
  WaveClient,
  type WaveClientEvents,
  type WaveClientOptions,
} from "./waveClient";
export type { ViewStream, ViewStreamHandler, WaveRpcTransport } from "./waveRpc";
