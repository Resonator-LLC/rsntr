// Ports of the Rust tests of `resonator-model` (`supplement/tests.rs`),
// themselves ports of the tests in `org.waveprotocol.wave.model.supplement`
// of Apache Wave (`PrimitiveSupplementTestBase` with both of its
// implementations, `WaveletBasedSupplementTest`,
// `GadgetStateCollectionTest`, `SimpleWantedEvaluationSetTest`,
// `SupplementedWaveImplTest`, `LiveSupplementedWaveImplAbuseTest`; Apache
// License 2.0), without the listener and event tests, and tests of what
// the port adds.
//
// Where the original tests build a conversation with the conversation
// model, these write the manifest and blip documents directly.

import { describe, expect, it } from "vitest";
import { WaveletData } from "../../data";
import { XmlFragment } from "../../doc";
import { ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import { HashedVersion } from "../../version";
import { OpBasedWavelet } from "../../wavelet";
import {
  ABUSE_DOCUMENT,
  ALL_FOLDER,
  ARCHIVING_DOCUMENT,
  FOLDERS_DOCUMENT,
  GADGETS_DOCUMENT,
  INBOX_FOLDER,
  NO_VERSION,
  PRESENTATION_DOCUMENT,
  PartitioningPrimitiveSupplement,
  PrimitiveSupplementImpl,
  READSTATE_DOCUMENT,
  SEEN_DOCUMENT,
  SupplementError,
  SupplementedWaveImpl,
  THREAD_STATE,
  TRASH_FOLDER,
  ThreadState,
  UserDataSupplement,
  WantedEvaluation,
  WantedEvaluationSet,
  WaveletBasedSupplement,
  WaveletDataView,
  isFollowedByDefault,
  type InboxState,
  type PrimitiveSupplement,
  type ReadablePrimitiveSupplement,
  type SupplementWaveView,
} from "..";

const TIME = 1234567890123;

const viewer = (): ParticipantId => ParticipantId.of("nobody@nowhere.com");
const wid = (id: string): WaveletId => WaveletId.of("example.com", id);
const hash = (...bytes: number[]): Uint8Array => Uint8Array.from(bytes);
const idsOf = (ids: WaveletId[]): string[] => ids.map((id) => id.id);

/**
 * An empty wavelet of the viewer, whose operations each raise its version
 * by one, as in the original's test wave model.
 */
function wavelet(id: string): OpBasedWavelet {
  const name = WaveletName.of(WaveId.of("example.com", "w+wave"), wid(id));
  const data = WaveletData.empty(name, viewer(), HashedVersion.unsigned(0), TIME);
  return new OpBasedWavelet(data, viewer(), { timestamp: TIME, bumpVersions: true });
}

const userData = (): OpBasedWavelet => wavelet("user+nobody@nowhere.com");

function append(target: OpBasedWavelet, doc: string, xml: string): void {
  const fragment = XmlFragment.parse(xml);
  target.editDocument(doc, (d) => void d.insertXml(d.size(), fragment));
}

const xml = (target: OpBasedWavelet, doc: string): string => target.document(doc).toXmlString();

// ---- PrimitiveSupplementTestBase ----

const primitiveTests: Record<string, (target: PrimitiveSupplement) => void> = {
  defaults(target) {
    const wavelet1 = wid("wavelet1");
    expect(target.lastReadBlipVersion(wavelet1, "blip1")).toBe(NO_VERSION);
    expect(target.lastReadParticipantsVersion(wavelet1)).toBe(NO_VERSION);
    expect(target.lastReadWaveletVersion(wavelet1)).toBe(NO_VERSION);
    expect(target.folders()).toEqual([]);
    expect(target.followed()).toBeNull();
    expect(target.archiveWaveletVersion(wavelet1)).toBe(NO_VERSION);
    expect(target.pendingNotification()).toBe(false);
    expect(target.gadgetState("Gadget #1").size).toBe(0);
  },

  inboxing(target) {
    const [wavelet1, wavelet2] = [wid("wavelet1"), wid("wavelet2")];
    target.archiveAtVersion(wavelet1, 10);
    expect(target.archiveWaveletVersion(wavelet1)).toBe(10);
    target.archiveAtVersion(wavelet2, 5);
    expect(target.archiveWaveletVersion(wavelet2)).toBe(5);
    target.clearArchiveState();
    expect(target.archiveWaveletVersion(wavelet1)).toBe(NO_VERSION);
    expect(target.archiveWaveletVersion(wavelet2)).toBe(NO_VERSION);
  },

  "clear read state"(target) {
    const [wavelet1, wavelet2] = [wid("wavelet1"), wid("wavelet2")];
    target.setLastReadBlipVersion(wavelet1, "blip1", 23);
    target.setLastReadBlipVersion(wavelet2, "blip2", 43);
    target.clearReadState();
    expect(target.lastReadBlipVersion(wavelet1, "blip1")).toBe(NO_VERSION);
    expect(target.lastReadBlipVersion(wavelet2, "blip2")).toBe(NO_VERSION);
    target.setLastReadBlipVersion(wavelet1, "blip1", 34);
    expect(target.lastReadBlipVersion(wavelet1, "blip1")).toBe(34);
    expect(target.lastReadBlipVersion(wavelet2, "blip2")).toBe(NO_VERSION);
  },

  "clear blip read state"(target) {
    const wavelet1 = wid("wavelet1");
    target.setLastReadBlipVersion(wavelet1, "blip1", 23);
    target.setLastReadBlipVersion(wavelet1, "blip2", 43);
    expect(target.lastReadBlipVersion(wavelet1, "blip1")).toBe(23);
    expect(target.lastReadBlipVersion(wavelet1, "blip2")).toBe(43);
    target.clearBlipReadState(wavelet1, "blip1");
    expect(target.lastReadBlipVersion(wavelet1, "blip1")).toBe(NO_VERSION);
    expect(target.lastReadBlipVersion(wavelet1, "blip2")).toBe(43);
    // A lesser version than the one cleared can be set.
    target.setLastReadBlipVersion(wavelet1, "blip1", 10);
    expect(target.lastReadBlipVersion(wavelet1, "blip1")).toBe(10);
  },

  "seen versions"(target) {
    const [wavelet1, wavelet2] = [wid("wavelet1"), wid("wavelet2")];
    const seen = HashedVersion.of(213819238, hash(1));
    target.setSeenVersion(wavelet1, seen);
    expect(target.seenVersion(wavelet1).equals(seen)).toBe(true);
    expect(target.seenVersion(wavelet2).equals(HashedVersion.unsigned(0))).toBe(true);
    expect(idsOf(target.seenWavelets())).toEqual(["wavelet1"]);
  },

  random(target) {
    const { Collapsed, Expanded } = ThreadState;
    const [wavelet1, wavelet2] = [wid("wavelet1"), wid("wavelet2")];
    const [blip1, blip2] = ["blip1", "blip2"];
    const [thread1, thread2] = ["thread1", "thread2"];

    target.setLastReadBlipVersion(wavelet1, blip1, 7);
    expect(target.lastReadBlipVersion(wavelet1, blip1)).toBe(7);
    target.setLastReadBlipVersion(wavelet1, blip1, 5);
    expect(target.lastReadBlipVersion(wavelet1, blip1)).toBe(7);
    target.setLastReadBlipVersion(wavelet1, blip2, 15);
    expect(target.lastReadBlipVersion(wavelet1, blip1)).toBe(7);
    expect(target.lastReadBlipVersion(wavelet1, blip2)).toBe(15);
    target.setLastReadBlipVersion(wavelet2, blip1, 8);
    expect(target.lastReadBlipVersion(wavelet1, blip1)).toBe(7);
    expect(target.lastReadBlipVersion(wavelet1, blip2)).toBe(15);
    expect(target.lastReadBlipVersion(wavelet2, blip1)).toBe(8);
    target.setLastReadParticipantsVersion(wavelet1, 10);
    expect(target.lastReadParticipantsVersion(wavelet1)).toBe(10);
    target.setLastReadWaveletVersion(wavelet2, 5);
    expect(target.lastReadWaveletVersion(wavelet2)).toBe(5);
    target.setLastReadBlipVersion(wavelet2, blip1, 13);
    expect(target.lastReadBlipVersion(wavelet1, blip1)).toBe(7);
    expect(target.lastReadBlipVersion(wavelet1, blip2)).toBe(15);
    expect(target.lastReadBlipVersion(wavelet2, blip1)).toBe(13);

    target.addFolder(3);
    expect(target.folders()).toEqual([3]);
    target.addFolder(5);
    expect(target.folders()).toEqual([3, 5]);
    target.addFolder(5);
    expect(target.folders()).toEqual([3, 5]);
    target.removeAllFolders();
    expect(target.folders()).toEqual([]);
    target.addFolder(5);
    expect(target.folders()).toEqual([5]);

    target.setNotifiedVersion(wavelet1, 10);
    target.setNotifiedVersion(wavelet2, 20);
    expect(idsOf(target.notifiedWavelets())).toEqual(["wavelet1", "wavelet2"]);
    expect(target.notifiedVersion(wavelet1)).toBe(10);
    expect(target.notifiedVersion(wavelet2)).toBe(20);
    expect(target.pendingNotification()).toBe(false);

    target.setSeenVersion(wavelet1, HashedVersion.of(213819238, hash(10)));
    target.setSeenVersion(wavelet2, HashedVersion.of(213819238, hash(20)));
    expect(target.pendingNotification()).toBe(false);

    expect(target.threadState(wavelet1, thread1)).toBeNull();
    expect(target.threadState(wavelet1, thread2)).toBeNull();
    target.setThreadState(wavelet1, thread1, Expanded);
    target.setThreadState(wavelet1, thread1, Collapsed);
    target.setThreadState(wavelet1, thread1, Expanded);
    target.setThreadState(wavelet1, thread2, Collapsed);
    target.setThreadState(wavelet2, thread1, Collapsed);
    target.setThreadState(wavelet2, thread2, Collapsed);
    target.setThreadState(wavelet2, thread2, Expanded);
    expect(target.threadState(wavelet1, thread1)).toBe(Expanded);
    expect(target.threadState(wavelet1, thread2)).toBe(Collapsed);
    expect(target.threadState(wavelet2, thread1)).toBe(Collapsed);
    expect(target.threadState(wavelet2, thread2)).toBe(Expanded);
    expect(target.statefulThreads(wavelet1)).toEqual([thread1, thread2]);
    expect(idsOf(target.waveletsWithThreadState())).toEqual(["wavelet1", "wavelet2"]);

    target.setThreadState(wavelet2, thread1, null);
    expect(target.threadState(wavelet2, thread1)).toBeNull();
    expect(target.threadState(wavelet1, thread1)).toBe(Expanded);
    expect(target.threadState(wavelet2, thread2)).toBe(Expanded);
    target.setThreadState(wavelet1, thread2, null);
    target.setThreadState(wavelet2, thread2, null);
    expect(target.threadState(wavelet1, thread1)).toBe(Expanded);
    expect(target.threadState(wavelet2, thread2)).toBeNull();
    expect(target.threadState(wavelet2, thread1)).toBeNull();
  },

  abuse(target) {
    const evaluation = (
      wanted: boolean,
      certainty: number,
      timestamp: number,
      agent: string,
      ignored: boolean,
    ): WantedEvaluation =>
      new WantedEvaluation({
        waveletId: wid("wavelet1"),
        adderAddress: "evilbob@evil.com",
        wanted,
        certainty: Math.fround(certainty),
        timestamp,
        agentIdentity: agent,
        ignored,
        comment: "no comment",
      });
    const eval1 = evaluation(true, 0.1, 1234, "agent1", false);
    const eval2 = evaluation(true, 0.8, 1256, "agent2", true);
    const eval3 = evaluation(false, 0.5, 1278, "agent1", false);
    const expectAll = (): void => {
      const all = target.wantedEvaluations();
      expect(all.length).toBe(3);
      [eval1, eval2, eval3].forEach((e, i) => expect(all[i]!.equals(e)).toBe(true));
    };
    expect(target.wantedEvaluations()).toEqual([]);
    target.addWantedEvaluation(eval1);
    expect(target.wantedEvaluations().length).toBe(1);
    target.addWantedEvaluation(eval2);
    target.addWantedEvaluation(eval3);
    expectAll();
    target.addWantedEvaluation(eval1);
    expectAll();
  },

  "gadget states"(target) {
    const [gadget1, gadget2] = ["Gadget 1", "Gadget 2"];
    const [state1, state2] = ["State 1", "State 2"];
    expect(target.gadgetState(gadget1).size).toBe(0);
    expect(target.gadgetState(gadget2).size).toBe(0);
    target.setGadgetState(gadget1, state1, "State 1 in gadget 1");
    target.setGadgetState(gadget2, state1, "State 1 in gadget 2");
    target.setGadgetState(gadget1, state2, "State 2 in gadget 1");
    target.setGadgetState(gadget2, state2, "State 2 in gadget 2");
    expect([...target.gadgetState(gadget1)]).toEqual([
      [state1, "State 1 in gadget 1"],
      [state2, "State 2 in gadget 1"],
    ]);
    expect([...target.gadgetState(gadget2)]).toEqual([
      [state1, "State 1 in gadget 2"],
      [state2, "State 2 in gadget 2"],
    ]);
    target.setGadgetState(gadget1, state1, null);
    target.setGadgetState(gadget2, state2, null);
    expect([...target.gadgetState(gadget1)]).toEqual([[state2, "State 2 in gadget 1"]]);
    expect([...target.gadgetState(gadget2)]).toEqual([[state1, "State 1 in gadget 2"]]);
  },
};

describe.each([
  ["in memory", (): PrimitiveSupplement => new PrimitiveSupplementImpl()],
  ["wavelet based", (): PrimitiveSupplement => WaveletBasedSupplement.create(userData())],
  // Through the partitioning wrapper in its pass-through configuration.
  [
    "partitioned",
    (): PrimitiveSupplement =>
      new PartitioningPrimitiveSupplement(new PrimitiveSupplementImpl(), true, true),
  ],
])("primitive supplement, %s", (_name, create) => {
  for (const [name, test] of Object.entries(primitiveTests)) {
    it(name, () => test(create()));
  }
});

// ---- WaveletBasedSupplementTest ----

/** A conversation wavelet with one blip. */
function conversation(id: string): OpBasedWavelet {
  const target = wavelet(id);
  append(target, "conversation", '<conversation><blip id="b+1"></blip></conversation>');
  append(target, "b+1", "<body><line></line></body>");
  return target;
}

describe("wavelet based supplement", () => {
  it("load on domainless wavelet ids does not fail", () => {
    const udw = userData();
    append(udw, READSTATE_DOCUMENT, '<wavelet i="conversation/root"></wavelet>');
    append(udw, ARCHIVING_DOCUMENT, '<archive i="conversation/root"></archive>');
    append(udw, SEEN_DOCUMENT, '<seen i="conversation/root"></seen>');
    udw.takeOps();
    const supplement = WaveletBasedSupplement.create(udw);
    // The id reads as wavelet "root" in the domain "conversation".
    const id = WaveletId.of("conversation", "root");
    expect(supplement.readWavelets().map(String)).toEqual([String(id)]);
    expect(supplement.lastReadWaveletVersion(id)).toBe(NO_VERSION);
    expect(supplement.archiveWaveletVersion(id)).toBe(NO_VERSION);
    expect(supplement.seenVersion(id).equals(HashedVersion.unsigned(0))).toBe(true);
    expect(udw.ops().length).toBe(0);
  });

  it("copy snapshot does not cause writes", () => {
    const udw = userData();
    const root = conversation("conv+root");
    root.addParticipant(viewer());
    const view = new WaveletDataView(viewer(), [root.data()]);
    new SupplementedWaveImpl(WaveletBasedSupplement.create(udw), view, "always").markAsRead();
    const version = udw.data().version;
    expect(udw.takeOps().length).toBe(2);
    const substrate = WaveletBasedSupplement.create(udw);
    const copy = PrimitiveSupplementImpl.copyOf(substrate);
    expect(copy.lastReadWaveletVersion(wid("conv+root"))).toBe(3);
    expect(substrate.snapshot().lastReadWaveletVersion(wid("conv+root"))).toBe(3);
    expect(udw.data().version).toBe(version);
    expect(udw.ops().length).toBe(0);
  });

  // GadgetStateCollectionTest
  it("set gadget state", () => {
    const udw = userData();
    const states = WaveletBasedSupplement.create(udw);
    const [gadget1, gadget2] = ["1", "2"];
    expect(states.gadgetState(gadget1).size).toBe(0);
    expect(states.gadgetState(gadget2).size).toBe(0);
    states.setGadgetState(gadget1, "key1", "value1 in 1");
    expect(states.gadgetState(gadget1).size).toBe(1);
    expect(states.gadgetState(gadget2).size).toBe(0);
    expect(states.gadgetState(gadget1).get("key1")).toBe("value1 in 1");
    states.setGadgetState(gadget2, "key1", "value1 in 2");
    expect(states.gadgetState(gadget1).get("key1")).toBe("value1 in 1");
    expect(states.gadgetState(gadget2).get("key1")).toBe("value1 in 2");
    states.setGadgetState(gadget1, "key1", null);
    states.setGadgetState(gadget2, "key2", "value2 in 2");
    expect(states.gadgetState(gadget1).size).toBe(0);
    expect(states.gadgetState(gadget2).size).toBe(2);
    expect(states.gadgetState(gadget2).get("key1")).toBe("value1 in 2");
    expect(states.gadgetState(gadget2).get("key2")).toBe("value2 in 2");
    expect(xml(udw, GADGETS_DOCUMENT)).toBe(
      '<gadget i="1"/><gadget i="2"><state name="key2" value="value2 in 2"/>' +
        '<state name="key1" value="value1 in 2"/></gadget>',
    );
    expect([...states.gadgetStates().keys()]).toEqual(["1", "2"]);
  });
});

// ---- SimpleWantedEvaluationSetTest, LiveSupplementedWaveImplAbuseTest ----

describe("wanted evaluations", () => {
  const evaluation = (
    waveletId: string | null,
    wanted: boolean,
    certainty: number,
    timestamp: number,
    agent: string,
    ignored: boolean,
    comment: string,
  ): WantedEvaluation =>
    new WantedEvaluation({
      waveletId: waveletId === null ? null : wid(waveletId),
      adderAddress: "badhorse@evilleagueofevil.com",
      wanted,
      certainty: Math.fround(certainty),
      timestamp,
      agentIdentity: agent,
      ignored,
      comment,
    });
  const same = (actual: readonly WantedEvaluation[], expected: WantedEvaluation[]): void => {
    expect(actual.length).toBe(expected.length);
    expected.forEach((e, i) => expect(actual[i]!.equals(e)).toBe(true));
  };

  it("basic", () => {
    const id = wid("wavelet1");
    const evaluation1 = evaluation("wavelet1", true, 0.2, 1000, "agent", false, "");
    const evaluation2 = evaluation("wavelet1", false, 0.2, 1010, "test2", false, "");
    const evaluation3 = evaluation("wavelet1", true, 0.3, 1011, "agent", true, "");

    let set = new WantedEvaluationSet(id, []);
    expect(set.waveletId.equals(id)).toBe(true);
    expect(set.isWanted()).toBe(true);
    const fallback = set.mostCertain();
    expect(fallback.wanted).toBe(true);
    expect(fallback.certainty).toBeLessThan(0.01);
    expect(set.evaluations).toEqual([]);

    set = new WantedEvaluationSet(id, [evaluation1, evaluation2]);
    expect(set.mostCertain().equals(evaluation2)).toBe(true);
    expect(set.isWanted()).toBe(false);
    expect(set.isIgnored()).toBe(false);

    const all = [evaluation1, evaluation2, evaluation3];
    set = new WantedEvaluationSet(id, all);
    expect(set.mostCertain().equals(evaluation3)).toBe(true);
    expect(set.isWanted()).toBe(true);
    expect(set.isIgnored()).toBe(true);
    same(set.evaluations, all);
  });

  it("duplicates are dropped and ties go to the hash code", () => {
    const a = evaluation("wavelet1", true, 0.5, 7, "a", false, "");
    const b = evaluation("wavelet1", false, 0.5, 7, "b", true, "");
    const forward = new WantedEvaluationSet(wid("wavelet1"), [a, b, a]);
    expect(forward.evaluations.length).toBe(2);
    const backward = new WantedEvaluationSet(wid("wavelet1"), [b, a]);
    expect(forward.mostCertain().equals(backward.mostCertain())).toBe(true);
    const expected = a.javaHashCode() > b.javaHashCode() ? a : b;
    expect(forward.mostCertain().equals(expected)).toBe(true);
  });

  /** The fixture of `LiveSupplementedWaveImplAbuseTest`. */
  const fixture = (): {
    udw: OpBasedWavelet;
    view: WaveletDataView;
    evaluations: [WantedEvaluation, WantedEvaluation, WantedEvaluation];
  } => ({
    udw: userData(),
    view: new WaveletDataView(viewer(), [
      conversation("conv+root").data(),
      conversation("conv+other").data(),
    ]),
    evaluations: [
      evaluation("conv+root", true, 0.1, 1234, "agent0", true, "no comment"),
      evaluation("conv+other", true, 0.8, 1256, "agent2", false, "no comment"),
      evaluation("conv+root", false, 0.5, 1278, "agent1", true, "no comment"),
    ],
  });

  it("empty evaluation set", () => {
    const { udw, view } = fixture();
    const supplemented = new SupplementedWaveImpl(
      WaveletBasedSupplement.create(udw),
      view,
      "always",
    );
    const set = supplemented.wantedEvaluationSet(wid("conv+root"));
    expect(set.isWanted()).toBe(true);
    expect(set.evaluations).toEqual([]);
    expect(set.mostCertain().certainty).toBeLessThan(0.01);
    expect(set.mostCertain().wanted).toBe(true);
  });

  it("invalid evaluations ignored", () => {
    const { udw, view, evaluations } = fixture();
    // An evaluation without a wavelet id.
    append(
      udw,
      ABUSE_DOCUMENT,
      '<wanted agent="invalid agent" certainty="0.0" timestamp="1234" ' +
        'comment="no comment" wanted="true"></wanted>',
    );
    const supplemented = new SupplementedWaveImpl(
      WaveletBasedSupplement.create(udw),
      view,
      "always",
    );
    supplemented.addWantedEvaluation(evaluations[1]);
    same(supplemented.wantedEvaluationSet(wid("conv+other")).evaluations, [evaluations[1]]);
    const all = supplemented.primitive.wantedEvaluations();
    expect(all.length).toBe(2);
    expect(all[0]!.waveletId).toBeNull();
    expect(all[0]!.agentIdentity).toBe("invalid agent");
    expect(all[0]!.adderAddress).toBe("");
  });

  it("multiple evaluation sets", () => {
    const { udw, view, evaluations } = fixture();
    const supplemented = new SupplementedWaveImpl(
      WaveletBasedSupplement.create(udw),
      view,
      "always",
    );
    for (const e of evaluations) {
      supplemented.addWantedEvaluation(e);
    }
    same(supplemented.wantedEvaluationSet(wid("conv+root")).evaluations, [
      evaluations[0],
      evaluations[2],
    ]);
    same(supplemented.wantedEvaluationSet(wid("conv+other")).evaluations, [evaluations[1]]);
  });

  it("evaluation without wavelet id cannot be stored", () => {
    const udw = userData();
    const primitive = WaveletBasedSupplement.create(udw);
    const bad = evaluation(null, true, 0.5, 1, "a", false, "");
    expect(() => primitive.addWantedEvaluation(bad)).toThrow(SupplementError);
    expect(udw.ops().length).toBe(0);
  });
});

// ---- SupplementedWaveImplTest ----

describe("supplemented wave", () => {
  const ROOT_VERSION = 30;
  const ROOT_BLIP1_VERSION = 28;
  const W1_VERSION = 20;
  const W1_BLIP1_VERSION = 15;
  const ROOT_BLIP1 = "b+rootblip";
  const W1_BLIP1 = "b+w1blip";
  const root = (): WaveletId => wid("conv+root");
  const w1 = (): WaveletId => wid("conv+w1");
  const key = (id: WaveletId): string => `${id.domain}/${id.id}`;

  /** `StubSupplementWaveView`. */
  class StubView implements SupplementWaveView {
    private readonly ids = new Map<string, WaveletId>();
    private readonly versions = new Map<string, number>();
    readonly signatures = new Map<string, HashedVersion>();
    private readonly blips = new Map<string, Map<string, number>>();
    explicitParticipant = false;

    put(id: WaveletId, version: number): void {
      this.ids.set(key(id), id);
      this.versions.set(key(id), version);
    }

    sign(id: WaveletId, signature: HashedVersion): void {
      this.signatures.set(key(id), signature);
    }

    putBlipVersion(id: WaveletId, blip: string, version: number): void {
      const blips = this.blips.get(key(id)) ?? new Map<string, number>();
      blips.set(blip, version);
      this.blips.set(key(id), blips);
    }

    touch(id: WaveletId): void {
      const version = this.versions.get(key(id));
      if (version !== undefined) {
        this.versions.set(key(id), version + 1);
      }
    }

    wavelets(): WaveletId[] {
      return [...this.ids.values()].sort((a, b) => a.compareTo(b));
    }

    version(id: WaveletId): number {
      return this.versions.get(key(id)) ?? NO_VERSION;
    }

    signature(id: WaveletId): HashedVersion {
      return this.signatures.get(key(id)) ?? HashedVersion.unsigned(0);
    }

    blipVersions(id: WaveletId): Map<string, number> {
      return new Map(this.blips.get(key(id)) ?? []);
    }

    blipVersion(id: WaveletId, blipId: string): number | null {
      return this.blips.get(key(id))?.get(blipId) ?? null;
    }

    isExplicitParticipant(): boolean {
      return this.explicitParticipant;
    }
  }

  type Target = SupplementedWaveImpl<PrimitiveSupplementImpl, StubView>;

  const over = (wave: StubView, primitive = new PrimitiveSupplementImpl()): Target =>
    new SupplementedWaveImpl(primitive, wave, "always");

  /** The original's `setUp`. */
  const target = (): Target => {
    const wave = new StubView();
    wave.put(root(), ROOT_VERSION);
    wave.put(w1(), W1_VERSION);
    wave.putBlipVersion(root(), ROOT_BLIP1, ROOT_BLIP1_VERSION);
    wave.putBlipVersion(w1(), W1_BLIP1, W1_BLIP1_VERSION);
    return over(wave);
  };

  const archiveVersions = (supplement: Target): [string, number][] =>
    supplement.primitive
      .archiveWavelets()
      .map((id) => [id.id, supplement.primitive.archiveWaveletVersion(id)]);

  const EXPECTED_ARCHIVE_VERSIONS: [string, number][] = [
    ["conv+root", ROOT_VERSION],
    ["conv+w1", W1_VERSION],
  ];

  const assertState = (
    supplement: SupplementedWaveImpl<ReadablePrimitiveSupplement, SupplementWaveView>,
    state: InboxState,
  ): void => {
    expect(supplement.isInbox()).toBe(state === "inbox");
    expect(supplement.isArchived()).toBe(state === "archive");
    expect(supplement.isMute()).toBe(state === "mute");
    expect(supplement.inboxState()).toBe(state);
  };

  // The original throws for a view that lists a wavelet that is not
  // conversational; here such a wavelet is disregarded.
  it("incorrect external wave view is disregarded", () => {
    const supplement = target();
    supplement.wave.put(wid("user+foo@bar.com"), 20);
    supplement.archive();
    supplement.markAsRead();
    supplement.see();
    supplement.markAsNotified();
    expect(archiveVersions(supplement)).toEqual(EXPECTED_ARCHIVE_VERSIONS);
    const substrate = supplement.primitive;
    expect(idsOf(substrate.readWavelets())).toEqual(["conv+root", "conv+w1"]);
    expect(idsOf(substrate.seenWavelets())).toEqual(["conv+root", "conv+w1"]);
    expect(idsOf(substrate.notifiedWavelets())).toEqual(["conv+root", "conv+w1"]);
    assertState(supplement, "archive");
  });

  it("stub blips are unread until read", () => {
    const supplement = target();
    expect(supplement.isUnread(root(), ROOT_BLIP1)).toBe(true);
    expect(supplement.isUnread(w1(), W1_BLIP1)).toBe(true);
    expect(supplement.isUnread(root(), "b+unknown")).toBe(false);
    supplement.markBlipAsRead(root(), ROOT_BLIP1);
    supplement.markBlipAsRead(root(), "b+unknown");
    expect(supplement.isUnread(root(), ROOT_BLIP1)).toBe(false);
    expect(supplement.isUnread(w1(), W1_BLIP1)).toBe(true);
    // Read at the version of the wavelet, not of the blip.
    expect(supplement.primitive.lastReadBlipVersion(root(), ROOT_BLIP1)).toBe(ROOT_VERSION);
    expect(supplement.primitive.readBlips(root())).toEqual([ROOT_BLIP1]);
  });

  /**
   * The original's `setUpWithWaveModel`: a root conversation over the wave
   * model and an in-memory supplement.
   */
  class Model {
    readonly root = wavelet("conv+root");
    readonly substrate = new PrimitiveSupplementImpl();
    private blips = 0;

    constructor() {
      append(this.root, "conversation", "<conversation></conversation>");
    }

    /**
     * `getRootThread().appendBlip()`: a manifest entry and a blip
     * document, two operations.
     */
    appendBlip(): string {
      this.blips += 1;
      const id = `b+${this.blips}`;
      this.root.editDocument(
        "conversation",
        (doc) => void doc.createElement(doc.size() - 1, "blip", [["id", id]]),
      );
      append(this.root, id, "<body><line></line></body>");
      return id;
    }

    version(): number {
      return this.root.data().version;
    }

    supplement(): SupplementedWaveImpl<PrimitiveSupplementImpl, WaveletDataView> {
      const view = new WaveletDataView(viewer(), [this.root.data()]);
      return new SupplementedWaveImpl(this.substrate, view, "always");
    }
  }

  it("new blip is unread", () => {
    const model = new Model();
    const b = model.appendBlip();
    expect(model.supplement().isUnread(root(), b)).toBe(true);
  });

  it("mark blip as read affects blip read state", () => {
    const model = new Model();
    const b = model.appendBlip();
    const supplement = model.supplement();
    supplement.markBlipAsRead(root(), b);
    expect(supplement.isUnread(root(), b)).toBe(false);
  });

  it("mark blip is idempotent", () => {
    const model = new Model();
    const b = model.appendBlip();
    model.supplement().markBlipAsRead(root(), b);
    const blipReadVersion = model.substrate.lastReadBlipVersion(root(), b);
    const waveletVersion = model.version();
    expect(blipReadVersion).toBe(waveletVersion);

    model.appendBlip();
    expect(model.version()).toBeGreaterThan(waveletVersion);
    model.supplement().markBlipAsRead(root(), b);
    expect(model.substrate.lastReadBlipVersion(root(), b)).toBe(blipReadVersion);
  });

  it("marking blip as read after wave unread marks at wavelet version", () => {
    const model = new Model();
    const b = model.appendBlip();
    model.supplement().markBlipAsRead(root(), b);
    model.supplement().markAsUnread();
    model.appendBlip();
    model.supplement().markBlipAsRead(root(), b);
    expect(model.substrate.lastReadBlipVersion(root(), b)).toBe(model.version());
  });

  it("marking blip as read after blip modified marks at wavelet version", () => {
    const model = new Model();
    const b = model.appendBlip();
    model.supplement().markBlipAsRead(root(), b);
    model.supplement().markAsUnread();
    append(model.root, b, "<body><line></line></body>");
    model.appendBlip();
    model.supplement().markBlipAsRead(root(), b);
    expect(model.substrate.lastReadBlipVersion(root(), b)).toBe(model.version());
  });

  it("unmuted unarchived is inbox", () => {
    assertState(target(), "inbox");
  });

  it("empty wave state", () => {
    assertState(over(new StubView()), "archive");
  });

  it("unmuted some archived is inbox", () => {
    const supplement = target();
    supplement.primitive.archiveAtVersion(root(), ROOT_VERSION);
    assertState(supplement, "inbox");
  });

  it("unmuted archived is not inbox", () => {
    const supplement = target();
    supplement.archive();
    assertState(supplement, "archive");
  });

  it("unmuted remote archived is not inbox", () => {
    const supplement = target();
    supplement.primitive.archiveAtVersion(root(), ROOT_VERSION);
    supplement.primitive.archiveAtVersion(w1(), W1_VERSION);
    assertState(supplement, "archive");
  });

  it("unfollowed is not inbox", () => {
    const supplement = target();
    supplement.unfollow();
    assertState(supplement, "mute");
    const remote = target();
    remote.primitive.unfollow();
    assertState(remote, "mute");
  });

  it("archive archives all wavelets", () => {
    const supplement = target();
    supplement.archive();
    expect(archiveVersions(supplement)).toEqual(EXPECTED_ARCHIVE_VERSIONS);
    assertState(supplement, "archive");
  });

  it("change after archive is inbox", () => {
    const supplement = target();
    supplement.archive();
    supplement.wave.touch(root());
    assertState(supplement, "inbox");
  });

  it("change after unfollow is not inbox", () => {
    const supplement = target();
    supplement.unfollow();
    supplement.wave.touch(root());
    assertState(supplement, "mute");
  });

  it("unfollow unfollows substrate", () => {
    const supplement = target();
    supplement.unfollow();
    expect(supplement.primitive.followed()).toBe(false);
    assertState(supplement, "mute");
  });

  it("mute clears archive versions", () => {
    const supplement = target();
    supplement.archive();
    supplement.mute();
    expect(archiveVersions(supplement)).toEqual([]);
  });

  it("archive does not clear mute nor sets archive versions", () => {
    const supplement = target();
    supplement.unfollow();
    supplement.archive();
    expect(supplement.primitive.followed()).toBe(false);
    expect(archiveVersions(supplement)).toEqual([]);
  });

  it("move to folder archives", () => {
    const supplement = target();
    supplement.moveToFolder(100);
    expect(archiveVersions(supplement)).toEqual(EXPECTED_ARCHIVE_VERSIONS);
    expect(supplement.isInbox()).toBe(false);
  });

  it("move to folder removes existing folders", () => {
    const supplement = target();
    supplement.primitive.addFolder(100);
    supplement.primitive.addFolder(101);
    supplement.moveToFolder(102);
    expect(supplement.folders()).toEqual([102]);
    expect(supplement.primitive.folders()).toEqual([102]);
  });

  it("move to folder preserves unfollow", () => {
    const supplement = target();
    supplement.unfollow();
    supplement.moveToFolder(100);
    expect(supplement.primitive.followed()).toBe(false);
  });

  it("move to inbox folder moves to inbox", () => {
    const supplement = target();
    supplement.moveToFolder(INBOX_FOLDER);
    expect(supplement.isInbox()).toBe(true);
    expect(supplement.primitive.folders()).toEqual([]);
  });

  it("move to inbox folder removes existing folders", () => {
    const supplement = target();
    supplement.moveToFolder(100);
    supplement.moveToFolder(INBOX_FOLDER);
    expect(supplement.folders()).toEqual([]);
  });

  it("inbox moves to inbox and clears archive state", () => {
    const supplement = target();
    supplement.inbox();
    assertState(supplement, "inbox");
    supplement.archive();
    supplement.inbox();
    expect(archiveVersions(supplement)).toEqual([]);
  });

  it("move to folder all archives and clears folder state", () => {
    const supplement = target();
    supplement.moveToFolder(100);
    supplement.moveToFolder(ALL_FOLDER);
    assertState(supplement, "archive");
    expect(supplement.folders()).toEqual([]);
    expect(archiveVersions(supplement)).toEqual(EXPECTED_ARCHIVE_VERSIONS);
    expect(supplement.primitive.folders()).toEqual([]);
  });

  it("move to folder all preserves mute", () => {
    const supplement = target();
    supplement.moveToFolder(100);
    supplement.unfollow();
    supplement.moveToFolder(ALL_FOLDER);
    expect(supplement.primitive.followed()).toBe(false);
  });

  it("mute preserves folders", () => {
    const supplement = target();
    supplement.moveToFolder(100);
    supplement.mute();
    expect(supplement.folders()).toEqual([100]);
  });

  it("participant read state", () => {
    const supplement = new Model().supplement();
    expect(supplement.haveParticipantsEverBeenRead(root())).toBe(false);
    expect(supplement.isParticipantsUnread(root())).toBe(true);
    supplement.markParticipantsAsRead(root());
    expect(supplement.haveParticipantsEverBeenRead(root())).toBe(true);
    expect(supplement.isParticipantsUnread(root())).toBe(false);
  });

  it("participant read state with blip", () => {
    const model = new Model();
    model.appendBlip();
    const supplement = model.supplement();
    supplement.markParticipantsAsRead(root());
    expect(supplement.isParticipantsUnread(root())).toBe(false);
  });

  it("participant read state affected by wavelet read state", () => {
    const supplement = new Model().supplement();
    expect(supplement.haveParticipantsEverBeenRead(root())).toBe(false);
    expect(supplement.isParticipantsUnread(root())).toBe(true);
    supplement.markAsRead();
    expect(supplement.haveParticipantsEverBeenRead(root())).toBe(true);
    expect(supplement.isParticipantsUnread(root())).toBe(false);
  });

  /** A view of two wavelets with signatures. */
  const signed = (): {
    id1: WaveletId;
    id2: WaveletId;
    signature1: HashedVersion;
    signature2: HashedVersion;
    supplement: Target;
  } => {
    const id1 = wid("somewaveletid1");
    const id2 = wid("somewaveletid2");
    const signature1 = HashedVersion.of(234, hash(1));
    const signature2 = HashedVersion.of(12828, hash(2));
    const wave = new StubView();
    wave.put(id1, 234);
    wave.put(id2, 12828);
    wave.sign(id1, signature1);
    wave.sign(id2, signature2);
    return { id1, id2, signature1, signature2, supplement: over(wave) };
  };

  it("see marks as seen at version", () => {
    const { id1, id2, signature1, signature2, supplement } = signed();
    supplement.see();
    expect(supplement.seenVersion(id1).equals(signature1)).toBe(true);
    expect(supplement.seenVersion(id2).equals(signature2)).toBe(true);
    expect(idsOf(supplement.primitive.seenWavelets())).toEqual([id1.id, id2.id]);
  });

  it("see wavelet marks one wavelet", () => {
    const { id1, id2, signature1, supplement } = signed();
    supplement.seeWavelet(id1);
    expect(supplement.seenVersion(id1).equals(signature1)).toBe(true);
    expect(supplement.seenVersion(id2).equals(HashedVersion.unsigned(0))).toBe(true);
  });

  it("trashed if wave in trash", () => {
    const supplement = target();
    supplement.primitive.addFolder(TRASH_FOLDER);
    expect(supplement.isTrashed()).toBe(true);
    const other = target();
    other.primitive.addFolder(INBOX_FOLDER);
    expect(other.isTrashed()).toBe(false);
  });

  it("has been seen", () => {
    const { supplement } = signed();
    expect(supplement.hasBeenSeen()).toBe(false);
    supplement.see();
    expect(supplement.hasBeenSeen()).toBe(true);
  });

  it("read state affected by tags state", () => {
    const supplement = new Model().supplement();
    expect(supplement.isTagsUnread(root())).toBe(true);
    supplement.markAsRead();
    expect(supplement.isTagsUnread(root())).toBe(false);
    supplement.markAsUnread();
    expect(supplement.isTagsUnread(root())).toBe(true);
    supplement.markTagsAsRead(root());
    expect(supplement.isTagsUnread(root())).toBe(false);
  });

  it("participant read state affected by wavelet read state with blip", () => {
    const model = new Model();
    const b = model.appendBlip();
    const supplement = model.supplement();
    supplement.markAsRead();
    expect(supplement.isUnread(root(), b)).toBe(false);
    expect(supplement.isParticipantsUnread(root())).toBe(false);
  });

  it("new wave has no pending notification", () => {
    expect(target().hasPendingNotification()).toBe(false);
  });

  it("reports pending notification", () => {
    const supplement = target();
    supplement.primitive.setNotifiedVersion(w1(), W1_VERSION);
    expect(supplement.hasPendingNotification()).toBe(true);
  });

  it("seeing clears pending notification", () => {
    const { supplement } = signed();
    supplement.markAsNotified();
    expect(supplement.hasPendingNotification()).toBe(true);
    supplement.see();
    expect(supplement.hasPendingNotification()).toBe(false);
  });

  it("seeing clears pending notification, pending notification flag set", () => {
    // The pending flag can only come from a user-data wavelet.
    const udw = userData();
    append(udw, SEEN_DOCUMENT, '<notification pending="true"></notification>');
    const id1 = wid("somewaveletid1");
    const wave = new StubView();
    wave.put(id1, 234);
    wave.sign(id1, HashedVersion.of(234, hash(1)));
    const supplement = new SupplementedWaveImpl(
      WaveletBasedSupplement.create(udw),
      wave,
      "always",
    );
    expect(supplement.hasPendingNotification()).toBe(true);
    supplement.markAsNotified();
    expect(supplement.hasPendingNotification()).toBe(true);
    supplement.see();
    expect(supplement.hasPendingNotification()).toBe(false);
    expect(supplement.primitive.pendingNotification()).toBe(false);
  });

  it("default follow policies", () => {
    let wave = new StubView();
    expect(isFollowedByDefault("always", wave)).toBe(true);
    expect(isFollowedByDefault("whenParticipant", wave)).toBe(false);
    wave.explicitParticipant = true;
    expect(isFollowedByDefault("whenParticipant", wave)).toBe(true);

    wave = new StubView();
    wave.put(root(), 1);
    const supplement = new SupplementedWaveImpl(
      new PrimitiveSupplementImpl(),
      wave,
      "whenParticipant",
    );
    expect(supplement.isMute()).toBe(true);
    // Not followed: archiving does nothing.
    supplement.archive();
    expect(supplement.primitive.archiveWavelets()).toEqual([]);
    supplement.follow();
    expect(supplement.isFollowed()).toBe(true);
    supplement.primitive.clearFollow();
    expect(supplement.isFollowed()).toBe(false);
  });

  it("versions are narrowed to int as in java", () => {
    const wave = new StubView();
    wave.put(root(), 2 ** 32 + 5);
    const supplement = over(wave);
    supplement.markAsRead();
    expect(supplement.primitive.lastReadWaveletVersion(root())).toBe(5);
  });
});

// ---- what the port adds ----

describe("port", () => {
  it("wavelet data view takes conversations only", () => {
    const root = conversation("conv+root");
    root.addParticipant(viewer());
    root.data().setHashedVersion(HashedVersion.of(9, hash(1, 2)));
    const other = conversation("conv+other");
    const noManifest = wavelet("conv+empty");
    const badManifest = wavelet("conv+bad");
    append(badManifest, "conversation", "<x></x><conversation></conversation>");
    const udw = userData();
    const notConversational = wavelet("other+data");
    append(notConversational, "conversation", "<conversation></conversation>");
    const all = [udw, other, noManifest, root, badManifest, notConversational].map((w) =>
      w.data(),
    );
    const view = new WaveletDataView(viewer(), all);
    expect(idsOf(view.wavelets())).toEqual(["conv+other", "conv+root"]);
    expect(view.version(wid("conv+root"))).toBe(3);
    expect(view.version(wid("conv+empty"))).toBe(-1);
    expect(view.signature(wid("conv+root")).equals(HashedVersion.of(9, hash(1, 2)))).toBe(true);
    expect(view.signature(wid("conv+empty")).equals(HashedVersion.unsigned(0))).toBe(true);
    expect(view.blipVersion(wid("conv+root"), "b+1")).toBe(2);
    expect(view.blipVersion(wid("conv+root"), "b+2")).toBeNull();
    expect(view.blipVersion(wid("conv+empty"), "b+1")).toBeNull();
    expect([...view.blipVersions(wid("conv+root"))]).toEqual([["b+1", 2]]);
    expect(view.blipVersions(wid("conv+empty")).size).toBe(0);
    expect(view.isExplicitParticipant()).toBe(true);
    const stranger = ParticipantId.of("stranger@example.com");
    expect(new WaveletDataView(stranger, all).isExplicitParticipant()).toBe(false);
  });

  // The use the server makes of the supplement for digests: count the
  // unread blips of a wave without writing anything.
  it("read only digest", () => {
    const root = conversation("conv+root");
    append(root, "b+2", "<body><line></line></body>");
    const udw = userData();
    const written = new SupplementedWaveImpl(
      WaveletBasedSupplement.create(udw),
      new WaveletDataView(viewer(), [root.data()]),
      "always",
    );
    written.markBlipAsRead(wid("conv+root"), "b+1");
    written.moveToFolder(7);
    // The legacy folders the original removes on creation.
    append(udw, FOLDERS_DOCUMENT, '<folder i="1"></folder><folder i="3"></folder>');

    const view = new WaveletDataView(viewer(), [root.data(), udw.data()]);
    const state = new UserDataSupplement(udw.data());
    expect(state.folders()).toEqual([7]);
    expect(state.isInFolder(3)).toBe(false);
    const unread = (
      s: SupplementedWaveImpl<ReadablePrimitiveSupplement, SupplementWaveView>,
    ): number => ["b+1", "b+2"].filter((blip) => s.isUnread(wid("conv+root"), blip)).length;
    let supplemented: SupplementedWaveImpl<ReadablePrimitiveSupplement, SupplementWaveView> =
      new SupplementedWaveImpl(state, view, "always");
    expect(unread(supplemented)).toBe(1);
    expect(supplemented.isArchived()).toBe(true);
    expect(supplemented.isInbox()).toBe(false);

    // The same from a snapshot, which answers without reading the
    // documents again.
    for (const snapshot of [PrimitiveSupplementImpl.copyOf(state), state.snapshot()]) {
      supplemented = new SupplementedWaveImpl(snapshot, view, "always");
      expect(unread(supplemented)).toBe(1);
      expect(supplemented.folders()).toEqual([7]);
      expect(supplemented.isArchived()).toBe(true);
    }

    // Without a user-data wavelet everything is unread and in the inbox.
    supplemented = new SupplementedWaveImpl(new PrimitiveSupplementImpl(), view, "always");
    expect(unread(supplemented)).toBe(2);
    expect(supplemented.isInbox()).toBe(true);
  });

  // The operations to submit are those the user-data wavelet recorded.
  it("mutations are recorded as operations", () => {
    const root = conversation("conv+root");
    const udw = userData();
    const supplemented = new SupplementedWaveImpl(
      WaveletBasedSupplement.create(udw),
      new WaveletDataView(viewer(), [root.data()]),
      "always",
    );
    supplemented.markAsRead();
    supplemented.archive();
    expect(udw.takeDelta()?.ops.length).toBe(3);
    expect(xml(udw, READSTATE_DOCUMENT)).toBe(
      '<wavelet i="example.com/conv+root"><all v="2"/></wavelet>',
    );
    expect(xml(udw, ARCHIVING_DOCUMENT)).toBe('<archive i="example.com/conv+root" v="2"/>');
  });

  it("create removes legacy folders and reader hides them", () => {
    const udw = userData();
    append(
      udw,
      FOLDERS_DOCUMENT,
      '<folder i="3"></folder><folder i="8"></folder><folder i="1"></folder>',
    );
    udw.takeOps();
    expect(new UserDataSupplement(udw.data()).folders()).toEqual([8]);
    expect(udw.ops().length).toBe(0);
    const supplement = WaveletBasedSupplement.create(udw);
    expect(supplement.folders()).toEqual([8]);
    expect(udw.ops().length).toBe(2);
    expect(xml(udw, FOLDERS_DOCUMENT)).toBe('<folder i="8"/>');
  });

  it("malformed state is ignored", () => {
    const udw = userData();
    append(
      udw,
      READSTATE_DOCUMENT,
      '<wavelet i="not an id"><all v="5"></all></wavelet>' +
        '<wavelet i="example.com/conv+root"><all v="x"></all><all></all>' +
        '<blip v="3"></blip><blip i="b+1" v=""></blip><blip i="b+2" v="4"></blip>' +
        "text</wavelet>",
    );
    append(
      udw,
      PRESENTATION_DOCUMENT,
      '<conversation i="example.com/conv+root">' +
        '<thread i="t" state="collapsed"></thread></conversation>',
    );
    append(
      udw,
      SEEN_DOCUMENT,
      '<seen i="example.com/conv+root" signature="x:y"></seen>' +
        '<notified i="example.com/conv+root" v="1.5"></notified>',
    );
    append(
      udw,
      ABUSE_DOCUMENT,
      '<wanted wavelet_id="example.com/conv+root" certainty="high"></wanted>',
    );
    append(udw, FOLDERS_DOCUMENT, '<folder></folder><folder i="x"></folder>');
    const state = new UserDataSupplement(udw.data());
    const root = wid("conv+root");
    expect(idsOf(state.readWavelets())).toEqual(["conv+root"]);
    expect(state.lastReadWaveletVersion(root)).toBe(NO_VERSION);
    expect(state.readBlips(root)).toEqual(["b+2"]);
    expect(state.lastReadBlipVersion(root, "b+1")).toBe(NO_VERSION);
    expect(state.lastReadBlipVersion(root, "b+2")).toBe(4);
    expect(state.threadState(root, "t")).toBeNull();
    expect(state.statefulThreads(root)).toEqual([]);
    expect(idsOf(state.waveletsWithThreadState())).toEqual(["conv+root"]);
    expect(state.seenVersion(root).equals(HashedVersion.unsigned(0))).toBe(true);
    expect(state.seenWavelets()).toEqual([]);
    expect(state.notifiedVersion(root)).toBe(NO_VERSION);
    expect(state.wantedEvaluations()).toEqual([]);
    expect(state.folders()).toEqual([]);
  });

  it("clear blip read state of unknown wavelet does nothing", () => {
    const udw = userData();
    WaveletBasedSupplement.create(udw).clearBlipReadState(wid("conv+root"), "b+1");
    expect(udw.ops().length).toBe(0);
  });

  it("in memory wavelet version quirk", () => {
    const target = new PrimitiveSupplementImpl();
    const id = wid("conv+root");
    target.setLastReadWaveletVersion(id, 5);
    // Set, and no participants version to compare with: the original
    // throws a NullPointerException.
    expect(() => target.setLastReadWaveletVersion(id, 9)).toThrow(SupplementError);
    expect(target.lastReadWaveletVersion(id)).toBe(5);
    // Replaced only while less than the participants version.
    target.setLastReadParticipantsVersion(id, 7);
    target.setLastReadWaveletVersion(id, 9);
    expect(target.lastReadWaveletVersion(id)).toBe(9);
    target.setLastReadWaveletVersion(id, 12);
    expect(target.lastReadWaveletVersion(id)).toBe(9);
  });

  it("partitioning keeps thread and gadget state apart", () => {
    const id = wid("conv+root");
    const collapsed = ThreadState.Collapsed;
    let target = new PartitioningPrimitiveSupplement(new PrimitiveSupplementImpl(), false, false);
    target.setThreadState(id, "t", collapsed);
    expect(target.threadState(id, "t")).toBe(collapsed);
    expect(target.statefulThreads(id)).toEqual(["t"]);
    expect(idsOf(target.waveletsWithThreadState())).toEqual(["conv+root"]);
    expect(target.real.threadState(id, "t")).toBeNull();
    target.setGadgetState("g", "k", "v");
    expect(target.gadgetState("g").size).toBe(0);
    expect(target.real.gadgetState("g").size).toBe(0);
    target.addFolder(4);
    expect(target.real.folders()).toEqual([4]);
    target.stopPartitioning();
    expect(target.threadState(id, "t")).toBeNull();

    target = new PartitioningPrimitiveSupplement(new PrimitiveSupplementImpl(), true, true);
    target.setThreadState(id, "t", collapsed);
    target.setGadgetState("g", "k", "v");
    target.stopPartitioning();
    expect(target.threadState(id, "t")).toBe(collapsed);
    expect(target.real.threadState(id, "t")).toBe(collapsed);
    expect(target.real.gadgetState("g").get("k")).toBe("v");
  });

  it("thread state names", () => {
    expect(ThreadState.Collapsed).toBe("COLLAPSED");
    expect(ThreadState.Expanded).toBe("EXPANDED");
    expect(THREAD_STATE.fromAttribute("EXPANDED")).toBe(ThreadState.Expanded);
    expect(THREAD_STATE.fromAttribute("expanded")).toBeNull();
  });
});
