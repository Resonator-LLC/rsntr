// Multiplexes several operation channels over one view channel.
//
//       |- OperationChannelMultiplexer -----------------------------------------|
//       |                                                                       |
//       |  |-Stacklet---------------------------------|                         |
//       |  | OperationChannel <-> WaveletDeltaChannel |-|                       |
//   <-> |  |------------------------------------------| |-|   <=> View Channel  | <-> WaveService
//       |    |------------------------------------------| |                     |
//       |      |------------------------------------------|                     |
//       |                                                                       |
//       |          All exceptions are directed here                             |
//       |-----------------------------------------------------------------------|
//
// All exceptions that are emitted from using the OperationChannel or
// OperationChannelMultiplexer interfaces are caught in this class, i.e. when
// the client calls methods from the left part of the diagram.
//
// All exceptions generated as a result of handling server messages in the
// ViewChannel are routed here through onException(), i.e. when the
// WaveService calls methods on the right part of the diagram through
// callbacks.
//
// This class is responsible for reporting all the exceptions to the user.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.channel.OperationChannelMultiplexerImpl`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import { WaveletData } from "../../model/data";
import {
  type IdFilter,
  type ParticipantId,
  type WaveId,
  type WaveletId,
  WaveletName,
} from "../../model/id";
import {
  TransformedWaveletDelta,
  type WaveletDelta,
} from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import { ConcurrencyControl } from "../client";
import {
  ChannelException,
  CorruptionDetail,
  IllegalArgumentError,
  IllegalStateError,
  type Logger,
  NO_LOGGER,
  Recoverable,
  ResponseCode,
  type UnsavedDataListenerFactory,
  checkState,
} from "../common";
import { Accessibility } from "./accessibility";
import type { InternalOperationChannel } from "./internalOperationChannel";
import type { OperationChannel } from "./operationChannel";
import { OperationChannelImpl } from "./operationChannelImpl";
import type {
  KnownWavelet,
  OperationChannelMultiplexer,
  OperationChannelMultiplexerListener,
} from "./operationChannelMultiplexer";
import type { Scheduler } from "./scheduler";
import type { SubmitCallback } from "./submitCallback";
import type { ViewChannel, ViewChannelListener } from "./viewChannel";
import type { ViewChannelFactory } from "./viewChannelFactory";
import type { WaveletChannel, WaveletChannelListener } from "./waveletChannel";
import type { WaveletDeltaChannel } from "./waveletDeltaChannel";
import {
  type MultiplexedDeltaChannel,
  WaveletDeltaChannelImpl,
} from "./waveletDeltaChannelImpl";
import { type KnownWaveletVersions, WaveletIdMap } from "./waveletIdMap";

/** No version (`Constants.NO_VERSION`). */
const NO_VERSION = -1;

/** Factory for creating delta channels. */
export interface DeltaChannelFactory {
  /** Creates a delta channel communicating through `waveletChannel`. */
  create(waveletChannel: WaveletChannel): MultiplexedDeltaChannel;
}

/** Factory for operation channels. */
export interface OperationChannelFactory {
  /**
   * Creates an operation channel for the wavelet `waveletId`, communicating
   * through `deltaChannel` and starting from `startVersion`.
   */
  create(
    deltaChannel: WaveletDeltaChannel,
    waveletId: WaveletId,
    startVersion: HashedVersion,
    accessibility: Accessibility,
  ): InternalOperationChannel;
}

/**
 * Synthesizer of initial snapshots for locally-created wavelets
 * (`ObservableWaveletData.Factory` applied to an `EmptyWaveletSnapshot`).
 */
export type EmptyWaveletFactory = (
  waveId: WaveId,
  waveletId: WaveletId,
  creator: ParticipantId,
  versionZero: HashedVersion,
  creationTime: number,
) => WaveletData;

/** The default {@link EmptyWaveletFactory}: an empty `WaveletData`. */
export const createEmptyWaveletSnapshot: EmptyWaveletFactory = (
  waveId,
  waveletId,
  creator,
  versionZero,
  creationTime,
) =>
  WaveletData.empty(
    WaveletName.of(waveId, waveletId),
    creator,
    versionZero,
    creationTime,
  );

/**
 * Produces the hashed version of a wavelet before any delta. The model's
 * `HashedVersionFactory` is one.
 */
export interface VersionZeroFactory {
  createVersionZero(waveletName: WaveletName): HashedVersion;
}

/** Holder class for the copious number of loggers. */
export class LoggerContext {
  /** Discards everything. */
  static readonly NONE = new LoggerContext(
    NO_LOGGER,
    NO_LOGGER,
    NO_LOGGER,
    NO_LOGGER,
  );

  constructor(
    readonly ops: Logger,
    readonly delta: Logger,
    readonly cc: Logger,
    readonly view: Logger,
  ) {}
}

/** Dependencies of the multiplexer that have defaults; overridden in tests. */
export interface OperationChannelMultiplexerOptions {
  /** Timestamps for locally created wavelets, in milliseconds. Default: `Date.now`. */
  clock?: () => number;
  /**
   * Whether the server answers the creation of a wavelet with a snapshot
   * of it, which is then dropped (the channel created locally already has
   * the faked empty one). Default true, as the original assumes. The
   * resonator server sends no such snapshot; with true, the first real
   * snapshot of a locally created wavelet (a reset after a failed resync)
   * would be swallowed instead and the channel left unconnected, so its
   * client passes false. Not in the original.
   */
  dropSnapshotOfCreatedWavelets?: boolean;
  /** Default: creates {@link WaveletDeltaChannelImpl}s. */
  deltaChannelFactory?: DeltaChannelFactory;
  /** Default: creates {@link OperationChannelImpl}s over a `ConcurrencyControl`. */
  opChannelFactory?: OperationChannelFactory;
}

/**
 * A per-wavelet stack above the multiplexer. A stacklet forwards messages
 * from the server to a listener at the bottom of the stacklet (a delta
 * channel). When communications fail a stacklet fetches reconnection
 * versions from the contained operation channel.
 */
class Stacklet implements WaveletChannelListener {
  private firstMessageReceived = false;

  /**
   * `deltaChannel` is at the bottom of the stacklet and `opChannel` at the
   * top. `dropAdditionalSnapshot` is whether to expect and drop an
   * additional snapshot after the first message.
   */
  constructor(
    private readonly deltaChannel: MultiplexedDeltaChannel,
    private readonly opChannel: InternalOperationChannel,
    private dropAdditionalSnapshot: boolean,
  ) {}

  onWaveletSnapshot(
    wavelet: WaveletData | null,
    lastCommittedVersion: HashedVersion | null,
    currentVersion: HashedVersion | null,
  ): void {
    // When a channel is created locally we fake an initial empty
    // snapshot. The server still sends one when it creates the wavelet
    // though, so it's dropped here if that's expected.
    // See createOperationChannel().
    if (!this.firstMessageReceived) {
      this.firstMessageReceived = true;
    } else if (this.dropAdditionalSnapshot) {
      this.dropAdditionalSnapshot = false;
      return;
    }

    this.deltaChannel.onWaveletSnapshot(
      wavelet,
      lastCommittedVersion,
      currentVersion,
    );
  }

  onWaveletUpdate(
    deltas: readonly TransformedWaveletDelta[] | null,
    lastCommittedVersion: HashedVersion | null,
    currentVersion: HashedVersion | null,
  ): void {
    if (!this.firstMessageReceived) {
      this.firstMessageReceived = true;
    }

    this.deltaChannel.onWaveletUpdate(
      deltas,
      lastCommittedVersion,
      currentVersion,
    );
  }

  /** The server's history was rewound to `version`. */
  onRewind(
    version: HashedVersion,
    rewound: readonly TransformedWaveletDelta[],
  ): void {
    this.opChannel.onRewind(version, rewound);
  }

  /** Resets this stacklet ready for reconnection. */
  reset(): void {
    this.deltaChannel.reset(this.opChannel);
    this.opChannel.reset();
  }

  /** Closes this stacklet permanently. */
  close(): void {
    this.deltaChannel.reset(null);
    this.opChannel.close();
  }

  getOperationChannel(): OperationChannel {
    return this.opChannel;
  }

  isExpectingSnapshot(): boolean {
    return this.dropAdditionalSnapshot;
  }
}

/** Multiplexer state. */
type State = "NOT_CONNECTED" | "CONNECTED" | "RECONNECTING";

/** `[a, b]`, like a Java collection. */
function listToString(items: readonly unknown[]): string {
  return `[${items.join(", ")}]`;
}

/**
 * Constructs a map of lists of wavelet signatures from a collection of
 * wavelet snapshots. Only readable wavelets are included.
 */
export function signaturesFromWavelets(
  knownWavelets: Iterable<KnownWavelet>,
): KnownWaveletVersions {
  const signatures: KnownWaveletVersions = new WaveletIdMap();
  for (const knownWavelet of knownWavelets) {
    if (knownWavelet.accessibility.isReadable()) {
      const snapshot = knownWavelet.snapshot;
      signatures.set(snapshot.waveletId, [snapshot.hashedVersion]);
    }
  }
  return signatures;
}

export class OperationChannelMultiplexerImpl implements OperationChannelMultiplexer {
  /** Wave id for channels in this mux. */
  private readonly waveId: WaveId;

  /** Multiplexed channels, indexed by wavelet id. */
  private readonly channels = new WaveletIdMap<Stacklet>();

  /** Factory for creating delta channels. */
  private readonly deltaChannelFactory: DeltaChannelFactory;

  /** Factory for creating operation-channel stacks on top of wave services. */
  private readonly opChannelFactory: OperationChannelFactory;

  /** Factory for creating a view channel. */
  private readonly viewFactory: ViewChannelFactory;

  private readonly logger: Logger;

  /** A stateful manager/factory for unsaved data listeners. */
  private readonly unsavedDataListenerFactory: UnsavedDataListenerFactory | null;

  /** Synthesizer of initial wavelet snapshots for locally-created wavelets. */
  private readonly dataFactory: EmptyWaveletFactory;

  /** Produces hashed versions. */
  private readonly hashFactory: VersionZeroFactory;

  /** Timestamps for locally-created wavelets. */
  private readonly clock: () => number;
  private readonly dropSnapshotOfCreatedWavelets: boolean;

  /** List of commands to run when the underlying view becomes connected. */
  private readonly onConnectedCommands: (() => void)[] = [];

  //
  // Mutable state.
  //

  /** Connection state of the mux. */
  private state: State;

  /** Whether the initial open of the mux has finished. */
  private openFinished = false;

  /**
   * Underlying multiplexed view channel; created on reconnection, set null
   * on close.
   */
  private viewChannel: ViewChannel | null = null;

  /**
   * Tag identifying which view connection is current. Changes on each
   * reconnection.
   */
  private connectionTag = 0;

  /** Filter specifying wavelets to open. */
  private waveletFilter: IdFilter | null = null;

  /** Listener for handling new operation channels. */
  private muxListener: OperationChannelMultiplexerListener | null = null;

  /** Used to back off when reconnecting. */
  private readonly scheduler: Scheduler;

  /**
   * Creates a multiplexer.
   *
   * WARNING: the scheduler should provide back-off. Providing a scheduler
   * which executes immediately or does not back off may cause
   * denial-of-service-like reconnection attempts against the servers.
   *
   * @param waveId wave id to open
   * @param viewFactory factory for opening view channels
   * @param dataFactory factory for making snapshots of empty wavelets (null
   *        for {@link createEmptyWaveletSnapshot})
   * @param loggers log targets (null for none)
   * @param unsavedDataListenerFactory a factory for adding listeners (may be null)
   * @param scheduler scheduler for reconnection
   * @param hashFactory factory for hashed versions
   * @param options overrides of the default clock and channel factories
   */
  constructor(
    waveId: WaveId,
    viewFactory: ViewChannelFactory,
    dataFactory: EmptyWaveletFactory | null,
    loggers: LoggerContext | null,
    unsavedDataListenerFactory: UnsavedDataListenerFactory | null,
    scheduler: Scheduler,
    hashFactory: VersionZeroFactory,
    options: OperationChannelMultiplexerOptions = {},
  ) {
    const loggerContext = loggers ?? LoggerContext.NONE;
    this.waveId = waveId;
    this.deltaChannelFactory = options.deltaChannelFactory ?? {
      create: (waveletChannel) =>
        new WaveletDeltaChannelImpl(waveletChannel, loggerContext.delta),
    };
    this.opChannelFactory = options.opChannelFactory ?? {
      create: (deltaChannel, waveletId, startVersion, accessibility) => {
        const cc = new ConcurrencyControl(loggerContext.cc, startVersion);
        if (unsavedDataListenerFactory !== null) {
          cc.setUnsavedDataListener(
            unsavedDataListenerFactory.create(waveletId),
          );
        }
        return new OperationChannelImpl(
          loggerContext.ops,
          deltaChannel,
          cc,
          accessibility,
        );
      },
    };
    this.viewFactory = viewFactory;
    this.dataFactory = dataFactory ?? createEmptyWaveletSnapshot;
    this.logger = loggerContext.view;
    this.unsavedDataListenerFactory = unsavedDataListenerFactory;
    this.state = "NOT_CONNECTED";
    this.scheduler = scheduler;
    this.hashFactory = hashFactory;
    this.clock = options.clock ?? Date.now;
    this.dropSnapshotOfCreatedWavelets =
      options.dropSnapshotOfCreatedWavelets ?? true;
  }

  open(
    listener: OperationChannelMultiplexerListener,
    waveletFilter: IdFilter,
    knownWaveletSnapshots: Iterable<KnownWavelet> = [],
  ): void {
    const knownWavelets = [...knownWaveletSnapshots];
    this.muxListener = listener;
    this.waveletFilter = waveletFilter;

    try {
      if (knownWavelets.length !== 0) {
        for (const knownWavelet of knownWavelets) {
          const dropAdditionalSnapshot = false;
          this.addOperationChannel(
            knownWavelet.snapshot.waveletId,
            knownWavelet.snapshot,
            knownWavelet.committedVersion,
            knownWavelet.accessibility,
            dropAdditionalSnapshot,
          );
        }
        // consider the wave as if open has finished.
        this.maybeOpenFinished();
      }

      const knownSignatures = signaturesFromWavelets(knownWavelets);
      this.connect(knownSignatures);
    } catch (e) {
      if (!(e instanceof ChannelException)) {
        throw e;
      }
      this.shutdownWithException("Multiplexer open failed.", e);
    }
  }

  close(): void {
    this.shutdown(ResponseCode.OK, "View closed.", null);
  }

  createOperationChannel(waveletId: WaveletId, creator: ParticipantId): void {
    if (this.channels.has(waveletId)) {
      throw new IllegalArgumentError(
        `Operation channel already exists for: ${waveletId}`,
      );
    }

    // Create the new channel, and fake an initial snapshot.
    const v0 = this.hashFactory.createVersionZero(
      WaveletName.of(this.waveId, waveletId),
    );
    const emptySnapshot = this.dataFactory(
      this.waveId,
      waveletId,
      creator,
      v0,
      this.clock(),
    );

    try {
      const dropAdditionalSnapshot = true;
      this.addOperationChannel(
        waveletId,
        emptySnapshot,
        v0,
        Accessibility.READ_WRITE,
        dropAdditionalSnapshot,
      );
    } catch (e) {
      if (!(e instanceof ChannelException)) {
        throw e;
      }
      this.shutdownWithException("Creating operation channel failed.", e);
    }
  }

  /**
   * Creates a view channel listener. The listener will forward messages to
   * stacklets while `connectionTag` has the value it had at creation time.
   * When a channel (re)connects the tag changes.
   *
   * `expectedWavelets` are the wavelets and reconnection versions we expect
   * to receive a message for before `onOpenFinished`.
   */
  private createViewListener(
    expectedWavelets: KnownWaveletVersions,
  ): ViewChannelListener {
    const expectedTag = this.connectionTag;

    /**
     * Wavelets for which we have not yet seen a message, or null after
     * onOpenFinished.
     */
    let missingWavelets: WaveletIdMap<WaveletId> | null = new WaveletIdMap(
      expectedWavelets.keys().map((id): [WaveletId, WaveletId] => [id, id]),
    );

    /** Removes a wavelet id from the set of missing ids if they are being tracked. */
    const removeMissingWavelet = (id: WaveletId): void => {
      if (missingWavelets !== null) {
        missingWavelets.delete(id);
      }
    };

    /**
     * Resets the reconnection scheduler if a message indicates the
     * connection is somewhat ok.
     */
    const maybeResetScheduler = (
      deltas: readonly TransformedWaveletDelta[],
    ): void => {
      // The connection is probably ok if we receive a delta. A snapshot
      // is not sufficient since some are locally generated. The delta need
      // not have ops; a reconnection delta is enough.
      if (deltas.length > 0) {
        this.scheduler.reset();
      }
    };

    return {
      onSnapshot: (
        waveletId: WaveletId,
        wavelet: WaveletData,
        lastCommittedVersion: HashedVersion | null,
        currentVersion: HashedVersion | null,
      ): void => {
        if (this.connectionTag === expectedTag) {
          removeMissingWavelet(waveletId);
          try {
            // Forward message to the appropriate stacklet, creating it if
            // needed.
            let stacklet = this.channels.get(waveletId);
            const dropAdditionalSnapshot = false;
            if (stacklet === undefined) {
              stacklet = this.createStacklet(
                waveletId,
                wavelet,
                Accessibility.READ_WRITE,
                dropAdditionalSnapshot,
              );
            } else if (!stacklet.isExpectingSnapshot()) {
              // Replace the existing stacklet by first removing the wavelet
              // and then adding the newly connected one.
              this.channels.delete(waveletId);
              // The original requires a factory here.
              this.unsavedDataListenerFactory?.destroy(waveletId);
              this.listener().onOperationChannelRemoved(
                stacklet.getOperationChannel(),
                waveletId,
              );
              stacklet = this.createStacklet(
                waveletId,
                wavelet,
                Accessibility.READ_WRITE,
                dropAdditionalSnapshot,
              );
            }
            stacklet.onWaveletSnapshot(
              wavelet,
              lastCommittedVersion,
              currentVersion,
            );
          } catch (e) {
            if (!(e instanceof ChannelException)) {
              throw e;
            }
            throw this.exceptionWithContext(e, waveletId);
          }
        }
      },

      onUpdate: (
        waveletId: WaveletId,
        deltas: readonly TransformedWaveletDelta[],
        lastCommittedVersion: HashedVersion | null,
        currentVersion: HashedVersion | null,
      ): void => {
        if (this.connectionTag === expectedTag) {
          removeMissingWavelet(waveletId);
          maybeResetScheduler(deltas);
          try {
            const stacklet = this.channels.get(waveletId);
            if (stacklet === undefined) {
              throw new IllegalStateError(
                "Received deltas with no stacklet present!",
              );
            }
            stacklet.onWaveletUpdate(
              deltas,
              lastCommittedVersion,
              currentVersion,
            );
          } catch (e) {
            if (!(e instanceof ChannelException)) {
              throw e;
            }
            throw this.exceptionWithContext(e, waveletId);
          }
        } else {
          this.logger.trace("Mux dropping update from defunct view");
        }
      },

      onReset: (
        waveletId: WaveletId,
        version: HashedVersion,
        rewound: readonly TransformedWaveletDelta[],
      ): void => {
        if (this.connectionTag !== expectedTag) {
          this.logger.trace("Mux dropping reset from defunct view");
          return;
        }
        // The concurrency control takes the rewound deltas back and keeps
        // the client's pending operations; reconnecting at the rewound
        // version then resends them, and late answers to a delta in
        // flight are ignored.
        const stacklet = this.channels.get(waveletId);
        if (stacklet !== undefined) {
          try {
            stacklet.onRewind(version, rewound);
          } catch (e) {
            if (!(e instanceof ChannelException)) {
              throw e;
            }
            throw this.exceptionWithContext(e, waveletId);
          }
        }
        this.reconnect(null);
      },

      onOpenFinished: (): void => {
        if (this.connectionTag === expectedTag) {
          if (missingWavelets === null) {
            throw new ChannelException(
              ResponseCode.INTERNAL_ERROR,
              "Multiplexer received openFinished twice",
              null,
              Recoverable.NOT_RECOVERABLE,
              this.waveId,
              null,
            );
          }

          // If a missing wavelet could be reconnected at version zero then
          // fake the resync message here. The server no longer knows about
          // the wavelet so we should resubmit changes from version zero.
          for (const maybeMissing of missingWavelets.keys()) {
            const resyncVersions = expectedWavelets.get(maybeMissing);
            const firstVersion =
              resyncVersions === undefined ? undefined : resyncVersions[0];
            if (firstVersion === undefined) {
              throw new IllegalStateError(
                `Empty resync versions for wavelet ${maybeMissing}`,
              );
            }
            if (firstVersion.version === 0) {
              const stacklet = this.channels.get(maybeMissing);
              if (stacklet === undefined) {
                throw new IllegalStateError(
                  `Resync wavelet has no stacklet. Channels: ${listToString(this.channels.keys())}` +
                    `, resync: ${listToString(expectedWavelets.keys())}`,
                );
              }
              const wavelet = WaveletName.of(this.waveId, maybeMissing);
              const resyncDeltaList = this.createVersionZeroResync(wavelet);
              const v0 = this.hashFactory.createVersionZero(wavelet);
              stacklet.onWaveletUpdate(resyncDeltaList, v0, v0);
              missingWavelets.delete(maybeMissing);
            }
          }

          // Check we received a message for each expected wavelet.
          if (!missingWavelets.isEmpty()) {
            throw new ChannelException(
              ResponseCode.NOT_AUTHORIZED,
              "Server didn't acknowledge known wavelets; perhaps access has been lost: " +
                listToString(missingWavelets.keys()),
              null,
              Recoverable.NOT_RECOVERABLE,
              this.waveId,
              null,
            );
          }
          missingWavelets = null;
          this.maybeOpenFinished();
        } else {
          this.logger.trace("Mux dropping openFinished from defunct view");
        }
      },

      onConnected: (): void => {
        if (this.connectionTag === expectedTag) {
          this.onConnected();
        } else {
          this.logger.trace("Mux dropping onConnected from defunct view");
        }
      },

      onClosed: (): void => {
        if (this.connectionTag === expectedTag) {
          this.reconnect(null);
        } else {
          this.logger.trace("Mux dropping onClosed from defunct view");
        }
      },

      onException: (e: ChannelException): void => {
        if (this.connectionTag === expectedTag) {
          this.onChannelException(e);
        } else {
          this.logger.trace("Mux dropping failure from defunct view");
        }
      },
    };
  }

  /**
   * Creates a stacklet and (optionally) initialises it with a snapshot.
   *
   * `initialiseLocalChannel` is whether to send the snapshot through the
   * stacklet, in which case it should expect and drop an additional snapshot
   * from the network.
   */
  private addOperationChannel(
    waveletId: WaveletId,
    snapshot: WaveletData,
    committedVersion: HashedVersion,
    accessibility: Accessibility,
    initialiseLocalChannel: boolean,
  ): void {
    const stacklet = this.createStacklet(
      waveletId,
      snapshot,
      accessibility,
      initialiseLocalChannel && this.dropSnapshotOfCreatedWavelets,
    );
    if (initialiseLocalChannel) {
      const currentVersion = snapshot.hashedVersion;
      this.initialiseLocallyCreatedStacklet(
        stacklet,
        waveletId,
        snapshot,
        committedVersion,
        currentVersion,
      );
    }
  }

  /**
   * This is an ugly work-around for the lack of ability to add channels to
   * a view in the view service API. We need to send some message through the
   * stacklet so it's connected but the server can't send us any message
   * until we submit the first delta, which requires a connected stacklet...
   */
  private initialiseLocallyCreatedStacklet(
    stacklet: Stacklet,
    waveletId: WaveletId,
    snapshot: WaveletData,
    committedVersion: HashedVersion,
    currentVersion: HashedVersion,
  ): void {
    if (this.state === "CONNECTED") {
      try {
        stacklet.onWaveletSnapshot(snapshot, committedVersion, currentVersion);
      } catch (e) {
        if (!(e instanceof ChannelException)) {
          throw e;
        }
        throw this.exceptionWithContext(e, waveletId);
      }
    } else {
      // Delay connecting the stacklet until the underlying view is connected.
      this.onConnectedCommands.push(() => {
        try {
          stacklet.onWaveletSnapshot(
            snapshot,
            committedVersion,
            currentVersion,
          );
        } catch (e) {
          if (!(e instanceof ChannelException)) {
            throw e;
          }
          this.shutdownWithException(
            `Fake snapshot for wavelet channel ${this.waveId}/${waveletId}failed`,
            this.exceptionWithContext(e, waveletId),
          );
        }
      });
    }
  }

  /**
   * Adds a new operation-channel stacklet to this multiplexer and notifies
   * the listener of the new channel's creation.
   *
   * If `accessibility` is not `READ_WRITE` then the stacklet will fail on
   * send. `dropSnapshot` is whether to expect and drop an additional
   * snapshot from the view.
   */
  private createStacklet(
    waveletId: WaveletId,
    snapshot: WaveletData,
    accessibility: Accessibility,
    dropSnapshot: boolean,
  ): Stacklet {
    if (this.channels.has(waveletId)) {
      throw new IllegalArgumentError(
        `Cannot create duplicate channel for wavelet: ${this.waveId}/${waveletId}`,
      );
    }
    const waveletChannel = this.createWaveletChannel(waveletId);
    const deltaChannel = this.deltaChannelFactory.create(waveletChannel);
    const opChannel = this.opChannelFactory.create(
      deltaChannel,
      waveletId,
      snapshot.hashedVersion,
      accessibility,
    );
    const stacklet = new Stacklet(deltaChannel, opChannel, dropSnapshot);
    stacklet.reset();
    this.channels.set(waveletId, stacklet);

    if (this.muxListener !== null) {
      this.muxListener.onOperationChannelCreated(
        stacklet.getOperationChannel(),
        snapshot,
        accessibility,
      );
    }

    return stacklet;
  }

  /** Executes any pending commands in the on-connected queue. */
  private onConnected(): void {
    this.state = "CONNECTED";
    // Connect all channels created before now.
    for (const command of this.onConnectedCommands) {
      command();
    }
    this.onConnectedCommands.length = 0;
  }

  /** Handles failure of the view channel or an operation channel. */
  private onChannelException(e: ChannelException): void {
    if (e.recoverable !== Recoverable.RECOVERABLE) {
      this.shutdown(e.responseCode, "Channel Exception", e);
    } else {
      this.reconnect(e);
    }
  }

  private connect(knownWavelets: KnownWaveletVersions): void {
    checkState(
      this.state !== "CONNECTED",
      "Cannot connect already-connected channel",
    );
    const waveletFilter = this.filter();
    this.checkConnectVersions(knownWavelets);
    this.logger.trace(`Multiplexer reconnecting wave ${this.waveId}`);
    this.viewChannel = this.viewFactory.create(this.waveId);
    this.viewChannel.open(
      this.createViewListener(knownWavelets),
      waveletFilter,
      knownWavelets,
    );
  }

  /**
   * Checks that reconnect versions are strictly increasing and removes any
   * that are not accepted by the connection's wavelet filter.
   */
  private checkConnectVersions(knownWavelets: KnownWaveletVersions): void {
    const waveletFilter = this.filter();
    for (const [id, versions] of knownWavelets.entries()) {
      if (waveletFilter.accepts(id)) {
        let prevVersion = NO_VERSION;
        for (const v of versions) {
          if (prevVersion !== NO_VERSION && v.version <= prevVersion) {
            throw new IllegalArgumentError(
              `Invalid reconnect versions for ${this.waveId}${id}: ${listToString(versions)}`,
            );
          }
          prevVersion = v.version;
        }
      } else {
        this.logger.error(
          `Mux for ${this.waveId} dropping resync versions for filtered wavelet ${id}` +
            `, filter ${waveletFilter}`,
        );
        knownWavelets.delete(id);
      }
    }
  }

  /**
   * Terminates all stacklets then reconnects with the known versions
   * provided by them. `_exception` is the exception that caused the
   * reconnection, if any.
   */
  private reconnect(_exception: ChannelException | null): void {
    this.logger.trace(
      "Multiplexer disconnected in state ",
      this.state,
      ", reconnecting.",
    );
    this.state = "RECONNECTING";

    // NOTE: don't clear the on-connected commands as we'll lose wavelets if
    // we've never been connected.

    // Reset each stacklet, collecting the reconnect versions.
    const knownWavelets: KnownWaveletVersions = new WaveletIdMap();
    for (const [wavelet, stacklet] of this.channels.entries()) {
      stacklet.reset();
      knownWavelets.set(
        wavelet,
        stacklet.getOperationChannel().getReconnectVersions(),
      );
    }

    // Close the view channel and ignore future messages from it.
    this.connectionTag++;
    this.view().close();

    // Run the connect part in the scheduler
    const tag = this.connectionTag;
    this.scheduler.schedule({
      execute: () => {
        if (tag === this.connectionTag) {
          // Reconnect by creating another view channel.
          this.connect(knownWavelets);
        }
      },
    });
  }

  /**
   * Shuts down this multiplexer permanently.
   *
   * If `reasonCode` is not `ResponseCode.OK` then the listener will be
   * notified of connection failure. `exception` is any exception that
   * caused the shutdown.
   */
  private shutdown(
    reasonCode: ResponseCode,
    description: string | null,
    exception: unknown,
  ): void {
    if (description === null) {
      description = "(No error description provided)";
    }

    const notifyFailure = reasonCode !== ResponseCode.OK;

    // We are telling the user through UI that the wave is corrupt, so we
    // must also report it.
    if (notifyFailure) {
      if (exception === null || exception === undefined) {
        this.logger.error(description);
      } else {
        this.logger.error(description, exception);
      }
    }

    if (this.viewChannel !== null) {
      // Ignore future messages.
      this.connectionTag++;
      this.state = "NOT_CONNECTED";

      for (const stacklet of this.channels.values()) {
        stacklet.close();
      }
      this.channels.clear();
      this.viewChannel.close();
      this.viewChannel = null;
      if (this.muxListener !== null && notifyFailure) {
        this.muxListener.onFailed(
          new CorruptionDetail(reasonCode, description, exception),
        );
      }
      this.muxListener = null;
    }
  }

  /** Shuts down this multiplexer permanently after an exception. */
  private shutdownWithException(message: string, e: ChannelException): void {
    this.shutdown(e.responseCode, message, e);
  }

  /** Creates a wavelet channel for submissions against a wavelet. */
  private createWaveletChannel(waveletId: WaveletId): WaveletChannel {
    return {
      submit: (delta: WaveletDelta, callback: SubmitCallback): void => {
        this.view().submitDelta(waveletId, delta, callback);
      },

      debugGetProfilingInfo: (): string | null =>
        this.view().debugGetProfilingInfo(waveletId),
    };
  }

  private maybeOpenFinished(): void {
    // Forward message to the mux's open listener.
    if (!this.openFinished) {
      this.openFinished = true;
      this.listener().onOpenFinished();
    }
  }

  /** Wraps a channel exception in another providing wave and wavelet id context. */
  private exceptionWithContext(
    e: ChannelException,
    waveletId: WaveletId,
  ): ChannelException {
    return new ChannelException(
      e.responseCode,
      "Nested ChannelException",
      e,
      e.recoverable,
      this.waveId,
      waveletId,
    );
  }

  /**
   * Creates a container message mimicking a resync message for a wavelet at
   * version zero.
   */
  private createVersionZeroResync(
    wavelet: WaveletName,
  ): TransformedWaveletDelta[] {
    // As in the original, the resync delta has no author. It has no
    // operations either, and the delta channel reads only its version.
    const noAuthor = null as unknown as ParticipantId;
    return [
      new TransformedWaveletDelta(
        noAuthor,
        this.hashFactory.createVersionZero(wavelet),
        0,
        [],
      ),
    ];
  }

  /** The view channel; the original fails on a null reference without one. */
  private view(): ViewChannel {
    if (this.viewChannel === null) {
      throw new IllegalStateError(
        `Multiplexer for ${this.waveId} has no view channel`,
      );
    }
    return this.viewChannel;
  }

  /** The mux listener; the original fails on a null reference without one. */
  private listener(): OperationChannelMultiplexerListener {
    if (this.muxListener === null) {
      throw new IllegalStateError(
        `Multiplexer for ${this.waveId} has no listener`,
      );
    }
    return this.muxListener;
  }

  /** The wavelet filter, set on open. */
  private filter(): IdFilter {
    if (this.waveletFilter === null) {
      throw new IllegalStateError(`Multiplexer for ${this.waveId} is not open`);
    }
    return this.waveletFilter;
  }
}
