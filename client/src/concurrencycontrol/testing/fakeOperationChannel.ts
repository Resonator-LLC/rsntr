// A minimal fake operation channel.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.testing.FakeOperationChannel`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveletOperation } from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { OperationChannel, OperationChannelListener } from "../channel";

export class FakeOperationChannel implements OperationChannel {
  getDebugString(): string {
    throw new Error("Unsupported operation");
  }

  getReconnectVersions(): HashedVersion[] {
    throw new Error("Unsupported operation");
  }

  peek(): WaveletOperation | null {
    // Does nothing
    return null;
  }

  receive(): WaveletOperation | null {
    // Does nothing
    return null;
  }

  send(..._operations: WaveletOperation[]): void {
    // Does nothing
  }

  setListener(_listener: OperationChannelListener | null): void {
    // Does nothing
  }
}
