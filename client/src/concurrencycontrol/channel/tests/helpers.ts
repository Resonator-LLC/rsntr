// Shared helpers for the ported channel tests: small stand-ins for
// `DeltaTestUtil`, `FakeHashedVersionFactory` and `BasicFactories` of the
// Apache Wave test tree (Apache License, Version 2.0).

import { expect } from "vitest";

import { WaveletData } from "../../../model/data";
import { DocOpBuilder } from "../../../model/docop";
import { type ParticipantId, type WaveId, type WaveletId, WaveletName } from "../../../model/id";
import {
  NO_TIMESTAMP,
  TransformedWaveletDelta,
  WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
} from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { ChannelException } from "../../common";
import type { KnownWaveletVersions, VersionZeroFactory } from "..";

/** A small deterministic generator (mulberry32), in place of `java.util.Random`. */
class SeededRandom {
  private state: number;

  constructor(seed: number) {
    this.state = seed >>> 0;
  }

  /** A non-negative 31-bit integer. */
  nextInt(): number {
    this.state = (this.state + 0x6d2b79f5) >>> 0;
    let t = this.state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) >>> 1;
  }
}

/** Makes operations and deltas by one author (`DeltaTestUtil`). */
export class DeltaTestUtil {
  readonly author: ParticipantId;
  private readonly random = new SeededRandom(42);

  constructor(author: ParticipantId) {
    this.author = author;
  }

  /** Create a NoOp operation. */
  noOp(): WaveletOperation {
    return WaveletOperation.noOp(new WaveletOperationContext(this.author, 0, 1));
  }

  /** Builds a random client delta. */
  makeDelta(targetVersion: HashedVersion, _timestamp: number, numOps: number): WaveletDelta {
    const context = new WaveletOperationContext(this.author, NO_TIMESTAMP, 1);
    const ops: WaveletOperation[] = [];
    for (let i = 0; i < numOps; ++i) {
      ops.push(this.randomOp(context));
    }
    return new WaveletDelta(this.author, targetVersion, ops);
  }

  /** Builds a random transformed delta. */
  makeTransformedDelta(
    applicationTimestamp: number,
    resultingVersion: HashedVersion,
    numOps: number,
  ): TransformedWaveletDelta {
    const dummy = new WaveletOperationContext(this.author, 0, 0);
    const ops: WaveletOperation[] = [];
    for (let i = 0; i < numOps; ++i) {
      ops.push(this.randomOp(dummy));
    }
    return TransformedWaveletDelta.cloneOperations(
      this.author,
      resultingVersion,
      applicationTimestamp,
      ops,
    );
  }

  /**
   * Creates a random op. The result is unlikely to be applicable to any
   * wavelet, but is generated such that we are fairly certain that it will
   * be unique so we can identify it when it completes a round-trip.
   */
  private randomOp(context: WaveletOperationContext): WaveletOperation {
    const blipOp = new DocOpBuilder()
      .retain(Math.floor(this.random.nextInt() / 2) + 1)
      .characters(`createRndOp#${this.random.nextInt()}`)
      .build();
    return WaveletOperation.blipContent(context, `createRndId#${this.random.nextInt()}`, blipOp);
  }
}

/** A hashed version factory which generates unsigned versions (`FakeHashedVersionFactory`). */
export const FAKE_HASHED_VERSION_FACTORY: VersionZeroFactory = {
  createVersionZero: () => HashedVersion.unsigned(0),
};

/** An empty wavelet at a version (`DATA_FACTORY.create(new EmptyWaveletSnapshot(...))`). */
export function emptySnapshot(
  waveId: WaveId,
  waveletId: WaveletId,
  creator: ParticipantId,
  version: HashedVersion,
  creationTime: number,
): WaveletData {
  return WaveletData.empty(WaveletName.of(waveId, waveletId), creator, version, creationTime);
}

export function bytes(...values: number[]): Uint8Array {
  return Uint8Array.from(values);
}

/** `assertEquals` on hashed versions that may be null. */
export function expectVersionEquals(
  actual: HashedVersion | null,
  expected: HashedVersion | null,
  what = "version",
): void {
  if (expected === null || actual === null) {
    expect(actual, what).toBe(expected);
  } else {
    expect(actual.equals(expected), `${what}: expected ${expected}, got ${actual}`).toBe(true);
  }
}

/** `assertEquals` on maps of known wavelet versions. */
export function expectKnownWaveletsEqual(
  actual: KnownWaveletVersions,
  expected: KnownWaveletVersions,
): void {
  const equal = actual.equals(
    expected,
    (a, b) => a.length === b.length && a.every((v, i) => v.equals(b[i]!)),
  );
  expect(equal, `known wavelets: expected ${expected}, got ${actual}`).toBe(true);
}

/** Runs `f`, which must throw a `ChannelException`, and returns it. */
export function expectChannelException(f: () => unknown, message: string): ChannelException {
  let thrown: unknown = null;
  try {
    f();
  } catch (e) {
    thrown = e;
  }
  if (thrown === null) {
    expect.fail(message);
  }
  if (!(thrown instanceof ChannelException)) {
    throw thrown;
  }
  return thrown;
}
