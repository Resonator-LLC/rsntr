// Ports of the Apache Wave unit tests for wavelet operations:
// `AddParticipantTest`, `BlipContentOperationTest`, `BlipOperationTest`,
// `BlipOperationVisitorTest`, `OperationEqualityTest`,
// `ParticipantTransformTest`, `VersionUpdateOpTest`, `WaveletOperationTest`
// and `WorthyChangeCheckerTest` (`wave.model.operation.wave`),
// `DeltaPairTest` (`wave.concurrencycontrol.client`),
// `OpComparatorsLargeTest` (`wave.model.operation`) and
// `WaveletDataUtilTest` (`box.server.util`), plus tests of the submitted
// delta transformation taken from `WaveletContainerImpl`.
//
// Blip operations are applied through wavelet operations on a real
// `WaveletData`; the original applies them to blips of test doubles.

import { describe, expect, test } from "vitest";

import { WaveletData } from "../../data";
import {
  AnnotationBoundaryMap,
  Attributes,
  BootstrapDocument,
  DocInitialization,
  DocOp,
  DocOpBuilder,
  toXmlString,
} from "../../docop";
import { RandomProviderImpl, creator, defaultParameters, generateRandomOp } from "../../docop/testing";
import { type ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import { HashedVersion } from "../../version";
import {
  BlipOperation,
  DeltaPair,
  NO_TIMESTAMP,
  SubmitTransformError,
  THREAD_INLINE_ANCHOR_TAGNAME,
  TransformedWaveletDelta,
  type UpdateContributorMethod,
  WaveTransformError,
  WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
  WaveletOperationError,
  buildWaveletFromDeltas,
  equalDocuments,
  isWorthy,
  syntacticIdentity,
  transform,
  transformOpLists,
  transformOps,
  transformSubmittedDelta,
} from "..";
import {
  checkInsert,
  createContent,
  expectError,
  expectOpsEqual,
  expectParticipants,
  insert,
  participant,
} from "./helpers";

const EMPTY_INIT = DocInitialization.fromOp(DocOp.EMPTY);

function waveletName(): WaveletName {
  return WaveletName.of(WaveId.of("example.com", "w+wave"), WaveletId.of("example.com", "conv+root"));
}

/**
 * The wavelet `WaveletDataFactory.create()` gives: empty, at unsigned
 * version zero and time zero.
 */
function emptyWavelet(): WaveletData {
  return WaveletData.empty(
    waveletName(),
    participant("fake@example.com"),
    HashedVersion.unsigned(0),
    0,
  );
}

function hash(...bytes: number[]): Uint8Array {
  return Uint8Array.from(bytes);
}

describe("AddParticipantTest", () => {
  const CREATOR = participant("abc@example.com");
  const ANOTHER = participant("def@example.com");
  const A_THIRD = participant("xyz@example.com");

  const wavelet = (): WaveletData =>
    new WaveletData(
      WaveletId.of("example.com", "conv+root"),
      CREATOR,
      0,
      0,
      HashedVersion.unsigned(0),
      0,
      WaveId.of("example.com", "c+123"),
    );
  const createContext = (): WaveletOperationContext =>
    new WaveletOperationContext(CREATOR, NO_TIMESTAMP, 1);
  const addAt = (p: ParticipantId, position: number): WaveletOperation =>
    WaveletOperation.addParticipant(createContext(), p, position);

  test("add many participants", () => {
    const w = wavelet();
    const participants: ParticipantId[] = [];
    expectParticipants(w.participants, participants);

    WaveletOperation.addParticipant(createContext(), CREATOR).apply(w);
    participants.push(CREATOR);
    expectParticipants(w.participants, participants);

    for (let i = 0; i < 10; i++) {
      const p = participant(`abc${i}@example.com`);
      participants.push(p);
      WaveletOperation.addParticipant(createContext(), p).apply(w);
    }
    expectParticipants(w.participants, participants);
  });

  test("cannot add same participant twice", () => {
    const w = wavelet();
    addAt(CREATOR, 0).apply(w);
    expectParticipants(w.participants, [CREATOR]);
    expectError(() => addAt(CREATOR, 0).apply(w), WaveletOperationError, {
      kind: "duplicateParticipant",
      participant: CREATOR.toString(),
    });
    // Adding another is ok.
    addAt(ANOTHER, 0).apply(w);
  });

  test("reverse of add participant is remove participant", () => {
    const w = wavelet();
    expectOpsEqual(addAt(CREATOR, 0).applyAndReturnReverse(w), [
      WaveletOperation.removeParticipant(createContext(), CREATOR),
    ]);
  });

  test("cannot remove non participant", () => {
    const w = wavelet();
    const remove = WaveletOperation.removeParticipant(createContext(), CREATOR);
    const missing = { kind: "missingParticipant", participant: CREATOR.toString() };
    expectError(() => remove.apply(w), WaveletOperationError, missing);

    addAt(CREATOR, 0).apply(w);
    // Now it's ok to remove.
    remove.apply(w);
    expect(w.participants.length).toBe(0);

    expectError(() => remove.apply(w), WaveletOperationError, missing);
  });

  test("reverse of remove participant is add participant with position", () => {
    const w = wavelet();
    const participants = [CREATOR, ANOTHER, A_THIRD];
    for (const p of participants) {
      w.addParticipant(p);
    }
    expectParticipants(w.participants, participants);

    // The reverse of removing any participant is an add at its position
    // which, when applied, restores the participant list.
    participants.forEach((p, i) => {
      const reverse = WaveletOperation.removeParticipant(createContext(), p).applyAndReturnReverse(w);
      expectOpsEqual(reverse, [addAt(p, i)]);
      reverse[0]!.apply(w);
      expectParticipants(w.participants, participants);
    });
  });
});

// Fixture of `OperationTestBase`.
const CREATION_TIMESTAMP = 100;
const LAST_MODIFIED_TIMESTAMP = CREATION_TIMESTAMP + 10;
const CONTEXT_TIMESTAMP = LAST_MODIFIED_TIMESTAMP + 5;
const CONTEXT_VERSION = 4;
const LARS = participant("lars@example.com");
const FRED = participant("fred@example.com");
const JANE = participant("jane@example.com");
const contextHashedVersion = (): HashedVersion => HashedVersion.of(CONTEXT_VERSION, hash(4, 4, 4, 4));
const baseContext = (): WaveletOperationContext =>
  new WaveletOperationContext(FRED, CONTEXT_TIMESTAMP, 1);

describe("BlipContentOperationTest", () => {
  const docOp = (): DocOp => new DocOpBuilder().characters("Hello").build();

  test("apply", () => {
    const wavelet = emptyWavelet();
    wavelet.createDocument("root", JANE, [], EMPTY_INIT, 0, 0);
    WaveletOperation.blipContent(baseContext(), "root", docOp()).apply(wavelet);
    const blip = wavelet.document("root")!;
    // The op reached the document.
    expect(docOp().equals(blip.content.asOperation())).toBe(true);
    // Editing the document makes the op creator a blip contributor.
    expectParticipants(blip.contributors, [FRED]);
  });

  test("reverse restores content", () => {
    const wavelet = emptyWavelet();
    wavelet.createDocument("root", FRED, [], EMPTY_INIT, 0, 0);
    const reverseOps = WaveletOperation.blipContent(
      baseContext(),
      "root",
      docOp(),
    ).applyAndReturnReverse(wavelet);
    expect(wavelet.document("root")!.content.toXmlString()).toBe("Hello");
    for (const reverse of reverseOps) {
      reverse.apply(wavelet);
    }
    const blip = wavelet.document("root")!;
    expect(blip.content.toXmlString()).toBe("");
    expect(blip.contributors.length).toBe(0);
  });
});

describe("BlipOperationTest", () => {
  const BLIP_ID = "blipid";
  const janeContext = (): WaveletOperationContext => new WaveletOperationContext(JANE, 42, 1);

  /** An op that doesn't change the document but still is worthy. */
  function sampleContentOperation(
    method: UpdateContributorMethod,
    context: WaveletOperationContext = baseContext(),
  ): WaveletOperation {
    const op = new DocOpBuilder()
      .retain(1)
      .deleteElementStart("line", Attributes.EMPTY)
      .deleteElementEnd()
      .elementStart("line", Attributes.EMPTY)
      .elementEnd()
      .retain(1)
      .build();
    expect(isWorthy(op)).toBe(true);
    return WaveletOperation.blip(context, BLIP_ID, BlipOperation.content(op, method));
  }

  /** A wavelet with the blip `createBlipData()` makes. */
  function waveletWithBlip(): WaveletData {
    const wavelet = emptyWavelet();
    wavelet.createDocument(BLIP_ID, FRED, [], createContent(""), 0, 0);
    return wavelet;
  }

  const contributors = (wavelet: WaveletData): string[] =>
    wavelet.document(BLIP_ID)!.contributors.map((p) => p.address);

  test("get context", () => {
    const op = WaveletOperation.blip(baseContext(), BLIP_ID, BlipOperation.SUBMIT);
    expect(op.context.equals(baseContext())).toBe(true);
  });

  test("apply updates timestamp", () => {
    const wavelet = waveletWithBlip();
    const op = sampleContentOperation("add");
    const oldWaveletVersion = wavelet.version;
    expect(wavelet.document(BLIP_ID)!.lastModifiedTime).not.toBe(baseContext().timestamp);
    op.apply(wavelet);
    const blip = wavelet.document(BLIP_ID)!;
    // The last modified time is now the context time.
    expect(blip.lastModifiedTime).toBe(baseContext().timestamp);
    // The blip version is the wavelet version before the op plus one.
    expect(blip.lastModifiedVersion).toBe(oldWaveletVersion + 1);
  });

  test("none contributor method leaves contributors", () => {
    const wavelet = waveletWithBlip();
    sampleContentOperation("none").apply(wavelet);
    expect(contributors(wavelet)).toEqual([]);
  });

  test("add contributor method adds new contributor", () => {
    const wavelet = waveletWithBlip();
    sampleContentOperation("add").apply(wavelet);
    expect(contributors(wavelet)).toContain(FRED.address);

    sampleContentOperation("add", janeContext()).apply(wavelet);
    expect(contributors(wavelet)).toContain(FRED.address);
    expect(contributors(wavelet)).toContain(JANE.address);
  });

  test("add contributor method doesn't duplicate contributors", () => {
    const wavelet = waveletWithBlip();
    sampleContentOperation("add").apply(wavelet);
    sampleContentOperation("add").apply(wavelet);
    expect(contributors(wavelet)).toEqual([FRED.address]);
  });

  test("reverse add contributor removes contributor", () => {
    const wavelet = waveletWithBlip();
    // Adds fred.
    const reverseOps = sampleContentOperation("add").applyAndReturnReverse(wavelet);
    expect(contributors(wavelet)).toEqual([FRED.address]);
    for (const reverse of reverseOps) {
      reverse.apply(wavelet);
    }
    expect(contributors(wavelet)).toEqual([]);
  });
});

// The visitor is replaced by switching on `BlipOperation.kind`; these check
// that each variant is told apart.
describe("BlipOperationVisitorTest", () => {
  test("blip content operation visitor", () => {
    const docOp = createContent("Hello");
    const op = BlipOperation.content(docOp);
    expect(op.kind.type).toBe("content");
    expect(op.isContent()).toBe(true);
    expect(op.contentOp).toBe(docOp);
  });

  test("submit blip visitor", () => {
    const op = BlipOperation.submit();
    expect(op.kind.type).toBe("submit");
    expect(op.isContent()).toBe(false);
    expect(op.contentOp).toBeNull();
  });
});

describe("OperationEqualityTest", () => {
  const createWaveletOperations = (): WaveletOperation[] => [
    WaveletOperation.addParticipant(baseContext(), FRED),
    WaveletOperation.addParticipant(baseContext(), JANE),
    WaveletOperation.blipContent(baseContext(), "b", createContent("Hello")),
    WaveletOperation.blipContent(baseContext(), "b", createContent("World")),
    WaveletOperation.noOp(baseContext()),
    WaveletOperation.removeParticipant(baseContext(), FRED),
    WaveletOperation.removeParticipant(baseContext(), JANE),
    WaveletOperation.blip(baseContext(), "b", BlipOperation.SUBMIT),
    WaveletOperation.versionUpdate(FRED, 1, null, "id1", null),
    WaveletOperation.versionUpdate(FRED, 1, null, "id2", null),
  ];

  /** The blip operations of the original list, unwrapped. */
  const createBlipOperations = (): BlipOperation[] => [
    BlipOperation.content(createContent("Hello")),
    BlipOperation.content(createContent("World")),
    BlipOperation.submit(),
  ];

  function checkPairwise<T extends { equals(other: T): boolean }>(
    operations1: readonly T[],
    operations2: readonly T[],
  ): void {
    operations1.forEach((a, i) => {
      operations2.forEach((b, j) => {
        expect(a.equals(b), `failed pair's equals [i:${i}, ${a}] [j:${j}, ${b}]`).toBe(i === j);
      });
    });
  }

  test("equality", () => {
    checkPairwise(createWaveletOperations(), createWaveletOperations());
    checkPairwise(createBlipOperations(), createBlipOperations());
  });

  // Not in the original: equality ignores the context and the contributor
  // method.
  test("equality ignores context and contributor method", () => {
    const other = new WaveletOperationContext(JANE, 7, 3, contextHashedVersion());
    const a = createWaveletOperations();
    const b = a.map((op) => new WaveletOperation(other, op.kind));
    checkPairwise(a, b);
    const op = createContent("Hello");
    expect(BlipOperation.content(op, "add").equals(BlipOperation.content(op, "none"))).toBe(true);
  });
});

describe("ParticipantTransformTest", () => {
  const a = participant("a@google.com");
  const b = participant("b@google.com");
  const context1 = new WaveletOperationContext(participant("p1@google.com"), 1, 1);
  const context2 = new WaveletOperationContext(participant("p2@google.com"), 1, 1);
  const contextA = new WaveletOperationContext(a, 1, 1);
  const remove1a = WaveletOperation.removeParticipant(context1, a);
  const remove2a = WaveletOperation.removeParticipant(context2, a);
  const remove2b = WaveletOperation.removeParticipant(context2, b);
  const add1a = WaveletOperation.addParticipant(context1, a);
  const add2a = WaveletOperation.addParticipant(context2, a);
  const add2b = WaveletOperation.addParticipant(context2, b);
  const noop1 = WaveletOperation.noOp(context1);
  const noop2 = WaveletOperation.noOp(context2);
  const mutation = WaveletOperation.blipContent(
    contextA,
    "dummy",
    new DocOpBuilder().characters("x").build(),
  );

  function checkTransform(
    clientOperation: WaveletOperation,
    serverOperation: WaveletOperation,
    transformedClientOperation: WaveletOperation,
    transformedServerOperation: WaveletOperation,
  ): void {
    const pair = transform(clientOperation, serverOperation);
    expect(pair.clientOp.equals(transformedClientOperation)).toBe(true);
    expect(pair.serverOp.equals(transformedServerOperation)).toBe(true);
    // Not in the original: the contexts stay with their sides.
    expect(pair.clientOp.context.equals(clientOperation.context)).toBe(true);
    expect(pair.serverOp.context.equals(serverOperation.context)).toBe(true);
  }

  function checkIdentityTransform(
    clientOperation: WaveletOperation,
    serverOperation: WaveletOperation,
  ): void {
    checkTransform(clientOperation, serverOperation, clientOperation, serverOperation);
  }

  // A removed participant issuing an operation is a distinct error.
  test("removed author exception", () => {
    expectError(() => transform(mutation, remove2a), WaveTransformError, {
      kind: "removedAuthor",
      author: "a@google.com",
    });
  });

  test("no exception", () => {
    checkIdentityTransform(remove2a, mutation);
    checkIdentityTransform(mutation, remove2b);
    checkIdentityTransform(remove2b, mutation);
  });

  test("addition vs addition", () => {
    checkTransform(add1a, add2a, noop1, noop2);
    checkIdentityTransform(add1a, add2b);
  });

  test("addition vs removal", () => {
    const involvingA = { kind: "participant", participant: "a@google.com" };
    expectError(() => transform(add1a, remove2a), WaveTransformError, involvingA);
    expectError(() => transform(remove2a, add1a), WaveTransformError, involvingA);
    checkIdentityTransform(add1a, remove2b);
    checkIdentityTransform(remove2b, add1a);
  });

  test("removal vs removal", () => {
    checkTransform(remove1a, remove2a, noop1, noop2);
    checkIdentityTransform(remove1a, remove2b);
  });
});

describe("VersionUpdateOpTest", () => {
  const ROOT_BLIP = "root";
  const SIGNATURE = hash(4, 4, 4, 4);
  const SIGNATURE2 = hash(8, 8, 8, 8);
  const creatorId = participant("lars@gwave.com");
  const fred = participant("fred@gwave.com");

  /** A context typical of a locally generated op; doesn't update metadata. */
  const localContext = (): WaveletOperationContext =>
    new WaveletOperationContext(creatorId, NO_TIMESTAMP, 0);

  function assertBlipVersion(wavelet: WaveletData, blipId: string, version: number): void {
    expect(wavelet.document(blipId)!.lastModifiedVersion).toBe(version);
  }

  function assertHashedVersion(wavelet: WaveletData, expected: HashedVersion): void {
    expect(wavelet.hashedVersion.equals(expected), wavelet.hashedVersion.toString()).toBe(true);
  }

  function assertWaveletMetadataIsInitialState(wavelet: WaveletData): void {
    expect(wavelet.version).toBe(0);
    assertHashedVersion(wavelet, HashedVersion.unsigned(0));
    if (wavelet.document(ROOT_BLIP) !== null) {
      assertBlipVersion(wavelet, ROOT_BLIP, 0);
    }
  }

  /** Applies an op that adds fred and returns it. */
  function addParticipant(wavelet: WaveletData): WaveletOperation {
    const op = WaveletOperation.addParticipant(localContext(), fred);
    op.apply(wavelet);
    return op;
  }

  /** Applies an empty document op to a blip and returns it. */
  function touchBlip(wavelet: WaveletData, blipId: string): WaveletOperation {
    const op = WaveletOperation.blipContent(localContext(), blipId, DocOp.EMPTY);
    op.apply(wavelet);
    return op;
  }

  /** Applies a document mutation to a blip and returns it. */
  function mutateBlip(wavelet: WaveletData, blipId: string): WaveletOperation {
    const op = WaveletOperation.blipContent(localContext(), blipId, createContent("Hello"));
    op.apply(wavelet);
    return op;
  }

  test("version update op from wavelet op ack", () => {
    const wavelet = emptyWavelet();
    assertWaveletMetadataIsInitialState(wavelet);

    // Apply an op that has no version information.
    const op = addParticipant(wavelet);
    assertWaveletMetadataIsInitialState(wavelet);

    // Create version update op from ack.
    const updateOp = op.createVersionUpdateOp(5, HashedVersion.of(5, SIGNATURE));
    updateOp.apply(wavelet);
    expect(wavelet.version).toBe(5);
    assertHashedVersion(wavelet, HashedVersion.of(5, SIGNATURE));
  });

  test("version update op from blip no-op ack", () => {
    const wavelet = emptyWavelet();
    assertWaveletMetadataIsInitialState(wavelet);

    const waveletOp = touchBlip(wavelet, ROOT_BLIP);
    assertWaveletMetadataIsInitialState(wavelet);

    const updateOp = waveletOp.createVersionUpdateOp(1, HashedVersion.of(1, SIGNATURE));
    updateOp.apply(wavelet);
    expect(wavelet.version).toBe(1);
    assertHashedVersion(wavelet, HashedVersion.of(1, SIGNATURE));
    assertBlipVersion(wavelet, ROOT_BLIP, 0);
  });

  test("version update op from blip content operation ack", () => {
    const wavelet = emptyWavelet();
    assertWaveletMetadataIsInitialState(wavelet);

    const waveletOp = mutateBlip(wavelet, ROOT_BLIP);
    assertWaveletMetadataIsInitialState(wavelet);

    const updateOp = waveletOp.createVersionUpdateOp(1, HashedVersion.of(1, SIGNATURE));
    updateOp.apply(wavelet);
    expect(wavelet.version).toBe(1);
    assertHashedVersion(wavelet, HashedVersion.of(1, SIGNATURE));
    assertBlipVersion(wavelet, ROOT_BLIP, 1);
  });

  test("version update op from submit ack", () => {
    const wavelet = emptyWavelet();
    assertWaveletMetadataIsInitialState(wavelet);

    const waveletOp = WaveletOperation.blip(localContext(), ROOT_BLIP, BlipOperation.SUBMIT);
    waveletOp.apply(wavelet);
    assertWaveletMetadataIsInitialState(wavelet);

    const updateOp = waveletOp.createVersionUpdateOp(1, HashedVersion.of(1, SIGNATURE));
    updateOp.apply(wavelet);
    expect(wavelet.version).toBe(1);
    assertHashedVersion(wavelet, HashedVersion.of(1, SIGNATURE));
    assertBlipVersion(wavelet, ROOT_BLIP, 0);
  });

  test("multiple version updates", () => {
    const wavelet = emptyWavelet();
    assertWaveletMetadataIsInitialState(wavelet);

    // First add participant.
    const addParticipantOp = addParticipant(wavelet);
    addParticipantOp.createVersionUpdateOp(1, HashedVersion.of(1, SIGNATURE)).apply(wavelet);
    expect(wavelet.version).toBe(1);
    assertHashedVersion(wavelet, HashedVersion.of(1, SIGNATURE));

    // Then mutate blip.
    const mutateBlipOp = mutateBlip(wavelet, ROOT_BLIP);
    mutateBlipOp.createVersionUpdateOp(1, HashedVersion.of(2, SIGNATURE2)).apply(wavelet);
    expect(wavelet.version).toBe(2);
    assertHashedVersion(wavelet, HashedVersion.of(2, SIGNATURE2));
    assertBlipVersion(wavelet, ROOT_BLIP, 2);
  });

  test("reverse", () => {
    const wavelet = emptyWavelet();
    assertWaveletMetadataIsInitialState(wavelet);

    const waveletOp = mutateBlip(wavelet, ROOT_BLIP);
    assertWaveletMetadataIsInitialState(wavelet);

    const updateOp = waveletOp.createVersionUpdateOp(1, HashedVersion.of(1, SIGNATURE));
    const reverse = updateOp.applyAndReturnReverse(wavelet);
    expect(wavelet.version).toBe(1);
    assertHashedVersion(wavelet, HashedVersion.of(1, SIGNATURE));
    assertBlipVersion(wavelet, ROOT_BLIP, 1);

    // Applying the reverse restores the versions.
    expect(reverse.length).toBe(1);
    reverse[0]!.apply(wavelet);
    assertWaveletMetadataIsInitialState(wavelet);
  });
});

describe("WaveletOperationTest", () => {
  test("op updates timestamp", () => {
    const wavelet = emptyWavelet();
    WaveletOperation.noOp(new WaveletOperationContext(LARS, CONTEXT_TIMESTAMP, 0)).apply(wavelet);
    expect(wavelet.lastModifiedTime).toBe(CONTEXT_TIMESTAMP);
  });

  test("op without timestamp doesn't update timestamp", () => {
    const wavelet = emptyWavelet();
    const oldTimestamp = wavelet.lastModifiedTime;
    WaveletOperation.noOp(new WaveletOperationContext(LARS, NO_TIMESTAMP, 0)).apply(wavelet);
    expect(wavelet.lastModifiedTime).toBe(oldTimestamp);
  });

  test("op updates version", () => {
    const wavelet = emptyWavelet();
    const oldVersion = wavelet.version;
    WaveletOperation.noOp(new WaveletOperationContext(LARS, NO_TIMESTAMP, 1)).apply(wavelet);
    expect(wavelet.version).toBe(oldVersion + 1);
  });

  test("op updates signature", () => {
    const wavelet = emptyWavelet();
    const context = WaveletOperationContext.withHashedVersion(
      LARS,
      NO_TIMESTAMP,
      0,
      contextHashedVersion(),
    );
    WaveletOperation.noOp(context).apply(wavelet);
    expect(wavelet.hashedVersion.equals(contextHashedVersion())).toBe(true);
  });

  test("op without signature doesn't update signature", () => {
    const wavelet = emptyWavelet();
    const oldHashedVersion = wavelet.hashedVersion;
    WaveletOperation.noOp(new WaveletOperationContext(LARS, NO_TIMESTAMP, 0)).apply(wavelet);
    expect(wavelet.hashedVersion.equals(oldHashedVersion)).toBe(true);
  });

  // `createReverseContext` is not public; the reverse context is read off
  // the reverse of a no-op.
  test("create reverse context reverses context", () => {
    const wavelet = emptyWavelet();
    const oldTimestamp = wavelet.lastModifiedTime;
    const oldHashedVersion = wavelet.hashedVersion;
    const context = WaveletOperationContext.withHashedVersion(
      LARS,
      CONTEXT_TIMESTAMP,
      1,
      contextHashedVersion(),
    );
    const reverseOps = WaveletOperation.noOp(context).applyAndReturnReverse(wavelet);
    expect(reverseOps.length).toBe(1);
    const reverse = reverseOps[0]!.context;
    expect(reverse.creator.equals(context.creator)).toBe(true);
    expect(reverse.versionIncrement).toBe(-context.versionIncrement);
    expect(reverse.timestamp).toBe(oldTimestamp);
    expect(reverse.hashedVersion!.equals(oldHashedVersion)).toBe(true);
  });

  // Not in the original: the string forms (Rust `Display`).
  test("string forms", () => {
    const v = HashedVersion.of(4, hash(4, 4, 4, 4));
    const plain = new WaveletOperationContext(FRED, 115, 1);
    const hashed = WaveletOperationContext.withHashedVersion(FRED, 115, 1, v);
    expect(plain.toString()).toBe(
      "WaveletOperationContext(creator: fred@example.com, timestamp: 115, " +
        "version increment: 1,hashedVersion: null)",
    );
    expect(hashed.toString()).toBe(
      "WaveletOperationContext(creator: fred@example.com, timestamp: 115, " +
        `version increment: 1,hashedVersion: ${v})`,
    );
    expect(WaveletOperation.noOp(plain).toString()).toBe(
      "no-op by fred@example.com at 115 version null",
    );
    expect(WaveletOperation.addParticipant(hashed, JANE).toString()).toBe(
      `add participant jane@example.com by fred@example.com at 115 version ${v}`,
    );
    expect(WaveletOperation.addParticipant(plain, JANE, 2).toString()).toBe(
      "add participant jane@example.com at position 2 by fred@example.com at 115 version null",
    );
    expect(WaveletOperation.removeParticipant(plain, JANE).toString()).toBe(
      "remove participant jane@example.com by fred@example.com at 115 version null",
    );
    const op = new DocOpBuilder().characters("x").build();
    expect(WaveletOperation.blipContent(plain, "b+1", op).toString()).toBe(
      `document op: ${op} on blip b+1 by fred@example.com at 115 version null isWorthy true`,
    );
    expect(WaveletOperation.blipContent(plain, "attach+1", op).toString()).toBe(
      `document op: ${op} on blip attach+1 by fred@example.com at 115 version null isWorthy false`,
    );
    expect(WaveletOperation.blip(plain, "b+1", BlipOperation.SUBMIT).toString()).toBe(
      "submit on blip b+1 by fred@example.com at 115 version null isWorthy true",
    );
    expect(WaveletOperation.versionUpdate(FRED, 1).toString()).toBe(
      "version update op, blip id null blipVersion -1",
    );
    expect(WaveletOperation.versionUpdate(FRED, 1, null, "b+1", 7).toString()).toBe(
      "version update op, blip id b+1 blipVersion 7",
    );
    const delta = new WaveletDelta(FRED, v, []);
    expect(delta.toString()).toBe(`CoreWaveletDelta(fred@example.com, ${v}, [])`);
    const noOp = WaveletOperation.noOp(plain);
    expect(new WaveletDelta(FRED, v, [noOp, noOp]).toString()).toBe(
      `CoreWaveletDelta(fred@example.com, ${v},  2 ops: [${noOp},${noOp}])`,
    );
    const last = WaveletOperation.noOp(hashed);
    expect(new TransformedWaveletDelta(FRED, v, 115, [noOp, last]).toString()).toBe(
      `TransformedWaveletDelta(fred@example.com, ${v}, 115 -> 2 ops: [${noOp},${last}])`,
    );
    expect(new TransformedWaveletDelta(FRED, v, 115, []).toString()).toBe(
      `TransformedWaveletDelta(fred@example.com, ${v}, 115 -> [])`,
    );
  });

  // Not in the original: cloning and the accessors the client concurrency
  // control relies on.
  test("clone op and accessors", () => {
    const plain = new WaveletOperationContext(FRED, 115, 1);
    const other = new WaveletOperationContext(JANE, 7, 1);
    const docOp = new DocOpBuilder().characters("x").build();
    const content = WaveletOperation.blip(plain, "b+1", BlipOperation.content(docOp, "none"));
    expect(content.blipId).toBe("b+1");
    expect(content.contentOp).toBe(docOp);
    expect(content.isBlipContent()).toBe(true);
    const cloned = content.cloneOp(other);
    expect(cloned.context).toBe(other);
    expect(cloned.equals(content)).toBe(true);
    expect(cloned.blipOp!.kind).toMatchObject({ type: "content", method: "add" });
    expect(
      WaveletOperation.addParticipant(plain, JANE, 3).cloneOp(other).kind,
    ).toMatchObject({ type: "addParticipant", position: null });
    const noOp = WaveletOperation.noOp(plain);
    expect(noOp.blipId).toBeNull();
    expect(noOp.contentOp).toBeNull();
    expect(noOp.isBlipContent()).toBe(false);
    for (const op of [
      WaveletOperation.blip(plain, "b+1", BlipOperation.SUBMIT),
      WaveletOperation.versionUpdate(FRED, 1),
    ]) {
      expectError(() => op.cloneOp(other), WaveletOperationError, { kind: "uncloneable" });
    }
  });

  // Not in the original.
  test("version below zero is rejected and leaves the wavelet unchanged", () => {
    const wavelet = emptyWavelet();
    const before = wavelet.clone();
    const op = WaveletOperation.addParticipant(new WaveletOperationContext(LARS, 5, -1), FRED);
    expectError(() => op.apply(wavelet), WaveletOperationError, {
      kind: "versionOutOfRange",
      version: 0,
      increment: -1,
    });
    expectError(() => op.applyAndReturnReverse(wavelet), WaveletOperationError, {
      kind: "versionOutOfRange",
    });
    expect(wavelet.equals(before)).toBe(true);
  });
});

describe("WorthyChangeCheckerTest", () => {
  // The original derives the deletion from a `Nindo` against the document;
  // here it is written out and checked to apply.
  test("anchor removal is unworthy", () => {
    const initialization = new DocOpBuilder()
      .annotationBoundary(AnnotationBoundaryMap.changesOnly([["a", null, "b"]]))
      .characters("a")
      .annotationBoundary(AnnotationBoundaryMap.ends(["a"]))
      .characters("a")
      .elementStart(THREAD_INLINE_ANCHOR_TAGNAME, Attributes.EMPTY)
      .elementEnd()
      .buildInitialization();
    const d = new BootstrapDocument();
    d.consume(initialization);

    const op = new DocOpBuilder()
      .retain(2)
      .deleteElementStart(THREAD_INLINE_ANCHOR_TAGNAME, Attributes.EMPTY)
      .deleteElementEnd()
      .build();
    d.consume(op);
    expect(isWorthy(op)).toBe(false);
  });
});

describe("DeltaPairTest", () => {
  const client = participant("client@example.com");
  const server = participant("server@example.com");

  function checkVersionUpdate(
    operation: WaveletOperation,
    versionIncrement: number,
    distinctVersion: HashedVersion | null,
  ): void {
    expect(operation.kind.type).toBe("versionUpdate");
    expect(operation.context.versionIncrement).toBe(versionIncrement);
    if (distinctVersion === null) {
      expect(operation.context.hashedVersion).toBeNull();
    } else {
      expect(operation.context.hashedVersion!.equals(distinctVersion)).toBe(true);
    }
  }

  test("multiple client server ops", () => {
    // Client inserts ".A.B".
    const clientOps = [insert(client, 1, "A", 1), insert(client, 3, "B", 0)];
    // Server inserts ".2.1".
    const serverOps = [insert(server, 2, "1", 0), insert(server, 1, "2", 2)];

    const pair = new DeltaPair(clientOps, serverOps).transform();

    // With the same insertion point, the client op goes to the left of the
    // server op. The client inserts ".A..B.".
    expect(pair.client.length).toBe(2);
    checkInsert(pair.client[0]!, 1, "A", 3);
    checkInsert(pair.client[1]!, 4, "B", 1);

    // The server inserts "..2..1".
    expect(pair.server.length).toBe(2);
    checkInsert(pair.server[0]!, 4, "1", 0);
    checkInsert(pair.server[1]!, 2, "2", 3);
  });

  test("is same", () => {
    const clientOps = [insert(client, 1, "A", 1), insert(client, 3, "B", 0)];
    // The server has the same ops by the same author.
    const resultingVersion = HashedVersion.of(1, hash(1, 2, 3, 4));
    const serverOps = [insert(client, 1, "A", 1), insert(client, 3, "B", 0, resultingVersion)];

    // Deltas with the same ops are the same; other info is ignored.
    expect(DeltaPair.areSame(clientOps, serverOps)).toBe(true);

    // Transforming leaves only version update server ops.
    const pair = new DeltaPair(clientOps, serverOps).transform();
    expect(pair.client.length).toBe(0);
    expect(pair.server.length).toBe(2);
    checkVersionUpdate(pair.server[0]!, 1, null);
    checkVersionUpdate(pair.server[1]!, 1, resultingVersion);
  });

  // Not in the original: the same ops by another creator are not the same.
  test("same ops by another creator are not the same", () => {
    expect(DeltaPair.areSame([insert(client, 1, "A", 1)], [insert(server, 1, "A", 1)])).toBe(false);
    expect(DeltaPair.areSame([insert(client, 1, "A", 1)], [])).toBe(false);
  });
});

describe("OpComparatorsLargeTest", () => {
  test("doc op", () => {
    const a1 = new DocOpBuilder().characters("a").build();
    const a2 = new DocOpBuilder().characters("a").build();
    const b1 = new DocOpBuilder().deleteCharacters("a").build();
    const b2 = new DocOpBuilder().deleteCharacters("a").build();

    for (const [x, y] of [
      [a1, a1],
      [a1, a2],
      [a2, a1],
      [a2, a2],
      [b1, b1],
      [b1, b2],
      [b2, b1],
      [b2, b2],
    ] as const) {
      expect(syntacticIdentity(x, y)).toBe(true);
      expect(x.equals(y)).toBe(true);
    }
    for (const [x, y] of [
      [a1, b1],
      [a1, b2],
      [a2, b1],
      [a2, b2],
    ] as const) {
      expect(syntacticIdentity(x, y)).toBe(false);
      expect(x.equals(y)).toBe(false);
    }
  });

  /**
   * Two documents with annotations "x", `joined` and "y" that differ in
   * whether "x" and "y" or `joined` end first.
   */
  function docsEndingInDifferentOrder(joined: string): [DocInitialization, DocInitialization] {
    const starts = (): AnnotationBoundaryMap =>
      AnnotationBoundaryMap.changesOnly([
        ["x", null, "1"],
        [joined, null, "3"],
        ["y", null, "2"],
      ]);
    const endsBoth = (): AnnotationBoundaryMap => AnnotationBoundaryMap.ends(["x", "y"]);
    const endsJoined = (): AnnotationBoundaryMap => AnnotationBoundaryMap.ends([joined]);
    const doc1 = new DocOpBuilder()
      .annotationBoundary(starts())
      .characters("m")
      .annotationBoundary(endsBoth())
      .characters("n")
      .annotationBoundary(endsJoined())
      .buildInitialization();
    const doc2 = new DocOpBuilder()
      .annotationBoundary(starts())
      .characters("m")
      .annotationBoundary(endsJoined())
      .characters("n")
      .annotationBoundary(endsBoth())
      .buildInitialization();
    return [doc1, doc2];
  }

  function checkDistinct(doc1: DocInitialization, doc2: DocInitialization): void {
    expect(syntacticIdentity(doc1, doc2), `\ndoc1: ${doc1}\ndoc2: ${doc2}`).toBe(false);
    expect(doc1.equals(doc2)).toBe(false);
    // Not in the original: both documents do have an XML form, so the
    // inequality is not that of a failed conversion.
    const xml1 = toXmlString(doc1);
    const xml2 = toXmlString(doc2);
    expect(equalDocuments(doc1, doc2), `\ndoc1: ${xml1}\ndoc2: ${xml2}`).toBe(false);
    expect(equalDocuments(doc1, doc1)).toBe(true);
    expect(equalDocuments(doc2, doc2)).toBe(true);
  }

  // Ending the single annotation "x y" is distinct from ending the two
  // annotations "x" and "y".
  test("equal handles spaces in annotation keys", () => {
    checkDistinct(...docsEndingInDifferentOrder("x y"));
  });

  // Annotation keys may contain double quotes without ambiguity.
  test("equal handles quotes in annotation keys", () => {
    checkDistinct(...docsEndingInDifferentOrder('x" "y'));
  });

  test("random doc ops", () => {
    const p = defaultParameters();
    for (let i = 0; i < 200; i++) {
      const doc = new BootstrapDocument();
      for (let j = 0; j < 20; j++) {
        const ra = RandomProviderImpl.ofSeed(i * 20 + j);
        const rb = RandomProviderImpl.ofSeed(i * 20 + j + 1);
        const a = generateRandomOp(ra, p, doc);
        const b = generateRandomOp(rb, p, doc);
        doc.consume(a);
        if (!syntacticIdentity(a, a)) {
          throw new Error(`a != a for i=${i} j=${j}`);
        }
        // Not guaranteed by the generator, but true for these seeds.
        if (syntacticIdentity(a, b)) {
          throw new Error(`seeds for i=${i} j=${j}`);
        }
      }
    }
  });
});

describe("WaveletDataUtilTest", () => {
  const CREATOR = participant("creator@example.com");
  const JOE = participant("joe@example.com");
  const name = (): WaveletName =>
    WaveletName.of(WaveId.of("example.com", "w+wave"), WaveletId.of("example.com", "wavelet"));

  const opContext = (timestamp: number, version: HashedVersion): WaveletOperationContext =>
    WaveletOperationContext.withHashedVersion(CREATOR, timestamp, 1, version);
  const addParticipant = (user: ParticipantId, time: number, version: HashedVersion) =>
    WaveletOperation.addParticipant(opContext(time, version), user);
  const addBlip = (id: string, time: number, version: HashedVersion) =>
    WaveletOperation.blipContent(opContext(time, version), id, DocOp.EMPTY);

  /** A delta with the author, version and timestamp of its last op. */
  function delta(ops: WaveletOperation[]): TransformedWaveletDelta {
    const context = ops[ops.length - 1]!.context;
    return new TransformedWaveletDelta(
      context.creator,
      context.hashedVersion!,
      context.timestamp,
      ops,
    );
  }

  test("build wavelet from one delta", () => {
    const deltas = [delta([addParticipant(CREATOR, 1093, HashedVersion.unsigned(1))])];
    const wavelet = buildWaveletFromDeltas(name(), deltas);
    expect(wavelet.waveletName.equals(name())).toBe(true);
    expect(wavelet.creator.equals(CREATOR)).toBe(true);
    expect(wavelet.creationTime).toBe(1093);
    expect(wavelet.lastModifiedTime).toBe(1093);
    expect(wavelet.hashedVersion.equals(HashedVersion.unsigned(1))).toBe(true);
    expect(wavelet.documentIds()).toEqual([]);
    expectParticipants(wavelet.participants, [CREATOR]);
  });

  test("build wavelet from three deltas", () => {
    const deltas = [
      delta([addParticipant(CREATOR, 1093, HashedVersion.unsigned(1))]),
      delta([addParticipant(JOE, 1492, HashedVersion.unsigned(2))]),
      delta([addBlip("blipid", 2010, HashedVersion.unsigned(3))]),
    ];
    const wavelet = buildWaveletFromDeltas(name(), deltas);
    expect(wavelet.waveletName.equals(name())).toBe(true);
    expect(wavelet.creator.equals(CREATOR)).toBe(true);
    expect(wavelet.creationTime).toBe(1093);
    expect(wavelet.lastModifiedTime).toBe(2010);
    expect(wavelet.hashedVersion.equals(HashedVersion.unsigned(3))).toBe(true);
    expect(wavelet.documentIds()).toEqual(["blipid"]);
    expectParticipants(wavelet.participants, [CREATOR, JOE]);
  });

  // Not in the original.
  test("build wavelet from no deltas or a later delta fails", () => {
    expectError(() => buildWaveletFromDeltas(name(), []), WaveletOperationError, {
      kind: "invalidArgument",
      message: "empty deltas",
    });
    const later = delta([addParticipant(CREATOR, 1093, HashedVersion.unsigned(2))]);
    expectError(() => buildWaveletFromDeltas(name(), [later]), WaveletOperationError, {
      kind: "invalidArgument",
      message: "first delta has non-zero version: 1",
    });
  });
});

// Tests of `transformSubmittedDelta` and `transformOps`, after
// `WaveletContainerImpl.maybeTransformSubmittedDelta`,
// `transformSubmittedDelta` and `transformOps`. The original has no unit
// tests of its own for these.
describe("submitted delta transform", () => {
  const alice = participant("alice@example.com");
  const bob = participant("bob@example.com");
  const carol = participant("carol@example.com");

  /** A version with a hash that depends on the version number. */
  const version = (n: number): HashedVersion => HashedVersion.of(n, hash(n, n, n, n));
  const context = (author: ParticipantId): WaveletOperationContext =>
    new WaveletOperationContext(author, NO_TIMESTAMP, 1);

  /** A server delta by `author` resulting in `resulting`. */
  const serverDelta = (
    author: ParticipantId,
    resulting: HashedVersion,
    ops: WaveletOperation[],
  ): TransformedWaveletDelta =>
    TransformedWaveletDelta.cloneOperations(author, resulting, 1234, ops);

  const expectVersion = (actual: HashedVersion, expected: HashedVersion): void => {
    expect(actual.equals(expected), `${actual} vs ${expected}`).toBe(true);
  };

  test("submit at head returns delta unchanged", () => {
    const delta = new WaveletDelta(alice, version(2), [insert(alice, 2, "A", 0)]);
    // The history is not looked at.
    const unrelated = serverDelta(bob, version(1), [insert(bob, 1, "1", 1)]);
    for (const history of [[], [unrelated]]) {
      const result = transformSubmittedDelta(delta, version(2), history);
      expect(result.equals(delta)).toBe(true);
      expect(result.ops[0]!.context.equals(delta.ops[0]!.context)).toBe(true);
    }
  });

  test("same version number with different hash is invalid hash", () => {
    const target = HashedVersion.of(2, hash(9, 9, 9, 9));
    const delta = new WaveletDelta(alice, target, [insert(alice, 2, "A", 0)]);
    const history = [serverDelta(bob, version(2), [insert(bob, 1, "1", 1)])];
    for (const h of [history, []]) {
      const e = expectError(
        () => transformSubmittedDelta(delta, version(2), h),
        SubmitTransformError,
        { kind: "invalidHash" },
      );
      if (e.detail.kind !== "invalidHash") {
        throw e;
      }
      expectVersion(e.detail.expected, version(2));
      expectVersion(e.detail.target, target);
    }
  });

  test("transform over one server delta", () => {
    // Alice appends to a document of two items; Bob has inserted before.
    const delta = new WaveletDelta(alice, version(0), [insert(alice, 2, "A", 0)]);
    const history = [serverDelta(bob, version(1), [insert(bob, 1, "1", 1)])];
    const result = transformSubmittedDelta(delta, version(1), history);
    expect(result.author.equals(alice)).toBe(true);
    expectVersion(result.targetVersion, version(1));
    expect(result.length).toBe(1);
    checkInsert(result.ops[0]!, 3, "A", 0);
    // The client op keeps its context.
    expect(result.ops[0]!.context.equals(delta.ops[0]!.context)).toBe(true);
  });

  test("transform over two server deltas", () => {
    const delta = new WaveletDelta(alice, version(0), [
      insert(alice, 2, "A", 0),
      WaveletOperation.addParticipant(context(alice), carol),
    ]);
    const history = [
      serverDelta(bob, version(1), [insert(bob, 1, "1", 1)]),
      serverDelta(bob, version(3), [
        insert(bob, 1, "2", 2),
        WaveletOperation.noOp(context(bob)),
      ]),
    ];
    const result = transformSubmittedDelta(delta, version(3), history);
    expect(result.author.equals(alice)).toBe(true);
    expectVersion(result.targetVersion, version(3));
    expect(result.length).toBe(2);
    checkInsert(result.ops[0]!, 4, "A", 0);
    expect(result.ops[1]!.equals(delta.ops[1]!)).toBe(true);
  });

  test("duplicate of server delta returns old target version", () => {
    const ops = [insert(alice, 2, "A", 0), WaveletOperation.addParticipant(context(alice), carol)];
    const delta = new WaveletDelta(alice, version(1), ops);
    const history = [
      // The delta as the server applied it the first time.
      serverDelta(alice, version(3), ops),
      serverDelta(bob, version(4), [insert(bob, 1, "1", 2)]),
    ];
    const result = transformSubmittedDelta(delta, version(4), history);
    expect(result.equals(delta)).toBe(true);
    expectVersion(result.targetVersion, version(1));
  });

  test("duplicate after other server delta returns version before duplicate", () => {
    // Alice's delta was transformed over Bob's before it was applied.
    const delta = new WaveletDelta(alice, version(0), [insert(alice, 2, "A", 0)]);
    const history = [
      serverDelta(bob, version(1), [insert(bob, 1, "1", 1)]),
      serverDelta(alice, version(2), [insert(alice, 3, "A", 0)]),
      serverDelta(bob, version(3), [insert(bob, 1, "2", 3)]),
    ];
    const result = transformSubmittedDelta(delta, version(3), history);
    expectVersion(result.targetVersion, version(1));
    expect(result.length).toBe(1);
    checkInsert(result.ops[0]!, 3, "A", 0);
  });

  test("same ops by other author are not a duplicate", () => {
    const addCarol = (author: ParticipantId): WaveletOperation =>
      WaveletOperation.addParticipant(context(author), carol);
    const delta = new WaveletDelta(alice, version(0), [addCarol(alice)]);
    const history = [serverDelta(bob, version(1), [addCarol(bob)])];
    const result = transformSubmittedDelta(delta, version(1), history);
    // Transformed to the current version, where the addition is a no-op.
    expectVersion(result.targetVersion, version(1));
    expectOpsEqual(result.ops, [WaveletOperation.noOp(context(alice))]);
    expect(result.ops[0]!.context.creator.equals(alice)).toBe(true);
  });

  test("delta without ops returns old target version", () => {
    // A delta that has transformed to nothing is returned at the version
    // where that was noticed, not at the current version.
    const delta = new WaveletDelta(alice, version(0), []);
    const history = [
      serverDelta(bob, version(1), [insert(bob, 1, "1", 1)]),
      serverDelta(bob, version(2), [insert(bob, 1, "2", 2)]),
    ];
    const result = transformSubmittedDelta(delta, version(2), history);
    expect(result.author.equals(alice)).toBe(true);
    expectVersion(result.targetVersion, version(0));
    expect(result.isEmpty()).toBe(true);
  });

  test("removed author fails", () => {
    const delta = new WaveletDelta(alice, version(0), [insert(alice, 2, "A", 0)]);
    const history = [
      serverDelta(bob, version(1), [insert(bob, 1, "1", 1)]),
      serverDelta(bob, version(2), [WaveletOperation.removeParticipant(context(bob), alice)]),
    ];
    const e = expectError(
      () => transformSubmittedDelta(delta, version(2), history),
      SubmitTransformError,
      { kind: "transform" },
    );
    if (e.detail.kind !== "transform") {
      throw e;
    }
    expect(e.detail.error).toBeInstanceOf(WaveTransformError);
    expect(e.detail.error.detail).toEqual({ kind: "removedAuthor", author: "alice@example.com" });
  });

  function expectHistoryError(
    f: () => unknown,
    target: HashedVersion,
    current: HashedVersion,
  ): void {
    const e = expectError(f, SubmitTransformError, { kind: "history" });
    if (e.detail.kind !== "history") {
      throw e;
    }
    expectVersion(e.detail.target, target);
    expectVersion(e.detail.current, current);
  }

  test("empty history fails", () => {
    const delta = new WaveletDelta(alice, version(0), [insert(alice, 2, "A", 0)]);
    expectHistoryError(
      () => transformSubmittedDelta(delta, version(1), []),
      version(0),
      version(1),
    );
  });

  test("history not ending at current version fails", () => {
    const delta = new WaveletDelta(alice, version(0), [insert(alice, 2, "A", 0)]);
    const history = [serverDelta(bob, version(1), [insert(bob, 1, "1", 1)])];
    // The history stops short of the current version.
    expectHistoryError(
      () => transformSubmittedDelta(delta, version(2), history),
      version(1),
      version(2),
    );
    // The history ends at the current version number with another hash.
    const current = HashedVersion.of(1, hash(9, 9, 9, 9));
    expectHistoryError(
      () => transformSubmittedDelta(delta, current, history),
      version(1),
      current,
    );
  });

  test("transformOps without server ops is identity", () => {
    const clientOps = [
      insert(alice, 2, "A", 0),
      WaveletOperation.addParticipant(context(alice), carol),
    ];
    expectOpsEqual(transformOps(clientOps, []), clientOps);
    expect(transformOps([], clientOps)).toEqual([]);
  });

  test("transformOps transforms each client op over all server ops", () => {
    const clientOps = [
      insert(alice, 2, "A", 0),
      WaveletOperation.addParticipant(context(alice), carol),
      WaveletOperation.removeParticipant(context(alice), bob),
    ];
    const serverOps = [
      insert(bob, 1, "1", 1),
      WaveletOperation.addParticipant(context(bob), carol),
      insert(bob, 1, "2", 2),
    ];
    const transformed = transformOps(clientOps, serverOps);
    expect(transformed.length).toBe(3);
    checkInsert(transformed[0]!, 4, "A", 0);
    // Carol was added concurrently.
    expect(transformed[1]!.equals(WaveletOperation.noOp(context(alice)))).toBe(true);
    expect(transformed[2]!.equals(clientOps[2]!)).toBe(true);
    clientOps.forEach((clientOp, i) => {
      expect(transformed[i]!.context.equals(clientOp.context)).toBe(true);
    });
  });

  test("transformOps fails on concurrent add and remove", () => {
    const clientOps = [
      insert(alice, 2, "A", 0),
      WaveletOperation.removeParticipant(context(alice), carol),
    ];
    const serverOps = [WaveletOperation.addParticipant(context(bob), carol)];
    expectError(() => transformOps(clientOps, serverOps), WaveTransformError, {
      kind: "participant",
      participant: "carol@example.com",
    });
  });
});

// The unit test of `transform.rs`.
describe("transform", () => {
  function insertInto(who: string, size: number, location: number, text: string): WaveletOperation {
    return WaveletOperation.blipContent(
      new WaveletOperationContext(participant(who), NO_TIMESTAMP, 1),
      "b+1",
      creator.insertCharacters(size, location, text),
    );
  }

  // Documents the quirk of `transformOps`: the second client operation is
  // transformed against a server operation for the wrong state.
  test("server transformOps does not carry server ops forward", () => {
    // Document of 5 items. The client inserts "a" and then "b" at the
    // start; the server concurrently inserts "s" at the end.
    const client = [insertInto("a@example.com", 5, 0, "a"), insertInto("a@example.com", 6, 0, "b")];
    const server = [insertInto("b@example.com", 5, 5, "s")];

    const [exact] = transformOpLists(client, server);
    expectOpsEqual(exact, [
      insertInto("a@example.com", 6, 0, "a"),
      insertInto("a@example.com", 7, 0, "b"),
    ]);
    // The second client op (for 6 items) meets the untransformed server op
    // (for 5 items), which the document transform rejects.
    expectError(() => transformOps(client, server), WaveTransformError, { kind: "document" });
  });
});

// Not in the original: the consistency checks of `TransformedWaveletDelta`.
describe("TransformedWaveletDelta", () => {
  const author = participant("a@example.com");
  const other = participant("b@example.com");
  const v2 = HashedVersion.of(2, hash(2, 2));
  const plain = new WaveletOperationContext(author, 10, 1);
  const last = WaveletOperationContext.withHashedVersion(author, 10, 1, v2);
  const build = (...contexts: WaveletOperationContext[]): TransformedWaveletDelta =>
    new TransformedWaveletDelta(author, v2, 10, contexts.map((c) => WaveletOperation.noOp(c)));

  test("accepts consistent contexts", () => {
    const delta = build(plain, last);
    expect(delta.appliedAtVersion).toBe(0);
    expect(delta.length).toBe(2);
    expect([...delta].length).toBe(2);
    expect(delta.equals(build(plain, last))).toBe(true);
    expect(build().isEmpty()).toBe(true);
  });

  test("rejects inconsistent contexts", () => {
    const check = (kind: string, ...contexts: WaveletOperationContext[]): void => {
      const e = (() => {
        try {
          build(...contexts);
        } catch (e) {
          return e;
        }
        return null;
      })();
      expect((e as { kind?: string } | null)?.kind, kind).toBe(kind);
    };
    check("versionTooSmall", plain, plain, last);
    check("creatorMismatch", new WaveletOperationContext(other, 10, 1), last);
    check("versionIncrement", new WaveletOperationContext(author, 10, 2), last);
    check("hashedVersionPresence", plain, plain);
    check("hashedVersionPresence", last, last);
    check(
      "hashedVersionMismatch",
      plain,
      WaveletOperationContext.withHashedVersion(author, 10, 1, HashedVersion.unsigned(2)),
    );
    check("timestampMismatch", new WaveletOperationContext(author, 11, 1), last);
  });

  test("cloneOperations rejects uncloneable operations", () => {
    const e = (() => {
      try {
        TransformedWaveletDelta.cloneOperations(author, v2, 10, [
          WaveletOperation.versionUpdate(author, 1),
        ]);
      } catch (e) {
        return e;
      }
      return null;
    })();
    expect((e as { kind?: string } | null)?.kind).toBe("operation");
  });
});
