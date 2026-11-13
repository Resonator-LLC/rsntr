// Errors of wavelet operations, deltas and their transformation.
//
// Derived from `org.waveprotocol.wave.model.operation` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import type { DataError } from "../data";
import type { OperationError, TransformError } from "../docop";
import type { HashedVersion } from "../version";

/** The reason for a {@link WaveletOperationError}. */
export type WaveletOperationErrorDetail =
  | { readonly kind: "duplicateParticipant"; readonly participant: string }
  | { readonly kind: "missingParticipant"; readonly participant: string }
  /** A version update names a document that does not exist. */
  | { readonly kind: "missingDocument"; readonly docId: string }
  /** The operation would move a version below zero or beyond the safe integers. */
  | { readonly kind: "versionOutOfRange"; readonly version: number; readonly increment: number }
  | { readonly kind: "document"; readonly error: OperationError }
  | { readonly kind: "data"; readonly error: DataError }
  /** The operation cannot be cloned with a new context. */
  | { readonly kind: "uncloneable"; readonly op: string }
  /** A delta does not apply at the wavelet's version. */
  | {
      readonly kind: "deltaVersionMismatch";
      readonly deltaVersion: number;
      readonly waveletVersion: number;
    }
  /** An operation of a delta failed and the operations before it were rolled back. */
  | {
      readonly kind: "deltaFailed";
      readonly applied: number;
      readonly total: number;
      readonly version: number;
      readonly failedOp: string;
      readonly cause: WaveletOperationError;
    }
  /** Rolling back a failed delta failed; the wavelet state is corrupt. */
  | { readonly kind: "rollbackFailed"; readonly inverse: string; readonly cause: WaveletOperationError }
  | { readonly kind: "invalidArgument"; readonly message: string };

function waveletOperationErrorMessage(d: WaveletOperationErrorDetail): string {
  switch (d.kind) {
    case "duplicateParticipant":
      return `Attempt to add a duplicate participant ${d.participant}`;
    case "missingParticipant":
      return `Attempt to remove non-existent participant ${d.participant}`;
    case "missingDocument":
      return `no document ${d.docId}`;
    case "versionOutOfRange":
      return `version out of range: ${d.version} + ${d.increment}`;
    case "document":
    case "data":
      return d.error.message;
    case "uncloneable":
      return `Un-cloneable operation: ${d.op}`;
    case "deltaVersionMismatch":
      return `Delta's version ${d.deltaVersion} doesn't apply to wavelet at ${d.waveletVersion}`;
    case "deltaFailed":
      return (
        `Only applied ${d.applied} of ${d.total} operations at version ${d.version}, ` +
        `rolling back, failed op was ${d.failedOp}: ${d.cause.message}`
      );
    case "rollbackFailed":
      return `Failed to roll back operation with inverse ${d.inverse}: ${d.cause.message}`;
    case "invalidArgument":
      return d.message;
  }
}

function causeOf(d: { readonly kind: string }): ErrorOptions | undefined {
  const c = d as { readonly error?: unknown; readonly cause?: unknown };
  const cause = c.error ?? c.cause;
  return cause === undefined ? undefined : { cause };
}

/**
 * Failure to apply a wavelet operation (`OperationException`, plus the
 * runtime exceptions the original throws on inconsistent input).
 */
export class WaveletOperationError extends Error {
  readonly detail: WaveletOperationErrorDetail;

  constructor(detail: WaveletOperationErrorDetail) {
    super(waveletOperationErrorMessage(detail), causeOf(detail));
    this.name = "WaveletOperationError";
    this.detail = detail;
  }

  get kind(): WaveletOperationErrorDetail["kind"] {
    return this.detail.kind;
  }

  static invalidArgument(message: string): WaveletOperationError {
    return new WaveletOperationError({ kind: "invalidArgument", message });
  }
}

/** The reason for a {@link WaveTransformError}. */
export type WaveTransformErrorDetail =
  /**
   * The author of the client operation was concurrently removed from the
   * wavelet (`RemovedAuthorException`).
   */
  | { readonly kind: "removedAuthor"; readonly author: string }
  /** A participant was concurrently added and removed. */
  | { readonly kind: "participant"; readonly participant: string }
  | { readonly kind: "document"; readonly error: TransformError };

function waveTransformErrorMessage(d: WaveTransformErrorDetail): string {
  switch (d.kind) {
    case "removedAuthor":
      return d.author;
    case "participant":
      return `Transform error involving participant: ${d.participant}`;
    case "document":
      return d.error.message;
  }
}

/** Failure to transform a pair of concurrent wavelet operations (`TransformException`). */
export class WaveTransformError extends Error {
  readonly detail: WaveTransformErrorDetail;

  constructor(detail: WaveTransformErrorDetail) {
    super(waveTransformErrorMessage(detail), causeOf(detail));
    this.name = "WaveTransformError";
    this.detail = detail;
  }

  get kind(): WaveTransformErrorDetail["kind"] {
    return this.detail.kind;
  }
}

/** The reason for a {@link DeltaError}. */
export type DeltaErrorDetail =
  | { readonly kind: "creatorMismatch"; readonly creator: string; readonly author: string }
  | { readonly kind: "versionIncrement"; readonly increment: number }
  /** `index` is one-based. */
  | { readonly kind: "hashedVersionPresence"; readonly index: number; readonly count: number }
  | {
      readonly kind: "hashedVersionMismatch";
      readonly context: HashedVersion;
      readonly delta: HashedVersion;
    }
  | { readonly kind: "timestampMismatch"; readonly context: number; readonly delta: number }
  /** The resulting version is smaller than the number of operations. */
  | { readonly kind: "versionTooSmall"; readonly version: number; readonly count: number }
  | { readonly kind: "operation"; readonly error: WaveletOperationError };

function deltaErrorMessage(d: DeltaErrorDetail): string {
  switch (d.kind) {
    case "creatorMismatch":
      return `Context creator ${d.creator} doesn't match delta author ${d.author}`;
    case "versionIncrement":
      return `Invalid context version increment ${d.increment}`;
    case "hashedVersionPresence":
      return `[Un]expected hashed version on op ${d.index} of ${d.count}`;
    case "hashedVersionMismatch":
      return `Context hashed version ${d.context} doesn't match delta hashed version ${d.delta}`;
    case "timestampMismatch":
      return `Context timestamp ${d.context} doesn't match delta timestamp ${d.delta}`;
    case "versionTooSmall":
      return `resulting version ${d.version} is less than the operation count ${d.count}`;
    case "operation":
      return d.error.message;
  }
}

/** A transformed delta whose operation contexts are inconsistent with it. */
export class DeltaError extends Error {
  readonly detail: DeltaErrorDetail;

  constructor(detail: DeltaErrorDetail) {
    super(deltaErrorMessage(detail), causeOf(detail));
    this.name = "DeltaError";
    this.detail = detail;
  }

  get kind(): DeltaErrorDetail["kind"] {
    return this.detail.kind;
  }
}

/** The reason for a {@link SubmitTransformError}. */
export type SubmitTransformErrorDetail =
  /**
   * The delta targets the current version number with a different hash
   * (`InvalidHashException`).
   */
  | { readonly kind: "invalidHash"; readonly expected: HashedVersion; readonly target: HashedVersion }
  /**
   * The server history given does not lead from the delta's target version
   * to the current version.
   */
  | { readonly kind: "history"; readonly target: HashedVersion; readonly current: HashedVersion }
  | { readonly kind: "transform"; readonly error: WaveTransformError };

function submitTransformErrorMessage(d: SubmitTransformErrorDetail): string {
  switch (d.kind) {
    case "invalidHash":
      return `Mismatched hash, expected ${d.expected} but delta targets ${d.target}`;
    case "history":
      return `No deltas between valid versions ${d.target} and ${d.current}`;
    case "transform":
      return d.error.message;
  }
}

/** Failure to transform a submitted delta to the current wavelet version. */
export class SubmitTransformError extends Error {
  readonly detail: SubmitTransformErrorDetail;

  constructor(detail: SubmitTransformErrorDetail) {
    super(submitTransformErrorMessage(detail), causeOf(detail));
    this.name = "SubmitTransformError";
    this.detail = detail;
  }

  get kind(): SubmitTransformErrorDetail["kind"] {
    return this.detail.kind;
  }
}
