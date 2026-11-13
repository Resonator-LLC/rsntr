// Helpers for tests of the concurrency control stack.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.testing.CcTestingUtils`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { WaveletDelta } from "../../model/operation";

/**
 * Whether two deltas have equal target versions and operations. The authors
 * are not compared.
 */
export function deltasAreEqual(delta1: WaveletDelta | null, delta2: WaveletDelta | null): boolean {
  if (delta1 === delta2) return true;
  if (delta1 === null || delta2 === null) return false;
  if (delta1.length !== delta2.length) return false;
  const v1 = delta1.targetVersion as typeof delta1.targetVersion | null;
  const v2 = delta2.targetVersion as typeof delta2.targetVersion | null;
  if (v1 === null || v2 === null ? v1 !== v2 : !v1.equals(v2)) return false;
  return delta1.ops.every((op, i) => op.equals(delta2.ops[i]!));
}

export const CcTestingUtils = { deltasAreEqual } as const;
