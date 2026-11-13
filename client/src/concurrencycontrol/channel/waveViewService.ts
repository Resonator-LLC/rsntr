// A service that provides a model object-based interface to the Wave RPC
// service. Implementations convert the model objects passed in to the wire
// messages and send them.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.WaveViewService`
// of Apache Wave, licensed under the Apache License, Version 2.0. The nested
// interfaces of the original are top-level here: `WaveViewServiceUpdate`,
// `OpenCallback`, `WaveViewServiceSubmitCallback` (`WaveViewService.SubmitCallback`;
// the channel package has another `SubmitCallback`) and `CloseCallback`.

import type { WaveletData } from "../../model/data";
import type { IdFilter, WaveId, WaveletId, WaveletName } from "../../model/id";
import type {
  TransformedWaveletDelta,
  WaveletDelta,
} from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { ChannelException, ResponseCode } from "../common";
import type { KnownWaveletVersions } from "./waveletIdMap";

/**
 * A stream update's contents. Only one of {channelId, snapshot, deltas}
 * should be present. A getter returns null when its `has` method is false.
 */
export interface WaveViewServiceUpdate {
  hasChannelId(): boolean;
  getChannelId(): string | null;

  hasWaveletId(): boolean;
  getWaveletId(): WaveletId | null;

  hasLastCommittedVersion(): boolean;
  getLastCommittedVersion(): HashedVersion | null;

  hasCurrentVersion(): boolean;
  getCurrentVersion(): HashedVersion | null;

  hasWaveletSnapshot(): boolean;
  getWaveletSnapshot(): WaveletData | null;

  hasDeltas(): boolean;
  /** Empty when there are no deltas. */
  getDeltaList(): readonly TransformedWaveletDelta[];

  hasMarker(): boolean;

  /**
   * Not in the original: the version the wavelet's history was rewound to
   * (a change of host); the delta list then holds the deltas taken back.
   */
  getResetVersion?(): HashedVersion | null;
}

/** Streaming callback for an open wave view connection to the server. */
export interface OpenCallback {
  /** Called when a protocol error occurs in processing before the callback is called. */
  onException(e: ChannelException): void;

  /** Called when an update arrives. */
  onUpdate(update: WaveViewServiceUpdate): void;

  /**
   * Called when the stream closes. `response` is an error message, or null
   * if the stream closed normally.
   */
  onSuccess(response: string | null): void;

  /** Called when the task fails. */
  onFailure(reason: string): void;
}

/** Callback for submitting a delta to the server (`WaveViewService.SubmitCallback`). */
export interface WaveViewServiceSubmitCallback {
  onSuccess(
    version: HashedVersion,
    opsApplied: number,
    errorMessage: string | null,
    responseCode: ResponseCode,
  ): void;

  onFailure(failure: string): void;
}

/** Callback for closing a connection to the server. */
export interface CloseCallback {
  onSuccess(): void;

  onFailure(failure: string): void;
}

export interface WaveViewService {
  /** Opens a wave connection to the server. */
  viewOpen(
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
    callback: OpenCallback,
  ): void;

  /**
   * Submits a delta to the server. On success, the server replies with the
   * latest version, the number of operations applied, and an error message
   * and/or response code.
   *
   * Returns the request id that can be passed later into
   * {@link WaveViewService.debugGetProfilingInfo}; may be null.
   */
  viewSubmit(
    wavelet: WaveletName,
    delta: WaveletDelta,
    channelId: string,
    callback: WaveViewServiceSubmitCallback,
  ): string | null;

  /** Closes a wave from the server. */
  viewClose(waveId: WaveId, channelId: string, callback: CloseCallback): void;

  /** Debug string with profile information about the data sent for a request. */
  debugGetProfilingInfo(requestId: string | null): string;
}
