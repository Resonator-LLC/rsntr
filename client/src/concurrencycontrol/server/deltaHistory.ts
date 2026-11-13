// Access to the history of deltas of a wavelet as though it's a simple list.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.server.DeltaHistory`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { TransformedWaveletDelta } from "../../model/operation";
import type { HashedVersion } from "../../model/version";

export interface DeltaHistory {
  /** The version of the last signature. */
  getCurrentVersion(): number;

  /** The delta starting at the given version, or null if there is none. */
  getDeltaStartingAt(version: number): TransformedWaveletDelta | null;

  /**
   * Whether the given signature exists in the history.
   *
   * Knowing a signature does not mean there is a delta at that signature:
   * there is no delta for version 0 and for the most recent signature.
   */
  hasSignature(signatureInformation: HashedVersion): boolean;
}
