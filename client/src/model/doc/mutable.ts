// DOM-like editing of a document, emitting document operations.
//
// TypeScript port of the Rust crate `resonator-model` (module `doc::mutable`),
// itself derived from Apache Wave (`MutableDocumentImpl`, `Annotations`),
// licensed under the Apache License, Version 2.0.

import {
  type AnnotationMap,
  Attributes,
  BootstrapDocument,
  type DocOp,
  normalize,
  validateAnnotationKey,
} from "../docop";
import { DocError } from "./error";
import { Nindo, type NindoAttributeUpdate, NindoBuilder, attributesFromPairs } from "./nindo";
import { DocView, type NodeId, type Point } from "./view";
import type { XmlFragment } from "./xmlFragment";

/**
 * Where a {@link MutableDocument} reads its document from and sends its
 * operations to (the original's `OperationSequencer` together with the
 * document substrate).
 */
export interface DocumentSink {
  /** The current document. */
  document(): BootstrapDocument;

  /**
   * Applies an operation to the document and passes it on. Throws
   * {@link DocError} if the operation is rejected, leaving the document
   * unchanged.
   */
  consume(op: DocOp): void;
}

/** A document on its own that records the operations applied to it. */
export class StandaloneDocument implements DocumentSink {
  private readonly doc: BootstrapDocument;
  private recorded: DocOp[] = [];

  constructor(document: BootstrapDocument = new BootstrapDocument()) {
    this.doc = document;
  }

  /** The operations consumed so far, oldest first. */
  ops(): readonly DocOp[] {
    return this.recorded;
  }

  /** Returns the operations consumed so far and forgets them. */
  takeOps(): DocOp[] {
    const ops = this.recorded;
    this.recorded = [];
    return ops;
  }

  mutable(): MutableDocument {
    return new MutableDocument(this);
  }

  document(): BootstrapDocument {
    return this.doc;
  }

  consume(op: DocOp): void {
    try {
      this.doc.consume(op);
    } catch (e) {
      throw DocError.lift(e);
    }
    this.recorded.push(op);
  }
}

/** An annotation value over a range of locations. */
export interface RangedAnnotation {
  readonly key: string;
  /** `null` where the key is not set. */
  readonly value: string | null;
  readonly start: number;
  readonly end: number;
}

/** The annotations of a document, read by location. */
export abstract class AnnotationReader {
  abstract annotationDocument(): BootstrapDocument;

  /** The value of `key` on the item at `location` (`getAnnotation`). */
  annotation(location: number, key: string): string | null {
    return this.annotationDocument().getAnnotation(location, key);
  }

  /** All annotations of the item at `location`; empty beyond the document. */
  annotationsAt(location: number): AnnotationMap {
    return this.annotationDocument().annotationsAt(location);
  }

  /**
   * The first location in `start..end` where `key` does not have
   * `fromValue` (`firstAnnotationChange`; `null` for the original's -1).
   */
  findAnnotationChange(
    start: number,
    end: number,
    key: string,
    fromValue: string | null,
  ): number | null {
    return this.annotationDocument().firstAnnotationChange(start, end, key, fromValue);
  }

  /** The annotation keys in use anywhere in the document, sorted. */
  annotationKeys(): string[] {
    const doc = this.annotationDocument();
    const keys = new Set<string>();
    let previous: AnnotationMap | null = null;
    const length = doc.length();
    for (let location = 0; location < length; location++) {
      const annotations = doc.annotationsAt(location);
      if (previous === null || (annotations !== previous && !annotations.equals(previous))) {
        for (const a of annotations.entries) {
          keys.add(a.name);
        }
        previous = annotations;
      }
    }
    return [...keys].sort();
  }

  /**
   * For each key (all keys in use if `keys` is `null`), the maximal ranges
   * of equal value that intersect `start..end`, unset ranges included,
   * ordered by start and then key (`rangedAnnotations`; the original leaves
   * the order of ranges with equal starts unspecified). Ranges are not
   * clipped to `start..end`.
   */
  rangedAnnotations(
    start: number,
    end: number,
    keys: readonly string[] | null = null,
  ): RangedAnnotation[] {
    const doc = this.annotationDocument();
    const size = doc.length();
    const limit = Math.min(end, size);
    const result: RangedAnnotation[] = [];
    if (!(start >= 0) || start >= limit) {
      return result;
    }
    for (const key of keys ?? this.annotationKeys()) {
      let rangeStart = start;
      let value = doc.getAnnotation(start, key);
      while (rangeStart > 0 && doc.getAnnotation(rangeStart - 1, key) === value) {
        rangeStart -= 1;
      }
      while (rangeStart < limit) {
        const rangeEnd = doc.firstAnnotationChange(rangeStart, size, key, value) ?? size;
        result.push({ key, value, start: rangeStart, end: rangeEnd });
        rangeStart = rangeEnd;
        value = doc.getAnnotation(rangeStart, key);
      }
    }
    // Stable, so equal ranges keep the order of `keys`.
    result.sort((a, b) => a.start - b.start || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
    return result;
  }
}

class DocumentAnnotations extends AnnotationReader {
  constructor(private readonly doc: BootstrapDocument) {
    super();
  }

  annotationDocument(): BootstrapDocument {
    return this.doc;
  }
}

/** An {@link AnnotationReader} over a plain document. */
export function annotationsOf(doc: BootstrapDocument): AnnotationReader {
  return new DocumentAnnotations(doc);
}

/**
 * A document that can be read as an element tree and edited through
 * DOM-like calls, each of which sends one document operation to the sink
 * (`MutableDocumentImpl`).
 *
 * Every edit rebuilds the tree, so a {@link NodeId} obtained before an edit
 * must not be used after it. Locations stay meaningful as far as the edit
 * did not shift them. If the sink's document is changed by someone else
 * (e.g. a remote operation), call {@link refresh} before the next use.
 *
 * Like the original, an edit that changes nothing in the document (empty
 * text, an empty fragment, an empty range inside a multi-element deletion)
 * still emits an operation, one that only retains.
 *
 * The operations emitted are normalized; they equal the original's after
 * normalization. (The original's raw operations split retains and deletions
 * in ways that depend on the history of its DOM.)
 *
 * All edits throw {@link DocError}; a failed edit emits nothing and leaves
 * the document unchanged (a multi-section {@link deleteRange} may have
 * emitted earlier sections).
 */
export class MutableDocument extends AnnotationReader {
  private readonly sink: DocumentSink;
  private currentView: DocView;

  constructor(sink: DocumentSink) {
    super();
    this.sink = sink;
    this.currentView = new DocView(sink.document());
  }

  annotationDocument(): BootstrapDocument {
    return this.sink.document();
  }

  /** The element tree as of the last edit (or {@link refresh}). */
  view(): DocView {
    return this.currentView;
  }

  /**
   * Rebuilds the tree from the sink's document. Needed only after the
   * document was changed other than through this object. Invalidates node
   * ids. (Not in the original, where a `MutableDocument` is short-lived.)
   */
  refresh(): void {
    this.currentView = new DocView(this.sink.document());
  }

  document(): BootstrapDocument {
    return this.sink.document();
  }

  size(): number {
    return this.currentView.size();
  }

  toXmlString(): string {
    return this.sink.document().toXmlString();
  }

  /** `DocView.textBetween` of the current tree. */
  textBetween(start: number, end: number): string {
    return this.currentView.textBetween(start, end);
  }

  /** All text of the document, or below `node`. */
  textContent(node: NodeId = this.currentView.root()): string {
    return this.currentView.textContent(node);
  }

  /**
   * Converts a non-invertible operation against the current document,
   * applies it and sends it on (`hackConsume`). The operation sent is
   * normalized.
   */
  consume(nindo: Nindo): void {
    let op: DocOp;
    try {
      op = normalize(nindo.toDocOp(this.sink.document()));
    } catch (e) {
      throw DocError.lift(e);
    }
    try {
      this.sink.consume(op);
    } catch (e) {
      throw DocError.lift(e);
    }
    this.currentView = new DocView(this.sink.document());
  }

  private static at(location: number): NindoBuilder {
    return Nindo.builder().skip(location);
  }

  private checkLocation(location: number): void {
    // Rust's unsigned locations can be neither negative nor fractional.
    if (!Number.isInteger(location) || location < 0 || location > this.size()) {
      throw DocError.invalidLocation(location, this.size());
    }
  }

  private checkRange(start: number, end: number): void {
    this.checkLocation(end);
    if (start > end) {
      throw DocError.invalidArgument(`range start ${start} is after its end ${end}`);
    }
    this.checkLocation(start);
  }

  /** A non-root element of the current view. */
  private checkElement(element: NodeId): void {
    if (!this.currentView.isElement(element) || element === this.currentView.root()) {
      throw DocError.invalidNode();
    }
  }

  /** The location of a point (`getLocation`). */
  pointLocation(point: Point): number {
    const location = this.currentView.pointLocation(point);
    if (location === null) {
      throw DocError.invalidNode();
    }
    return location;
  }

  insertText(location: number, text: string): void {
    this.checkLocation(location);
    this.consume(MutableDocument.at(location).characters(text).build());
  }

  /**
   * Inserts a fragment and returns the element then starting at the
   * insertion location, if any (`insertXml`).
   */
  insertXml(location: number, xml: XmlFragment): NodeId | null {
    this.checkLocation(location);
    const b = MutableDocument.at(location);
    for (const item of xml.items) {
      switch (item.type) {
        case "text":
          b.characters(item.text);
          break;
        case "start":
          b.elementStart(item.tag, item.attributes);
          break;
        case "end":
          b.elementEnd();
          break;
      }
    }
    this.consume(b.build());
    return this.currentView.elementStartingAt(location);
  }

  /** Inserts a fragment at the end of the document (`appendXml`). */
  appendXml(xml: XmlFragment): NodeId | null {
    return this.insertXml(this.size(), xml);
  }

  /**
   * Inserts an empty element and returns it (`createElement`). Of two
   * attributes with the same name the later wins.
   */
  createElement(
    location: number,
    tag: string,
    attributes: Iterable<readonly [name: string, value: string]> = [],
  ): NodeId {
    this.checkLocation(location);
    const b = MutableDocument.at(location);
    b.elementStart(tag, attributesFromPairs(attributes));
    b.elementEnd();
    this.consume(b.build());
    const element = this.currentView.elementStartingAt(location);
    if (element === null) {
      throw DocError.invalidNode();
    }
    return element;
  }

  /**
   * Inserts an empty element at the end of `parent`
   * (`createChildElement`).
   */
  createChildElement(
    parent: NodeId,
    tag: string,
    attributes: Iterable<readonly [name: string, value: string]> = [],
  ): NodeId {
    if (!this.currentView.isElement(parent)) {
      throw DocError.invalidNode();
    }
    return this.createElement(this.currentView.contentEnd(parent), tag, attributes);
  }

  private deleteElementInto(element: NodeId, b: NindoBuilder): void {
    b.deleteElementStart();
    this.emptyElementInto(element, b);
    b.deleteElementEnd();
  }

  private emptyElementInto(element: NodeId, b: NindoBuilder): void {
    for (const child of this.currentView.children(element)) {
      if (this.currentView.isElement(child)) {
        this.deleteElementInto(child, b);
      } else {
        b.deleteCharacters(this.currentView.itemSize(child));
      }
    }
  }

  /** Deletes an element with its content (`deleteNode`). */
  deleteNode(element: NodeId): void {
    this.checkElement(element);
    const b = MutableDocument.at(this.currentView.location(element));
    this.deleteElementInto(element, b);
    this.consume(b.build());
  }

  /**
   * Deletes the content of an element, which may be the root
   * (`emptyElement`).
   */
  emptyElement(element: NodeId): void {
    if (!this.currentView.isElement(element)) {
      throw DocError.invalidNode();
    }
    const b = MutableDocument.at(this.currentView.contentStart(element));
    this.emptyElementInto(element, b);
    this.consume(b.build());
  }

  /** Deletes what lies between two locations of the same element. */
  private deleteRangeInternal(start: number, end: number): Nindo {
    const view = this.currentView;
    const b = MutableDocument.at(start);
    const startPoint = view.locate(start);
    const endPoint = view.locate(end);
    if (startPoint === null || endPoint === null) {
      throw DocError.invalidLocation(end, this.size());
    }
    let node: NodeId | null;
    if (startPoint.type === "inText") {
      if (endPoint.type === "inText" && endPoint.node === startPoint.node) {
        b.deleteCharacters(Math.max(endPoint.offset - startPoint.offset, 0));
        return b.build();
      }
      b.deleteCharacters(view.itemSize(startPoint.node) - startPoint.offset);
      node = view.nextSibling(startPoint.node);
    } else {
      node = startPoint.nodeAfter;
    }
    const stop = endPoint.type === "inText" ? endPoint.node : endPoint.nodeAfter;
    while (node !== stop && node !== null) {
      if (view.isElement(node)) {
        this.deleteElementInto(node, b);
      } else {
        b.deleteCharacters(view.itemSize(node));
      }
      node = view.nextSibling(node);
    }
    if (endPoint.type === "inText") {
      b.deleteCharacters(endPoint.offset);
    }
    return b.build();
  }

  /**
   * Deletes the content between two locations (`deleteRange`). If they are
   * in different elements, only what lies between them is deleted, in
   * several operations from right to left; the elements they are in are
   * kept.
   */
  deleteRange(start: number, end: number): void {
    this.checkRange(start, end);
    if (start === end) {
      return;
    }
    const view = this.currentView;
    const startEl = view.elementAt(start);
    const endEl = view.elementAt(end);
    if (startEl === null || endEl === null) {
      throw DocError.invalidNode();
    }
    if (startEl === endEl) {
      this.consume(this.deleteRangeInternal(start, end));
      return;
    }
    // All sections are computed up front: each one lies to the left of
    // those deleted before it, so its locations do not move.
    const startAncestors: NodeId[] = [startEl];
    for (let parent = view.parent(startEl); parent !== null; parent = view.parent(parent)) {
      startAncestors.push(parent);
    }
    const sections: [number, number][] = [];
    let rightEl = endEl;
    let prevRightEl = endEl;
    let commonIndex: number;
    for (;;) {
      const ancestorIndex = startAncestors.indexOf(rightEl);
      let sectionStart: number;
      if (ancestorIndex === 0) {
        sectionStart = start;
      } else if (ancestorIndex > 0) {
        sectionStart = view.locationAfter(startAncestors[ancestorIndex - 1]!);
      } else {
        sectionStart = view.contentStart(rightEl);
      }
      const sectionEnd = rightEl === endEl ? end : view.location(prevRightEl);
      sections.push([sectionStart, sectionEnd]);
      prevRightEl = rightEl;
      if (ancestorIndex >= 0) {
        commonIndex = ancestorIndex;
        break;
      }
      const parent = view.parent(rightEl);
      if (parent === null) {
        // Unreachable: the root is an ancestor of every element.
        throw DocError.invalidNode();
      }
      rightEl = parent;
    }
    // The tails of the start element's ancestors below the common one,
    // outermost first, then the tail of the start element itself.
    for (let i = commonIndex - 1; i >= 1; i--) {
      sections.push([view.locationAfter(startAncestors[i - 1]!), view.contentEnd(startAncestors[i]!)]);
    }
    if (commonIndex >= 1) {
      sections.push([start, view.contentEnd(startEl)]);
    }
    for (const [sectionStart, sectionEnd] of sections) {
      this.consume(this.deleteRangeInternal(sectionStart, sectionEnd));
    }
  }

  /**
   * Sets or removes (`null`) one attribute; does nothing if it already has
   * the value (`setElementAttribute`).
   */
  setElementAttribute(element: NodeId, name: string, value: string | null): void {
    this.checkElement(element);
    if (this.currentView.attribute(element, name) === value) {
      return;
    }
    this.updateElementAttributes(element, [[name, value]]);
  }

  /** Replaces all attributes (`setElementAttributes`). */
  setElementAttributes(element: NodeId, attributes: Attributes): void {
    this.checkElement(element);
    const b = MutableDocument.at(this.currentView.location(element));
    b.replaceAttributes(attributes);
    this.consume(b.build());
  }

  /**
   * Sets or removes (`null`) several attributes in one operation, including
   * any that already have the value (`updateElementAttributes`).
   */
  updateElementAttributes(element: NodeId, updates: Iterable<NindoAttributeUpdate>): void {
    this.checkElement(element);
    const b = MutableDocument.at(this.currentView.location(element));
    b.updateAttributes(updates);
    this.consume(b.build());
  }

  private static checkPersistentKey(key: string): void {
    try {
      validateAnnotationKey(key);
    } catch (e) {
      throw DocError.invalidArgument(e instanceof Error ? e.message : String(e));
    }
  }

  /**
   * Sets annotation `key` over `start..end`; `null` removes it
   * (`setAnnotation`).
   */
  setAnnotation(start: number, end: number, key: string, value: string | null): void {
    MutableDocument.checkPersistentKey(key);
    this.checkRange(start, end);
    if (start === end) {
      return;
    }
    this.consume(Nindo.setAnnotation(start, end, key, value));
  }

  /**
   * Sets annotation `key` over `start..end` and removes it everywhere else
   * (`resetAnnotation`).
   */
  resetAnnotation(start: number, end: number, key: string, value: string | null): void {
    MutableDocument.checkPersistentKey(key);
    this.checkRange(start, end);
    const size = this.size();
    const b = Nindo.builder();
    if (start > 0) {
      b.startAnnotation(key, null);
      b.skip(start);
    }
    if (start !== end) {
      b.startAnnotation(key, value);
      b.skip(end - start);
    }
    if (size !== end) {
      b.startAnnotation(key, null);
      b.skip(size - end);
    }
    if (size === 0) {
      // The original would end an annotation it never started.
      return;
    }
    b.endAnnotation(key);
    this.consume(b.build());
  }

  /**
   * `Annotations.guardedResetAnnotation`: {@link resetAnnotation} unless
   * the document already is in that state. Returns whether an operation was
   * emitted.
   */
  guardedResetAnnotation(start: number, end: number, key: string, value: string | null): boolean {
    this.checkRange(start, end);
    const doc = this.sink.document();
    const isSet =
      doc.firstAnnotationChange(0, start, key, null) === null &&
      doc.firstAnnotationChange(start, end, key, value) === null &&
      doc.firstAnnotationChange(end, this.size(), key, null) === null;
    if (isSet) {
      return false;
    }
    this.resetAnnotation(start, end, key, value);
    return true;
  }
}
