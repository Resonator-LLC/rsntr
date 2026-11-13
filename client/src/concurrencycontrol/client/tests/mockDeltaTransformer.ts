// Mock for describing interactions with an operation queue `Transformer`,
// including specifying what deltas should result from those interactions.
// Port of the Apache Wave test class `MockDeltaTransformer` (Apache License,
// Version 2.0).

import { expect } from "vitest";

import { DeltaPair, type WaveletOperation } from "../../../model/operation";
import type { Transformer } from "..";

/** An expected call to `transform`. */
export class Expectation {
  private readonly inputClientOps: readonly WaveletOperation[];
  private outputClientDelta: readonly WaveletOperation[] | null = null;
  private readonly outputServerDelta: readonly WaveletOperation[] = [];

  /**
   * An expected transform that accepts an input client delta which has the
   * given op list. It accepts any input server delta and outputs its own
   * server delta.
   */
  constructor(inputClientOps: readonly WaveletOperation[]) {
    this.inputClientOps = inputClientOps;
  }

  /** Asserts that the given delta is the expected input client delta. */
  assertClientInputValid(candidate: readonly WaveletOperation[]): void {
    expect(candidate.length).toBe(this.inputClientOps.length);
    candidate.forEach((op, i) => {
      expect(op.equals(this.inputClientOps[i] as WaveletOperation)).toBe(true);
    });
  }

  /** The output client delta is the same as the input client delta. */
  echo(): void {
    this.transformTo(...this.inputClientOps);
  }

  /** The client/server delta pair output by this expected transform. */
  getOutput(): DeltaPair {
    if (this.outputClientDelta === null) {
      throw new Error("No output client delta specified");
    }
    return new DeltaPair(this.outputClientDelta, this.outputServerDelta);
  }

  kill(): void {
    this.transformTo();
  }

  /** The output client delta consists of the given operations. */
  transformTo(...outputClientOps: WaveletOperation[]): void {
    expect(this.outputClientDelta, "Output client delta already specified").toBeNull();
    this.outputClientDelta = outputClientOps;
  }
}

/**
 * A transformer that initially has no expectations, and only allows calls
 * to `transform` that have been expected via {@link MockDeltaTransformer.expect}.
 */
export class MockDeltaTransformer implements Transformer {
  private readonly expectations: Expectation[] = [];
  private readonly firstServerInputDelta: readonly WaveletOperation[] = [];

  /** Checks that all expected calls to transform have been made. */
  checkDone(): void {
    expect(this.expectations.length).toBe(0);
  }

  /**
   * The transform call after all existing expectations must be one with a
   * client input delta consisting of the given ops. The returned expectation
   * must be told what to output.
   */
  expect(...ops: WaveletOperation[]): Expectation {
    const expectation = new Expectation(ops);
    this.expectations.push(expectation);
    return expectation;
  }

  /** The ops that should be given to the first `transform` call. */
  getInputServerDelta(): readonly WaveletOperation[] {
    return this.firstServerInputDelta;
  }

  /**
   * The delta that should be produced if all expected transforms are
   * performed in order on the input server delta.
   */
  getOutputServerDelta(): readonly WaveletOperation[] {
    const last = this.expectations[this.expectations.length - 1];
    return last === undefined ? this.firstServerInputDelta : last.getOutput().server;
  }

  transform(client: readonly WaveletOperation[]): DeltaPair {
    const expectation = this.expectations.shift();
    if (expectation === undefined) {
      throw new Error("Unexpected call to transform");
    }
    expectation.assertClientInputValid(client);
    return expectation.getOutput();
  }
}
