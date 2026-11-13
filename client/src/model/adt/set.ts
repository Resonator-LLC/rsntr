// A set stored as one element per value (`DocumentBasedBasicSet`).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased`
// (`DocumentBasedBasicSet`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { DocView, MutableDocument, NodeId } from "../doc";
import { deleteNodes, elementsAt, inDocumentOrder, locate, type Locator } from "./locator";
import type { Serializer } from "./serializer";

/**
 * A set stored as one element per value. Of several elements with the same
 * value the first in the document counts and the others are redundant.
 *
 *     <container>
 *       <entry value="3"/>
 *       <entry value="12"/>
 *       <entry value="12"/>      redundant
 *     </container>
 *
 * Adding a value appends an element to the container. Every change to the
 * set also deletes the redundant elements.
 */
export class DocumentBasedBasicSet<T> {
  constructor(
    private readonly entryTag: string,
    private readonly valueAttr: string,
    private readonly serializer: Serializer<T>,
  ) {}

  /**
   * The values with their elements, ordered by value, and the redundant
   * elements.
   */
  private load(
    view: DocView,
    container: NodeId,
  ): { values: { value: T; element: NodeId }[]; obsolete: NodeId[] } {
    const values: { value: T; element: NodeId }[] = [];
    const obsolete: NodeId[] = [];
    for (const element of view.childElements(container)) {
      if (!view.hasTag(element, this.entryTag)) {
        continue;
      }
      const attribute = view.attribute(element, this.valueAttr);
      const value = attribute === null ? null : this.serializer.fromAttribute(attribute);
      if (value === null) {
        continue;
      }
      if (values.some((known) => this.serializer.equals(known.value, value))) {
        obsolete.push(element);
      } else {
        values.push({ value, element });
      }
    }
    values.sort((a, b) => this.serializer.compare(a.value, b.value));
    return { values, obsolete };
  }

  /** The values, in order (`getValues`). */
  values(view: DocView, container: NodeId): T[] {
    return this.load(view, container).values.map(({ value }) => value);
  }

  contains(view: DocView, container: NodeId, value: T): boolean {
    return this.load(view, container).values.some((known) =>
      this.serializer.equals(known.value, value),
    );
  }

  /** Adds a value unless present. Returns whether the document was changed. */
  add(doc: MutableDocument, container: Locator, value: T): boolean {
    const node = locate(container, doc.view());
    const { values, obsolete } = this.load(doc.view(), node);
    if (values.some((known) => this.serializer.equals(known.value, value))) {
      return false;
    }
    const attribute = this.serializer.toAttribute(value);
    const locations = obsolete.map((element) => doc.view().location(element));
    doc.createChildElement(node, this.entryTag, [[this.valueAttr, attribute]]);
    deleteNodes(doc, elementsAt(doc.view(), locations));
    return true;
  }

  /**
   * Removes a value if present. Returns whether the document was changed.
   */
  remove(doc: MutableDocument, container: Locator, value: T): boolean {
    const node = locate(container, doc.view());
    const { values, obsolete } = this.load(doc.view(), node);
    const found = values.find((known) => this.serializer.equals(known.value, value));
    if (found === undefined) {
      return false;
    }
    const doomed = inDocumentOrder(doc.view(), obsolete);
    doomed.push(found.element);
    deleteNodes(doc, doomed);
    return true;
  }

  /**
   * Removes all values, the redundant elements first, then the values in
   * ascending order. Returns whether the document was changed.
   */
  clear(doc: MutableDocument, container: Locator): boolean {
    const node = locate(container, doc.view());
    const { values, obsolete } = this.load(doc.view(), node);
    if (values.length === 0) {
      return false;
    }
    const doomed = inDocumentOrder(doc.view(), obsolete);
    doomed.push(...values.map(({ element }) => element));
    deleteNodes(doc, doomed);
    return true;
  }
}
