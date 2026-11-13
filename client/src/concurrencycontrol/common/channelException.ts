// Failure within a communication channel or stack.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.common.ChannelException`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveId, WaveletId } from "../../model/id";
import type { Recoverable } from "./recoverable";
import { ResponseCode } from "./responseCode";

function isRecoverableValue(x: unknown): x is Recoverable {
  return x === "RECOVERABLE" || x === "NOT_RECOVERABLE";
}

export class ChannelException extends Error {
  /** Identifies the problem. */
  readonly responseCode: ResponseCode;
  /** Whether recovery by reconnection is possible. */
  readonly recoverable: Recoverable;
  /** The associated wave id, if any. */
  readonly waveId: WaveId | null;
  /** The associated wavelet id, if any. */
  readonly waveletId: WaveletId | null;

  /**
   * Without a response code the failure is an
   * {@link ResponseCode.INTERNAL_ERROR}: some sanity check failed or another
   * low-level problem occurred.
   */
  constructor(message: string, recoverable: Recoverable);
  constructor(
    responseCode: ResponseCode,
    message: string | null,
    cause: unknown,
    recoverable: Recoverable,
    waveId: WaveId | null,
    waveletId: WaveletId | null,
  );
  constructor(
    a: ResponseCode | string,
    b: string | null | Recoverable,
    cause?: unknown,
    recoverable?: Recoverable,
    waveId?: WaveId | null,
    waveletId?: WaveletId | null,
  ) {
    if (a instanceof ResponseCode) {
      const message = b as string | null;
      super(
        message ?? undefined,
        cause === null || cause === undefined ? undefined : { cause },
      );
      this.responseCode = a;
      this.recoverable = recoverable as Recoverable;
      this.waveId = waveId ?? null;
      this.waveletId = waveletId ?? null;
    } else {
      if (!isRecoverableValue(b)) {
        throw new TypeError("ChannelException(message, recoverable): bad recoverable");
      }
      super(a);
      this.responseCode = ResponseCode.INTERNAL_ERROR;
      this.recoverable = b;
      this.waveId = null;
      this.waveletId = null;
    }
    this.name = "ChannelException";
  }

  getRecoverable(): Recoverable {
    return this.recoverable;
  }

  getWaveId(): WaveId | null {
    return this.waveId;
  }

  getWaveletId(): WaveletId | null {
    return this.waveletId;
  }

  getResponseCode(): ResponseCode {
    return this.responseCode;
  }

  override toString(): string {
    // Space before \n in case some logger swallows the newline.
    return (
      `${this.name}: ${this.message}, \nwaveId: ${this.waveId} waveletId: ${this.waveletId}` +
      `, isRecoverable: ${this.recoverable}, responseCode: ${this.responseCode}`
    );
  }
}
