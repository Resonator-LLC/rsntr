// Ordered list of operations extractable as single-creator deltas.
// Consecutive operations with matching creators are merged into a single
// delta. Adjacent operations with mismatched creators result in two separate
// deltas. Allows transformation of the enqueued client operations against a
// server delta.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.client.OperationQueue`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { ParticipantId } from "../../model/id";
import { DeltaPair, type WaveletOperation } from "../../model/operation";
import { NoSuchElementError } from "../common";
import { MergingSequence } from "./mergingSequence";

/**
 * Transforms a client delta against a server delta in a way that can be
 * substituted for testing. Throws on transformation failure.
 */
export interface Transformer {
  transform(client: readonly WaveletOperation[], server: readonly WaveletOperation[]): DeltaPair;
}

/**
 * `SENT`: the delta has been sent; its operations must not be concatenated
 * with those of another delta. `OPTIMISED`: the delta has been optimised.
 * `NONE`: a newly created, untouched delta.
 */
type ItemState = "SENT" | "OPTIMISED" | "NONE";

/** What lives in the queue. */
class Item {
  constructor(
    readonly opSequence: MergingSequence,
    readonly state: ItemState,
  ) {}

  toString(): string {
    return `Delta: ${this.opSequence}, item state: ${this.state}`;
  }
}

/** Transforms deltas using `DeltaPair.transform`. */
const TRANSFORMER: Transformer = {
  transform: (client, server) => new DeltaPair(client, server).transform(),
};

/** Number of head deltas to inspect when estimating queue size. */
const ESTIMATE_DELTAS_TO_COUNT = 4;

export class OperationQueue {
  private readonly queue: Item[] = [];
  private tailCreator: ParticipantId | null = null;
  private readonly transformer: Transformer;

  /**
   * An empty queue. Deltas are transformed with `DeltaPair.transform` unless
   * a transformer is given (for testing).
   */
  constructor(transformer: Transformer = TRANSFORMER) {
    this.transformer = transformer;
  }

  /**
   * Adds the given operation to the tail of the operation queue. Merges with
   * the delta at the tail if the creators match, otherwise creates a new
   * tail delta.
   */
  add(op: WaveletOperation): void {
    const creator = op.context.creator;
    const last = this.queue[this.queue.length - 1];
    if (
      last === undefined ||
      this.tailCreator === null ||
      !creator.equals(this.tailCreator) ||
      last.state !== "NONE"
    ) {
      const item = new Item(new MergingSequence(), "NONE");
      this.queue.push(item);
      this.tailCreator = creator;
      item.opSequence.add(op);
    } else {
      last.opSequence.add(op);
    }
  }

  /**
   * Prepends the given delta onto the queue's head. No merging is allowed on
   * this delta: we don't know if the server has actually got the previously
   * sent delta, so it can't be changed once it's sent.
   *
   * @param newHead must only contain operations from a single author. May be
   *        empty, in which case this call does nothing.
   */
  insertHead(newHead: Iterable<WaveletOperation>): void {
    const mergingHead = new MergingSequence(newHead);
    if (mergingHead.isEmpty()) {
      return;
    }
    const item = new Item(mergingHead, "SENT");
    const creator = mergingHead.get(0).context.creator;
    if (this.queue.length === 0) {
      this.queue.push(item);
      this.tailCreator = creator;
    } else {
      this.queue.unshift(item);
    }
  }

  /** Whether there are no pending operations in the queue. */
  isEmpty(): boolean {
    return this.queue.length === 0;
  }

  /**
   * Estimates the number of operations in this queue. To bound the execution
   * time the result is an underestimate of the true number.
   */
  estimateSize(): number {
    const queue = this.queue;
    let estimate = 0;
    // Sum delta size for a fixed number of deltas at the start.
    const counted = Math.min(ESTIMATE_DELTAS_TO_COUNT, queue.length);
    for (let i = 0; i < counted; i++) {
      estimate += (queue[i] as Item).opSequence.size();
    }
    if (queue.length > counted) {
      // Add size of the last delta as new ops are likely to be pushed into it.
      estimate += (queue[queue.length - 1] as Item).opSequence.size();
      // Add one for each other delta in the queue.
      if (queue.length > ESTIMATE_DELTAS_TO_COUNT + 1) {
        estimate += queue.length - (ESTIMATE_DELTAS_TO_COUNT + 1);
      }
    }
    return estimate;
  }

  /**
   * Takes a delta full of operations by the same creator from the head of
   * the queue, removing those operations from the queue. It contains all
   * operations from the head of the queue up until but not including the
   * first change in creator (or the first boundary of a sent delta).
   *
   * Returns a non-empty list of operations with the same creator. Throws
   * {@link NoSuchElementError} if the queue is empty.
   */
  take(): WaveletOperation[] {
    const item = this.takeMergedAndOptimisedItem();

    if (this.isEmpty()) {
      this.tailCreator = null;
    }

    return item.opSequence.toArray();
  }

  /**
   * Transforms the given server delta against the queued operations. Updates
   * the queued deltas with the results of the transformation and returns the
   * transformed server delta.
   *
   * Queued deltas which have all of their operations transformed away and
   * hence become empty are discarded.
   *
   * Throws if the transformation of any operations fails; as in the
   * original, the deltas taken off the queue up to that point are then lost.
   */
  transform(serverOps: readonly WaveletOperation[]): readonly WaveletOperation[] {
    let transformedServerOps = serverOps;
    const newQueue: Item[] = [];

    while (this.queue.length > 0) {
      // Merge in all subsequent consecutive deltas that have the same author
      // and optimise the delta before transforming against the server delta
      // because it makes transformation more efficient.
      const item = this.takeMergedAndOptimisedItem();
      const queuedDelta = item.opSequence;

      const transformedDeltas = this.transformer.transform(
        queuedDelta.toArray(),
        transformedServerOps,
      );

      // Even if a server op is nullified we must still count the nullified
      // op: the transformed server delta has as many ops as the input.
      transformedServerOps = transformedDeltas.server;

      // Discard client deltas which have had all their ops transformed away.
      if (transformedDeltas.client.length > 0) {
        newQueue.push(new Item(new MergingSequence(transformedDeltas.client), item.state));
      }
    }
    this.queue.push(...newQueue);

    return transformedServerOps;
  }

  /**
   * Removes the first item from the queue and subsequent consecutive items
   * that have the same author to produce a single item that contains all the
   * ops in the items removed.
   *
   * Sent items are not merged due to non-commutativity of transformation and
   * composition. If the returned item does not have the SENT state, its
   * delta is always optimised.
   *
   * Throws {@link NoSuchElementError} if the queue is empty.
   */
  private takeMergedAndOptimisedItem(): Item {
    const queue = this.queue;
    const item = queue.shift();
    if (item === undefined) {
      throw new NoSuchElementError("The operation queue is empty");
    }

    // Cannot change delta of sent delta.
    if (item.state === "SENT") {
      return item;
    }

    const resultDelta = item.opSequence;
    const creator = resultDelta.get(0).context.creator;
    let needOptimisation = item.state !== "OPTIMISED";

    for (;;) {
      const nextItem = queue[0];
      if (nextItem === undefined) {
        break;
      }
      const nextDelta = nextItem.opSequence;
      const nextCreator = nextDelta.get(0).context.creator;

      // Don't merge sent ones due to non-commutativity of transformation
      // and composition.
      if (nextItem.state !== "SENT" && creator.equals(nextCreator)) {
        resultDelta.addAll(nextDelta);
        queue.shift();
        needOptimisation = true;
      } else {
        break;
      }
    }

    if (needOptimisation) {
      resultDelta.optimise();
    }

    return new Item(resultDelta, "OPTIMISED");
  }

  toString(): string {
    // Empty space before \n intentional to print in browser.
    return (
      "Operation Queue = " +
      `[deltas: ${this.queue.length}] \n` +
      `[queue: [${this.queue.join(", ")}]] \n` +
      `[tailCreator: ${this.tailCreator}] \n`
    );
  }
}
