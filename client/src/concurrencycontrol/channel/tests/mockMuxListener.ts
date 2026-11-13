// The mock listener on the OperationChannelMultiplexer.
//
// Derived from `MockMuxListener` of the Apache Wave test tree (Apache
// License, Version 2.0).

import { expect } from "vitest";

import type { WaveletData } from "../../../model/data";
import type { WaveletId } from "../../../model/id";
import type { CorruptionDetail } from "../../common";
import type { Accessibility, OperationChannel, OperationChannelMultiplexerListener } from "..";

type MethodCall =
  | "ON_OPERATION_CHANNEL_CREATED"
  | "ON_OPERATION_CHANNEL_REMOVED"
  | "ON_OPEN_FINISHED"
  | "ON_FAILED";

interface MethodCallContext {
  readonly method: MethodCall;
  readonly channel: OperationChannel | null;
  readonly snapshot: WaveletData | null;
  readonly accessibility: Accessibility | null;
}

export class MockMuxListener implements OperationChannelMultiplexerListener {
  private readonly methodCalls: MethodCallContext[] = [];

  onOperationChannelCreated(
    channel: OperationChannel,
    snapshot: WaveletData,
    accessibility: Accessibility,
  ): void {
    this.methodCalls.push({ method: "ON_OPERATION_CHANNEL_CREATED", channel, snapshot, accessibility });
  }

  onOperationChannelRemoved(channel: OperationChannel, _waveletId: WaveletId): void {
    this.methodCalls.push({
      method: "ON_OPERATION_CHANNEL_REMOVED",
      channel,
      snapshot: null,
      accessibility: null,
    });
  }

  onOpenFinished(): void {
    this.methodCalls.push({
      method: "ON_OPEN_FINISHED",
      channel: null,
      snapshot: null,
      accessibility: null,
    });
  }

  onFailed(_detail: CorruptionDetail): void {
    this.methodCalls.push({ method: "ON_FAILED", channel: null, snapshot: null, accessibility: null });
  }

  private remove(): MethodCallContext {
    const context = this.methodCalls.shift();
    if (context === undefined) {
      return expect.fail("Expected a mux listener call, there was none");
    }
    return context;
  }

  verifyNoMoreInteractions(): void {
    expect(
      this.methodCalls.map((c) => c.method),
      "Unexpected method calls",
    ).toEqual([]);
  }

  verifyOperationChannelCreated(
    snapshot: WaveletData,
    accessibility: Accessibility,
  ): OperationChannel {
    const context = this.remove();
    expect(context.method).toBe("ON_OPERATION_CHANNEL_CREATED");
    // The snapshot is not compared with equals(), as in the original (the
    // creation time comes from the clock).
    const actual = context.snapshot as WaveletData;
    expect(actual.waveId.equals(snapshot.waveId)).toBe(true);
    expect(actual.waveletId.equals(snapshot.waveletId)).toBe(true);
    expect(actual.creator.equals(snapshot.creator)).toBe(true);
    expect(actual.version).toBe(snapshot.version);
    expect(actual.hashedVersion.equals(snapshot.hashedVersion)).toBe(true);
    expect(context.accessibility).toBe(accessibility);
    return context.channel as OperationChannel;
  }

  verifyOperationChannelRemoved(channel: OperationChannel): void {
    const context = this.remove();
    expect(context.method).toBe("ON_OPERATION_CHANNEL_REMOVED");
    expect(context.channel).toBe(channel);
  }

  verifyOpenFinished(): void {
    const context = this.remove();
    expect(context.method).toBe("ON_OPEN_FINISHED");
  }

  verifyFailed(): void {
    const context = this.remove();
    expect(context.method).toBe("ON_FAILED");
  }
}
