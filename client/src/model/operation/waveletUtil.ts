// Building wavelet state from deltas (`WaveletDataUtil`).
//
// Derived from `org.waveprotocol.box.server.util.WaveletDataUtil` of Apache
// Wave, licensed under the Apache License, Version 2.0.

import { type BlipData, DataError, WaveletData } from "../data";
import { DocInitialization, DocOp } from "../docop";
import type { ParticipantId, WaveletName } from "../id";
import { HashedVersion } from "../version";
import type { TransformedWaveletDelta } from "./delta";
import { WaveletOperationError } from "./error";
import type { WaveletOperation } from "./waveOp";

/**
 * Applies a delta to a wavelet at the delta's applied-at version. If an
 * operation fails, the operations before it are rolled back with their
 * reverse operations and a {@link WaveletOperationError} of kind
 * `"deltaFailed"` is thrown.
 *
 * The rollback is only as exact as the reverse operations are; see
 * {@link WaveletOperation.applyAndReturnReverse}.
 */
export function applyWaveletDelta(delta: TransformedWaveletDelta, wavelet: WaveletData): void {
  if (delta.appliedAtVersion !== wavelet.version) {
    throw new WaveletOperationError({
      kind: "deltaVersionMismatch",
      deltaVersion: delta.appliedAtVersion,
      waveletVersion: wavelet.version,
    });
  }
  const reverseOps: WaveletOperation[] = [];
  let applied = 0;
  for (const op of delta.ops) {
    try {
      reverseOps.push(...op.applyAndReturnReverse(wavelet));
    } catch (cause) {
      if (!(cause instanceof WaveletOperationError)) {
        throw cause;
      }
      // Deltas are atomic.
      for (let i = reverseOps.length - 1; i >= 0; i--) {
        const inverse = reverseOps[i]!;
        try {
          inverse.apply(wavelet);
        } catch (rollbackCause) {
          if (!(rollbackCause instanceof WaveletOperationError)) {
            throw rollbackCause;
          }
          throw new WaveletOperationError({
            kind: "rollbackFailed",
            inverse: inverse.toString(),
            cause: rollbackCause,
          });
        }
      }
      throw new WaveletOperationError({
        kind: "deltaFailed",
        applied,
        total: delta.length,
        version: wavelet.version,
        failedOp: op.toString(),
        cause,
      });
    }
    applied += 1;
  }
}

/**
 * Creates a wavelet from its first delta, which must apply at version zero.
 * The delta's author becomes the creator and its timestamp the creation
 * time.
 */
export function buildWaveletFromFirstDelta(
  waveletName: WaveletName,
  delta: TransformedWaveletDelta,
): WaveletData {
  if (delta.appliedAtVersion !== 0) {
    throw WaveletOperationError.invalidArgument(
      `first delta has non-zero version: ${delta.appliedAtVersion}`,
    );
  }
  const wavelet = WaveletData.empty(
    waveletName,
    delta.author,
    // Overwritten by the delta.
    HashedVersion.unsigned(0),
    delta.applicationTimestamp,
  );
  applyWaveletDelta(delta, wavelet);
  return wavelet;
}

/**
 * Creates a wavelet by applying a non-empty, contiguous sequence of deltas
 * starting at version zero.
 */
export function buildWaveletFromDeltas(
  waveletName: WaveletName,
  deltas: Iterable<TransformedWaveletDelta>,
): WaveletData {
  let wavelet: WaveletData | null = null;
  for (const delta of deltas) {
    if (wavelet === null) {
      wavelet = buildWaveletFromFirstDelta(waveletName, delta);
    } else {
      applyWaveletDelta(delta, wavelet);
    }
  }
  if (wavelet === null) {
    throw WaveletOperationError.invalidArgument("empty deltas");
  }
  return wavelet;
}

/**
 * Adds an empty document authored by `author`. As in the original, `time`
 * is used for both the last-modified time and the last-modified version.
 */
export function addEmptyBlip(
  wavelet: WaveletData,
  blipId: string,
  author: ParticipantId,
  time: number,
): BlipData {
  if (time < 0) {
    throw WaveletOperationError.invalidArgument(`negative time ${time}`);
  }
  try {
    return wavelet.createDocument(
      blipId,
      author,
      [author],
      DocInitialization.fromOp(DocOp.EMPTY),
      time,
      time,
    );
  } catch (e) {
    if (e instanceof DataError) {
      throw new WaveletOperationError({ kind: "data", error: e });
    }
    throw e;
  }
}
