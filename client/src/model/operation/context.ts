// The context in which a wavelet operation is applied.
//
// Derived from `org.waveprotocol.wave.model.operation.wave.WaveletOperationContext`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { ParticipantId } from "../id";
import type { HashedVersion } from "../version";

/** The timestamp of a context without one (`Constants.NO_TIMESTAMP`). */
export const NO_TIMESTAMP = -1;

/**
 * Who performed an operation and when, and how the operation moves the
 * wavelet version. Immutable.
 */
export class WaveletOperationContext {
  readonly creator: ParticipantId;
  /** Milliseconds since the epoch, or {@link NO_TIMESTAMP}. */
  readonly timestamp: number;
  /** Negative for reverse operations. */
  readonly versionIncrement: number;
  /**
   * The hashed version the wavelet gets, as on the last operation of a
   * delta; otherwise null.
   */
  readonly hashedVersion: HashedVersion | null;

  constructor(
    creator: ParticipantId,
    timestamp: number,
    versionIncrement: number,
    hashedVersion: HashedVersion | null = null,
  ) {
    this.creator = creator;
    this.timestamp = timestamp;
    this.versionIncrement = versionIncrement;
    this.hashedVersion = hashedVersion;
  }

  /** A context that also sets the wavelet's hashed version. */
  static withHashedVersion(
    creator: ParticipantId,
    timestamp: number,
    versionIncrement: number,
    hashedVersion: HashedVersion,
  ): WaveletOperationContext {
    return new WaveletOperationContext(creator, timestamp, versionIncrement, hashedVersion);
  }

  hasTimestamp(): boolean {
    return this.timestamp !== NO_TIMESTAMP;
  }

  hasHashedVersion(): boolean {
    return this.hashedVersion !== null;
  }

  equals(other: WaveletOperationContext): boolean {
    return (
      this.creator.equals(other.creator) &&
      this.timestamp === other.timestamp &&
      this.versionIncrement === other.versionIncrement &&
      (this.hashedVersion === null
        ? other.hashedVersion === null
        : other.hashedVersion !== null && this.hashedVersion.equals(other.hashedVersion))
    );
  }

  toString(): string {
    return (
      `WaveletOperationContext(creator: ${this.creator}, timestamp: ${this.timestamp}, ` +
      `version increment: ${this.versionIncrement},hashedVersion: ` +
      `${this.hashedVersion === null ? "null" : this.hashedVersion.toString()})`
    );
  }
}
