// Errors for failed sanity checks.
//
// Stand-ins for the `IllegalStateException` and `IllegalArgumentException`
// thrown by `Preconditions` in the concurrency control code of Apache Wave
// (Apache License, Version 2.0).

import { OperationError, TransformError } from "../../model/docop";
import { WaveTransformError, WaveletOperationError } from "../../model/operation";

/** A state check failed (`Preconditions.checkState` / `illegalState`). */
export class IllegalStateError extends Error {
  constructor(message?: string) {
    super(message);
    this.name = "IllegalStateError";
  }
}

/** An argument check failed (`Preconditions.checkArgument`). */
export class IllegalArgumentError extends Error {
  constructor(message?: string) {
    super(message);
    this.name = "IllegalArgumentError";
  }
}

/** Throws {@link IllegalStateError} unless `condition` holds. */
export function checkState(condition: boolean, message?: string): asserts condition {
  if (!condition) {
    throw new IllegalStateError(message);
  }
}

/** Throws {@link IllegalArgumentError} unless `condition` holds. */
export function checkArgument(condition: boolean, message?: string): asserts condition {
  if (!condition) {
    throw new IllegalArgumentError(message);
  }
}

/**
 * A transformation failure detected by the concurrency control itself
 * (`TransformException` thrown with a message). Failures inside the model's
 * transform are `WaveTransformError`; use {@link isTransformException} to
 * recognise both.
 */
export class TransformException extends Error {
  constructor(message?: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "TransformException";
  }
}

/**
 * An operation failure detected by the concurrency control itself
 * (`OperationException` thrown with a message). Failures inside the model
 * are `WaveletOperationError` or `OperationError`; use
 * {@link isOperationException} to recognise all of them.
 */
export class OperationException extends Error {
  constructor(message?: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "OperationException";
  }
}

/** Whether `e` stands for a Java `TransformException`. */
export function isTransformException(e: unknown): e is Error {
  return (
    e instanceof TransformException ||
    e instanceof WaveTransformError ||
    e instanceof TransformError
  );
}

/** Whether `e` stands for a Java `OperationException`. */
export function isOperationException(e: unknown): e is Error {
  return (
    e instanceof OperationException ||
    e instanceof WaveletOperationError ||
    e instanceof OperationError
  );
}

/** An element was requested from an empty collection (`NoSuchElementException`). */
export class NoSuchElementError extends Error {
  constructor(message?: string) {
    super(message);
    this.name = "NoSuchElementError";
  }
}
