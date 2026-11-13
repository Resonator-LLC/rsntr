// Deltas: lists of wavelet operations by one author.
//
// Derived from `org.waveprotocol.wave.model.operation.wave` (`WaveletDelta`,
// `TransformedWaveletDelta`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { ParticipantId } from "../id";
import type { HashedVersion } from "../version";
import { WaveletOperationContext } from "./context";
import { DeltaError, WaveletOperationError } from "./error";
import type { WaveletOperation } from "./waveOp";

function opsEqual(a: readonly WaveletOperation[], b: readonly WaveletOperation[]): boolean {
  return a.length === b.length && a.every((op, i) => op.equals(b[i]!));
}

/**
 * Operations an author wants applied to a wavelet at a target version, as
 * submitted by a client. Immutable; iterates over its operations.
 *
 * Equality compares author, target version and operations, the latter
 * without their contexts.
 */
export class WaveletDelta implements Iterable<WaveletOperation> {
  readonly author: ParticipantId;
  /** The version the delta applies at. */
  readonly targetVersion: HashedVersion;
  readonly ops: readonly WaveletOperation[];

  constructor(
    author: ParticipantId,
    targetVersion: HashedVersion,
    ops: readonly WaveletOperation[],
  ) {
    this.author = author;
    this.targetVersion = targetVersion;
    this.ops = [...ops];
  }

  /** The version number after the delta: each operation counts for one. */
  get resultingVersion(): number {
    return this.targetVersion.version + this.ops.length;
  }

  /** Number of operations. */
  get length(): number {
    return this.ops.length;
  }

  isEmpty(): boolean {
    return this.ops.length === 0;
  }

  [Symbol.iterator](): IterableIterator<WaveletOperation> {
    return this.ops[Symbol.iterator]();
  }

  equals(other: WaveletDelta): boolean {
    return (
      this.author.equals(other.author) &&
      this.targetVersion.equals(other.targetVersion) &&
      opsEqual(this.ops, other.ops)
    );
  }

  toString(): string {
    const ops =
      this.ops.length === 0 ? "[]" : ` ${this.ops.length} ops: [${this.ops.join(",")}]`;
    return `CoreWaveletDelta(${this.author}, ${this.targetVersion}, ${ops})`;
  }
}

/**
 * A delta as applied by the server: transformed to the version it was
 * applied at, with the resulting hashed version and the application
 * timestamp. Immutable; iterates over its operations.
 *
 * Every operation's context has the delta's author and timestamp and a
 * version increment of one; exactly the last one carries the resulting
 * hashed version.
 */
export class TransformedWaveletDelta implements Iterable<WaveletOperation> {
  readonly author: ParticipantId;
  /** The version after the delta. */
  readonly resultingVersion: HashedVersion;
  /** Milliseconds since the epoch. */
  readonly applicationTimestamp: number;
  readonly ops: readonly WaveletOperation[];

  /**
   * Checks that the operation contexts are consistent with the delta;
   * throws {@link DeltaError} otherwise.
   */
  constructor(
    author: ParticipantId,
    resultingVersion: HashedVersion,
    applicationTimestamp: number,
    ops: readonly WaveletOperation[],
  ) {
    const count = ops.length;
    if (resultingVersion.version < count) {
      throw new DeltaError({
        kind: "versionTooSmall",
        version: resultingVersion.version,
        count,
      });
    }
    ops.forEach((op, i) => {
      const c = op.context;
      const last = i === count - 1;
      if (!c.creator.equals(author)) {
        throw new DeltaError({
          kind: "creatorMismatch",
          creator: c.creator.toString(),
          author: author.toString(),
        });
      }
      if (c.versionIncrement !== 1) {
        throw new DeltaError({ kind: "versionIncrement", increment: c.versionIncrement });
      }
      if ((c.hashedVersion !== null) !== last) {
        throw new DeltaError({ kind: "hashedVersionPresence", index: i + 1, count });
      }
      if (c.hashedVersion !== null && !c.hashedVersion.equals(resultingVersion)) {
        throw new DeltaError({
          kind: "hashedVersionMismatch",
          context: c.hashedVersion,
          delta: resultingVersion,
        });
      }
      if (c.timestamp !== applicationTimestamp) {
        throw new DeltaError({
          kind: "timestampMismatch",
          context: c.timestamp,
          delta: applicationTimestamp,
        });
      }
    });
    this.author = author;
    this.resultingVersion = resultingVersion;
    this.applicationTimestamp = applicationTimestamp;
    this.ops = [...ops];
  }

  /**
   * Builds a transformed delta from operations in any context by cloning
   * them into the contexts the delta requires. Throws {@link DeltaError} on
   * operations that cannot be cloned (version updates and blip submits).
   */
  static cloneOperations(
    author: ParticipantId,
    resultingVersion: HashedVersion,
    applicationTimestamp: number,
    ops: readonly WaveletOperation[],
  ): TransformedWaveletDelta {
    const initialContext = new WaveletOperationContext(author, applicationTimestamp, 1);
    const finalContext = WaveletOperationContext.withHashedVersion(
      author,
      applicationTimestamp,
      1,
      resultingVersion,
    );
    let transformed: WaveletOperation[];
    try {
      transformed = ops.map((op, i) =>
        op.cloneOp(i === ops.length - 1 ? finalContext : initialContext),
      );
    } catch (e) {
      if (e instanceof WaveletOperationError) {
        throw new DeltaError({ kind: "operation", error: e });
      }
      throw e;
    }
    return new TransformedWaveletDelta(author, resultingVersion, applicationTimestamp, transformed);
  }

  /**
   * {@link TransformedWaveletDelta.cloneOperations} with the author and
   * operations of `delta`.
   */
  static fromDelta(
    resultingVersion: HashedVersion,
    applicationTimestamp: number,
    delta: WaveletDelta,
  ): TransformedWaveletDelta {
    return TransformedWaveletDelta.cloneOperations(
      delta.author,
      resultingVersion,
      applicationTimestamp,
      delta.ops,
    );
  }

  /** The version number the delta was applied at. */
  get appliedAtVersion(): number {
    return this.resultingVersion.version - this.ops.length;
  }

  /** Number of operations. */
  get length(): number {
    return this.ops.length;
  }

  isEmpty(): boolean {
    return this.ops.length === 0;
  }

  [Symbol.iterator](): IterableIterator<WaveletOperation> {
    return this.ops[Symbol.iterator]();
  }

  equals(other: TransformedWaveletDelta): boolean {
    return (
      this.author.equals(other.author) &&
      this.resultingVersion.equals(other.resultingVersion) &&
      this.applicationTimestamp === other.applicationTimestamp &&
      opsEqual(this.ops, other.ops)
    );
  }

  toString(): string {
    const ops = this.ops.length === 0 ? "[]" : `${this.ops.length} ops: [${this.ops.join(",")}]`;
    return (
      `TransformedWaveletDelta(${this.author}, ${this.resultingVersion}, ` +
      `${this.applicationTimestamp} -> ${ops})`
    );
  }
}
