// Non-invertible document operations and their conversion to `DocOp`s.
//
// A `Nindo` says what to do to a document without saying what was there
// before: deletions carry only sizes, attribute updates only new values,
// annotation changes only the value to set. `Nindo.toDocOp` fills in the old
// state from the document the operation applies to, following
// `IndexedDocumentImpl.NonInvertibleCursor` step by step so that the
// resulting operation is the one the original would have emitted.
//
// TypeScript port of the Rust crate `resonator-model` (module `doc::nindo`),
// itself derived from Apache Wave (`operation.Nindo`,
// `indexed.IndexedDocumentImpl`), licensed under the Apache License,
// Version 2.0.

import {
  AnnotationBoundaryMap,
  AnnotationsNormalizer,
  Attributes,
  AttributesUpdate,
  type AutomatonDocument,
  BufferSink,
  DocOp,
  type DocOpComponent,
} from "../docop";
import { isHighSurrogate, isLowSurrogate } from "../docop/utf16";
import { DocError } from "./error";

/** A new attribute value by name; `null` removes the attribute. */
export type NindoAttributeUpdate = readonly [name: string, newValue: string | null];

/** One component of a {@link Nindo}. */
export type NindoComponent =
  /** Moves over `count` items, applying the open annotation changes to them. */
  | { readonly type: "skip"; readonly count: number }
  | { readonly type: "characters"; readonly characters: string }
  | { readonly type: "elementStart"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "elementEnd" }
  /** Deletes this many UTF-16 code units of text. */
  | { readonly type: "deleteCharacters"; readonly count: number }
  | { readonly type: "deleteElementStart" }
  | { readonly type: "deleteElementEnd" }
  /** Replaces all attributes of the element starting here. */
  | { readonly type: "replaceAttributes"; readonly attributes: Attributes }
  /** Sets or removes (`null`) attributes of the element starting here. */
  | { readonly type: "updateAttributes"; readonly updates: readonly NindoAttributeUpdate[] }
  /** From here on, sets annotation `key` to `value` (`null` removes it). */
  | { readonly type: "startAnnotation"; readonly key: string; readonly value: string | null }
  | { readonly type: "endAnnotation"; readonly key: string };

function nindoComponentEquals(a: NindoComponent, b: NindoComponent): boolean {
  switch (a.type) {
    case "skip":
    case "deleteCharacters":
      return b.type === a.type && a.count === b.count;
    case "characters":
      return b.type === a.type && a.characters === b.characters;
    case "elementStart":
      return b.type === a.type && a.tag === b.tag && a.attributes.equals(b.attributes);
    case "elementEnd":
    case "deleteElementStart":
    case "deleteElementEnd":
      return b.type === a.type;
    case "replaceAttributes":
      return b.type === a.type && a.attributes.equals(b.attributes);
    case "updateAttributes":
      return (
        b.type === a.type &&
        a.updates.length === b.updates.length &&
        a.updates.every((u, i) => u[0] === b.updates[i]?.[0] && u[1] === b.updates[i]?.[1])
      );
    case "startAnnotation":
      return b.type === a.type && a.key === b.key && a.value === b.value;
    case "endAnnotation":
      return b.type === a.type && a.key === b.key;
  }
}

/**
 * A non-invertible document operation. Unlike a `DocOp` it need not span the
 * whole document: the rest is implicitly skipped.
 */
export class Nindo {
  readonly components: readonly NindoComponent[];

  constructor(components: readonly NindoComponent[] = []) {
    this.components = components;
  }

  static builder(): NindoBuilder {
    return new NindoBuilder();
  }

  /**
   * Sets annotation `key` to `value` over `start..end`
   * (`Nindo.setAnnotation`). Empty if the range is.
   */
  static setAnnotation(start: number, end: number, key: string, value: string | null): Nindo {
    const b = new NindoBuilder();
    if (start < end) {
      b.skip(start);
      b.startAnnotation(key, value);
      b.skip(end - start);
      b.endAnnotation(key);
    }
    return b.build();
  }

  /**
   * The non-invertible form of `op` (`Nindo.fromDocOp`), optionally without
   * a trailing skip.
   */
  static fromDocOp(op: DocOp, removeTrailingSkip: boolean): Nindo {
    const b = new NindoBuilder();
    for (const c of op.components) {
      switch (c.type) {
        case "annotationBoundary":
          for (const key of c.boundary.endKeys) {
            b.endAnnotation(key);
          }
          for (const change of c.boundary.changes) {
            b.startAnnotation(change.key, change.newValue);
          }
          break;
        case "characters":
          b.characters(c.characters);
          break;
        case "elementStart":
          b.elementStart(c.tag, c.attributes);
          break;
        case "elementEnd":
          b.elementEnd();
          break;
        case "retain":
          b.skip(c.itemCount);
          break;
        case "deleteCharacters":
          b.deleteCharacters(c.characters.length);
          break;
        case "deleteElementStart":
          b.deleteElementStart();
          break;
        case "deleteElementEnd":
          b.deleteElementEnd();
          break;
        case "replaceAttributes":
          b.replaceAttributes(c.newAttributes);
          break;
        case "updateAttributes":
          b.updateAttributes(c.update.updates.map((u) => [u.name, u.newValue] as const));
          break;
      }
    }
    const components = [...b.build().components];
    if (removeTrailingSkip && components[components.length - 1]?.type === "skip") {
      components.pop();
    }
    return new Nindo(components);
  }

  /**
   * Converts the operation to the invertible operation that has the same
   * effect on `doc`, as the original document implementation does when it
   * consumes a `Nindo`. Throws {@link DocError} if it does not apply.
   *
   * The result is not range-normalized: like the original's, it can hold
   * adjacent retains. It is not validated against a schema either.
   *
   * Checked against the original with random operations that mix
   * insertions, deletions, attribute changes and annotation changes
   * (`testdata/NindoGoldenGenerator.java`).
   */
  toDocOp(doc: AutomatonDocument): DocOp {
    const cursor = new Converter(doc);
    for (const component of this.components) {
      cursor.component(component);
    }
    return cursor.finish();
  }

  equals(other: Nindo): boolean {
    return (
      this.components.length === other.components.length &&
      this.components.every((c, i) => {
        const o = other.components[i];
        return o !== undefined && nindoComponentEquals(c, o);
      })
    );
  }
}

/**
 * Builds a {@link Nindo}. Zero-length skips, insertions and deletions are
 * dropped, as `Nindo.Builder` does for empty text. Methods return the
 * builder for chaining.
 */
export class NindoBuilder {
  private components: NindoComponent[] = [];

  /** Builds the operation and resets the builder. */
  build(): Nindo {
    const components = this.components;
    this.components = [];
    return new Nindo(components);
  }

  skip(count: number): this {
    if (count > 0) {
      this.components.push({ type: "skip", count });
    }
    return this;
  }

  characters(characters: string): this {
    if (characters.length > 0) {
      this.components.push({ type: "characters", characters });
    }
    return this;
  }

  elementStart(tag: string, attributes: Attributes = Attributes.EMPTY): this {
    this.components.push({ type: "elementStart", tag, attributes });
    return this;
  }

  elementEnd(): this {
    this.components.push({ type: "elementEnd" });
    return this;
  }

  deleteCharacters(count: number): this {
    if (count > 0) {
      this.components.push({ type: "deleteCharacters", count });
    }
    return this;
  }

  deleteElementStart(): this {
    this.components.push({ type: "deleteElementStart" });
    return this;
  }

  deleteElementEnd(): this {
    this.components.push({ type: "deleteElementEnd" });
    return this;
  }

  replaceAttributes(attributes: Attributes): this {
    this.components.push({ type: "replaceAttributes", attributes });
    return this;
  }

  updateAttributes(updates: Iterable<NindoAttributeUpdate>): this {
    this.components.push({
      type: "updateAttributes",
      updates: Array.from(updates, ([name, value]) => [name, value] as const),
    });
    return this;
  }

  startAnnotation(key: string, value: string | null): this {
    this.components.push({ type: "startAnnotation", key, value });
    return this;
  }

  endAnnotation(key: string): this {
    this.components.push({ type: "endAnnotation", key });
    return this;
  }
}

/** Annotation values by key; `null` is "set to nothing". */
type Values = Map<string, string | null>;

type Event =
  /** An annotation change starting at an index, with the old value. */
  | { readonly type: "start"; readonly index: number; readonly key: string; readonly oldValue: string | null }
  | { readonly type: "end"; readonly index: number; readonly key: string };

/** Keys in UTF-16 order (the iteration order of the Rust `BTreeMap`s). */
function sortedKeys(keys: Iterable<string>): string[] {
  return [...keys].sort();
}

/** Local annotations (keys starting with `@`) are never inherited. */
function isLocal(key: string): boolean {
  return key.startsWith("@");
}

function toMap(annotations: Attributes): Map<string, string> {
  const map = new Map<string, string>();
  for (const a of annotations.entries) {
    map.set(a.name, a.value);
  }
  return map;
}

function applyValues(base: ReadonlyMap<string, string>, values: Values): Map<string, string> {
  const result = new Map(base);
  for (const [key, value] of values) {
    if (value !== null) {
      result.set(key, value);
    } else {
      result.delete(key);
    }
  }
  return result;
}

function sameValues(a: readonly (string | null)[] | null, b: readonly (string | null)[]): boolean {
  return a !== null && a.length === b.length && a.every((value, i) => value === b[i]);
}

/** Whether the code units form text without unpaired surrogates. */
function isWellFormedUtf16(units: readonly number[], from: number, to: number): boolean {
  for (let i = from; i < to; i++) {
    const unit = units[i]!;
    if (isHighSurrogate(unit)) {
      if (i + 1 >= to || !isLowSurrogate(units[i + 1]!)) {
        return false;
      }
      i++;
    } else if (isLowSurrogate(unit)) {
      return false;
    }
  }
  return true;
}

/**
 * The state of `IndexedDocumentImpl.NonInvertibleCursor`, over an unmodified
 * document: positions are positions in the old document.
 */
class Converter {
  private readonly doc: AutomatonDocument;
  private readonly size: number;
  /** The next old item to process. */
  private pos = 0;
  private readonly out = new AnnotationsNormalizer(new BufferSink());
  private readonly requested: Values = new Map();
  private newValues: Values = new Map();
  private endKeys = new Set<string>();
  private deletionValues: Values = new Map();
  /** Old annotations of the last old item passed (skipped or deleted). */
  private inherited = new Map<string, string>();
  /** New annotations of the last item of the new document so far. */
  private left = new Map<string, string>();
  /** The annotation keys in use in the document. Computed on demand. */
  private docKeys: Set<string> | null = null;
  /**
   * Keys this operation has set on at least one item so far. Together with
   * the document's keys these are the keys the original's annotation set
   * "knows" at this point: it learns a key only when an open annotation
   * that covers something is ended or replaced.
   */
  private readonly appliedKeys = new Set<string>();
  /**
   * For each open annotation, its value and the number of items passed or
   * inserted when it was started.
   */
  private readonly open = new Map<string, { readonly value: string | null; readonly start: number }>();
  /** The number of items passed or inserted so far. */
  private cursor = 0;
  private deletionDepth = 0;
  private insertionDepth = 0;

  constructor(doc: AutomatonDocument) {
    this.doc = doc;
    this.size = doc.length();
  }

  private start(key: string, oldValue: string | null, newValue: string | null): void {
    this.out.push({
      type: "annotationBoundary",
      boundary: AnnotationBoundaryMap.unchecked([], [{ key, oldValue, newValue }]),
    });
  }

  private end(key: string): void {
    this.out.push({
      type: "annotationBoundary",
      boundary: AnnotationBoundaryMap.unchecked([key], []),
    });
  }

  private oldAnnotations(pos: number): Map<string, string> {
    return toMap(this.doc.annotationsAt(pos));
  }

  private checkRemaining(count: number, what: string): void {
    // Rust's unsigned counts can be neither negative nor fractional.
    if (!Number.isInteger(count) || count < 0) {
      throw DocError.invalidOperation(`${what} of invalid size ${count}`);
    }
    if (count > this.size - this.pos) {
      throw DocError.invalidOperation(
        `${what} past end of document [location: ${this.pos + count}] [doc size: ${this.size}]`,
      );
    }
  }

  component(component: NindoComponent): void {
    switch (component.type) {
      case "skip":
        this.skip(component.count);
        break;
      case "characters":
        this.beginInsert();
        this.out.push({ type: "characters", characters: component.characters });
        this.inserted(component.characters.length);
        break;
      case "elementStart":
        this.beginInsert();
        this.out.push({
          type: "elementStart",
          tag: component.tag,
          attributes: component.attributes,
        });
        this.insertionDepth += 1;
        this.inserted(1);
        break;
      case "elementEnd":
        if (this.insertionDepth === 0) {
          throw DocError.invalidOperation("element end without a matching start");
        }
        this.beginInsert();
        this.out.push({ type: "elementEnd" });
        this.insertionDepth -= 1;
        this.inserted(1);
        break;
      case "deleteCharacters":
        this.deleteCharacters(component.count);
        break;
      case "deleteElementStart":
        this.deleteElementStart();
        break;
      case "deleteElementEnd":
        this.deleteElementEnd();
        break;
      case "replaceAttributes": {
        const oldAttributes = this.beginAttributeChange();
        this.out.push({
          type: "replaceAttributes",
          oldAttributes,
          newAttributes: component.attributes,
        });
        break;
      }
      case "updateAttributes": {
        const oldAttributes = this.beginAttributeChange();
        const updates = component.updates.map(([name, newValue]) => ({
          name,
          oldValue: oldAttributes.get(name),
          newValue,
        }));
        let update: AttributesUpdate;
        try {
          update = AttributesUpdate.fromUnsorted(updates);
        } catch (e) {
          throw DocError.invalidOperation(e instanceof Error ? e.message : String(e));
        }
        this.out.push({ type: "updateAttributes", update });
        break;
      }
      case "startAnnotation": {
        const { key, value } = component;
        this.endKeys.delete(key);
        const open = this.open.get(key);
        if (open === undefined || open.value !== value) {
          this.close(key);
          this.open.set(key, { value, start: this.cursor });
        }
        this.requested.set(key, value);
        this.newValues.set(key, value);
        break;
      }
      case "endAnnotation": {
        const key = component.key;
        if (!this.requested.delete(key)) {
          throw DocError.invalidOperation(`end of annotation ${key} that was not started`);
        }
        this.newValues.delete(key);
        if (this.deletionValues.has(key)) {
          this.endKeys.add(key);
        }
        this.close(key);
        break;
      }
    }
  }

  /** Ends an open annotation as the annotation set sees it. */
  private close(key: string): void {
    const open = this.open.get(key);
    if (open !== undefined) {
      this.open.delete(key);
      if (this.cursor > open.start) {
        this.appliedKeys.add(key);
      }
    }
  }

  private inserted(count: number): void {
    this.cursor += count;
    this.left = applyValues(this.inherited, this.requested);
  }

  private closeEndKeys(): void {
    const keys = sortedKeys(this.endKeys);
    this.endKeys = new Set();
    for (const key of keys) {
      this.end(key);
    }
  }

  private beginInsert(): void {
    const newValues = this.newValues;
    this.newValues = new Map();
    for (const key of sortedKeys(newValues.keys())) {
      const inherited = isLocal(key) ? null : (this.inherited.get(key) ?? null);
      this.start(key, inherited, newValues.get(key) ?? null);
    }
    this.deletionValues = new Map(this.requested);
    this.closeEndKeys();
  }

  /** Expects `pos` to be inside the document. */
  private beginUpdate(): void {
    const current = this.oldAnnotations(this.pos);
    for (const key of sortedKeys(this.requested.keys())) {
      this.start(key, current.get(key) ?? null, this.requested.get(key) ?? null);
    }
    this.newValues.clear();
    this.deletionValues = new Map(this.requested);
    this.closeEndKeys();
  }

  private passed(count: number): void {
    this.pos += count;
    this.cursor += count;
    this.inherited = this.oldAnnotations(this.pos - 1);
    this.left = applyValues(this.inherited, this.requested);
  }

  private maybeRenewAnnotation(key: string): void {
    if (!this.newValues.has(key) && this.requested.has(key)) {
      this.newValues.set(key, this.requested.get(key) ?? null);
    }
  }

  private skip(count: number): void {
    if (count === 0) {
      return;
    }
    this.checkRemaining(count, "Retain");
    this.beginUpdate();
    const finalPos = this.pos + count;
    let current = this.pos;
    if (this.requested.size > 0) {
      const events: Event[] = [];
      const open = new Set<string>();
      const keys = sortedKeys(this.requested.keys());
      let previous: (string | null)[] | null = null;
      for (let index = this.pos; index < finalPos; index++) {
        const annotations = this.doc.annotationsAt(index);
        const here = keys.map((key) => annotations.get(key));
        if (sameValues(previous, here)) {
          continue;
        }
        keys.forEach((key, i) => {
          const oldValue = here[i] ?? null;
          if ((this.requested.get(key) ?? null) !== oldValue) {
            events.push({ type: "start", index, key, oldValue });
            open.add(key);
          } else if (open.delete(key)) {
            events.push({ type: "end", index, key });
          }
        });
        previous = here;
      }
      for (const key of sortedKeys(open)) {
        events.push({ type: "end", index: finalPos, key });
      }
      for (const event of events) {
        if (event.index > current) {
          this.out.retain(event.index - current);
          current = event.index;
        }
        if (event.type === "end") {
          this.maybeRenewAnnotation(event.key);
          this.end(event.key);
        } else {
          this.start(event.key, event.oldValue, this.requested.get(event.key) ?? null);
        }
      }
    }
    if (current < finalPos) {
      this.out.retain(finalPos - current);
    }
    this.passed(count);
  }

  private beginAttributeChange(): Attributes {
    const attributes = this.doc.attributesAt(this.pos);
    if (attributes === null) {
      throw DocError.invalidOperation(`no element start to change attributes of at ${this.pos}`);
    }
    this.beginUpdate();
    this.passed(1);
    return attributes;
  }

  private knownKeys(): string[] {
    if (this.docKeys === null) {
      const keys = new Set<string>();
      let previous = Attributes.EMPTY;
      for (let index = 0; index < this.size; index++) {
        const annotations = this.doc.annotationsAt(index);
        if (annotations !== previous && !annotations.equals(previous)) {
          for (const a of annotations.entries) {
            keys.add(a.name);
          }
          previous = annotations;
        }
      }
      this.docKeys = keys;
    }
    return sortedKeys(new Set([...this.docKeys, ...this.appliedKeys]));
  }

  /**
   * The annotation events for deleting `size` items at the current position
   * (`deleteAnnotations`): every known annotation is restarted at each
   * annotation interval of the deleted range and ended after it.
   */
  private deleteAnnotations(size: number): Event[] {
    const start = this.pos;
    const end = this.pos + size;
    const keys = this.knownKeys();
    const deletionKeys = sortedKeys(this.deletionValues.keys());
    const events: Event[] = [];
    const open = new Set<string>();
    let previous: (string | null)[] | null = null;
    for (let index = start; index < end; index++) {
      const annotations = this.doc.annotationsAt(index);
      const here = keys.map((key) => annotations.get(key));
      if (sameValues(previous, here)) {
        continue;
      }
      for (const key of deletionKeys) {
        events.push({ type: "start", index, key, oldValue: annotations.get(key) });
        open.add(key);
      }
      keys.forEach((key, i) => {
        if (!this.deletionValues.has(key)) {
          events.push({ type: "start", index, key, oldValue: here[i] ?? null });
          open.add(key);
        }
      });
      previous = here;
    }
    for (const key of sortedKeys(open)) {
      events.push({ type: "end", index: end, key });
    }
    return events;
  }

  private leftNeighbourAnnotation(key: string): string | null {
    return this.deletionValues.has(key)
      ? (this.deletionValues.get(key) ?? null)
      : (this.left.get(key) ?? null);
  }

  private deleted(count: number): void {
    this.pos += count;
    this.inherited = this.oldAnnotations(this.pos - 1);
  }

  private deletionEvent(event: Event): void {
    if (event.type === "end") {
      this.maybeRenewAnnotation(event.key);
      this.end(event.key);
    } else {
      this.start(event.key, event.oldValue, this.leftNeighbourAnnotation(event.key));
    }
  }

  private deleteCharacters(count: number): void {
    if (count === 0) {
      return;
    }
    this.checkRemaining(count, "Deletion");
    const units: number[] = [];
    for (let index = this.pos; index < this.pos + count; index++) {
      const unit = this.doc.charAt(index);
      if (unit === null) {
        throw DocError.invalidOperation(`no character to delete at ${index}`);
      }
      units.push(unit);
    }
    const piece = (from: number, to: number): string => {
      if (!isWellFormedUtf16(units, from, to)) {
        throw DocError.invalidOperation("deletion splits a surrogate pair");
      }
      let characters = "";
      // In chunks, to stay within the argument limit of fromCharCode.
      for (let i = from; i < to; i += 4096) {
        characters += String.fromCharCode(...units.slice(i, Math.min(i + 4096, to)));
      }
      return characters;
    };
    const events = this.deleteAnnotations(count);
    let index = 0;
    for (const event of events) {
      const newIndex = event.index - this.pos;
      if (newIndex > index) {
        this.out.push({ type: "deleteCharacters", characters: piece(index, newIndex) });
        index = newIndex;
      }
      this.deletionEvent(event);
    }
    if (index < count) {
      this.out.push({ type: "deleteCharacters", characters: piece(index, count) });
    }
    this.deleted(count);
  }

  /** Deletes one element start or end (`doSingleDelete`). */
  private singleDelete(component: DocOpComponent): void {
    const events = this.deleteAnnotations(1);
    let pending: DocOpComponent | null = component;
    for (const event of events) {
      if (event.index > this.pos && pending !== null) {
        this.out.push(pending);
        pending = null;
      }
      this.deletionEvent(event);
    }
    if (pending !== null) {
      this.out.push(pending);
    }
    this.deleted(1);
  }

  private deleteElementStart(): void {
    const tag = this.doc.elementStartingAt(this.pos);
    const attributes = this.doc.attributesAt(this.pos);
    if (tag === null || attributes === null) {
      throw DocError.invalidOperation(`no element to delete at ${this.pos}`);
    }
    this.singleDelete({ type: "deleteElementStart", tag, attributes });
    this.deletionDepth += 1;
  }

  private deleteElementEnd(): void {
    if (this.deletionDepth === 0 || this.doc.elementEndingAt(this.pos) === null) {
      throw DocError.invalidOperation(`no element end to delete at ${this.pos}`);
    }
    this.singleDelete({ type: "deleteElementEnd" });
    this.deletionDepth -= 1;
  }

  finish(): DocOp {
    const unended = sortedKeys(this.requested.keys())[0];
    if (unended !== undefined) {
      throw DocError.invalidOperation(`annotation ${unended} still open at end of operation`);
    }
    if (this.deletionDepth !== 0 || this.insertionDepth !== 0) {
      throw DocError.invalidOperation("unbalanced elements at end of operation");
    }
    this.closeEndKeys();
    const remaining = this.size - this.pos;
    if (remaining > 0) {
      this.out.retain(remaining);
    }
    return DocOp.unchecked(this.out.finish());
  }
}

/**
 * Sorted attributes from name/value pairs, the later of two equal names
 * winning (a Java `Map` built by successive `put`s).
 */
export function attributesFromPairs(
  pairs: Iterable<readonly [name: string, value: string]>,
): Attributes {
  const map = new Map<string, string>();
  for (const [name, value] of pairs) {
    map.set(name, value);
  }
  return Attributes.fromUnsortedUnchecked(Array.from(map, ([name, value]) => ({ name, value })));
}
