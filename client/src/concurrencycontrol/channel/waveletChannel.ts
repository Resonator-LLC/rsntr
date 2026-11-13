// A raw channel for messages on a single wavelet.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.WaveletChannel`
// of Apache Wave, licensed under the Apache License, Version 2.0.
// `WaveletChannel.Listener` of the original is `WaveletChannelListener`.

import type { WaveletData } from "../../model/data";
import type {
  TransformedWaveletDelta,
  WaveletDelta,
} from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { SubmitCallback } from "./submitCallback";

/** Receiver for wavelet updates. */
export interface WaveletChannelListener {
  /**
   * Called when a stream update is received. At least one of `deltas` and
   * `lastCommittedVersion` will be non-null or non-empty.
   * `currentSignedVersion` (the latest version on the server) is non-null
   * only in the first message when a stream is reconnecting. Throws
   * `ChannelException` if the channel fails in processing the update.
   */
  onWaveletUpdate(
    deltas: readonly TransformedWaveletDelta[] | null,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void;

  /**
   * Called when a stream update is received. At least one of `snapshot` and
   * `lastCommittedVersion` will be non-null. `currentSignedVersion` is
   * non-null only in the first message when a stream is reconnecting. Throws
   * `ChannelException` if the channel fails in processing the update.
   */
  onWaveletSnapshot(
    snapshot: WaveletData | null,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void;
}

export interface WaveletChannel {
  /** Submits a delta on this channel; `callback` is invoked with the response. */
  submit(delta: WaveletDelta, callback: SubmitCallback): void;

  /** Debug string with profile information about the data sent. */
  debugGetProfilingInfo(): string | null;
}
