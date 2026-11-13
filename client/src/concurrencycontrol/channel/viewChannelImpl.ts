// Implementation of a view channel. This is a one-off object: once
// connected to the server, the same channel cannot be used to reconnect.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.ViewChannelImpl`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import {
  type IdFilter,
  type WaveId,
  type WaveletId,
  WaveletName,
} from "../../model/id";
import type { WaveletData } from "../../model/data";
import type { WaveletDelta } from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import {
  ChannelException,
  IllegalStateError,
  type Logger,
  NO_LOGGER,
  Recoverable,
  type ResponseCode,
  checkState,
} from "../common";
import type { SubmitCallback } from "./submitCallback";
import type { ViewChannel, ViewChannelListener } from "./viewChannel";
import type { ViewChannelFactory } from "./viewChannelFactory";
import type {
  OpenCallback,
  WaveViewService,
  WaveViewServiceUpdate,
} from "./waveViewService";
import type { KnownWaveletVersions } from "./waveletIdMap";

/**
 * A place holder value in `debugLastSubmit` while a delta is being
 * submitted.
 */
const SUBMITTING = "Submitting";

/**
 * For 2 pairs of wave + playback channel. The second pair is used in
 * recovery whilst the first pair is closing.
 */
const DEFAULT_MAX_VIEW_CHANNELS_PER_WAVE = 4;

/** Counts the view channels for each wave, by serialised wave id. */
const viewChannelsPerWave = new Map<string, number>();

let maxViewChannelsPerWave = DEFAULT_MAX_VIEW_CHANNELS_PER_WAVE;

type State =
  /** Post-constructor state. */
  | "INITIAL"
  /** An open request has been sent. */
  | "CONNECTING"
  /** A response (and a channelId) have been received. */
  | "CONNECTED"
  /**
   * A close request is waiting to be sent to the server: the channel was
   * closed before the channel id arrived.
   */
  | "CLOSING"
  /** Channel is closed (either successfully or due to error). */
  | "CLOSED";

export class ViewChannelImpl implements ViewChannel, OpenCallback {
  /** Id of the wave being viewed. */
  private readonly waveId: WaveId;

  /** Service through which rpcs are made. */
  private readonly waveService: WaveViewService;

  private readonly logger: Logger;

  /** The last submit request id of each wavelet, by serialised wavelet id. */
  private readonly debugLastSubmit = new Map<string, string | null>();

  //
  // Mutable state.
  //

  /** State this channel is in. */
  private state: State;

  /**
   * Channel id, which must be provided for all delta submission (including
   * the first submit on a new wavelet). This channel id is supplied in the
   * first message received from the server. As a corollary:
   *   INITIAL | CONNECTING => !hasChannelId()
   *   CONNECTED => hasChannelId()
   * Even in the CLOSING and CLOSED states, we retain the channel id, in
   * order that a ViewClose RPC can be sent from those states in some
   * situations.
   */
  private channelId: string | null = null;

  /**
   * Listens for connection lifecycle events and for incoming updates from
   * the server stream. Set on opening, and retained until closed.
   */
  private openListener: ViewChannelListener | null = null;

  /** Creates a factory for view channels backed by `waveService`. */
  static factory(
    waveService: WaveViewService,
    logger: Logger = NO_LOGGER,
  ): ViewChannelFactory {
    return {
      create: (viewWaveId: WaveId) =>
        new ViewChannelImpl(viewWaveId, waveService, logger),
    };
  }

  /**
   * Constructs a view channel for a view of the wave `waveId`, making its
   * RPCs through `service`. Throws {@link IllegalStateError} if the wave
   * already has the maximum number of open view channels.
   */
  constructor(
    waveId: WaveId,
    service: WaveViewService,
    logger: Logger = NO_LOGGER,
  ) {
    this.waveId = waveId;
    this.waveService = service;
    this.logger = logger;
    this.state = "INITIAL";

    this.registerChannel();
  }

  open(
    openListener: ViewChannelListener,
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
  ): void {
    checkState(
      this.state === "INITIAL",
      `Cannot re-open view channel: ${this}`,
    );
    this.state = "CONNECTING";
    this.openListener = openListener;
    this.logger.trace("connect: new view channel initialized");
    this.doOpen(waveletFilter, knownWavelets);
  }

  /** Makes the server call to open a stream of deltas to the client. */
  private doOpen(
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
  ): void {
    this.waveService.viewOpen(waveletFilter, knownWavelets, this);
  }

  close(): void {
    this.terminate(null);
  }

  submitDelta(
    waveletId: WaveletId,
    delta: WaveletDelta,
    callback: SubmitCallback,
  ): void {
    checkState(
      this.state === "CONNECTED",
      `Cannot submit to disconnected view channel: ${this}, delta version ${delta.targetVersion}`,
    );
    this.doSubmitDelta(waveletId, delta, callback);
  }

  /** Makes the RPC call to the server to submit a delta. */
  private doSubmitDelta(
    waveletId: WaveletId,
    delta: WaveletDelta,
    callback: SubmitCallback,
  ): void {
    const key = waveletId.serialise();

    // It's possible that the callback happens synchronously, so put in
    // a fake request id and detect its removal later.
    this.debugLastSubmit.set(key, SUBMITTING);

    // The channel id is non-null in the CONNECTED state.
    const channelId = this.channelId as string;
    const waveId = this.waveId;

    const handleException = (methodName: string, e: ChannelException): void => {
      this.debugLastSubmit.delete(key);
      // Throwing this exception back to the wave service will crash
      // the client so we must catch it here and fail just this view.
      this.triggerOnException(e, waveletId);
      this.terminate(
        `View submit [${methodName}] for wavelet ${waveId}/${waveletId} raised exception: ${e}`,
      );
    };

    const requestId = this.waveService.viewSubmit(
      WaveletName.of(waveId, waveletId),
      delta,
      channelId,
      {
        onSuccess: (
          version: HashedVersion,
          opsApplied: number,
          errorMessage: string | null,
          responseCode: ResponseCode,
        ): void => {
          this.debugLastSubmit.delete(key);
          try {
            callback.onSuccess(opsApplied, version, responseCode, errorMessage);
          } catch (e) {
            if (!(e instanceof ChannelException)) {
              throw e;
            }
            handleException("onSuccess", e);
          }
        },

        onFailure: (failure: string): void => {
          this.debugLastSubmit.delete(key);
          try {
            callback.onFailure(failure);
          } catch (e) {
            if (!(e instanceof ChannelException)) {
              throw e;
            }
            handleException("onFailure", e);
          }
        },
      },
    );

    if (this.debugLastSubmit.has(key)) {
      this.debugLastSubmit.set(key, requestId);
    }
  }

  /** Whether a channel id has been received from the server. */
  private hasChannelId(): boolean {
    return this.channelId !== null;
  }

  //
  // ViewOpen RPC callback methods
  //

  private checkUpdateProtocolRestrictions(update: WaveViewServiceUpdate): void {
    if (
      update.hasChannelId() &&
      (update.hasMarker() ||
        update.hasWaveletSnapshot() ||
        update.hasDeltas() ||
        update.hasLastCommittedVersion() ||
        update.hasCurrentVersion())
    ) {
      let whichData = "";
      if (update.hasMarker()) {
        whichData += "marker, ";
      }
      if (update.hasWaveletSnapshot()) {
        whichData += "snapshot, ";
      }
      if (update.hasDeltas()) {
        whichData += "deltas, ";
      }
      if (update.hasLastCommittedVersion()) {
        whichData += "lastCommittedVersion, ";
      }
      if (update.hasCurrentVersion()) {
        whichData += "currentVersion";
      }
      throw new ChannelException(
        `An update contained a channel id AND other data: ${whichData}`,
        Recoverable.NOT_RECOVERABLE,
      );
    }
    if (
      (update.hasWaveletSnapshot() ||
        update.hasDeltas() ||
        update.hasLastCommittedVersion() ||
        update.hasCurrentVersion()) &&
      !update.hasWaveletId()
    ) {
      throw new ChannelException(
        "An update lacked a required wavelet id.",
        Recoverable.NOT_RECOVERABLE,
      );
    }
    if (update.hasWaveletSnapshot() && update.hasDeltas()) {
      throw new ChannelException(
        "Message has both snapshot and deltas",
        Recoverable.NOT_RECOVERABLE,
      );
    }
  }

  onUpdate(update: WaveViewServiceUpdate): void {
    try {
      this.checkUpdateProtocolRestrictions(update);
    } catch (e) {
      if (!(e instanceof ChannelException)) {
        throw e;
      }
      this.triggerOnException(
        e,
        update.hasWaveletId() ? update.getWaveletId() : null,
      );
      this.terminate(`View update raised exception: ${e}`);
    }
    switch (this.state) {
      case "INITIAL":
        // We can't report a channel exception because there's no listener.
        throw new IllegalStateError(
          `Unexpected update before view channel opened: ${this}, update: ${update}`,
        );
      case "CONNECTING":
        // First update: extract channel id.
        if (!update.hasChannelId()) {
          this.onException(
            new ChannelException(
              `First update did not contain channel id. Wave id: ${this.waveId}, update: ${update}`,
              Recoverable.NOT_RECOVERABLE,
            ),
          );
          return;
        }
        this.channelId = update.getChannelId();
        this.state = "CONNECTED";
        if (this.openListener !== null) {
          this.openListener.onConnected();
        }
        break;
      case "CONNECTED":
        if (update.hasChannelId()) {
          this.logger.trace(
            `A non-first update contained a channel id: ${update}`,
          );
        }
        if (this.openListener !== null) {
          const waveletId = update.hasWaveletId()
            ? update.getWaveletId()
            : null;
          const lastCommittedVersion = update.hasLastCommittedVersion()
            ? update.getLastCommittedVersion()
            : null;
          const currentVersion = update.hasCurrentVersion()
            ? update.getCurrentVersion()
            : null;
          try {
            // The protocol restrictions checked above guarantee a wavelet id
            // on an update with a snapshot, deltas or versions.
            const resetVersion = update.getResetVersion?.() ?? null;
            if (resetVersion !== null) {
              this.openListener.onReset?.(
                waveletId as WaveletId,
                resetVersion,
                update.getDeltaList(),
              );
            } else if (update.hasWaveletSnapshot()) {
              // it's a snapshot
              this.openListener.onSnapshot(
                waveletId as WaveletId,
                update.getWaveletSnapshot() as WaveletData,
                lastCommittedVersion,
                currentVersion,
              );
            } else if (
              update.hasDeltas() ||
              update.hasLastCommittedVersion() ||
              update.hasCurrentVersion()
            ) {
              // it's deltas or versions.
              this.openListener.onUpdate(
                waveletId as WaveletId,
                update.getDeltaList(),
                lastCommittedVersion,
                currentVersion,
              );
            }
            if (update.hasMarker()) {
              // The listener may have closed this channel while handling the
              // update above (the original would fail on a null listener).
              (
                this.openListener as ViewChannelListener | null
              )?.onOpenFinished();
            }
          } catch (e) {
            if (!(e instanceof ChannelException)) {
              throw e;
            }
            this.triggerOnException(e, waveletId);
            this.terminate(`View update raised exception: ${e}`);
          }
        }
        break;
      case "CLOSING":
        // Already closed: do nothing, except in the following special case.
        // If the channel was closed on the client end before any updates were
        // received from the server, then at that point the ViewClose could
        // not have been sent (because there was no channel id to close).
        // Therefore, if the channel receives its first update (identified by
        // !this.hasChannelId()) when it is already closing, it is assumed
        // that the above scenario has occurred, in which case the ViewClose
        // must be sent now.
        if (!this.hasChannelId()) {
          if (!update.hasChannelId()) {
            this.onException(
              new ChannelException(
                `First update did not contain channel id. Wave id: ${this.waveId}, update: ${update}`,
                Recoverable.NOT_RECOVERABLE,
              ),
            );
          }
          this.channelId = update.getChannelId();
          this.requestViewClose();
        }
        this.state = "CLOSED";
        break;
      case "CLOSED":
        break;
      default:
        throw new IllegalStateError(`update in unknown state${this.state}`);
    }
  }

  onSuccess(response: string | null): void {
    const fatal = response !== null;
    const errorMessage = fatal ? response : "<no remote error specified>";

    switch (this.state) {
      case "INITIAL":
        // We can't report a channel exception because there's no listener.
        throw new IllegalStateError(
          `View channel received success before open: ${this}, response: ${response}`,
        );
      case "CONNECTING":
      case "CONNECTED": {
        const state = this.state;
        if (fatal) {
          this.triggerOnException(
            new ChannelException(
              `Server unexpectedly closed channel with error: ${errorMessage}`,
              Recoverable.NOT_RECOVERABLE,
            ),
            null,
          );
        }
        this.terminate(
          `Received onSuccess in state ${state} with message: ${errorMessage}`,
        );
        break;
      }
      case "CLOSING":
        this.state = "CLOSED";
        break;
      // The ViewOpen RPC has completed, and the channel has closed.
      case "CLOSED":
        // Even if there are outstanding RPCs since we are already closed
        // just be silent.
        break;
      default:
        throw new IllegalStateError(`success in unknown state${this.state}`);
    }
  }

  onFailure(failure: string): void {
    switch (this.state) {
      case "INITIAL":
        // We can't report a channel exception because there's no listener.
        throw new IllegalStateError(
          `View channel received failure before open: ${this}, response: ${failure}`,
        );
      case "CONNECTING":
      case "CONNECTED":
        // Fail, but not fatally. Encourage reconnection.
        this.terminate(`Received failure: ${failure}`);
        break;
      case "CLOSING":
        this.state = "CLOSED";
        break;
      case "CLOSED":
        break;
      default:
        throw new IllegalStateError(`failure in unknown state${this.state}`);
    }
  }

  onException(e: ChannelException): void {
    this.triggerOnException(e, null);
    this.terminate(
      `WaveService raised exception (probably in translation): ${e}`,
    );
  }

  toString(): string {
    return `[ViewChannel id: ${this.channelId}\n waveId: ${this.waveId}\n state: ${this.state}]`;
  }

  /**
   * Tells the listener of an exception on handling server messages. Wave
   * and wavelet id context is attached to the exception.
   */
  private triggerOnException(
    e: ChannelException,
    waveletId: WaveletId | null,
  ): void {
    if (this.openListener !== null) {
      this.openListener.onException(
        new ChannelException(
          e.responseCode,
          `Exception in view channel, state ${this}`,
          e,
          e.recoverable,
          this.waveId,
          waveletId,
        ),
      );
    }
  }

  /**
   * Terminates the connection, whichever state it's in. The lifecycle of
   * this channel always terminates with a call to this method. `failure` is
   * a failure message, or null.
   */
  private terminate(failure: string | null): void {
    switch (this.state) {
      case "CLOSED":
        return;
      case "INITIAL":
        this.state = "CLOSED";
        break;
      default:
        // CONNECTING, CONNECTED, CLOSING
        // Send close request, even in error case, in case the server is
        // unaware that the channel failed. We close the client state first
        // though, in case sending the view-close fails too. The channel id
        // comes through the first onUpdate message. If we haven't got that
        // yet, we go into CLOSING and wait for that message.
        if (this.hasChannelId()) {
          this.state = "CLOSED";
          this.requestViewClose();
        } else {
          this.state = "CLOSING";
        }
    }

    // Connection is now closed.
    if (this.logger.shouldLogTrace()) {
      this.logger.trace(`${this} terminated: ${failure}`);
    }
    if (this.openListener !== null) {
      this.openListener.onClosed();
    }
    this.openListener = null;

    // This channel is now no longer tracked, as it's closed.
    this.unregisterChannel();
  }

  /** Sends a request to close the stream. */
  private requestViewClose(): void {
    // There must be a channel id in order to request a close.
    checkState(
      this.channelId !== null,
      "ViewClose requested without a channel id",
    );

    this.waveService.viewClose(this.waveId, this.channelId, {
      onFailure: () => {
        // Do nothing.
      },
      onSuccess: () => {
        // Be silent about it, since we are already closed.
      },
    });
  }

  /** Tracks the current channel globally. */
  private registerChannel(): void {
    // Only allow a small set of view channels per client per wave.
    const key = this.waveId.serialise();
    const viewChannelsForWave = viewChannelsPerWave.get(key);
    if (viewChannelsForWave === undefined) {
      viewChannelsPerWave.set(key, 1);
    } else if (viewChannelsForWave >= maxViewChannelsPerWave) {
      throw new IllegalStateError(
        `Cannot create more than ${maxViewChannelsPerWave} channels per wave. Wave id: ${this.waveId}`,
      );
    } else {
      viewChannelsPerWave.set(key, viewChannelsForWave + 1);
    }
  }

  /** Untracks the current channel globally. */
  private unregisterChannel(): void {
    const key = this.waveId.serialise();
    const viewChannelsForWave = viewChannelsPerWave.get(key);
    if (viewChannelsForWave !== undefined) {
      if (viewChannelsForWave <= 1) {
        viewChannelsPerWave.delete(key);
      } else {
        viewChannelsPerWave.set(key, viewChannelsForWave - 1);
      }
    }
  }

  /** Sets the number of view channels allowed per wave. */
  static setMaxViewChannelsPerWave(size: number): void {
    maxViewChannelsPerWave = size;
  }

  /**
   * Forgets all tracked channels and restores the default maximum. For
   * tests: the registry is global, and channels that are never closed stay
   * counted.
   */
  static resetViewChannelsPerWave(): void {
    viewChannelsPerWave.clear();
    maxViewChannelsPerWave = DEFAULT_MAX_VIEW_CHANNELS_PER_WAVE;
  }

  debugGetProfilingInfo(waveletId: WaveletId): string {
    const key = waveletId.serialise();
    if (!this.debugLastSubmit.has(key)) {
      return `ViewChannelImpl: No submits to the server for ${waveletId}`;
    }
    return this.waveService.debugGetProfilingInfo(
      this.debugLastSubmit.get(key) ?? null,
    );
  }
}
