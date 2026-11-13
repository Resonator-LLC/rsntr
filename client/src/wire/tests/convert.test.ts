// Ports the conversion tests of `crates/resonator-model/src/proto/tests.rs`
// (`WaveletOperationSerializerTest`, `SnapshotSerializerTest`, converter
// validation and round trips; through JSON here instead of protobuf bytes),
// and checks the conversions against the messages of `json_golden.txt`.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";

import { type BlipData, DataError, WaveletData } from "../../model/data";
import {
  AnnotationBoundaryMap,
  Attributes,
  AttributesUpdate,
  BootstrapDocument,
  DocOp,
  DocOpBuilder,
  OperationError,
} from "../../model/docop";
import {
  RandomProviderImpl,
  defaultParameters,
  generateRandomOp,
} from "../../model/docop/testing";
import {
  InvalidIdError,
  InvalidParticipantAddress,
  ParticipantId,
  WaveId,
  WaveletId,
  WaveletName,
} from "../../model/id";
import {
  BlipOperation,
  DeltaError,
  NO_TIMESTAMP,
  TransformedWaveletDelta,
  WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
} from "../../model/operation";
import { HashedVersion, HashedVersionFactory } from "../../model/version";
import {
  type AnnotationBoundary,
  type Component,
  type DocumentSnapshot,
  type KeyValuePair,
  type KeyValueUpdate,
  type MessageCodec,
  ProtoError,
  ProtocolDocumentOperation,
  type ProtocolHashedVersion,
  ProtocolSubmitRequest,
  ProtocolWaveletDelta,
  ProtocolWaveletOperation,
  ProtocolWaveletUpdate,
  WaveViewSnapshot,
  WaveletSnapshot,
  deserializeDelta,
  deserializeDocOp,
  deserializeHashedVersion,
  deserializeOp,
  deserializeTransformedDelta,
  deserializeWavelet,
  parseJson,
  serializeDelta,
  serializeDocOp,
  serializeDocument,
  serializeHashedVersion,
  serializeOp,
  serializeTransformedDelta,
  serializeWavelet,
} from "..";

const pid = (address: string): ParticipantId => ParticipantId.of(address);

/** A context for ops that don't have one (`OP_CONTEXT`). */
const opContext = (): WaveletOperationContext =>
  new WaveletOperationContext(ParticipantId.newUnchecked("test@example.com"), 0, 1);

const attrs = (pairs: readonly (readonly [string, string])[]): Attributes =>
  Attributes.fromPairs(pairs);

const bytes = (...values: number[]): Uint8Array => Uint8Array.from(values);

const hashedVersion = (version: number, hash: Uint8Array): ProtocolHashedVersion => ({
  version,
  historyHash: hash,
});

const kvp = (key: string, value: string): KeyValuePair => ({ key, value });

function kvu(key: string, oldValue: string | null, newValue: string | null): KeyValueUpdate {
  return {
    key,
    ...(oldValue !== null ? { oldValue } : {}),
    ...(newValue !== null ? { newValue } : {}),
  };
}

const characters = (text: string): Component => ({ characters: text });
const retain = (itemCount: number): Component => ({ retainItemCount: itemCount });
const elementStart = (tag: string, attribute: KeyValuePair[] = []): Component => ({
  elementStart: { type: tag, attribute },
});
const elementEnd = (): Component => ({ elementEnd: true });
const boundary = (end: string[], change: KeyValueUpdate[]): Component => ({
  annotationBoundary: { end, change },
});
const updateAttributes = (attributeUpdate: KeyValueUpdate[]): Component => ({
  updateAttributes: { attributeUpdate },
});
const docOpMessage = (component: Component[]): ProtocolDocumentOperation => ({ component });
const noOpMessage = (): ProtocolWaveletOperation => ({ noOp: true });
const addParticipantMessage = (address: string): ProtocolWaveletOperation => ({
  addParticipant: address,
});

/** Removes fields from a message, as a message built without them. */
function without<T extends object>(message: T, ...fields: (keyof T)[]): T {
  const copy = { ...message };
  for (const field of fields) {
    delete copy[field];
  }
  return copy;
}

function protoError(f: () => unknown): ProtoError {
  try {
    f();
  } catch (error) {
    expect(error).toBeInstanceOf(ProtoError);
    return error as ProtoError;
  }
  throw new Error("expected the call to throw");
}

function expectMissing(f: () => unknown, field: string): void {
  const d = protoError(f).detail;
  expect(d.kind).toBe("missingField");
  expect(d.kind === "missingField" && d.field.endsWith(field), JSON.stringify(d)).toBe(true);
}

function expectInvalid(f: () => unknown, field: string): void {
  const d = protoError(f).detail;
  expect(d.kind, JSON.stringify(d)).toBe("invalidValue");
  expect(d.kind === "invalidValue" && d.field.endsWith(field), JSON.stringify(d)).toBe(true);
}

function expectKind(f: () => unknown, kind: ProtoError["kind"]): ProtoError {
  const error = protoError(f);
  expect(error.kind, error.message).toBe(kind);
  return error;
}

/** Through the JSON text and back. */
function viaJson<T>(codec: MessageCodec<T>, message: T): T {
  return codec.decode(parseJson(codec.encode(message)));
}

// WaveletOperationSerializerTest.

describe("WaveletOperationSerializerTest", () => {
  /** An operation is unchanged when serialised then deserialised. */
  function assertReversible(op: WaveletOperation): void {
    expect(deserializeOp(serializeOp(op), opContext()).equals(op)).toBe(true);

    const author = pid("kalman@google.com");
    const version = HashedVersion.unsigned(0);
    const delta = new WaveletDelta(author, version, [op, op, op]);
    const serialized = serializeDelta(delta);
    const deserialized = deserializeDelta(serialized);
    expect(serialized.hashedVersion.version).toBe(version.version);
    expect(serialized.hashedVersion.historyHash).toEqual(version.historyHash);
    expect(deserialized.author.equals(delta.author)).toBe(true);
    expect(deserialized.length).toBe(delta.length);
    deserialized.ops.forEach((o, i) => expect(o.equals(delta.ops[i]!)).toBe(true));

    // Not in the original: the same holds through the wire text.
    const decoded = viaJson(ProtocolWaveletDelta, serialized);
    expect(decoded).toEqual(serialized);
    expect(deserializeDelta(decoded).equals(delta)).toBe(true);
  }

  const blipOp = (blipId: string, mutation: DocOpBuilder): WaveletOperation =>
    WaveletOperation.blipContent(opContext(), blipId, mutation.build());

  const a = (): Attributes =>
    attrs([
      ["a1", "1"],
      ["a2", "2"],
    ]);
  const b = (): Attributes => Attributes.empty();
  const c = (): Attributes =>
    attrs([
      ["c1", "1"],
      ["c2", "2"],
      ["c3", "3"],
    ]);

  test("no op", () => assertReversible(WaveletOperation.noOp(opContext())));

  test("add participant", () =>
    assertReversible(WaveletOperation.addParticipant(opContext(), pid("kalman@google.com"))));

  test("remove participant", () =>
    assertReversible(WaveletOperation.removeParticipant(opContext(), pid("kalman@google.com"))));

  test("empty document mutation", () => assertReversible(blipOp("empty", new DocOpBuilder())));

  test("single characters", () =>
    assertReversible(blipOp("single", new DocOpBuilder().characters("hello"))));

  test("many characters", () =>
    assertReversible(
      blipOp(
        "many",
        new DocOpBuilder()
          .characters("hello")
          .characters("world")
          .characters("foo")
          .characters("bar"),
      ),
    ));

  test("retain", () =>
    assertReversible(
      blipOp(
        "retain",
        new DocOpBuilder()
          .characters("hello")
          .retain(5)
          .characters("world")
          .retain(10)
          .characters("foo")
          .retain(13)
          .characters("bar")
          .retain(16),
      ),
    ));

  test("delete characters", () =>
    assertReversible(
      blipOp(
        "deleteCharacters",
        new DocOpBuilder()
          .characters("hello")
          .retain(1)
          .deleteCharacters("ab")
          .characters("world")
          .retain(2)
          .deleteCharacters("cd"),
      ),
    ));

  test("elements", () =>
    assertReversible(
      blipOp(
        "elements",
        new DocOpBuilder()
          .elementStart("a", a())
          .elementStart("b", b())
          .elementStart("c", c())
          .elementEnd()
          .elementEnd()
          .elementEnd(),
      ),
    ));

  test("characters and elements", () =>
    assertReversible(
      blipOp(
        "charactersAndElements",
        new DocOpBuilder()
          .elementStart("a", a())
          .characters("hello")
          .elementStart("b", b())
          .characters("world")
          .elementStart("c", c())
          .elementEnd()
          .characters("blah")
          .elementEnd()
          .elementEnd(),
      ),
    ));

  test("delete elements", () =>
    assertReversible(
      blipOp(
        "deleteElements",
        new DocOpBuilder()
          .deleteElementStart("a", a())
          .deleteElementStart("b", b())
          .deleteElementStart("c", c())
          .deleteElementEnd()
          .deleteElementEnd()
          .deleteElementEnd(),
      ),
    ));

  test("delete characters and elements", () =>
    assertReversible(
      blipOp(
        "deleteCharactersAndElements",
        new DocOpBuilder()
          .deleteElementStart("a", a())
          .deleteCharacters("hello")
          .deleteElementStart("b", b())
          .deleteCharacters("world")
          .deleteElementStart("c", c())
          .deleteElementEnd()
          .deleteCharacters("blah")
          .deleteElementEnd()
          .deleteElementEnd(),
      ),
    ));

  test("annotation boundary", () =>
    assertReversible(
      blipOp(
        "annotationBoundary",
        new DocOpBuilder()
          .elementStart("a", a())
          .annotationBoundary(AnnotationBoundaryMap.changesOnly([["a", null, "b"]]))
          .characters("test")
          .annotationBoundary(AnnotationBoundaryMap.changesOnly([["a", "b", null]]))
          .characters("text")
          .annotationBoundary(AnnotationBoundaryMap.ends(["a"]))
          .elementEnd(),
      ),
    ));

  test("empty annotation boundary", () =>
    assertReversible(
      blipOp(
        "emptyAnnotationBoundary",
        new DocOpBuilder()
          .elementStart("a", a())
          .annotationBoundary(AnnotationBoundaryMap.empty())
          .characters("text")
          .annotationBoundary(AnnotationBoundaryMap.empty())
          .elementEnd(),
      ),
    ));

  test("replace attributes", () =>
    assertReversible(
      blipOp(
        "replaceAttributes",
        new DocOpBuilder()
          .retain(4)
          .replaceAttributes(
            a(),
            attrs([
              ["a1", "3"],
              ["a2", "4"],
            ]),
          )
          .retain(4),
      ),
    ));

  test("empty replace attributes", () =>
    assertReversible(
      blipOp(
        "emptyReplaceAttributes",
        new DocOpBuilder()
          .retain(4)
          .replaceAttributes(Attributes.empty(), Attributes.empty())
          .retain(4),
      ),
    ));

  test("update attributes", () =>
    assertReversible(
      blipOp(
        "updateAttributes",
        new DocOpBuilder()
          .retain(4)
          .updateAttributes(
            AttributesUpdate.fromTriples([
              ["a", null, "2"],
              ["b", "1", null],
            ]),
          )
          .retain(4),
      ),
    ));

  test("empty update attributes", () =>
    assertReversible(
      blipOp(
        "emptyUpdateAttributes",
        new DocOpBuilder().retain(4).updateAttributes(AttributesUpdate.empty()).retain(4),
      ),
    ));
});

// SnapshotSerializerTest.

describe("SnapshotSerializerTest", () => {
  /** `TestDataUtil.createSimpleWaveletData`. */
  function createSimpleWaveletData(): WaveletData {
    const name = WaveletName.of(
      WaveId.of("example.com", "w+abc123"),
      WaveletId.of("example.com", "conv+root"),
    );
    const creator = ParticipantId.newUnchecked("sam@example.com");
    const time = 1234567890;
    const wavelet = WaveletData.empty(name, creator, HashedVersion.unsigned(0), time);
    const content = new DocOpBuilder().characters("Hello there").buildInitialization();
    wavelet.createDocument("b+abc123", creator, [], content, time, 0);
    return wavelet;
  }

  function checkSerializedDocument(expected: BlipData, actual: BlipData): void {
    expect(actual.id).toBe(expected.id);
    expect(actual.content.toXmlString()).toBe(expected.content.toXmlString());
    expect(actual.author.equals(expected.author)).toBe(true);
    expect(actual.contributors.map(String)).toEqual(expected.contributors.map(String));
    expect(actual.lastModifiedTime).toBe(expected.lastModifiedTime);
    expect(actual.lastModifiedVersion).toBe(expected.lastModifiedVersion);
  }

  test("wavelet roundtrip", () => {
    const expected = createSimpleWaveletData();
    const version = HashedVersionFactory.createVersionZero(expected.waveletName);
    const snapshot = serializeWavelet(expected, version);
    const actual = deserializeWavelet(snapshot, expected.waveId);

    expect(actual.waveId.equals(expected.waveId)).toBe(true);
    expect(actual.participants.map(String)).toEqual(expected.participants.map(String));
    expect(actual.version).toBe(expected.version);
    expect(actual.lastModifiedTime).toBe(expected.lastModifiedTime);
    expect(actual.creator.equals(expected.creator)).toBe(true);
    expect(actual.creationTime).toBe(expected.creationTime);
    expect(actual.documentIds()).toEqual(expected.documentIds());
    for (const docId of expected.documentIds()) {
      checkSerializedDocument(expected.document(docId)!, actual.document(docId)!);
    }
  });
});

// Converter validation.

describe("validation", () => {
  test("deserializeDocOp rejects negative retain count", () => {
    expectInvalid(() => deserializeDocOp(docOpMessage([retain(-1)])), "retain_item_count");
    expectInvalid(
      () => deserializeDocOp(docOpMessage([retain(-0x80000000)])),
      "retain_item_count",
    );
  });

  test("deserializeDocOp rejects zero retain count", () => {
    expectKind(() => deserializeDocOp(docOpMessage([retain(0)])), "operation");
  });

  test("serializeDocOp rejects retain count above int32", () => {
    const tooBig = DocOp.unchecked([{ type: "retain", itemCount: 0x80000000 }]);
    expectInvalid(() => serializeDocOp(tooBig), "retain_item_count");
    const max = DocOp.unchecked([{ type: "retain", itemCount: 0x7fffffff }]);
    expect(serializeDocOp(max)).toEqual(docOpMessage([retain(0x7fffffff)]));
  });

  test("deserializeDocOp rejects unclosed element", () => {
    expectKind(
      () => deserializeDocOp(docOpMessage([elementStart("p"), characters("a")])),
      "operation",
    );
    const closed = docOpMessage([elementStart("p"), characters("a"), elementEnd()]);
    expect(
      deserializeDocOp(closed).equals(
        new DocOpBuilder()
          .elementStart("p", Attributes.empty())
          .characters("a")
          .elementEnd()
          .build(),
      ),
    ).toBe(true);
  });

  test("deserializeDocOp rejects unmatched element end", () => {
    expectKind(
      () => deserializeDocOp(docOpMessage([characters("a"), elementEnd()])),
      "operation",
    );
    expectKind(() => deserializeDocOp(docOpMessage([{ deleteElementEnd: true }])), "operation");
  });

  test("deserializeDocOp rejects empty characters", () => {
    expectKind(() => deserializeDocOp(docOpMessage([characters("")])), "operation");
    expectKind(() => deserializeDocOp(docOpMessage([{ deleteCharacters: "" }])), "operation");
  });

  test("deserializeDocOp rejects unclosed annotation", () => {
    expectKind(
      () =>
        deserializeDocOp(docOpMessage([boundary([], [kvu("k", null, "n")]), characters("a")])),
      "operation",
    );
  });

  test("deserializeDocOp rejects annotation boundary with duplicate key", () => {
    const boundaries: AnnotationBoundary[] = [
      { end: [], change: [kvu("k", null, "a"), kvu("k", null, "b")] },
      { end: ["k", "k"], change: [] },
      { end: ["k"], change: [kvu("k", null, "a")] },
    ];
    for (const annotationBoundary of boundaries) {
      const message = docOpMessage([
        boundary([], [kvu("k", null, "x")]),
        characters("a"),
        { annotationBoundary },
        characters("b"),
        boundary(["k"], []),
      ]);
      expectKind(() => deserializeDocOp(message), "operation");
    }
  });

  test("deserializeDocOp rejects annotation boundary with invalid key", () => {
    for (const key of ["", "a?b", "a@b"]) {
      const message = docOpMessage([
        boundary([], [kvu(key, null, "x")]),
        characters("a"),
        boundary([key], []),
      ]);
      expectKind(() => deserializeDocOp(message), "operation");
    }
  });

  test("deserializeDocOp rejects unsorted annotation boundary keys", () => {
    const message = (changes: KeyValueUpdate[]): ProtocolDocumentOperation =>
      docOpMessage([boundary([], changes), characters("a"), boundary(["a", "b"], [])]);
    deserializeDocOp(message([kvu("a", null, "1"), kvu("b", null, "2")]));
    expectKind(
      () => deserializeDocOp(message([kvu("b", null, "2"), kvu("a", null, "1")])),
      "operation",
    );
  });

  test("deserializeDocOp rejects duplicate attributes update keys", () => {
    const message = docOpMessage([
      updateAttributes([kvu("a", null, "1"), kvu("a", null, "2")]),
    ]);
    expectKind(() => deserializeDocOp(message), "operation");
  });

  test("deserializeDocOp sorts unsorted attributes update keys", () => {
    const message = docOpMessage([
      updateAttributes([kvu("b", "1", null), kvu("a", null, "2")]),
    ]);
    const op = deserializeDocOp(message);
    const first = op.components[0]!;
    expect(first.type === "updateAttributes" && first.update.updates.map((u) => u.name)).toEqual([
      "a",
      "b",
    ]);
    expect(
      op.equals(
        new DocOpBuilder()
          .updateAttributes(
            AttributesUpdate.fromTriples([
              ["a", null, "2"],
              ["b", "1", null],
            ]),
          )
          .build(),
      ),
    ).toBe(true);
  });

  test("deserializeDocOp accepts duplicate attribute keys with last wins", () => {
    const pairs = [kvp("k", "first"), kvp("a", "1"), kvp("k", "last")];
    const expected = attrs([
      ["a", "1"],
      ["k", "last"],
    ]);
    const message = docOpMessage([
      elementStart("p", pairs),
      elementEnd(),
      { deleteElementStart: { type: "q", attribute: pairs } },
      { deleteElementEnd: true },
      { replaceAttributes: { oldAttribute: pairs, newAttribute: pairs } },
    ]);
    expect(
      deserializeDocOp(message).equals(
        new DocOpBuilder()
          .elementStart("p", expected)
          .elementEnd()
          .deleteElementStart("q", expected)
          .deleteElementEnd()
          .replaceAttributes(expected, expected)
          .build(),
      ),
    ).toBe(true);
  });

  test("deserializeDocOp sorts attributes", () => {
    const op = deserializeDocOp(
      docOpMessage([elementStart("p", [kvp("c", "3"), kvp("a", "1"), kvp("b", "2")]), elementEnd()]),
    );
    const first = op.components[0]!;
    expect(first.type === "elementStart" && first.attributes.entries).toEqual([
      { name: "a", value: "1" },
      { name: "b", value: "2" },
      { name: "c", value: "3" },
    ]);
  });

  test("deserializeDocOp rejects messages missing required fields", () => {
    const noType = docOpMessage([
      { elementStart: { attribute: [] } as unknown as { type: string; attribute: [] } },
    ]);
    expectMissing(() => deserializeDocOp(noType), "type");
    const noValue = docOpMessage([
      elementStart("p", [{ key: "k" } as unknown as KeyValuePair]),
      elementEnd(),
    ]);
    expectMissing(() => deserializeDocOp(noValue), "KeyValuePair.value");
    const noKey = docOpMessage([updateAttributes([{ newValue: "n" } as unknown as KeyValueUpdate])]);
    expectMissing(() => deserializeDocOp(noKey), "KeyValueUpdate.key");
  });

  test("deserializeDocOp skips component without fields", () => {
    const op = deserializeDocOp(docOpMessage([{}, characters("a"), {}]));
    expect(op.equals(new DocOpBuilder().characters("a").build())).toBe(true);
  });

  test("deserializeDocOp counts false booleans as present", () => {
    const message = docOpMessage([
      elementStart("p"),
      { elementEnd: false },
      { deleteElementStart: { type: "q", attribute: [] } },
      { deleteElementEnd: false },
    ]);
    expect(
      deserializeDocOp(message).equals(
        new DocOpBuilder()
          .elementStart("p", Attributes.empty())
          .elementEnd()
          .deleteElementStart("q", Attributes.empty())
          .deleteElementEnd()
          .build(),
      ),
    ).toBe(true);
  });

  test("deserializeDocOp reads first set field of component", () => {
    let message = docOpMessage([{ characters: "a", retainItemCount: 3, deleteCharacters: "b" }]);
    expect(deserializeDocOp(message).equals(new DocOpBuilder().characters("a").build())).toBe(
      true,
    );
    message = docOpMessage([{ retainItemCount: 3, deleteCharacters: "b" }]);
    expect(deserializeDocOp(message).equals(new DocOpBuilder().retain(3).build())).toBe(true);
  });

  test("deserializeDocOp ignores contents when empty flag is set", () => {
    let message = docOpMessage([
      {
        replaceAttributes: {
          empty: true,
          oldAttribute: [kvp("a", "1")],
          newAttribute: [kvp("a", "2")],
        },
      },
      { updateAttributes: { empty: true, attributeUpdate: [kvu("a", null, "1")] } },
    ]);
    expect(
      deserializeDocOp(message).equals(
        new DocOpBuilder()
          .replaceAttributes(Attributes.empty(), Attributes.empty())
          .updateAttributes(AttributesUpdate.empty())
          .build(),
      ),
    ).toBe(true);
    // A false flag is the same as an absent one.
    message = docOpMessage([
      { updateAttributes: { empty: false, attributeUpdate: [kvu("a", null, "1")] } },
    ]);
    expect(
      deserializeDocOp(message).equals(
        new DocOpBuilder().updateAttributes(AttributesUpdate.single("a", null, "1")).build(),
      ),
    ).toBe(true);
  });

  test("deserializeHashedVersion rejects negative version", () => {
    expectInvalid(
      () => deserializeHashedVersion(hashedVersion(-1, bytes())),
      "ProtocolHashedVersion.version",
    );
    expectInvalid(
      () => deserializeHashedVersion(hashedVersion(Number.MIN_SAFE_INTEGER, bytes(1))),
      "ProtocolHashedVersion.version",
    );
    const max = deserializeHashedVersion(hashedVersion(Number.MAX_SAFE_INTEGER, bytes(1)));
    expect(max.equals(HashedVersion.of(Number.MAX_SAFE_INTEGER, bytes(1)))).toBe(true);
  });

  test("deserializeHashedVersion rejects missing fields", () => {
    expectMissing(
      () => deserializeHashedVersion({ historyHash: bytes() } as ProtocolHashedVersion),
      "version",
    );
    expectMissing(
      () => deserializeHashedVersion({ version: 1 } as ProtocolHashedVersion),
      "history_hash",
    );
  });

  test("serializeHashedVersion rejects versions beyond the safe range", () => {
    expectInvalid(
      () => serializeHashedVersion(HashedVersion.unsigned(Number.MAX_SAFE_INTEGER + 1)),
      "ProtocolHashedVersion.version",
    );
    expectInvalid(
      () => serializeHashedVersion(HashedVersion.of(2 ** 64, bytes(1))),
      "ProtocolHashedVersion.version",
    );
    expect(
      serializeHashedVersion(HashedVersion.of(Number.MAX_SAFE_INTEGER, bytes(1))),
    ).toEqual(hashedVersion(Number.MAX_SAFE_INTEGER, bytes(1)));
  });

  test("deserializeOp rejects empty operation", () => {
    expectKind(() => deserializeOp({}, opContext()), "unsupported");
    const decoded = ProtocolWaveletOperation.decode(parseJson("{}"));
    expectKind(() => deserializeOp(decoded, opContext()), "unsupported");
  });

  test("deserializeOp reads false noOp as no-op", () => {
    expect(deserializeOp({ noOp: false }, opContext()).kind).toEqual({ type: "noOp" });
    const decoded = ProtocolWaveletOperation.decode(parseJson('{"4":false}'));
    expect(decoded.noOp).toBe(false);
    expect(deserializeOp(decoded, opContext()).kind).toEqual({ type: "noOp" });
  });

  test("deserializeOp prefers noOp then add then remove then mutate", () => {
    const full: ProtocolWaveletOperation = {
      addParticipant: "a@b.c",
      removeParticipant: "d@e.f",
      mutateDocument: { documentId: "b", documentOperation: docOpMessage([characters("a")]) },
      noOp: true,
    };
    expect(deserializeOp(full, opContext()).kind.type).toBe("noOp");
    const add = deserializeOp(without(full, "noOp"), opContext()).kind;
    expect(add.type === "addParticipant" && add.participant.address).toBe("a@b.c");
    expect(add.type === "addParticipant" && add.position).toBe(null);
    const remove = deserializeOp(without(full, "noOp", "addParticipant"), opContext()).kind;
    expect(remove.type === "removeParticipant" && remove.participant.address).toBe("d@e.f");
    const mutate = deserializeOp(
      without(full, "noOp", "addParticipant", "removeParticipant"),
      opContext(),
    );
    expect(
      mutate.equals(
        WaveletOperation.blipContent(opContext(), "b", new DocOpBuilder().characters("a").build()),
      ),
    ).toBe(true);
  });

  test("deserializeOp keeps context and does not validate addresses", () => {
    const context = WaveletOperationContext.withHashedVersion(
      pid("a@b.c"),
      1234,
      1,
      HashedVersion.of(5, bytes(1)),
    );
    const op = deserializeOp(addParticipantMessage("Not An Address"), context);
    expect(op.context).toBe(context);
    // Lower-cased but otherwise taken as is.
    expect(op.kind.type === "addParticipant" && op.kind.participant.address).toBe(
      "not an address",
    );
  });

  test("deserializeOp rejects invalid document operation", () => {
    expectInvalid(
      () =>
        deserializeOp(
          { mutateDocument: { documentId: "b", documentOperation: docOpMessage([retain(-1)]) } },
          opContext(),
        ),
      "retain_item_count",
    );
    expectMissing(
      () =>
        deserializeOp(
          { mutateDocument: { documentId: "b" } } as unknown as ProtocolWaveletOperation,
          opContext(),
        ),
      "MutateDocument.document_operation",
    );
  });

  test("serializeOp rejects submit and version update", () => {
    const submit = WaveletOperation.blip(opContext(), "b", BlipOperation.submit());
    expectKind(() => serializeOp(submit), "unsupported");
    expectKind(
      () =>
        serializeOp(WaveletOperation.versionUpdate(pid("a@b.c"), 1, HashedVersion.unsigned(1))),
      "unsupported",
    );
    expectKind(
      () => serializeOp(WaveletOperation.versionUpdate(pid("a@b.c"), 1, null, "b", 3)),
      "unsupported",
    );
    // A delta containing one cannot be serialised either.
    const delta = new WaveletDelta(pid("a@b.c"), HashedVersion.unsigned(0), [
      WaveletOperation.noOp(opContext()),
      submit,
    ]);
    expectKind(() => serializeDelta(delta), "unsupported");
  });

  const deltaMessage = (
    author: string,
    operation: ProtocolWaveletOperation[],
  ): ProtocolWaveletDelta => ({
    hashedVersion: hashedVersion(3, bytes(1, 2)),
    author,
    operation,
    addressPath: [],
  });

  test("deserializeDelta sets contexts", () => {
    const delta = deserializeDelta(
      deltaMessage("A@b.c", [noOpMessage(), addParticipantMessage("x@y.z")]),
    );
    // The author is lower-cased.
    expect(delta.author.address).toBe("a@b.c");
    expect(delta.targetVersion.equals(HashedVersion.of(3, bytes(1, 2)))).toBe(true);
    expect(delta.length).toBe(2);
    for (const op of delta) {
      expect(
        op.context.equals(new WaveletOperationContext(pid("a@b.c"), NO_TIMESTAMP, 1)),
      ).toBe(true);
      expect(op.context.hashedVersion).toBe(null);
    }
  });

  test("deserializeDelta validates author only with operations", () => {
    const empty = deserializeDelta(deltaMessage("nobody", []));
    expect(empty.author.address).toBe("nobody");
    expect(empty.isEmpty()).toBe(true);
    const error = expectKind(
      () => deserializeDelta(deltaMessage("nobody", [noOpMessage()])),
      "participantId",
    );
    expect(error.cause).toBeInstanceOf(InvalidParticipantAddress);
  });

  test("deserializeDelta rejects invalid content", () => {
    const message = deltaMessage("a@b.c", [noOpMessage()]);
    expectInvalid(
      () => deserializeDelta({ ...message, hashedVersion: hashedVersion(-1, bytes()) }),
      "version",
    );
    expectMissing(() => deserializeDelta(without(message, "hashedVersion")), "hashed_version");
    expectMissing(() => deserializeDelta(without(message, "author")), "author");
    expectKind(() => deserializeDelta(deltaMessage("a@b.c", [{}])), "unsupported");
  });

  test("deserializeTransformedDelta rejects empty delta", () => {
    expectInvalid(
      () =>
        deserializeTransformedDelta(deltaMessage("a@b.c", []), HashedVersion.of(4, bytes(9)), 1000),
      "ProtocolWaveletDelta.operation",
    );
  });

  test("deserializeTransformedDelta rejects invalid author address", () => {
    for (const author of ["nobody", "a@", "a@b@c", ""]) {
      expectKind(
        () =>
          deserializeTransformedDelta(
            deltaMessage(author, [noOpMessage()]),
            HashedVersion.of(4, bytes(9)),
            1000,
          ),
        "participantId",
      );
    }
  });

  test("deserializeTransformedDelta sets contexts and ignores message version", () => {
    const message = deltaMessage("a@b.c", [noOpMessage(), addParticipantMessage("x@y.z")]);
    const resultingVersion = HashedVersion.of(10, bytes(9));
    const delta = deserializeTransformedDelta(message, resultingVersion, 1000);
    expect(delta.author.address).toBe("a@b.c");
    expect(delta.resultingVersion.equals(resultingVersion)).toBe(true);
    expect(delta.appliedAtVersion).toBe(8);
    expect(delta.applicationTimestamp).toBe(1000);
    const [first, second] = delta.ops;
    expect(first!.context.equals(new WaveletOperationContext(pid("a@b.c"), 1000, 1))).toBe(true);
    expect(first!.context.hashedVersion).toBe(null);
    expect(second!.context.timestamp).toBe(1000);
    expect(second!.context.hashedVersion!.equals(resultingVersion)).toBe(true);
  });

  test("deserializeTransformedDelta rejects resulting version below operation count", () => {
    const error = expectKind(
      () =>
        deserializeTransformedDelta(
          deltaMessage("a@b.c", [noOpMessage(), noOpMessage()]),
          HashedVersion.of(1, bytes(9)),
          1000,
        ),
      "delta",
    );
    expect(error.cause).toBeInstanceOf(DeltaError);
  });

  const documentMessage = (): DocumentSnapshot => ({
    documentId: "b+1",
    documentOperation: docOpMessage([characters("hi")]),
    author: "a@example.com",
    contributor: ["b@example.com"],
    lastModifiedVersion: 3n,
    lastModifiedTime: 4n,
  });

  const snapshotMessage = (document: DocumentSnapshot[] = [documentMessage()]): WaveletSnapshot => ({
    waveletId: "example.com/conv+root",
    participantId: ["a@example.com", "b@example.com"],
    document,
    version: hashedVersion(3, bytes(7)),
    lastModifiedTime: 4n,
    creator: "a@example.com",
    creationTime: 2n,
  });

  const waveId = (): WaveId => WaveId.of("example.com", "w+abc");

  test("deserializeWavelet accepts valid snapshot", () => {
    const wavelet = deserializeWavelet(snapshotMessage(), waveId());
    expect(wavelet.participants.map(String)).toEqual(["a@example.com", "b@example.com"]);
    expect(wavelet.documentIds()).toEqual(["b+1"]);
    expect(wavelet.version).toBe(3);
    expect(wavelet.hashedVersion.equals(HashedVersion.of(3, bytes(7)))).toBe(true);
    expect(wavelet.creationTime).toBe(2);
    expect(wavelet.lastModifiedTime).toBe(4);
  });

  test("deserializeWavelet rejects invalid participant addresses", () => {
    const base = snapshotMessage();
    expectKind(
      () =>
        deserializeWavelet(
          { ...base, participantId: [...base.participantId, "nobody"] },
          waveId(),
        ),
      "participantId",
    );
    expectKind(() => deserializeWavelet({ ...base, creator: "a@" }, waveId()), "participantId");
    expectKind(
      () =>
        deserializeWavelet(snapshotMessage([{ ...documentMessage(), author: "a@b@c" }]), waveId()),
      "participantId",
    );
    expectKind(
      () =>
        deserializeWavelet(
          snapshotMessage([{ ...documentMessage(), contributor: ["b@example.com", ""] }]),
          waveId(),
        ),
      "participantId",
    );
  });

  test("deserializeWavelet rejects negative last modified version", () => {
    expectInvalid(
      () =>
        deserializeWavelet(
          snapshotMessage([{ ...documentMessage(), lastModifiedVersion: -1n }]),
          waveId(),
        ),
      "DocumentSnapshot.last_modified_version",
    );
  });

  test("deserializeWavelet rejects negative version", () => {
    expectInvalid(
      () =>
        deserializeWavelet({ ...snapshotMessage(), version: hashedVersion(-1, bytes(7)) }, waveId()),
      "ProtocolHashedVersion.version",
    );
  });

  test("deserializeWavelet rejects invalid wavelet id", () => {
    for (const id of ["", "conv+root", "example.com/conv+root/x", "example.com/"]) {
      const error = expectKind(
        () => deserializeWavelet({ ...snapshotMessage(), waveletId: id }, waveId()),
        "id",
      );
      expect(error.cause).toBeInstanceOf(InvalidIdError);
    }
  });

  test("deserializeWavelet rejects invalid documents", () => {
    // Content that is not an initialization.
    expectKind(
      () =>
        deserializeWavelet(
          snapshotMessage([{ ...documentMessage(), documentOperation: docOpMessage([retain(1)]) }]),
          waveId(),
        ),
      "operation",
    );
    // Ill-formed content.
    const error = expectKind(
      () =>
        deserializeWavelet(
          snapshotMessage([
            { ...documentMessage(), documentOperation: docOpMessage([elementStart("p")]) },
          ]),
          waveId(),
        ),
      "operation",
    );
    expect(error.cause).toBeInstanceOf(OperationError);
    // Two documents with the same id.
    const duplicate = expectKind(
      () => deserializeWavelet(snapshotMessage([documentMessage(), documentMessage()]), waveId()),
      "data",
    );
    expect(duplicate.cause).toBeInstanceOf(DataError);
    // A missing required field in a message built in memory.
    expectMissing(
      () =>
        deserializeWavelet(snapshotMessage([without(documentMessage(), "documentId")]), waveId()),
      "DocumentSnapshot.document_id",
    );
  });

  test("deserializeWavelet drops duplicate participants and contributors", () => {
    const base = snapshotMessage([
      { ...documentMessage(), contributor: ["b@example.com", "b@example.com"] },
    ]);
    const wavelet = deserializeWavelet(
      { ...base, participantId: [...base.participantId, "a@example.com"] },
      waveId(),
    );
    expect(wavelet.participants.map(String)).toEqual(["a@example.com", "b@example.com"]);
    expect(wavelet.document("b+1")!.contributors.map(String)).toEqual(["b@example.com"]);
  });

  test("serializeWavelet rejects versions beyond the safe range", () => {
    const wavelet = deserializeWavelet(snapshotMessage(), waveId());
    expectInvalid(
      () => serializeWavelet(wavelet, HashedVersion.unsigned(2 ** 64)),
      "ProtocolHashedVersion.version",
    );
    wavelet.document("b+1")!.setLastModifiedVersion(2 ** 64);
    expectInvalid(
      () => serializeWavelet(wavelet, HashedVersion.unsigned(3)),
      "DocumentSnapshot.last_modified_version",
    );
  });
});

// Round trips through the JSON text.

describe("round trips", () => {
  const DOC_IDS = ["b+1", "b+2", "conversation"] as const;

  /** Random operations that apply in sequence to an empty document. */
  function randomOps(seed: number, count: number): { ops: DocOp[]; doc: BootstrapDocument } {
    const p = defaultParameters();
    const r = RandomProviderImpl.ofSeed(seed);
    const doc = new BootstrapDocument();
    const ops: DocOp[] = [];
    for (let i = 0; i < count; i++) {
      const op = generateRandomOp(r, p, doc);
      doc.consume(op);
      ops.push(op);
    }
    return { ops, doc };
  }

  /** Operations of every serialisable kind by `author`. */
  function randomWaveletOps(seed: number, author: ParticipantId): WaveletOperation[] {
    const context = new WaveletOperationContext(author, NO_TIMESTAMP, 1);
    return [
      WaveletOperation.addParticipant(context, pid("x@example.com")),
      WaveletOperation.noOp(context),
      ...randomOps(seed, 4).ops.map((op, i) =>
        WaveletOperation.blipContent(context, DOC_IDS[i % DOC_IDS.length]!, op),
      ),
      WaveletOperation.removeParticipant(context, pid("x@example.com")),
    ];
  }

  function assertSameContexts(
    a: readonly WaveletOperation[],
    b: readonly WaveletOperation[],
  ): void {
    expect(a.length).toBe(b.length);
    a.forEach((op, i) => {
      const other = b[i]!.context;
      expect(op.context.equals(other)).toBe(true);
      expect(op.context.hashedVersion === null).toBe(other.hashedVersion === null);
      if (op.context.hashedVersion !== null && other.hashedVersion !== null) {
        expect(op.context.hashedVersion.equals(other.hashedVersion)).toBe(true);
      }
    });
  }

  test("random doc ops survive round trip", () => {
    let components = 0;
    for (let seed = 0; seed < 50; seed++) {
      for (const op of randomOps(seed, 6).ops) {
        const message = serializeDocOp(op);
        expect(message.component.length).toBe(op.length);
        const decoded = viaJson(ProtocolDocumentOperation, message);
        expect(decoded).toEqual(message);
        expect(deserializeDocOp(decoded).equals(op), `seed ${seed}`).toBe(true);
        components += op.length;
      }
    }
    // The generator produced something to convert.
    expect(components).toBeGreaterThan(500);
  });

  test("random doc ops with supplementary characters survive round trip", () => {
    const p = { ...defaultParameters(), supplementaryCharacters: true };
    for (let seed = 0; seed < 20; seed++) {
      const r = RandomProviderImpl.ofSeed(seed);
      const doc = new BootstrapDocument();
      for (let i = 0; i < 6; i++) {
        const op = generateRandomOp(r, p, doc);
        doc.consume(op);
        const decoded = viaJson(ProtocolDocumentOperation, serializeDocOp(op));
        expect(deserializeDocOp(decoded).equals(op), `seed ${seed}`).toBe(true);
      }
    }
  });

  test("deltas survive round trip", () => {
    for (let seed = 0; seed < 20; seed++) {
      const author = pid("author@example.com");
      const delta = new WaveletDelta(
        author,
        HashedVersion.of(seed * 1000, new Uint8Array(20).fill(seed)),
        randomWaveletOps(seed, author),
      );
      const message = serializeDelta(delta);
      const decoded = viaJson(ProtocolWaveletDelta, message);
      expect(decoded).toEqual(message);
      const parsed = deserializeDelta(decoded);
      expect(parsed.equals(delta), `seed ${seed}`).toBe(true);
      assertSameContexts(parsed.ops, delta.ops);
    }
  });

  test("empty delta survives round trip", () => {
    const delta = new WaveletDelta(pid("a@example.com"), HashedVersion.unsigned(0), []);
    const decoded = viaJson(ProtocolWaveletDelta, serializeDelta(delta));
    expect(deserializeDelta(decoded).equals(delta)).toBe(true);
  });

  test("transformed deltas survive round trip", () => {
    for (let seed = 0; seed < 20; seed++) {
      const author = pid("author@example.com");
      const ops = randomWaveletOps(seed, author);
      const resultingVersion = HashedVersion.of(
        seed * 1000 + ops.length,
        new Uint8Array(20).fill(seed),
      );
      const timestamp = 1_000_000 + seed;
      const delta = TransformedWaveletDelta.cloneOperations(
        author,
        resultingVersion,
        timestamp,
        ops,
      );
      const message = serializeTransformedDelta(delta);
      // The message carries the applied-at version without a hash.
      expect(message.hashedVersion).toEqual(hashedVersion(seed * 1000, bytes()));
      const parsed = deserializeTransformedDelta(
        viaJson(ProtocolWaveletDelta, message),
        resultingVersion,
        timestamp,
      );
      expect(parsed.equals(delta), `seed ${seed}`).toBe(true);
      assertSameContexts(parsed.ops, delta.ops);
    }
  });

  test("wavelet snapshots survive round trip", () => {
    for (let seed = 0; seed < 20; seed++) {
      const name = WaveletName.of(
        WaveId.of("example.com", "w+abc123"),
        WaveletId.of("other.example.com", "conv+root"),
      );
      const version = HashedVersion.of(seed + 17, new Uint8Array(20).fill(seed));
      const creator = pid("creator@example.com");
      const wavelet = WaveletData.empty(name, creator, version, 1234);
      wavelet.addParticipant(pid("b@example.com"));
      wavelet.addParticipant(creator);
      wavelet.addParticipant(pid("a@example.com"));
      // Created out of id order.
      [...DOC_IDS].reverse().forEach((docId, i) => {
        const { doc } = randomOps(seed * 10 + i, 3);
        wavelet.createDocument(
          docId,
          pid("a@example.com"),
          [pid("b@example.com"), creator],
          doc.asOperation(),
          5000 + i,
          seed + i,
        );
      });
      wavelet.setLastModifiedTime(9999);

      const message = serializeWavelet(wavelet, version);
      expect(message.document.map((d) => d.documentId)).toEqual([...DOC_IDS]);
      const text = WaveletSnapshot.encode(message);
      const decoded = WaveletSnapshot.decode(parseJson(text));
      expect(decoded).toEqual(message);
      const parsed = deserializeWavelet(decoded, name.waveId);
      expect(parsed.equals(wavelet), `seed ${seed}`).toBe(true);
      // Serialising again gives the same text.
      expect(WaveletSnapshot.encode(serializeWavelet(parsed, version))).toBe(text);
    }
  });

  test("serialized wavelet carries given version", () => {
    // The snapshot version is the argument, not the wavelet's own.
    const name = WaveletName.of(
      WaveId.of("example.com", "w+abc123"),
      WaveletId.of("example.com", "conv+root"),
    );
    const wavelet = WaveletData.empty(name, pid("a@example.com"), HashedVersion.unsigned(0), 7);
    const version = HashedVersion.of(42, bytes(1, 2, 3));
    const message = serializeWavelet(wavelet, version);
    expect(message.version).toEqual(hashedVersion(42, bytes(1, 2, 3)));
    expect(message.waveletId).toBe("example.com/conv+root");
    const parsed = deserializeWavelet(message, name.waveId);
    expect(parsed.version).toBe(42);
    expect(parsed.hashedVersion.equals(version)).toBe(true);
    expect(parsed.creationTime).toBe(7);
    expect(parsed.lastModifiedTime).toBe(7);
  });

  test("serialized document carries its metadata", () => {
    const name = WaveletName.of(
      WaveId.of("example.com", "w+abc123"),
      WaveletId.of("example.com", "conv+root"),
    );
    const wavelet = WaveletData.empty(name, pid("a@example.com"), HashedVersion.unsigned(0), 7);
    const blip = wavelet.createDocument(
      "b+1",
      pid("a@example.com"),
      [pid("c@example.com")],
      new DocOpBuilder().characters("hi").buildInitialization(),
      11,
      5,
    );
    expect(serializeDocument(blip)).toEqual({
      documentId: "b+1",
      documentOperation: docOpMessage([characters("hi")]),
      author: "a@example.com",
      contributor: ["c@example.com"],
      lastModifiedVersion: 5n,
      lastModifiedTime: 11n,
    });
  });
});

// The messages of the Java-generated golden file: JSON -> message -> model
// -> message -> JSON reproduces the text wherever the model can hold the
// message.

describe("json_golden.txt", () => {
  const GOLDEN_PATH = fileURLToPath(
    new URL("../../../../crates/resonator-model/testdata/json_golden.txt", import.meta.url),
  );

  /** The JSON texts of the `M` records of a message type. */
  function records(name: string): string[] {
    const out: string[] = [];
    for (const line of readFileSync(GOLDEN_PATH, "utf8").split("\n")) {
      const fields = line.split("\t");
      if (fields[0] === "M" && fields[1] === name && fields[3] !== undefined) {
        // Undo the escaping of `JsonGoldenGenerator.line`.
        out.push(
          fields[3].replace(/\\u([0-9A-Fa-f]{4})/g, (_, hex: string) =>
            String.fromCharCode(Number.parseInt(hex, 16)),
          ),
        );
      }
    }
    return out;
  }

  interface Tally {
    exact: number;
    lossy: string[];
    rejected: string[];
  }

  /** Converts every message there and back and sorts the outcomes. */
  function tally<T>(
    codec: MessageCodec<T>,
    messages: readonly T[],
    there: (message: T) => T,
  ): Tally {
    const result: Tally = { exact: 0, lossy: [], rejected: [] };
    for (const message of messages) {
      const text = codec.encode(message);
      let back: string;
      try {
        back = codec.encode(there(message));
      } catch (error) {
        if (!(error instanceof ProtoError)) {
          throw error;
        }
        result.rejected.push(`${error.kind}: ${error.message}`);
        continue;
      }
      if (back === text) {
        result.exact += 1;
      } else {
        result.lossy.push(text);
      }
    }
    return result;
  }

  function decodeAll<T>(codec: MessageCodec<T>): T[] {
    return records(codec.name).map((text) => {
      const message = codec.decode(parseJson(text));
      // The codec itself reproduces the golden text.
      expect(codec.encode(message)).toBe(text);
      return message;
    });
  }

  const updates = (): ProtocolWaveletUpdate[] => decodeAll(ProtocolWaveletUpdate);

  /** Every rejection is of the sample operation that has one component of
   * each kind, which is not a well-formed operation. */
  function expectIllFormedSample(rejected: readonly string[]): void {
    for (const reason of rejected) {
      expect(reason.startsWith("operation: ill-formed operation"), reason).toBe(true);
      expect(reason).toContain("adjacent annotation boundaries");
    }
  }

  const counts = (result: Tally): { exact: number; lossy: number; rejected: number } => ({
    exact: result.exact,
    lossy: result.lossy.length,
    rejected: result.rejected.length,
  });

  test("document operations", () => {
    const messages = [
      ...decodeAll(ProtocolDocumentOperation),
      ...decodeAll(ProtocolWaveletOperation).flatMap((op) =>
        op.mutateDocument ? [op.mutateDocument.documentOperation] : [],
      ),
    ];
    const result = tally(ProtocolDocumentOperation, messages, (m) =>
      serializeDocOp(deserializeDocOp(m)),
    );
    expect(messages.length).toBe(27);
    expect(counts(result)).toEqual({ exact: 25, lossy: 0, rejected: 2 });
    expectIllFormedSample(result.rejected);
  });

  test("wavelet operations", () => {
    const messages = decodeAll(ProtocolWaveletOperation);
    const result = tally(ProtocolWaveletOperation, messages, (m) =>
      serializeOp(deserializeOp(m, opContext())),
    );
    expect(messages.length).toBe(28);
    expect(counts(result)).toEqual({ exact: 27, lossy: 0, rejected: 1 });
    expectIllFormedSample(result.rejected);
  });

  test("wavelet deltas", () => {
    const messages = [
      ...decodeAll(ProtocolWaveletDelta),
      ...decodeAll(ProtocolSubmitRequest).map((request) => request.delta),
      ...updates().flatMap((update) => update.appliedDelta),
    ];
    const result = tally(ProtocolWaveletDelta, messages, (m) =>
      serializeDelta(deserializeDelta(m)),
    );
    expect(messages.length).toBe(5);
    expect(counts(result)).toEqual({ exact: 2, lossy: 0, rejected: 3 });
    expectIllFormedSample(result.rejected);

    // As transformed deltas the text cannot come back: the message then
    // carries the applied-at version without the hash.
    const transformed = tally(ProtocolWaveletDelta, messages, (m) => {
      const resulting = HashedVersion.of(
        m.hashedVersion.version + m.operation.length,
        bytes(1, 2, 3),
      );
      const delta = deserializeTransformedDelta(m, resulting, 1234);
      expect(delta.appliedAtVersion).toBe(m.hashedVersion.version);
      const back = serializeTransformedDelta(delta);
      expect({ ...back, hashedVersion: m.hashedVersion }).toEqual(m);
      return back;
    });
    expect(counts(transformed)).toEqual({ exact: 0, lossy: 2, rejected: 3 });
    expectIllFormedSample(transformed.rejected);
  });

  test("wavelet snapshots", () => {
    const waveId = WaveId.of("example.com", "w+abc");
    const messages = [
      ...decodeAll(WaveletSnapshot),
      ...decodeAll(WaveViewSnapshot).flatMap((view) => view.wavelet),
      ...updates().flatMap((update) => (update.snapshot ? [update.snapshot] : [])),
    ];
    const result = tally(WaveletSnapshot, messages, (m) => {
      const wavelet = deserializeWavelet(m, waveId);
      return serializeWavelet(wavelet, wavelet.hashedVersion);
    });
    expect(messages.length).toBe(15);
    expect(counts(result)).toEqual({ exact: 13, lossy: 0, rejected: 2 });
    // The two records with timestamps at the ends of the int64 range,
    // which the model's numbers cannot hold.
    for (const reason of result.rejected) {
      expect(reason).toContain("exceeds the safe integer range");
    }
  });
});
