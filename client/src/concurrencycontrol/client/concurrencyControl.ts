// The client side concurrency control for one wavelet.
//
// Keeps at most one unacknowledged delta in flight, queues further client
// operations, transforms the queued and in-flight operations against
// incoming server deltas, and remembers the acknowledged deltas (the
// inferred server path) so that it can recover after a reconnection.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.client.ConcurrencyControl`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import {
  DeltaPair,
  type TransformedWaveletDelta,
  WaveletDelta,
  type WaveletOperation,
  invertDeltas,
} from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import {
  ChannelException,
  type Logger,
  NO_LOGGER,
  OperationException,
  Recoverable,
  TransformException,
  type UnsavedDataInfo,
  type UnsavedDataListener,
  IllegalStateError,
  checkState,
} from "../common";
import { OperationQueue } from "./operationQueue";
import type { ServerConnection } from "./serverConnection";
import type { ServerConnectionListener } from "./serverConnectionListener";

/** The size and result version of an acknowledged delta. */
class AckInfo {
  constructor(
    /** Number of ops acknowledged. */
    readonly numOps: number,
    /** Version of the wavelet after the acknowledged ops. */
    readonly ackedVersion: HashedVersion,
  ) {}
}

/** A client delta with the server acknowledgement. */
class AckedDelta {
  constructor(
    /** The delta sent to the server. */
    readonly delta: WaveletDelta,
    /** The server's acknowledgement. */
    readonly ack: AckInfo,
  ) {}

  toString(): string {
    return `AckedDelta(${this.delta}, acked at ${this.ack.ackedVersion})`;
  }
}

/** A listener for server operations (`ConcurrencyControl.ConnectionListener`). */
export interface ConnectionListener {
  /** Called when a server operation is received. */
  onOperationReceived(): void;
}

export class ConcurrencyControl implements ServerConnectionListener {
  private readonly unsavedDataInfo: UnsavedDataInfo = {
    inFlightSize: () =>
      this.unacknowledged !== null ? this.unacknowledged.length : 0,
    estimateUnacknowledgedSize: () =>
      this.clientOperationQueue.estimateSize() +
      this.unsavedDataInfo.inFlightSize(),
    estimateUncommittedSize: () => {
      let ackedButUncommitted = 0;
      for (const ack of this.acks) {
        ackedButUncommitted += ack.numOps;
      }
      return (
        ackedButUncommitted + this.unsavedDataInfo.estimateUnacknowledgedSize()
      );
    },
    lastAckVersion: () => {
      const last = this.acks[this.acks.length - 1];
      if (last !== undefined) {
        return last.ackedVersion.version;
      }
      return this.lastCommitVersion;
    },
    lastCommitVersion: () => this.lastCommitVersion,
    getInfo: () =>
      `${this.serverConnection === null ? "" : this.serverConnection.debugGetProfilingInfo()}` +
      `\n ====== CC Info ====== \n${this}`,
  };

  private readonly logger: Logger;

  /** The hash from the server before any of the inferredServerPath. */
  private startSignature: HashedVersion;

  /**
   * Marks the end of the initial sequence of deltas the server sends to the
   * client when we open a connection to the server.
   */
  private endOfStartingDelta: HashedVersion | null = null;

  /**
   * The server's path inferred by the client. It contains deltas that were
   * sent by the client and acked by the server. The list is cleared when any
   * server delta is received since we can't currently recover past a server
   * delta.
   */
  private inferredServerPath: AckedDelta[] = [];

  /**
   * Acknowledgments received for deltas not yet committed, in contiguous
   * version order. Unlike {@link inferredServerPath} items are not cleared
   * when server deltas are received, but when commits are received.
   */
  private acks: AckInfo[] = [];

  /** Latest committed version. */
  private lastCommitVersion = 0;

  /**
   * The delta that is not yet acked by the server. Any new operations coming
   * from the client are queued in other deltas after this one. If null,
   * nothing is in flight to the server.
   */
  private unacknowledged: WaveletDelta | null = null;

  /** Queue of operations from the client not yet sent to the server. */
  private readonly clientOperationQueue = new OperationQueue();

  /**
   * The operations that have been received from the server and have been
   * transformed against any relevant client operations but have not yet been
   * received by the wave client, which grabs them whenever it's ready.
   */
  private serverOperations: WaveletOperation[] = [];

  /** A listener for this class to broadcast its transformed server ops. */
  private clientListener: ConnectionListener | null = null;

  /** The connection to the server where we can send client operations. */
  private serverConnection: ServerConnection | null = null;

  /**
   * Pauses sending to the server. This is needed when we tell the client to
   * flush its operations and we want to send to the server when all the
   * operations are flushed.
   */
  private pauseSendForFlush = false;

  /** Listener for unsaved data info. */
  private unsavedDataListener: UnsavedDataListener | null = null;

  /**
   * Constructs a client side concurrency control module. It must be
   * {@link ConcurrencyControl.initialise initialised} before use.
   *
   * @param logger a logger to use for trace output (null for none)
   * @param startSignature the version/hash at which to begin connecting
   */
  constructor(
    logger: Logger | null | undefined,
    startSignature: HashedVersion,
  ) {
    this.logger = logger ?? NO_LOGGER;
    this.startSignature = startSignature;
  }

  /**
   * Initialises the server connection and client listener for this
   * connection. This method must be invoked exactly once.
   *
   * @param serverConnection connection to which to send deltas
   * @param clientListener listener for inbound deltas
   */
  initialise(
    serverConnection: ServerConnection,
    clientListener: ConnectionListener,
  ): void {
    this.clientListener = clientListener;
    this.serverConnection = serverConnection;
  }

  /** Sets the listener for unsaved data info. */
  setUnsavedDataListener(udl: UnsavedDataListener | null): void {
    this.unsavedDataListener = udl;
  }

  /** Closes this concurrency control. */
  close(): void {
    if (this.unsavedDataListener !== null) {
      this.unsavedDataListener.onClose(this.everythingIsCommitted());
    }
    if (!this.clientOperationQueue.isEmpty()) {
      this.logger.error(
        "Concurrency control closed with pending operations. Data has been lost",
      );
    }
  }

  /** Throws {@link ChannelException}. */
  onOpen(connectVersion: HashedVersion, currentVersion: HashedVersion): void {
    if (
      connectVersion === null ||
      connectVersion === undefined ||
      currentVersion === null ||
      currentVersion === undefined ||
      connectVersion.version < 0 ||
      currentVersion.version < connectVersion.version
    ) {
      throw new ChannelException(
        "ConcurrencyControl onOpen received invalid versions, " +
          `connect version: ${connectVersion}, current version: ${currentVersion}`,
        Recoverable.NOT_RECOVERABLE,
      );
    }

    // Try to recover from where we were.
    // Find the point in the inferred server path to start resending pending
    // deltas to the server.
    let startResend = -1;
    if (this.startSignature.equals(connectVersion)) {
      startResend = 0;
    } else {
      const i = this.inferredServerPath.findIndex((d) =>
        connectVersion.equals(d.ack.ackedVersion),
      );
      if (i >= 0) {
        startResend = i + 1;
      }
    }

    if (startResend < 0) {
      // No matching signatures.
      throw new ChannelException(
        "Failed to recover from reconnection to server. No matching signatures. " +
          `[Received startSignature:${connectVersion} endSignature:${currentVersion}] ${this}`,
        Recoverable.NOT_RECOVERABLE,
      );
    } else if (startResend < this.inferredServerPath.length) {
      // Found a matching signature.
      this.mergeToClientQueue(startResend);
    } else if (
      startResend === this.inferredServerPath.length &&
      connectVersion.equals(currentVersion)
    ) {
      // Matched all signatures and we are also at the end of the server
      // operations. We are short circuiting, because we know that the server
      // definitely has not got our unacknowledged delta since we are at the
      // end of its signatures.
      this.mergeToClientQueue(startResend);
    } else {
      // All the signatures matched, that means we need to compare server
      // operations when we get them.
      this.logger.trace(
        "All signatures in CC queue matched on reconnection to server.",
      );
    }
    this.forgetAcksAfter(this.startSignature.version);
    this.endOfStartingDelta = currentVersion;
    this.sendDelta();
  }

  /**
   * Pushes all deltas at startMerge and after back into the client operation
   * queue, discarding their ack info.
   *
   * @param startMerge the index of the delta in the inferred server path at
   *        which to start merging into the client operation queue.
   */
  private mergeToClientQueue(startMerge: number): void {
    const deltas: WaveletDelta[] = [];
    // Use the version at the resend.
    if (startMerge < this.inferredServerPath.length) {
      for (const acked of this.inferredServerPath.splice(startMerge)) {
        deltas.push(acked.delta);
      }
    }

    if (this.unacknowledged !== null) {
      deltas.push(this.unacknowledged);
      this.unacknowledged = null;
    }

    deltas.reverse();
    for (const delta of deltas) {
      this.clientOperationQueue.insertHead(delta.ops);
    }
  }

  /** Forgets about any acknowledgments after some version. */
  private forgetAcksAfter(version: number): void {
    this.acks = this.acks.filter(
      (ack) => !(ack.ackedVersion.version > version),
    );
  }

  /**
   * Packages up transformed client operations as a delta, and sends it to
   * the server. A send does not occur if there are unacknowledged deltas.
   */
  private sendDelta(): void {
    if (!this.isReadyToSend()) {
      return;
    }

    if (this.unacknowledged !== null) {
      this.logger.trace(
        "Unacknowledged delta, expected to be applied at version ",
        this.unacknowledged.targetVersion.version,
      );
      return;
    }

    if (this.clientOperationQueue.isEmpty()) {
      this.logger.trace("Nothing to send");
      // Since the outgoing queue might have been transformed away, we need to
      // reset the estimated queue.
      this.triggerUnsavedDataListener();
      return;
    }

    // If we are sending something then we have inferred our location on the
    // server path.
    this.endOfStartingDelta = null;

    const ops = this.clientOperationQueue.take();
    const unacknowledged = new WaveletDelta(
      (ops[0] as WaveletOperation).context.creator,
      this.getLastSignature(),
      ops,
    );
    this.unacknowledged = unacknowledged;

    if (this.logger.shouldLogTrace()) {
      this.logger.trace(
        `Sending delta to server with last known server version ${unacknowledged.targetVersion}`,
        unacknowledged,
      );
    }

    this.connection().send(unacknowledged);
    this.triggerUnsavedDataListener();
  }

  /**
   * Transforms all the operations in the incoming server delta against all
   * the operations in the unacknowledged delta and the client operation
   * queue before notifying the client.
   *
   * Also keeps track of the transformed client operations so that we can
   * infer the server's OT path.
   *
   * Assumptions: the server delta applies at the version of the
   * unacknowledged delta; client ops never skip a version.
   *
   * Throws transform and operation exceptions.
   */
  private transformOperationsAndNotify(
    serverDelta: TransformedWaveletDelta,
  ): void {
    // Sanity check.
    const lastAcked =
      this.inferredServerPath[this.inferredServerPath.length - 1];
    const latestVersion =
      lastAcked !== undefined
        ? lastAcked.ack.ackedVersion.version
        : this.startSignature.version;
    if (serverDelta.appliedAtVersion < latestVersion) {
      throw new OperationException(
        "Server sent a delta containing a version that is older than " +
          `the version at end of inferred server path. [Received serverDelta${serverDelta}] ` +
          `${this}`,
      );
    }

    if (this.detectEchoBack(serverDelta)) {
      return;
    }

    // Clear inferred server path as we can not recover past a server delta.
    this.inferredServerPath = [];
    this.startSignature = serverDelta.resultingVersion;

    // Transform against any unacknowledged ops.
    let transformedServerDelta: readonly WaveletOperation[] = serverDelta.ops;

    const unacknowledged = this.unacknowledged;
    if (unacknowledged !== null) {
      if (
        serverDelta.appliedAtVersion !== unacknowledged.targetVersion.version
      ) {
        throw new TransformException(
          "Cannot accept server version newer than unacknowledged. server version:" +
            `${serverDelta.appliedAtVersion} unacknowledged version:` +
            `${unacknowledged.targetVersion}. [Received serverDelta:${serverDelta}] ${this}`,
        );
      }

      const transformedPair = new DeltaPair(
        unacknowledged.ops,
        serverDelta.ops,
      ).transform();
      // The ops of the server delta are transformed, all metadata remains.
      transformedServerDelta = transformedPair.server;
      // The unacknowledged delta must have applied after the server delta.
      this.unacknowledged = new WaveletDelta(
        unacknowledged.author,
        serverDelta.resultingVersion,
        transformedPair.client,
      );
    }

    // Transform against any queued ops.
    transformedServerDelta = this.clientOperationQueue.transform(
      transformedServerDelta,
    );

    // Notify server-operation listeners.
    for (const serverOp of transformedServerDelta) {
      this.serverOperations.push(serverOp);
    }
  }

  /**
   * Detects if the whole delta is an echo back. If it is then takes it as if
   * it was an ack. Echo back is a result of reconnection/recovery.
   */
  private detectEchoBack(serverDelta: TransformedWaveletDelta): boolean {
    // We have got all the initial list of operations. So do nothing.
    if (
      this.endOfStartingDelta === null ||
      this.endOfStartingDelta.version <= serverDelta.appliedAtVersion
    ) {
      return false;
    }

    // Check to see if we are getting a delta that was sent by us, in case the
    // server echos back our own delta from a recovery scenario.
    if (
      this.unacknowledged !== null &&
      DeltaPair.areSame(serverDelta.ops, this.unacknowledged.ops)
    ) {
      // If we completely match then take it as an ack.
      this.onSuccess(serverDelta.length, serverDelta.resultingVersion);
      return true;
    }

    // We've just got to the end of the initial list of operations and there
    // is no match. That means we need to merge the unacknowledged ops and
    // resend again.
    if (this.endOfStartingDelta.equals(serverDelta.resultingVersion)) {
      this.mergeToClientQueue(this.inferredServerPath.length);
    }
    return false;
  }

  /** Throws {@link TransformException}. */
  onSuccess(opsApplied: number, signature: HashedVersion): void {
    const unacknowledged = this.unacknowledged;
    if (unacknowledged === null) {
      // Note: An ACK will only occur before echoBack delta.
      throw new TransformException(
        `Got ACK from server, but we had not sent anything. ${this}`,
      );
    }

    if (unacknowledged.resultingVersion !== signature.version) {
      throw new TransformException(
        "Got ACK from server, but we don't have the same version. " +
          `Client expects new version ${unacknowledged.resultingVersion}` +
          ` and ${unacknowledged.length} acked ops, ` +
          ` Server acked ${opsApplied}, new version ${signature.version}. ` +
          `[Received signature:${signature}] [Received opsApplied:${opsApplied}] ${this}`,
      );
    }

    if (opsApplied !== unacknowledged.length) {
      throw new TransformException(
        "Unable to accept ACK of different number of operations than " +
          `client issued. Client sent = ${unacknowledged.length} Server acked = ` +
          `${opsApplied}. ${this}`,
      );
    }

    const lastAck = this.acks[this.acks.length - 1];
    if (lastAck !== undefined) {
      checkState(
        signature.version > lastAck.ackedVersion.version,
        `Unexpected ack for version ${signature.version} less than last acked version ` +
          `${lastAck.ackedVersion}`,
      );
    }

    // We know the server has done this now.
    if (unacknowledged.targetVersion.version < this.startSignature.version) {
      this.logger.error(
        `unexpected ack for version ${unacknowledged.targetVersion}` +
          ` before start version ${this.startSignature.version}. [Received signature:` +
          `${signature}] [Received opsApplied:${opsApplied}] ${this}`,
      );
    }

    // Remember delta as received by the server (unless it transformed away).
    const ack = new AckInfo(opsApplied, signature);
    if (unacknowledged.length > 0) {
      this.inferredServerPath.push(new AckedDelta(unacknowledged, ack));
    }
    this.acks.push(ack);

    // We now need to tell the client model about how the server applied the
    // operation by faking a server operation which contains the version
    // number.
    this.makeFakeServerOpsFromAckAndNotify(unacknowledged, signature);

    // Mark nothing in flight.
    this.unacknowledged = null;

    this.triggerUnsavedDataListener();

    // Send any pending client operations as a new delta.
    this.sendDelta();
  }

  /**
   * We have just been acked: makes fake version update operations to tell
   * the client about the server version and last modified time.
   *
   * Assumption: the server applied the ops starting at the version of the
   * unacknowledged delta.
   */
  private makeFakeServerOpsFromAckAndNotify(
    unacknowledged: WaveletDelta,
    ackedSignature: HashedVersion,
  ): void {
    // All unacknowledged ops are assumed to be acked now. Ops in unacknowledged
    // are transformed against any server ops received prior to the ack. We now
    // create a version update op for each unacknowledged op. The last one also
    // includes the acked signature.
    const count = unacknowledged.ops.length;
    const versionOps = unacknowledged.ops.map((op, i) =>
      op.createVersionUpdateOp(1, i === count - 1 ? ackedSignature : null),
    );

    // Transform against any queued ops. Note server ops even when they are
    // nullified will leave behind a version updating op, i.e. the total
    // number of server ops will never change. As in the original, the
    // transformed server ops are not used: version updates transform to
    // themselves.
    this.clientOperationQueue.transform(versionOps);

    // Notify server-operation listeners.
    for (const serverOp of versionOps) {
      this.serverOperations.push(serverOp);
      this.logger.trace("Fake version update op ", serverOp);
    }

    if (this.serverOperations.length > 0) {
      this.onOperationReceived();
    }
  }

  onServerDelta(delta: TransformedWaveletDelta): void {
    this.onServerDeltas([delta]);
  }

  onServerDeltas(deltas: readonly TransformedWaveletDelta[]): void {
    if (deltas.length === 0) {
      this.logger.error("Unexpected empty deltas.");
      return;
    }

    if (this.logger.shouldLogTrace()) {
      this.logger.trace("Server deltas received: ");
      this.logger.trace(...deltas);
    }

    for (const delta of deltas) {
      this.transformOperationsAndNotify(delta);
    }

    if (this.serverOperations.length > 0) {
      this.onOperationReceived();
    }

    // Re-send any pending client operations.
    this.sendDelta();
  }

  /**
   * The server's history was rewound to `fork` (a change of host, see the
   * server's host epochs): `rewound` are the deltas after it the server had
   * sent or acknowledged, which no longer exist, in order. Not in the
   * original.
   *
   * Their inverses become server operations, transformed against the
   * client's pending operations as any server delta is; the pending
   * operations, in flight or queued, are moved onto `fork` and go back into
   * the queue, to be sent again once the channel has reconnected at `fork`.
   * The rewound deltas themselves are the server's to replay (the host
   * resubmits orphaned deltas), so they are not resent. The caller must
   * reconnect, so that answers to a delta in flight are ignored.
   */
  onReset(
    fork: HashedVersion,
    rewound: readonly TransformedWaveletDelta[],
  ): void {
    const inverse = invertDeltas(rewound, fork);
    let transformed: readonly WaveletOperation[] = inverse;
    const unacknowledged = this.unacknowledged;
    if (unacknowledged !== null) {
      const pair = new DeltaPair(unacknowledged.ops, transformed).transform();
      transformed = pair.server;
      this.unacknowledged = new WaveletDelta(
        unacknowledged.author,
        fork,
        pair.client,
      );
    }
    transformed = this.clientOperationQueue.transform(transformed);
    for (const op of transformed) {
      this.serverOperations.push(op);
    }
    this.inferredServerPath = [];
    this.startSignature = fork;
    this.endOfStartingDelta = null;
    this.forgetAcksAfter(fork.version);
    this.lastCommitVersion = Math.min(this.lastCommitVersion, fork.version);
    this.mergeToClientQueue(0);
    if (this.serverOperations.length > 0) {
      this.onOperationReceived();
    }
    this.triggerUnsavedDataListener();
  }

  /**
   * The delta in flight, as it stands now: transformed against the server
   * deltas received since it was handed to the server connection, and
   * targeting the last of them. Null if nothing is in flight.
   *
   * Not in the original. See `OperationChannelImpl`: a delta the delta
   * channel holds back while it works through queued server messages must
   * be sent in this form, not as it was when `send` was called.
   */
  getUnacknowledgedDelta(): WaveletDelta | null {
    return this.unacknowledged;
  }

  /**
   * Returns a list of signature information providing versions on the
   * inferred server path, suitable for reconnection.
   *
   * The start signature is always a reconnection version, even if it's zero
   * and we never even sent an op to the server, so that the server sends us
   * ops rather than a clobbering snapshot.
   */
  getReconnectionVersions(): HashedVersion[] {
    const signatures = [this.startSignature];
    for (const d of this.inferredServerPath) {
      signatures.push(d.ack.ackedVersion);
    }
    return signatures;
  }

  /**
   * Whether the buffered client operations can and should be sent to the
   * server as a delta.
   */
  private isReadyToSend(): boolean {
    const connection = this.connection();
    const ready = !this.pauseSendForFlush && connection.isOpen();
    if (!ready && this.logger.shouldLogTrace()) {
      this.logger.trace(
        "Not ready to send, pauseSendForFlush is ",
        this.pauseSendForFlush,
        " server connection ",
        connection.isOpen() ? "IS" : "is NOT",
        " open",
      );
    }
    return ready;
  }

  /**
   * Queues the client operations, and sends them to the server as a delta at
   * the first opportunity. Calls any registered unsaved data listener before
   * returning.
   *
   * @param operations the operations to send, all of which must specify a
   *        creator
   */
  onClientOperations(operations: readonly WaveletOperation[]): void {
    const transformedPair = new DeltaPair(
      operations,
      this.serverOperations,
    ).transform();
    this.serverOperations = [...transformedPair.server];

    for (const o of transformedPair.client) {
      this.clientOperationQueue.add(o);
    }
    this.triggerUnsavedDataListener();
    this.sendDelta();
  }

  onCommit(committedVersion: number): void {
    // Remove old cache.
    for (;;) {
      const d = this.inferredServerPath[0];
      if (d !== undefined && d.delta.resultingVersion <= committedVersion) {
        this.startSignature = d.ack.ackedVersion;
        this.inferredServerPath.shift();
      } else {
        break;
      }
    }

    // Remove from acked-but-uncommitted.
    for (;;) {
      const ack = this.acks[0];
      if (ack !== undefined && ack.ackedVersion.version <= committedVersion) {
        this.acks.shift();
      } else {
        break;
      }
    }

    this.lastCommitVersion = committedVersion;
    this.logger.trace(
      "onCommit: version =",
      committedVersion,
      " serverpathsize =",
      this.inferredServerPath.length,
      " any unacknowledged ? ",
      this.unacknowledged === null,
    );
    this.triggerUnsavedDataListener();
  }

  private onOperationReceived(): void {
    const oldPauseValue = this.pauseSendForFlush;
    // Pause sending first as we want to gather all the client ops in case
    // the client does multiple operations.
    this.pauseSendForFlush = true;
    if (this.clientListener !== null) {
      this.clientListener.onOperationReceived();
    }
    this.pauseSendForFlush = oldPauseValue;
  }

  /**
   * Receives a (transformed) server operation, if any is available: the next
   * operation received from the server that made it through transformation,
   * else null.
   */
  receive(): WaveletOperation | null {
    const op = this.serverOperations.shift();
    if (op === undefined) {
      return null;
    }
    this.logger.trace("Processing server op ", op);
    return op;
  }

  /**
   * Peeks at the next (transformed) server operation, if any is available;
   * else null.
   */
  peek(): WaveletOperation | null {
    return this.serverOperations[0] ?? null;
  }

  /** The last received signature. */
  private getLastSignature(): HashedVersion {
    const last = this.inferredServerPath[this.inferredServerPath.length - 1];
    return last === undefined ? this.startSignature : last.ack.ackedVersion;
  }

  /** True if nothing is in flight or uncommitted. */
  private everythingIsCommitted(): boolean {
    return this.acks.length === 0 && this.unacknowledged === null;
  }

  private triggerUnsavedDataListener(): void {
    if (this.unsavedDataListener !== null) {
      this.unsavedDataListener.onUpdate(this.unsavedDataInfo);
    }
  }

  private connection(): ServerConnection {
    if (this.serverConnection === null) {
      throw new IllegalStateError(
        "ConcurrencyControl used before initialise()",
      );
    }
    return this.serverConnection;
  }

  toString(): string {
    // Space before \n in case some logger swallows the newline.
    return (
      "Client CC State = " +
      `[startSignature:${this.startSignature}] \n` +
      `[endOfStartingDelta:${this.endOfStartingDelta}] \n` +
      `[lastCommittedVersion: ${this.lastCommitVersion}] \n` +
      `[inferredServerPath:[${this.inferredServerPath.join(", ")}]] \n` +
      `[unacknowledged:${this.unacknowledged}] \n` +
      `[clientOperationQueue:${this.clientOperationQueue}] \n` +
      `[serverOperations:[${this.serverOperations.join(", ")}]] \n`
    );
  }
}
