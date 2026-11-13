// Inverses of wavelet operations, without the wavelet they applied to.
//
// Not in the original: used when a wavelet's history is rewound (a change
// of host epoch), so that a client can take back operations it has applied.
// Unlike the reverse operations `applyOperationAndReturnReverse` makes, an
// inverse made here knows nothing of the state the operation applied to:
// a removed participant comes back at the end of the list, and blip
// metadata (contributors, versions) is not restored.

import { invert } from "../docop";
import type { HashedVersion } from "../version";
import { WaveletOperationContext } from "./context";
import { BlipOperation, WaveletOperation } from "./waveOp";

/**
 * The inverse of `op`. Its context has the same creator and timestamp and
 * the negated version increment, and sets the wavelet's hashed version to
 * `hashedVersion` when given.
 */
export function invertOperation(
  op: WaveletOperation,
  hashedVersion: HashedVersion | null = null,
): WaveletOperation {
  const c = op.context;
  const context = new WaveletOperationContext(
    c.creator,
    c.timestamp,
    -c.versionIncrement,
    hashedVersion,
  );
  const kind = op.kind;
  switch (kind.type) {
    case "noOp":
      return WaveletOperation.noOp(context);
    case "addParticipant":
      return WaveletOperation.removeParticipant(context, kind.participant);
    case "removeParticipant":
      return WaveletOperation.addParticipant(context, kind.participant);
    case "blip": {
      const blipKind = kind.op.kind;
      if (blipKind.type === "content") {
        return WaveletOperation.blip(
          context,
          kind.blipId,
          BlipOperation.content(invert(blipKind.op), "none"),
        );
      }
      return WaveletOperation.blip(context, kind.blipId, BlipOperation.SUBMIT);
    }
    case "versionUpdate":
      return WaveletOperation.noOp(context);
  }
}

/**
 * The operations that take back a run of deltas: each delta's operations
 * inverted, last first. The last sets the wavelet's hashed version to
 * `start`, where the deltas started.
 */
export function invertDeltas(
  deltas: readonly { readonly ops: readonly WaveletOperation[] }[],
  start: HashedVersion,
): WaveletOperation[] {
  const ops = deltas.flatMap((delta) => delta.ops);
  const inverse: WaveletOperation[] = [];
  for (let i = ops.length - 1; i >= 0; i--) {
    inverse.push(invertOperation(ops[i]!, i === 0 ? start : null));
  }
  return inverse;
}
