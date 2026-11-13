// Random generator of valid document operations (`RandomDocOpGenerator`).
//
// The generator drives a `DocOpAutomaton` and only ever picks components the
// automaton accepts as valid, so its output is valid for the document by
// construction. It consumes random numbers in the same order as the
// original, so that with `RandomProviderImpl` and the same seed both produce
// the same operations.
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import { AnnotationBoundaryMap, type AnnotationChange } from "../annotation";
import { Attributes, AttributesUpdate } from "../attributes";
import { OperationError } from "../error";
import { DocOp, type DocOpComponent } from "../op";
import { NO_SCHEMA_CONSTRAINTS } from "../schema";
import { cmpUtf16, isHighSurrogate } from "../utf16";
import { type AutomatonDocument, DocOpAutomaton, ValidationResult } from "../validate";

const { Valid, IllFormed } = ValidationResult;

/** Source of random numbers for the generator. */
export interface RandomProvider {
  /** An integer in `0..upperBound`. */
  nextInt(upperBound: number): number;

  nextBoolean(): boolean;
}

/** Port of the original's simplistic linear congruential `RandomProviderImpl`. */
export class RandomProviderImpl implements RandomProvider {
  private next32: number;

  private constructor(seed: number) {
    this.next32 = seed | 0;
  }

  static ofSeed(seed: number): RandomProviderImpl {
    return new RandomProviderImpl(seed);
  }

  nextInt(upperBound: number): number {
    if (!(upperBound > 0)) {
      throw new RangeError("upperBound must be positive");
    }
    // Java: next32 = (int) (0x77DD9E95L * next32 + 1), i.e. the low 32 bits.
    this.next32 = (Math.imul(0x77dd9e95, this.next32) + 1) | 0;
    const d = (this.next32 + 2147483648) / 4294967296;
    return Math.floor(d * upperBound);
  }

  nextBoolean(): boolean {
    return this.nextInt(2) !== 0;
  }
}

/** An annotation key the generator may use, with its possible values. */
export interface AnnotationOption {
  readonly key: string;
  readonly valueAlternatives: readonly (string | null)[];
}

/** Tuning knobs of the generator. The defaults are those of the original. */
export interface Parameters {
  readonly maxOpeningComponents: number;
  readonly maxInsertLength: number;
  readonly maxDeleteLength: number;
  readonly elementTypes: readonly string[];
  readonly attributeNames: readonly string[];
  /** Attribute removals are only generated if `null` is in this list. */
  readonly attributeValues: readonly (string | null)[];
  readonly annotationOptions: readonly AnnotationOption[];
  /**
   * Not in the original: mix characters outside the BMP into inserted text.
   * Changes the sequence of random numbers consumed.
   */
  readonly supplementaryCharacters: boolean;
}

export function defaultParameters(): Parameters {
  return {
    maxOpeningComponents: 16,
    maxInsertLength: 10,
    maxDeleteLength: 5,
    elementTypes: ["body", "line", "input", "image", "caption", "br"],
    attributeNames: [
      "_t",
      "t",
      "i",
      "attachment",
      "style",
      "blipId",
      "state",
      "url",
      "fontWeight",
      "fontStyle",
      "invalid_dummy",
    ],
    attributeValues: [
      null,
      "title",
      "li",
      "h1",
      "h2",
      "h3",
      "h4",
      "",
      "0",
      "1",
      "2",
      "3",
      "4",
      "5",
      "114",
      "9817",
    ],
    annotationOptions: [
      { key: "a", valueAlternatives: [null, "1", "2"] },
      { key: "b", valueAlternatives: [null, "1"] },
      { key: "c", valueAlternatives: [null, "1"] },
    ],
    supplementaryCharacters: false,
  };
}

const SUPPLEMENTARY_CHARACTERS: readonly string[] = ["😀", "𝄞", "🚀", "\u{10FFFD}"];

type Stage =
  /** All components are permitted. */
  | "unrestricted"
  /** Only components that close open deletions and insertions. */
  | "closeStructure"
  /** Close the open annotations. */
  | "closeAnnotations"
  /** Retain to the end of the document. */
  | "skipToEnd";

type ComponentGenerator =
  | "annotationBoundary"
  | "characters"
  | "elementStart"
  | "elementEnd"
  | "skip"
  | "deleteCharacters"
  | "deleteElementStart"
  | "deleteElementEnd"
  | "replaceAttributes"
  | "updateAttributes";

const COMPONENT_GENERATORS: readonly ComponentGenerator[] = [
  "annotationBoundary",
  "characters",
  "elementStart",
  "elementEnd",
  "skip",
  "deleteCharacters",
  "deleteElementStart",
  "deleteElementEnd",
  "replaceAttributes",
  "updateAttributes",
];

function randomIntFromRange(r: RandomProvider, min: number, limit: number): number {
  return r.nextInt(limit - min) + min;
}

function shuffle<T>(r: RandomProvider, a: T[]): void {
  const n = a.length;
  for (let i = 0; i < n; i++) {
    const j = randomIntFromRange(r, i, n);
    const tmp = a[i]!;
    a[i] = a[j]!;
    a[j] = tmp;
  }
}

/**
 * Tries the elements of `input` in random order and returns the first value
 * `mapper` produces.
 */
function pickRandomMappedElement<I, O>(
  r: RandomProvider,
  input: readonly I[],
  mapper: (r: RandomProvider, element: I) => O | null,
): O | null {
  const list = [...input];
  while (list.length > 0) {
    const index = randomIntFromRange(r, 0, list.length);
    const value = mapper(r, list[index]!);
    if (value !== null) {
      return value;
    }
    // Swap-remove.
    list[index] = list[list.length - 1]!;
    list.pop();
  }
  return null;
}

/** Orders `null` before any string, and strings by UTF-16 code units. */
function cmpNullable(a: string | null, b: string | null): number {
  if (a === null || b === null) {
    return a === b ? 0 : a === null ? -1 : 1;
  }
  return cmpUtf16(a, b);
}

/** The distinct values in sorted order (a `BTreeSet`). */
function sortedSet<T>(values: readonly T[], cmp: (a: T, b: T) => number): T[] {
  return [...new Set(values)].sort(cmp);
}

function attemptUpdate(attributes: Attributes, update: AttributesUpdate): Attributes | null {
  try {
    return attributes.updateWith(update);
  } catch (e) {
    if (e instanceof OperationError) {
      return null;
    }
    throw e;
  }
}

class Generator {
  constructor(readonly p: Parameters) {}

  static isValid(a: DocOpAutomaton, component: DocOpComponent): boolean {
    return a.checkComponent(component, null) === Valid;
  }

  static validOrNull(a: DocOpAutomaton, component: DocOpComponent): DocOpComponent | null {
    return Generator.isValid(a, component) ? component : null;
  }

  generateComponent(
    generator: ComponentGenerator,
    r: RandomProvider,
    a: DocOpAutomaton,
    stage: Stage,
  ): DocOpComponent | null {
    const unrestricted = stage === "unrestricted";
    const closing = stage === "closeStructure";
    const deleting = a.deletionStackComplexityMeasure() > 0;
    switch (generator) {
      case "annotationBoundary":
        switch (stage) {
          case "unrestricted":
          case "closeStructure":
            return this.annotationWithLookahead(r, a, stage);
          case "closeAnnotations":
            return Generator.closingAnnotation(a);
          case "skipToEnd":
            return null;
        }
      // eslint-disable-next-line no-fallthrough
      case "characters":
        return this.characters(r, a, stage);
      case "elementStart":
        if (!unrestricted) {
          return null;
        }
        return pickRandomMappedElement(r, this.p.elementTypes, (r, tag) =>
          this.elementStartGivenTag(r, a, tag),
        );
      case "elementEnd":
        if (unrestricted || (closing && a.insertionStackComplexityMeasure() > 0)) {
          return Generator.validOrNull(a, { type: "elementEnd" });
        }
        return null;
      case "skip":
        return Generator.skip(r, a, stage);
      case "deleteCharacters":
        return this.deleteCharacters(r, a, stage);
      case "deleteElementStart": {
        if (!(unrestricted || (closing && deleting))) {
          return null;
        }
        const tag = a.currentElementStartTag();
        const attributes = a.currentElementStartAttributes();
        if (tag === null || attributes === null) {
          return null;
        }
        return Generator.validOrNull(a, { type: "deleteElementStart", tag, attributes });
      }
      case "deleteElementEnd":
        if (unrestricted || (closing && deleting)) {
          return Generator.validOrNull(a, { type: "deleteElementEnd" });
        }
        return null;
      case "replaceAttributes": {
        if (!unrestricted) {
          return null;
        }
        const oldAttributes = a.currentElementStartAttributes();
        if (oldAttributes === null) {
          return null;
        }
        const replace = (update: AttributesUpdate): DocOpComponent | null => {
          const newAttributes = attemptUpdate(oldAttributes, update);
          return newAttributes === null
            ? null
            : { type: "replaceAttributes", oldAttributes, newAttributes };
        };
        const update = this.randomAttributesUpdate(r, oldAttributes, (update) => {
          const c = replace(update);
          return c !== null && Generator.isValid(a, c);
        });
        return update === null ? null : replace(update);
      }
      case "updateAttributes": {
        if (!unrestricted) {
          return null;
        }
        const oldAttributes = a.currentElementStartAttributes();
        if (oldAttributes === null) {
          return null;
        }
        const update = this.randomAttributesUpdate(
          r,
          oldAttributes,
          (update) => a.checkUpdateAttributes(update, null) === Valid,
        );
        return update === null ? null : { type: "updateAttributes", update };
      }
    }
  }

  static skip(r: RandomProvider, a: DocOpAutomaton, stage: Stage): DocOpComponent | null {
    const maxDistance = a.maxRetainItemCount();
    switch (stage) {
      case "unrestricted": {
        if (maxDistance === 0 || a.checkRetain(1, null) !== Valid) {
          return null;
        }
        let distance = randomIntFromRange(r, 1, maxDistance + 1);
        while (a.checkRetain(distance, null) !== Valid) {
          distance -= 1;
        }
        return { type: "retain", itemCount: distance };
      }
      case "closeStructure":
      case "closeAnnotations":
        return null;
      case "skipToEnd":
        return maxDistance === 0
          ? null
          : Generator.validOrNull(a, { type: "retain", itemCount: maxDistance });
    }
  }

  characters(r: RandomProvider, a: DocOpAutomaton, stage: Stage): DocOpComponent | null {
    if (stage !== "unrestricted" || a.checkCharacters("a", null) !== Valid) {
      return null;
    }
    const max = this.p.maxInsertLength;
    if (max === 0) {
      return null;
    }
    const count = randomIntFromRange(r, 1, max + 1);
    const startChar = (r.nextBoolean() ? "a" : "A").charCodeAt(0);
    let s = "";
    for (let i = 0; i < count; i++) {
      let c = i <= 26 ? String.fromCharCode(startChar + i) : ".";
      if (this.p.supplementaryCharacters && r.nextInt(4) === 0) {
        c = SUPPLEMENTARY_CHARACTERS[r.nextInt(SUPPLEMENTARY_CHARACTERS.length)]!;
      }
      s += c;
    }
    return { type: "characters", characters: s };
  }

  deleteCharacters(r: RandomProvider, a: DocOpAutomaton, stage: Stage): DocOpComponent | null {
    if (
      stage !== "unrestricted" &&
      (stage !== "closeStructure" || a.deletionStackComplexityMeasure() === 0)
    ) {
      return null;
    }
    if (a.nextChar(0) === null) {
      return null;
    }
    if (a.checkDeleteCharacters("a", null) === IllFormed) {
      return null;
    }
    const max = Math.min(a.maxCharactersToDelete(), this.p.maxDeleteLength);
    if (max === 0) {
      return null;
    }
    const count = randomIntFromRange(r, 1, max + 1);
    // Extend the deletion one character at a time for as long as it stays
    // valid. A surrogate pair is taken as a whole, so the deletion may be
    // one code unit longer than `count`.
    let deleted = "";
    let candidate = "";
    let i = 0;
    while (i < count) {
      const unit = a.nextChar(i);
      if (unit === null) {
        break;
      }
      candidate += String.fromCharCode(unit);
      i += 1;
      if (isHighSurrogate(unit)) {
        const low = a.nextChar(i);
        if (low === null) {
          break;
        }
        candidate += String.fromCharCode(low);
        i += 1;
      }
      // An unpaired surrogate makes the candidate ill-formed, which ends
      // the loop here too.
      if (a.checkDeleteCharacters(candidate, null) !== Valid) {
        break;
      }
      deleted = candidate;
    }
    if (deleted.length === 0) {
      return null;
    }
    return Generator.validOrNull(a, { type: "deleteCharacters", characters: deleted });
  }

  /**
   * Grows a random update of `oldAttributes` for as long as `isValid`
   * accepts it.
   */
  randomAttributesUpdate(
    r: RandomProvider,
    oldAttributes: Attributes,
    isValid: (update: AttributesUpdate) => boolean,
  ): AttributesUpdate | null {
    let accu = AttributesUpdate.EMPTY;
    if (!isValid(accu)) {
      return null;
    }
    while (r.nextBoolean()) {
      const current = accu;
      const newAccu = pickRandomMappedElement(r, this.p.attributeNames, (r, name) => {
        if (current.containsKey(name)) {
          return null;
        }
        return pickRandomMappedElement(r, this.p.attributeValues, (_r, value) => {
          let b: AttributesUpdate;
          try {
            b = current.composeWith(
              AttributesUpdate.single(name, oldAttributes.get(name), value),
            );
          } catch (e) {
            if (e instanceof OperationError) {
              return null;
            }
            throw e;
          }
          return isValid(b) ? b : null;
        });
      });
      if (newAccu === null) {
        return accu;
      }
      accu = newAccu;
    }
    return accu;
  }

  elementStartGivenTag(r: RandomProvider, a: DocOpAutomaton, tag: string): DocOpComponent | null {
    const empty = Attributes.EMPTY;
    if (a.checkElementStart(tag, empty, null) !== Valid) {
      // Early exit if we can't build an element start with this tag.
      return null;
    }
    const update = this.randomAttributesUpdate(r, empty, (update) => {
      const attributes = attemptUpdate(empty, update);
      return attributes !== null && a.checkElementStart(tag, attributes, null) === Valid;
    });
    if (update === null) {
      return null;
    }
    const attributes = attemptUpdate(empty, update);
    return attributes === null ? null : { type: "elementStart", tag, attributes };
  }

  static closingAnnotation(a: DocOpAutomaton): DocOpComponent | null {
    const open = a.openAnnotations();
    if (open.length === 0) {
      return null;
    }
    return { type: "annotationBoundary", boundary: AnnotationBoundaryMap.unchecked(open, []) };
  }

  /**
   * Searches for an annotation boundary after which some valid component can
   * follow.
   */
  annotationWithLookahead(r: RandomProvider, a: DocOpAutomaton, stage: Stage): DocOpComponent | null {
    const probe = AnnotationBoundaryMap.unchecked([], [{ key: "a", oldValue: null, newValue: "1" }]);
    if (a.checkAnnotationBoundary(probe, null) === IllFormed) {
      return null;
    }
    const keys = sortedSet(
      [
        ...this.p.annotationOptions.map((option) => option.key),
        ...a.currentAnnotations().entries.map((e) => e.name),
        ...a.inheritedAnnotations().entries.map((e) => e.name),
      ],
      cmpUtf16,
    );
    const search = new AnnotationSearch(this, a, stage, keys, a.openAnnotations());
    const boundary = search.nextKey(r, 0);
    return boundary === null ? null : { type: "annotationBoundary", boundary };
  }

  pickComponent(r: RandomProvider, a: DocOpAutomaton, stage: Stage): DocOpComponent | null {
    return pickRandomMappedElement(r, COMPONENT_GENERATORS, (r, generator) =>
      this.generateComponent(generator, r, a, stage),
    );
  }
}

/**
 * Backtracking search over annotation boundaries: for every key, either
 * leave it alone, end it (if open) or change it between any pair of
 * plausible old and new values.
 */
class AnnotationSearch {
  private readonly keysToEnd: string[] = [];
  private readonly changes: AnnotationChange[] = [];

  constructor(
    private readonly generator: Generator,
    private readonly a: DocOpAutomaton,
    private readonly stage: Stage,
    private readonly keys: readonly string[],
    private readonly open: readonly string[],
  ) {}

  private tryThisOption(r: RandomProvider): AnnotationBoundaryMap | null {
    const map = AnnotationBoundaryMap.unchecked([...this.keysToEnd], [...this.changes]);
    const temp = this.a.clone();
    temp.doAnnotationBoundary(map);
    return this.generator.pickComponent(r, temp, this.stage) !== null ? map : null;
  }

  private take(r: RandomProvider, nextKeyIndex: number, key: string): AnnotationBoundaryMap | null {
    if (this.open.includes(key)) {
      this.keysToEnd.push(key);
      const found = this.nextKey(r, nextKeyIndex);
      this.keysToEnd.pop();
      if (found !== null) {
        return found;
      }
    }
    const values: (string | null)[] = [];
    for (const option of this.generator.p.annotationOptions) {
      if (option.key === key) {
        values.push(...option.valueAlternatives);
      }
    }
    values.push(this.a.inheritedAnnotations().get(key));
    const newValues = sortedSet(values, cmpNullable);
    values.push(this.a.currentAnnotations().get(key));
    const oldValues = sortedSet(values, cmpNullable);
    shuffle(r, oldValues);
    shuffle(r, newValues);
    for (const oldValue of oldValues) {
      for (const newValue of newValues) {
        this.changes.push({ key, oldValue, newValue });
        const found = this.nextKey(r, nextKeyIndex);
        this.changes.pop();
        if (found !== null) {
          return found;
        }
      }
    }
    return null;
  }

  nextKey(r: RandomProvider, nextKeyIndex: number): AnnotationBoundaryMap | null {
    const key = this.keys[nextKeyIndex];
    if (key === undefined) {
      return this.tryThisOption(r);
    }
    if (r.nextBoolean()) {
      return this.take(r, nextKeyIndex + 1, key) ?? this.nextKey(r, nextKeyIndex + 1);
    }
    return this.nextKey(r, nextKeyIndex + 1) ?? this.take(r, nextKeyIndex + 1, key);
  }
}

/**
 * Generates a random operation that is valid for `doc` (without schema
 * constraints).
 *
 * Throws a plain `Error` if no valid component can be found at some point,
 * which would indicate a bug in the generator or the automaton.
 */
export function generateRandomOp(r: RandomProvider, p: Parameters, doc: AutomatonDocument): DocOp {
  const generator = new Generator(p);
  const a = new DocOpAutomaton(doc, NO_SCHEMA_CONSTRAINTS);
  const components: DocOpComponent[] = [];
  const step = (stage: Stage): boolean => {
    const component = generator.pickComponent(r, a, stage);
    if (component === null) {
      throw new Error(`no valid component in stage ${stage}`);
    }
    a.doComponent(component);
    components.push(component);
    return component.type === "annotationBoundary";
  };
  const desiredNumComponents = randomIntFromRange(r, 0, p.maxOpeningComponents);
  let numComponentsPicked = 0;
  while (numComponentsPicked < desiredNumComponents) {
    if (!step("unrestricted")) {
      numComponentsPicked += 1;
    }
  }
  while (a.deletionStackComplexityMeasure() > 0) {
    step("closeStructure");
  }
  while (a.insertionStackComplexityMeasure() > 0) {
    step("closeStructure");
  }
  if (a.openAnnotations().length > 0) {
    step("closeAnnotations");
  }
  if (a.maxRetainItemCount() > 0) {
    step("skipToEnd");
  }
  if (a.checkFinish(null) !== Valid) {
    throw new Error("generated operation does not finish validly");
  }
  return DocOp.create(components);
}
