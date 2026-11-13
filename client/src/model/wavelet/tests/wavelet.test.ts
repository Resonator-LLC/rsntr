// Tests of the op-based wavelet and the wave view: the unit tests of the
// Rust `wavelet.rs`, and tests of what the TypeScript port adds (events,
// remote operations, the output sink).

import { describe, expect, test } from "vitest";

import { DocError, XmlFragment } from "../../doc";
import { DocOpBuilder } from "../../docop";
import { IdGeneratorImpl, ParticipantId, WaveId, WaveletId } from "../../id";
import {
  WaveletOperation,
  WaveletOperationContext,
  WaveletOperationError,
} from "../../operation";
import { HashedVersion, HashedVersionFactory } from "../../version";
import {
  OpBasedWavelet,
  type OpBasedWaveletOptions,
  WaveView,
  WaveletError,
  type WaveletEvents,
} from "..";

const alice = (): ParticipantId => ParticipantId.of("alice@example.com");
const bob = (): ParticipantId => ParticipantId.of("bob@other.org");

function view(options: OpBasedWaveletOptions = {}): WaveView {
  return new WaveView(WaveId.of("example.com", "w+1"), alice(), {
    timestamp: 1000,
    waveletOptions: () => options,
  });
}

function thrownBy(f: () => unknown): unknown {
  try {
    f();
  } catch (e) {
    return e;
  }
  throw new Error("expected the call to throw");
}

const addresses = (participants: readonly ParticipantId[]): string[] =>
  participants.map((p) => p.address);

/** Records every event of a wavelet as a compact string. */
function log(wavelet: OpBasedWavelet): string[] {
  const out: string[] = [];
  const where = (local: boolean): string => (local ? "local" : "remote");
  const e = wavelet.events;
  e.on("operation", ({ op, local }) => out.push(`op ${op.kind.type} ${where(local)}`));
  e.on("participantAdded", (x) =>
    out.push(`+p ${x.participant} by ${x.author} ${where(x.local)}`),
  );
  e.on("participantRemoved", (x) =>
    out.push(`-p ${x.participant} by ${x.author} ${where(x.local)}`),
  );
  e.on("documentChanged", (x) =>
    out.push(`doc ${x.docId} by ${x.author} ${where(x.local)}${x.created ? " created" : ""}`),
  );
  e.on("blipContributorAdded", (x) => out.push(`+c ${x.docId} ${x.contributor}`));
  e.on("blipContributorRemoved", (x) => out.push(`-c ${x.docId} ${x.contributor}`));
  e.on("blipTimestampChanged", (x) => out.push(`bt ${x.docId} ${x.oldTime}->${x.newTime}`));
  e.on("blipVersionChanged", (x) => out.push(`bv ${x.docId} ${x.oldVersion}->${x.newVersion}`));
  e.on("lastModifiedTimeChanged", (x) => out.push(`t ${x.oldTime}->${x.newTime}`));
  e.on("versionChanged", (x) => out.push(`v ${x.oldVersion}->${x.newVersion}`));
  e.on("hashedVersionChanged", (x) =>
    out.push(`hv ${x.oldVersion.version}->${x.newVersion.version}`),
  );
  return out;
}

describe("ported from wavelet.rs", () => {
  test("mutations are applied and recorded", () => {
    const wavelet = view().createRoot();
    const versionZero = wavelet.data().hashedVersion;
    expect(versionZero.version).toBe(0);
    wavelet.editDocument("b+1", (doc) => doc.appendXml(XmlFragment.text("hi").wrap("p")));
    wavelet.touch();
    // Applied locally: content and times change, versions do not.
    expect(wavelet.document("b+1").toXmlString()).toBe("<p>hi</p>");
    expect(addresses(wavelet.participants())).toEqual(["alice@example.com"]);
    expect(wavelet.data().version).toBe(0);
    expect(wavelet.data().hashedVersion.equals(versionZero)).toBe(true);
    const blip = wavelet.blip("b+1")!;
    expect([blip.author.address, blip.lastModifiedTime, blip.lastModifiedVersion]).toEqual([
      "alice@example.com",
      1000,
      0,
    ]);
    expect(wavelet.ops().length).toBe(3);
    for (const op of wavelet.ops()) {
      expect(op.context.creator.equals(alice())).toBe(true);
      expect([op.context.timestamp, op.context.versionIncrement]).toEqual([1000, 0]);
      expect(op.context.hasHashedVersion()).toBe(false);
    }
    const delta = wavelet.takeDelta()!;
    expect(delta.author.equals(alice())).toBe(true);
    expect(delta.targetVersion.equals(versionZero)).toBe(true);
    expect(delta.length).toBe(3);
    expect(delta.ops[0]!.kind.type).toBe("addParticipant");
    expect(delta.ops[2]!.kind.type).toBe("noOp");
    expect(wavelet.ops().length).toBe(0);
    expect(wavelet.takeDelta()).toBeNull();
  });

  test("missing documents read as empty", () => {
    const wavelet = view().createRoot();
    expect(wavelet.hasDocument("nope")).toBe(false);
    expect(wavelet.blip("nope")).toBeNull();
    expect(wavelet.document("nope").toXmlString()).toBe("");
    expect(wavelet.documentView("nope").size()).toBe(0);
    // Reading creates nothing.
    expect(wavelet.hasDocument("nope")).toBe(false);
  });

  test("failed edit records nothing", () => {
    const wavelet = view().createRoot();
    wavelet.takeOps();
    const events = log(wavelet);
    expect(thrownBy(() => wavelet.editDocument("b+1", (doc) => doc.insertText(5, "x")))).toBeInstanceOf(
      DocError,
    );
    expect(wavelet.ops().length).toBe(0);
    const e = thrownBy(() =>
      wavelet.applyDocOp("b+1", new DocOpBuilder().retain(3).buildUnchecked()),
    );
    expect(e).toBeInstanceOf(WaveletOperationError);
    expect(wavelet.ops().length).toBe(0);
    expect(events).toEqual([]);
    // Through a document sink the wavelet's error is wrapped.
    const wrapped = thrownBy(() =>
      wavelet.documentSink("b+1").consume(new DocOpBuilder().retain(3).buildUnchecked()),
    ) as DocError;
    expect(wrapped).toBeInstanceOf(DocError);
    expect(wrapped.kind).toBe("wavelet");
  });

  test("participation helpers", () => {
    const data = view().createRoot().data();
    // Auto: the author joins first.
    const auto = new OpBasedWavelet(data.clone(), bob(), { timestamp: 5 });
    auto.touch();
    expect(addresses(auto.participants())).toEqual(["alice@example.com", "bob@other.org"]);
    expect(auto.ops().length).toBe(2);
    // Ignorant: the operation goes out as it is.
    const ignorant = new OpBasedWavelet(data.clone(), bob(), {
      timestamp: 5,
      participation: "ignorant",
    });
    ignorant.touch();
    expect(addresses(ignorant.participants())).toEqual(["alice@example.com"]);
    expect(ignorant.ops().length).toBe(1);
    // Read only.
    const readOnly = new OpBasedWavelet(data.clone(), bob(), {
      timestamp: 5,
      participation: "readOnly",
    });
    const e = thrownBy(() => readOnly.touch()) as WaveletError;
    expect(e).toBeInstanceOf(WaveletError);
    expect(e.detail).toMatchObject({ kind: "readOnly" });
    expect(thrownBy(() => readOnly.editDocument("d", (doc) => doc.insertText(0, "x")))).toBeInstanceOf(
      DocError,
    );
    expect(thrownBy(() => readOnly.addParticipant(bob()))).toBeInstanceOf(WaveletError);
    expect(readOnly.ops().length).toBe(0);
    // Adding someone who is there is not a mutation.
    readOnly.addParticipant(alice());
    expect(readOnly.ops().length).toBe(0);
  });

  test("author and timestamp can change", () => {
    const wavelet = view().createRoot();
    wavelet.takeDelta();
    wavelet.setAuthor(bob());
    wavelet.setTimestamp(7);
    wavelet.touch();
    expect([wavelet.author().address, wavelet.timestamp()]).toEqual(["bob@other.org", 7]);
    const delta = wavelet.takeDelta()!;
    expect(delta.author.equals(bob())).toBe(true);
    // The auto-add of bob and the no-op.
    expect(delta.length).toBe(2);
    expect(delta.ops[0]!.context.timestamp).toBe(7);
    wavelet.data().setVersion(9);
    expect(wavelet.data().version).toBe(9);
  });

  test("wave view wavelets", () => {
    const v = view();
    expect(v.root()).toBeNull();
    expect(v.userData()).toBeNull();
    expect(v.rootId().id).toBe("conv+root");
    v.createRoot();
    expect((thrownBy(() => v.createRoot()) as WaveletError).kind).toBe("duplicateWavelet");
    const ids = new IdGeneratorImpl("example.com", () => "x");
    const conversation = v.createConversationWavelet(ids).waveletId();
    expect(conversation.id.startsWith("conv+x")).toBe(true);
    const udw = v.createUserData().waveletId();
    expect([udw.domain, udw.id]).toEqual(["example.com", "user+alice@example.com"]);
    expect(v.userData()!.waveletId().equals(udw)).toBe(true);
    expect(v.waveletIds().length).toBe(3);
    expect(v.viewer().equals(alice())).toBe(true);
    // One delta per wavelet: the creator joining each.
    const deltas = v.takeDeltas();
    expect(deltas.length).toBe(3);
    for (const [name, delta] of deltas) {
      expect(name.waveId.equals(v.waveId())).toBe(true);
      expect(delta.length).toBe(1);
      expect(delta.targetVersion.equals(HashedVersionFactory.createVersionZero(name))).toBe(true);
    }
    expect(v.takeDeltas()).toEqual([]);
    // The user-data wavelet of a viewer from another domain lives in that
    // domain.
    const other = new WaveView(WaveId.of("example.com", "w+1"), bob(), { timestamp: 0 });
    expect(other.userDataId().domain).toBe("other.org");
    // Wrapping existing data replaces the wavelet.
    const data = v.wavelet(udw)!.data().clone();
    v.wavelet(udw)!.touch();
    expect(v.addWavelet(data).ops().length).toBe(0);
  });
});

describe("events", () => {
  test("local operations", () => {
    const sent: WaveletOperation[] = [];
    const wavelet = view({ output: (op) => sent.push(op) }).createRoot();
    expect(sent.length).toBe(1);
    const events = log(wavelet);
    wavelet.addParticipant(bob());
    wavelet.addParticipant(bob());
    expect(events.splice(0)).toEqual([
      "+p bob@other.org by alice@example.com local",
      "op addParticipant local",
    ]);
    wavelet.setTimestamp(2000);
    wavelet.editDocument("b+1", (doc) => doc.appendXml(XmlFragment.text("hi").wrap("p")));
    expect(events.splice(0)).toEqual([
      "doc b+1 by alice@example.com local created",
      "+c b+1 alice@example.com",
      "t 1000->2000",
      "op blip local",
    ]);
    wavelet.setTimestamp(3000);
    wavelet.setAuthor(bob());
    wavelet.editDocument("b+1", (doc) => doc.insertText(1, "o"));
    expect(events.splice(0)).toEqual([
      "doc b+1 by bob@other.org local",
      "+c b+1 bob@other.org",
      "bt b+1 2000->3000",
      "t 2000->3000",
      "op blip local",
    ]);
    wavelet.removeParticipant(alice());
    wavelet.touch();
    expect(events.splice(0)).toEqual([
      "-p alice@example.com by bob@other.org local",
      "op removeParticipant local",
      "op noOp local",
    ]);
    // Everything went to the sink, in order, and was recorded too.
    expect(sent.map((op) => op.kind.type)).toEqual([
      "addParticipant",
      "addParticipant",
      "blip",
      "blip",
      "removeParticipant",
      "noOp",
    ]);
    expect(wavelet.ops().length).toBe(6);
  });

  test("record: false keeps nothing", () => {
    const sent: WaveletOperation[] = [];
    const wavelet = view({ output: (op) => sent.push(op), record: false }).createRoot();
    wavelet.touch();
    expect(sent.length).toBe(2);
    expect(wavelet.ops()).toEqual([]);
    expect(wavelet.takeDelta()).toBeNull();
  });

  test("remote operations and acknowledgements", () => {
    const sent: WaveletOperation[] = [];
    const wavelet = view({ output: (op) => sent.push(op) }).createRoot();
    wavelet.editDocument("b+1", (doc) => doc.appendXml(XmlFragment.text("hi").wrap("p")));
    const local = wavelet.takeOps();
    sent.length = 0;
    const events = log(wavelet);
    // The server acknowledges the two local operations.
    const v2 = HashedVersion.of(2, new Uint8Array([1, 2]));
    wavelet.consumeRemote(local[0]!.createVersionUpdateOp(1));
    wavelet.consumeRemote(local[1]!.createVersionUpdateOp(1, v2));
    expect(events.splice(0)).toEqual([
      "v 0->1",
      "op versionUpdate remote",
      "bv b+1 0->2",
      "v 1->2",
      "hv 0->2",
      "op versionUpdate remote",
    ]);
    // Bob's operations arrive.
    const v4 = HashedVersion.of(4, new Uint8Array([3, 4]));
    wavelet.consumeRemote(
      WaveletOperation.addParticipant(new WaveletOperationContext(bob(), 5000, 1), bob()),
    );
    wavelet.consumeRemote(
      WaveletOperation.blipContent(
        new WaveletOperationContext(bob(), 5000, 1, v4),
        "b+1",
        new DocOpBuilder().retain(1).characters("o").retain(3).build(),
      ),
    );
    expect(events.splice(0)).toEqual([
      "+p bob@other.org by bob@other.org remote",
      "t 1000->5000",
      "v 2->3",
      "op addParticipant remote",
      "doc b+1 by bob@other.org remote",
      "+c b+1 bob@other.org",
      "bt b+1 1000->5000",
      "bv b+1 2->4",
      "v 3->4",
      "hv 2->4",
      "op blip remote",
    ]);
    expect(wavelet.document("b+1").toXmlString()).toBe("<p>ohi</p>");
    expect(wavelet.data().hashedVersion.equals(v4)).toBe(true);
    // Remote operations are neither recorded nor sent.
    expect(wavelet.ops()).toEqual([]);
    expect(sent).toEqual([]);
    // A delta made now targets the version it was made at.
    wavelet.touch();
    wavelet.consumeRemote(WaveletOperation.noOp(new WaveletOperationContext(bob(), 6000, 1)));
    expect(wavelet.takeDelta()!.targetVersion.equals(v4)).toBe(true);
  });

  test("a remote operation that does not apply throws and emits nothing", () => {
    const wavelet = view().createRoot();
    const events = log(wavelet);
    const e = thrownBy(() =>
      wavelet.consumeRemote(
        WaveletOperation.removeParticipant(new WaveletOperationContext(bob(), 1, 1), bob()),
      ),
    ) as WaveletOperationError;
    expect(e).toBeInstanceOf(WaveletOperationError);
    expect(e.kind).toBe("missingParticipant");
    expect(events).toEqual([]);
    expect(wavelet.data().version).toBe(0);
  });

  test("a failing listener stops neither the others nor the sink", () => {
    const sent: WaveletOperation[] = [];
    const errors: unknown[] = [];
    const wavelet = view({
      output: (op) => sent.push(op),
      onListenerError: (error) => errors.push(error),
    }).createRoot();
    sent.length = 0;
    let seen = 0;
    wavelet.events.on("operation", () => {
      throw new Error("boom");
    });
    wavelet.events.on("operation", () => seen++);
    wavelet.touch();
    wavelet.consumeRemote(WaveletOperation.noOp(new WaveletOperationContext(bob(), 1, 1)));
    expect(seen).toBe(2);
    expect(sent.length).toBe(1);
    expect(errors.length).toBe(2);
  });

  test("a failing sink reaches the caller after the events", () => {
    const wavelet = new OpBasedWavelet(view().createRoot().data(), alice(), {
      output: () => {
        throw new Error("closed");
      },
    });
    const events = log(wavelet);
    expect(() => wavelet.touch()).toThrow("closed");
    expect(events.at(-1)).toBe("op noOp local");
    expect(wavelet.ops().length).toBe(1);
  });

  test("operations made by a listener are reported in order", () => {
    const sent: string[] = [];
    const wavelet = view({ output: (op) => sent.push(op.kind.type) }).createRoot();
    sent.length = 0;
    const seen: string[] = [];
    let replied = false;
    wavelet.events.on("participantAdded", ({ participant }) => {
      seen.push(`first +${participant.name()}`);
      if (!replied) {
        replied = true;
        wavelet.touch();
        // Applied at once, reported later.
        expect(wavelet.ops().at(-1)!.kind.type).toBe("noOp");
      }
    });
    wavelet.events.on("participantAdded", ({ participant }) =>
      seen.push(`second +${participant.name()}`),
    );
    wavelet.events.on("operation", ({ op }) => seen.push(op.kind.type));
    wavelet.consumeRemote(
      WaveletOperation.addParticipant(new WaveletOperationContext(bob(), 1, 1), bob()),
    );
    expect(seen).toEqual(["first +bob", "second +bob", "addParticipant", "noOp"]);
    expect(sent).toEqual(["noOp"]);
  });

  test("bumpVersions acknowledges every local operation", () => {
    const wavelet = new WaveView(WaveId.of("example.com", "w+1"), alice(), {
      timestamp: 1000,
      bumpVersions: true,
    }).createRoot();
    const events = log(wavelet);
    wavelet.editDocument("b+1", (doc) => doc.appendXml(XmlFragment.text("hi").wrap("p")));
    expect(wavelet.data().version).toBe(2);
    expect(wavelet.blip("b+1")!.lastModifiedVersion).toBe(2);
    expect(wavelet.timestamp()).toBe(1002);
    expect(events).toEqual([
      "doc b+1 by alice@example.com local created",
      "+c b+1 alice@example.com",
      "t 1000->1001",
      "op blip local",
      "bv b+1 1->2",
      "v 1->2",
      "op versionUpdate remote",
    ]);
  });
});

describe("wave view", () => {
  test("wavelets come and go", () => {
    const v = view();
    const seen: string[] = [];
    v.events.on("waveletAdded", ({ waveletId, wavelet }) =>
      seen.push(`+${waveletId.id} ${wavelet.participants().length}`),
    );
    v.events.on("waveletRemoved", ({ waveletId }) => seen.push(`-${waveletId.id}`));
    const root = v.createRoot();
    expect(seen.splice(0)).toEqual(["+conv+root 0"]);
    expect(root.participants().length).toBe(1);
    // A new snapshot replaces the wavelet object.
    const replacement = v.replaceWavelet(root.data().clone());
    expect(replacement).not.toBe(root);
    expect(v.root()).toBe(replacement);
    expect(seen.splice(0)).toEqual(["-conv+root", "+conv+root 1"]);
    // Adding the same object again changes nothing.
    v.addWavelet(replacement);
    expect(seen).toEqual([]);
    // A ready-made wavelet is taken as it is.
    const other = new OpBasedWavelet(root.data().clone(), bob());
    expect(v.addWavelet(other)).toBe(other);
    expect(v.root()!.author().equals(bob())).toBe(true);
    expect(seen.splice(0)).toEqual(["-conv+root", "+conv+root 1"]);
    expect(v.removeWavelet(v.rootId())).toBe(other);
    expect(v.removeWavelet(v.rootId())).toBeNull();
    expect(seen).toEqual(["-conv+root"]);
    expect(v.wavelets()).toEqual([]);
  });

  test("wavelet options: view defaults, factory, explicit", () => {
    const sent: string[] = [];
    const v = new WaveView(WaveId.of("example.com", "w+1"), alice(), {
      clock: () => 42,
      waveletOptions: (data) => ({ output: (op) => sent.push(`${data.waveletId.id} ${op.kind.type}`) }),
    });
    const root = v.createRoot();
    expect(root.ops()[0]!.context.timestamp).toBe(42);
    expect(root.data().creationTime).toBe(42);
    v.createWavelet(WaveletId.of("example.com", "conv+x"), {
      output: (op) => sent.push(`explicit ${op.kind.type}`),
    });
    expect(sent).toEqual(["conv+root addParticipant", "explicit addParticipant"]);
    v.setTimestamp(7);
    root.touch();
    expect(root.ops()[1]!.context.timestamp).toBe(7);
    v.setTimestamp(null);
    expect(root.timestamp()).toBe(42);
  });

  test("wavelets are ordered by id", () => {
    const v = view();
    v.createUserData();
    v.createWavelet(WaveletId.of("example.com", "conv+b"));
    v.createRoot();
    v.createWavelet(WaveletId.of("example.com", "conv+a"));
    expect(v.waveletIds().map((id) => id.id)).toEqual([
      "conv+a",
      "conv+b",
      "conv+root",
      "user+alice@example.com",
    ]);
  });
});

// Type-level check that the event map is what the contract says.
const _events: (keyof WaveletEvents)[] = [
  "operation",
  "participantAdded",
  "participantRemoved",
  "documentChanged",
  "blipContributorAdded",
  "blipContributorRemoved",
  "blipTimestampChanged",
  "blipVersionChanged",
  "lastModifiedTimeChanged",
  "versionChanged",
  "hashedVersionChanged",
];
void _events;
