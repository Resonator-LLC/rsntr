// Carrying document locations (cursors, selections, anything a UI remembers
// as an item offset) across a document operation.
//
// The idea is that of `org.waveprotocol.wave.model.document.util.LocationMapper`
// and the editor's selection maintenance in Apache Wave (Apache License
// 2.0); the code is new.

import type { DocOp } from "../model/docop";

/**
 * Where a location lands when the operation inserts exactly at it:
 * `"before"` keeps it in front of the inserted content (a selection end, or
 * a cursor that should not be pushed by a remote user typing at the same
 * spot), `"after"` moves it behind (the local cursor after local typing).
 */
export type LocationBias = "before" | "after";

/**
 * The location in the document after `op` that corresponds to `location`
 * in the document before it. Locations are item offsets (a character is one
 * UTF-16 code unit, an element start or end one item). A location inside a
 * deleted range collapses to the start of the deletion.
 */
export function transformLocation(
  location: number,
  op: DocOp,
  bias: LocationBias = "before",
): number {
  // Position in the document before the op, and the shift accumulated.
  let before = 0;
  let shift = 0;
  for (const component of op.components) {
    switch (component.type) {
      case "characters":
      case "elementStart":
      case "elementEnd": {
        const length = component.type === "characters" ? component.characters.length : 1;
        if (before < location || (before === location && bias === "after")) {
          shift += length;
        }
        break;
      }
      case "deleteCharacters":
      case "deleteElementStart":
      case "deleteElementEnd": {
        const length =
          component.type === "deleteCharacters" ? component.characters.length : 1;
        if (location >= before + length) {
          shift -= length;
        } else if (location > before) {
          shift -= location - before;
        }
        before += length;
        break;
      }
      case "retain":
        before += component.itemCount;
        break;
      case "replaceAttributes":
      case "updateAttributes":
        before += 1;
        break;
      case "annotationBoundary":
        break;
    }
    if (before > location) {
      break;
    }
  }
  return location + shift;
}

/** A range of locations, e.g. a selection; `start <= end`. */
export interface LocationRange {
  readonly start: number;
  readonly end: number;
}

/**
 * Carries a range across an operation: content inserted at its start stays
 * outside it, as does content inserted at its end; a collapsed range (a
 * cursor) follows `cursorBias`.
 */
export function transformRange(
  range: LocationRange,
  op: DocOp,
  cursorBias: LocationBias = "before",
): LocationRange {
  if (range.start === range.end) {
    const at = transformLocation(range.start, op, cursorBias);
    return { start: at, end: at };
  }
  const start = transformLocation(range.start, op, "after");
  const end = transformLocation(range.end, op, "before");
  return { start, end: Math.max(start, end) };
}
