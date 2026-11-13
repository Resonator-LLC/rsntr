// A simple in-memory delta history.
//
// Derived from the test class
// `org.waveprotocol.wave.concurrencycontrol.client.SimpleDeltaHistory` of
// Apache Wave, licensed under the Apache License, Version 2.0. Kept with the
// server core because simulated servers need it too.

import type { TransformedWaveletDelta } from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import { IllegalArgumentError, IllegalStateError } from "../common";
import type { DeltaHistory } from "./deltaHistory";

export class SimpleDeltaHistory implements DeltaHistory {
  private currentSignature: HashedVersion;

  /** Maps applied-at version -> delta. */
  private readonly history = new Map<number, TransformedWaveletDelta>();

  /**
   * All the signatures we have, by version. Knowing a signature does not
   * mean we have a delta at that signature, e.g. for the most recent one.
   */
  private readonly signatures = new Map<number, HashedVersion>();

  /** A history always starts at version 0. */
  constructor(v0: HashedVersion) {
    if (v0.version !== 0) {
      throw new IllegalArgumentError("Delta history must start at zero");
    }
    this.currentSignature = v0;
    // We always have a signature at version 0, but we have no operation for it.
    this.signatures.set(0, v0);
  }

  getDeltaStartingAt(version: number): TransformedWaveletDelta | null {
    return this.history.get(version) ?? null;
  }

  hasSignature(signatureInformation: HashedVersion): boolean {
    const found = this.signatures.get(signatureInformation.version);
    return found !== undefined && found.equals(signatureInformation);
  }

  /** The last known signature. */
  getCurrentSignature(): HashedVersion {
    return this.currentSignature;
  }

  getCurrentVersion(): number {
    return this.currentSignature.version;
  }

  /** Makes the given signature, which must be a known one, the current one. */
  setCurrentSignature(currentSignature: HashedVersion): void {
    const known = this.signatures.get(currentSignature.version);
    if (known === undefined || !known.equals(currentSignature)) {
      throw new IllegalStateError(`Unknown signature ${currentSignature}`);
    }
    this.currentSignature = currentSignature;
  }

  /**
   * Adds a delta and its resulting signature. Does not change the current
   * signature.
   *
   * Unlike the original, an empty delta is not stored as the delta starting
   * at its version (only its signature is): it would be its own successor
   * and make walking the history loop forever.
   */
  addDelta(delta: TransformedWaveletDelta): void {
    if (!delta.isEmpty()) {
      this.history.set(delta.appliedAtVersion, delta);
    }
    this.signatures.set(delta.resultingVersion.version, delta.resultingVersion);
  }

  /** {@link addDelta} and makes the delta's resulting version the current signature. */
  appendDelta(delta: TransformedWaveletDelta): void {
    this.addDelta(delta);
    this.setCurrentSignature(delta.resultingVersion);
  }

  /** Removes a delta and its resulting signature. */
  removeDelta(delta: TransformedWaveletDelta): void {
    this.history.delete(delta.appliedAtVersion);
    this.signatures.delete(delta.resultingVersion.version);
  }

  /** The signature at the given version, or null. */
  getSignatureAt(version: number): HashedVersion | null {
    return this.signatures.get(version) ?? null;
  }

  /**
   * Truncates to the given version: forgets the deltas starting there and
   * after. Throws {@link IllegalStateError} if there is no signature at
   * that version.
   */
  truncateAt(version: number): void {
    const signature = this.getSignatureAt(version);
    if (signature === null) {
      throw new IllegalStateError(`No signature at version ${version}`);
    }
    this.setCurrentSignature(signature);
    let currentDelta = this.getDeltaStartingAt(version);
    while (currentDelta !== null) {
      this.removeDelta(currentDelta);
      currentDelta = this.getDeltaStartingAt(currentDelta.resultingVersion.version);
    }
  }
}
