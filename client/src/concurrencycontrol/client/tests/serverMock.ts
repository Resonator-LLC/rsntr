// A server for us to pretend it did things. Port of the Apache Wave test
// class `ServerMock` (Apache License, Version 2.0).

import { TransformedWaveletDelta, type WaveletDelta } from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import type { ConcurrencyControlCore, SimpleDeltaHistory } from "../../server";
import type { ServerConnectionMock } from "./serverConnectionMock";

/** An entry of a delta received from the client. */
interface ReceiveCache {
  readonly connection: ServerConnectionMock | null;
  readonly delta: WaveletDelta;
}

/** Generates some predictable signature. */
function generateSignature(transformedClientDelta: WaveletDelta): HashedVersion {
  const version = transformedClientDelta.resultingVersion;
  return HashedVersion.of(version, new Uint8Array([version & 0xff]));
}

export class ServerMock {
  /** Connections to clients. */
  private readonly connectionMocks: ServerConnectionMock[] = [];

  /** A cache of all the deltas received from the clients. */
  private clientDeltas: ReceiveCache[] = [];

  constructor(
    private readonly ccCore: ConcurrencyControlCore,
    /** History that we need to update. */
    private readonly history: SimpleDeltaHistory,
  ) {}

  /** Receives a delta from a client; it is processed by {@link start}. */
  receive(connection: ServerConnectionMock | null, delta: WaveletDelta): void {
    this.clientDeltas.push({ connection, delta });
  }

  /**
   * Processes non stop until all client deltas have been processed: a simple
   * server that transforms each delta and publishes the changes to all the
   * other clients.
   */
  start(): void {
    // Use an index instead of an iterator because the clients can send more
    // data to the server in the triggerXXX calls.
    for (let i = 0; i < this.clientDeltas.length; i++) {
      const cache = this.clientDeltas[i] as ReceiveCache;
      const connection = cache.connection;
      const initialClientOpsSize = cache.delta.length;
      // Assume that they are working only on 1 wave.
      const transformedClientDelta = this.ccCore.onClientDelta(cache.delta);
      const endVersion = generateSignature(transformedClientDelta);
      const serverDelta = TransformedWaveletDelta.fromDelta(endVersion, 0, transformedClientDelta);

      // Update the version of the wave.
      this.history.addDelta(serverDelta);
      this.history.setCurrentSignature(endVersion);

      // Tell all other clients of the delta.
      for (const conn of [...this.connectionMocks]) {
        if (conn !== connection) {
          conn.triggerServerDelta(serverDelta);
        } else {
          conn.triggerServerSuccess(initialClientOpsSize, serverDelta.resultingVersion);
        }
      }
    }
    this.clientDeltas = [];
  }

  addClientConnection(connection: ServerConnectionMock): void {
    this.connectionMocks.push(connection);
  }

  removeClientConnection(connection: ServerConnectionMock): void {
    const i = this.connectionMocks.indexOf(connection);
    if (i >= 0) {
      this.connectionMocks.splice(i, 1);
    }
  }

  /** Reopens a connection to the client. Throws `ChannelException`. */
  reOpen(clientConnection: ServerConnectionMock, clientKnownDeltas: readonly HashedVersion[]): void {
    const toClient = this.ccCore.reopen(clientKnownDeltas);
    // We have none of the client's signatures. Then return the most recent.
    if (toClient === null) {
      const startSignature = this.history.getCurrentSignature();
      clientConnection.triggerOnOpen(startSignature, startSignature);
    } else {
      const deltas = toClient.deltas;
      const startSignature = toClient.startSignature;
      const last = deltas[deltas.length - 1];
      const endSignature = last !== undefined ? last.resultingVersion : startSignature;
      clientConnection.triggerOnOpen(startSignature, endSignature);
      clientConnection.triggerServerDeltas(deltas);
    }
  }

  /**
   * Reboots the server and makes it wake up with only deltas that end before
   * or at the given version.
   */
  reboot(version: number): void {
    this.history.truncateAt(version);
  }

  getCcCore(): ConcurrencyControlCore {
    return this.ccCore;
  }

  getHistory(): SimpleDeltaHistory {
    return this.history;
  }
}
