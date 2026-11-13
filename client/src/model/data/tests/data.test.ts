// Ports `WaveletDataTestBase` and `BlipDataTestBase` (as instantiated by
// `WaveletDataWithWaveletDataImplTest` and `BlipDataWithBlipDataImplTest`)
// of Apache Wave. Listener assertions and the disabled `testSetTitle` are
// not ported.

import { describe, expect, test } from "vitest";

import {
  Attributes,
  BootstrapDocument,
  type DocInitialization,
  DocOpBuilder,
  toXmlString,
} from "../../docop";
import { ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import { HashedVersion } from "../../version";
import { type BlipData, DataError, WaveletData } from "..";

function pid(address: string): ParticipantId {
  return ParticipantId.of(address);
}

/**
 * `WaveletDataFactory.create`: an empty wavelet at version zero and time
 * zero created by `fake@example.com`.
 */
function createWaveletData(): WaveletData {
  return new WaveletData(
    WaveletId.of("example.com", "conv+root"),
    pid("fake@example.com"),
    0,
    0,
    HashedVersion.unsigned(0),
    0,
    WaveId.of("example.com", "w+1"),
  );
}

/** `ModelTestUtils.createContent`. */
function createContent(text: string): DocInitialization {
  const builder = new DocOpBuilder()
    .elementStart("body", Attributes.EMPTY)
    .elementStart("line", Attributes.EMPTY)
    .elementEnd();
  if (text.length > 0) {
    builder.characters(text);
  }
  return builder.elementEnd().buildInitialization();
}

function addresses(participants: readonly ParticipantId[]): string[] {
  return participants.map((p) => p.address);
}

function dataError(f: () => unknown): DataError {
  try {
    f();
  } catch (e) {
    expect(e).toBeInstanceOf(DataError);
    return e as DataError;
  }
  throw new Error("expected a DataError");
}

const FRED = pid("fred@gwave.com");
const JANE = pid("jane@gwave.com");

describe("WaveletDataTestBase", () => {
  test("new wave has creator", () => {
    const target = createWaveletData();
    expect(target.creator.equals(pid("fake@example.com"))).toBe(true);
  });

  test("new wave has no participants", () => {
    // The creator is not automatically a participant.
    const target = createWaveletData();
    expect(target.participants.length).toBe(0);
  });

  test("new wave has no documents", () => {
    const target = createWaveletData();
    expect(target.documentIds().length).toBe(0);
    expect(target.documents().length).toBe(0);
  });

  test("create blip", () => {
    const target = createWaveletData();
    const creator = target.creator;
    target.createDocument("root", creator, [creator], createContent("test"), 42, 42);

    // The root blip is retrievable by id.
    const blip = target.document("root")!;
    expect(target.documentIds()).toEqual(["root"]);

    expect(blip.id).toBe("root");
    expect(blip.author.equals(target.creator)).toBe(true);
    expect(toXmlString(blip.content.asOperation())).toBe("<body><line/>test</body>");
    expect(blip.lastModifiedTime).toBe(42);
  });

  test("can not create multiple blips with same id", () => {
    const target = createWaveletData();
    const creator = target.creator;
    target.createDocument("DOCUMENT_NAME", creator, [creator], createContent("test"), 42, 42);
    const e = dataError(() =>
      target.createDocument("DOCUMENT_NAME", creator, [creator], createContent("test"), 42, 42),
    );
    expect(e.detail).toEqual({ kind: "duplicateDocument", docId: "DOCUMENT_NAME" });
    expect(e.message).toBe("Duplicate doc id: DOCUMENT_NAME");
    expect(target.documents().length).toBe(1);
  });

  test("get non existent blip fails", () => {
    const target = createWaveletData();
    expect(target.document("not a real blip id")).toBeNull();
  });

  test("add participant", () => {
    const target = createWaveletData();
    expect(target.addParticipant(FRED)).toBe(true);
    expect(target.addParticipant(JANE)).toBe(true);
  });

  test("add participant at position", () => {
    const target = createWaveletData();
    expect(target.addParticipantAt(FRED, 0)).toBe(true);
    expect(addresses(target.participants)).toEqual([FRED.address]);
    expect(target.addParticipantAt(FRED, 0)).toBe(false);
    expect(target.addParticipantAt(JANE, 0)).toBe(true);
    expect(addresses(target.participants)).toEqual([JANE.address, FRED.address]);
    expect(target.addParticipantAt(JANE, 0)).toBe(false);
  });

  test("add participant at position past end", () => {
    const target = createWaveletData();
    expect(dataError(() => target.addParticipantAt(FRED, 1)).detail).toEqual({
      kind: "positionOutOfBounds",
      position: 1,
      size: 0,
    });
    expect(target.addParticipantAt(FRED, 0)).toBe(true);
    expect(dataError(() => target.addParticipantAt(JANE, 2)).detail).toEqual({
      kind: "positionOutOfBounds",
      position: 2,
      size: 1,
    });
    expect(addresses(target.participants)).toEqual([FRED.address]);
  });

  test("remove participant", () => {
    const target = createWaveletData();
    target.addParticipant(FRED);
    target.addParticipant(JANE);
    expect(target.removeParticipant(FRED)).toBe(true);
    expect(addresses(target.participants)).toEqual([JANE.address]);
  });

  test("add duplicate participant returns false", () => {
    const target = createWaveletData();
    target.addParticipant(FRED);
    expect(target.addParticipant(pid("fred@gwave.com"))).toBe(false);
    expect(addresses(target.participants)).toEqual([FRED.address]);
  });

  test("remove invalid participant returns false", () => {
    const target = createWaveletData();
    target.addParticipant(FRED);
    expect(target.removeParticipant(JANE)).toBe(false);
    expect(addresses(target.participants)).toEqual([FRED.address]);
  });

  test("add participants retrieved in order", () => {
    const target = createWaveletData();
    target.addParticipant(FRED);
    target.addParticipant(JANE);
    expect(addresses(target.participants)).toEqual([FRED.address, JANE.address]);
  });

  test("removal of multiply added participant", () => {
    const target = createWaveletData();
    target.addParticipant(FRED);
    target.addParticipant(FRED);
    target.addParticipant(JANE);
    expect(target.removeParticipant(FRED)).toBe(true);
    expect(addresses(target.participants)).toEqual([JANE.address]);
  });

  test("set version", () => {
    const target = createWaveletData();
    expect(target.setVersion(1234)).toBe(0);
    expect(target.version).toBe(1234);
  });

  test("set last modified time", () => {
    const target = createWaveletData();
    expect(target.setLastModifiedTime(4321)).toBe(0);
    expect(target.lastModifiedTime).toBe(4321);
  });
});

describe("BlipDataTestBase", () => {
  /**
   * `BlipDataWithBlipDataImplTest.createBlipData`: an empty document `root`
   * without contributors in a new wavelet.
   */
  function createBlipData(): WaveletData {
    const wavelet = createWaveletData();
    wavelet.createDocument(
      "root",
      wavelet.creator,
      wavelet.participants,
      new DocOpBuilder().buildInitialization(),
      42,
      42,
    );
    return wavelet;
  }

  const target = (wavelet: WaveletData): BlipData => wavelet.document("root")!;

  test("get wave", () => {
    // A blip has no back reference; it is reachable from its wavelet.
    const wavelet = createBlipData();
    const blip = target(wavelet).clone();
    expect(wavelet.document(blip.id)!.equals(blip)).toBe(true);
  });

  test("blip has id", () => {
    expect(target(createBlipData()).id).toBe("root");
  });

  test("blip author", () => {
    expect(target(createBlipData()).author.equals(pid("fake@example.com"))).toBe(true);
  });

  test("new blip has no contributors", () => {
    expect(target(createBlipData()).contributors.length).toBe(0);
  });

  test("add contributors retrieved in order", () => {
    const blip = target(createBlipData());
    blip.addContributor(FRED);
    blip.addContributor(JANE);
    expect(addresses(blip.contributors)).toEqual([FRED.address, JANE.address]);
  });

  test("remove contributors", () => {
    const blip = target(createBlipData());
    blip.addContributor(FRED);
    // Added twice, should be completely removed.
    blip.addContributor(FRED);
    blip.addContributor(JANE);

    blip.removeContributor(FRED);
    expect(addresses(blip.contributors)).toEqual([JANE.address]);
    blip.removeContributor(JANE);
    expect(blip.contributors.length).toBe(0);
  });

  test("remove invalid contributor ignored", () => {
    const blip = target(createBlipData());
    blip.addContributor(FRED);
    blip.removeContributor(JANE);
    expect(addresses(blip.contributors)).toEqual([FRED.address]);
  });

  test("set last modified time", () => {
    const blip = target(createBlipData());
    const lmt = blip.lastModifiedTime + 42;
    expect(blip.setLastModifiedTime(lmt)).toBe(42);
    expect(blip.lastModifiedTime).toBe(lmt);
  });

  test("last modified version", () => {
    const blip = target(createBlipData());
    const lmv = blip.lastModifiedVersion + 23;
    expect(blip.setLastModifiedVersion(lmv)).toBe(42);
    expect(blip.lastModifiedVersion).toBe(lmv);
  });

  test("empty blip has content", () => {
    const blip = target(createBlipData());
    expect(blip.content.asOperation().isEmpty()).toBe(true);
    expect(blip.content.equals(new BootstrapDocument())).toBe(true);
  });
});

// Not in the original.
describe("WaveletData", () => {
  function sample(): WaveletData {
    const wavelet = WaveletData.empty(
      WaveletName.of(WaveId.of("example.com", "w+1"), WaveletId.of("example.com", "conv+root")),
      FRED,
      HashedVersion.of(3, Uint8Array.of(1, 2)),
      77,
    );
    wavelet.addParticipant(FRED);
    wavelet.createDocument("b+2", FRED, [FRED, FRED, JANE], createContent("two"), 5, 2);
    wavelet.createDocument("b+1", JANE, [], createContent("one"), 6, 3);
    return wavelet;
  }

  test("empty takes version and times from its arguments", () => {
    const wavelet = sample();
    expect(wavelet.version).toBe(3);
    expect(wavelet.hashedVersion.equals(HashedVersion.of(3, Uint8Array.of(1, 2)))).toBe(true);
    expect(wavelet.creationTime).toBe(77);
    expect(wavelet.lastModifiedTime).toBe(77);
    expect(wavelet.waveletName.toString()).toBe(
      WaveletName.ofParts("example.com", "w+1", "example.com", "conv+root").toString(),
    );
    // Duplicate contributors are dropped.
    expect(addresses(wavelet.document("b+2")!.contributors)).toEqual([FRED.address, JANE.address]);
  });

  test("documents are ordered by id in UTF-16 order", () => {
    const wavelet = createWaveletData();
    // U+FF5E sorts after the surrogates of U+10000 in UTF-16 order (and
    // before U+10000 in code point order).
    const ids = ["b+2", "～", "b+10", "\u{10000}", "a", "conv", "B"];
    for (const id of ids) {
      wavelet.createDocument(id, FRED, [], createContent(""), 0, 0);
    }
    const expected = ["B", "a", "b+10", "b+2", "conv", "\u{10000}", "～"];
    expect(wavelet.documentIds()).toEqual(expected);
    expect(wavelet.documents().map((d) => d.id)).toEqual(expected);
    expect(wavelet.hasDocument("conv")).toBe(true);
    expect(wavelet.hasDocument("nope")).toBe(false);
  });

  test("invalid content is rejected and creates nothing", () => {
    const wavelet = createWaveletData();
    const unbalanced = new DocOpBuilder().elementStart("p", Attributes.EMPTY).buildUnchecked();
    const e = dataError(() =>
      wavelet.createDocument("b", FRED, [], unbalanced as DocInitialization, 0, 0),
    );
    expect(e.kind).toBe("content");
    expect(wavelet.documentIds()).toEqual([]);
  });

  test("clone is deep and equal", () => {
    const wavelet = sample();
    const copy = wavelet.clone();
    expect(copy.equals(wavelet)).toBe(true);
    expect(wavelet.equals(copy)).toBe(true);
    expect(copy.toString()).toBe(wavelet.toString());
    expect(copy.documentIds()).toEqual(["b+1", "b+2"]);

    copy.document("b+1")!.content.consume(new DocOpBuilder().retain(6).characters("x").retain(1).build());
    expect(copy.equals(wavelet)).toBe(false);
    expect(wavelet.document("b+1")!.content.toXmlString()).toBe("<body><line/>one</body>");

    const second = wavelet.clone();
    second.addParticipant(JANE);
    second.document("b+2")!.addContributor(pid("x@gwave.com"));
    second.createDocument("b+3", FRED, [], createContent(""), 0, 0);
    second.setVersion(9);
    expect(addresses(wavelet.participants)).toEqual([FRED.address]);
    expect(addresses(wavelet.document("b+2")!.contributors)).toEqual([FRED.address, JANE.address]);
    expect(wavelet.documentIds()).toEqual(["b+1", "b+2"]);
    expect(wavelet.version).toBe(3);
    expect(wavelet.equals(sample())).toBe(true);
  });

  test("equality compares every field", () => {
    const variants: ((w: WaveletData) => void)[] = [
      (w) => w.setVersion(4),
      (w) => w.setHashedVersion(HashedVersion.unsigned(3)),
      (w) => w.setLastModifiedTime(78),
      (w) => w.addParticipant(JANE),
      (w) => w.document("b+1")!.setLastModifiedTime(7),
      (w) => w.document("b+1")!.setLastModifiedVersion(7),
      (w) => w.document("b+1")!.addContributor(FRED),
      (w) => w.createDocument("b+0", FRED, [], createContent(""), 0, 0),
    ];
    for (const change of variants) {
      const changed = sample();
      change(changed);
      expect(changed.equals(sample())).toBe(false);
      expect(sample().equals(changed)).toBe(false);
    }
  });

  test("check access permission", () => {
    const wavelet = sample();
    const shared = ParticipantId.ofUnsafe("@gwave.com");
    expect(wavelet.checkAccessPermission(FRED)).toBe(true);
    expect(wavelet.checkAccessPermission(JANE)).toBe(false);
    expect(wavelet.checkAccessPermission(JANE, shared)).toBe(false);
    wavelet.addParticipant(shared);
    expect(wavelet.checkAccessPermission(JANE, shared)).toBe(true);
    expect(wavelet.checkAccessPermission(JANE, null)).toBe(false);
  });

  test("string forms", () => {
    const wavelet = sample();
    wavelet.addParticipant(JANE);
    const blip1 =
      "Blip state = [id:b+1] [author: jane@gwave.com] [contributors: []] " +
      "[lastModifiedVersion:3] [lastModifiedTime:6]";
    const blip2 =
      "Blip state = [id:b+2] [author: fred@gwave.com] " +
      "[contributors: [fred@gwave.com, jane@gwave.com]] " +
      "[lastModifiedVersion:2] [lastModifiedTime:5]";
    expect(wavelet.document("b+1")!.toString()).toBe(blip1);
    expect(wavelet.document("b+2")!.toString()).toBe(blip2);
    expect(wavelet.toString()).toBe(
      `WaveletDataImpl: ${wavelet.waveId}/${wavelet.waveletId} \n[version:3] ` +
        "\n[creator: fred@gwave.com] \n[participants: [fred@gwave.com, jane@gwave.com]] " +
        "\n[creation time: 77] \n[lastModifiedTime:77]\n[documents:" +
        ` \n  [b+1: ${blip1}] \n  [b+2: ${blip2}] \n]`,
    );
  });
});
