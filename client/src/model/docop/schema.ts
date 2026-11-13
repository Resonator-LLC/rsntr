// Document schema constraints consulted during validation.
//
// Derived from Apache Wave (`DocumentSchema`), licensed under the Apache
// License, Version 2.0.

import {
  REPLACEMENT_CHARACTER,
  isCodePointGoodForBlip,
  isCodePointValid,
  isSurrogate,
  mapCodePoints,
} from "./utf16";

/** Constraint on the text allowed directly inside an element. */
export type PermittedCharacters =
  /** No characters permitted. */
  | "none"
  /** Only "blip text" characters permitted. */
  | "blipText"
  /** Any valid Unicode. */
  | "any";

/**
 * Converts any string into one that satisfies the constraint, or `null` if
 * no text is permitted at all.
 *
 * Deviation from the original: supplementary code points are kept rather
 * than replaced by U+FFFD. Unpaired surrogates are replaced.
 */
export function coerceString(permitted: PermittedCharacters, string: string): string | null {
  switch (permitted) {
    case "none":
      return null;
    case "blipText":
      return mapCodePoints(string, (c) => {
        if (c === 0x09) {
          return "    ";
        }
        if (c === 0x0a || c === 0x0d) {
          return " ";
        }
        if (!isSurrogate(c) && isCodePointGoodForBlip(c) === "ok") {
          return String.fromCodePoint(c);
        }
        return REPLACEMENT_CHARACTER;
      });
    case "any":
      return mapCodePoints(string, (c) =>
        !isSurrogate(c) && isCodePointValid(c) ? String.fromCodePoint(c) : REPLACEMENT_CHARACTER,
      );
  }
}

/**
 * Queries on what the XML schema of a document allows. `null` as an element
 * type stands for the top level of the document.
 */
export interface DocumentSchema {
  /** Whether `child` elements may occur as children of `parent`. */
  permitsChild(parent: string | null, child: string): boolean;

  /** What text is permitted directly inside elements of type `element`. */
  permittedCharacters(element: string | null): PermittedCharacters;

  /** Whether `element` permits an attribute named `name`. */
  permitsAttribute(element: string, name: string): boolean;

  /** Whether `element` permits attribute `name` with the given value. */
  permitsAttributeValue(element: string, name: string, value: string): boolean;

  /**
   * Element types that must occur, in this order, at the start of `element`.
   * Validation supports at most one required child and ignores any further
   * entries (the original throws in that case).
   */
  requiredInitialChildren(element: string | null): readonly string[];
}

/** A schema that permits anything. */
export class NoSchemaConstraints implements DocumentSchema {
  permitsChild(_parent: string | null, _child: string): boolean {
    return true;
  }

  permittedCharacters(_element: string | null): PermittedCharacters {
    return "any";
  }

  permitsAttribute(_element: string, _name: string): boolean {
    return true;
  }

  permitsAttributeValue(_element: string, _name: string, _value: string): boolean {
    return true;
  }

  requiredInitialChildren(_element: string | null): readonly string[] {
    return [];
  }
}

/** A schema that permits anything. */
export const NO_SCHEMA_CONSTRAINTS: DocumentSchema = new NoSchemaConstraints();
