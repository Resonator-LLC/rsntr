// Inversion of document operations (`DocOpInverter`).
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import { DocOp, type DocOpComponent } from "./op";

/**
 * The operation that undoes `op` when applied to the document `op` produces.
 * Fails if the result is ill-formed.
 */
export function invert(op: DocOp): DocOp {
  return DocOp.create(op.components.map(invertComponent));
}

function invertComponent(c: DocOpComponent): DocOpComponent {
  switch (c.type) {
    case "retain":
      return c;
    case "characters":
      return { type: "deleteCharacters", characters: c.characters };
    case "elementStart":
      return { type: "deleteElementStart", tag: c.tag, attributes: c.attributes };
    case "elementEnd":
      return { type: "deleteElementEnd" };
    case "deleteCharacters":
      return { type: "characters", characters: c.characters };
    case "deleteElementStart":
      return { type: "elementStart", tag: c.tag, attributes: c.attributes };
    case "deleteElementEnd":
      return { type: "elementEnd" };
    case "replaceAttributes":
      return {
        type: "replaceAttributes",
        oldAttributes: c.newAttributes,
        newAttributes: c.oldAttributes,
      };
    case "updateAttributes":
      return { type: "updateAttributes", update: c.update.invert() };
    case "annotationBoundary":
      return { type: "annotationBoundary", boundary: c.boundary.invert() };
  }
}
