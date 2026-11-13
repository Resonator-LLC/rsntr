// A map keyed by wavelet id.
//
// Stands in for the `Map<WaveletId, V>` of the Apache Wave channel
// interfaces (Apache License, Version 2.0). JavaScript maps compare object
// keys by identity, so entries are indexed by the serialised wavelet id.

import type { WaveletId } from "../../model/id";
import type { HashedVersion } from "../../model/version";

/** Entries iterate in insertion order. */
export class WaveletIdMap<V> implements Iterable<[WaveletId, V]> {
  private readonly entryMap = new Map<string, [WaveletId, V]>();

  constructor(entries: Iterable<readonly [WaveletId, V]> = []) {
    for (const [id, value] of entries) {
      this.set(id, value);
    }
  }

  /** The string under which an id is indexed: `waveletId.serialise()`. */
  static keyOf(id: WaveletId): string {
    return id.serialise();
  }

  get size(): number {
    return this.entryMap.size;
  }

  isEmpty(): boolean {
    return this.entryMap.size === 0;
  }

  has(id: WaveletId): boolean {
    return this.entryMap.has(WaveletIdMap.keyOf(id));
  }

  get(id: WaveletId): V | undefined {
    return this.entryMap.get(WaveletIdMap.keyOf(id))?.[1];
  }

  set(id: WaveletId, value: V): this {
    this.entryMap.set(WaveletIdMap.keyOf(id), [id, value]);
    return this;
  }

  /** Returns whether an entry was removed. */
  delete(id: WaveletId): boolean {
    return this.entryMap.delete(WaveletIdMap.keyOf(id));
  }

  clear(): void {
    this.entryMap.clear();
  }

  /** A snapshot of the keys; the map may be modified while iterating it. */
  keys(): WaveletId[] {
    return [...this.entryMap.values()].map(([id]) => id);
  }

  /** A snapshot of the values. */
  values(): V[] {
    return [...this.entryMap.values()].map(([, value]) => value);
  }

  /** A snapshot of the entries. */
  entries(): [WaveletId, V][] {
    return [...this.entryMap.values()].map(([id, value]) => [id, value]);
  }

  [Symbol.iterator](): IterableIterator<[WaveletId, V]> {
    return this.entries()[Symbol.iterator]();
  }

  /** Whether both maps have the same ids with equal values. */
  equals(
    other: WaveletIdMap<V>,
    valueEquals: (a: V, b: V) => boolean,
  ): boolean {
    if (this.size !== other.size) {
      return false;
    }
    for (const [key, [, value]] of this.entryMap) {
      const theirs = other.entryMap.get(key);
      if (theirs === undefined || !valueEquals(value, theirs[1])) {
        return false;
      }
    }
    return true;
  }

  /** `{id=value, ...}`, like a Java map. */
  toString(): string {
    return `{${this.entries()
      .map(
        ([id, value]) =>
          `${id}=${Array.isArray(value) ? `[${value.join(", ")}]` : value}`,
      )
      .join(", ")}}`;
  }
}

/**
 * For each wavelet, the versions the client knows, in increasing order;
 * the server resumes the delta stream from one of them.
 */
export type KnownWaveletVersions = WaveletIdMap<readonly HashedVersion[]>;
