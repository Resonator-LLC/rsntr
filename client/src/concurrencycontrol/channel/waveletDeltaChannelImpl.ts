// A delta channel over a raw wavelet channel. Submissions are serialised
// (one in flight at a time) and server messages (deltas, commits, acks and
// nacks) are delivered to the receiver in version order, holding back
// messages that arrive out of order.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.channel.WaveletDeltaChannelImpl`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveletData } from "../../model/data";
import type {
  TransformedWaveletDelta,
  WaveletDelta,
} from "../../model/operation";
import { HashedVersion } from "../../model/version";
import {
  ChannelException,
  IllegalStateError,
  type Logger,
  NO_LOGGER,
  Recoverable,
  ResponseCode,
  checkArgument,
  checkState,
} from "../common";
import type { WaveletChannel, WaveletChannelListener } from "./waveletChannel";
import type {
  WaveletDeltaChannel,
  WaveletDeltaChannelReceiver,
  WaveletDeltaChannelTransmitter,
} from "./waveletDeltaChannel";

/** The version of a channel that has no version yet (`Constants.NO_VERSION`). */
const NO_VERSION = -1;

/**
 * Binds together both ends of a delta channel
 * (`OperationChannelMultiplexerImpl.MultiplexedDeltaChannel`).
 */
export interface MultiplexedDeltaChannel
  extends WaveletDeltaChannel, WaveletChannelListener {}

function summariseDelta(delta: WaveletDelta | null): string {
  if (delta === null) {
    return "null";
  }
  let b = `delta version: ${delta.targetVersion}, ops: ${delta.length}, `;
  for (const op of delta) {
    b += `${op}, `;
  }
  return b;
}

function summariseTransformedDelta(
  delta: TransformedWaveletDelta | null,
): string {
  if (delta === null) {
    return "null";
  }
  let b =
    `delta applied-version: ${delta.appliedAtVersion}, ` +
    `end-version: ${delta.resultingVersion}, ops: ${delta.length}, `;
  for (const op of delta) {
    b += `${op}, `;
  }
  return b;
}

/**
 * Base class for wave delta channel messages from wave server to wave client.
 *
 *     datatype ServerMessage = Delta(WaveDeltaMessage delta)
 *                            | Committed(long version)
 *                            | Ack(int opsApplied, long version)
 *                            | Nack(String errorString, long version)
 */
abstract class ServerMessage {
  /**
   * `startVersion` is the wavelet version to which the message applies and
   * `endVersion` the wavelet version after the message.
   */
  protected constructor(
    readonly startVersion: number,
    readonly endVersion: number,
  ) {}

  /**
   * Delivers the server message to a receiver by calling the receiver
   * method that corresponds to the message type. Throws `ChannelException`
   * if the upstream channel fails.
   */
  abstract deliverTo(receiver: WaveletDeltaChannelReceiver): void;

  /** Messages are ordered lexicographically by (startVersion, endVersion). */
  compareTo(other: ServerMessage): number {
    if (this.startVersion < other.startVersion) {
      return -1;
    } else if (this.startVersion > other.startVersion) {
      return 1;
    }
    return this.endVersion < other.endVersion
      ? -1
      : this.endVersion > other.endVersion
        ? 1
        : 0;
  }
}

/** Delta update from another wave participant. */
class ServerDelta extends ServerMessage {
  constructor(private readonly delta: TransformedWaveletDelta) {
    super(delta.appliedAtVersion, delta.appliedAtVersion + delta.length);
  }

  override deliverTo(receiver: WaveletDeltaChannelReceiver): void {
    receiver.onDelta(this.delta);
  }

  override toString(): string {
    return `ServerDelta(${this.startVersion}, ${summariseTransformedDelta(this.delta)})`;
  }
}

/**
 * Notification that the wave server has committed operations up to the
 * specified version number to replicated, persistent storage.
 */
class Committed extends ServerMessage {
  constructor(
    sequenceVersion: number,
    private readonly committedVersion: number,
  ) {
    super(sequenceVersion, sequenceVersion);
  }

  override deliverTo(receiver: WaveletDeltaChannelReceiver): void {
    receiver.onCommit(this.committedVersion);
  }

  override toString(): string {
    return `Committed(${this.startVersion}, ${this.committedVersion})`;
  }
}

/**
 * Positive acknowledgement (accept) of operations submitted by the wave
 * client on this connection.
 */
class Ack extends ServerMessage {
  constructor(
    private readonly opsApplied: number,
    private readonly hashedVersion: HashedVersion,
  ) {
    super(hashedVersion.version - opsApplied, hashedVersion.version);
  }

  override deliverTo(receiver: WaveletDeltaChannelReceiver): void {
    receiver.onAck(this.opsApplied, this.hashedVersion);
  }

  override toString(): string {
    return `Ack(${this.startVersion}, ${this.opsApplied} ops, ${this.hashedVersion})`;
  }
}

/**
 * Negative acknowledgment (reject) of an operation submitted by the wave
 * client on this connection.
 */
class Nack extends ServerMessage {
  constructor(
    sequenceVersion: number,
    private readonly responseCode: ResponseCode,
    private readonly errorString: string | null,
  ) {
    super(sequenceVersion, sequenceVersion);
    checkArgument(
      responseCode !== ResponseCode.OK,
      "Shouldn't build NACK message for response with status OK",
    );
  }

  override deliverTo(receiver: WaveletDeltaChannelReceiver): void {
    receiver.onNack(this.responseCode, this.errorString, this.endVersion);
  }

  override toString(): string {
    return `Nack(${this.endVersion}, code ${this.responseCode}, "${this.errorString}")`;
  }
}

type State = "INITIAL" | "CONNECTED";

export class WaveletDeltaChannelImpl implements MultiplexedDeltaChannel {
  private readonly logger: Logger;
  private state: State = "INITIAL";
  /** Channel on which to submit messages. */
  private readonly channel: WaveletChannel;
  /** Listener for received messages. */
  private receiver: WaveletDeltaChannelReceiver | null = null;
  /** Tag which changes with each reconnection to identify late acks. */
  private connectionTag = 0;
  /** Ready-to-transmit message. */
  private transmitter: WaveletDeltaChannelTransmitter | null = null;

  /**
   * The single in-flight outbound message, if any, otherwise null.
   * Only one "slot" is needed as outbound messages are serialized.
   */
  private transmitDelta: WaveletDelta | null = null;

  /** The last delta submitted and responded to by the server. */
  private lastTransmitDelta: WaveletDelta | null = null;

  /** Last submit acknowledgement from the server. */
  private lastAckedVersion: HashedVersion | null = null;

  /** Version number of last delta or acknowledgement delivered to receiver. */
  private lastServerVersion = NO_VERSION;

  /**
   * Queue to hold back out-of-order messages while awaiting the next
   * in-order message. Items in the queue are ordered by (startVersion,
   * endVersion) and the startVersion of each message should equal the
   * endVersion of the previous message.
   */
  private readonly queue: ServerMessage[] = [];

  /**
   * Constructs a new channel submitting deltas on `channel`. The channel is
   * initially disconnected until it receives a first wavelet update. The
   * channel must be `reset` to install a receiver before any messages may be
   * received.
   */
  constructor(channel: WaveletChannel, logger: Logger = NO_LOGGER) {
    this.channel = channel;
    this.logger = logger;
    this.logTrace("New delta channel created");
  }

  // WaveletDeltaChannel implementation.
  // Receives messages from the local client.

  reset(receiver: WaveletDeltaChannelReceiver | null): void {
    this.internalReset();
    this.receiver = receiver;
  }

  send(t: WaveletDeltaChannelTransmitter): void {
    if (this.state !== "CONNECTED") {
      throw new IllegalStateError(
        `Sending message over a channel that is not connected. state: ${this.state}`,
      );
    }
    this.transmitter = t;
    this.tryTransmit();
  }

  // WaveletChannelListener implementation.
  // Receives messages from the remote server.

  onWaveletSnapshot(
    wavelet: WaveletData | null,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void {
    if (this.state !== "INITIAL") {
      throw new IllegalStateError(
        `Received a snapshot in a state that is not initial. state: ${this.state}`,
      );
    }
    checkState(
      this.lastServerVersion === NO_VERSION,
      "Expected no last version for initial state",
    );
    this.processConnectSnapshotMessage(
      wavelet,
      lastCommittedVersion,
      currentSignedVersion,
    );
  }

  onWaveletUpdate(
    deltas: readonly TransformedWaveletDelta[] | null,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void {
    switch (this.state) {
      case "INITIAL":
        checkState(
          this.lastServerVersion === NO_VERSION,
          "Expected no last version for initial state",
        );
        this.processConnectUpdateMessage(
          deltas,
          lastCommittedVersion,
          currentSignedVersion,
        );
        break;
      case "CONNECTED":
        checkState(
          this.lastServerVersion >= 0,
          "Expected last server version > 0",
        );
        this.processUpdateMessage(deltas, lastCommittedVersion);
        break;
    }
  }

  private processConnect(
    connectVersion: HashedVersion,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void {
    // The first update must contain a committed version.
    if (lastCommittedVersion === null) {
      throw new ChannelException(
        "Channel connect message lacks committed version",
        Recoverable.NOT_RECOVERABLE,
      );
    }
    // Update must contain either wavelet (+ blips etc)
    // or deltas (beginning with an empty delta to communicate the initial
    // version and signature).
    this.state = "CONNECTED";
    this.connectionTag++;

    const currentVersion =
      currentSignedVersion !== null ? currentSignedVersion : connectVersion;

    this.lastServerVersion = connectVersion.version;
    if (this.receiver !== null) {
      this.receiver.onConnection(connectVersion, currentVersion);
    }
    this.processLastCommittedVersion(lastCommittedVersion);
    this.flushServerMessages();
  }

  /**
   * Processes the first incoming stream message, if it's a snapshot.
   * `currentSignedVersion` is the current wavelet version, when reconnecting
   * at an older version.
   */
  private processConnectSnapshotMessage(
    wavelet: WaveletData | null,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void {
    if (wavelet === null) {
      throw new ChannelException(
        "Delta channel: invalid first wave stream message: null" +
          `, last committed version: ${lastCommittedVersion}` +
          `currentSignedVersion: ${currentSignedVersion}`,
        Recoverable.NOT_RECOVERABLE,
      );
    }

    // This stream has connected at the latest version of the wavelet.
    const connectVersion = wavelet.hashedVersion;
    this.logTrace(
      "Delta channel connecting, wavelet id ",
      wavelet.waveId,
      "/",
      wavelet.waveletId,
      ", version ",
      connectVersion.version,
    );

    this.processConnect(
      connectVersion,
      lastCommittedVersion,
      currentSignedVersion,
    );
  }

  /**
   * Processes the first incoming stream message, if it's a list of deltas:
   * an empty delta carrying the reconnect version and signature.
   */
  private processConnectUpdateMessage(
    deltas: readonly TransformedWaveletDelta[] | null,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void {
    const firstDelta = deltas === null ? undefined : deltas[0];
    if (firstDelta === undefined) {
      throw new ChannelException(
        "Delta channel: no deltas in first wave stream message",
        Recoverable.NOT_RECOVERABLE,
      );
    }

    // This stream has reconnected at a previous version of the wavelet.
    // The first delta has zero operations. It carries the reconnect version
    // and signature.
    if (firstDelta.length !== 0) {
      throw new ChannelException(
        "Delta channel: invalid first wave stream message, " +
          `expected no ops, got ${firstDelta.length}`,
        Recoverable.NOT_RECOVERABLE,
      );
    }
    const connectVersion = HashedVersion.of(
      firstDelta.appliedAtVersion,
      firstDelta.resultingVersion.historyHash,
    );
    this.logTrace(
      "Delta channel reconnecting, connect version ",
      connectVersion.version,
      " current version ",
      currentSignedVersion !== null
        ? currentSignedVersion.version
        : connectVersion.version,
    );

    this.processConnect(
      connectVersion,
      lastCommittedVersion,
      currentSignedVersion,
    );
  }

  /**
   * Processes a subsequent (not first) incoming stream message. Either
   * parameter may be null.
   */
  private processUpdateMessage(
    deltas: readonly TransformedWaveletDelta[] | null,
    lastCommittedVersion: HashedVersion | null,
  ): void {
    if (deltas !== null) {
      // Update must contain deltas or lastCommittedVersion, no wave.
      if (deltas.length === 0 && lastCommittedVersion === null) {
        throw new ChannelException(
          "Delta channel: invalid non-first wave stream message: count " +
            `${deltas.length}deltas (lastServerVersion: ${this.lastServerVersion})`,
          Recoverable.NOT_RECOVERABLE,
        );
      }
      this.processDeltas(deltas);
    }
    this.processLastCommittedVersion(lastCommittedVersion);
    this.flushServerMessages();
  }

  /** Processes the last committed version (if any) from an update. */
  private processLastCommittedVersion(
    lastCommittedVersion: HashedVersion | null,
  ): void {
    if (lastCommittedVersion !== null) {
      // Synthesize a "sequenceVersion" for the committed message to put it in
      // the most useful place in the message queue. If possible use the
      // committed version, which causes it to be delivered to the receiver in
      // sequence with other messages. If the queue has already progressed past
      // that, place the message at the beginning of the queue, namely at
      // lastServerVersion.
      const committedVersion = lastCommittedVersion.version;
      const sequenceVersion = Math.max(
        committedVersion,
        this.lastServerVersion,
      );
      this.onServerMessage(new Committed(sequenceVersion, committedVersion));
    }
  }

  /** Processes deltas from an incoming server message. */
  private processDeltas(deltas: readonly TransformedWaveletDelta[]): void {
    for (const delta of deltas) {
      this.logDelta("Incoming", delta);
      const serverDelta = new ServerDelta(delta);
      const queuePos = this.onServerMessage(serverDelta);
      this.checkForMissingMessages(serverDelta, queuePos);
    }
  }

  /**
   * Queues up a message from the server in startVersion order. Returns the
   * position in the queue at which the message was inserted.
   */
  private onServerMessage(message: ServerMessage): number {
    if (message.startVersion < this.lastServerVersion) {
      throw new ChannelException(
        "Delta channel: out of sequence server message with version " +
          `${message.startVersion}: ${message}, ${this}` +
          `; lastServerVersion: ${this.lastServerVersion}`,
        Recoverable.NOT_RECOVERABLE,
      );
    }
    let pos = this.queue.length;
    while (
      pos > 0 &&
      (this.queue[pos - 1] as ServerMessage).compareTo(message) > 0
    ) {
      pos--;
    }
    this.queue.splice(pos, 0, message);
    return pos;
  }

  /**
   * Delivers queued messages from the server to the wave client in version
   * order. If there are missing versions (should mean that messages are
   * reordered and messages with the missing numbers are still in flight) we
   * hold back the queued messages with larger versions.
   */
  private flushServerMessages(): void {
    for (;;) {
      const message = this.queue[0];
      if (
        message === undefined ||
        message.startVersion !== this.lastServerVersion
      ) {
        break;
      }
      this.queue.shift();
      if (message.endVersion < this.lastServerVersion) {
        throw new IllegalStateError(
          "Delta channel queue is out of order. Message endVersion " +
            `${message.endVersion}, lastServerVersion ${this.lastServerVersion}`,
        );
      }
      this.lastServerVersion = message.endVersion;
      if (this.receiver !== null) {
        this.logTrace("Releasing message ", message);
        message.deliverTo(this.receiver);
      }
    }

    this.tryTransmit();
  }

  /**
   * Checks that inserting an incoming message into the queue at `queuePos`
   * does not leave the channel with a version gap. Throws
   * `ChannelException` if a missing message is detected.
   */
  private checkForMissingMessages(
    incoming: ServerMessage,
    queuePos: number,
  ): void {
    let expectedVersion: number;
    if (queuePos === 0) {
      expectedVersion = this.lastServerVersion;
    } else {
      const previous = this.queue[queuePos - 1] as ServerMessage;
      expectedVersion = previous.endVersion;
    }

    // If there's a gap between the incoming message start version and
    // the previous message end version then it must be accounted for by an
    // in-flight submission, or there's a problem.
    const gap = incoming.startVersion - expectedVersion;
    if (gap > 0) {
      // An in-flight delta can account for a gap of at most its op count.
      // Server deltas are fully processed synchronously upon receiving them,
      // and the server sends them in order with no gaps except for our own
      // submissions, so a valid gap may only occur at the front of the queue.
      if (this.transmitDelta !== null && queuePos === 0) {
        if (gap > this.transmitDelta.length) {
          throw new ChannelException(
            `Message missing! Incoming message ${incoming}` +
              ` expected version ${expectedVersion}, gap ${gap}, in-flight delta has ` +
              `${this.transmitDelta.length} ops`,
            Recoverable.NOT_RECOVERABLE,
          );
        }
      } else {
        throw new ChannelException(
          `Message missing! Incoming message ${incoming}` +
            ` expected version ${expectedVersion}, gap ${gap}, no in-flight delta`,
          Recoverable.NOT_RECOVERABLE,
        );
      }
    }
  }

  /**
   * Transmits a delta obtained from the transmitter, if the conditions are
   * right (channel is connected, no other transmission is in flight, no
   * queued messages, etc).
   */
  private tryTransmit(): void {
    if (this.state !== "CONNECTED") {
      throw new IllegalStateError(
        `Cannot send to delta channel in state ${this.state}`,
      );
    }

    if (this.queue.length !== 0) {
      // There are queued incoming messages. Don't transmit now
      // but wait until the queued messages are processed and
      // transformed with the transmitted message.
      //
      // NOTE: This condition does not starve the transmitter,
      // because the queue is only non-empty when incoming stream
      // messages and responses to delta transmissions are reordered
      // on the wire, a transient condition that can occur only as a
      // result of the last transmission response. As soon as it
      // leaves the queue, the queue remains empty until the next
      // transmission response.
      return;
    }

    if (this.deltaIsInFlight()) {
      // Serialize messages from client to server.
      return;
    }

    const delta = this.takeArgs();

    if (delta === null) {
      return; // Transmission has been cancelled.
    }

    if (this.logger.shouldLogTrace()) {
      this.logTrace(`Outgoing ${summariseDelta(delta)}`);
    }

    this.transmitDelta = delta; // Flags that an outbound message is in flight.

    const submitConnectionTag = this.connectionTag;

    /**
     * Checks whether the channel is connected and the connection tag is
     * current, i.e. the channel has not been reconnected since the delta
     * was sent.
     */
    const connectionIsCurrent = (): boolean => {
      if (this.state !== "CONNECTED") {
        this.logTrace("Ignoring orphaned ack on disconnected channel");
        return false;
      }
      if (this.connectionTag !== submitConnectionTag) {
        // Ignore, the channel has been reset since this was sent.
        this.logTrace(
          "Ignoring ophaned ack on with connection tag ",
          submitConnectionTag,
          ", connectionTag now ",
          this.connectionTag,
        );
        return false;
      }
      return true;
    };

    this.channel.submit(delta, {
      onSuccess: (
        opsApplied: number,
        newVersion: HashedVersion,
        responseCode: ResponseCode,
        errorMessage: string | null,
      ): void => {
        if (connectionIsCurrent()) {
          if (opsApplied < 0) {
            throw new ChannelException(
              `Delta channel: invalid submit delta response, opsApplied: ${opsApplied}`,
              Recoverable.NOT_RECOVERABLE,
            );
          }

          // An error message with an OK code is an error all the same.
          if (errorMessage !== null && responseCode === ResponseCode.OK) {
            responseCode = ResponseCode.INTERNAL_ERROR;
          }

          if (opsApplied > 0 || responseCode === ResponseCode.OK) {
            // It is not necessarily the case that
            // opsApplied == delta.getOpListSize() since ops may disappear in
            // transform. Zero ops applied is a valid ack when the server
            // transforms all operations away.
            this.lastAckedVersion = newVersion;
            this.onServerMessage(new Ack(opsApplied, newVersion));
          }

          if (responseCode === ResponseCode.TOO_OLD) {
            throw new ChannelException(
              ResponseCode.TOO_OLD,
              "Delta targeted too old version",
              null,
              Recoverable.RECOVERABLE,
              null,
              null,
            );
          } else if (responseCode !== ResponseCode.OK) {
            // Using lastServerVersion when opsApplied is 0 is a harmless cop
            // out to deal with the fact that the view server access control
            // responds with a bogus version and 0 opsApplied when it rejects
            // a delta.
            this.onServerMessage(
              new Nack(
                opsApplied > 0 ? newVersion.version : this.lastServerVersion,
                responseCode,
                errorMessage,
              ),
            );
          }

          this.lastTransmitDelta = this.transmitDelta;
          this.transmitDelta = null; // Enables flushServerMessages() to transmit.
          this.flushServerMessages();

          // Check the ack didn't leave a gap in the queue.
          const head = this.queue[0];
          if (
            head !== undefined &&
            head.startVersion !== newVersion.version - opsApplied
          ) {
            throw new ChannelException(
              "Delta channel couldn't flush messages after submit response: lastServerVersion " +
                `${this.lastServerVersion}, queued message version ${head.startVersion}` +
                ` response version ${newVersion}, opsApplied ${opsApplied}, code ` +
                `${responseCode}, errorMessage ${errorMessage}`,
              Recoverable.NOT_RECOVERABLE,
            );
          }
        }
      },

      onFailure: (reason: string): void => {
        if (connectionIsCurrent()) {
          throw new ChannelException(
            `Delta channel: submission failed: ${reason}`,
            Recoverable.RECOVERABLE,
          );
        }
      },
    });
  }

  /**
   * Invokes the transmitter to get a delta. Returns null to abort
   * transmission.
   */
  private takeArgs(): WaveletDelta | null {
    if (this.transmitter === null) {
      return null; // Transmission has been cancelled.
    }

    const message = this.transmitter.takeMessage();
    this.transmitter = null; // Transmitter is one-shot, will not be used again.
    return message.getDelta();
  }

  /** Checks whether a delta submission is pending response. */
  private deltaIsInFlight(): boolean {
    return this.transmitDelta !== null;
  }

  /** Resets this channel, ready to receive another connection message. */
  private internalReset(): void {
    this.logTrace("Delta channel reset");
    this.state = "INITIAL";
    this.transmitter = null;
    this.transmitDelta = null;
    this.lastServerVersion = NO_VERSION;
    this.queue.length = 0;
  }

  toString(): string {
    // Space before \n in case some logger swallows the newline.
    return (
      `Delta Channel State = [state:${this.state}] [connectionTag:${this.connectionTag}] \n` +
      `[transmitDelta:${summariseDelta(this.transmitDelta)}] \n` +
      `[lastServerVersion:${this.lastServerVersion}] ` +
      `[lastTransmitDelta:${summariseDelta(this.lastTransmitDelta)}] \n` +
      `[lastAckedVersion: ${this.lastAckedVersion}] \n` +
      `[queue (${this.queue.length} msgs):[${this.queue.join(", ")}]]`
    );
  }

  /** Logs the ops from a delta message. */
  private logDelta(prefix: string, delta: TransformedWaveletDelta): void {
    if (this.logger.shouldLogTrace()) {
      this.logTrace(prefix + summariseTransformedDelta(delta));
    }
  }

  /**
   * Logs a trace message, evaluating and concatenating components only if
   * trace is enabled.
   */
  private logTrace(...components: unknown[]): void {
    if (this.logger.shouldLogTrace()) {
      this.logger.trace(components.map((c) => String(c)).join(""));
    }
  }

  debugGetProfilingInfo(): string {
    return `${this.channel.debugGetProfilingInfo()}\n ====== Delta Channel Info ====== \n${this} \n`;
  }
}
