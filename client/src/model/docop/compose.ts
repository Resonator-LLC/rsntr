// Composition of document operations (`Composer` and `DocOpCollector`).
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import { AnnotationBoundaryMap, type AnnotationChange } from "./annotation";
import type { Attributes, AttributesUpdate } from "./attributes";
import { OperationError } from "./error";
import { type OpNormalizer, newNormalizer } from "./normalize";
import { DocInitialization, DocOp, type DocOpComponent } from "./op";
import { splitAtUtf16 } from "./utf16";
import { initialDocumentLength, resultingDocumentLength } from "./util";

/** The old and new value of an annotation change. */
export interface ValueUpdate {
  readonly oldValue: string | null;
  readonly newValue: string | null;
}

/**
 * What the composer is waiting for.
 *
 * `*Pre` states consume components of the first operation; `*Post` states
 * hold a pending component of the first operation (or the tail of one) and
 * consume components of the second.
 */
type Target =
  | { readonly type: "defaultPre" }
  /** The second operation retains this many more items. */
  | { readonly type: "retainPre"; readonly itemCount: number }
  /** The second operation deletes these characters. */
  | { readonly type: "deleteCharactersPre"; readonly characters: string }
  | { readonly type: "retainPost"; readonly itemCount: number }
  | { readonly type: "charactersPost"; readonly characters: string }
  | { readonly type: "elementStartPost"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "elementEndPost" }
  | {
      readonly type: "replaceAttributesPost";
      readonly oldAttributes: Attributes;
      readonly newAttributes: Attributes;
    }
  | { readonly type: "updateAttributesPost"; readonly update: AttributesUpdate }
  /** The first operation is exhausted; only insertions may follow. */
  | { readonly type: "finisherPost" };

const DEFAULT_PRE: Target = { type: "defaultPre" };

function isPostTarget(target: Target): boolean {
  switch (target.type) {
    case "defaultPre":
    case "retainPre":
    case "deleteCharactersPre":
      return false;
    default:
      return true;
  }
}

const retainPre = (itemCount: number): Target => ({ type: "retainPre", itemCount });
const retainPost = (itemCount: number): Target => ({ type: "retainPost", itemCount });
const deleteCharactersPre = (characters: string): Target => ({
  type: "deleteCharactersPre",
  characters,
});
const charactersPost = (characters: string): Target => ({ type: "charactersPost", characters });

/**
 * The state after `size` items of a pending retain of `itemCount` have been
 * matched.
 */
function cancelRetain(itemCount: number, size: number, rest: (n: number) => Target): Target {
  return size < itemCount ? rest(itemCount - size) : DEFAULT_PRE;
}

/** The state after `size` code units of pending `chars` have been matched. */
function cancelCharacters(chars: string, size: number, rest: (s: string) => Target): Target {
  return size < chars.length ? rest(splitAtUtf16(chars, size)[1]) : DEFAULT_PRE;
}

/** The state after a pending single-item component met a retain. */
function afterSingleItem(itemCount: number): Target {
  return itemCount > 1 ? retainPre(itemCount - 1) : DEFAULT_PRE;
}

class Composer {
  private readonly normalizer: OpNormalizer = newNormalizer();
  private readonly preAnnotations = new Map<string, ValueUpdate>();
  private readonly postAnnotations = new Map<string, ValueUpdate>();
  private preAnnotationQueue: AnnotationBoundaryMap[] = [];
  private postAnnotationQueue: AnnotationBoundaryMap[] = [];
  private target: Target = DEFAULT_PRE;

  private flushPreAnnotations(): void {
    const queue = this.preAnnotationQueue;
    this.preAnnotationQueue = [];
    for (const map of queue) {
      const endKeys: string[] = [];
      const changes: AnnotationChange[] = [];
      for (const key of map.endKeys) {
        const postValues = this.postAnnotations.get(key);
        if (postValues !== undefined) {
          changes.push({ key, oldValue: postValues.oldValue, newValue: postValues.newValue });
        } else {
          endKeys.push(key);
        }
        this.preAnnotations.delete(key);
      }
      for (const change of map.changes) {
        const postValues = this.postAnnotations.get(change.key);
        changes.push({
          key: change.key,
          oldValue: change.oldValue,
          newValue: postValues !== undefined ? postValues.newValue : change.newValue,
        });
        this.preAnnotations.set(change.key, {
          oldValue: change.oldValue,
          newValue: change.newValue,
        });
      }
      this.normalizer.annotationBoundary(AnnotationBoundaryMap.create(endKeys, changes));
    }
  }

  private flushPostAnnotations(): void {
    const queue = this.postAnnotationQueue;
    this.postAnnotationQueue = [];
    for (const map of queue) {
      const endKeys: string[] = [];
      const changes: AnnotationChange[] = [];
      for (const key of map.endKeys) {
        const preValues = this.preAnnotations.get(key);
        if (preValues !== undefined) {
          changes.push({ key, oldValue: preValues.oldValue, newValue: preValues.newValue });
        } else {
          endKeys.push(key);
        }
        this.postAnnotations.delete(key);
      }
      for (const change of map.changes) {
        const preValues = this.preAnnotations.get(change.key);
        changes.push({
          key: change.key,
          oldValue: preValues !== undefined ? preValues.oldValue : change.oldValue,
          newValue: change.newValue,
        });
        this.postAnnotations.set(change.key, {
          oldValue: change.oldValue,
          newValue: change.newValue,
        });
      }
      this.normalizer.annotationBoundary(AnnotationBoundaryMap.create(endKeys, changes));
    }
  }

  private flushAnnotations(): void {
    this.flushPreAnnotations();
    this.flushPostAnnotations();
  }

  /**
   * Feeds a component of the first operation; the current target is a `*Pre`
   * state.
   */
  private applyPre(c: DocOpComponent): void {
    // Handled the same way by every pre target.
    switch (c.type) {
      case "deleteCharacters":
      case "deleteElementStart":
      case "deleteElementEnd":
        this.flushPreAnnotations();
        this.normalizer.push(c);
        return;
      case "annotationBoundary":
        this.preAnnotationQueue.push(c.boundary);
        return;
      default:
        break;
    }
    const target = this.target;
    this.target = DEFAULT_PRE;
    switch (target.type) {
      case "defaultPre":
        switch (c.type) {
          case "retain":
            this.target = retainPost(c.itemCount);
            break;
          case "characters":
            this.target = charactersPost(c.characters);
            break;
          case "elementStart":
            this.target = { type: "elementStartPost", tag: c.tag, attributes: c.attributes };
            break;
          case "elementEnd":
            this.target = { type: "elementEndPost" };
            break;
          case "replaceAttributes":
            this.target = {
              type: "replaceAttributesPost",
              oldAttributes: c.oldAttributes,
              newAttributes: c.newAttributes,
            };
            break;
          case "updateAttributes":
            this.target = { type: "updateAttributesPost", update: c.update };
            break;
        }
        return;
      case "retainPre": {
        const pending = target.itemCount;
        this.flushAnnotations();
        switch (c.type) {
          case "retain":
            if (c.itemCount <= pending) {
              this.normalizer.retain(c.itemCount);
              this.target = cancelRetain(pending, c.itemCount, retainPre);
            } else {
              this.normalizer.retain(pending);
              this.target = retainPost(c.itemCount - pending);
            }
            break;
          case "characters": {
            const length = c.characters.length;
            if (length <= pending) {
              this.normalizer.characters(c.characters);
              this.target = cancelRetain(pending, length, retainPre);
            } else {
              const [head, tail] = splitAtUtf16(c.characters, pending);
              this.normalizer.characters(head);
              this.target = charactersPost(tail);
            }
            break;
          }
          default:
            // Element start, element end or attribute change.
            this.normalizer.push(c);
            this.target = cancelRetain(pending, 1, retainPre);
        }
        return;
      }
      case "deleteCharactersPre": {
        const pending = target.characters;
        switch (c.type) {
          case "retain":
            this.flushAnnotations();
            if (c.itemCount <= pending.length) {
              const [head] = splitAtUtf16(pending, c.itemCount);
              this.normalizer.deleteCharacters(head);
              this.target = cancelCharacters(pending, c.itemCount, deleteCharactersPre);
            } else {
              this.normalizer.deleteCharacters(pending);
              this.target = retainPost(c.itemCount - pending.length);
            }
            return;
          case "characters": {
            this.flushAnnotations();
            // As in the original, the inserted and deleted characters are
            // not compared.
            const length = c.characters.length;
            this.target =
              length <= pending.length
                ? cancelCharacters(pending, length, deleteCharactersPre)
                : charactersPost(splitAtUtf16(c.characters, pending.length)[1]);
            return;
          }
          default:
            throw OperationError.illegalComposition();
        }
      }
      default:
        // Unreachable: applyPre is only called in a pre state.
        throw OperationError.illegalComposition();
    }
  }

  /**
   * Feeds a component of the second operation; the current target is a
   * `*Post` state.
   */
  private applyPost(c: DocOpComponent): void {
    // Handled the same way by every post target.
    switch (c.type) {
      case "characters":
      case "elementStart":
      case "elementEnd":
        this.flushPostAnnotations();
        this.normalizer.push(c);
        return;
      case "annotationBoundary":
        this.postAnnotationQueue.push(c.boundary);
        return;
      default:
        break;
    }
    const target = this.target;
    this.target = DEFAULT_PRE;
    switch (target.type) {
      case "retainPost": {
        const pending = target.itemCount;
        this.flushAnnotations();
        switch (c.type) {
          case "retain":
            if (c.itemCount <= pending) {
              this.normalizer.retain(c.itemCount);
              this.target = cancelRetain(pending, c.itemCount, retainPost);
            } else {
              this.normalizer.retain(pending);
              this.target = retainPre(c.itemCount - pending);
            }
            break;
          case "deleteCharacters": {
            const length = c.characters.length;
            if (length <= pending) {
              this.normalizer.deleteCharacters(c.characters);
              this.target = cancelRetain(pending, length, retainPost);
            } else {
              const [head, tail] = splitAtUtf16(c.characters, pending);
              this.normalizer.deleteCharacters(head);
              this.target = deleteCharactersPre(tail);
            }
            break;
          }
          default:
            // Element deletion or attribute change.
            this.normalizer.push(c);
            this.target = cancelRetain(pending, 1, retainPost);
        }
        return;
      }
      case "charactersPost": {
        const pending = target.characters;
        switch (c.type) {
          case "retain":
            this.flushAnnotations();
            if (c.itemCount <= pending.length) {
              const [head] = splitAtUtf16(pending, c.itemCount);
              this.normalizer.characters(head);
              this.target = cancelCharacters(pending, c.itemCount, charactersPost);
            } else {
              this.normalizer.characters(pending);
              this.target = retainPre(c.itemCount - pending.length);
            }
            return;
          case "deleteCharacters": {
            this.flushAnnotations();
            // As in the original, the inserted and deleted characters are
            // not compared.
            const length = c.characters.length;
            this.target =
              length <= pending.length
                ? cancelCharacters(pending, length, charactersPost)
                : deleteCharactersPre(splitAtUtf16(c.characters, pending.length)[1]);
            return;
          }
          default:
            throw OperationError.illegalComposition();
        }
      }
      case "elementStartPost":
        switch (c.type) {
          case "retain":
            this.flushAnnotations();
            this.normalizer.elementStart(target.tag, target.attributes);
            this.target = afterSingleItem(c.itemCount);
            return;
          case "deleteElementStart":
            this.flushAnnotations();
            return;
          case "replaceAttributes":
            this.flushAnnotations();
            this.normalizer.elementStart(target.tag, c.newAttributes);
            return;
          case "updateAttributes":
            this.flushAnnotations();
            this.normalizer.elementStart(target.tag, target.attributes.updateWith(c.update));
            return;
          default:
            throw OperationError.illegalComposition();
        }
      case "elementEndPost":
        switch (c.type) {
          case "retain":
            this.flushAnnotations();
            this.normalizer.elementEnd();
            this.target = afterSingleItem(c.itemCount);
            return;
          case "deleteElementEnd":
            this.flushAnnotations();
            return;
          default:
            throw OperationError.illegalComposition();
        }
      case "replaceAttributesPost":
        switch (c.type) {
          case "retain":
            this.flushAnnotations();
            this.normalizer.replaceAttributes(target.oldAttributes, target.newAttributes);
            this.target = afterSingleItem(c.itemCount);
            return;
          case "deleteElementStart":
            this.flushAnnotations();
            this.normalizer.deleteElementStart(c.tag, target.oldAttributes);
            return;
          case "replaceAttributes":
            this.flushAnnotations();
            this.normalizer.replaceAttributes(target.oldAttributes, c.newAttributes);
            return;
          case "updateAttributes":
            this.flushAnnotations();
            this.normalizer.replaceAttributes(
              target.oldAttributes,
              target.newAttributes.updateWith(c.update),
            );
            return;
          default:
            throw OperationError.illegalComposition();
        }
      case "updateAttributesPost": {
        const pending = target.update;
        switch (c.type) {
          case "retain":
            this.flushAnnotations();
            this.normalizer.updateAttributes(pending);
            this.target = afterSingleItem(c.itemCount);
            return;
          case "deleteElementStart":
            this.flushAnnotations();
            this.normalizer.deleteElementStart(c.tag, c.attributes.updateWith(pending.invert()));
            return;
          case "replaceAttributes":
            this.flushAnnotations();
            this.normalizer.replaceAttributes(
              c.oldAttributes.updateWith(pending.invert()),
              c.newAttributes,
            );
            return;
          case "updateAttributes":
            this.flushAnnotations();
            this.normalizer.updateAttributes(pending.composeWith(c.update));
            return;
          default:
            throw OperationError.illegalComposition();
        }
      }
      default:
        // The finisher accepts insertions only; pre states are unreachable
        // here.
        throw OperationError.illegalComposition();
    }
  }

  composeOperations(op1: DocOp, op2: DocOp): DocOpComponent[] {
    const second = op2.components;
    let index = 0;
    for (const component of op1.components) {
      this.applyPre(component);
      while (isPostTarget(this.target)) {
        const next = second[index++];
        if (next === undefined) {
          throw new OperationError({
            kind: "sizeMismatch",
            resulting: resultingDocumentLength(op1),
            initial: initialDocumentLength(op2),
          });
        }
        this.applyPost(next);
      }
    }
    // Note that, as in the original, a retain or deletion of the second
    // operation left pending at this point goes unreported.
    if (index < second.length) {
      this.target = { type: "finisherPost" };
      for (; index < second.length; index++) {
        this.applyPost(second[index]!);
      }
    }
    this.flushAnnotations();
    return this.normalizer.finish();
  }
}

/**
 * Composes two operations: the result is equivalent to applying `op1` and
 * then `op2`. The result is normalized and checked for well-formedness.
 */
export function compose(op1: DocOp, op2: DocOp): DocOp {
  return DocOp.create(new Composer().composeOperations(op1, op2));
}

/** Like {@link compose}, but does not check the result for well-formedness. */
export function composeUnchecked(op1: DocOp, op2: DocOp): DocOp {
  return DocOp.unchecked(new Composer().composeOperations(op1, op2));
}

/** Applies an operation to a document given as an initialization. */
export function composeInitialization(op1: DocInitialization, op2: DocOp): DocInitialization {
  return DocInitialization.fromOp(compose(op1, op2));
}

/** Composes a sequence of operations; `null` for an empty sequence. */
export function composeAll(operations: Iterable<DocOp>): DocOp | null {
  const collector = new DocOpCollector();
  for (const operation of operations) {
    collector.add(operation);
  }
  return collector.composeAll();
}

/**
 * Collects operations and composes them pairwise in a tree-like manner,
 * which costs O(n log N) instead of O(n^2) for sequential composition of N
 * operations of total size n.
 */
export class DocOpCollector {
  // Slot i holds either nothing or the composition of 2^i consecutive
  // operations; later operations sit in lower slots.
  private operations: (DocOp | null)[] = [];

  /** Adds the next operation of the sequence. */
  add(operation: DocOp): void {
    const slots = this.operations;
    for (let i = 0; i < slots.length; i++) {
      const earlier = slots[i] ?? null;
      slots[i] = null;
      if (earlier === null) {
        slots[i] = operation;
        return;
      }
      operation = compose(earlier, operation);
    }
    slots.push(operation);
  }

  /**
   * Composes everything collected so far and empties the collector. Returns
   * `null` if nothing was collected.
   */
  composeAll(): DocOp | null {
    const slots = this.operations;
    this.operations = [];
    let result: DocOp | null = null;
    for (const operation of slots) {
      if (operation !== null) {
        result = result !== null ? compose(operation, result) : operation;
      }
    }
    return result;
  }

  isEmpty(): boolean {
    return this.operations.length === 0;
  }
}
