// A mock operation channel listener that records calls to
// onOperationReceived() for future inspection.
//
// Derived from `MockOperationChannelListener` of the Apache Wave test tree
// (Apache License, Version 2.0).

import { expect } from "vitest";

import type { OperationChannelListener } from "..";

export class MockOperationChannelListener implements OperationChannelListener {
  private numOperationsReceived = 0;
  private readonly listeners: OperationChannelListener[] = [];

  /** Allows a test to hook into the callbacks. */
  addListener(newListener: OperationChannelListener): void {
    this.listeners.push(newListener);
  }

  onOperationReceived(): void {
    this.numOperationsReceived++;
    for (const l of this.listeners) {
      l.onOperationReceived();
    }
  }

  /** The number of previous calls to onOperationReceived(). */
  getEventCount(): number {
    return this.numOperationsReceived;
  }

  clear(): void {
    this.numOperationsReceived = 0;
  }

  checkOpsReceived(expectedOpCount: number): void {
    expect(this.numOperationsReceived).toBe(expectedOpCount);
  }
}
