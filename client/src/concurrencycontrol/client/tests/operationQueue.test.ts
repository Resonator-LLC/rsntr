// Port of `org.waveprotocol.wave.concurrencycontrol.client.OperationQueueTest`
// of Apache Wave (Apache License, Version 2.0).

import { beforeEach, describe, expect, it } from "vitest";

import { DocOpBuilder } from "../../../model/docop";
import { ParticipantId } from "../../../model/id";
import { WaveletOperation, WaveletOperationContext } from "../../../model/operation";
import { NoSuchElementError } from "../../common";
import { MergingSequence, OperationQueue } from "..";
import { DeltaTestUtil, expectOpsEqual } from "./helpers";
import { MockDeltaTransformer } from "./mockDeltaTransformer";

function makeContext(participant: ParticipantId, timestamp: number): WaveletOperationContext {
  return new WaveletOperationContext(participant, timestamp, 1);
}

const BOB = ParticipantId.newUnchecked("bob@example.com");
const BOB_A = WaveletOperation.noOp(makeContext(BOB, 1000));
const BOB_B = WaveletOperation.noOp(makeContext(BOB, 2000));
const BOB_C = WaveletOperation.noOp(makeContext(BOB, 3000));

const JIM = ParticipantId.newUnchecked("jim@example.com");
const JIM_A = WaveletOperation.noOp(makeContext(JIM, 1000));
const JIM_B = WaveletOperation.noOp(makeContext(JIM, 2000));
const JIM_C = WaveletOperation.noOp(makeContext(JIM, 3000));

const TOM = ParticipantId.newUnchecked("tom@example.com");
const TOM_B = WaveletOperation.noOp(makeContext(TOM, 2000));

describe("OperationQueueTest", () => {
  let queue: OperationQueue;
  let transformer: MockDeltaTransformer;

  beforeEach(() => {
    transformer = new MockDeltaTransformer();
    queue = new OperationQueue(transformer);
  });

  /** Asserts that the queue is empty and size estimate is zero. */
  function assertQueueIsEmpty(): void {
    expect(queue.isEmpty(), "Expected empty queue").toBe(true);
    expect(queue.estimateSize(), "Expected queue size zero").toBe(0);
  }

  /** Asserts that the queue is not empty and the size estimate is as expected. */
  function assertQueueSize(size: number): void {
    expect(queue.isEmpty(), "Expected non-empty queue").toBe(false);
    expect(queue.estimateSize()).toBe(size);
  }

  /**
   * Asserts that the queue is not empty and the size estimate is within
   * expected bounds (inclusive).
   */
  function assertQueueSizeBetween(minSize: number, maxSize: number): void {
    expect(queue.isEmpty(), "Expected non-empty queue").toBe(false);
    const estimate = queue.estimateSize();
    expect(estimate).toBeGreaterThanOrEqual(minSize);
    expect(estimate).toBeLessThanOrEqual(maxSize);
  }

  /**
   * Transforms the mock's input server delta against the queue and checks
   * that the mock's output server delta comes back.
   */
  function transformAndCheck(): void {
    const expected = transformer.getOutputServerDelta();
    expectOpsEqual(queue.transform(transformer.getInputServerDelta()), expected);
  }

  /** Tests that newly created queues are empty. */
  it("testQueueStartsEmpty", () => {
    assertQueueIsEmpty();
  });

  /**
   * Tests that adding operations to an empty queue results in it being
   * non-empty until those operations are taken out again.
   */
  it("testQueueNonemptyAfterOperationsAddedUntilTaken", () => {
    queue.add(BOB_A);
    assertQueueSize(1);
    queue.take();
    assertQueueIsEmpty();
  });

  /**
   * Tests that inserting a new head delta into an empty queue makes the queue
   * non-empty if and only if the delta has ops in it.
   */
  it("testQueueNonemptyAfterInsertingNonemptyHead", () => {
    queue.insertHead(new MergingSequence());
    assertQueueIsEmpty();
    const head = new MergingSequence();
    head.add(BOB_A);
    queue.insertHead(head);
    assertQueueSize(1);
  });

  /**
   * Tests that attempting to take a delta from an empty queue results in a
   * {@link NoSuchElementException} being thrown.
   */
  it("testThrowsExceptionTakingFromEmptyQueue", () => {
    expect(() => queue.take()).toThrow(NoSuchElementError);
  });

  /**
   * Tests that adding multiple consecutive operations from a single author
   * results in them being merged into the one delta.
   */
  it("testMergesConsecutiveOperationsWithSameCreator", () => {
    queue.add(BOB_A);
    queue.add(BOB_B);
    assertQueueSizeBetween(1, 2);
    expectOpsEqual(queue.take(), [BOB_A, BOB_B]);
    // Queue should now be empty, even though we did two adds and only one take
    assertQueueIsEmpty();
  });

  /** Tests that operations by different authors are not merged. */
  it("testDoesNotMergeOperationsDividedByOtherAuthor", () => {
    queue.add(BOB_A);
    queue.add(JIM_B);
    assertQueueSizeBetween(1, 2);
    expectOpsEqual(queue.take(), [BOB_A]);
    expectOpsEqual(queue.take(), [JIM_B]);
  });

  /**
   * Tests that two operations from a single author that are added one after
   * another are not merged if the calls to add() are separated by a take()
   * which extracts the first operation.
   */
  it("testOperationsNotMergedOverQueueEmptyPoints", () => {
    queue.add(BOB_A);
    expectOpsEqual(queue.take(), [BOB_A]);
    assertQueueIsEmpty();
    queue.add(BOB_B);
    expectOpsEqual(queue.take(), [BOB_B]);
  });

  /**
   * Tests that two operations from a single author that are added one after
   * another are still merge if the calls to add() are separated by a take()
   * which extracts previously added operations rather than either of the two
   * aforementioned operations.
   */
  it("testOperationsMergeDespiteTakeIfQueueNotMadeEmpty", () => {
    queue.add(JIM_A);
    queue.add(BOB_B);
    expectOpsEqual(queue.take(), [JIM_A]);
    assertQueueSize(1);
    assertQueueSizeBetween(1, 2);
    queue.add(BOB_C);
    expectOpsEqual(queue.take(), [BOB_B, BOB_C]);
    assertQueueIsEmpty();
  });

  /**
   * Tests that a new head delta can be inserted in at the start of an empty
   * queue. Future adds of ops by the same creator do not merge with it.
   */
  it("testHeadInsertedIntoEmptyQueueAllowsFutureMerging", () => {
    const head = new MergingSequence();
    head.add(BOB_A);
    head.add(BOB_B);
    queue.insertHead(head);
    queue.add(BOB_C);
    assertQueueSizeBetween(1, 3);
    // Delta pushed to head is not mergable to a single delta.
    expectOpsEqual(queue.take(), [BOB_A, BOB_B]);
    expectOpsEqual(queue.take(), [BOB_C]);
    expect(queue.isEmpty()).toBe(true);
  });

  /**
   * Tests that a new head delta can be inserted in a non-empty queue.
   * It does not merge with the existing head.
   */
  it("testInsertHeadMergesIfSameCreatorAsExistingHead", () => {
    queue.add(BOB_C);
    const head = new MergingSequence();
    head.add(BOB_A);
    head.add(BOB_B);
    queue.insertHead(head);
    assertQueueSizeBetween(1, 3);

    // Delta pushed to head is not mergable to a single delta.
    expectOpsEqual(queue.take(), [BOB_A, BOB_B]);
    expectOpsEqual(queue.take(), [BOB_C]);
    expect(queue.isEmpty()).toBe(true);
  });

  /**
   * Tests that a new head delta which is being inserted does not merge with the
   * existing head if the creators do not match, hence the old head is moved to
   * be the second delta but otherwise remains unmodified.
   */
  it("testInsertHeadMakesNewDeltaIfCreatorDiffersFromExistingHead", () => {
    queue.add(BOB_C);
    const head = new MergingSequence();
    head.add(JIM_A);
    head.add(JIM_B);
    queue.insertHead(head);
    expectOpsEqual(queue.take(), [JIM_A, JIM_B]);
    expectOpsEqual(queue.take(), [BOB_C]);
    expect(queue.isEmpty()).toBe(true);
  });

  /**
   * Tests that the deltas provided by take() are optimised. Note that this test
   * makes assumptions about what type of deltas are used by
   * {@link OperationQueue} and what operations those deltas merge.
   */
  it("testProducesOptimisedDeltas", () => {
    queue.add(
      WaveletOperation.blipContent(
        BOB_A.context,
        "a",
        new DocOpBuilder().retain(1).characters("hi").retain(1).build(),
      ),
    );
    queue.add(
      WaveletOperation.blipContent(
        BOB_B.context,
        "a",
        new DocOpBuilder().retain(1).characters("hi").retain(3).build(),
      ),
    );
    assertQueueSizeBetween(1, 2);
    expect(queue.take().length).toBe(1);
    assertQueueIsEmpty();
  });

  /**
   * Tests that deltas which are transformed make their way back into the queue,
   * replacing the untransformed versions and being sent out of take().
   */
  it("testTransformedDeltasReplaceOriginals", () => {
    queue.add(BOB_A);
    queue.add(BOB_B);
    queue.add(JIM_C);

    transformer.expect(BOB_A, BOB_B).transformTo(BOB_A);
    transformer.expect(JIM_C).echo();

    transformAndCheck();
    transformer.checkDone();
    assertQueueSizeBetween(1, 2);
    expectOpsEqual(queue.take(), [BOB_A]);
    expectOpsEqual(queue.take(), [JIM_C]);
    assertQueueIsEmpty();
  });

  /**
   * Tests that deltas which become empty due to a transform are discarded from
   * the queue.
   */
  it("testDeltasEmptyAfterTransformAreDiscarded", () => {
    queue.add(BOB_A);
    queue.add(JIM_A);
    queue.add(BOB_B);
    queue.add(TOM_B);
    queue.add(BOB_C);

    transformer.expect(BOB_A).kill();
    transformer.expect(JIM_A).echo();
    transformer.expect(BOB_B).kill();
    transformer.expect(TOM_B).echo();
    transformer.expect(BOB_C).kill();

    transformAndCheck();
    transformer.checkDone();
    assertQueueSizeBetween(1, 2);
    expectOpsEqual(queue.take(), [JIM_A]);
    expectOpsEqual(queue.take(), [TOM_B]);
    assertQueueIsEmpty();
  });

  /**
   * Tests that deltas with the same author are merged if they end up being
   * consecutive after deltas which were previously dividing them are discarded
   * in a transform.
   */
  it("testAdjacentDeltasBySameAuthorAfterTransformDiscardsAreMerged", () => {
    queue.add(BOB_A);
    queue.add(JIM_A);
    queue.add(BOB_B);
    queue.add(TOM_B);
    queue.add(BOB_C);

    transformer.expect(BOB_A).echo();
    transformer.expect(JIM_A).kill();
    transformer.expect(BOB_B).echo();
    transformer.expect(TOM_B).kill();
    transformer.expect(BOB_C).echo();

    transformAndCheck();
    transformer.checkDone();
    assertQueueSizeBetween(1, 3);
    expectOpsEqual(queue.take(), [BOB_A, BOB_B, BOB_C]);
    assertQueueIsEmpty();
  });

  /**
   * Test we still compose operations after we've transformed.
   */
  it("testComposistionAfterTransform", () => {
    const util = new DeltaTestUtil(BOB);

    // 2 ops merged into one.
    queue.add(util.noOpDocOp("blipA"));
    queue.add(util.noOpDocOp("blipA"));

    // Get a server op
    transformer.expect(util.noOpDocOp("blipA")).echo();
    transformAndCheck();

    // Adding one more operation after that should not merge into previously
    // transformed client op
    queue.add(util.noOpDocOp("blipA"));

    // This operation should merge into the previous op as the previous op is not
    // yet transformed
    queue.add(util.noOpDocOp("blipA"));

    // Should get 1 op, the result of merging and composing the two deltas.
    assertQueueSizeBetween(1, 3);
    expectOpsEqual(queue.take(), [util.noOpDocOp("blipA")]);

    assertQueueIsEmpty();
  });

  it("testCompositionBeforeTransform", () => {
    const util = new DeltaTestUtil(BOB);

    // 2 ops merged into one.
    queue.add(util.noOpDocOp("blipA"));
    queue.add(util.noOpDocOp("blipA"));

    // Get a server op
    transformer.expect(util.noOpDocOp("blipA")).echo();
    transformAndCheck();

    // Should get only 1 op
    assertQueueSize(1);
    expectOpsEqual(queue.take(), [util.noOpDocOp("blipA")]);

    assertQueueIsEmpty();
  });

  /**
   * Test we compose operations after we've transformed. However, we should get several
   * transformed client operations out in the same delta if they don't compose.
   */
  it("testGettingSeveralOpsInOneDelta", () => {
    const util = new DeltaTestUtil(BOB);

    // Do 1 client op
    queue.add(util.noOpDocOp("blipA"));

    // Get a server op
    transformer.expect(util.noOpDocOp("blipA")).echo();
    transformAndCheck();

    // Do another client op
    queue.add(util.noOpDocOp("blipB"));

    // Get a server op
    transformer.expect(util.noOpDocOp("blipA"), util.noOpDocOp("blipB")).echo();
    transformAndCheck();

    // Should get 2 ops in the same delta
    assertQueueSizeBetween(1, 2);
    expectOpsEqual(queue.take(), [util.noOpDocOp("blipA"), util.noOpDocOp("blipB")]);

    assertQueueIsEmpty();
  });

  /**
   * Since transform and compose doesn't commute, test we don't compose operations after
   * we've sent the operation. So when we push to head, we shouldn't merge sent deltas.
   */
  it("testNoCompositionWithInsertHead", () => {
    const util = new DeltaTestUtil(BOB);

    // Do 1 client op
    queue.add(util.noOpDocOp("blipA"));
    // Pushing an op to the head should never merge
    queue.insertHead([util.noOpDocOp("blipA")]);

    // Get a server op
    transformer.expect(util.noOpDocOp("blipA")).echo();
    transformer.expect(util.noOpDocOp("blipA")).echo();
    transformAndCheck();


    // Should be 2 deltas a the first one is not mergable
    assertQueueSizeBetween(1, 2);
    expectOpsEqual(queue.take(), [util.noOpDocOp("blipA")]);
    expectOpsEqual(queue.take(), [util.noOpDocOp("blipA")]);

    assertQueueIsEmpty();
  });

  /**
   * Test operations are actually transformed.
   */
  it("testOpsAreTransformed", () => {
    queue = new OperationQueue();
    const bob = new DeltaTestUtil(BOB);
    const jim = new DeltaTestUtil(JIM);

    // Do bob client op 1
    queue.add(bob.insert(1, "a", 1, null));

    // Get delta 1 from jim
    queue.transform([jim.insert(1, "j", 1, null)]);

    // Do bob client op 2
    queue.add(bob.insert(1, "b", 3, null));

    // Get delta 2 from jim
    queue.transform([jim.insert(1, "i", 2, null)]);

    // check ops are transformed
    expectOpsEqual(queue.take(), [bob.insert(1, "ba", 3, null)]);

    assertQueueIsEmpty();
  });

  /**
   * Tests that the queue's size estimate is at least one per un-mergable delta.
   * Since it's an underestimate it must be exactly correct if each delta
   * contains one op.
   */
  it("testQueueSizeEstimateIsAtLeastDeltaSize", () => {
    queue.add(BOB_A);
    queue.add(JIM_A);
    queue.add(BOB_B);
    queue.add(JIM_B);
    queue.add(BOB_C);
    queue.add(JIM_C);
    expect(queue.estimateSize()).toBe(6);
  });
});
