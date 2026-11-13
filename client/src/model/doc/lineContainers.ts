// Lines of text in a blip body (`LineContainers`).
//
// A blip's text lives in a line container, the `body` element, as a
// sequence of empty `line` elements each followed by that line's content.
// The original lets clients register further line container tag names; only
// `body` is one here.
//
// TypeScript port of the Rust crate `resonator-model` (module
// `doc::line_containers`), itself derived from Apache Wave
// (`util.LineContainers`), licensed under the Apache License, Version 2.0.

import { Attributes } from "../docop";
import { DocError } from "./error";
import type { MutableDocument } from "./mutable";
import type { NodeId } from "./view";
import { XmlFragment } from "./xmlFragment";

export const LINE_TAGNAME = "line";

/**
 * The tag name of the top-level line container
 * (`LineContainers.topLevelContainerTagname`, set by `Blips`).
 */
export const TOP_LEVEL_CONTAINER_TAGNAME = "body";

/** `LineContainers.isLineContainerTagname`. */
export function isLineContainerTagname(tag: string): boolean {
  return tag === TOP_LEVEL_CONTAINER_TAGNAME;
}

function lastLineContainer(doc: MutableDocument): NodeId | null {
  const view = doc.view();
  const elements = view.descendantElements(view.root());
  for (let i = elements.length - 1; i >= 0; i--) {
    const element = elements[i]!;
    const tag = view.tagName(element);
    if (tag !== null && isLineContainerTagname(tag)) {
      return element;
    }
  }
  return null;
}

function elementStartingAt(doc: MutableDocument, location: number): NodeId {
  const element = doc.view().elementStartingAt(location);
  if (element === null) {
    throw DocError.invalidNode();
  }
  return element;
}

/**
 * Appends a line with content to the last line container, creating
 * `<body><line/></body>` if the document has none, and returns the new line
 * element (`LineContainers.appendLine`).
 *
 * As in the original, when the container had to be created the line's
 * attributes are set with a separate replace-attributes operation, even if
 * there are none (but not if `attributes` is `null`).
 */
export function appendLine(
  doc: MutableDocument,
  content: XmlFragment | null = null,
  attributes: Attributes | null = null,
): NodeId {
  const filled = content !== null && content.itemCount() > 0 ? content : null;
  const container = lastLineContainer(doc);
  if (container === null) {
    const initial = XmlFragment.empty().wrap(LINE_TAGNAME).wrap(TOP_LEVEL_CONTAINER_TAGNAME);
    const containerLocation = doc.size();
    doc.appendXml(initial);
    if (filled !== null) {
      // The end of the new container: after <body><line></line>.
      doc.insertXml(containerLocation + 3, filled);
    }
    const line = elementStartingAt(doc, containerLocation + 1);
    if (attributes !== null) {
      doc.setElementAttributes(line, attributes);
    }
    return elementStartingAt(doc, containerLocation + 1);
  }
  const pairs = (attributes ?? Attributes.EMPTY).entries.map((a) => [a.name, a.value] as const);
  const lineLocation = doc.view().contentEnd(container);
  doc.createElement(lineLocation, LINE_TAGNAME, pairs);
  if (filled !== null) {
    doc.insertXml(lineLocation + 2, filled);
  }
  return elementStartingAt(doc, lineLocation);
}

/**
 * Appends content to the end of the last line container, or as a new line
 * if there is no container (`LineContainers.appendToLastLine`).
 */
export function appendToLastLine(doc: MutableDocument, content: XmlFragment): void {
  const container = lastLineContainer(doc);
  if (container !== null) {
    doc.insertXml(doc.view().contentEnd(container), content);
  } else {
    appendLine(doc, content, Attributes.EMPTY);
  }
}
