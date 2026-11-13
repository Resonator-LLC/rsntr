// A list stored as one element per item (`DocumentBasedElementList`).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased`
// (`DocumentBasedElementList`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { DocView, MutableDocument, NodeId } from "../doc";
import { AdtError } from "./error";
import type { InitialAttributes } from "./initializer";
import { locate, type Locator } from "./locator";

/**
 * A list whose items are the child elements of a container with a given
 * tag name, in document order. Other children are ignored.
 *
 * The original adapts every element to a value object through a `Factory`
 * and identifies items by those objects. Here an item is identified by its
 * index, `elements` gives the elements to read values from, and a new item
 * is created with the attributes an `Initializer` built.
 */
export class DocumentBasedElementList {
  constructor(private readonly childTag: string) {}

  /** The elements of the items, in order (`getValues`). */
  elements(view: DocView, container: NodeId): NodeId[] {
    return view.childElements(container).filter((element) => view.hasTag(element, this.childTag));
  }

  /** The items, each adapted from its element. */
  values<T>(view: DocView, container: NodeId, adapt: (view: DocView, element: NodeId) => T): T[] {
    return this.elements(view, container).map((element) => adapt(view, element));
  }

  /** The element of the item at an index (`get`), or `null`. */
  get(view: DocView, container: NodeId, index: number): NodeId | null {
    return this.elements(view, container)[index] ?? null;
  }

  /** The index of the item an element stands for (`indexOf`), or `null`. */
  indexOf(view: DocView, container: NodeId, element: NodeId): number | null {
    const index = this.elements(view, container).indexOf(element);
    return index < 0 ? null : index;
  }

  size(view: DocView, container: NodeId): number {
    return this.elements(view, container).length;
  }

  /**
   * Appends an item, as the last child of the container, and returns its
   * element (`add(I)`).
   */
  add(doc: MutableDocument, container: Locator, attributes: InitialAttributes): NodeId {
    const node = locate(container, doc.view());
    return doc.createChildElement(node, this.childTag, attributes);
  }

  /**
   * Inserts an item so that it gets an index, just before the element of
   * the item that has that index now, and returns its element
   * (`add(int, I)`). Throws {@link AdtError} `indexOutOfBounds`.
   */
  addAt(
    doc: MutableDocument,
    container: Locator,
    index: number,
    attributes: InitialAttributes,
  ): NodeId {
    const node = locate(container, doc.view());
    const elements = this.elements(doc.view(), node);
    if (!Number.isInteger(index) || index < 0 || index > elements.length) {
      throw AdtError.indexOutOfBounds(index, elements.length);
    }
    const after = elements[index];
    if (after === undefined) {
      return this.add(doc, container, attributes);
    }
    return doc.createElement(doc.view().location(after), this.childTag, attributes);
  }

  /**
   * Deletes the item at an index with its element. Returns false if there
   * is no such item.
   */
  remove(doc: MutableDocument, container: Locator, index: number): boolean {
    const node = locate(container, doc.view());
    const element = this.get(doc.view(), node, index);
    if (element === null) {
      return false;
    }
    doc.deleteNode(element);
    return true;
  }

  /** Deletes all items, first to last, one operation each. */
  clear(doc: MutableDocument, container: Locator): void {
    while (this.remove(doc, container, 0)) {
      // Until the list is empty.
    }
  }
}
