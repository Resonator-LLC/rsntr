// Document operations and their components.
//
// Derived from the `org.waveprotocol.wave.model.document.operation` package
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { AnnotationBoundaryMap } from "./annotation";
import type { Attributes, AttributesUpdate } from "./attributes";
import { OperationError } from "./error";
import { toConciseString } from "./util";
import { ViolationCollector, isWellFormed } from "./validate";

export { BufferSink, OpSink } from "./sink";

/**
 * One component of a document operation.
 *
 * Counts and string lengths are measured in document items; a character
 * item is one UTF-16 code unit.
 */
export type DocOpComponent =
  | { readonly type: "annotationBoundary"; readonly boundary: AnnotationBoundaryMap }
  | { readonly type: "characters"; readonly characters: string }
  | { readonly type: "elementStart"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "elementEnd" }
  | { readonly type: "retain"; readonly itemCount: number }
  | { readonly type: "deleteCharacters"; readonly characters: string }
  | { readonly type: "deleteElementStart"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "deleteElementEnd" }
  | {
      readonly type: "replaceAttributes";
      readonly oldAttributes: Attributes;
      readonly newAttributes: Attributes;
    }
  | { readonly type: "updateAttributes"; readonly update: AttributesUpdate };

export type DocOpComponentType = DocOpComponent["type"];

/** Constructors for components. */
export const Component = {
  annotationBoundary: (boundary: AnnotationBoundaryMap): DocOpComponent => ({
    type: "annotationBoundary",
    boundary,
  }),
  characters: (characters: string): DocOpComponent => ({ type: "characters", characters }),
  elementStart: (tag: string, attributes: Attributes): DocOpComponent => ({
    type: "elementStart",
    tag,
    attributes,
  }),
  elementEnd: (): DocOpComponent => ({ type: "elementEnd" }),
  retain: (itemCount: number): DocOpComponent => ({ type: "retain", itemCount }),
  deleteCharacters: (characters: string): DocOpComponent => ({
    type: "deleteCharacters",
    characters,
  }),
  deleteElementStart: (tag: string, attributes: Attributes): DocOpComponent => ({
    type: "deleteElementStart",
    tag,
    attributes,
  }),
  deleteElementEnd: (): DocOpComponent => ({ type: "deleteElementEnd" }),
  replaceAttributes: (oldAttributes: Attributes, newAttributes: Attributes): DocOpComponent => ({
    type: "replaceAttributes",
    oldAttributes,
    newAttributes,
  }),
  updateAttributes: (update: AttributesUpdate): DocOpComponent => ({
    type: "updateAttributes",
    update,
  }),
} as const;

const TYPE_NAMES: Readonly<Record<DocOpComponentType, string>> = {
  annotationBoundary: "annotation boundary",
  characters: "characters",
  elementStart: "element start",
  elementEnd: "element end",
  retain: "retain",
  deleteCharacters: "delete characters",
  deleteElementStart: "delete element start",
  deleteElementEnd: "delete element end",
  replaceAttributes: "replace attributes",
  updateAttributes: "update attributes",
};

/** The component's type name, as in the original `DocOpComponentType`. */
export function componentTypeName(component: DocOpComponent): string {
  return TYPE_NAMES[component.type];
}

/** Whether the component may appear in a document initialization. */
export function isInitializationComponent(component: DocOpComponent): boolean {
  switch (component.type) {
    case "annotationBoundary":
    case "characters":
    case "elementStart":
    case "elementEnd":
      return true;
    default:
      return false;
  }
}

/** Syntactic equality of two components. */
export function componentEquals(a: DocOpComponent, b: DocOpComponent): boolean {
  switch (a.type) {
    case "annotationBoundary":
      return b.type === a.type && a.boundary.equals(b.boundary);
    case "characters":
    case "deleteCharacters":
      return b.type === a.type && a.characters === b.characters;
    case "elementStart":
    case "deleteElementStart":
      return b.type === a.type && a.tag === b.tag && a.attributes.equals(b.attributes);
    case "elementEnd":
    case "deleteElementEnd":
      return b.type === a.type;
    case "retain":
      return b.type === a.type && a.itemCount === b.itemCount;
    case "replaceAttributes":
      return (
        b.type === a.type &&
        a.oldAttributes.equals(b.oldAttributes) &&
        a.newAttributes.equals(b.newAttributes)
      );
    case "updateAttributes":
      return b.type === a.type && a.update.equals(b.update);
  }
}

/**
 * An immutable document operation: a sequence of components that spans the
 * whole document it applies to.
 *
 * Equality ({@link DocOp.equals}) is syntactic (component by component).
 */
export class DocOp {
  /** The operation without components. */
  static readonly EMPTY = new DocOp([]);

  readonly components: readonly DocOpComponent[];

  protected constructor(components: readonly DocOpComponent[]) {
    this.components = components;
  }

  /** Builds an operation, checking that it is well-formed. */
  static create(components: readonly DocOpComponent[]): DocOp {
    const op = new DocOp(components);
    if (!isWellFormed(null, op)) {
      // Check again, collecting violations this time.
      const v = new ViolationCollector();
      isWellFormed(v, op);
      throw new OperationError({
        kind: "illFormed",
        violation: v.toString(),
        op: op.toString(),
      });
    }
    return op;
  }

  /** Builds an operation without checking well-formedness. */
  static unchecked(components: readonly DocOpComponent[]): DocOp {
    return new DocOp(components);
  }

  /** Number of components. */
  get length(): number {
    return this.components.length;
  }

  isEmpty(): boolean {
    return this.components.length === 0;
  }

  [Symbol.iterator](): IterableIterator<DocOpComponent> {
    return this.components[Symbol.iterator]();
  }

  equals(other: DocOp): boolean {
    return (
      this.components.length === other.components.length &&
      this.components.every((c, i) => {
        const o = other.components[i];
        return o !== undefined && componentEquals(c, o);
      })
    );
  }

  /** The concise string form, see {@link toConciseString}. */
  toString(): string {
    return toConciseString(this);
  }
}

/**
 * A document operation that applies to the empty document: only insertion
 * and annotation boundary components.
 */
export class DocInitialization extends DocOp {
  /**
   * Views an operation as an initialization. Fails if it contains a retain,
   * deletion or attribute change. Well-formedness is not checked.
   */
  static fromOp(op: DocOp): DocInitialization {
    if (op instanceof DocInitialization) {
      return op;
    }
    for (const c of op.components) {
      if (!isInitializationComponent(c)) {
        throw OperationError.notAnInitialization(componentTypeName(c));
      }
    }
    return new DocInitialization(op.components);
  }

  /** Wraps components known to be initialization components. */
  static uncheckedInitialization(components: readonly DocOpComponent[]): DocInitialization {
    return new DocInitialization(components);
  }

  asOp(): DocOp {
    return this;
  }
}

/** A chaining builder for {@link DocOp}s. */
export class DocOpBuilder {
  private readonly components: DocOpComponent[] = [];

  /** Builds the operation, checking that it is well-formed. */
  build(): DocOp {
    return DocOp.create([...this.components]);
  }

  /** Builds the operation without checking well-formedness. */
  buildUnchecked(): DocOp {
    return DocOp.unchecked([...this.components]);
  }

  /** Builds a well-formed document initialization. */
  buildInitialization(): DocInitialization {
    return DocInitialization.fromOp(this.build());
  }

  component(component: DocOpComponent): this {
    this.components.push(component);
    return this;
  }

  annotationBoundary(boundary: AnnotationBoundaryMap): this {
    return this.component(Component.annotationBoundary(boundary));
  }

  characters(characters: string): this {
    return this.component(Component.characters(characters));
  }

  elementStart(tag: string, attributes: Attributes): this {
    return this.component(Component.elementStart(tag, attributes));
  }

  elementEnd(): this {
    return this.component(Component.elementEnd());
  }

  retain(itemCount: number): this {
    return this.component(Component.retain(itemCount));
  }

  deleteCharacters(characters: string): this {
    return this.component(Component.deleteCharacters(characters));
  }

  deleteElementStart(tag: string, attributes: Attributes): this {
    return this.component(Component.deleteElementStart(tag, attributes));
  }

  deleteElementEnd(): this {
    return this.component(Component.deleteElementEnd());
  }

  replaceAttributes(oldAttributes: Attributes, newAttributes: Attributes): this {
    return this.component(Component.replaceAttributes(oldAttributes, newAttributes));
  }

  updateAttributes(update: AttributesUpdate): this {
    return this.component(Component.updateAttributes(update));
  }
}
