// Decides whether a change is worth attributing to its author
// (`WorthyChangeChecker`).
//
// Derived from `org.waveprotocol.wave.model.operation.wave` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import type { DocOp } from "../docop";

/**
 * Tag of the inline reply anchor element, whose insertion and deletion are
 * not worthy changes.
 */
export const THREAD_INLINE_ANCHOR_TAGNAME = "reply";

/**
 * Annotation key prefixes whose changes are not worthy: user, spelling,
 * link, translation and language annotations.
 */
const UNWORTHY_ANNOTATION_PREFIXES: readonly string[] = ["user", "spell", "link", "tr", "lang"];

/**
 * Whether a document operation is worth attributing to its author, i.e.
 * whether it makes its author a contributor and updates the blip's
 * last-modified metadata. Retains, reply anchors and changes to bookkeeping
 * annotations are not.
 */
export function isWorthy(op: DocOp): boolean {
  return op.components.some((component) => {
    switch (component.type) {
      case "retain":
      case "elementEnd":
      case "deleteElementEnd":
        return false;
      case "characters":
      case "deleteCharacters":
      case "replaceAttributes":
      case "updateAttributes":
        return true;
      case "elementStart":
      case "deleteElementStart":
        return component.tag !== THREAD_INLINE_ANCHOR_TAGNAME;
      case "annotationBoundary":
        return component.boundary.changes.some(
          (change) =>
            change.oldValue !== change.newValue &&
            !UNWORTHY_ANNOTATION_PREFIXES.some((prefix) => change.key.startsWith(prefix)),
        );
    }
  });
}

/**
 * Whether changes to the document with this id are attributed at all: false
 * for attachment, `mini` and translation documents.
 */
export function isBlipIdWorthy(docId: string): boolean {
  return !(docId.startsWith("attach") || docId === "mini" || docId.startsWith("tr+"));
}
