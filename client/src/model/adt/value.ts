// Single values stored in attributes or in one element per write.
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased`
// (`DocumentBasedBasicValue`, `DocumentBasedStructuredValue`,
// `DocumentBasedMonotonicValue`, `DocumentBasedBoolean`) of Apache Wave,
// licensed under the Apache License, Version 2.0.

import type { DocView, MutableDocument, NodeId } from "../doc";
import { initialiseAttribute, type Initializer } from "./initializer";
import { deleteNodes, elementsAt, inDocumentOrder, locate, type Locator } from "./locator";
import type { Serializer } from "./serializer";

/**
 * A value stored in one attribute of an element
 * (`DocumentBasedBasicValue`).
 */
export class DocumentBasedBasicValue<T> {
  constructor(
    private readonly valueAttr: string,
    private readonly serializer: Serializer<T>,
  ) {}

  /** The value; `null` if the attribute is absent or does not parse. */
  get(view: DocView, element: NodeId): T | null {
    const attribute = view.attribute(element, this.valueAttr);
    return attribute === null ? null : this.serializer.fromAttribute(attribute);
  }

  /**
   * Sets the attribute, or removes it for `null`. Does nothing if the
   * attribute already has that string.
   */
  set(doc: MutableDocument, element: NodeId, value: T | null): void {
    const attribute = value === null ? null : this.serializer.toAttribute(value);
    doc.setElementAttribute(element, this.valueAttr, attribute);
  }

  /**
   * An initializer that gives a new element this value
   * (`createInitialiser`); it does nothing for `null`.
   */
  createInitialiser(value: T | null): Initializer {
    return (target) => {
      if (value !== null) {
        initialiseAttribute(target, this.valueAttr, this.serializer.toAttribute(value));
      }
    };
  }
}

/**
 * A set of named values stored in the attributes of one element, the names
 * being the attribute names (`DocumentBasedStructuredValue`, whose names
 * are the constants of an enum).
 */
export class DocumentBasedStructuredValue<K extends string, V> {
  constructor(private readonly serializer: Serializer<V>) {}

  /**
   * The value of a field; `null` if the attribute is absent or does not
   * parse.
   */
  get(view: DocView, element: NodeId, name: K): V | null {
    const attribute = view.attribute(element, name);
    return attribute === null ? null : this.serializer.fromAttribute(attribute);
  }

  /**
   * Sets one field, or removes it for `null`. Does nothing if the
   * attribute already has that string.
   */
  set(doc: MutableDocument, element: NodeId, name: K, value: V | null): void {
    const attribute = value === null ? null : this.serializer.toAttribute(value);
    doc.setElementAttribute(element, name, attribute);
  }

  /**
   * Sets several fields in one operation, `null` removing a field. The
   * operation is emitted even if no field changes.
   */
  setAll(
    doc: MutableDocument,
    element: NodeId,
    values: Iterable<readonly [name: K, value: V | null]>,
  ): void {
    const updates = Array.from(
      values,
      ([name, value]) =>
        [name, value === null ? null : this.serializer.toAttribute(value)] as const,
    );
    doc.updateElementAttributes(element, updates);
  }

  /**
   * An initializer that gives a new element these fields
   * (`createInitialiser`); fields that are `null` are skipped.
   */
  createInitialiser(values: Iterable<readonly [name: K, value: V | null]>): Initializer {
    const fields = Array.from(values);
    return (target) => {
      for (const [name, value] of fields) {
        if (value !== null) {
          initialiseAttribute(target, name, this.serializer.toAttribute(value));
        }
      }
    };
  }
}

/**
 * A value that only ever increases, stored as one element per write
 * (`DocumentBasedMonotonicValue`). The element with the greatest value
 * counts; of several, the first in the document.
 *
 *     <container>
 *       <entry value="10"/>
 *       <entry value="30"/>      the value is 30
 *       <entry value="20"/>
 *     </container>
 *
 * A write appends a new element to the container and then deletes all
 * others.
 */
export class DocumentBasedMonotonicValue<C> {
  constructor(
    private readonly entryTag: string,
    private readonly valueAttr: string,
    private readonly serializer: Serializer<C>,
  ) {}

  /** The element that counts, with its value, and the other elements. */
  private load(
    view: DocView,
    container: NodeId,
  ): { current: { value: C; element: NodeId } | null; obsolete: NodeId[] } {
    let current: { value: C; element: NodeId } | null = null;
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
      if (current !== null && this.serializer.compare(current.value, value) >= 0) {
        obsolete.push(element);
      } else {
        if (current !== null) {
          obsolete.push(current.element);
        }
        current = { value, element };
      }
    }
    return { current, obsolete };
  }

  get(view: DocView, container: NodeId): C | null {
    return this.load(view, container).current?.value ?? null;
  }

  /**
   * Sets the value if it is greater than the current one. Returns
   * whether the document was changed.
   */
  set(doc: MutableDocument, container: Locator, value: C): boolean {
    const node = locate(container, doc.view());
    const { current, obsolete } = this.load(doc.view(), node);
    if (current !== null) {
      if (this.serializer.compare(current.value, value) >= 0) {
        return false;
      }
      obsolete.push(current.element);
    }
    const attribute = this.serializer.toAttribute(value);
    // The elements to delete all precede the new one, so their locations
    // stay put.
    const locations = obsolete.map((element) => doc.view().location(element));
    doc.createChildElement(node, this.entryTag, [[this.valueAttr, attribute]]);
    const old = inDocumentOrder(doc.view(), elementsAt(doc.view(), locations));
    deleteNodes(doc, old);
    return true;
  }
}

/**
 * An optional boolean stored as one element per write
 * (`DocumentBasedBoolean`). The last element in the document counts; its
 * value is true unless its attribute is exactly `false`.
 *
 *     <container>
 *       <flag value="false"/>
 *       <flag value="true"/>     the value is true
 *     </container>
 *
 * Writing true or false appends a new element and then deletes all others,
 * first to last. Writing `null` deletes all elements, the one that counts
 * last.
 */
export class DocumentBasedBoolean {
  constructor(
    private readonly tag: string,
    private readonly valueAttr: string,
  ) {}

  private elements(view: DocView, container: NodeId): NodeId[] {
    return view.childElements(container).filter((element) => view.hasTag(element, this.tag));
  }

  get(view: DocView, container: NodeId): boolean | null {
    const element = this.elements(view, container).pop();
    if (element === undefined) {
      return null;
    }
    return view.attribute(element, this.valueAttr) !== "false";
  }

  /**
   * Sets or clears the value; does nothing if it is that already.
   * Returns whether the document was changed.
   */
  set(doc: MutableDocument, container: Locator, value: boolean | null): boolean {
    const node = locate(container, doc.view());
    if (this.get(doc.view(), node) === value) {
      return false;
    }
    const all = this.elements(doc.view(), node);
    if (value === null) {
      deleteNodes(doc, all);
    } else {
      const locations = all.map((element) => doc.view().location(element));
      doc.createChildElement(node, this.tag, [[this.valueAttr, value ? "true" : "false"]]);
      deleteNodes(doc, elementsAt(doc.view(), locations));
    }
    return true;
  }
}
