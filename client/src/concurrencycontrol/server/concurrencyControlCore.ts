// The core of the concurrency control that lives on the server. It manages
// all of the transforms for a single wavelet that needs CC.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.server.ConcurrencyControlCore`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import { DeltaPair, type TransformedWaveletDelta, WaveletDelta } from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import { IllegalStateError, TransformException } from "../common";
import type { DeltaHistory } from "./deltaHistory";

/** The information needed to resend to a client that reopens a wavelet. */
export class ReOpenInfo {
  constructor(
    /** The starting signature for the client. */
    readonly startSignature: HashedVersion,
    /** The deltas to give to the client initially. */
    readonly deltas: readonly TransformedWaveletDelta[],
  ) {}
}

export class ConcurrencyControlCore {
  /** The delta history used for transformation. */
  private readonly deltaHistory: DeltaHistory;

  constructor(deltaHistory: DeltaHistory) {
    this.deltaHistory = deltaHistory;
  }

  /**
   * Transforms the given client delta against the known delta history.
   * Returns the transformed client delta, which targets the latest version.
   *
   * A client delta that equals the server delta at its target version (same
   * operations by the same creators) transforms to an empty delta.
   *
   * Throws {@link TransformException} if the delta targets a version newer
   * than the server's, the model's `WaveTransformError` if the operations
   * do not transform, and {@link IllegalStateError} if the history has no
   * delta starting at a version on the way.
   */
  onClientDelta(delta: WaveletDelta): WaveletDelta {
    if (delta.targetVersion.version > this.deltaHistory.getCurrentVersion()) {
      throw new TransformException(
        "Client has a newer version than server knows. client: " +
          `${delta.targetVersion}, server: ${this.deltaHistory.getCurrentVersion()}`,
      );
    }
    let result = delta;
    while (result.targetVersion.version < this.deltaHistory.getCurrentVersion()) {
      const serverDelta = this.deltaHistory.getDeltaStartingAt(result.targetVersion.version);
      if (serverDelta === null) {
        // This triggers if the available history changes out from under us.
        throw new IllegalStateError(`No delta at version: ${result.targetVersion}`);
      }
      const pair = new DeltaPair(result.ops, serverDelta.ops).transform();
      result = new WaveletDelta(delta.author, serverDelta.resultingVersion, pair.client);
    }
    return result;
  }

  /**
   * A client wants to reopen a wave and sends the signatures that it knows
   * of. Returns the deltas from the last of those signatures that the
   * history knows, or null if it knows none of them.
   */
  reopen(clientKnownSignatures: readonly HashedVersion[]): ReOpenInfo | null {
    const deltas: TransformedWaveletDelta[] = [];
    // Find the most recent delta.
    for (let i = clientKnownSignatures.length - 1; i >= 0; i--) {
      const signature = clientKnownSignatures[i] as HashedVersion;
      if (this.deltaHistory.hasSignature(signature)) {
        let old = this.deltaHistory.getDeltaStartingAt(signature.version);
        while (old !== null) {
          deltas.push(old);
          old = this.deltaHistory.getDeltaStartingAt(old.resultingVersion.version);
        }
        return new ReOpenInfo(signature, deltas);
      }
    }
    return null;
  }
}
