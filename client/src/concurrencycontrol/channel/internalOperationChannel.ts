// Binds together both ends of an operation channel.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.channel.InternalOperationChannel`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { TransformedWaveletDelta } from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { OperationChannel } from "./operationChannel";
import type { WaveletDeltaChannelReceiver } from "./waveletDeltaChannel";

export interface InternalOperationChannel
  extends OperationChannel, WaveletDeltaChannelReceiver {
  /** Resets the channel ready for reconnection. */
  reset(): void;

  /**
   * Not in the original: the server's history was rewound to `version`;
   * `rewound` are the deltas received since. The caller reconnects next.
   */
  onRewind(
    version: HashedVersion,
    rewound: readonly TransformedWaveletDelta[],
  ): void;

  /** Closes this channel. */
  close(): void;
}
