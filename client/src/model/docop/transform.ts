// Transformation of concurrent document operations (`Transformer`,
// `Decomposer`, `InsertionTransformer`, `InsertionNoninsertionTransformer`
// and `NoninsertionTransformer`).
//
// Where the original uses two "target" objects that reference each other,
// each transformer here is one class holding the state of both sides.
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import { AnnotationBoundaryMap, type AnnotationChange } from "./annotation";
import { type Attributes, AttributesUpdate } from "./attributes";
import { type ValueUpdate, compose } from "./compose";
import { OperationError, TransformError } from "./error";
import { type OpNormalizer, RangeNormalizer, newNormalizer } from "./normalize";
import { DocOp, type DocOpComponent, componentTypeName } from "./op";
import { BufferSink } from "./sink";
import { MAX_COUNT, splitAtUtf16 } from "./utf16";

/** The result of transforming a client operation against a server operation. */
export interface OperationPair {
  /** The client operation, transformed to apply after the server's. */
  readonly clientOp: DocOp;
  /** The server operation, transformed to apply after the client's. */
  readonly serverOp: DocOp;
}

/**
 * Transforms a pair of operations that apply to the same document, such
 * that `compose(client, server') == compose(server, client')` in effect.
 *
 * Throws {@link TransformError}; an {@link OperationError} raised on the way
 * is wrapped in one of kind `"operation"`.
 */
export function transform(clientOp: DocOp, serverOp: DocOp): OperationPair {
  try {
    // Each operation is decomposed into an insertion operation followed by
    // an insertion-free operation, and four transforms are performed,
    // structured as in the following diagram:
    //     ci0     cn0
    // si0     si1     si2
    //     ci1     cn1
    // sn0     sn1     sn2
    //     ci2     cn2
    const [ci0, cn0] = decompose(clientOp);
    const [si0, sn0] = decompose(serverOp);
    const [ci1, si1] = transformInsertions(ci0, si0);
    const [ci2, sn1] = transformInsertionNoninsertion(ci1, sn0);
    const [si2, cn1] = transformInsertionNoninsertion(si1, cn0);
    const [cn2, sn2] = transformNoninsertions(cn1, sn1);
    return {
      clientOp: compose(ci2, cn2),
      serverOp: compose(si2, sn2),
    };
  } catch (e) {
    if (e instanceof OperationError) {
      throw TransformError.fromOperation(e);
    }
    throw e;
  }
}

/**
 * Decomposes an operation into an insertion operation and an operation that
 * contains no insertions, such that their composition is equivalent to the
 * original.
 */
export function decompose(op: DocOp): [insertionOp: DocOp, noninsertionOp: DocOp] {
  const insertionOp = newNormalizer();
  const noninsertionOp = newNormalizer();
  for (const c of op.components) {
    switch (c.type) {
      case "retain":
        insertionOp.retain(c.itemCount);
        noninsertionOp.retain(c.itemCount);
        break;
      case "characters":
        insertionOp.push(c);
        noninsertionOp.retain(c.characters.length);
        break;
      case "elementStart":
      case "elementEnd":
        insertionOp.push(c);
        noninsertionOp.retain(1);
        break;
      case "deleteCharacters":
        insertionOp.retain(c.characters.length);
        noninsertionOp.push(c);
        break;
      case "deleteElementStart":
      case "deleteElementEnd":
      case "replaceAttributes":
      case "updateAttributes":
        insertionOp.retain(1);
        noninsertionOp.push(c);
        break;
      case "annotationBoundary":
        noninsertionOp.push(c);
        break;
    }
  }
  return [DocOp.create(insertionOp.finish()), DocOp.create(noninsertionOp.finish())];
}

/** Index of one of the two operations being transformed. */
type Side = 0 | 1;
const FIRST: Side = 0;
const SECOND: Side = 1;

const otherSide = (side: Side): Side => (side === FIRST ? SECOND : FIRST);

/**
 * Tracks the positions of two cursors relative to each other, in items of
 * the document both operations apply to (`PositionTracker`).
 */
class PositionTracker {
  /** Position of the first cursor minus position of the second. */
  private position = 0;

  /** Position of `side`'s cursor relative to the other side's. */
  get(side: Side): number {
    // `0 - x` rather than `-x`, to avoid negative zero.
    return side === FIRST ? this.position : 0 - this.position;
  }

  increase(side: Side, amount: number): void {
    // Saturating, so that absurd retain counts stay finite. Amounts that
    // are not positive numbers (which Rust's types exclude) are ignored.
    const delta = amount > 0 ? Math.min(amount, MAX_COUNT) : 0;
    const position = side === FIRST ? this.position + delta : this.position - delta;
    this.position = Math.max(-MAX_COUNT, Math.min(MAX_COUNT, position));
  }
}

/**
 * Returned by the `resolveRange` methods when the whole range was resolved.
 * As in the original, indistinguishable from a resolved portion of -1,
 * which callers treat the same way.
 */
const ENTIRELY_RESOLVED = -1;

/**
 * Feeds the components of two operations to `step` in a linearly ordered
 * interleaving: the first operation leads, and the second catches up
 * whenever the first cursor is ahead.
 */
function interleave(
  ops: readonly [DocOp, DocOp],
  names: readonly [string, string],
  position: () => number,
  step: (side: Side, component: DocOpComponent) => void,
): void {
  const first = ops[FIRST].components;
  const second = ops[SECOND].components;
  let secondIndex = 0;
  for (let index = 0; index < first.length; index++) {
    step(FIRST, first[index]!);
    while (position() > 0) {
      const component = second[secondIndex++];
      if (component === undefined) {
        throw new TransformError({
          kind: "ranOut",
          this: names[FIRST],
          other: names[SECOND],
          otherSize: second.length,
          index: index + 1,
          size: first.length,
          spare: position(),
        });
      }
      step(SECOND, component);
    }
  }
  for (; secondIndex < second.length; secondIndex++) {
    step(SECOND, second[secondIndex]!);
  }
}

function unexpected(component: DocOpComponent, op: string): TransformError {
  return new TransformError({
    kind: "unexpectedComponent",
    component: componentTypeName(component),
    op,
  });
}

// ---------------------------------------------------------------------------
// Insertion vs insertion

class InsertionTransformer {
  readonly outputs: readonly [RangeNormalizer, RangeNormalizer] = [
    new RangeNormalizer(new BufferSink()),
    new RangeNormalizer(new BufferSink()),
  ];
  readonly tracker = new PositionTracker();

  step(side: Side, c: DocOpComponent): void {
    const other = otherSide(side);
    switch (c.type) {
      case "retain": {
        const oldPosition = this.tracker.get(side);
        this.tracker.increase(side, c.itemCount);
        if (this.tracker.get(side) < 0) {
          this.outputs[side].retain(c.itemCount);
          this.outputs[other].retain(c.itemCount);
        } else if (oldPosition < 0) {
          this.outputs[side].retain(-oldPosition);
          this.outputs[other].retain(-oldPosition);
        }
        return;
      }
      case "characters":
        this.outputs[side].push(c);
        this.outputs[other].retain(c.characters.length);
        return;
      case "elementStart":
      case "elementEnd":
        this.outputs[side].push(c);
        this.outputs[other].retain(1);
        return;
      default:
        throw unexpected(c, "insertion");
    }
  }
}

/**
 * Transforms a pair of insertion operations. On a tie the client's insertion
 * ends up before the server's.
 */
function transformInsertions(clientOp: DocOp, serverOp: DocOp): [DocOp, DocOp] {
  const transformer = new InsertionTransformer();
  interleave(
    [clientOp, serverOp],
    ["client", "server"],
    () => transformer.tracker.get(FIRST),
    (side, c) => transformer.step(side, c),
  );
  const [client, server] = transformer.outputs;
  return [DocOp.create(client.finish()), DocOp.create(server.finish())];
}

// ---------------------------------------------------------------------------
// Insertion vs insertion-free

/**
 * The effect of the most recent component of an insertion-free operation on
 * the range that the other operation has yet to pass. Resolution trims the
 * characters of a deletion down to the unresolved part.
 */
type RangeCache =
  | { readonly type: "retain" }
  | { readonly type: "deleteCharacters"; characters: string }
  | { readonly type: "deleteElementStart"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "deleteElementEnd" }
  | {
      readonly type: "replaceAttributes";
      readonly oldAttributes: Attributes;
      readonly newAttributes: Attributes;
    }
  | { readonly type: "updateAttributes"; readonly update: AttributesUpdate };

const RETAIN_CACHE: RangeCache = { type: "retain" };

class InsertionNoninsertionTransformer {
  readonly insertionOutput = new RangeNormalizer(new BufferSink());
  readonly noninsertionOutput: OpNormalizer = newNormalizer();
  /** Insertion cursor is FIRST, insertion-free cursor is SECOND. */
  readonly tracker = new PositionTracker();
  private rangeCache: RangeCache = RETAIN_CACHE;
  /** Current depth of element deletions. */
  private depth = 0;

  /** Emits the cached insertion-free component for the next `itemCount` items. */
  private resolve(cache: RangeCache, itemCount: number): void {
    switch (cache.type) {
      case "retain":
        this.noninsertionOutput.retain(itemCount);
        this.insertionOutput.retain(itemCount);
        break;
      case "deleteCharacters": {
        const [head, tail] = splitAtUtf16(cache.characters, itemCount);
        this.noninsertionOutput.deleteCharacters(head);
        cache.characters = tail;
        break;
      }
      case "deleteElementStart":
        this.noninsertionOutput.deleteElementStart(cache.tag, cache.attributes);
        this.depth += 1;
        break;
      case "deleteElementEnd":
        this.noninsertionOutput.deleteElementEnd();
        this.depth -= 1;
        break;
      case "replaceAttributes":
        this.noninsertionOutput.replaceAttributes(cache.oldAttributes, cache.newAttributes);
        this.insertionOutput.retain(1);
        break;
      case "updateAttributes":
        this.noninsertionOutput.updateAttributes(cache.update);
        this.insertionOutput.retain(1);
        break;
    }
  }

  /**
   * Advances the insertion-free cursor by `size` and resolves as much of
   * `cache` as the insertion cursor has already passed. Returns the resolved
   * portion, or -1 if the entire range was resolved.
   */
  private resolveRange(size: number, cache: RangeCache): number {
    const oldPosition = this.tracker.get(SECOND);
    this.tracker.increase(SECOND, size);
    if (this.tracker.get(SECOND) > 0) {
      if (oldPosition < 0) {
        this.resolve(cache, -oldPosition);
      }
      return 0 - oldPosition;
    }
    this.resolve(cache, size);
    return ENTIRELY_RESOLVED;
  }

  step(side: Side, c: DocOpComponent): void {
    if (side === FIRST) {
      this.stepInsertion(c);
    } else {
      this.stepNoninsertion(c);
    }
  }

  private stepInsertion(c: DocOpComponent): void {
    switch (c.type) {
      case "retain": {
        const oldPosition = this.tracker.get(FIRST);
        this.tracker.increase(FIRST, c.itemCount);
        if (this.tracker.get(FIRST) < 0) {
          this.resolve(this.rangeCache, c.itemCount);
        } else if (oldPosition < 0) {
          this.resolve(this.rangeCache, -oldPosition);
        }
        return;
      }
      case "characters":
        if (this.depth > 0) {
          this.noninsertionOutput.deleteCharacters(c.characters);
        } else {
          this.insertionOutput.push(c);
          this.noninsertionOutput.retain(c.characters.length);
        }
        return;
      case "elementStart":
        if (this.depth > 0) {
          this.noninsertionOutput.deleteElementStart(c.tag, c.attributes);
        } else {
          this.insertionOutput.push(c);
          this.noninsertionOutput.retain(1);
        }
        return;
      case "elementEnd":
        if (this.depth > 0) {
          this.noninsertionOutput.deleteElementEnd();
        } else {
          this.insertionOutput.push(c);
          this.noninsertionOutput.retain(1);
        }
        return;
      default:
        throw unexpected(c, "insertion");
    }
  }

  private stepNoninsertion(c: DocOpComponent): void {
    let cache: RangeCache;
    switch (c.type) {
      case "retain":
        this.resolveRange(c.itemCount, RETAIN_CACHE);
        this.rangeCache = RETAIN_CACHE;
        return;
      case "annotationBoundary":
        this.noninsertionOutput.push(c);
        return;
      case "deleteCharacters": {
        // Resolution trims the cache down to the unresolved part.
        const deletion: RangeCache = { type: "deleteCharacters", characters: c.characters };
        if (this.resolveRange(c.characters.length, deletion) >= 0) {
          this.rangeCache = deletion;
        }
        return;
      }
      case "deleteElementStart":
        cache = { type: "deleteElementStart", tag: c.tag, attributes: c.attributes };
        break;
      case "deleteElementEnd":
        cache = { type: "deleteElementEnd" };
        break;
      case "replaceAttributes":
        cache = {
          type: "replaceAttributes",
          oldAttributes: c.oldAttributes,
          newAttributes: c.newAttributes,
        };
        break;
      case "updateAttributes":
        cache = { type: "updateAttributes", update: c.update };
        break;
      default:
        throw unexpected(c, "insertion-free");
    }
    if (this.resolveRange(1, cache) === 0) {
      this.rangeCache = cache;
    }
  }
}

/**
 * Transforms an insertion operation against an insertion-free operation.
 * Insertions that fall inside a deleted element are deleted along with it.
 */
function transformInsertionNoninsertion(insertionOp: DocOp, noninsertionOp: DocOp): [DocOp, DocOp] {
  const transformer = new InsertionNoninsertionTransformer();
  interleave(
    [insertionOp, noninsertionOp],
    ["insertion", "noninsertion"],
    () => transformer.tracker.get(FIRST),
    (side, c) => transformer.step(side, c),
  );
  return [
    DocOp.create(transformer.insertionOutput.finish()),
    DocOp.create(transformer.noninsertionOutput.finish()),
  ];
}

// ---------------------------------------------------------------------------
// Insertion-free vs insertion-free

const CLIENT: Side = 0;
const SERVER: Side = 1;

/** Tracks annotation changes of one operation at the current cursor position. */
class AnnotationTracker {
  /** Changes open in the original operation. */
  readonly tracked = new Map<string, ValueUpdate>();
  /** Changes open in the transformed output. */
  readonly active = new Map<string, ValueUpdate>();
  /**
   * Changes to restore when the current deletion concludes; `null` means the
   * key is to be ended.
   */
  temporary = new Map<string, ValueUpdate | null>();
  /**
   * What `active` held, per key, when the key last changed since the
   * trackers were synced.
   */
  readonly propagating = new Map<string, ValueUpdate | null>();
}

class NoninsertionSide {
  readonly output: OpNormalizer = newNormalizer();
  readonly tracker = new AnnotationTracker();
  rangeCache: RangeCache = RETAIN_CACHE;
}

function boundary(
  endKeys: readonly string[],
  changes: readonly AnnotationChange[],
): AnnotationBoundaryMap {
  return AnnotationBoundaryMap.create(endKeys, changes);
}

function change(key: string, oldValue: string | null, newValue: string | null): AnnotationChange {
  return { key, oldValue, newValue };
}

/** Entries in key order, like the ordered maps of the Rust port. */
function sortedEntries<V>(map: ReadonlyMap<string, V>): [string, V][] {
  return [...map.entries()].sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
}

// The original also tracks an element deletion depth per side here, but
// never reads it, so it is omitted.
class NoninsertionTransformer {
  readonly sides: readonly [NoninsertionSide, NoninsertionSide] = [
    new NoninsertionSide(),
    new NoninsertionSide(),
  ];
  /** Client cursor is FIRST, server cursor is SECOND. */
  readonly position = new PositionTracker();

  // Annotation tracking.

  private commenceDeletion(side: Side): void {
    const other = otherSide(side);
    const changes: AnnotationChange[] = [];
    const mine = this.sides[side].tracker;
    const theirs = this.sides[other].tracker;
    for (const [key, update] of sortedEntries(theirs.propagating)) {
      const forCombining = mine.active.get(key) ?? null;
      const otherActive = theirs.active.get(key);
      if (update !== null) {
        const oldValue =
          forCombining !== null
            ? forCombining.oldValue
            : otherActive !== undefined
              ? otherActive.newValue
              : update.oldValue;
        changes.push(change(key, oldValue, update.newValue));
      } else if (otherActive !== undefined) {
        const newValue = forCombining !== null ? forCombining.newValue : otherActive.oldValue;
        changes.push(change(key, otherActive.newValue, newValue));
      }
      mine.temporary.set(key, forCombining);
    }
    this.commit(side, boundary([], changes));
  }

  private concludeDeletion(side: Side): void {
    const endKeys: string[] = [];
    const changes: AnnotationChange[] = [];
    const tracker = this.sides[side].tracker;
    const temporary = tracker.temporary;
    tracker.temporary = new Map();
    for (const [key, update] of sortedEntries(temporary)) {
      if (update !== null) {
        changes.push(change(key, update.oldValue, update.newValue));
      } else {
        endKeys.push(key);
      }
    }
    this.sync(side);
    this.commit(side, boundary(endKeys, changes));
  }

  /** Records an annotation boundary of `side`'s original operation. */
  private register(side: Side, map: AnnotationBoundaryMap): void {
    const tracked = this.sides[side].tracker.tracked;
    for (const key of map.endKeys) {
      tracked.delete(key);
    }
    for (const c of map.changes) {
      tracked.set(c.key, { oldValue: c.oldValue, newValue: c.newValue });
    }
    if (side === CLIENT) {
      this.processClient(map);
    } else {
      this.processServer(map);
    }
  }

  /** Writes a boundary to `side`'s output, updating its tracker. */
  private commit(side: Side, map: AnnotationBoundaryMap): void {
    const { output, tracker } = this.sides[side];
    for (const key of map.endKeys) {
      if (!tracker.propagating.has(key)) {
        tracker.propagating.set(key, tracker.active.get(key) ?? null);
      }
      tracker.active.delete(key);
    }
    for (const c of map.changes) {
      const oldActive = tracker.active.get(c.key);
      if (
        oldActive === undefined ||
        oldActive.oldValue !== c.oldValue ||
        oldActive.newValue !== c.newValue
      ) {
        if (!tracker.propagating.has(c.key)) {
          tracker.propagating.set(c.key, oldActive ?? null);
        }
        tracker.active.set(c.key, { oldValue: c.oldValue, newValue: c.newValue });
      }
    }
    output.annotationBoundary(map);
  }

  private sync(side: Side): void {
    this.sides[side].tracker.propagating.clear();
  }

  private syncAnnotations(): void {
    this.sync(CLIENT);
    this.sync(SERVER);
  }

  /**
   * A client annotation change wins over a concurrent server change of the
   * same key.
   */
  private processClient(map: AnnotationBoundaryMap): void {
    const clientEndKeys: string[] = [];
    const clientChanges: AnnotationChange[] = [];
    const serverEndKeys: string[] = [];
    const serverChanges: AnnotationChange[] = [];
    const serverTracked = this.sides[SERVER].tracker.tracked;
    for (const key of map.endKeys) {
      clientEndKeys.push(key);
      const serverValues = serverTracked.get(key);
      if (serverValues !== undefined) {
        serverChanges.push(change(key, serverValues.oldValue, serverValues.newValue));
      }
    }
    for (const c of map.changes) {
      const serverValues = serverTracked.get(c.key);
      if (serverValues !== undefined) {
        clientChanges.push(change(c.key, serverValues.newValue, c.newValue));
        serverEndKeys.push(c.key);
      } else {
        clientChanges.push(c);
      }
    }
    this.commit(CLIENT, boundary(clientEndKeys, clientChanges));
    this.commit(SERVER, boundary(serverEndKeys, serverChanges));
  }

  private processServer(map: AnnotationBoundaryMap): void {
    const serverEndKeys: string[] = [];
    const serverChanges: AnnotationChange[] = [];
    const clientChanges: AnnotationChange[] = [];
    const clientTracked = this.sides[CLIENT].tracker.tracked;
    for (const key of map.endKeys) {
      const clientValues = clientTracked.get(key);
      if (clientValues !== undefined) {
        clientChanges.push(change(key, clientValues.oldValue, clientValues.newValue));
      } else {
        serverEndKeys.push(key);
      }
    }
    for (const c of map.changes) {
      const clientValues = clientTracked.get(c.key);
      if (clientValues !== undefined) {
        clientChanges.push(change(c.key, c.newValue, clientValues.newValue));
      } else {
        serverChanges.push(c);
      }
    }
    this.commit(SERVER, boundary(serverEndKeys, serverChanges));
    this.commit(CLIENT, boundary([], clientChanges));
  }

  // Deletions surrounded by the annotation adjustments they need.

  private doDelete(side: Side, component: DocOpComponent): void {
    this.commenceDeletion(side);
    this.sides[side].output.push(component);
    this.concludeDeletion(side);
  }

  private doDeleteCharacters(side: Side, characters: string): void {
    this.doDelete(side, { type: "deleteCharacters", characters });
  }

  private doDeleteElementStart(side: Side, tag: string, attributes: Attributes): void {
    this.doDelete(side, { type: "deleteElementStart", tag, attributes });
  }

  private doDeleteElementEnd(side: Side): void {
    this.doDelete(side, { type: "deleteElementEnd" });
  }

  /**
   * Resolves `size` items of `component`, which belongs to side `p`, against
   * the cached component of the other side `w`.
   */
  private resolve(p: Side, c: DocOpComponent, size: number, cache: RangeCache): void {
    const w = otherSide(p);
    const outP = this.sides[p].output;
    const outW = this.sides[w].output;
    switch (cache.type) {
      case "retain":
        switch (c.type) {
          case "retain":
            this.syncAnnotations();
            outW.retain(size);
            outP.retain(size);
            return;
          case "deleteCharacters":
            this.doDeleteCharacters(p, splitAtUtf16(c.characters, size)[0]);
            return;
          case "deleteElementStart":
            this.doDeleteElementStart(p, c.tag, c.attributes);
            return;
          case "deleteElementEnd":
            this.doDeleteElementEnd(p);
            return;
          case "replaceAttributes":
          case "updateAttributes":
            this.syncAnnotations();
            outW.retain(1);
            outP.push(c);
            return;
          default:
            throw unexpected(c, "insertion-free");
        }
      case "deleteCharacters":
        switch (c.type) {
          case "retain": {
            const [head, tail] = splitAtUtf16(cache.characters, size);
            this.doDeleteCharacters(w, head);
            cache.characters = tail;
            return;
          }
          case "deleteCharacters":
            // Both sides delete the same characters; neither needs to any
            // more.
            cache.characters = splitAtUtf16(cache.characters, size)[1];
            return;
          default:
            throw TransformError.incompatible();
        }
      case "deleteElementStart":
        switch (c.type) {
          case "retain":
            this.doDeleteElementStart(w, cache.tag, cache.attributes);
            return;
          case "deleteElementStart":
            return;
          case "replaceAttributes":
            this.doDeleteElementStart(w, cache.tag, c.newAttributes);
            return;
          case "updateAttributes":
            this.doDeleteElementStart(w, cache.tag, cache.attributes.updateWith(c.update));
            return;
          default:
            throw TransformError.incompatible();
        }
      case "deleteElementEnd":
        switch (c.type) {
          case "retain":
            this.doDeleteElementEnd(w);
            return;
          case "deleteElementEnd":
            return;
          default:
            throw TransformError.incompatible();
        }
      case "replaceAttributes":
        switch (c.type) {
          case "retain":
            this.syncAnnotations();
            outW.replaceAttributes(cache.oldAttributes, cache.newAttributes);
            outP.retain(1);
            return;
          case "deleteElementStart":
            this.doDeleteElementStart(p, c.tag, cache.newAttributes);
            return;
          case "replaceAttributes":
            this.syncAnnotations();
            outW.replaceAttributes(c.newAttributes, cache.newAttributes);
            outP.retain(1);
            return;
          case "updateAttributes": {
            this.syncAnnotations();
            const updated = cache.oldAttributes.updateWith(c.update);
            outW.replaceAttributes(updated, cache.newAttributes);
            outP.retain(1);
            return;
          }
          default:
            throw TransformError.incompatible();
        }
      case "updateAttributes": {
        const cached = cache.update;
        switch (c.type) {
          case "retain":
            this.syncAnnotations();
            outW.updateAttributes(cached);
            outP.retain(1);
            return;
          case "deleteElementStart":
            this.doDeleteElementStart(p, c.tag, c.attributes.updateWith(cached));
            return;
          case "replaceAttributes": {
            this.syncAnnotations();
            outW.retain(1);
            const updated = c.oldAttributes.updateWith(cached);
            outP.replaceAttributes(updated, c.newAttributes);
            return;
          }
          case "updateAttributes": {
            this.syncAnnotations();
            // The cached update wins on shared keys: its old values become
            // the other update's new values, and the other update loses
            // those keys.
            const updated = new Map<string, string | null>();
            for (const u of c.update.updates) {
              updated.set(u.name, u.newValue);
            }
            let newUpdate = AttributesUpdate.EMPTY;
            for (const u of cached.updates) {
              const value = updated.get(u.name);
              const newOldValue = value !== undefined ? value : u.oldValue;
              newUpdate = newUpdate.composeWith(
                AttributesUpdate.single(u.name, newOldValue, u.newValue),
              );
            }
            outW.updateAttributes(newUpdate);
            const keySet = new Set(cached.updates.map((u) => u.name));
            outP.updateAttributes(c.update.exclude((key) => keySet.has(key)));
            return;
          }
          default:
            throw TransformError.incompatible();
        }
      }
    }
  }

  /**
   * Advances `side`'s cursor by `size` and resolves as much of `component`
   * as the other cursor has already passed. Returns the resolved portion, or
   * -1 if the entire range was resolved.
   */
  private resolveRange(side: Side, size: number, component: DocOpComponent): number {
    const other = otherSide(side);
    const oldPosition = this.position.get(side);
    this.position.increase(side, size);
    let resolved: number | null;
    let result: number;
    if (this.position.get(side) > 0) {
      resolved = oldPosition < 0 ? -oldPosition : null;
      result = 0 - oldPosition;
    } else {
      resolved = size;
      result = ENTIRELY_RESOLVED;
    }
    if (resolved !== null) {
      this.resolve(side, component, resolved, this.sides[other].rangeCache);
    }
    return result;
  }

  step(side: Side, c: DocOpComponent): void {
    let cache: RangeCache;
    switch (c.type) {
      case "annotationBoundary":
        this.register(side, c.boundary);
        return;
      case "retain":
        this.resolveRange(side, c.itemCount, c);
        this.sides[side].rangeCache = RETAIN_CACHE;
        return;
      case "deleteCharacters": {
        const resolutionSize = this.resolveRange(side, c.characters.length, c);
        if (resolutionSize >= 0) {
          this.sides[side].rangeCache = {
            type: "deleteCharacters",
            characters: splitAtUtf16(c.characters, resolutionSize)[1],
          };
        }
        return;
      }
      case "deleteElementStart":
        cache = { type: "deleteElementStart", tag: c.tag, attributes: c.attributes };
        break;
      case "deleteElementEnd":
        cache = { type: "deleteElementEnd" };
        break;
      case "replaceAttributes":
        cache = {
          type: "replaceAttributes",
          oldAttributes: c.oldAttributes,
          newAttributes: c.newAttributes,
        };
        break;
      case "updateAttributes":
        cache = { type: "updateAttributes", update: c.update };
        break;
      default:
        throw unexpected(c, "insertion-free");
    }
    if (this.resolveRange(side, 1, c) === 0) {
      this.sides[side].rangeCache = cache;
    }
  }
}

/** Transforms a pair of insertion-free operations. */
function transformNoninsertions(clientOp: DocOp, serverOp: DocOp): [DocOp, DocOp] {
  const transformer = new NoninsertionTransformer();
  interleave(
    [clientOp, serverOp],
    ["client", "server"],
    () => transformer.position.get(CLIENT),
    (side, c) => transformer.step(side, c),
  );
  const [client, server] = transformer.sides;
  return [DocOp.create(client.output.finish()), DocOp.create(server.output.finish())];
}
