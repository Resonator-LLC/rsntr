// Receives the result of submitting a delta.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.SubmitCallback`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { HashedVersion } from "../../model/version";
import type { ResponseCode } from "../common";

export interface SubmitCallback {
  /**
   * Called when submission succeeds (an acknowledgment). `errorMessage` is
   * any error message in applying the ops, null if no error. Throws
   * `ChannelException` if the channel fails when processing the ack.
   */
  onSuccess(
    opsApplied: number,
    newVersion: HashedVersion,
    responseCode: ResponseCode,
    errorMessage: string | null,
  ): void;

  /**
   * Called when submission fails due to network or channel failure. Throws
   * `ChannelException` if the channel fails when processing the failure.
   */
  onFailure(reason: string): void;
}
