// Mocks a server connection on which we can pretend things were received on
// the wire. Port of the Apache Wave test class `ServerConnectionMock`
// (Apache License, Version 2.0).

import type { TransformedWaveletDelta, WaveletDelta } from "../../../model/operation";
import type { HashedVersion } from "../../../model/version";
import { isOperationException, isTransformException } from "../../common";
import type { ServerConnection, ServerConnectionListener } from "..";
import type { ServerMock } from "./serverMock";

export class ServerConnectionMock implements ServerConnection {
  private listener: ServerConnectionListener | null = null;
  private serverMock: ServerMock | null = null;
  private ghostDeltas: WaveletDelta[] = [];
  private readonly sentDeltas: WaveletDelta[] = [];
  private readonly receivedDeltas: TransformedWaveletDelta[] = [];
  private readonly reconnectSignatures: HashedVersion[] = [];
  private open = true;
  /** If all the deltas to be sent should be turned into ghost deltas. */
  private ghostSend = false;

  /** Sends data to the server mock if we have one. */
  send(delta: WaveletDelta): void {
    if (this.ghostSend) {
      this.ghostDeltas.push(delta);
    } else {
      this.sentDeltas.push(delta);
      if (this.serverMock !== null) {
        this.serverMock.receive(this, delta);
      }
    }
  }

  /** If true, all deltas in send() are turned into ghosts, i.e. not sent to the server. */
  setGhostSend(ghostSend: boolean): void {
    this.ghostSend = ghostSend;
  }

  /** Sends all the ghost deltas to the server. */
  sendGhosts(): void {
    for (const delta of this.ghostDeltas) {
      if (this.serverMock !== null) {
        // No call back for ack.
        this.serverMock.receive(null, delta);
      }
    }
    this.ghostDeltas = [];
  }

  /** Pretend we got the delta on the wire. */
  triggerServerDelta(delta: TransformedWaveletDelta): void {
    this.receivedDeltas.push(delta);
    if (this.listener !== null) {
      this.listener.onServerDelta(delta);
    }
  }

  /** Pretend we got the deltas on the wire. */
  triggerServerDeltas(deltas: readonly TransformedWaveletDelta[]): void {
    this.receivedDeltas.push(...deltas);
    if (this.listener !== null) {
      this.listener.onServerDeltas([...deltas]);
    }
  }

  /** Triggers an ack. */
  triggerServerSuccess(numDeltasApplied: number, signature: HashedVersion): void {
    if (this.listener !== null) {
      this.listener.onSuccess(numDeltasApplied, signature);
    }
  }

  /** Triggers a commit. */
  triggerServerCommit(version: number): void {
    if (this.listener !== null) {
      this.listener.onCommit(version);
    }
  }

  isOpen(): boolean {
    return this.open;
  }

  /** Sets what {@link isOpen} should return. */
  setOpen(open: boolean): void {
    this.open = open;
  }

  /** Throws `ChannelException`. */
  reconnect(signatures: readonly HashedVersion[]): void {
    this.reconnectSignatures.push(...signatures);
    if (this.serverMock !== null) {
      try {
        this.serverMock.reOpen(this, signatures);
      } catch (e) {
        if (isTransformException(e) || isOperationException(e)) {
          throw new Error(`reconnect failed: ${e.message}`, { cause: e });
        }
        throw e;
      }
    }
  }

  getListener(): ServerConnectionListener | null {
    return this.listener;
  }

  setListener(listener: ServerConnectionListener | null): void {
    this.listener = listener;
  }

  getServerMock(): ServerMock | null {
    return this.serverMock;
  }

  setServerMock(serverMock: ServerMock | null): void {
    this.serverMock = serverMock;
  }

  /** The live list: tests clear it. */
  getSentDeltas(): WaveletDelta[] {
    return this.sentDeltas;
  }

  /** The live list: tests clear it. */
  getReceivedDeltas(): TransformedWaveletDelta[] {
    return this.receivedDeltas;
  }

  /** The live list: tests clear it. */
  getReconnectSignatures(): HashedVersion[] {
    return this.reconnectSignatures;
  }

  /**
   * Pretend the server opened the connection at the given startSignature and
   * tells of the last signature.
   */
  triggerOnOpen(startSignature: HashedVersion, endSignature: HashedVersion): void {
    if (this.listener === null) {
      throw new Error("no listener");
    }
    this.listener.onOpen(startSignature, endSignature);
  }

  debugGetProfilingInfo(): string {
    return "";
  }
}
