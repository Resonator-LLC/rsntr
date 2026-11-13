// Schemas built from rules, and the predicates their value checks use
// (`XmlSchemaConstraints` and the `SchemaUtils` functions of the Rust
// `schema/mod.rs`; a separate module here only to keep the module graph
// free of cycles).
//
// TypeScript port of the Rust crate `resonator-model` (module `schema`), itself
// derived from `org.waveprotocol.wave.model.schema`
// (`AbstractXmlSchemaConstraints`, `SchemaUtils`) of Apache Wave, licensed
// under the Apache License, Version 2.0.

import { isJavaLong, parseJavaDouble, parseJavaInt } from "../adt";
import type { DocumentSchema, PermittedCharacters } from "../docop";
import { util as idUtil, type WaveletId } from "../id";

/** The values of a boolean attribute (`SchemaUtils.BOOLEAN_VALUES`). */
export const BOOLEAN_VALUES: readonly string[] = ["true", "false"];

/**
 * Whether a string is a 32-bit integer of at least `min`
 * (`SchemaUtils.isValidInteger`).
 */
export function isValidInteger(value: string, min: number): boolean {
  const parsed = parseJavaInt(value);
  return parsed !== null && parsed >= min;
}

/** `SchemaUtils.isNonNegativeInteger`. */
export function isNonNegativeInteger(value: string): boolean {
  return isValidInteger(value, 0);
}

/** `SchemaUtils.isPositiveInteger`. */
export function isPositiveInteger(value: string): boolean {
  return isValidInteger(value, 1);
}

/** `SchemaUtils.isWaveletId`, which accepts anything. */
export function isWaveletId(_value: string): boolean {
  return true;
}

/** `SchemaUtils.isLong`. */
export function isLong(value: string): boolean {
  return isJavaLong(value);
}

/** Whether `Double.parseDouble` accepts a string (`SchemaUtils.isDouble`). */
export function isDouble(value: string): boolean {
  return parseJavaDouble(value) !== null;
}

/**
 * Whether a wavelet id is conversational; false for an id that is not
 * properly escaped (where `isConversationalId` throws).
 */
export function isConversational(waveletId: WaveletId): boolean {
  try {
    return idUtil.isConversationalId(waveletId);
  } catch {
    return false;
  }
}

/**
 * A check of an attribute value that takes precedence over the rules of
 * an {@link XmlSchemaConstraints}: a boolean decides, `null` leaves the
 * decision to the rules. Stands in for the
 * `permitsAttribute(tag, attr, value)` overrides of the original's schema
 * subclasses.
 */
export type ValueCheck = (element: string, name: string, value: string) => boolean | null;

/**
 * The key of an element type in the rule tables: "" for the top level;
 * `null` for the invalid type "".
 */
function fixType(element: string | null): string | null {
  return element === null ? "" : element === "" ? null : element;
}

/**
 * A schema built from rules (`AbstractXmlSchemaConstraints`). Nothing is
 * permitted until a rule permits it.
 */
export class XmlSchemaConstraints implements DocumentSchema {
  /** Keyed by parent type, "" for the top level. */
  private readonly permittedChildren = new Map<string, Set<string>>();
  /**
   * By element type and attribute name: the permitted values, `null` for
   * any value.
   */
  private readonly permittedAttrs = new Map<string, Map<string, Set<string> | null>>();
  private readonly textPermitted = new Map<string, PermittedCharacters>();
  private readonly requiredInitial = new Map<string, readonly string[]>();
  private valueCheck: ValueCheck | null = null;

  /** Sets the check consulted before the rules for attribute values. */
  setValueCheck(check: ValueCheck): void {
    this.valueCheck = check;
  }

  /** Sets the children an element must start with (`addRequiredInitial`). */
  addRequiredInitial(element: string, children: readonly string[]): void {
    this.requiredInitial.set(element, [...children]);
  }

  /** Permits blip text in elements (`containsBlipText`). */
  containsBlipText(elements: readonly string[]): void {
    for (const element of elements) {
      this.textPermitted.set(element, "blipText");
    }
  }

  /** Permits any text in elements (`containsAnyText`). */
  containsAnyText(elements: readonly string[]): void {
    for (const element of elements) {
      this.textPermitted.set(element, "any");
    }
  }

  /**
   * Permits children in a parent, `null` being the top level
   * (`addChildren`).
   */
  addChildren(parent: string | null, children: readonly string[]): void {
    const key = fixType(parent);
    if (key === null) {
      return;
    }
    let permitted = this.permittedChildren.get(key);
    if (permitted === undefined) {
      permitted = new Set();
      this.permittedChildren.set(key, permitted);
    }
    for (const child of children) {
      permitted.add(child);
    }
  }

  /** Permits attributes with any value (`addAttrs`). */
  addAttrs(element: string, names: readonly string[]): void {
    for (const name of names) {
      this.addAttrWithValues(element, name, []);
    }
  }

  /**
   * Permits an attribute with the given values, in addition to the values
   * it may have so far; without values, with any value
   * (`addAttrWithValues`).
   */
  addAttrWithValues(element: string, name: string, values: readonly string[]): void {
    let attrs = this.permittedAttrs.get(element);
    if (attrs === undefined) {
      attrs = new Map();
      this.permittedAttrs.set(element, attrs);
    }
    if (values.length === 0) {
      attrs.set(name, null);
      return;
    }
    let permitted = attrs.get(name);
    if (permitted === undefined || permitted === null) {
      permitted = new Set();
      attrs.set(name, permitted);
    }
    for (const value of values) {
      permitted.add(value);
    }
  }

  permitsChild(parent: string | null, child: string): boolean {
    const key = fixType(parent);
    return key !== null && child !== "" && (this.permittedChildren.get(key)?.has(child) ?? false);
  }

  permittedCharacters(element: string | null): PermittedCharacters {
    const key = fixType(element);
    return (key === null ? undefined : this.textPermitted.get(key)) ?? "none";
  }

  permitsAttribute(element: string, name: string): boolean {
    return this.permittedAttrs.get(element)?.has(name) ?? false;
  }

  permitsAttributeValue(element: string, name: string, value: string): boolean {
    const decision = this.valueCheck?.(element, name, value) ?? null;
    if (decision !== null) {
      return decision;
    }
    const values = this.permittedAttrs.get(element)?.get(name);
    return values === undefined ? false : values === null ? true : values.has(value);
  }

  requiredInitialChildren(element: string | null): readonly string[] {
    const key = fixType(element);
    return (key === null ? undefined : this.requiredInitial.get(key)) ?? [];
  }
}

/** Finds the schema of a document (`SchemaProvider`). */
export interface SchemaProvider {
  /**
   * The schema of a document of a wavelet; `null` if the provider puts no
   * constraints on the document (`getSchemaForId`).
   */
  schemaForId(waveletId: WaveletId, documentId: string): DocumentSchema | null;
}
