// Collaboration-aware undo: undoes only the operations registered as
// undoable, transformed past everything that happened since (the
// operations of other participants and local ones that must not be undone).
//
// A port of `org.waveprotocol.wave.model.undo.UndoManagerImpl` and
// `UndoStack` of Apache Wave (Apache License 2.0), specialised to `DocOp`.

import { type DocOp, compose, invert, transform } from "../../model/docop";

interface StackEntry {
  readonly op: DocOp;
  nonUndoables: DocOp[];
}

function composeList(ops: readonly DocOp[]): DocOp {
  let result = ops[0]!;
  for (let i = 1; i < ops.length; i++) {
    result = compose(result, ops[i]!);
  }
  return result;
}

/** `UndoStack`. */
class UndoStack {
  private stack: StackEntry[] = [];

  push(op: DocOp): void {
    this.stack.push({ op, nonUndoables: [] });
  }

  /**
   * Pops an operation; returns the operation that effects the undo and the
   * non-undoable operations transformed over it (null if there were none).
   */
  pop(): [DocOp, DocOp | null] | null {
    const entry = this.stack.pop();
    if (entry === undefined) {
      return null;
    }
    const op = invert(entry.op);
    if (entry.nonUndoables.length === 0) {
      return [op, null];
    }
    const pair = transform(op, composeList(entry.nonUndoables));
    const next = this.stack[this.stack.length - 1];
    if (next !== undefined) {
      next.nonUndoables.push(pair.serverOp);
    }
    return [pair.clientOp, pair.serverOp];
  }

  nonUndoableOperation(op: DocOp): void {
    this.stack[this.stack.length - 1]?.nonUndoables.push(op);
  }

  clear(): void {
    this.stack = [];
  }

  isEmpty(): boolean {
    return this.stack.length === 0;
  }
}

/** `UndoManagerImpl.Checkpointer`. */
class Checkpointer {
  private partitions: number[] = [];
  private lastPartition = 0;

  checkpoint(): void {
    if (this.lastPartition > 0) {
      this.partitions.push(this.lastPartition);
      this.lastPartition = 0;
    }
  }

  releaseCheckpoint(): number {
    if (this.lastPartition > 0) {
      const value = this.lastPartition;
      this.lastPartition = 0;
      return value;
    }
    return this.partitions.pop() ?? 0;
  }

  increment(): void {
    this.lastPartition++;
  }

  clear(): void {
    this.partitions = [];
    this.lastPartition = 0;
  }
}

/** `UndoManagerImpl<DocOp>`. */
export class UndoManager {
  private readonly undoStack = new UndoStack();
  private readonly redoStack = new UndoStack();
  private readonly checkpointer = new Checkpointer();

  /** Registers an operation that can be undone. Clears the redo stack. */
  undoableOp(op: DocOp): void {
    this.undoStack.push(op);
    this.checkpointer.increment();
    this.redoStack.clear();
  }

  /** Registers an operation that happened but is not to be undone. */
  nonUndoableOp(op: DocOp): void {
    this.undoStack.nonUndoableOperation(op);
    this.redoStack.nonUndoableOperation(op);
  }

  /** The undoable operations since the last checkpoint are one undo step. */
  checkpoint(): void {
    this.checkpointer.checkpoint();
  }

  /** The operation that undoes the last step, or null. */
  undo(): DocOp | null {
    return this.undoPlus()?.[0] ?? null;
  }

  redo(): DocOp | null {
    return this.redoPlus()?.[0] ?? null;
  }

  /**
   * The undo operation and the non-undoable operations since the undone
   * step, transformed over the undo (null if none).
   */
  undoPlus(): [DocOp, DocOp | null] | null {
    const numToUndo = this.checkpointer.releaseCheckpoint();
    if (numToUndo === 0) {
      return null;
    }
    const operations: DocOp[] = [];
    let last: [DocOp, DocOp | null] | null = null;
    for (let i = 0; i < numToUndo; i++) {
      last = this.undoStack.pop();
      if (last === null) {
        break;
      }
      operations.push(last[0]);
    }
    if (operations.length === 0 || last === null) {
      return null;
    }
    const op = composeList(operations);
    this.redoStack.push(op);
    return [op, last[1]];
  }

  redoPlus(): [DocOp, DocOp | null] | null {
    const ops = this.redoStack.pop();
    if (ops !== null) {
      this.checkpointer.checkpoint();
      this.undoStack.push(ops[0]);
      this.checkpointer.increment();
    }
    return ops;
  }

  canUndo(): boolean {
    return !this.undoStack.isEmpty();
  }

  canRedo(): boolean {
    return !this.redoStack.isEmpty();
  }

  clear(): void {
    this.undoStack.clear();
    this.redoStack.clear();
    this.checkpointer.clear();
  }
}
