// A mock WaveletDeltaChannel.
//
// Derived from `MockWaveletDeltaChannel` of the Apache Wave test tree
// (Apache License, Version 2.0).

import { expect } from "vitest";

import type { WaveletData } from "../../../model/data";
import type { TransformedWaveletDelta, WaveletDelta } from "../../../model/operation";
import type { HashedVersion } from "../../../model/version";
import { deltasAreEqual } from "../../testing";
import type {
  MultiplexedDeltaChannel,
  WaveletDeltaChannelReceiver,
  WaveletDeltaChannelTransmitter,
} from "..";

type Expectation =
  | { readonly method: "RESET"; readonly receiver: WaveletDeltaChannelReceiver | null }
  | { readonly method: "SEND"; readonly delta: WaveletDelta };

export class MockWaveletDeltaChannel implements MultiplexedDeltaChannel {
  private readonly expectations: Expectation[] = [];

  expectReset(r: WaveletDeltaChannelReceiver | null): void {
    this.expectations.push({ method: "RESET", receiver: r });
  }

  expectSend(delta: WaveletDelta): void {
    this.expectations.push({ method: "SEND", delta });
  }

  checkExpectationsSatisfied(): void {
    expect(
      this.expectations.map((e) => e.method),
      "Unsatisfied delta channel expectations",
    ).toEqual([]);
  }

  private next(method: string): Expectation {
    const expected = this.expectations.shift();
    if (expected === undefined) {
      return expect.fail(`Unexpected delta channel call: ${method}`);
    }
    expect(expected.method).toBe(method);
    return expected;
  }

  reset(receiver: WaveletDeltaChannelReceiver | null): void {
    const expected = this.next("RESET") as Extract<Expectation, { method: "RESET" }>;
    expect(receiver).toBe(expected.receiver);
  }

  send(transmitter: WaveletDeltaChannelTransmitter): void {
    const expected = this.next("SEND") as Extract<Expectation, { method: "SEND" }>;
    const delta = transmitter.takeMessage().getDelta();
    expect(deltasAreEqual(expected.delta, delta), `${expected.delta} vs. ${delta}`).toBe(true);
  }

  // The original has no way to expect the two calls below either: any such
  // call fails the test.

  onWaveletSnapshot(
    _snapshot: WaveletData | null,
    _lastCommittedVersion: HashedVersion | null,
    _currentSignedVersion: HashedVersion | null,
  ): void {
    this.next("WAVELET_SNAPSHOT");
  }

  onWaveletUpdate(
    _deltas: readonly TransformedWaveletDelta[] | null,
    _lastCommittedVersion: HashedVersion | null,
    _currentSignedVersion: HashedVersion | null,
  ): void {
    this.next("WAVELET_UPDATE");
  }

  debugGetProfilingInfo(): string {
    return "null";
  }
}
