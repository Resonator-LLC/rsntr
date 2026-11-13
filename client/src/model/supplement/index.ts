// The per-user state of a wave: what the user has read, where the wave is
// filed, whether it is archived or muted, and so on (the "supplement").
//
// The state lives in the user's user-data wavelet of the wave, one small
// document per concern (see `WaveletBasedSupplement` for the document
// shapes). The module has the same layers as the original:
//
// * `ReadablePrimitiveSupplement` and `PrimitiveSupplement` are the raw
//   state (`PrimitiveSupplement`): versions and flags, without
//   interpretation. `WaveletBasedSupplement` keeps it in a user-data
//   wavelet, emitting an operation for every change; `UserDataSupplement`
//   only reads one; `PrimitiveSupplementImpl` keeps it in memory.
// * `SupplementImpl` interprets the raw state (`Supplement`): a blip is
//   unread if it changed after the version at which it, or its whole
//   wavelet, was last read.
// * `SupplementedWaveImpl` combines the state with what it needs to know
//   about the wave itself, given by a `SupplementWaveView`
//   (`SupplementedWave`): is the wave in the inbox, mark it all as read,
//   archive it.
// * `LocalSupplementedWave` (`local.ts`, not in the Rust port) adds the
//   client's optimistic read state (`LocalSupplementedWaveImpl`): blips the
//   viewer reads or marks stay read while their own edits are acknowledged.
// * `LiveSupplementedWave` (not in the Rust port) is the supplement of an
//   open wave in a client: it sits on a `WaveView`, answers from a cached
//   snapshot, writes through the user-data wavelet (creating it when first
//   needed) and reports changes, local or remote, as events.
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement` of Apache
// Wave (`PrimitiveSupplement`, `PrimitiveSupplementImpl`,
// `WaveletBasedSupplement`, `WaveletReadStateCollection`,
// `DocumentBasedWaveletReadState`, `WaveletThreadStateCollection`,
// `DocumentBasedWaveletThreadState`, `GadgetStateCollection`,
// `DocumentBasedGadgetState`, `DocumentBasedAbuseStore`, `SupplementImpl`,
// `SupplementedWaveImpl`, `SupplementWaveView`,
// `PartitioningPrimitiveSupplement`, `ThreadState`, `WantedEvaluation`,
// `SimpleWantedEvaluation`, `SimpleWantedEvaluationSet`,
// `ObservablePrimitiveSupplement`, `ObservableSupplementedWave`,
// `LiveSupplementedWaveImpl`) and `conversation.InboxState`, licensed under
// the Apache License, Version 2.0.
//
// Not ported: the pure delegation wrapper `SupplementedWaveWrapper`, the
// client presentation policy (`PresentationPolicy`,
// `SimplePresentationPolicy`), `ThreadStateSerializer` with
// `PrimitiveThreadState`, which nothing uses.
//
// Versions are 32-bit integers with `NO_VERSION` for "none", as in the
// original.

export {
  ABUSE_DOCUMENT,
  ARCHIVING_DOCUMENT,
  CLEARED_DOCUMENT,
  FOLDERS_DOCUMENT,
  GADGETS_DOCUMENT,
  MUTED_DOCUMENT,
  PRESENTATION_DOCUMENT,
  READSTATE_DOCUMENT,
  SEEN_DOCUMENT,
} from "./constants";
export {
  LiveSupplementedWave,
  type LiveSupplementedWaveOptions,
  type SupplementEvents,
} from "./live";
export {
  EVICT_TIME_MS,
  LocalReadState,
  LocalSupplementedWave,
  REPEAT_MS,
  type LocalReadStateOptions,
  type LocalSupplementedWaveOptions,
  type LocalTimers,
  type ReadStateDelegate,
} from "./local";
export { PartitioningPrimitiveSupplement } from "./partitioning";
export { PrimitiveSupplementImpl } from "./primitive";
export { SupplementImpl } from "./supplementImpl";
export {
  ALL_FOLDER,
  INBOX_FOLDER,
  SupplementedWaveImpl,
  TRASH_FOLDER,
  WaveletDataView,
  hasConversation,
  isFollowedByDefault,
  type DefaultFollow,
  type InboxState,
  type SupplementWaveView,
} from "./supplementedWave";
export {
  NO_VERSION,
  SupplementError,
  THREAD_STATE,
  ThreadState,
  type PrimitiveSupplement,
  type ReadablePrimitiveSupplement,
  type SupplementErrorDetail,
} from "./types";
export { WantedEvaluation, WantedEvaluationSet, type WantedEvaluationFields } from "./wanted";
export { UserDataSupplement, WaveletBasedSupplement, type UserDataWavelet } from "./waveletBased";
