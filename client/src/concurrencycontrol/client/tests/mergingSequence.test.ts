// Port of `org.waveprotocol.wave.concurrencycontrol.client.MergingSequenceTest`
// of Apache Wave (Apache License, Version 2.0): simple test case for the
// merging logic.

import { beforeEach, describe, expect, it } from "vitest";

import { ParticipantId } from "../../../model/id";
import {
  BlipOperation,
  WaveletOperation,
  WaveletOperationContext,
} from "../../../model/operation";
import { MergingSequence } from "..";
import { TestDoc } from "./helpers";

describe("MergingSequenceTest", () => {
  let docs: Map<string, TestDoc>;

  beforeEach(() => {
    docs = new Map();
  });

  function getContext(timestamp: number): WaveletOperationContext {
    return new WaveletOperationContext(ParticipantId.newUnchecked("blah"), timestamp, 1);
  }

  function makeBlipContentOp(id: string, timestamp: number): WaveletOperation {
    let doc = docs.get(id);
    if (doc === undefined) {
      doc = TestDoc.parse("<x></x>");
      docs.set(id, doc);
    }
    const op = doc.insertCharacters(1, "hi");
    return WaveletOperation.blipContent(getContext(timestamp), id, op);
  }

  function makeOtherBlipOp(id: string, timestamp: number): WaveletOperation {
    return WaveletOperation.blip(getContext(timestamp), id, BlipOperation.submit());
  }

  function assertTimestamp(timestamp: number, delta: MergingSequence): void {
    const op = delta.get(0);
    expect(op.kind.type).toBe("blip");
    expect(op.context.timestamp).toBe(timestamp);
  }

  function addParticipant(jim: ParticipantId): WaveletOperation {
    return WaveletOperation.addParticipant(new WaveletOperationContext(jim, 5, 1), jim);
  }

  it("testOptimiseEmptyDoesNothing", () => {
    const delta = new MergingSequence();
    delta.optimise();
    expect(delta.size()).toBe(0);
  });

  it("testOptimisesDocumentOpsOnSameBlipWithLastTimestamp", () => {
    const delta = new MergingSequence();
    delta.addAll([makeBlipContentOp("a", 1), makeBlipContentOp("a", 2), makeBlipContentOp("a", 3)]);
    delta.optimise();
    expect(delta.size()).toBe(1);
    assertTimestamp(3, delta);
    // Not in the original: the composed operation is the whole insertion.
    const composed = TestDoc.parse("<x></x>");
    composed.consume(delta.get(0).contentOp!);
    expect(composed.asOperation().equals(TestDoc.parse("<x>hihihi</x>").asOperation())).toBe(true);
  });

  it("testOptimisesOpsOnSeparateBlips1", () => {
    const delta = new MergingSequence();
    delta.addAll([makeBlipContentOp("a", 1), makeBlipContentOp("a", 2), makeBlipContentOp("b", 3)]);
    delta.optimise();
    expect(delta.size()).toBe(2);
  });

  it("testOptimisesOpsOnSeparateBlips2", () => {
    const delta = new MergingSequence();
    delta.addAll([
      makeBlipContentOp("a", 1),
      makeBlipContentOp("a", 2),
      makeBlipContentOp("b", 3),
      makeBlipContentOp("b", 4),
      makeBlipContentOp("b", 5),
      makeBlipContentOp("a", 6),
      makeBlipContentOp("a", 7),
    ]);
    delta.optimise();
    expect(delta.size()).toBe(3);
  });

  it("testOptimisesDeltasWithNonDocOps", () => {
    const delta = new MergingSequence();
    delta.addAll([makeBlipContentOp("a", 2), makeBlipContentOp("a", 1), makeOtherBlipOp("a", 3)]);
    delta.optimise();
    expect(delta.size()).toBe(2);
  });

  it("testOptimisesDeltasWithNonBlipOps1", () => {
    const delta = new MergingSequence();
    const jim = ParticipantId.newUnchecked("jim");
    delta.addAll([makeBlipContentOp("a", 1), makeBlipContentOp("a", 2), addParticipant(jim)]);
    delta.optimise();
    expect(delta.size()).toBe(2);
  });

  it("testOptimisesDeltasWithNonBlipOps2", () => {
    const delta = new MergingSequence();
    const jim = ParticipantId.newUnchecked("jim");
    delta.addAll([
      makeBlipContentOp("a", 1),
      makeBlipContentOp("a", 2),
      addParticipant(jim),
      makeBlipContentOp("a", 4),
      makeBlipContentOp("a", 5),
    ]);
    delta.optimise();
    expect(delta.size()).toBe(3);
  });

  it("testOptimisesDeltasWithNonBlipOps3", () => {
    const delta = new MergingSequence();
    const jim = ParticipantId.newUnchecked("jim");
    delta.addAll([
      makeBlipContentOp("a", 1),
      makeBlipContentOp("a", 2),
      addParticipant(jim),
      makeBlipContentOp("b", 4),
      makeBlipContentOp("b", 5),
    ]);
    delta.optimise();
    expect(delta.size()).toBe(3);
  });
});
