// Simple client mock that stores all the calls. Port of the Apache Wave test
// class `ClientMock` (Apache License, Version 2.0).

import type { ParticipantId } from "../../../model/id";
import { WaveletOperation, WaveletOperationContext } from "../../../model/operation";
import type { HashedVersion } from "../../../model/version";
import { isOperationException } from "../../common";
import type { ConcurrencyControl, ConnectionListener } from "..";
import type { TestDoc } from "./helpers";
import type { ServerConnectionMock } from "./serverConnectionMock";

export class ClientMock implements ConnectionListener {
  private numOperationsReceived = 0;
  private readonly serverOperations: WaveletOperation[] = [];
  private clientOperations: WaveletOperation[] = [];

  /**
   * Given a doc, this mock applies the client ops when they are added and
   * the server ops on receive. Client ops are compared via context, so a
   * participant identifies the client ops.
   */
  constructor(
    private readonly cc: ConcurrencyControl,
    private readonly doc: TestDoc | null = null,
    private readonly participantId: ParticipantId | null = null,
    private readonly connection: ServerConnectionMock | null = null,
  ) {}

  private onServerOperation(operation: WaveletOperation): void {
    this.serverOperations.push(operation);
    try {
      if (this.doc !== null) {
        this.applyOperation(operation);
      }
    } catch (e) {
      if (isOperationException(e)) {
        throw new Error("OperationException", { cause: e });
      }
      throw e;
    }
  }

  /** Gets the operations from concurrency control. */
  receiveServerOperations(): void {
    for (let op = this.cc.receive(); op !== null; op = this.cc.receive()) {
      this.onServerOperation(op);
    }
  }

  /** Gets the reconnection versions from concurrency control. */
  getReconnectionVersions(): HashedVersion[] {
    return this.cc.getReconnectionVersions();
  }

  /** The operations received from concurrency control (the live list). */
  getServerOperations(): WaveletOperation[] {
    return this.serverOperations;
  }

  clearServerOperations(): void {
    this.serverOperations.length = 0;
  }

  /** Applies an insertion to the document. */
  doInsert(offset: number, chars: string): void {
    const op = this.requireDoc().insertCharacters(offset, chars);
    this.clientOperations.push(
      WaveletOperation.blipContent(
        new WaveletOperationContext(this.getParticipantId(), 0, 1),
        "blip id",
        op,
      ),
    );
  }

  /**
   * Pretend a client did the given op, so that when we call flush, the
   * operation will be ejected.
   */
  addClientOperation(operation: WaveletOperation): void {
    this.clientOperations.push(operation);
    if (this.doc !== null) {
      this.applyOperation(operation);
    }
  }

  /** Applies the given operation to the doc. */
  private applyOperation(operation: WaveletOperation): void {
    const contentOp = operation.contentOp;
    if (contentOp !== null) {
      this.requireDoc().consume(contentOp);
    }
  }

  /** Flushes the client's operations to cc. */
  flush(): void {
    const ops = this.clientOperations;
    this.cc.onClientOperations(ops);
    this.clientOperations = [];
  }

  getDoc(): TestDoc | null {
    return this.doc;
  }

  requireDoc(): TestDoc {
    if (this.doc === null) {
      throw new Error("client mock has no doc");
    }
    return this.doc;
  }

  getParticipantId(): ParticipantId {
    if (this.participantId === null) {
      throw new Error("client mock has no participant");
    }
    return this.participantId;
  }

  getConnection(): ServerConnectionMock {
    if (this.connection === null) {
      throw new Error("client mock has no connection");
    }
    return this.connection;
  }

  onOperationReceived(): void {
    this.numOperationsReceived++;
  }

  /** The count of events from cc that we have received. */
  getNumOpsReceived(): number {
    return this.numOperationsReceived;
  }

  /** Resets the number of ops received. */
  clearEvents(): void {
    this.numOperationsReceived = 0;
  }
}
