// A minimal fake multiplexer.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.testing.FakeOperationChannelMultiplexer`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { IdFilter, ParticipantId, WaveletId } from "../../model/id";
import type {
  KnownWavelet,
  OperationChannelMultiplexer,
  OperationChannelMultiplexerListener,
} from "../channel";
import { FakeOperationChannel } from "./fakeOperationChannel";

export class FakeOperationChannelMultiplexer implements OperationChannelMultiplexer {
  close(): void {
    // Does nothing
  }

  createOperationChannel(_waveletId: WaveletId, _creator: ParticipantId): void {
    throw new Error("Unsupported operation");
  }

  /**
   * Notifies the listener of a fake channel for each known wavelet and, if
   * there are any, that the open has finished. Without known wavelets it
   * does nothing.
   */
  open(
    muxListener: OperationChannelMultiplexerListener,
    _waveletFilter: IdFilter,
    knownWavelets: Iterable<KnownWavelet> = [],
  ): void {
    const known = [...knownWavelets];
    if (known.length !== 0) {
      for (const knownWavelet of known) {
        muxListener.onOperationChannelCreated(
          new FakeOperationChannel(),
          knownWavelet.snapshot,
          knownWavelet.accessibility,
        );
      }
      // consider the wave as if open has finished.
      muxListener.onOpenFinished();
    }
  }
}
