// Annotation boundary maps.
//
// Derived from Apache Wave (`AnnotationBoundaryMapImpl`), licensed under the
// Apache License, Version 2.0.

import { OperationError } from "./error";

/** A change of one annotation key, starting at an annotation boundary. */
export interface AnnotationChange {
  readonly key: string;
  readonly oldValue: string | null;
  readonly newValue: string | null;
}

export function annotationChange(
  key: string,
  oldValue: string | null,
  newValue: string | null,
): AnnotationChange {
  return { key, oldValue, newValue };
}

/** A `[key, old, new]` triple. */
export type ChangeTriple = readonly [key: string, oldValue: string | null, newValue: string | null];

/**
 * The payload of an annotation boundary component: the keys whose change
 * ends here and the keys whose change starts (or is replaced) here.
 *
 * In a well-formed operation both lists are strictly increasing in UTF-16
 * order; that is checked by validation, not by this type.
 */
export class AnnotationBoundaryMap {
  /** The boundary that ends and changes nothing. */
  static readonly EMPTY = new AnnotationBoundaryMap([], []);

  readonly endKeys: readonly string[];
  readonly changes: readonly AnnotationChange[];

  private constructor(endKeys: readonly string[], changes: readonly AnnotationChange[]) {
    this.endKeys = endKeys;
    this.changes = changes;
  }

  /** The boundary that ends and changes nothing. */
  static empty(): AnnotationBoundaryMap {
    return AnnotationBoundaryMap.EMPTY;
  }

  /**
   * Builds a boundary with the same argument checks as the original
   * `AnnotationBoundaryMapImpl`: keys unique, non-empty, free of `?` and
   * `@`, and end keys disjoint from change keys. Order is not checked.
   */
  static create(
    endKeys: readonly string[],
    changes: readonly AnnotationChange[],
  ): AnnotationBoundaryMap {
    const changeKeySet = new Set(changes.map((c) => c.key));
    const endKeySet = new Set(endKeys);
    if (changeKeySet.size !== changes.length || endKeySet.size !== endKeys.length) {
      throw OperationError.invalidArgument("annotation keys must all be unique");
    }
    if (changeKeySet.has("") || endKeySet.has("")) {
      throw OperationError.invalidArgument("empty-string annotation keys are not permitted");
    }
    for (const key of changeKeySet) {
      validateAnnotationKey(key);
    }
    for (const key of endKeySet) {
      validateAnnotationKey(key);
    }
    for (const key of endKeySet) {
      if (changeKeySet.has(key)) {
        throw OperationError.invalidArgument("change keys and end keys must be disjoint sets");
      }
    }
    return new AnnotationBoundaryMap(endKeys, changes);
  }

  /** Builds a boundary without any check. */
  static unchecked(
    endKeys: readonly string[],
    changes: readonly AnnotationChange[],
  ): AnnotationBoundaryMap {
    return new AnnotationBoundaryMap(endKeys, changes);
  }

  /** A boundary that only ends the given keys. */
  static ends(keys: readonly string[]): AnnotationBoundaryMap {
    return AnnotationBoundaryMap.create([...keys], []);
  }

  /** A boundary that only starts the given `[key, old, new]` changes. */
  static changesOnly(triples: readonly ChangeTriple[]): AnnotationBoundaryMap {
    return AnnotationBoundaryMap.create(
      [],
      triples.map(([key, oldValue, newValue]) => ({ key, oldValue, newValue })),
    );
  }

  isEmpty(): boolean {
    return this.endKeys.length === 0 && this.changes.length === 0;
  }

  /** The boundary with old and new values of every change swapped. */
  invert(): AnnotationBoundaryMap {
    return new AnnotationBoundaryMap(
      this.endKeys,
      this.changes.map((c) => ({ key: c.key, oldValue: c.newValue, newValue: c.oldValue })),
    );
  }

  /** Syntactic equality: same end keys and changes in the same order. */
  equals(other: AnnotationBoundaryMap): boolean {
    return (
      this.endKeys.length === other.endKeys.length &&
      this.changes.length === other.changes.length &&
      this.endKeys.every((key, i) => key === other.endKeys[i]) &&
      this.changes.every((c, i) => {
        const o = other.changes[i];
        return (
          o !== undefined && c.key === o.key && c.oldValue === o.oldValue && c.newValue === o.newValue
        );
      })
    );
  }
}

/** Rejects annotation keys containing `?` or `@`. */
export function validateAnnotationKey(key: string): void {
  if (key.includes("?") || key.includes("@")) {
    throw OperationError.invalidArgument(
      "annotation keys must not contain the '?' or '@' characters",
    );
  }
}
