// A wave view kept live by the concurrency-control channel stack: the
// wavelets of one wave, fed by the server's snapshots and operations, whose
// local operations are sent to the server.
//
// Derived from `CcBasedWaveViewImpl`, `CcBasedWavelet`, `OperationSucker`
// and `StageTwo`'s channel wiring in `org.waveprotocol.wave.concurrencycontrol.wave`
// and `org.waveprotocol.wave.client` of Apache Wave, licensed under the
// Apache License, Version 2.0.
//
// Each wavelet of the view has an operation channel. A local mutation is
// applied to the wavelet at once and its operation is handed to the channel
// (`OpBasedWavelet`'s `output`); operations the channel has received from
// the server, transformed against the local ones, are applied through
// `consumeRemote`. The editor-flush protocol of the original
// (`FlushingOperationSink`) is not needed: nothing here buffers operations
// outside the model.

import {
  Accessibility,
  type OperationChannel,
  OperationChannelMultiplexerImpl,
  type Scheduler,
  ViewChannelImpl,
  type WaveViewService,
} from "../concurrencycontrol/channel";
import {
  type CorruptionDetail,
  type Logger,
  ResponseCode,
  type UnsavedDataInfo,
  type UnsavedDataListenerFactory,
} from "../concurrencycontrol/common";
import type { WaveletData } from "../model/data";
import { IdFilter, type ParticipantId, type WaveId, type WaveletId } from "../model/id";
import { HashedVersionFactory } from "../model/version";
import { type OpBasedWavelet, WaveView } from "../model/wavelet";
import { Emitter, type Observable } from "../util/events";

/** How much of the local work the server has. */
export interface UnsavedState {
  /** Operations sent and not yet acknowledged. */
  readonly inFlight: number;
  /** Operations not yet acknowledged: in flight or queued (estimate). */
  readonly unacknowledged: number;
  /** Operations not yet committed to the server's storage (estimate). */
  readonly uncommitted: number;
  /** Nothing is waiting to be acknowledged or committed. */
  readonly saved: boolean;
}

const SAVED: UnsavedState = { inFlight: 0, unacknowledged: 0, uncommitted: 0, saved: true };

/** Why a live wave view stopped working. */
export class WaveFailedError extends Error {
  constructor(
    /** The response code of the failure, by name (`ResponseCode`). */
    readonly code: string,
    message: string,
    options?: { cause?: unknown },
  ) {
    super(message, options);
    this.name = "WaveFailedError";
  }

  /** Access to the wave, or to a wavelet of it, has been lost. */
  get notAuthorized(): boolean {
    return this.code === ResponseCode.NOT_AUTHORIZED.name;
  }
}

export type LiveWaveViewState = "new" | "opening" | "open" | "failed" | "closed";

export type LiveWaveViewEvents = {
  /** The initial set of wavelets has arrived. Emitted once. */
  opened: Record<string, never>;
  /** The view stopped: nothing more is sent or received. */
  failed: { error: WaveFailedError };
  closed: Record<string, never>;
  /** The unsaved state changed. */
  unsaved: UnsavedState;
  /**
   * A wavelet could not be resumed after a reconnection and was replaced
   * by the server's current state; local changes the server had not
   * acknowledged are lost.
   */
  waveletReset: { waveletId: WaveletId };
};

export interface LiveWaveViewOptions {
  waveId: WaveId;
  viewer: ParticipantId;
  service: WaveViewService;
  /** Scheduler of reconnections; must back off. */
  scheduler: Scheduler;
  /** Which wavelets to open. Default: all. */
  filter?: IdFilter;
  clock?: () => number;
  logger?: Logger;
  /** Receives what model event listeners throw. Default: rethrown from a microtask. */
  onListenerError?: (error: unknown) => void;
}

interface Channel {
  readonly channel: OperationChannel;
  readonly wavelet: OpBasedWavelet;
}

export class LiveWaveView {
  readonly view: WaveView;
  readonly events: Observable<LiveWaveViewEvents>;

  private readonly emitter: Emitter<LiveWaveViewEvents>;
  private readonly mux: OperationChannelMultiplexerImpl;
  private readonly channels = new Map<string, Channel>();
  private readonly unsavedInfo = new Map<string, UnsavedDataInfo>();
  private readonly removedSinceOpen = new Set<string>();
  private currentState: LiveWaveViewState = "new";
  private lastUnsaved: UnsavedState = SAVED;
  private failure: WaveFailedError | null = null;
  /** Inside the mux listener: unsaved notifications are collected. */
  private readonly filter: IdFilter;

  constructor(private readonly options: LiveWaveViewOptions) {
    this.emitter = new Emitter<LiveWaveViewEvents>(options.onListenerError ?? rethrowLater);
    this.events = this.emitter;
    this.filter = options.filter ?? IdFilter.allIds();
    const viewOptions: ConstructorParameters<typeof WaveView>[2] = {};
    if (options.clock !== undefined) {
      viewOptions.clock = options.clock;
    }
    if (options.onListenerError !== undefined) {
      viewOptions.onListenerError = options.onListenerError;
    }
    this.view = new WaveView(options.waveId, options.viewer, viewOptions);

    const unsavedFactory: UnsavedDataListenerFactory = {
      create: (waveletId) => ({
        onUpdate: (info) => {
          this.unsavedInfo.set(waveletId.serialise(), info);
          this.unsavedChanged();
        },
        onClose: () => {
          this.unsavedInfo.delete(waveletId.serialise());
          this.unsavedChanged();
        },
      }),
      destroy: (waveletId) => {
        this.unsavedInfo.delete(waveletId.serialise());
        this.unsavedChanged();
      },
    };
    const muxOptions: ConstructorParameters<typeof OperationChannelMultiplexerImpl>[7] = {
      // This server does not answer the creation of a wavelet with a snapshot.
      dropSnapshotOfCreatedWavelets: false,
    };
    if (options.clock !== undefined) {
      muxOptions.clock = options.clock;
    }
    this.mux = new OperationChannelMultiplexerImpl(
      options.waveId,
      ViewChannelImpl.factory(options.service, options.logger),
      null,
      null,
      unsavedFactory,
      options.scheduler,
      HashedVersionFactory,
      muxOptions,
    );
  }

  get state(): LiveWaveViewState {
    return this.currentState;
  }

  /** The failure, in state `failed`. */
  get error(): WaveFailedError | null {
    return this.failure;
  }

  get unsavedState(): UnsavedState {
    return this.lastUnsaved;
  }

  /** Starts opening the view. The `opened` or `failed` event follows. */
  open(): void {
    if (this.currentState !== "new") {
      throw new Error(`wave view of ${this.options.waveId} was already opened`);
    }
    this.currentState = "opening";
    this.mux.open(
      {
        onOperationChannelCreated: (channel, snapshot, accessibility) =>
          this.onChannelCreated(channel, snapshot, accessibility),
        onOperationChannelRemoved: (channel, waveletId) => this.onChannelRemoved(channel, waveletId),
        onOpenFinished: () => {
          if (this.currentState === "opening") {
            this.currentState = "open";
            this.emitter.emit("opened", {});
          }
        },
        onFailed: (detail) => this.onFailed(detail),
      },
      this.filter,
    );
  }

  /** Resolves when the initial wavelets have arrived; rejects if the view fails or closes first. */
  whenOpened(): Promise<void> {
    switch (this.currentState) {
      case "open":
        return Promise.resolve();
      case "failed":
        return Promise.reject(this.failure);
      case "closed":
        return Promise.reject(new WaveFailedError("OK", "wave view closed"));
      default:
        return new Promise((resolve, reject) => {
          const offs = [
            this.events.on("opened", () => done(resolve)),
            this.events.on("failed", ({ error }) => done(() => reject(error))),
            this.events.on("closed", () =>
              done(() => reject(new WaveFailedError("OK", "wave view closed"))),
            ),
          ];
          const done = (settle: () => void): void => {
            for (const off of offs) {
              off();
            }
            settle();
          };
        });
    }
  }

  /** Resolves when everything local is committed by the server; rejects if the view fails or closes first. */
  whenSaved(): Promise<void> {
    if (this.currentState === "failed") {
      return Promise.reject(this.failure);
    }
    if (this.lastUnsaved.saved) {
      return Promise.resolve();
    }
    return new Promise((resolve, reject) => {
      const offs = [
        this.events.on("unsaved", (state) => {
          if (state.saved) {
            done(resolve);
          }
        }),
        this.events.on("failed", ({ error }) => done(() => reject(error))),
        this.events.on("closed", () =>
          done(() => reject(new WaveFailedError("OK", "wave view closed"))),
        ),
      ];
      const done = (settle: () => void): void => {
        for (const off of offs) {
          off();
        }
        settle();
      };
    });
  }

  /**
   * Creates a wavelet of the wave, empty at version zero with the viewer
   * as its participant, and connects it: its operations are submitted from
   * version zero. Throws if the view is not usable or the wavelet exists.
   */
  createWavelet(waveletId: WaveletId): OpBasedWavelet {
    this.checkUsable();
    if (this.view.hasWavelet(waveletId)) {
      throw new Error(`wavelet ${waveletId} already exists`);
    }
    // Calls back onChannelCreated with an empty snapshot.
    this.mux.createOperationChannel(waveletId, this.options.viewer);
    const wavelet = this.view.wavelet(waveletId);
    if (wavelet === null) {
      throw this.failure ?? new Error(`wavelet ${waveletId} could not be created`);
    }
    wavelet.addParticipant(this.options.viewer);
    return wavelet;
  }

  /** Stops the view. Local operations not yet acknowledged are abandoned. */
  close(): void {
    if (this.currentState === "closed" || this.currentState === "failed") {
      if (this.currentState === "failed") {
        this.currentState = "closed";
      }
      return;
    }
    this.currentState = "closed";
    this.detachAll();
    this.mux.close();
    this.emitter.emit("closed", {});
  }

  /** A dump of the channel stack of a wavelet, for diagnostics. */
  debugString(waveletId: WaveletId): string {
    return this.channels.get(waveletId.serialise())?.channel.getDebugString() ?? "(no channel)";
  }

  /** The versions each wavelet could resume from; for diagnostics and tests. */
  reconnectVersions(waveletId: WaveletId): string[] {
    const entry = this.channels.get(waveletId.serialise());
    return entry === undefined ? [] : entry.channel.getReconnectVersions().map(String);
  }

  private checkUsable(): void {
    if (this.currentState === "failed") {
      throw this.failure;
    }
    if (this.currentState !== "open" && this.currentState !== "opening") {
      throw new Error(`wave view of ${this.options.waveId} is ${this.currentState}`);
    }
  }

  private onChannelCreated(
    channel: OperationChannel,
    snapshot: WaveletData,
    accessibility: Accessibility,
  ): void {
    const key = snapshot.waveletId.serialise();
    const wavelet = this.view.addWavelet(snapshot, {
      record: false,
      participation: accessibility === Accessibility.READ_WRITE ? "auto" : "readOnly",
      output: (op) => {
        try {
          channel.send(op);
        } catch (error) {
          this.fail(
            new WaveFailedError(
              ResponseCode.INVALID_OPERATION.name,
              `Sending an operation of ${snapshot.waveletId} failed: ${messageOf(error)}`,
              { cause: error },
            ),
          );
          throw error;
        }
      },
    });
    this.channels.set(key, { channel, wavelet });
    channel.setListener({
      onOperationReceived: () => this.pump(key, channel, wavelet),
    });
    if (this.removedSinceOpen.delete(key)) {
      this.emitter.emit("waveletReset", { waveletId: snapshot.waveletId });
    }
  }

  private onChannelRemoved(channel: OperationChannel, waveletId: WaveletId): void {
    const key = waveletId.serialise();
    channel.setListener(null);
    if (this.channels.get(key)?.channel === channel) {
      this.channels.delete(key);
      this.removedSinceOpen.add(key);
      this.view.removeWavelet(waveletId);
    }
  }

  /** Applies the operations a channel has received (`OperationSucker`). */
  private pump(key: string, channel: OperationChannel, wavelet: OpBasedWavelet): void {
    while (this.channels.get(key)?.channel === channel) {
      const op = channel.receive();
      if (op === null) {
        return;
      }
      try {
        wavelet.consumeRemote(op);
      } catch (error) {
        this.fail(
          new WaveFailedError(
            ResponseCode.INVALID_OPERATION.name,
            `A server operation does not apply to ${wavelet.waveletId()}: ${messageOf(error)}`,
            { cause: error },
          ),
        );
        return;
      }
    }
  }

  private onFailed(detail: CorruptionDetail): void {
    this.failWith(
      new WaveFailedError(detail.responseCode.name, detail.message, { cause: detail }),
      false,
    );
  }

  private fail(error: WaveFailedError): void {
    this.failWith(error, true);
  }

  private failWith(error: WaveFailedError, closeMux: boolean): void {
    if (this.currentState === "failed" || this.currentState === "closed") {
      return;
    }
    this.currentState = "failed";
    this.failure = error;
    this.detachAll();
    if (closeMux) {
      this.mux.close();
    }
    this.emitter.emit("failed", { error });
  }

  private detachAll(): void {
    for (const { channel } of this.channels.values()) {
      channel.setListener(null);
    }
    this.channels.clear();
  }

  private unsavedChanged(): void {
    let inFlight = 0;
    let unacknowledged = 0;
    let uncommitted = 0;
    for (const info of this.unsavedInfo.values()) {
      inFlight += info.inFlightSize();
      unacknowledged += info.estimateUnacknowledgedSize();
      uncommitted += info.estimateUncommittedSize();
    }
    const next: UnsavedState = {
      inFlight,
      unacknowledged,
      uncommitted,
      saved: unacknowledged === 0 && uncommitted === 0,
    };
    const last = this.lastUnsaved;
    if (
      last.inFlight === next.inFlight &&
      last.unacknowledged === next.unacknowledged &&
      last.uncommitted === next.uncommitted
    ) {
      return;
    }
    this.lastUnsaved = next;
    this.emitter.emit("unsaved", next);
  }
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function rethrowLater(error: unknown): void {
  queueMicrotask(() => {
    throw error;
  });
}
