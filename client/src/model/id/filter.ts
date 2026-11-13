// Filters over wavelet ids (`IdFilter`, `IdFilters`).
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { cmpUtf16 } from "../docop/utf16";
import { CONVERSATION_WAVELET_PREFIX } from "./constants";
import type { WaveletId } from "./waveId";

/**
 * Accepts wavelet ids that are in a set of ids or whose local id starts
 * with one of a set of prefixes. Both sets are kept sorted and without
 * duplicates.
 */
export class IdFilter {
  /** Sorted, distinct. */
  readonly ids: readonly WaveletId[];
  /** Sorted in UTF-16 order, distinct. */
  readonly prefixes: readonly string[];

  private constructor(ids: Iterable<WaveletId>, prefixes: Iterable<string>) {
    const sortedIds = [...ids].sort((a, b) => a.compareTo(b));
    this.ids = sortedIds.filter((id, i) => i === 0 || !id.equals(sortedIds[i - 1] as WaveletId));
    this.prefixes = [...new Set(prefixes)].sort(cmpUtf16);
  }

  static of(ids: Iterable<WaveletId>, prefixes: Iterable<string>): IdFilter {
    return new IdFilter(ids, prefixes);
  }

  static ofIds(ids: Iterable<WaveletId>): IdFilter {
    return new IdFilter(ids, []);
  }

  static ofPrefixes(prefixes: Iterable<string>): IdFilter {
    return new IdFilter([], prefixes);
  }

  /** Accepts every id (a single empty prefix). */
  static allIds(): IdFilter {
    return IdFilter.ofPrefixes([""]);
  }

  /** Accepts no id. */
  static noIds(): IdFilter {
    return new IdFilter([], []);
  }

  /** Accepts conversational wavelet ids. */
  static conversationWaveletIds(): IdFilter {
    return IdFilter.ofPrefixes([CONVERSATION_WAVELET_PREFIX]);
  }

  accepts(id: WaveletId): boolean {
    return this.ids.some((i) => i.equals(id)) || this.prefixes.some((p) => id.id.startsWith(p));
  }

  equals(other: IdFilter): boolean {
    return (
      this.ids.length === other.ids.length &&
      this.prefixes.length === other.prefixes.length &&
      this.ids.every((id, i) => id.equals(other.ids[i] as WaveletId)) &&
      this.prefixes.every((p, i) => p === other.prefixes[i])
    );
  }

  /** E.g. `IdFilter[prefixes={"conv"}, ids=[[WaveletId example.com/x]]]`. */
  toString(): string {
    let out = "IdFilter[";
    // Distinguishes no prefixes from a single empty prefix.
    if (this.prefixes.length > 0) {
      out += `prefixes={${this.prefixes.map((p) => JSON.stringify(p)).join(", ")}}`;
    }
    if (this.ids.length > 0) {
      if (this.prefixes.length > 0) {
        out += ", ";
      }
      out += `ids=[${this.ids.join(", ")}]`;
    }
    return out + "]";
  }
}
