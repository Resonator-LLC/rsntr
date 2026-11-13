// A mock view channel which allows setting of expectations.
//
// Derived from `MockViewChannel` of the Apache Wave test tree (Apache
// License, Version 2.0).

import { expect } from "vitest";

import type { IdFilter, WaveId, WaveletId } from "../../../model/id";
import type { WaveletDelta } from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { ResponseCode } from "../../common";
import { deltasAreEqual } from "../../testing";
import type {
  KnownWaveletVersions,
  SubmitCallback,
  ViewChannel,
  ViewChannelFactory,
  ViewChannelListener,
} from "..";
import { expectKnownWaveletsEqual } from "./helpers";

type Expectation =
  | {
      readonly method: "OPEN";
      readonly waveletFilter: IdFilter;
      readonly knownWavelets: KnownWaveletVersions;
    }
  | { readonly method: "SUBMIT"; readonly waveletId: WaveletId; readonly delta: WaveletDelta }
  | { readonly method: "CLOSE" };

/** Factory for mock view channels. Provides access to the created channels. */
export class MockViewChannelFactory implements ViewChannelFactory {
  private readonly channels: MockViewChannel[] = [];

  /** Expects a channel creation. Returns the channel which `create` will return. */
  expectCreate(): MockViewChannel {
    const ch = new MockViewChannel();
    this.channels.push(ch);
    return ch;
  }

  create(_waveId: WaveId): ViewChannel {
    const ch = this.channels.shift();
    if (ch === undefined) {
      return expect.fail("Unexpected view channel creation");
    }
    return ch;
  }
}

export class MockViewChannel implements ViewChannel {
  /** `MockViewChannel.Factory` of the original. */
  static readonly Factory = MockViewChannelFactory;

  private readonly expectations: Expectation[] = [];
  private readonly listeners: ViewChannelListener[] = [];
  private readonly awaitingAck: SubmitCallback[] = [];

  expectOpen(waveletFilter: IdFilter, knownWavelets: KnownWaveletVersions): void {
    this.expectations.push({ method: "OPEN", waveletFilter, knownWavelets });
  }

  expectSubmitDelta(waveletId: WaveletId, delta: WaveletDelta): void {
    this.expectations.push({ method: "SUBMIT", waveletId, delta });
  }

  expectClose(): void {
    this.expectations.push({ method: "CLOSE" });
  }

  checkExpectationsSatisified(): void {
    expect(
      this.expectations.map((e) => e.method),
      "Unsatisified view channel expectations",
    ).toEqual([]);
  }

  private next<M extends Expectation["method"]>(method: M): Extract<Expectation, { method: M }> {
    const expected = this.expectations.shift();
    if (expected === undefined) {
      return expect.fail(`Unexpected view channel call: ${method}`);
    }
    expect(expected.method).toBe(method);
    return expected as Extract<Expectation, { method: M }>;
  }

  open(
    viewListener: ViewChannelListener,
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
  ): void {
    const expected = this.next("OPEN");
    expect(
      waveletFilter.equals(expected.waveletFilter),
      `filter: expected ${expected.waveletFilter}, got ${waveletFilter}`,
    ).toBe(true);
    expectKnownWaveletsEqual(knownWavelets, expected.knownWavelets);
    this.listeners.push(viewListener);
  }

  submitDelta(waveletId: WaveletId, delta: WaveletDelta, callback: SubmitCallback): void {
    const expected = this.next("SUBMIT");
    expect(waveletId.equals(expected.waveletId)).toBe(true);
    expect(deltasAreEqual(expected.delta, delta), `expected ${expected.delta}, got ${delta}`).toBe(
      true,
    );
    this.awaitingAck.push(callback);
  }

  close(): void {
    this.next("CLOSE");
    // Emulate the real view channel in sending close() synchronously.
    for (const l of this.listeners) {
      l.onClosed();
    }
  }

  takeListener(): ViewChannelListener {
    const listener = this.listeners.shift();
    if (listener === undefined) {
      return expect.fail("No view listener to take");
    }
    return listener;
  }

  private nextCallback(): SubmitCallback {
    const callback = this.awaitingAck.shift();
    if (callback === undefined) {
      return expect.fail("No submit awaiting an ack");
    }
    return callback;
  }

  ackSubmit(opsApplied: number, version: number, signature: Uint8Array): void {
    this.nextCallback().onSuccess(
      opsApplied,
      HashedVersion.of(version, signature),
      ResponseCode.OK,
      null,
    );
  }

  nackSubmit(reason: string, version: number, signature: Uint8Array): void {
    this.nextCallback().onSuccess(
      0,
      HashedVersion.of(version, signature),
      ResponseCode.INTERNAL_ERROR,
      reason,
    );
  }

  debugGetProfilingInfo(_waveletId: WaveletId): string | null {
    return null;
  }
}
