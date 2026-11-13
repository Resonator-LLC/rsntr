// Encapsulates the WaveView rpcs as a channel. Lifecycle: `open` then
// `close`.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.ViewChannel`
// of Apache Wave, licensed under the Apache License, Version 2.0.
// `ViewChannel.Listener` of the original is `ViewChannelListener`.

import type { WaveletData } from "../../model/data";
import type { IdFilter, WaveletId } from "../../model/id";
import type {
  TransformedWaveletDelta,
  WaveletDelta,
} from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { ChannelException } from "../common";
import type { SubmitCallback } from "./submitCallback";
import type { KnownWaveletVersions } from "./waveletIdMap";

/**
 * Callback interface for notification of connection events, which
 * corresponds to the following regular expression:
 *
 *     Connection = onConnected onUpdate* [onOpenFinished] onUpdate* [onFailure] onClosed
 *
 * Each of the lifecycle events can only occur at most once.
 */
export interface ViewChannelListener {
  /** The channel is now connected, and deltas can be submitted. */
  onConnected(): void;

  /** The initial set of updates has been received. May throw `ChannelException`. */
  onOpenFinished(): void;

  /**
   * An exception has occurred whilst handling responses from the server.
   * The view channel is not closed yet when this method is called, but it
   * will be closed after this call.
   */
  onException(ex: ChannelException): void;

  /**
   * The connection has terminated.
   *
   * "Terminated" is client-side termination, either through a requested
   * close or through a failure. It does not necessarily mean that the
   * server has acknowledged the channel closure: there may be update
   * messages in transit both to and from the server. It is up to a higher
   * layer of the communication stack to deal with such outstanding messages.
   */
  onClosed(): void;

  /**
   * A snapshot on the stream. `lastCommittedVersion` and
   * `currentSignedVersion` (the latest version on the server) are optional.
   * May throw `ChannelException`.
   */
  onSnapshot(
    waveletId: WaveletId,
    wavelet: WaveletData,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void;

  /**
   * An update on the stream. `waveletDeltas` may be empty;
   * `lastCommittedVersion` and `currentSignedVersion` (the latest version on
   * the server) are optional. May throw `ChannelException`.
   */
  onUpdate(
    waveletId: WaveletId,
    waveletDeltas: readonly TransformedWaveletDelta[],
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void;

  /**
   * Not in the original: the wavelet's history was rewound to `version`
   * (a change of host); `rewound` are the deltas received since, which no
   * longer exist. May throw `ChannelException`.
   */
  onReset?(
    waveletId: WaveletId,
    version: HashedVersion,
    rewound: readonly TransformedWaveletDelta[],
  ): void;
}

export interface ViewChannel {
  /**
   * Opens this WaveView channel, requesting that, for the known wavelets,
   * only deltas should be received from one of the known versions. Can be
   * called only once.
   */
  open(
    viewListener: ViewChannelListener,
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
  ): void;

  /** Closes this WaveView channel. */
  close(): void;

  /** Submits a delta on this channel. */
  submitDelta(
    waveletId: WaveletId,
    delta: WaveletDelta,
    callback: SubmitCallback,
  ): void;

  /** Debug string with profile information about the data sent for a wavelet. */
  debugGetProfilingInfo(waveletId: WaveletId): string | null;
}
