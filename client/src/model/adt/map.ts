// Maps stored as one element per entry (`AbstractDocumentBasedMap`).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased`
// (`AbstractDocumentBasedMap`, `DocumentBasedBasicMap`,
// `DocumentBasedMonotonicMap`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { DocView, MutableDocument, NodeId } from "../doc";
import { deleteNodes, inDocumentOrder, locate, type Locator } from "./locator";
import type { Serializer } from "./serializer";

interface Entry<K, V> {
  key: K;
  value: V;
  element: NodeId;
}

/**
 * What a container holds for a map: the entry element that counts for each
 * key, and the entry elements that are superseded.
 */
interface Loaded<K, V> {
  entries: Entry<K, V>[];
  obsolete: NodeId[];
}

abstract class DocumentBasedMap<K, V> {
  constructor(
    private readonly entryTag: string,
    private readonly keyAttr: string,
    private readonly valueAttr: string,
    protected readonly keys: Serializer<K>,
    protected readonly values: Serializer<V>,
  ) {}

  /** Whether a later entry takes the place of the one known for its key. */
  protected abstract canReplace(old: V, replacement: V): boolean;

  /** Whether putting `replacement` over `old` would change nothing. */
  protected abstract isRedundant(old: V, replacement: V): boolean;

  /**
   * Replays the original's load: the entries in document order, each
   * either taking the place of the entry known for its key or becoming
   * obsolete.
   */
  private load(view: DocView, container: NodeId): Loaded<K, V> {
    const loaded: Loaded<K, V> = { entries: [], obsolete: [] };
    for (const element of view.childElements(container)) {
      if (!view.hasTag(element, this.entryTag)) {
        continue;
      }
      const keyAttribute = view.attribute(element, this.keyAttr);
      const valueAttribute = view.attribute(element, this.valueAttr);
      const key = keyAttribute === null ? null : this.keys.fromAttribute(keyAttribute);
      const value = valueAttribute === null ? null : this.values.fromAttribute(valueAttribute);
      if (key === null || value === null) {
        continue;
      }
      const known = loaded.entries.find((entry) => this.keys.equals(entry.key, key));
      if (known === undefined) {
        loaded.entries.push({ key, value, element });
      } else if (this.canReplace(known.value, value)) {
        loaded.obsolete.push(known.element);
        known.key = key;
        known.value = value;
        known.element = element;
      } else {
        loaded.obsolete.push(element);
      }
    }
    return loaded;
  }

  /** The value of a key, or `null`. */
  get(view: DocView, container: NodeId, key: K): V | null {
    const entry = this.load(view, container).entries.find((e) => this.keys.equals(e.key, key));
    return entry === undefined ? null : entry.value;
  }

  /** The entries, ordered by key. */
  entries(view: DocView, container: NodeId): [K, V][] {
    return this.load(view, container)
      .entries.map(({ key, value }): [K, V] => [key, value])
      .sort((a, b) => this.keys.compare(a[0], b[0]));
  }

  /** The keys, in order (`keySet`). */
  keySet(view: DocView, container: NodeId): K[] {
    return this.entries(view, container).map(([key]) => key);
  }

  /**
   * Removes a key. Also deletes all superseded entries, whether or not
   * the key is present.
   */
  remove(doc: MutableDocument, container: Locator, key: K): void {
    const node = locate(container, doc.view());
    const loaded = this.load(doc.view(), node);
    const doomed = loaded.obsolete;
    for (const entry of loaded.entries) {
      if (this.keys.equals(entry.key, key)) {
        doomed.push(entry.element);
      }
    }
    deleteNodes(doc, inDocumentOrder(doc.view(), doomed));
  }

  /**
   * Unless redundant, removes the key and then inserts a new entry as the
   * first child of the container. Returns whether the document was
   * changed.
   */
  put(doc: MutableDocument, container: Locator, key: K, value: V): boolean {
    const current = this.get(doc.view(), locate(container, doc.view()), key);
    if (current !== null && this.isRedundant(current, value)) {
      return false;
    }
    const keyString = this.keys.toAttribute(key);
    const valueString = this.values.toAttribute(value);
    this.remove(doc, container, key);
    const node = locate(container, doc.view());
    doc.createElement(doc.view().contentStart(node), this.entryTag, [
      [this.keyAttr, keyString],
      [this.valueAttr, valueString],
    ]);
    return true;
  }
}

/**
 * A map in which the entry element that comes last in the document counts
 * (`DocumentBasedBasicMap`).
 *
 *     <container>
 *       <entry key="a" value="2"/>
 *       <entry key="b" value="7"/>
 *       <entry key="a" value="1"/>     the value of "a" is 1
 *     </container>
 *
 * A new entry is inserted as the first child of the container, after the
 * entry it replaces and all superseded entries have been deleted. `put`
 * does nothing if the key has that value already.
 */
export class DocumentBasedBasicMap<K, V> extends DocumentBasedMap<K, V> {
  constructor(
    entryTag: string,
    keyAttr: string,
    valueAttr: string,
    keys: Serializer<K>,
    values: Serializer<V>,
  ) {
    super(entryTag, keyAttr, valueAttr, keys, values);
  }

  // A later entry always replaces an earlier one.
  protected canReplace(_old: V, _replacement: V): boolean {
    return true;
  }

  protected isRedundant(old: V, replacement: V): boolean {
    return this.values.equals(old, replacement);
  }
}

/**
 * A map in which the greatest value of a key counts, so that values only
 * ever increase (`DocumentBasedMonotonicMap`). Of several entries with the
 * greatest value, the first in the document counts.
 *
 * A new entry is inserted as the first child of the container, after the
 * entry it replaces and all superseded entries have been deleted. `put`
 * does nothing unless the value is greater than the current one; after
 * `remove`, a lesser value can be put.
 */
export class DocumentBasedMonotonicMap<K, V> extends DocumentBasedMap<K, V> {
  constructor(
    entryTag: string,
    keyAttr: string,
    valueAttr: string,
    keys: Serializer<K>,
    values: Serializer<V>,
  ) {
    super(entryTag, keyAttr, valueAttr, keys, values);
  }

  protected canReplace(old: V, replacement: V): boolean {
    return this.values.compare(old, replacement) < 0;
  }

  protected isRedundant(old: V, replacement: V): boolean {
    return this.values.compare(old, replacement) >= 0;
  }
}
