// Response codes of the wave server.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.common.ResponseCode`
// of Apache Wave, licensed under the Apache License, Version 2.0.

/** An enum-like class: the members are the static fields. */
export class ResponseCode {
  /** Unknown response code. */
  static readonly UNKNOWN = new ResponseCode("UNKNOWN", -1);
  /** The delta was successfully transformed and applied. */
  static readonly OK = new ResponseCode("OK", 0);
  /** The WaveletSubmitRequest was ill-formed. */
  static readonly BAD_REQUEST = new ResponseCode("BAD_REQUEST", 1);
  /** An internal error occurred. */
  static readonly INTERNAL_ERROR = new ResponseCode("INTERNAL_ERROR", 2);
  /** The verifiedUserId or delta.userId were rejected by access control. */
  static readonly NOT_AUTHORIZED = new ResponseCode("NOT_AUTHORIZED", 3);
  /** The delta.version and delta.preSignature didn't match a point in the wavelet history. */
  static readonly VERSION_ERROR = new ResponseCode("VERSION_ERROR", 4);
  /** An operation in the delta was invalid (before, during, or after transformation). */
  static readonly INVALID_OPERATION = new ResponseCode("INVALID_OPERATION", 5);
  /** An operation in the delta didn't preserve the document schema. */
  static readonly SCHEMA_VIOLATION = new ResponseCode("SCHEMA_VIOLATION", 6);
  /** The delta is too big or the resulting document count or size is too big. */
  static readonly SIZE_LIMIT_EXCEEDED = new ResponseCode("SIZE_LIMIT_EXCEEDED", 7);
  /** An operation was rejected by the namespace policer. */
  static readonly POLICY_VIOLATION = new ResponseCode("POLICY_VIOLATION", 8);
  /** The object is unavailable because it has been quarantined. */
  static readonly QUARANTINED = new ResponseCode("QUARANTINED", 9);
  /**
   * The version the delta was being submitted against is too old.
   * The client should reconnect, transform, and retry.
   */
  static readonly TOO_OLD = new ResponseCode("TOO_OLD", 10);

  private static readonly VALUES: readonly ResponseCode[] = [
    ResponseCode.UNKNOWN,
    ResponseCode.OK,
    ResponseCode.BAD_REQUEST,
    ResponseCode.INTERNAL_ERROR,
    ResponseCode.NOT_AUTHORIZED,
    ResponseCode.VERSION_ERROR,
    ResponseCode.INVALID_OPERATION,
    ResponseCode.SCHEMA_VIOLATION,
    ResponseCode.SIZE_LIMIT_EXCEEDED,
    ResponseCode.POLICY_VIOLATION,
    ResponseCode.QUARANTINED,
    ResponseCode.TOO_OLD,
  ];

  private constructor(
    /** The Java enum constant name. */
    readonly name: string,
    readonly value: number,
  ) {}

  /** All response codes, in declaration order. */
  static values(): readonly ResponseCode[] {
    return ResponseCode.VALUES;
  }

  getValue(): number {
    return this.value;
  }

  /**
   * The response code whose value is the given integer. Throws `RangeError`
   * if there is none (`IndexOutOfBoundsException` in the original).
   */
  static of(value: number): ResponseCode {
    // The response codes have consecutive integer values beginning with -1.
    const code = Number.isInteger(value) ? ResponseCode.VALUES[value + 1] : undefined;
    if (code === undefined) {
      throw new RangeError(`No response code with value ${value}`);
    }
    return code;
  }

  toString(): string {
    return this.name;
  }
}
