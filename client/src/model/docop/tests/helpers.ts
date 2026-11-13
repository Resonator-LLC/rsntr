// Shared helpers for the ported tests (the Rust `tests/mod.rs`).

import { expect } from "vitest";

import {
  AnnotationBoundaryMap,
  Attributes,
  type ChangeTriple,
  OperationError,
  type OperationErrorDetail,
  TransformError,
  type TransformErrorDetail,
} from "..";

/** `new AttributesImpl("name", "value", ...)`. */
export function attrs(...pairs: string[]): Attributes {
  if (pairs.length % 2 !== 0) {
    throw new Error("pairs must come in groups of two");
  }
  const entries: [string, string][] = [];
  for (let i = 0; i < pairs.length; i += 2) {
    entries.push([pairs[i]!, pairs[i + 1]!]);
  }
  return Attributes.fromPairs(entries);
}

/** A boundary that starts one annotation change. */
export function beginAnnotation(
  key: string,
  oldValue: string | null,
  newValue: string | null,
): AnnotationBoundaryMap {
  return AnnotationBoundaryMap.changesOnly([[key, oldValue, newValue]]);
}

/** A boundary that ends the given keys. */
export function finishAnnotation(...keys: string[]): AnnotationBoundaryMap {
  return AnnotationBoundaryMap.ends(keys);
}

/** A boundary with end keys and `[key, old, new]` changes. */
export function boundary(
  endKeys: readonly string[],
  changes: readonly ChangeTriple[],
): AnnotationBoundaryMap {
  return AnnotationBoundaryMap.create(
    [...endKeys],
    changes.map(([key, oldValue, newValue]) => ({ key, oldValue, newValue })),
  );
}

/** Runs `f` and returns what it throws; fails if it does not throw. */
export function thrownBy(f: () => unknown): unknown {
  try {
    f();
  } catch (e) {
    return e;
  }
  throw new Error("expected the call to throw");
}

/**
 * Asserts that `f` throws an `OperationError` (Rust `Err(OperationError::..)`),
 * optionally of the given kind, and returns it.
 */
export function expectOperationError(
  f: () => unknown,
  kind?: OperationErrorDetail["kind"],
): OperationError {
  const e = thrownBy(f);
  expect(e).toBeInstanceOf(OperationError);
  const error = e as OperationError;
  if (kind !== undefined) {
    expect(error.kind).toBe(kind);
  }
  return error;
}

/**
 * Asserts that `f` throws a `TransformError`, optionally of the given kind,
 * and returns it.
 */
export function expectTransformError(
  f: () => unknown,
  kind?: TransformErrorDetail["kind"],
): TransformError {
  const e = thrownBy(f);
  expect(e).toBeInstanceOf(TransformError);
  const error = e as TransformError;
  if (kind !== undefined) {
    expect(error.kind).toBe(kind);
  }
  return error;
}
