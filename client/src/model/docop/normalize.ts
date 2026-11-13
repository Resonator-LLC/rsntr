// Operation normalizers: sinks that merge adjacent ranges and drop redundant
// annotation changes.
//
// Derived from Apache Wave (`RangeNormalizer`, `AnnotationsNormalizer`),
// licensed under the Apache License, Version 2.0.

import { AnnotationBoundaryMap, type AnnotationChange } from "./annotation";
import type { DocOpComponent } from "./op";
import { BufferSink, OpSink } from "./sink";
import { saturatingAdd } from "./utf16";

type RangeCache =
  | { readonly type: "empty" }
  | { readonly type: "retain"; itemCount: number }
  | { readonly type: "characters"; characters: string }
  | { readonly type: "deleteCharacters"; characters: string };

const EMPTY_CACHE: RangeCache = { type: "empty" };

/**
 * Merges adjacent retains, adjacent insertions of characters and adjacent
 * deletions of characters, and drops empty ones.
 */
export class RangeNormalizer extends OpSink {
  private readonly target: OpSink;
  private cache: RangeCache = EMPTY_CACHE;

  constructor(target: OpSink) {
    super();
    this.target = target;
  }

  private flush(): void {
    const cache = this.cache;
    this.cache = EMPTY_CACHE;
    switch (cache.type) {
      case "empty":
        break;
      case "retain":
        this.target.push({ type: "retain", itemCount: cache.itemCount });
        break;
      case "characters":
        this.target.push({ type: "characters", characters: cache.characters });
        break;
      case "deleteCharacters":
        this.target.push({ type: "deleteCharacters", characters: cache.characters });
        break;
    }
  }

  push(component: DocOpComponent): void {
    switch (component.type) {
      case "retain":
        // Counts that are not positive are dropped (in Rust, only zero can
        // occur).
        if (component.itemCount > 0) {
          if (this.cache.type === "retain") {
            this.cache.itemCount = saturatingAdd(this.cache.itemCount, component.itemCount);
          } else {
            this.flush();
            this.cache = { type: "retain", itemCount: component.itemCount };
          }
        }
        break;
      case "characters":
      case "deleteCharacters":
        if (component.characters.length > 0) {
          if (this.cache.type === component.type) {
            this.cache.characters += component.characters;
          } else {
            this.flush();
            this.cache = { type: component.type, characters: component.characters };
          }
        }
        break;
      default:
        this.flush();
        this.target.push(component);
    }
  }

  finish(): DocOpComponent[] {
    this.flush();
    return this.target.finish();
  }
}

interface ChangeValues {
  readonly oldValue: string | null;
  readonly newValue: string | null;
}

/**
 * Merges consecutive annotation boundaries and drops changes that repeat the
 * change already in effect, and ends of keys that are not open.
 */
export class AnnotationsNormalizer extends OpSink {
  private readonly target: OpSink;
  /** Changes currently in effect in the output. */
  private readonly annotationTracker = new Map<string, ChangeValues>();
  /** Changes (values) and ends (`null`) seen since the last flush. */
  private annotationChanges = new Map<string, ChangeValues | null>();

  constructor(target: OpSink) {
    super();
    this.target = target;
  }

  private flushAnnotations(): void {
    if (this.annotationChanges.size === 0) {
      return;
    }
    const pending = this.annotationChanges;
    this.annotationChanges = new Map();
    const changes: AnnotationChange[] = [];
    const ends: string[] = [];
    // Keys in UTF-16 order, which is what the default sort gives.
    for (const key of [...pending.keys()].sort()) {
      const values = pending.get(key);
      if (values === undefined) {
        continue;
      }
      if (values === null) {
        if (this.annotationTracker.delete(key)) {
          ends.push(key);
        }
      } else {
        const tracked = this.annotationTracker.get(key);
        if (
          tracked === undefined ||
          tracked.oldValue !== values.oldValue ||
          tracked.newValue !== values.newValue
        ) {
          changes.push({ key, oldValue: values.oldValue, newValue: values.newValue });
          this.annotationTracker.set(key, values);
        }
      }
    }
    if (changes.length > 0 || ends.length > 0) {
      this.target.push({
        type: "annotationBoundary",
        boundary: AnnotationBoundaryMap.unchecked(ends, changes),
      });
    }
  }

  push(component: DocOpComponent): void {
    switch (component.type) {
      case "annotationBoundary":
        for (const change of component.boundary.changes) {
          this.annotationChanges.set(change.key, {
            oldValue: change.oldValue,
            newValue: change.newValue,
          });
        }
        for (const key of component.boundary.endKeys) {
          this.annotationChanges.set(key, null);
        }
        return;
      case "retain":
        if (component.itemCount === 0) {
          return;
        }
        break;
      case "characters":
      case "deleteCharacters":
        if (component.characters.length === 0) {
          return;
        }
        break;
      default:
        break;
    }
    this.flushAnnotations();
    this.target.push(component);
  }

  finish(): DocOpComponent[] {
    this.flushAnnotations();
    return this.target.finish();
  }
}

/** The standard normalizer: annotations first, then ranges. */
export type OpNormalizer = AnnotationsNormalizer;

/** Creates the standard normalizer writing to a fresh buffer. */
export function newNormalizer(): OpNormalizer {
  return new AnnotationsNormalizer(new RangeNormalizer(new BufferSink()));
}
