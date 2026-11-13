// Tests of the live supplement: reads, writes through the user-data
// wavelet, and events for local and remote changes alike. The events
// follow `ObservableSupplementedWave.Listener` and
// `LiveSupplementedWaveImpl` of Apache Wave (Apache License 2.0).

import { describe, expect, it } from "vitest";
import { WaveletData } from "../../data";
import { XmlFragment, type MutableDocument } from "../../doc";
import { ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import { WaveletOperation, WaveletOperationContext } from "../../operation";
import { HashedVersion } from "../../version";
import { OpBasedWavelet, WaveView } from "../../wavelet";
import {
  LiveSupplementedWave,
  READSTATE_DOCUMENT,
  ThreadState,
  WantedEvaluation,
  type LiveSupplementedWaveOptions,
  type SupplementEvents,
} from "..";

const TIME = 1234567890123;
const WAVE = WaveId.of("example.com", "w+wave");
const alice = ParticipantId.of("alice@example.com");
const bob = ParticipantId.of("bob@example.com");
const wid = (id: string): WaveletId => WaveletId.of("example.com", id);
const ROOT = wid("conv+root");
const UDW = wid("user+alice@example.com");

/** Applies an edit to a document as an operation by someone else. */
function remoteEdit(
  wavelet: OpBasedWavelet,
  docId: string,
  edit: (doc: MutableDocument) => void,
  author = bob,
): void {
  const scratch = new OpBasedWavelet(wavelet.data().clone(), author, {
    timestamp: TIME,
    participation: "ignorant",
  });
  scratch.editDocument(docId, edit);
  for (const op of scratch.takeOps()) {
    const context = new WaveletOperationContext(author, TIME, 1);
    wavelet.consumeRemote(WaveletOperation.blipContent(context, docId, op.contentOp!));
  }
}

function remoteAppend(wavelet: OpBasedWavelet, docId: string, xml: string): void {
  const fragment = XmlFragment.parse(xml);
  remoteEdit(wavelet, docId, (doc) => void doc.insertXml(doc.size(), fragment));
}

/** The server's acknowledgement of the local operations recorded so far. */
function acknowledge(wavelet: OpBasedWavelet): void {
  for (const op of wavelet.takeOps()) {
    wavelet.consumeRemote(op.createVersionUpdateOp(1));
  }
}

const EVENTS: (keyof SupplementEvents)[] = [
  "maybeBlipReadChanged",
  "maybeWaveletReadChanged",
  "maybeParticipantsReadChanged",
  "maybeTagsReadChanged",
  "maybeInboxStateChanged",
  "maybeFollowStateChanged",
  "threadStateChanged",
  "folderAdded",
  "folderRemoved",
  "wantedEvaluationsChanged",
  "maybeGadgetStateChanged",
  "maybeSeenChanged",
];

function describeEvent(name: string, event: Record<string, unknown>): string {
  const parts = [name];
  for (const value of Object.values(event)) {
    parts.push(value instanceof WaveletId ? value.id : String(value));
  }
  return parts.join(" ");
}

/** One device of alice: a wave view, its supplement, and what they emit. */
class Device {
  readonly wave: WaveView;
  readonly live: LiveSupplementedWave;
  /** The events emitted since the last `take`. */
  private events: string[] = [];
  /** The local operations of each wavelet, by wavelet id. */
  readonly sent = new Map<string, WaveletOperation[]>();

  constructor(options: LiveSupplementedWaveOptions = {}) {
    this.wave = new WaveView(WAVE, alice, {
      timestamp: TIME,
      waveletOptions: (data) => ({
        output: (op) => {
          const ops = this.sent.get(data.waveletId.id) ?? [];
          ops.push(op);
          this.sent.set(data.waveletId.id, ops);
        },
      }),
    });
    this.live = new LiveSupplementedWave(this.wave, {
      onListenerError: (error) => {
        throw error;
      },
      ...options,
    });
    for (const name of EVENTS) {
      this.live.events.on(name, (event) => this.events.push(describeEvent(name, event)));
    }
  }

  /** Adds an empty wavelet created by bob, as its snapshot arrives. */
  add(id: WaveletId): OpBasedWavelet {
    const name = WaveletName.of(WAVE, id);
    return this.wave.addWavelet(WaveletData.empty(name, bob, HashedVersion.unsigned(0), TIME));
  }

  /** Adds the root conversation with two blips, written by bob. */
  addRoot(): OpBasedWavelet {
    const root = this.add(ROOT);
    root.consumeRemote(
      WaveletOperation.addParticipant(new WaveletOperationContext(bob, TIME, 1), bob),
    );
    root.consumeRemote(
      WaveletOperation.addParticipant(new WaveletOperationContext(bob, TIME, 1), alice),
    );
    remoteAppend(
      root,
      "conversation",
      '<conversation><blip id="b+1"></blip><blip id="b+2"></blip></conversation>',
    );
    remoteAppend(root, "b+1", "<body><line></line>one</body>");
    remoteAppend(root, "b+2", "<body><line></line>two</body>");
    this.take();
    return root;
  }

  /** The distinct events emitted since the last call, sorted. */
  take(): string[] {
    const events = [...new Set(this.events)].sort();
    this.events = [];
    return events;
  }
}

describe("live supplemented wave", () => {
  it("without a user-data wavelet everything is unread and in the inbox", () => {
    const device = new Device();
    device.addRoot();
    const { live } = device;
    expect(live.hasUserData()).toBe(false);
    expect(live.conversationWavelets().map((id) => id.id)).toEqual(["conv+root"]);
    expect(live.isUnread(ROOT, "b+1")).toBe(true);
    expect(live.isUnread(ROOT, "b+9")).toBe(false);
    expect(live.unreadBlips(ROOT)).toEqual(["b+1", "b+2"]);
    expect(live.unreadCount(ROOT, ["b+2", "b+7"])).toBe(1);
    expect(live.isParticipantsUnread(ROOT)).toBe(true);
    expect(live.isTagsUnread(ROOT)).toBe(true);
    expect(live.inboxState()).toBe("inbox");
    expect(live.isFollowed()).toBe(true);
    expect(live.folders()).toEqual([]);
    expect(live.threadState(ROOT, "t+1")).toBeNull();
    expect(live.gadgetState("g").size).toBe(0);
    // Reading creates nothing.
    expect(device.wave.userData()).toBeNull();
  });

  it("marking a blip read creates the user-data wavelet, emits and flips isUnread", () => {
    const device = new Device();
    const root = device.addRoot();
    const { live } = device;
    const added: string[] = [];
    device.wave.events.on("waveletAdded", ({ waveletId }) => added.push(waveletId.id));

    live.markBlipAsRead(ROOT, "b+1");

    expect(added).toEqual(["user+alice@example.com"]);
    const udw = device.wave.userData();
    expect(udw?.waveletId().equals(UDW)).toBe(true);
    expect(udw?.participants().map((p) => p.address)).toEqual(["alice@example.com"]);
    // The participant first, then the read state.
    const sent = device.sent.get(UDW.id) ?? [];
    expect(sent[0]?.kind.type).toBe("addParticipant");
    expect(sent.slice(1).every((op) => op.isBlipContent())).toBe(true);
    expect(sent.length).toBeGreaterThan(1);
    expect(udw?.document(READSTATE_DOCUMENT).toXmlString()).toBe(
      `<wavelet i="example.com/conv+root"><blip i="b+1" v="${root.data().version}"/></wavelet>`,
    );
    expect(device.take()).toEqual(["maybeBlipReadChanged conv+root b+1"]);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    expect(live.isUnread(ROOT, "b+2")).toBe(true);
    expect(live.unreadBlips(ROOT)).toEqual(["b+2"]);
    expect(live.hasUserData()).toBe(true);

    // Again: nothing changes, nothing is sent or emitted.
    const count = sent.length;
    live.markBlipAsRead(ROOT, "b+1");
    expect(device.sent.get(UDW.id)?.length).toBe(count);
    expect(device.take()).toEqual([]);
  });

  it("a remote edit to a read blip emits and makes it unread again", () => {
    const device = new Device();
    const root = device.addRoot();
    const { live } = device;
    live.markBlipAsRead(ROOT, "b+1");
    live.markBlipAsRead(ROOT, "b+2");
    device.take();
    expect(live.unreadCount(ROOT)).toBe(0);

    remoteEdit(root, "b+1", (doc) => doc.insertText(3, "x"));

    expect(device.take()).toEqual([
      "maybeBlipReadChanged conv+root b+1",
      "maybeInboxStateChanged",
      "maybeParticipantsReadChanged conv+root",
      "maybeTagsReadChanged conv+root",
    ]);
    expect(live.isUnread(ROOT, "b+1")).toBe(true);
    expect(live.isUnread(ROOT, "b+2")).toBe(false);

    live.markBlipAsRead(ROOT, "b+1");
    expect(device.take()).toEqual(["maybeBlipReadChanged conv+root b+1"]);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
  });

  it("a new blip is reported and unread", () => {
    const device = new Device();
    const root = device.addRoot();
    device.live.markAsRead();
    device.take();
    remoteAppend(root, "b+3", "<body><line></line>three</body>");
    expect(device.take()).toContain("maybeBlipReadChanged conv+root b+3");
    expect(device.live.unreadBlips(ROOT)).toEqual(["b+3"]);
  });

  it("a local edit keeps a blip read until the server acknowledges it", () => {
    const device = new Device();
    const root = device.addRoot();
    const { live } = device;
    live.markBlipAsRead(ROOT, "b+1");
    device.take();

    // A local operation stamps the blip with the current wavelet version,
    // at which it was read (the event may fire all the same).
    root.editDocument("b+1", (doc) => doc.insertText(3, "y"));
    device.take();
    expect(live.isUnread(ROOT, "b+1")).toBe(false);

    // The acknowledgement raises the blip's version: the optimistic
    // `LocalSupplementedWaveImpl` of the original client is not ported.
    acknowledge(root);
    expect(device.take()).toContain("maybeBlipReadChanged conv+root b+1");
    expect(live.isUnread(ROOT, "b+1")).toBe(true);
  });

  it("marking the wave read and unread", () => {
    const device = new Device();
    device.addRoot();
    const { live } = device;
    live.markBlipAsRead(ROOT, "b+1");
    device.take();

    live.markAsRead();
    expect(device.take()).toEqual(["maybeWaveletReadChanged conv+root"]);
    expect(live.unreadCount(ROOT)).toBe(0);
    expect(live.isParticipantsUnread(ROOT)).toBe(false);
    expect(live.isTagsUnread(ROOT)).toBe(false);
    expect(live.haveParticipantsEverBeenRead(ROOT)).toBe(true);

    live.markAsUnread();
    expect(device.take()).toEqual([
      "maybeBlipReadChanged conv+root b+1",
      "maybeWaveletReadChanged conv+root",
    ]);
    expect(live.unreadCount(ROOT)).toBe(2);

    live.markParticipantsAsRead(ROOT);
    live.markTagsAsRead(ROOT);
    expect(device.take()).toEqual([
      "maybeParticipantsReadChanged conv+root",
      "maybeTagsReadChanged conv+root",
    ]);
    expect(live.isParticipantsUnread(ROOT)).toBe(false);
    expect(live.isTagsUnread(ROOT)).toBe(false);
  });

  it("mark-read operations arriving from another device emit", () => {
    const here = new Device();
    here.addRoot();
    const there = new Device();
    there.addRoot();

    // The other device marks a blip read, creating the user-data wavelet,
    // whose snapshot then arrives here.
    there.live.markBlipAsRead(ROOT, "b+1");
    const udwThere = there.wave.userData()!;
    const udwHere = here.wave.addWavelet(udwThere.data().clone());
    expect(here.take()).toEqual(["maybeBlipReadChanged conv+root b+1"]);
    expect(here.live.isUnread(ROOT, "b+1")).toBe(false);
    expect(here.live.isUnread(ROOT, "b+2")).toBe(true);
    expect(here.sent.get(UDW.id)).toBeUndefined();

    // From now on its operations arrive one by one.
    const from = there.sent.get(UDW.id)!.length;
    there.live.markAsRead();
    there.live.archive();
    there.live.setThreadState(ROOT, "t+1", ThreadState.Collapsed);
    for (const op of there.sent.get(UDW.id)!.slice(from)) {
      udwHere.consumeRemote(op);
    }
    expect(here.take()).toEqual([
      "maybeInboxStateChanged",
      "maybeWaveletReadChanged conv+root",
      "threadStateChanged conv+root t+1",
    ]);
    expect(here.live.unreadCount(ROOT)).toBe(0);
    expect(here.live.isArchived()).toBe(true);
    expect(here.live.threadState(ROOT, "t+1")).toBe(ThreadState.Collapsed);
    expect(here.sent.get(UDW.id)).toBeUndefined();
  });

  it("follow, archive and inbox", () => {
    const device = new Device();
    const root = device.addRoot();
    const { live } = device;

    live.archive();
    expect(device.take()).toEqual(["maybeInboxStateChanged"]);
    expect(live.inboxState()).toBe("archive");
    expect(live.isInbox()).toBe(false);

    // Any change to the wave brings it back to the inbox.
    remoteEdit(root, "b+2", (doc) => doc.insertText(3, "x"));
    expect(device.take()).toContain("maybeInboxStateChanged");
    expect(live.inboxState()).toBe("inbox");

    live.archive();
    live.unfollow();
    expect(device.take()).toEqual(["maybeFollowStateChanged", "maybeInboxStateChanged"]);
    expect(live.inboxState()).toBe("mute");
    expect(live.isMute()).toBe(true);
    expect(live.isFollowed()).toBe(false);

    live.follow();
    expect(device.take()).toEqual(["maybeFollowStateChanged", "maybeInboxStateChanged"]);
    expect(live.inboxState()).toBe("inbox");

    live.mute();
    live.inbox();
    expect(live.inboxState()).toBe("inbox");
    device.take();
    live.inbox();
    expect(device.take()).toEqual([]);
  });

  it("folders", () => {
    const device = new Device();
    device.addRoot();
    const { live } = device;
    live.moveToFolder(5);
    expect(device.take()).toEqual(["folderAdded 5", "maybeInboxStateChanged"]);
    expect(live.folders()).toEqual([5]);
    expect(live.isArchived()).toBe(true);
    live.moveToFolder(8);
    expect(device.take()).toEqual(["folderAdded 8", "folderRemoved 5"]);
    expect(live.isTrashed()).toBe(true);
    live.inbox();
    expect(device.take()).toEqual([
      "folderRemoved 8",
      "maybeFollowStateChanged",
      "maybeInboxStateChanged",
    ]);
    expect(live.folders()).toEqual([]);
  });

  it("the follow policy when participant follows the viewer's participation", () => {
    const device = new Device({ followPolicy: "whenParticipant" });
    const root = device.addRoot();
    const { live } = device;
    expect(live.isFollowed()).toBe(true);
    root.consumeRemote(
      WaveletOperation.removeParticipant(new WaveletOperationContext(bob, TIME, 1), alice),
    );
    expect(device.take()).toContain("maybeFollowStateChanged");
    expect(live.isFollowed()).toBe(false);
    expect(live.inboxState()).toBe("mute");
    live.follow();
    expect(live.isFollowed()).toBe(true);
  });

  it("thread states, gadget states, evaluations and seen versions", () => {
    const device = new Device();
    const root = device.addRoot();
    const { live } = device;

    live.setThreadState(ROOT, "t+1", ThreadState.Collapsed);
    expect(device.take()).toEqual(["threadStateChanged conv+root t+1"]);
    expect(live.threadState(ROOT, "t+1")).toBe(ThreadState.Collapsed);
    live.setThreadState(ROOT, "t+1", ThreadState.Collapsed);
    expect(device.take()).toEqual([]);
    live.setThreadState(ROOT, "t+1", null);
    expect(device.take()).toEqual(["threadStateChanged conv+root t+1"]);
    expect(live.threadState(ROOT, "t+1")).toBeNull();

    live.setGadgetState("g1", "k", "v");
    expect(device.take()).toEqual(["maybeGadgetStateChanged g1"]);
    expect([...live.gadgetState("g1")]).toEqual([["k", "v"]]);
    expect(live.gadgetStateValue("g1", "k")).toBe("v");
    expect(live.gadgetStateValue("g1", "x")).toBeNull();
    live.setGadgetState("g1", "k", null);
    expect(device.take()).toEqual(["maybeGadgetStateChanged g1"]);

    const evaluation = new WantedEvaluation({
      waveletId: ROOT,
      adderAddress: "bob@example.com",
      wanted: false,
      certainty: 0.5,
      timestamp: 12,
      agentIdentity: "agent",
      ignored: false,
      comment: "",
    });
    expect(live.wantedEvaluationSet(ROOT).isWanted()).toBe(true);
    live.addWantedEvaluation(evaluation);
    expect(device.take()).toEqual(["wantedEvaluationsChanged conv+root"]);
    expect(live.wantedEvaluationSet(ROOT).isWanted()).toBe(false);

    root.data().setHashedVersion(HashedVersion.of(root.data().version, Uint8Array.of(7)));
    expect(live.hasBeenSeen()).toBe(false);
    live.see();
    expect(device.take()).toEqual(["maybeSeenChanged"]);
    expect(live.hasBeenSeen()).toBe(true);
    expect(live.seenVersion(ROOT).equals(root.data().hashedVersion)).toBe(true);
    live.markAsNotified();
    expect(live.hasPendingNotification()).toBe(false);
  });

  it("the first change removes the legacy folders, which are never reported", () => {
    const device = new Device();
    device.addRoot();
    const udw = device.add(UDW);
    remoteAppend(udw, "m/folder", '<folder i="1"></folder><folder i="7"></folder>');
    expect(device.take()).toEqual(["folderAdded 7"]);
    expect(device.live.folders()).toEqual([7]);
    device.live.follow();
    expect(udw.document("m/folder").toXmlString()).toBe('<folder i="7"/>');
    expect(device.live.folders()).toEqual([7]);
  });

  it("follows wavelets that come, change into conversations, and go", () => {
    const device = new Device();
    const { live } = device;
    expect(live.conversationWavelets()).toEqual([]);
    expect(live.inboxState()).toBe("archive");

    // A conversational wavelet without a manifest is not a conversation.
    const root = device.add(ROOT);
    remoteAppend(root, "b+1", "<body><line></line>one</body>");
    expect(live.conversationWavelets()).toEqual([]);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    device.take();
    remoteAppend(root, "conversation", '<conversation><blip id="b+1"></blip></conversation>');
    expect(device.take()).toContain("maybeInboxStateChanged");
    expect(live.conversationWavelets().map((id) => id.id)).toEqual(["conv+root"]);
    expect(live.isUnread(ROOT, "b+1")).toBe(true);
    expect(live.inboxState()).toBe("inbox");

    live.markAsRead();
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    device.take();

    // The user-data wavelet is reset from an older snapshot.
    const name = WaveletName.of(WAVE, UDW);
    device.wave.replaceWavelet(WaveletData.empty(name, alice, HashedVersion.unsigned(0), TIME));
    expect(device.take()).toEqual(["maybeWaveletReadChanged conv+root"]);
    expect(live.isUnread(ROOT, "b+1")).toBe(true);
    // And written to again.
    live.markBlipAsRead(ROOT, "b+1");
    expect(device.take()).toEqual(["maybeBlipReadChanged conv+root b+1"]);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);

    device.wave.removeWavelet(UDW);
    expect(device.take()).toEqual(["maybeBlipReadChanged conv+root b+1"]);
    expect(live.isUnread(ROOT, "b+1")).toBe(true);

    device.wave.removeWavelet(ROOT);
    expect(device.take()).toContain("maybeInboxStateChanged");
    expect(live.conversationWavelets()).toEqual([]);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
  });

  it("answers consistently inside listeners, and writes from them", () => {
    const device = new Device();
    device.addRoot();
    const { live } = device;
    const seen: boolean[] = [];
    live.events.on("maybeBlipReadChanged", ({ waveletId, blipId }) => {
      seen.push(live.isUnread(waveletId, blipId));
      if (blipId === "b+1") {
        // A write from a listener of an event the user-data wavelet caused.
        live.markBlipAsRead(ROOT, "b+2");
        expect(live.isUnread(ROOT, "b+2")).toBe(false);
      }
    });
    live.markBlipAsRead(ROOT, "b+1");
    expect(seen).toEqual([false, false]);
    expect(live.unreadCount(ROOT)).toBe(0);
  });

  it("dispose stops the events", () => {
    const device = new Device();
    const root = device.addRoot();
    device.live.markBlipAsRead(ROOT, "b+1");
    device.take();
    device.live.dispose();
    remoteEdit(root, "b+1", (doc) => doc.insertText(3, "x"));
    device.wave.userData()!.editDocument("m/muted", (doc) => {
      doc.createElement(0, "muted", [["muted", "true"]]);
    });
    device.wave.removeWavelet(ROOT);
    expect(device.take()).toEqual([]);
  });
});
