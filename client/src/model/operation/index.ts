// Wavelet operations, deltas and their transformation.
//
// A TypeScript port of the Rust crate `resonator-model` (module `operation`),
// itself derived from `org.waveprotocol.wave.model.operation.wave`,
// `org.waveprotocol.wave.concurrencycontrol.common.DeltaPair` and the pure
// parts of `org.waveprotocol.box.server.waveserver.WaveletContainerImpl`
// and `org.waveprotocol.box.server.util.WaveletDataUtil` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { type DocInitialization, type DocOp, toXmlString } from "../docop";

export { NO_TIMESTAMP, WaveletOperationContext } from "./context";
export { TransformedWaveletDelta, WaveletDelta } from "./delta";
export { invertDeltas, invertOperation } from "./invert";
export {
  DeltaError,
  SubmitTransformError,
  WaveTransformError,
  WaveletOperationError,
  type DeltaErrorDetail,
  type SubmitTransformErrorDetail,
  type WaveTransformErrorDetail,
  type WaveletOperationErrorDetail,
} from "./error";
export {
  DeltaPair,
  transform,
  transformBlipOps,
  transformOpLists,
  transformOps,
  transformSubmittedDelta,
  type OperationPair,
} from "./transform";
export {
  BlipOperation,
  WaveletOperation,
  type BlipOperationKind,
  type UpdateContributorMethod,
  type WaveletOperationKind,
} from "./waveOp";
export {
  addEmptyBlip,
  applyWaveletDelta,
  buildWaveletFromDeltas,
  buildWaveletFromFirstDelta,
} from "./waveletUtil";
export { THREAD_INLINE_ANCHOR_TAGNAME, isBlipIdWorthy, isWorthy } from "./worthy";

/**
 * Syntactic equality of document operations
 * (`OpComparators.SYNTACTIC_IDENTITY`); the same as `DocOp.equals`.
 */
export function syntacticIdentity(a: DocOp, b: DocOp): boolean {
  return a.equals(b);
}

/**
 * Whether two initializations describe the same document, comparing their
 * XML forms (`OpComparators.equalDocuments`).
 */
export function equalDocuments(a: DocInitialization, b: DocInitialization): boolean {
  try {
    return toXmlString(a) === toXmlString(b);
  } catch {
    return false;
  }
}
