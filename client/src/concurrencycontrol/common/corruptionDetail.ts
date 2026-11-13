// Information about a turbulence or related phenomenon (such as an operation
// channel shutdown).
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.common.CorruptionDetail`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { ResponseCode } from "./responseCode";

/**
 * An error, so that it can be logged and reveals the stack trace of where it
 * originated. Its `message` is `[<code>] <errorMessage>`.
 */
export class CorruptionDetail extends Error {
  readonly responseCode: ResponseCode;
  /** The message without the response code prefix. */
  readonly errorMessage: string;

  constructor(responseCode: ResponseCode, message: string, cause: unknown = null) {
    super(`[${responseCode}] ${message}`, cause === null || cause === undefined ? undefined : { cause });
    this.name = "CorruptionDetail";
    this.responseCode = responseCode;
    this.errorMessage = message;
  }

  /** The Java name of {@link CorruptionDetail.responseCode}. */
  getErrorCode(): ResponseCode {
    return this.responseCode;
  }
}
