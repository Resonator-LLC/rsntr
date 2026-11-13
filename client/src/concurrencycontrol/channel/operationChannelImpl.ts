// Mediates between a WaveletDeltaChannel and the client ConcurrencyControl.
// It sends operations via concurrency control in deltas through a wavelet
// delta channel, and accepts deltas from the delta channel as broadcast
// from the server. Ops are transformed and queued in ConcurrencyControl.
//
// The delta channel is connected through a raw wavelet channel.
//
//   ConcurrencyControl
//          ^
//          |
//          v
//   OperationChannel  <=>  WaveletDeltaChannel <=> WaveletChannel
//
// The channel allows reconnection after some failures by providing
// reconnection versions and reconciling concurrency control state with the
// server's state. Recovery can fail when more than one client modifies the
// wave, and the wave server crashes losing history about the order of
// deltas accepted.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.channel.OperationChannelImpl`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type {
  TransformedWaveletDelta,
  WaveletDelta,
  WaveletOperation,
} from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type {
  ConcurrencyControl,
  ConnectionListener,
  ServerConnection,
} from "../client";
import {
  ChannelException,
  IllegalStateError,
  type Logger,
  NO_LOGGER,
  Recoverable,
  ResponseCode,
  checkState,
  isOperationException,
  isTransformException,
} from "../common";
import type { Accessibility } from "./accessibility";
import type { InternalOperationChannel } from "./internalOperationChannel";
import type { OperationChannelListener } from "./operationChannel";
import { ClientMessage, type WaveletDeltaChannel } from "./waveletDeltaChannel";

type State =
  /** Initial state, or when we are recovering from disconnection. */
  | "NOT_CONNECTED"
  /** After onConnection(). */
  | "CONNECTED"
  /** After close(). */
  | "CLOSED";

export class OperationChannelImpl implements InternalOperationChannel {
  private readonly logger: Logger;
  private readonly accessibility: Accessibility;

  // "Final", but null after the channel is closed.
  private deltaChannel: WaveletDeltaChannel | null;
  private listener: OperationChannelListener | null = null;
  private cc: ConcurrencyControl | null;
  private state: State = "NOT_CONNECTED";

  /** CC server connection which passes requests to the delta channel. */
  private readonly ccServerConnection: ServerConnection = {
    isOpen: (): boolean => this.state === "CONNECTED",

    send: (delta: WaveletDelta): void => {
      if (this.logger.shouldLogTrace()) {
        this.logger.trace("sending delta: ", delta);
      }
      // Deviation from the original, which transmits `delta` as captured
      // here. The delta channel may hold the transmission back while it
      // delivers queued server deltas (several deltas in one update, as
      // after a reconnection); meanwhile the concurrency control
      // transforms its unacknowledged delta against them and moves its
      // target version, or takes it back into its queue. The captured delta
      // would then be stale: against an intermediate version, for which
      // this protocol supplies no history hash, and possibly sent a second
      // time later. So the delta is read when the transmission happens.
      this.openDeltaChannel().send({
        takeMessage: () =>
          new ClientMessage(this.cc?.getUnacknowledgedDelta() ?? null, false),
      });
    },

    debugGetProfilingInfo: (): string =>
      this.openDeltaChannel().debugGetProfilingInfo(),
  };

  /** CC client listener which passes events on to the connection listener. */
  private readonly ccListener: ConnectionListener = {
    onOperationReceived: (): void => this.signalOperationReceived(),
  };

  /**
   * Constructs an operation channel over `deltaChannel`, with the
   * concurrency control module `cc`.
   *
   * Any operations sent before onConnection are queued to be sent upon
   * connection.
   */
  constructor(
    opsLogger: Logger | null | undefined,
    deltaChannel: WaveletDeltaChannel,
    cc: ConcurrencyControl,
    accessibility: Accessibility,
  ) {
    this.deltaChannel = deltaChannel;
    this.cc = cc;
    this.accessibility = accessibility;
    this.logger = opsLogger ?? NO_LOGGER;
    this.state = "NOT_CONNECTED";

    cc.initialise(this.ccServerConnection, this.ccListener);
  }

  /** The concurrency control; using it after close is a programming error. */
  private openCc(): ConcurrencyControl {
    if (this.cc === null) {
      throw new IllegalStateError(`Operation channel is closed: ${this}`);
    }
    return this.cc;
  }

  /** The delta channel; using it after close is a programming error. */
  private openDeltaChannel(): WaveletDeltaChannel {
    if (this.deltaChannel === null) {
      throw new IllegalStateError(`Operation channel is closed: ${this}`);
    }
    return this.deltaChannel;
  }

  private describe(): string {
    return `${this}, ${this.deltaChannel}, ${this.cc}`;
  }

  // WaveletDeltaChannelReceiver implementation //

  onConnection(
    connectVersion: HashedVersion,
    currentVersion: HashedVersion,
  ): void {
    checkState(
      this.state === "NOT_CONNECTED",
      `OperationChannel received onConnection in state ${this.state}`,
    );
    checkState(this.cc !== null, "Cannot connect a closed channel");
    this.state = "CONNECTED";
    this.cc.onOpen(connectVersion, currentVersion);
  }

  onDelta(delta: TransformedWaveletDelta): void {
    try {
      if (this.logger.shouldLogTrace()) {
        this.logger.trace("Received delta: ", delta);
      }
      this.openCc().onServerDelta(delta);
    } catch (e) {
      // The model's operation errors do not tell schema violations apart, so
      // unlike the original no failure maps to SCHEMA_VIOLATION here.
      if (isTransformException(e) || isOperationException(e)) {
        throw new ChannelException(
          ResponseCode.INVALID_OPERATION,
          `Operation channel failed on server delta: ${this.describe()}`,
          e,
          Recoverable.NOT_RECOVERABLE,
          null,
          null,
        );
      }
      throw e;
    }
  }

  onAck(opsApplied: number, signature: HashedVersion): void {
    try {
      this.openCc().onSuccess(opsApplied, signature);
    } catch (e) {
      if (isTransformException(e)) {
        throw new ChannelException(
          ResponseCode.INVALID_OPERATION,
          `Operation channel failed on ack: ${this.describe()}`,
          e,
          Recoverable.NOT_RECOVERABLE,
          null,
          null,
        );
      }
      throw e;
    }
  }

  onNack(
    responseCode: ResponseCode,
    errorString: string | null,
    _version: number,
  ): void {
    throw new ChannelException(
      responseCode,
      `Operation channel failed on nack: code=${responseCode}, ${errorString}, ${this.describe()}`,
      null,
      Recoverable.NOT_RECOVERABLE,
      null,
      null,
    );
  }

  onCommit(version: number): void {
    this.openCc().onCommit(version);
  }

  // OperationChannel implementation //

  setListener(listener: OperationChannelListener | null): void {
    this.listener = listener;
  }

  send(...operations: WaveletOperation[]): void {
    if (this.state === "CLOSED") {
      this.logger.error(`Cannot send to closed operation channel: ${this}`);
    } else if (this.accessibility.isWritable()) {
      try {
        this.openCc().onClientOperations(operations);
      } catch (e) {
        if (isTransformException(e)) {
          throw new ChannelException(
            ResponseCode.INVALID_OPERATION,
            `Operation channel failed on send: ${this.describe()}`,
            e,
            Recoverable.NOT_RECOVERABLE,
            null,
            null,
          );
        }
        throw e;
      }
    } else {
      throw new ChannelException(
        ResponseCode.NOT_AUTHORIZED,
        "Attempt to write to inaccessible wavelet",
        null,
        Recoverable.NOT_RECOVERABLE,
        null,
        null,
      );
    }
  }

  receive(): WaveletOperation | null {
    if (this.state === "CLOSED") {
      this.logger.error(
        `Cannot receive from closed operation channel: ${this}`,
      );
      return null;
    } else {
      return this.openCc().receive();
    }
  }

  peek(): WaveletOperation | null {
    if (this.state === "CLOSED") {
      this.logger.error(`Cannot peek at closed operation channel: ${this}`);
      return null;
    } else {
      return this.openCc().peek();
    }
  }

  getReconnectVersions(): HashedVersion[] {
    if (this.state === "CLOSED") {
      throw new IllegalStateError(
        `Cannot query closed operation channel: ${this}`,
      );
    }
    return this.openCc().getReconnectionVersions();
  }

  onRewind(
    version: HashedVersion,
    rewound: readonly TransformedWaveletDelta[],
  ): void {
    try {
      this.openCc().onReset(version, rewound);
    } catch (e) {
      if (isTransformException(e) || isOperationException(e)) {
        throw new ChannelException(
          ResponseCode.INVALID_OPERATION,
          `Operation channel failed on a rewind: ${this.describe()}`,
          e,
          Recoverable.NOT_RECOVERABLE,
          null,
          null,
        );
      }
      throw e;
    }
  }

  /** Resets this channel ready to be reconnected. */
  reset(): void {
    this.state = "NOT_CONNECTED";
  }

  /** Closes this channel permanently. */
  close(): void {
    checkState(this.state !== "CLOSED", "Cannot close already-closed channel");
    this.state = "CLOSED";
    this.openCc().close();

    // Disconnect everything.
    this.cc = null;
    this.deltaChannel = null;
    this.listener = null;
  }

  /** Signals to the listener that an operation is ready. */
  private signalOperationReceived(): void {
    if (this.listener !== null) {
      this.listener.onOperationReceived();
    }
  }

  toString(): string {
    return `Operation Channel State = [state: ${this.state}]`;
  }

  getDebugString(): string {
    return `${this.cc}==========\n${this}==========\n${this.deltaChannel}`;
  }
}
