// A simple document that operations can be applied to: an array of items,
// one per UTF-16 code unit, element start or element end, each with its
// annotations. Easy to understand but not efficient (`BootstrapDocument` in
// the original).
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import { AnnotationBoundaryMap, type AnnotationChange } from "./annotation";
import { AnnotationMap, AnnotationsUpdate, type Attributes } from "./attributes";
import { OperationError } from "./error";
import { DocInitialization, type DocOp, type DocOpComponent } from "./op";
import { type DocumentSchema, NO_SCHEMA_CONSTRAINTS } from "./schema";
import { cmpUtf16 } from "./utf16";
import { toXmlString } from "./util";
import { type AutomatonDocument, ViolationCollector, validate } from "./validate";

type ItemKind =
  | { readonly type: "character"; readonly unit: number }
  | { readonly type: "elementStart"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "elementEnd" };

interface Item {
  readonly kind: ItemKind;
  readonly annotations: AnnotationMap;
}

const ELEMENT_END: ItemKind = { type: "elementEnd" };

function kindEquals(a: ItemKind, b: ItemKind): boolean {
  switch (a.type) {
    case "character":
      return b.type === "character" && a.unit === b.unit;
    case "elementStart":
      return b.type === "elementStart" && a.tag === b.tag && a.attributes.equals(b.attributes);
    case "elementEnd":
      return b.type === "elementEnd";
  }
}

/** A document represented as a flat array of annotated items. */
export class BootstrapDocument implements AutomatonDocument {
  private items: readonly Item[] = [];
  /**
   * For each item, the index of the innermost element start that is open
   * just before it.
   */
  private enclosing: (number | null)[] = [];

  /**
   * Validates `op` against this document and `schema` (default: no schema
   * constraints) and applies it. The document is unchanged on error.
   */
  consume(op: DocOp, schema: DocumentSchema = NO_SCHEMA_CONSTRAINTS): void {
    const v = new ViolationCollector();
    validate(v, schema, this, op);
    if (!v.isValid()) {
      throw new OperationError({ kind: "validation", message: v.toString() });
    }
    this.items = this.apply(op);
    this.rebuildIndex();
  }

  /** Same as {@link consume} with an explicit schema. */
  consumeWithSchema(op: DocOp, schema: DocumentSchema): void {
    this.consume(op, schema);
  }

  /** An independent copy of the document. */
  clone(): BootstrapDocument {
    const copy = new BootstrapDocument();
    copy.items = this.items;
    copy.enclosing = [...this.enclosing];
    return copy;
  }

  /** Whether both documents hold the same items with the same annotations. */
  equals(other: BootstrapDocument): boolean {
    return (
      this.items.length === other.items.length &&
      this.items.every((item, i) => {
        const o = other.items[i];
        return (
          o !== undefined && kindEquals(item.kind, o.kind) && item.annotations.equals(o.annotations)
        );
      })
    );
  }

  // The validator makes the error checking here redundant in theory; the
  // original checks anyway in case the validator is wrong, and so do we.
  private apply(op: DocOp): Item[] {
    const result: Item[] = [];
    const oldItems = this.items;
    let oldIndex = 0;
    let annotationUpdates = AnnotationsUpdate.EMPTY;
    // Annotations, before this operation, of the last item passed.
    let inherited = AnnotationMap.EMPTY;
    const next = (what: string): Item => {
      const item = oldItems[oldIndex];
      if (item === undefined) {
        throw OperationError.doesNotApply(
          `${what} past end of document, of size: ${oldItems.length}`,
        );
      }
      oldIndex++;
      return item;
    };
    for (const c of op.components) {
      switch (c.type) {
        case "annotationBoundary":
          annotationUpdates = annotationUpdates.composeWithBoundary(c.boundary);
          break;
        case "characters": {
          const annotations = inherited.updateWith(annotationUpdates);
          for (let i = 0; i < c.characters.length; i++) {
            result.push({
              kind: { type: "character", unit: c.characters.charCodeAt(i) },
              annotations,
            });
          }
          break;
        }
        case "elementStart":
          result.push({
            kind: { type: "elementStart", tag: c.tag, attributes: c.attributes },
            annotations: inherited.updateWith(annotationUpdates),
          });
          break;
        case "elementEnd":
          result.push({ kind: ELEMENT_END, annotations: inherited.updateWith(annotationUpdates) });
          break;
        case "deleteCharacters":
          for (let i = 0; i < c.characters.length; i++) {
            const item = next("deletion");
            if (item.kind.type !== "character") {
              throw OperationError.doesNotApply("not at a character");
            }
            if (item.kind.unit !== c.characters.charCodeAt(i)) {
              throw OperationError.doesNotApply("mismatched deleted characters");
            }
            inherited = item.annotations;
          }
          break;
        case "deleteElementStart": {
          const item = next("deletion");
          if (item.kind.type !== "elementStart") {
            throw OperationError.doesNotApply("not at an element start");
          }
          inherited = item.annotations;
          break;
        }
        case "deleteElementEnd": {
          const item = next("deletion");
          if (item.kind.type !== "elementEnd") {
            throw OperationError.doesNotApply("not at an element end");
          }
          inherited = item.annotations;
          break;
        }
        case "retain":
          for (let i = 0; i < c.itemCount; i++) {
            const item = next("retain");
            inherited = item.annotations;
            result.push({
              kind: item.kind,
              annotations: item.annotations.updateWith(annotationUpdates),
            });
          }
          break;
        case "replaceAttributes": {
          const item = next("attribute change");
          if (item.kind.type !== "elementStart") {
            throw OperationError.doesNotApply("not at an element start");
          }
          inherited = item.annotations;
          result.push({
            kind: { type: "elementStart", tag: item.kind.tag, attributes: c.newAttributes },
            annotations: item.annotations.updateWith(annotationUpdates),
          });
          break;
        }
        case "updateAttributes": {
          const item = next("attribute change");
          if (item.kind.type !== "elementStart") {
            throw OperationError.doesNotApply("not at an element start");
          }
          inherited = item.annotations;
          result.push({
            kind: {
              type: "elementStart",
              tag: item.kind.tag,
              attributes: item.kind.attributes.updateWith(c.update),
            },
            annotations: item.annotations.updateWith(annotationUpdates),
          });
          break;
        }
      }
    }
    const remaining = oldItems.length - oldIndex;
    if (remaining > 0) {
      throw OperationError.doesNotApply(`missing retain to end of document (${remaining} items)`);
    }
    if (!annotationUpdates.isEmpty()) {
      throw OperationError.doesNotApply(
        `unended annotations at end of operation: ${annotationUpdates.toString()}`,
      );
    }
    return result;
  }

  private rebuildIndex(): void {
    const enclosing: (number | null)[] = [];
    const open: number[] = [];
    this.items.forEach((item, index) => {
      enclosing.push(open[open.length - 1] ?? null);
      if (item.kind.type === "elementStart") {
        open.push(index);
      } else if (item.kind.type === "elementEnd") {
        open.pop();
      }
    });
    this.enclosing = enclosing;
  }

  private tagAt(index: number | null | undefined): string | null {
    if (index === null || index === undefined) {
      return null;
    }
    const kind = this.items[index]?.kind;
    return kind !== undefined && kind.type === "elementStart" ? kind.tag : null;
  }

  /**
   * The document as an operation on the empty document, in normalized form:
   * an annotation boundary wherever the annotations change.
   */
  asOperation(): DocInitialization {
    const components: DocOpComponent[] = [];
    let text: number[] = [];
    const flushText = (): void => {
      if (text.length > 0) {
        let characters = "";
        // In chunks, to stay within the argument limit of fromCharCode.
        for (let i = 0; i < text.length; i += 4096) {
          characters += String.fromCharCode(...text.slice(i, i + 4096));
        }
        components.push({ type: "characters", characters });
        text = [];
      }
    };
    let previous = AnnotationMap.EMPTY;
    for (const item of this.items) {
      if (!item.annotations.equals(previous)) {
        flushText();
        components.push({
          type: "annotationBoundary",
          boundary: boundaryBetween(previous, item.annotations),
        });
        previous = item.annotations;
      }
      switch (item.kind.type) {
        case "character":
          text.push(item.kind.unit);
          break;
        case "elementStart":
          flushText();
          components.push({
            type: "elementStart",
            tag: item.kind.tag,
            attributes: item.kind.attributes,
          });
          break;
        case "elementEnd":
          flushText();
          components.push({ type: "elementEnd" });
          break;
      }
    }
    flushText();
    if (!previous.isEmpty()) {
      components.push({
        type: "annotationBoundary",
        boundary: boundaryBetween(previous, AnnotationMap.EMPTY),
      });
    }
    return DocInitialization.uncheckedInitialization(components);
  }

  /** The canonical XML form of the document. */
  toXmlString(): string {
    // A document only ever holds balanced elements, so this cannot fail.
    try {
      return toXmlString(this.asOperation());
    } catch {
      return "";
    }
  }

  toString(): string {
    return `BootstrapDocument: ${this.toXmlString()}`;
  }

  // AutomatonDocument

  elementStartingAt(pos: number): string | null {
    return this.tagAt(pos);
  }

  attributesAt(pos: number): Attributes | null {
    const kind = this.items[pos]?.kind;
    return kind !== undefined && kind.type === "elementStart" ? kind.attributes : null;
  }

  elementEndingAt(pos: number): string | null {
    const kind = this.items[pos]?.kind;
    return kind !== undefined && kind.type === "elementEnd" ? this.tagAt(this.enclosing[pos]) : null;
  }

  charAt(pos: number): number | null {
    const kind = this.items[pos]?.kind;
    return kind !== undefined && kind.type === "character" ? kind.unit : null;
  }

  nthEnclosingElementTag(insertionPoint: number, depth: number): string | null {
    let element = this.enclosing[insertionPoint];
    for (let i = 0; i < depth; i++) {
      if (element === null || element === undefined) {
        return null;
      }
      element = this.enclosing[element];
    }
    return this.tagAt(element);
  }

  remainingCharactersInElement(insertionPoint: number): number {
    let count = 0;
    for (let i = insertionPoint; i < this.items.length; i++) {
      if (this.items[i]?.kind.type !== "character") {
        break;
      }
      count++;
    }
    return count;
  }

  annotationsAt(pos: number): AnnotationMap {
    return this.items[pos]?.annotations ?? AnnotationMap.EMPTY;
  }

  getAnnotation(pos: number, key: string): string | null {
    return this.items[pos]?.annotations.get(key) ?? null;
  }

  length(): number {
    return this.items.length;
  }

  firstAnnotationChange(
    start: number,
    end: number,
    key: string,
    fromValue: string | null,
  ): number | null {
    const limit = Math.min(end, this.items.length);
    for (let pos = start; pos < limit; pos++) {
      if (this.items[pos]!.annotations.get(key) !== fromValue) {
        return pos;
      }
    }
    return null;
  }
}

/**
 * The boundary that turns the open annotation set `from` into `to`, both
 * sorted by key.
 */
function boundaryBetween(fromMap: AnnotationMap, toMap: AnnotationMap): AnnotationBoundaryMap {
  const ends: string[] = [];
  const changes: AnnotationChange[] = [];
  const from = fromMap.entries;
  const to = toMap.entries;
  let i = 0;
  let j = 0;
  while (i < from.length || j < to.length) {
    const a = from[i];
    const b = to[j];
    const order = a !== undefined && b !== undefined ? cmpUtf16(a.name, b.name) : a !== undefined ? -1 : 1;
    if (order < 0) {
      ends.push(a!.name);
      i++;
    } else if (order > 0) {
      changes.push({ key: b!.name, oldValue: null, newValue: b!.value });
      j++;
    } else {
      if (a!.value !== b!.value) {
        changes.push({ key: b!.name, oldValue: null, newValue: b!.value });
      }
      i++;
      j++;
    }
  }
  return AnnotationBoundaryMap.unchecked(ends, changes);
}
