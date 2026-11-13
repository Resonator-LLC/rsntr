// Multiplexes several operation channels together. The lifecycle of a
// multiplexer is simply `open` followed by `close`.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.channel.OperationChannelMultiplexer`
// of Apache Wave, licensed under the Apache License, Version 2.0. The nested
// `Listener` of the original is `OperationChannelMultiplexerListener`.

import type { WaveletData } from "../../model/data";
import type { IdFilter, ParticipantId, WaveletId } from "../../model/id";
import type { HashedVersion } from "../../model/version";
import type { CorruptionDetail } from "../common";
import type { Accessibility } from "./accessibility";
import type { OperationChannel } from "./operationChannel";

/** Listener for handling multiplexer events. */
export interface OperationChannelMultiplexerListener {
  /**
   * A new channel has been established. The new channel, based on the
   * snapshot given here, is ready to be used immediately.
   */
  onOperationChannelCreated(
    channel: OperationChannel,
    snapshot: WaveletData,
    accessibility: Accessibility,
  ): void;

  /**
   * An existing channel has been destroyed. The dropped channel is typically
   * likely to be replaced by a new one.
   */
  onOperationChannelRemoved(
    channel: OperationChannel,
    waveletId: WaveletId,
  ): void;

  /**
   * The initial open has finished.
   *
   * The "initial open" is the point after which the channel listener has
   * been notified of all operation channels for some "initial set" of
   * wavelets as decided by the server. The channel listener may also be
   * notified of other channels, interleaved amongst the channels for the
   * initial set, before this is called.
   */
  onOpenFinished(): void;

  /**
   * The multiplexer has failed. This may occur before or after any channels
   * are created, before or after `onOpenFinished`.
   *
   * No further messages will be received by any callback or operation
   * channel after this message.
   */
  onFailed(detail: CorruptionDetail): void;
}

/** Information required to open a wavelet at a known state. */
export class KnownWavelet {
  /** Wavelet data. */
  readonly snapshot: WaveletData;
  /** Last committed version of the wavelet. */
  readonly committedVersion: HashedVersion;
  /** The wavelet's accessibility to the user. */
  readonly accessibility: Accessibility;

  constructor(
    snapshot: WaveletData,
    committedVersion: HashedVersion,
    accessibility: Accessibility,
  ) {
    this.snapshot = snapshot;
    this.committedVersion = committedVersion;
    this.accessibility = accessibility;
  }
}

export interface OperationChannelMultiplexer {
  /**
   * Opens this multiplexer. After it becomes open, operation channels will
   * be passed to the `muxListener` as they come into existence. The listener
   * may be notified as part of this call, or afterwards.
   *
   * A channel is immediately opened for each of the `knownWavelets`
   * (default: none).
   */
  open(
    muxListener: OperationChannelMultiplexerListener,
    waveletFilter: IdFilter,
    knownWavelets?: Iterable<KnownWavelet>,
  ): void;

  /**
   * Closes this multiplexer. The channel listener will no longer be notified
   * of new channels, and no more operations will be received on existing
   * channels. The behavior of requests to create new channels, or the
   * submission of operations to existing channels, is undefined.
   */
  close(): void;

  /**
   * Creates a new operation channel. The listener will be notified of the
   * new channel during this method. `creator` is the wavelet creator.
   */
  createOperationChannel(waveletId: WaveletId, creator: ParticipantId): void;
}
