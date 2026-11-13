// A sequence of operations that composes adjacent operations when they can
// be safely merged together into a single operation.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.client.MergingSequence`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import { type DocOp, composeAll } from "../../model/docop";
import { WaveletOperation, type WaveletOperationContext } from "../../model/operation";

export class MergingSequence implements Iterable<WaveletOperation> {
  private ops: WaveletOperation[] = [];

  /**
   * An empty sequence, or one with the given operations. Does not copy any
   * version or signature information.
   */
  constructor(ops?: Iterable<WaveletOperation>) {
    if (ops !== undefined) {
      this.addAll(ops);
    }
  }

  /**
   * Composes runs of adjacent content operations on the same document into
   * one content operation with the context of the last operation of the
   * run. The creators are not looked at: the caller keeps sequences to a
   * single creator. As in the original, the composed operation gets the
   * default contributor method.
   *
   * Throws the docop `OperationError` if the document operations do not
   * compose.
   */
  optimise(): void {
    const startSize = this.size();
    if (startSize === 1) {
      return;
    }

    let docOps: DocOp[] = [];
    const oldOperations = this.ops;
    let currentId: string | null = null;
    let lastOperationContext: WaveletOperationContext | null = null;
    const result: WaveletOperation[] = [];
    this.ops = result;

    const composeDocOps = (): void => {
      const composed = composeAll(docOps);
      if (composed !== null && currentId !== null && lastOperationContext !== null) {
        result.push(WaveletOperation.blipContent(lastOperationContext, currentId, composed));
      }
      docOps = [];
    };

    for (const waveletOp of oldOperations) {
      const kind = waveletOp.kind;
      if (kind.type === "blip" && kind.op.kind.type === "content") {
        const id = kind.blipId;
        if (docOps.length > 0 && id !== currentId) {
          composeDocOps();
        }
        docOps.push(kind.op.kind.op);
        lastOperationContext = waveletOp.context;
        currentId = id;
        continue;
      }
      if (docOps.length > 0) {
        composeDocOps();
      }
      result.push(waveletOp);
    }
    if (docOps.length > 0) {
      composeDocOps();
    }
  }

  /** Throws `RangeError` if there is no operation at `index`. */
  get(index: number): WaveletOperation {
    const op = this.ops[index];
    if (op === undefined) {
      throw new RangeError(`Index: ${index}, Size: ${this.ops.length}`);
    }
    return op;
  }

  size(): number {
    return this.ops.length;
  }

  get length(): number {
    return this.ops.length;
  }

  isEmpty(): boolean {
    return this.ops.length === 0;
  }

  add(op: WaveletOperation): void {
    this.ops.push(op);
  }

  addAll(ops: Iterable<WaveletOperation>): void {
    // Copy first: `ops` may be this sequence.
    for (const op of [...ops]) {
      this.add(op);
    }
  }

  /** Removes and returns the operation at `index`. */
  remove(index: number): WaveletOperation {
    const op = this.get(index);
    this.ops.splice(index, 1);
    return op;
  }

  clear(): void {
    this.ops = [];
  }

  /** A copy of the operations. */
  toArray(): WaveletOperation[] {
    return [...this.ops];
  }

  [Symbol.iterator](): IterableIterator<WaveletOperation> {
    return this.ops[Symbol.iterator]();
  }

  toString(): string {
    return `[${this.ops.join(", ")}]`;
  }
}
