// A value stored in at most one element (`DocumentBasedSingleton`).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased`
// (`DocumentBasedSingleton`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { DocView, MutableDocument, NodeId } from "../doc";
import { AdtError } from "./error";
import type { InitialAttributes } from "./initializer";
import { locate, type Locator } from "./locator";

/**
 * An optional value held by the first element with a given tag name
 * anywhere below a container; further such elements are redundant.
 *
 * Setting the value inserts a new element as the first child of the
 * container and then deletes all others, last to first. As in the
 * original, elements with the tag name nested deeper in the container
 * count too.
 */
export class DocumentBasedSingleton {
  constructor(private readonly valueTag: string) {}

  private all(view: DocView, container: NodeId): NodeId[] {
    return view
      .descendantElements(container)
      .filter((element) => view.hasTag(element, this.valueTag));
  }

  /** The element holding the value, if any (`get`). */
  get(view: DocView, container: NodeId): NodeId | null {
    return view.elementWithTagName(this.valueTag, container);
  }

  hasValue(view: DocView, container: NodeId): boolean {
    return this.get(view, container) !== null;
  }

  /** Deletes all elements with the tag name but the first, last to first. */
  private cleanup(doc: MutableDocument, container: Locator): void {
    for (;;) {
      const all = this.all(doc.view(), locate(container, doc.view()));
      const last = all[all.length - 1];
      if (last === undefined || all.length <= 1) {
        return;
      }
      doc.deleteNode(last);
    }
  }

  /**
   * Replaces the value by a new element with the given attributes and
   * returns that element (`set`).
   */
  set(doc: MutableDocument, container: Locator, attributes: InitialAttributes): NodeId {
    const node = locate(container, doc.view());
    doc.createElement(doc.view().contentStart(node), this.valueTag, attributes);
    this.cleanup(doc, container);
    const element = this.get(doc.view(), locate(container, doc.view()));
    if (element === null) {
      throw AdtError.missingContainer();
    }
    return element;
  }

  /** Removes the value: deletes every element with the tag name. */
  clear(doc: MutableDocument, container: Locator): void {
    this.cleanup(doc, container);
    const element = this.get(doc.view(), locate(container, doc.view()));
    if (element !== null) {
      doc.deleteNode(element);
    }
  }
}
