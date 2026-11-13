// Shared helpers for the ported operation tests.

import { expect } from "vitest";

import { Attributes, type DocInitialization, DocOpBuilder } from "../../docop";
import { ParticipantId } from "../../id";
import type { HashedVersion } from "../../version";
import { WaveletOperation, WaveletOperationContext, syntacticIdentity } from "..";

export function participant(address: string): ParticipantId {
  return ParticipantId.of(address);
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

/** Asserts that `f` throws an instance of `type` whose detail matches. */
export function expectError<E extends Error & { readonly detail: { readonly kind: string } }>(
  f: () => unknown,
  type: abstract new (...args: never[]) => E,
  detail: Record<string, unknown>,
): E {
  const e = thrownBy(f);
  expect(e).toBeInstanceOf(type);
  expect((e as E).detail).toMatchObject(detail);
  return e as E;
}

/** `ModelTestUtils.createContent`. */
export function createContent(text: string): DocInitialization {
  const builder = new DocOpBuilder()
    .elementStart("body", Attributes.EMPTY)
    .elementStart("line", Attributes.EMPTY)
    .elementEnd();
  if (text.length > 0) {
    builder.characters(text);
  }
  return builder.elementEnd().buildInitialization();
}

/** `DeltaTestUtil.insert`: an insertion into the document "blip id". */
export function insert(
  author: ParticipantId,
  pos: number,
  text: string,
  remaining: number,
  resultingVersion: HashedVersion | null = null,
): WaveletOperation {
  const builder = new DocOpBuilder().retain(pos).characters(text);
  if (remaining > 0) {
    builder.retain(remaining);
  }
  return WaveletOperation.blipContent(
    new WaveletOperationContext(author, 0, 1, resultingVersion),
    "blip id",
    builder.build(),
  );
}

/** Asserts that `operation` is the insertion `DeltaTestUtil.insert` builds. */
export function checkInsert(
  operation: WaveletOperation,
  location: number,
  content: string,
  remaining: number,
): void {
  const op = operation.contentOp;
  if (op === null) {
    throw new Error("Did not get an insertion operation.");
  }
  const builder = new DocOpBuilder().retain(location).characters(content);
  if (remaining > 0) {
    builder.retain(remaining);
  }
  expect(syntacticIdentity(builder.build(), op), op.toString()).toBe(true);
}

export function expectOpsEqual(
  actual: readonly WaveletOperation[],
  expected: readonly WaveletOperation[],
): void {
  // Equality ignores the contexts.
  expect(actual.length).toBe(expected.length);
  actual.forEach((op, i) =>
    expect(op.equals(expected[i]!), `op ${i}: ${op} vs ${expected[i]}`).toBe(true),
  );
}

export function expectParticipants(
  actual: readonly ParticipantId[],
  expected: readonly ParticipantId[],
): void {
  expect(actual.map((p) => p.address)).toEqual(expected.map((p) => p.address));
}
