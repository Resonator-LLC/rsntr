// Operation validation: a port of `DocOpAutomaton` and `DocOpValidator`.
//
// The automaton accepts exactly the operations that are well-formed, apply
// to a given document and preserve a given schema. Each `check*` method
// classifies a proposed next component; the matching `do*` method performs
// the transition. Ill-formed transitions must not be performed.
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import type { AnnotationBoundaryMap } from "./annotation";
import { AnnotationMap, AnnotationsUpdate, type Attributes, type AttributesUpdate } from "./attributes";
import type { DocInitialization, DocOp, DocOpComponent } from "./op";
import { type DocumentSchema, NO_SCHEMA_CONSTRAINTS } from "./schema";
import {
  isGoodForBlip,
  isHighSurrogate,
  isLowSurrogate,
  isValidText,
  isXmlName,
  saturatingAdd,
} from "./utf16";

/** The overall result of validating an operation, most severe first. */
export const ValidationResult = {
  /**
   * The operation is meaningless: illegal field values or illegal sequence
   * of components.
   */
  IllFormed: 0,
  /** The operation is well-formed but does not match the document. */
  InvalidDocument: 1,
  /** The operation applies but the result would violate the schema. */
  InvalidSchema: 2,
  /** The operation is valid for the document and schema. */
  Valid: 3,
} as const;
export type ValidationResult = (typeof ValidationResult)[keyof typeof ValidationResult];

const { IllFormed, InvalidDocument, InvalidSchema, Valid } = ValidationResult;

/** The more severe of the two results. */
export function mergeValidationResults(a: ValidationResult, b: ValidationResult): ValidationResult {
  return a < b ? a : b;
}

/** One reason why an operation is not valid. */
export class Violation {
  constructor(
    readonly validationResult: ValidationResult,
    private readonly text: string,
    readonly originalDocumentPos: number,
    readonly resultingDocumentPos: number,
  ) {}

  /** A developer-readable description of the violation. */
  description(): string {
    return (
      `${this.text} at original document position ${this.originalDocumentPos}` +
      ` / resulting document position ${this.resultingDocumentPos}`
    );
  }
}

/** Collects the violations found by the automaton's `check*` methods. */
export class ViolationCollector {
  private readonly operationIllFormed: Violation[] = [];
  private readonly operationInvalid: Violation[] = [];
  private readonly schemaViolations: Violation[] = [];

  add(violation: Violation): void {
    switch (violation.validationResult) {
      case IllFormed:
        this.operationIllFormed.push(violation);
        break;
      case InvalidDocument:
        this.operationInvalid.push(violation);
        break;
      case InvalidSchema:
        this.schemaViolations.push(violation);
        break;
      case Valid:
        break;
    }
  }

  isIllFormed(): boolean {
    return this.validationResult() === IllFormed;
  }

  isInvalidDocument(): boolean {
    return this.validationResult() === InvalidDocument;
  }

  isInvalidSchema(): boolean {
    return this.validationResult() === InvalidSchema;
  }

  isValid(): boolean {
    return this.validationResult() === Valid;
  }

  /** The most severe result among the collected violations. */
  validationResult(): ValidationResult {
    if (this.operationIllFormed.length > 0) {
      return IllFormed;
    } else if (this.operationInvalid.length > 0) {
      return InvalidDocument;
    } else if (this.schemaViolations.length > 0) {
      return InvalidSchema;
    }
    return Valid;
  }

  /** All collected violations, most severe kind first. */
  violations(): Violation[] {
    return [...this.operationIllFormed, ...this.operationInvalid, ...this.schemaViolations];
  }

  /** Description of the first (most severe) violation, if any. */
  firstDescription(): string | null {
    const v = this.operationIllFormed[0] ?? this.operationInvalid[0] ?? this.schemaViolations[0];
    if (v === undefined) {
      return null;
    }
    const prefix =
      v.validationResult === IllFormed
        ? "ill-formed"
        : v.validationResult === InvalidDocument
          ? "invalid operation"
          : "schema violation";
    return `${prefix}: ${v.description()}`;
  }

  private size(): number {
    return (
      this.operationIllFormed.length + this.operationInvalid.length + this.schemaViolations.length
    );
  }

  toString(): string {
    const first = this.firstDescription();
    return first === null
      ? "ViolationCollector[0]"
      : `ViolationCollector[${this.size()}: ${first}]`;
  }
}

/**
 * The document queries the automaton needs. Positions are item indices; a
 * character item is one UTF-16 code unit. All queries must tolerate
 * positions past the end of the document.
 */
export interface AutomatonDocument {
  /** The tag of the element starting at `pos`, if any. */
  elementStartingAt(pos: number): string | null;

  /** The attributes of the element starting at `pos`, if any. */
  attributesAt(pos: number): Attributes | null;

  /** The tag of the element ending at `pos`, if any. */
  elementEndingAt(pos: number): string | null;

  /** The UTF-16 code unit at `pos`, or `null` at an element start or end. */
  charAt(pos: number): number | null;

  /**
   * Tag of the `depth`-th element enclosing the insertion point; depth 0 is
   * the innermost. `null` if nesting is not that deep.
   */
  nthEnclosingElementTag(insertionPoint: number, depth: number): string | null;

  /**
   * Number of characters after the insertion point before the next element
   * start, element end or document end.
   */
  remainingCharactersInElement(insertionPoint: number): number;

  annotationsAt(pos: number): AnnotationMap;

  getAnnotation(pos: number, key: string): string | null;

  /** Number of items. */
  length(): number;

  /**
   * First position in `start..end` whose value for `key` differs from
   * `fromValue`, or `null`.
   */
  firstAnnotationChange(
    start: number,
    end: number,
    key: string,
    fromValue: string | null,
  ): number | null;
}

/** The empty document. */
export class EmptyDocument implements AutomatonDocument {
  elementStartingAt(_pos: number): string | null {
    return null;
  }

  attributesAt(_pos: number): Attributes | null {
    return null;
  }

  elementEndingAt(_pos: number): string | null {
    return null;
  }

  charAt(_pos: number): number | null {
    return null;
  }

  nthEnclosingElementTag(_insertionPoint: number, _depth: number): string | null {
    return null;
  }

  remainingCharactersInElement(_insertionPoint: number): number {
    return 0;
  }

  annotationsAt(_pos: number): AnnotationMap {
    return AnnotationMap.EMPTY;
  }

  getAnnotation(_pos: number, _key: string): string | null {
    return null;
  }

  length(): number {
    return 0;
  }

  firstAnnotationChange(
    _start: number,
    _end: number,
    _key: string,
    _fromValue: string | null,
  ): number | null {
    return null;
  }
}

/** The empty document. */
export const EMPTY_DOCUMENT: AutomatonDocument = new EmptyDocument();

type DocSymbol = "character" | "open" | "close" | "end";

/** Where violations are recorded; `null` to only classify. */
export type Collector = ViolationCollector | null;

const nullOr = (s: string | null): string => s ?? "null";

/** A state machine over operation components for one document and schema. */
export class DocOpAutomaton {
  private readonly doc: AutomatonDocument;
  private readonly constraints: DocumentSchema;
  /** Position in the original document. */
  private effectivePos = 0;
  /** Tags of the inserted elements that are still open; last is innermost. */
  private insertionStack: string[] = [];
  private nextRequiredElement: string | null = null;
  private deletionStackDepth = 0;
  private annotationsUpdate: AnnotationsUpdate = AnnotationsUpdate.EMPTY;
  private afterAnnotationBoundary = false;
  /** `null` once the operation has run past the end of the document. */
  private targetAnnotationsForDeletion: AnnotationMap | null = AnnotationMap.EMPTY;
  /** Position in the resulting document, for diagnostics only. */
  private resultingPos = 0;

  /**
   * Creates an automaton accepting the operations valid on `doc` under
   * `constraints`.
   */
  constructor(doc: AutomatonDocument, constraints: DocumentSchema) {
    this.doc = doc;
    this.constraints = constraints;
    this.nextRequiredElement = this.requiredFirstChild(null);
  }

  /** An independent copy of the automaton in its current state. */
  clone(): DocOpAutomaton {
    const copy = new DocOpAutomaton(this.doc, this.constraints);
    copy.effectivePos = this.effectivePos;
    copy.insertionStack = [...this.insertionStack];
    copy.nextRequiredElement = this.nextRequiredElement;
    copy.deletionStackDepth = this.deletionStackDepth;
    copy.annotationsUpdate = this.annotationsUpdate;
    copy.afterAnnotationBoundary = this.afterAnnotationBoundary;
    copy.targetAnnotationsForDeletion = this.targetAnnotationsForDeletion;
    copy.resultingPos = this.resultingPos;
    return copy;
  }

  // Violation constructors.

  private violation(v: Collector, kind: ValidationResult, description: () => string): ValidationResult {
    if (v !== null) {
      v.add(new Violation(kind, description(), this.effectivePos, this.resultingPos));
    }
    return kind;
  }

  private illFormed(v: Collector, description: () => string): ValidationResult {
    return this.violation(v, IllFormed, description);
  }

  private invalid(v: Collector, description: () => string): ValidationResult {
    return this.violation(v, InvalidDocument, description);
  }

  private schemaViolation(v: Collector, description: () => string): ValidationResult {
    return this.violation(v, InvalidSchema, description);
  }

  private oldAnnotationsDifferFromDocument(
    v: Collector,
    key: string,
    oldValue: string | null,
    valueInDoc: string | null,
  ): ValidationResult {
    return this.invalid(
      v,
      () =>
        `old annotations differ from document: purported old value for key ${key} is ` +
        `${nullOr(oldValue)}, actual value in document is ${nullOr(valueInDoc)}`,
    );
  }

  private missingAnnotationForDeletion(
    v: Collector,
    key: string,
    valueInDoc: string | null,
    requiredValue: string | null,
  ): ValidationResult {
    return this.invalid(
      v,
      () =>
        `deletion does not reset value for key ${key} from ${nullOr(valueInDoc)} to ` +
        `${nullOr(requiredValue)}`,
    );
  }

  private attributeChangeInsideInsertOrDelete(v: Collector): ValidationResult {
    return this.illFormed(v, () => "attribute change inside insert or delete");
  }

  private invalidAttribute(v: Collector, element: string, name: string, value: string): ValidationResult {
    return this.schemaViolation(
      v,
      () => `type ${element} does not permit attribute ${name} with value ${value}`,
    );
  }

  // Schema accessors.

  /** The type of the first required child of `parent`, if any. */
  private requiredFirstChild(parent: string | null): string | null {
    return this.constraints.requiredInitialChildren(parent)[0] ?? null;
  }

  // Current state readers.

  private effectiveDocSymbol(): DocSymbol {
    if (this.effectivePos >= this.doc.length()) {
      return "end";
    } else if (this.doc.elementStartingAt(this.effectivePos) !== null) {
      return "open";
    } else if (this.doc.elementEndingAt(this.effectivePos) !== null) {
      return "close";
    }
    return "character";
  }

  private insertionStackIsEmpty(): boolean {
    return this.insertionStack.length === 0;
  }

  private deletionStackIsEmpty(): boolean {
    return this.deletionStackDepth === 0;
  }

  /** `null` at top level. */
  private effectiveEnclosingElementTag(): string | null {
    const top = this.insertionStack[this.insertionStack.length - 1];
    if (top !== undefined) {
      return top;
    }
    if (this.effectivePos > this.doc.length()) {
      return null;
    }
    return this.doc.nthEnclosingElementTag(this.effectivePos, 0);
  }

  /**
   * Deviation from the original, which forbids surrogates altogether:
   * characters outside the BMP are allowed, so validation must instead
   * reject operations that would cut a surrogate pair of the document.
   */
  private splitsSurrogatePair(pos: number): boolean {
    if (!(pos > 0 && pos < this.doc.length())) {
      return false;
    }
    const before = this.doc.charAt(pos - 1);
    const after = this.doc.charAt(pos);
    return before !== null && isHighSurrogate(before) && after !== null && isLowSurrogate(after);
  }

  /** The maximum item count of a valid retain at this point. */
  maxRetainItemCount(): number {
    return Math.max(0, this.doc.length() - this.effectivePos);
  }

  currentElementStartTag(): string | null {
    return this.doc.elementStartingAt(this.effectivePos);
  }

  currentElementStartAttributes(): Attributes | null {
    return this.doc.attributesAt(this.effectivePos);
  }

  currentAnnotations(): AnnotationMap {
    if (this.effectivePos >= this.doc.length()) {
      return AnnotationMap.EMPTY;
    }
    return this.doc.annotationsAt(this.effectivePos);
  }

  /** The code unit `offset` items ahead, if that item is a character. */
  nextChar(offset: number): number | null {
    if (offset >= Math.max(0, this.doc.length() - this.effectivePos)) {
      return null;
    }
    return this.doc.charAt(this.effectivePos + offset);
  }

  /** Number of inserted elements that are still open. */
  insertionStackComplexityMeasure(): number {
    return this.insertionStack.length;
  }

  /** Number of deleted elements that are still open. */
  deletionStackComplexityMeasure(): number {
    return this.deletionStackDepth;
  }

  /** Keys of the annotation changes currently open, in sorted order. */
  openAnnotations(): string[] {
    return this.annotationsUpdate.updates.map((u) => u.name);
  }

  /**
   * The maximum number of characters a `deleteCharacters` component can
   * delete here, assuming one is permitted at all.
   */
  maxCharactersToDelete(): number {
    if (this.effectivePos >= this.doc.length()) {
      return 0;
    }
    return this.doc.remainingCharactersInElement(this.effectivePos);
  }

  /** The annotations an insertion at the current position inherits. */
  inheritedAnnotations(): AnnotationMap {
    if (this.effectivePos === 0 || this.effectivePos > this.doc.length()) {
      return AnnotationMap.EMPTY;
    }
    return this.doc.annotationsAt(this.effectivePos - 1);
  }

  private updateDeletionTargetAnnotations(): void {
    this.targetAnnotationsForDeletion =
      this.effectivePos > this.doc.length()
        ? null
        : this.inheritedAnnotations().updateWithNoCompatibilityCheck(this.annotationsUpdate);
  }

  // Check and transition methods.

  private checkAnnotationsForRetain(v: Collector, itemCount: number): ValidationResult {
    for (const update of this.annotationsUpdate.updates) {
      const firstChange = this.doc.firstAnnotationChange(
        this.effectivePos,
        this.effectivePos + itemCount,
        update.name,
        update.oldValue,
      );
      if (firstChange !== null) {
        return this.oldAnnotationsDifferFromDocument(
          v,
          update.name,
          update.oldValue,
          this.doc.getAnnotation(firstChange, update.name),
        );
      }
    }
    return Valid;
  }

  checkRetain(itemCount: number, v: Collector): ValidationResult {
    // well-formedness
    // Also rejects counts that are not integers, which Rust's type excludes.
    if (!Number.isSafeInteger(itemCount) || itemCount <= 0) {
      return this.illFormed(v, () => "retain item count not positive");
    }
    if (!this.insertionStackIsEmpty() || !this.deletionStackIsEmpty()) {
      return this.illFormed(v, () => "retain inside insert or delete");
    }
    // validity
    if (itemCount > this.maxRetainItemCount()) {
      return this.invalid(
        v,
        () =>
          `retain past end of document, document length ${this.doc.length()}, ` +
          `retain item count ${itemCount}`,
      );
    }
    if (this.splitsSurrogatePair(this.effectivePos + itemCount)) {
      return this.invalid(v, () => "retain ends inside a surrogate pair");
    }
    return this.checkAnnotationsForRetain(v, itemCount);
  }

  doRetain(itemCount: number): void {
    this.effectivePos = saturatingAdd(this.effectivePos, itemCount);
    this.updateDeletionTargetAnnotations();
    this.resultingPos = saturatingAdd(this.resultingPos, itemCount);
    this.afterAnnotationBoundary = false;
  }

  private validateAnnotationKey(key: string, v: Collector): ValidationResult {
    if (key.includes("?") || key.includes("@")) {
      return this.illFormed(v, () => `invalid character in annotation key: ${key}`);
    }
    if (!isValidText(key)) {
      return this.illFormed(v, () => "annotation key is not valid UTF-16");
    }
    return Valid;
  }

  private validateAnnotationValue(value: string | null, v: Collector): ValidationResult {
    if (value !== null && !isValidText(value)) {
      return this.illFormed(v, () => "annotation value is not valid UTF-16");
    }
    return Valid;
  }

  checkAnnotationBoundary(map: AnnotationBoundaryMap, v: Collector): ValidationResult {
    // well-formedness
    if (this.afterAnnotationBoundary) {
      return this.illFormed(v, () => "adjacent annotation boundaries");
    }
    const notMonotonic = (key1: string, key2: string): ValidationResult =>
      this.illFormed(v, () => `annotation keys not strictly monotonic: ${key1} >= ${key2}`);
    const endKeys = new Set<string>();
    const ends = map.endKeys;
    for (let i = 0; i < ends.length; i++) {
      const key = ends[i]!;
      const r = this.validateAnnotationKey(key, v);
      if (r !== Valid) {
        return r;
      }
      if (i > 0 && !(ends[i - 1]! < key)) {
        return notMonotonic(ends[i - 1]!, key);
      }
      if (!this.annotationsUpdate.containsKey(key)) {
        return this.illFormed(v, () => `annotation of key ${key} ends without having started`);
      }
      endKeys.add(key);
    }
    const changes = map.changes;
    for (let i = 0; i < changes.length; i++) {
      const change = changes[i]!;
      let r = this.validateAnnotationKey(change.key, v);
      if (r !== Valid) {
        return r;
      }
      r = this.validateAnnotationValue(change.oldValue, v);
      if (r !== Valid) {
        return r;
      }
      r = this.validateAnnotationValue(change.newValue, v);
      if (r !== Valid) {
        return r;
      }
      if (i > 0 && !(changes[i - 1]!.key < change.key)) {
        return notMonotonic(changes[i - 1]!.key, change.key);
      }
      if (endKeys.has(change.key)) {
        return this.illFormed(v, () => `annotation boundary contains duplicate key ${change.key}`);
      }
    }
    return Valid;
  }

  doAnnotationBoundary(map: AnnotationBoundaryMap): void {
    // Cannot fail for a boundary that is not ill-formed.
    try {
      this.annotationsUpdate = this.annotationsUpdate.composeWithBoundary(map);
    } catch {
      // Keep the current update, as the Rust port does.
    }
    this.afterAnnotationBoundary = true;
  }

  private checkAnnotationsForInsertion(v: Collector): ValidationResult {
    if (this.effectivePos > this.doc.length()) {
      // Invalid operation, nothing to check.
      return Valid;
    }
    // Part of the surrogate deviation, see splitsSurrogatePair.
    if (this.splitsSurrogatePair(this.effectivePos)) {
      return this.invalid(v, () => "insertion inside a surrogate pair");
    }
    for (const update of this.annotationsUpdate.updates) {
      const defaultFromDocument =
        this.effectivePos === 0 ? null : this.doc.getAnnotation(this.effectivePos - 1, update.name);
      if (update.oldValue !== defaultFromDocument) {
        return this.oldAnnotationsDifferFromDocument(
          v,
          update.name,
          update.oldValue,
          defaultFromDocument,
        );
      }
    }
    return Valid;
  }

  private checkForInsertionBeforeRequiredChild(v: Collector): ValidationResult {
    if (this.effectivePos < this.doc.length() && this.insertionStackIsEmpty()) {
      const parentType = this.doc.nthEnclosingElementTag(this.effectivePos, 0);
      const requiredFirstChild = this.requiredFirstChild(parentType);
      const isFirstChild =
        this.effectivePos === 0 || this.doc.elementStartingAt(this.effectivePos - 1) !== null;
      if (isFirstChild && requiredFirstChild !== null) {
        return this.schemaViolation(v, () => "attempt to insert before required child");
      }
    }
    return Valid;
  }

  checkCharacters(chars: string, v: Collector): ValidationResult {
    // well-formedness
    if (chars.length === 0) {
      return this.illFormed(v, () => "characters is empty");
    }
    // Deviation from the original: no "characters component contains
    // surrogate" rule, so text outside the BMP is accepted.
    if (!isValidText(chars)) {
      return this.illFormed(v, () => "characters component contains invalid unicode");
    }
    if (!this.deletionStackIsEmpty()) {
      return this.illFormed(v, () => "insertion inside deletion");
    }
    // validity
    let r = this.checkAnnotationsForInsertion(v);
    if (r !== Valid) {
      return r;
    }
    // schema
    const required = this.nextRequiredElement;
    if (required !== null) {
      return this.schemaViolation(v, () => `child element required, expected type ${required}`);
    }
    r = this.checkForInsertionBeforeRequiredChild(v);
    if (r !== Valid) {
      return r;
    }
    const enclosingTag = this.effectiveEnclosingElementTag();
    switch (this.constraints.permittedCharacters(enclosingTag)) {
      case "none":
        return this.schemaViolation(
          v,
          () => `element type ${nullOr(enclosingTag)} does not allow text content`,
        );
      case "blipText":
        if (!isGoodForBlip(chars)) {
          return this.schemaViolation(
            v,
            () =>
              `element type ${nullOr(enclosingTag)} only allows blip text content, ` +
              "not arbitrary characters",
          );
        }
        return Valid;
      default:
        return Valid;
    }
  }

  doCharacters(chars: string): void {
    this.updateDeletionTargetAnnotations();
    this.resultingPos = saturatingAdd(this.resultingPos, chars.length);
    this.afterAnnotationBoundary = false;
  }

  private checkAnnotationsForDeletion(v: Collector, itemCount: number): ValidationResult {
    const target = this.targetAnnotationsForDeletion;
    if (target === null) {
      // Invalid operation, nothing to check.
      return Valid;
    }
    // All annotations in the update must have correct old and new values.
    for (const update of this.annotationsUpdate.updates) {
      const key = update.name;
      const firstChange = this.doc.firstAnnotationChange(
        this.effectivePos,
        this.effectivePos + itemCount,
        key,
        update.oldValue,
      );
      if (firstChange !== null) {
        return this.oldAnnotationsDifferFromDocument(
          v,
          key,
          update.oldValue,
          this.doc.getAnnotation(firstChange, key),
        );
      }
      if (update.newValue !== target.get(key)) {
        return this.invalid(v, () => "new annotation value incorrect for deletion");
      }
    }
    // The update must contain all values that need to be reset: the keys to
    // check are those at each deleted position and those at the position the
    // deletion inherits from.
    for (let offset = 0; offset < itemCount; offset++) {
      const annotationsHere = this.doc.annotationsAt(this.effectivePos + offset);
      for (const entries of [annotationsHere.entries, target.entries]) {
        for (const { name: key } of entries) {
          const valueInDoc = annotationsHere.get(key);
          const requiredValue = target.get(key);
          if (valueInDoc !== requiredValue && !this.annotationsUpdate.containsKey(key)) {
            return this.missingAnnotationForDeletion(v, key, valueInDoc, requiredValue);
          }
        }
      }
    }
    return Valid;
  }

  private checkAttributesWellFormed(attributes: Attributes, v: Collector): ValidationResult {
    let previousKey: string | null = null;
    for (const { name: key, value } of attributes.entries) {
      if (!isXmlName(key)) {
        return this.illFormed(v, () => `attribute name is not an XML Name: "${key}"`);
      }
      if (!isValidText(value)) {
        return this.illFormed(v, () => "attribute value is not valid UTF-16");
      }
      if (previousKey !== null && !(previousKey < key)) {
        const previous = previousKey;
        return this.illFormed(
          v,
          () => `attribute keys not strictly monotonic: ${previous} >= ${key}`,
        );
      }
      previousKey = key;
    }
    return Valid;
  }

  private checkAttributesUpdateWellFormed(update: AttributesUpdate, v: Collector): ValidationResult {
    let previousKey: string | null = null;
    for (const u of update.updates) {
      const key = u.name;
      if (!isXmlName(key)) {
        return this.illFormed(v, () => `attribute name is not an XML Name: "${key}"`);
      }
      if (previousKey !== null && !(previousKey < key)) {
        const previous = previousKey;
        return this.illFormed(
          v,
          () => `attribute keys not strictly monotonic: ${previous} >= ${key}`,
        );
      }
      const valid = (value: string | null): boolean => value === null || isValidText(value);
      if (!valid(u.oldValue) || !valid(u.newValue)) {
        return this.illFormed(v, () => "attribute value is not valid UTF-16");
      }
      previousKey = key;
    }
    return Valid;
  }

  private validateAttributes(tag: string, attributes: Attributes, v: Collector): ValidationResult {
    for (const { name: key, value } of attributes.entries) {
      if (!this.constraints.permitsAttributeValue(tag, key, value)) {
        return this.invalidAttribute(v, tag, key, value);
      }
    }
    return Valid;
  }

  checkElementStart(tag: string, attributes: Attributes, v: Collector): ValidationResult {
    // well-formedness
    if (!isXmlName(tag)) {
      return this.illFormed(v, () => `element type is not an XML Name: "${tag}"`);
    }
    let r = this.checkAttributesWellFormed(attributes, v);
    if (r !== Valid) {
      return r;
    }
    if (!this.deletionStackIsEmpty()) {
      return this.illFormed(v, () => "insertion inside deletion");
    }
    // validity
    r = this.checkAnnotationsForInsertion(v);
    if (r !== Valid) {
      return r;
    }
    // schema
    r = this.validateAttributes(tag, attributes, v);
    if (r !== Valid) {
      return r;
    }
    const parentTag = this.effectiveEnclosingElementTag();
    if (!this.constraints.permitsChild(parentTag, tag)) {
      return this.schemaViolation(v, () =>
        parentTag === null
          ? `element type ${tag} not permitted at top level`
          : `element type ${parentTag} does not permit subelement type ${tag}`,
      );
    }
    r = this.checkForInsertionBeforeRequiredChild(v);
    if (r !== Valid) {
      return r;
    }
    const required = this.nextRequiredElement;
    if (required !== null && required !== tag) {
      return this.schemaViolation(v, () => `element of type ${required} required, not ${tag}`);
    }
    return Valid;
  }

  doElementStart(tag: string, _attributes: Attributes): void {
    this.updateDeletionTargetAnnotations();
    this.insertionStack.push(tag);
    this.nextRequiredElement = this.requiredFirstChild(tag);
    this.resultingPos = saturatingAdd(this.resultingPos, 1);
    this.afterAnnotationBoundary = false;
  }

  checkElementEnd(v: Collector): ValidationResult {
    // well-formedness
    if (!this.deletionStackIsEmpty()) {
      return this.illFormed(v, () => "insertion inside deletion");
    }
    if (this.insertionStackIsEmpty()) {
      return this.illFormed(v, () => "elementEnd with no matching elementStart");
    }
    // validity
    const r = this.checkAnnotationsForInsertion(v);
    if (r !== Valid) {
      return r;
    }
    // schema
    const required = this.nextRequiredElement;
    if (required !== null) {
      return this.schemaViolation(v, () => `child element required, expected type ${required}`);
    }
    return Valid;
  }

  doElementEnd(): void {
    this.updateDeletionTargetAnnotations();
    this.insertionStack.pop();
    this.resultingPos = saturatingAdd(this.resultingPos, 1);
    this.afterAnnotationBoundary = false;
  }

  private static attributesEqual(a: Attributes, b: Attributes): boolean {
    return a.size === b.size && a.entries.every(({ name, value }) => b.get(name) === value);
  }

  checkDeleteCharacters(chars: string, v: Collector): ValidationResult {
    // well-formedness
    if (chars.length === 0) {
      return this.illFormed(v, () => "characters is empty");
    }
    // Deviation from the original: surrogates are allowed, see
    // checkCharacters.
    if (!isValidText(chars)) {
      return this.illFormed(v, () => "delete characters component contains invalid unicode");
    }
    if (!this.insertionStackIsEmpty()) {
      return this.illFormed(v, () => "deletion inside insertion");
    }
    // validity
    const docLength = this.doc.length();
    const attempted = chars.length;
    const cannotDeleteSoMany = (available: number): ValidationResult =>
      this.invalid(v, () => `cannot delete ${attempted} characters, only ${available} available`);
    for (let offset = 0; offset < attempted; offset++) {
      const unit = chars.charCodeAt(offset);
      const pos = saturatingAdd(this.effectivePos, offset);
      if (pos >= docLength) {
        return cannotDeleteSoMany(offset);
      }
      const charHere = this.doc.charAt(pos);
      if (charHere === null) {
        return cannotDeleteSoMany(offset);
      }
      if (charHere !== unit) {
        return this.invalid(
          v,
          () =>
            `attempt to delete character ${lossyUnit(unit)} when the actual character is ` +
            lossyUnit(charHere),
        );
      }
    }
    return this.checkAnnotationsForDeletion(v, attempted);
  }

  doDeleteCharacters(chars: string): void {
    this.effectivePos = saturatingAdd(this.effectivePos, chars.length);
    this.afterAnnotationBoundary = false;
  }

  checkDeleteElementStart(tag: string, attributes: Attributes, v: Collector): ValidationResult {
    // well-formedness
    if (!isXmlName(tag)) {
      return this.illFormed(v, () => `element type is not an XML Name: "${tag}"`);
    }
    let r = this.checkAttributesWellFormed(attributes, v);
    if (r !== Valid) {
      return r;
    }
    if (!this.insertionStackIsEmpty()) {
      return this.illFormed(v, () => "deletion inside insertion");
    }
    // validity
    const start = this.effectiveElementStart();
    if (start === null) {
      return this.invalid(v, () => "no element start to delete here");
    }
    if (start.tag !== tag) {
      return this.invalid(v, () => "old element type differs from document");
    }
    if (!DocOpAutomaton.attributesEqual(attributes, start.attributes)) {
      return this.invalid(v, () => "old attributes differ from document");
    }
    r = this.checkAnnotationsForDeletion(v, 1);
    if (r !== Valid) {
      return r;
    }
    // schema
    if (this.deletionStackDepth === 0 && this.effectivePos < this.doc.length()) {
      const parentType = this.doc.nthEnclosingElementTag(this.effectivePos, 0);
      const requiredFirstChild = this.requiredFirstChild(parentType);
      const isFirstChild =
        this.effectivePos === 0 || this.doc.elementStartingAt(this.effectivePos - 1) !== null;
      if (isFirstChild && requiredFirstChild !== null) {
        return this.schemaViolation(v, () => "attempt to delete required child");
      }
    }
    return Valid;
  }

  doDeleteElementStart(_tag: string, _attributes: Attributes): void {
    this.deletionStackDepth += 1;
    this.effectivePos = saturatingAdd(this.effectivePos, 1);
    this.afterAnnotationBoundary = false;
  }

  checkDeleteElementEnd(v: Collector): ValidationResult {
    // well-formedness
    if (!this.insertionStackIsEmpty()) {
      return this.illFormed(v, () => "deletion inside insertion");
    }
    if (this.deletionStackIsEmpty()) {
      return this.illFormed(v, () => "deleteElementEnd with no matching deleteElementStart");
    }
    // validity
    if (this.effectiveDocSymbol() !== "close") {
      return this.invalid(v, () => "no element end to delete here");
    }
    return this.checkAnnotationsForDeletion(v, 1);
  }

  doDeleteElementEnd(): void {
    this.deletionStackDepth = Math.max(0, this.deletionStackDepth - 1);
    this.effectivePos = saturatingAdd(this.effectivePos, 1);
    this.afterAnnotationBoundary = false;
  }

  /**
   * The tag and attributes of the element starting at the current position,
   * if the position is inside the document.
   */
  private effectiveElementStart(): { tag: string; attributes: Attributes } | null {
    if (this.effectiveDocSymbol() !== "open") {
      return null;
    }
    const tag = this.doc.elementStartingAt(this.effectivePos);
    const attributes = this.doc.attributesAt(this.effectivePos);
    return tag === null || attributes === null ? null : { tag, attributes };
  }

  checkUpdateAttributes(update: AttributesUpdate, v: Collector): ValidationResult {
    // well-formedness
    let r = this.checkAttributesUpdateWellFormed(update, v);
    if (r !== Valid) {
      return r;
    }
    if (!this.deletionStackIsEmpty() || !this.insertionStackIsEmpty()) {
      return this.attributeChangeInsideInsertOrDelete(v);
    }
    // validity
    const start = this.effectiveElementStart();
    if (start === null) {
      return this.invalid(v, () => "no element start to change attributes here");
    }
    for (const u of update.updates) {
      if (u.oldValue !== start.attributes.get(u.name)) {
        return this.invalid(v, () => "old attributes differ from document");
      }
    }
    r = this.checkAnnotationsForRetain(v, 1);
    if (r !== Valid) {
      return r;
    }
    // schema
    for (const u of update.updates) {
      if (
        u.newValue !== null &&
        !this.constraints.permitsAttributeValue(start.tag, u.name, u.newValue)
      ) {
        return this.invalidAttribute(v, start.tag, u.name, u.newValue);
      }
    }
    return Valid;
  }

  doUpdateAttributes(_update: AttributesUpdate): void {
    this.effectivePos = saturatingAdd(this.effectivePos, 1);
    this.updateDeletionTargetAnnotations();
    this.resultingPos = saturatingAdd(this.resultingPos, 1);
    this.afterAnnotationBoundary = false;
  }

  checkReplaceAttributes(
    oldAttributes: Attributes,
    newAttributes: Attributes,
    v: Collector,
  ): ValidationResult {
    // well-formedness
    let r = this.checkAttributesWellFormed(oldAttributes, v);
    if (r !== Valid) {
      return r;
    }
    r = this.checkAttributesWellFormed(newAttributes, v);
    if (r !== Valid) {
      return r;
    }
    if (!this.deletionStackIsEmpty() || !this.insertionStackIsEmpty()) {
      return this.attributeChangeInsideInsertOrDelete(v);
    }
    // validity
    const start = this.effectiveElementStart();
    if (start === null) {
      return this.invalid(v, () => "no element start to change attributes here");
    }
    if (!DocOpAutomaton.attributesEqual(start.attributes, oldAttributes)) {
      return this.invalid(v, () => "old attributes differ from document");
    }
    r = this.checkAnnotationsForRetain(v, 1);
    if (r !== Valid) {
      return r;
    }
    // schema
    return this.validateAttributes(start.tag, newAttributes, v);
  }

  doReplaceAttributes(_oldAttributes: Attributes, _newAttributes: Attributes): void {
    this.effectivePos = saturatingAdd(this.effectivePos, 1);
    this.updateDeletionTargetAnnotations();
    this.resultingPos = saturatingAdd(this.resultingPos, 1);
    this.afterAnnotationBoundary = false;
  }

  /** Whether the operation may end here. */
  checkFinish(v: Collector): ValidationResult {
    // well-formedness
    if (!this.insertionStackIsEmpty()) {
      return this.illFormed(v, () => "elementStart with no matching elementEnd");
    }
    if (!this.deletionStackIsEmpty()) {
      return this.illFormed(v, () => "deleteElementStart with no matching deleteElementEnd");
    }
    const open = this.annotationsUpdate.updates[0];
    if (open !== undefined) {
      return this.illFormed(v, () => `annotation of key ${open.name} starts but never ends`);
    }
    // validity
    if (this.effectivePos !== this.doc.length()) {
      return this.invalid(
        v,
        () =>
          `operation shorter than document, document length ${this.doc.length()}, ` +
          `length of input of operation ${this.effectivePos}`,
      );
    }
    return Valid;
  }

  /** Dispatches to the `check*` method for the component. */
  checkComponent(c: DocOpComponent, v: Collector): ValidationResult {
    switch (c.type) {
      case "annotationBoundary":
        return this.checkAnnotationBoundary(c.boundary, v);
      case "characters":
        return this.checkCharacters(c.characters, v);
      case "elementStart":
        return this.checkElementStart(c.tag, c.attributes, v);
      case "elementEnd":
        return this.checkElementEnd(v);
      case "retain":
        return this.checkRetain(c.itemCount, v);
      case "deleteCharacters":
        return this.checkDeleteCharacters(c.characters, v);
      case "deleteElementStart":
        return this.checkDeleteElementStart(c.tag, c.attributes, v);
      case "deleteElementEnd":
        return this.checkDeleteElementEnd(v);
      case "replaceAttributes":
        return this.checkReplaceAttributes(c.oldAttributes, c.newAttributes, v);
      case "updateAttributes":
        return this.checkUpdateAttributes(c.update, v);
    }
  }

  /** Dispatches to the `do*` method for the component. */
  doComponent(c: DocOpComponent): void {
    switch (c.type) {
      case "annotationBoundary":
        return this.doAnnotationBoundary(c.boundary);
      case "characters":
        return this.doCharacters(c.characters);
      case "elementStart":
        return this.doElementStart(c.tag, c.attributes);
      case "elementEnd":
        return this.doElementEnd();
      case "retain":
        return this.doRetain(c.itemCount);
      case "deleteCharacters":
        return this.doDeleteCharacters(c.characters);
      case "deleteElementStart":
        return this.doDeleteElementStart(c.tag, c.attributes);
      case "deleteElementEnd":
        return this.doDeleteElementEnd();
      case "replaceAttributes":
        return this.doReplaceAttributes(c.oldAttributes, c.newAttributes);
      case "updateAttributes":
        return this.doUpdateAttributes(c.update);
    }
  }
}

/** A single code unit as text; an unpaired surrogate becomes U+FFFD. */
function lossyUnit(unit: number): string {
  return unit >= 0xd800 && unit <= 0xdfff ? "�" : String.fromCharCode(unit);
}

/**
 * Classifies `op` against `doc` and `schema`: whether it is well-formed,
 * applies to the document and preserves the schema. Does not modify `doc`.
 *
 * Checking stops at the first ill-formed component.
 */
export function validate(
  v: Collector,
  schema: DocumentSchema,
  doc: AutomatonDocument,
  op: DocOp,
): ValidationResult {
  const automaton = new DocOpAutomaton(doc, schema);
  let accu: ValidationResult = Valid;
  for (const component of op.components) {
    accu = mergeValidationResults(accu, automaton.checkComponent(component, v));
    if (accu === IllFormed) {
      return IllFormed;
    }
    automaton.doComponent(component);
  }
  return mergeValidationResults(accu, automaton.checkFinish(v));
}

/** Classifies a document initialization against the empty document. */
export function validateInitialization(
  v: Collector,
  schema: DocumentSchema,
  op: DocInitialization,
): ValidationResult {
  return validate(v, schema, EMPTY_DOCUMENT, op);
}

/**
 * Whether `op` is well-formed, independent of any document.
 *
 * Violations recorded in `v` other than well-formedness violations are
 * meaningless.
 */
export function isWellFormed(v: Collector, op: DocOp): boolean {
  // Validating against the empty document will usually report the operation
  // as invalid; only ill-formedness matters here.
  return validate(v, NO_SCHEMA_CONSTRAINTS, EMPTY_DOCUMENT, op) !== IllFormed;
}
