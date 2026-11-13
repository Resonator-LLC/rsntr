// A mock operation channel which allows expectations to be set.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.testing.MockOperationChannel`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveletOperation } from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { OperationChannel, OperationChannelListener } from "../channel";
import { assertTrue } from "./testAssert";

type Expectation =
  | { readonly method: "SEND"; readonly operations: readonly WaveletOperation[] }
  | { readonly method: "RECEIVE"; readonly op: WaveletOperation | null }
  | { readonly method: "PEEK"; readonly op: WaveletOperation | null };

export class MockOperationChannel implements OperationChannel {
  private readonly expectations: Expectation[] = [];

  /** Expects a call to send() with a list of operations. */
  expectSend(...operations: WaveletOperation[]): void {
    this.expectations.push({ method: "SEND", operations });
  }

  /**
   * Expects a call to receive(), which will return the provided operation
   * (may be null).
   */
  expectReceive(op: WaveletOperation | null): void {
    this.expectations.push({ method: "RECEIVE", op });
  }

  /**
   * Expects a call to peek(), which will return the provided operation (may
   * be null).
   */
  expectPeek(op: WaveletOperation | null): void {
    this.expectations.push({ method: "PEEK", op });
  }

  /** Checks that all expectations have been satisfied. */
  checkExpectationsSatisfied(): void {
    assertTrue(this.expectations.length === 0, "Unsatisfied expectations");
  }

  private next<M extends Expectation["method"]>(method: M): Extract<Expectation, { method: M }> {
    const expected = this.expectations.shift();
    assertTrue(expected !== undefined, `Unexpected call to ${method}`);
    assertTrue(expected.method === method, `Expected ${expected.method}, got ${method}`);
    return expected as Extract<Expectation, { method: M }>;
  }

  setListener(_listener: OperationChannelListener | null): void {
    // Do nothing.
  }

  /**
   * The operations must equal the expected ones (the original compares the
   * arrays themselves, which only holds for the same array).
   */
  send(...operations: WaveletOperation[]): void {
    const expected = this.next("SEND").operations;
    assertTrue(
      expected.length === operations.length && expected.every((op, i) => op.equals(operations[i]!)),
      `Expected send of [${expected.join(", ")}], got [${operations.join(", ")}]`,
    );
  }

  receive(): WaveletOperation | null {
    return this.next("RECEIVE").op;
  }

  peek(): WaveletOperation | null {
    return this.next("PEEK").op;
  }

  getReconnectVersions(): HashedVersion[] {
    throw new Error("Reconnection not supported");
  }

  getDebugString(): string {
    return "[Mock operation channel]";
  }
}
