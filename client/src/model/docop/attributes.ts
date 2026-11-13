// Attribute maps, annotation maps and updates to them.
//
// Ports `ImmutableStateMap`, `ImmutableUpdateMap`, `AttributesImpl`,
// `AttributesUpdateImpl`, `AnnotationMapImpl` and `AnnotationsUpdateImpl` of
// Apache Wave, licensed under the Apache License, Version 2.0.

import type { AnnotationBoundaryMap } from "./annotation";
import { OperationError } from "./error";
import { cmpUtf16 } from "./utf16";

/** A name-value pair. */
export interface Attribute {
  readonly name: string;
  readonly value: string;
}

export function attributeToString(a: Attribute): string {
  return `Attribute(${a.name}=${a.value})`;
}

/** A change of one key from an old value to a new value; `null` means unset. */
export interface AttributeUpdate {
  readonly name: string;
  readonly oldValue: string | null;
  readonly newValue: string | null;
}

export function attributeUpdateToString(u: AttributeUpdate): string {
  return `[${u.name}: ${u.oldValue ?? "null"} -> ${u.newValue ?? "null"}]`;
}

/** A `[name, old, new]` triple. */
export type UpdateTriple = readonly [name: string, oldValue: string | null, newValue: string | null];

const byName = (a: { readonly name: string }, b: { readonly name: string }): number =>
  cmpUtf16(a.name, b.name);

/** Checks that names are strictly increasing in UTF-16 order. */
export function checkAttributesSorted(entries: readonly { readonly name: string }[]): void {
  for (let i = 1; i < entries.length; i++) {
    const a = entries[i - 1]!;
    const b = entries[i]!;
    if (!(a.name < b.name)) {
      throw OperationError.invalidArgument(
        `attribute keys not strictly monotonic: ${a.name}, ${b.name}`,
      );
    }
  }
}

/** Checks that names are strictly increasing in UTF-16 order. */
export const checkUpdatesSorted: (updates: readonly AttributeUpdate[]) => void =
  checkAttributesSorted;

/**
 * An immutable map from names to values, kept as a list sorted by name
 * (UTF-16 order).
 *
 * The unchecked constructors can produce unsorted or duplicate entries; such
 * maps are rejected by validation, not here.
 */
export class Attributes {
  /** The empty map. */
  static readonly EMPTY = new Attributes([]);

  readonly entries: readonly Attribute[];

  private constructor(entries: readonly Attribute[]) {
    this.entries = entries;
  }

  /** The empty map. */
  static empty(): Attributes {
    return Attributes.EMPTY;
  }

  /**
   * Builds a map from name-value pairs in any order. Fails on duplicate
   * names.
   */
  static fromPairs(pairs: Iterable<readonly [name: string, value: string]>): Attributes {
    return Attributes.fromUnsorted(Array.from(pairs, ([name, value]) => ({ name, value })));
  }

  /** Builds a map from an object's own properties. */
  static fromRecord(record: Readonly<Record<string, string>>): Attributes {
    return Attributes.fromPairs(Object.entries(record));
  }

  /**
   * Uses the entries as given, after checking that names are strictly
   * increasing.
   */
  static fromSorted(entries: readonly Attribute[]): Attributes {
    checkAttributesSorted(entries);
    return new Attributes(entries);
  }

  /** Uses the entries as given, without any check. */
  static fromSortedUnchecked(entries: readonly Attribute[]): Attributes {
    return new Attributes(entries);
  }

  /** Sorts the entries, failing on duplicate names. */
  static fromUnsorted(entries: readonly Attribute[]): Attributes {
    return Attributes.fromSorted([...entries].sort(byName));
  }

  /** Sorts the entries but does not check for duplicate names. */
  static fromUnsortedUnchecked(entries: readonly Attribute[]): Attributes {
    return new Attributes([...entries].sort(byName));
  }

  get size(): number {
    return this.entries.length;
  }

  isEmpty(): boolean {
    return this.entries.length === 0;
  }

  /** Value of the first entry with the given name, or `null`. */
  get(name: string): string | null {
    for (const a of this.entries) {
      if (a.name === name) {
        return a.value;
      }
    }
    return null;
  }

  containsKey(name: string): boolean {
    return this.get(name) !== null;
  }

  /** Applies an update, checking that its old values match this map. */
  updateWith(update: AttributesUpdate): Attributes {
    return this.updateImpl(update, true);
  }

  /** Applies an update, ignoring its old values. */
  updateWithNoCompatibilityCheck(update: AttributesUpdate): Attributes {
    return this.updateImpl(update, false);
  }

  // Line-by-line port of ImmutableStateMap.updateWith. Note the quirk that
  // an update whose key sorts after every existing entry is never checked
  // against its old value.
  private updateImpl(update: AttributesUpdate, checkCompatibility: boolean): Attributes {
    const result: Attribute[] = [];
    const entries = this.entries;
    let index = 0;
    for (const u of update.updates) {
      while (index < entries.length) {
        const attribute = entries[index]!;
        const order = cmpUtf16(u.name, attribute.name);
        if (order > 0) {
          result.push(attribute);
          index++;
        } else if (order < 0) {
          if (checkCompatibility && u.oldValue !== null) {
            throw OperationError.mismatchedOldValue(
              `attempt to update unset attribute with ${attributeUpdateToString(u)}`,
            );
          }
          break;
        } else {
          if (checkCompatibility && u.oldValue !== attribute.value) {
            throw OperationError.mismatchedOldValue(
              `attempt to update ${attributeToString(attribute)} with ${attributeUpdateToString(u)}`,
            );
          }
          index++;
          break;
        }
      }
      if (u.newValue !== null) {
        result.push({ name: u.name, value: u.newValue });
      }
    }
    for (; index < entries.length; index++) {
      result.push(entries[index]!);
    }
    return new Attributes(result);
  }

  /** Syntactic equality: same entries in the same order. */
  equals(other: Attributes): boolean {
    return (
      this.entries.length === other.entries.length &&
      this.entries.every((a, i) => {
        const b = other.entries[i];
        return b !== undefined && a.name === b.name && a.value === b.value;
      })
    );
  }
}

/** The annotations of a document item. Same representation as {@link Attributes}. */
export type AnnotationMap = Attributes;
export const AnnotationMap = Attributes;

/**
 * An immutable set of per-key changes, kept as a list sorted by name (UTF-16
 * order).
 */
export class AttributesUpdate {
  /** The empty update. */
  static readonly EMPTY = new AttributesUpdate([]);

  readonly updates: readonly AttributeUpdate[];

  private constructor(updates: readonly AttributeUpdate[]) {
    this.updates = updates;
  }

  /** The empty update. */
  static empty(): AttributesUpdate {
    return AttributesUpdate.EMPTY;
  }

  /** An update of a single key. */
  static single(name: string, oldValue: string | null, newValue: string | null): AttributesUpdate {
    return new AttributesUpdate([{ name, oldValue, newValue }]);
  }

  /**
   * Builds an update from `[name, old, new]` triples in any order. Fails on
   * duplicate names.
   */
  static fromTriples(triples: Iterable<UpdateTriple>): AttributesUpdate {
    const updates = Array.from(triples, ([name, oldValue, newValue]) => ({
      name,
      oldValue,
      newValue,
    })).sort(byName);
    for (let i = 1; i < updates.length; i++) {
      if (updates[i - 1]!.name === updates[i]!.name) {
        throw OperationError.invalidArgument(`duplicate key: ${updates[i]!.name}`);
      }
    }
    return new AttributesUpdate(updates);
  }

  /**
   * Uses the updates as given, after checking that names are strictly
   * increasing.
   */
  static fromSorted(updates: readonly AttributeUpdate[]): AttributesUpdate {
    checkUpdatesSorted(updates);
    return new AttributesUpdate(updates);
  }

  /** Uses the updates as given, without any check. */
  static fromSortedUnchecked(updates: readonly AttributeUpdate[]): AttributesUpdate {
    return new AttributesUpdate(updates);
  }

  /** Sorts the updates, failing on duplicate names. */
  static fromUnsorted(updates: readonly AttributeUpdate[]): AttributesUpdate {
    return AttributesUpdate.fromSorted([...updates].sort(byName));
  }

  /** Sorts the updates but does not check for duplicate names. */
  static fromUnsortedUnchecked(updates: readonly AttributeUpdate[]): AttributesUpdate {
    return new AttributesUpdate([...updates].sort(byName));
  }

  get size(): number {
    return this.updates.length;
  }

  isEmpty(): boolean {
    return this.updates.length === 0;
  }

  containsKey(name: string): boolean {
    return this.updates.some((u) => u.name === name);
  }

  /** Drops the changes whose key satisfies `excluded`. */
  exclude(excluded: (name: string) => boolean): AttributesUpdate {
    return new AttributesUpdate(this.updates.filter((u) => !excluded(u.name)));
  }

  /**
   * The update equivalent to applying `this` and then `mutation`.
   *
   * Line-by-line port of ImmutableUpdateMap.composeWith: old values are only
   * checked for keys present in both updates.
   */
  composeWith(mutation: AttributesUpdate): AttributesUpdate {
    const result: AttributeUpdate[] = [];
    const existingUpdates = this.updates;
    let index = 0;
    mutation: for (const attribute of mutation.updates) {
      while (index < existingUpdates.length) {
        const existing = existingUpdates[index]!;
        const order = cmpUtf16(attribute.name, existing.name);
        if (order < 0) {
          break;
        } else if (order > 0) {
          result.push(existing);
          index++;
        } else {
          if (existing.newValue !== attribute.oldValue) {
            throw OperationError.mismatchedOldValue(
              `attempt to update ${attributeUpdateToString(existing)} with ${attributeUpdateToString(attribute)}`,
            );
          }
          result.push({
            name: attribute.name,
            oldValue: existing.oldValue,
            newValue: attribute.newValue,
          });
          index++;
          continue mutation;
        }
      }
      result.push(attribute);
    }
    for (; index < existingUpdates.length; index++) {
      result.push(existingUpdates[index]!);
    }
    return new AttributesUpdate(result);
  }

  /**
   * The update with old and new values swapped.
   *
   * Built, like the original, by composing single-key updates one by one.
   */
  invert(): AttributesUpdate {
    let inverse = AttributesUpdate.EMPTY;
    for (const u of this.updates) {
      inverse = inverse.composeWith(AttributesUpdate.single(u.name, u.newValue, u.oldValue));
    }
    return inverse;
  }

  /**
   * Folds an annotation boundary into the set of open annotation changes
   * (`AnnotationsUpdateImpl.composeWith`). End keys must be open and the
   * boundary's keys sorted.
   */
  composeWithBoundary(map: AnnotationBoundaryMap): AnnotationsUpdate {
    // An exhausted list compares greater than any key (the original uses the
    // sentinel "￿").
    const cmpKeys = (a: string | undefined, b: string | undefined): number => {
      if (a !== undefined && b !== undefined) {
        return cmpUtf16(a, b);
      }
      if (a !== undefined) {
        return -1;
      }
      return b !== undefined ? 1 : 0;
    };
    const updates = this.updates;
    const changes = map.changes;
    const ends = map.endKeys;
    const result: AttributeUpdate[] = [];
    let existingIndex = 0;
    let changeIndex = 0;
    let endIndex = 0;
    while (
      existingIndex < updates.length ||
      changeIndex < changes.length ||
      endIndex < ends.length
    ) {
      const existing = updates[existingIndex];
      const change = changes[changeIndex];
      const endKey = ends[endIndex];
      const pushChange = (): void => {
        if (change !== undefined) {
          result.push({ name: change.key, oldValue: change.oldValue, newValue: change.newValue });
        }
      };
      const existingVsChange = cmpKeys(existing?.name, change?.key);
      const existingVsEnd = cmpKeys(existing?.name, endKey);
      if (existingVsEnd < 0) {
        if (existingVsChange < 0) {
          // existing is defined: it compares less than something.
          result.push(existing!);
          existingIndex++;
        } else if (existingVsChange === 0) {
          pushChange();
          existingIndex++;
          changeIndex++;
        } else {
          pushChange();
          changeIndex++;
        }
      } else if (existingVsEnd === 0) {
        if (existingVsChange < 0) {
          existingIndex++;
          endIndex++;
        } else if (existingVsChange === 0) {
          throw OperationError.invalidArgument(
            `annotation boundary with key both in change and end: ${change?.key ?? ""}`,
          );
        } else {
          pushChange();
          existingIndex++;
          endIndex++;
          changeIndex++;
        }
      } else {
        throw OperationError.invalidArgument(
          `attempt to end key that is not part of the update: ${endKey ?? ""}`,
        );
      }
    }
    return new AttributesUpdate(result);
  }

  /** Syntactic equality: same updates in the same order. */
  equals(other: AttributesUpdate): boolean {
    return (
      this.updates.length === other.updates.length &&
      this.updates.every((a, i) => {
        const b = other.updates[i];
        return (
          b !== undefined &&
          a.name === b.name &&
          a.oldValue === b.oldValue &&
          a.newValue === b.newValue
        );
      })
    );
  }

  toString(): string {
    return `Updates: [${this.updates.map(attributeUpdateToString).join(", ")}]`;
  }
}

/**
 * The annotation changes in effect at a point in an operation. Same
 * representation as {@link AttributesUpdate}.
 */
export type AnnotationsUpdate = AttributesUpdate;
export const AnnotationsUpdate = AttributesUpdate;
