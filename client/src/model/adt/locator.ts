// Naming the element that holds an embedding, and helpers shared by the
// document-based data types (`Locator`, `Root`, `FirstTopLevel`,
// `delete_nodes` in the Rust `adt/mod.rs`).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { DocError, type DocView, type MutableDocument, type NodeId } from "../doc";
import { AdtError } from "./error";

/**
 * Finds the element that holds an embedding in the current tree of a
 * document. Stands in for the element object the original types keep: a
 * node of a `DocView` does not survive an edit.
 */
export type Locator = (view: DocView) => NodeId | null;

/** The document itself (the original's document element). */
export const ROOT: Locator = (view) => view.root();

/** The first top-level element with a tag name. */
export function firstTopLevel(tag: string): Locator {
  return (view) => view.childElements(view.root()).find((child) => view.hasTag(child, tag)) ?? null;
}

/** The located element; throws {@link AdtError} `missingContainer`. */
export function locate(container: Locator, view: DocView): NodeId {
  const node = container(view);
  if (node === null || !view.isElement(node)) {
    throw AdtError.missingContainer();
  }
  return node;
}

/**
 * Deletes elements of the current tree, none inside another, one edit
 * each, in the order given.
 */
export function deleteNodes(doc: MutableDocument, nodes: readonly NodeId[]): void {
  const spans = nodes.map((node) => ({
    location: doc.view().location(node),
    size: doc.view().itemSize(node),
  }));
  spans.forEach(({ location }, index) => {
    // Earlier deletions to the left have shifted this element.
    let shift = 0;
    for (const earlier of spans.slice(0, index)) {
      if (earlier.location < location) {
        shift += earlier.size;
      }
    }
    const node = doc.view().elementStartingAt(location - shift);
    if (node === null) {
      throw DocError.invalidNode();
    }
    doc.deleteNode(node);
  });
}

/** The elements of the current tree in document order. */
export function inDocumentOrder(view: DocView, nodes: readonly NodeId[]): NodeId[] {
  return [...nodes].sort((a, b) => view.location(a) - view.location(b));
}

/** The nodes now starting at the locations recorded before an edit. */
export function elementsAt(view: DocView, locations: readonly number[]): NodeId[] {
  const found: NodeId[] = [];
  for (const location of locations) {
    const node = view.elementStartingAt(location);
    if (node !== null) {
      found.push(node);
    }
  }
  return found;
}
