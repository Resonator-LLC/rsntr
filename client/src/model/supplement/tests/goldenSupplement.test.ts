// Scenarios of the supplement (the `*Scenario` methods of the generator;
// port of the Rust `supplement/golden_tests/supplement.rs`).
//
// The generator runs most scenarios twice, once keeping one
// `WaveletBasedSupplement` for the whole scenario (`<name>/live`) and once
// creating a new one in every step (`<name>/fresh`). The port has no
// long-lived supplement, so one run here is compared with both.

import { describe, it } from "vitest";
import { javaDoubleToString } from "../../adt";
import type { WaveletId } from "../../id";
import { HashedVersion } from "../../version";
import {
  PrimitiveSupplementImpl,
  SupplementedWaveImpl,
  TRASH_FOLDER,
  ThreadState,
  WantedEvaluation,
  WaveletBasedSupplement,
  WaveletDataView,
  type DefaultFollow,
} from "..";
import { OTHER, ROOT, UDW, Wave, alice, bob, list, nullable, wid } from "./goldenHarness";

type Sw = SupplementedWaveImpl<WaveletBasedSupplement, WaveletDataView>;

/** A wave with the supplement of its author (the generator's `Sup`). */
class Sup {
  readonly wave = new Wave();

  constructor(private readonly follow: DefaultFollow) {}

  /** A step that uses the supplement, which is created at its start. */
  step(label: string, step: (sw: Sw, answers: string[]) => void): void {
    this.wave.step(label, (w) => {
      w.wavelet(UDW);
      const view = new WaveletDataView(
        alice(),
        w.view.wavelets().map((wavelet) => wavelet.data()),
      );
      const answers: string[] = [];
      try {
        const primitive = WaveletBasedSupplement.create(w.wavelet(UDW));
        step(new SupplementedWaveImpl(primitive, view, this.follow), answers);
      } finally {
        for (const answer of answers) {
          w.q(answer);
        }
      }
    });
  }

  /** A step that creates the supplement and then changes the wave. */
  waveStep(label: string, step: (wave: Wave) => void): void {
    this.wave.step(label, (w) => {
      WaveletBasedSupplement.create(w.wavelet(UDW));
      step(w);
    });
  }

  edit(label: string, wavelet: string, blip: string): void {
    this.waveStep(label, (w) => w.edit(wavelet, blip, (doc) => doc.insertText(3, "x")));
  }

  append(label: string, doc: string, xml: string): void {
    this.waveStep(label, (w) => w.append(UDW, doc, xml));
  }
}

/** Builds a conversation with three blips, the third in a reply thread. */
function conversation(w: Wave, wavelet: string): void {
  w.append(
    wavelet,
    "conversation",
    '<conversation><blip id="b+1"></blip>' +
      '<blip id="b+2"><thread id="t+1"><blip id="b+3"></blip></thread></blip>' +
      "</conversation>",
  );
  for (const blip of ["b+1", "b+2", "b+3"]) {
    w.append(wavelet, blip, "<body><line></line>text</body>");
  }
}

function ids(wavelets: WaveletId[]): string {
  return list(wavelets.map((id) => id.id).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0)));
}

function unread(sw: Sw, a: string[], wavelet: string): void {
  const id = wid(wavelet);
  a.push(
    `unread ${wavelet} ${sw.isUnread(id, "b+1")} ${sw.isUnread(id, "b+2")} ` +
      `${sw.isUnread(id, "b+3")} participants ${sw.isParticipantsUnread(id)} ` +
      `${sw.haveParticipantsEverBeenRead(id)} tags ${sw.isTagsUnread(id)}`,
  );
  const prim = sw.primitive;
  a.push(
    `read versions ${wavelet} ${prim.lastReadBlipVersion(id, "b+1")} ` +
      `${prim.lastReadBlipVersion(id, "b+2")} ${prim.lastReadBlipVersion(id, "b+3")} ` +
      `${prim.lastReadParticipantsVersion(id)} ${prim.lastReadTagsVersion(id)} ` +
      `${prim.lastReadWaveletVersion(id)} ${list(prim.readBlips(id))} ` +
      ids(prim.readWavelets()),
  );
}

function inbox(sw: Sw, a: string[]): void {
  const prim = sw.primitive;
  a.push(
    `inbox ${sw.isInbox()} archived ${sw.isArchived()} mute ${sw.isMute()} ` +
      `followed ${sw.isFollowed()} ${nullable(prim.followed())} trashed ${sw.isTrashed()} ` +
      `folders ${list(sw.folders())}`,
  );
  a.push(
    `archive versions ${prim.archiveWaveletVersion(wid(ROOT))} ` +
      `${prim.archiveWaveletVersion(wid(OTHER))} ${ids(prim.archiveWavelets())}`,
  );
}

function seen(sw: Sw, a: string[]): void {
  const prim = sw.primitive;
  const root = wid(ROOT);
  const other = wid(OTHER);
  a.push(
    `seen ${sw.hasBeenSeen()} ${sw.seenVersion(root)} ${sw.seenVersion(other)} ` +
      `${ids(prim.seenWavelets())} notified ${prim.notifiedVersion(root)} ` +
      `${prim.notifiedVersion(other)} ${ids(prim.notifiedWavelets())} pending ` +
      `${sw.hasPendingNotification()} ${prim.pendingNotification()} ` +
      `${sw.supplement.hasPendingNotificationFor(root)} ` +
      `${sw.supplement.hasPendingNotificationFor(other)}`,
  );
}

function evaluation(
  wavelet: string,
  wanted: boolean,
  certainty: number,
  timestamp: number,
  agent: string,
  ignored: boolean,
): WantedEvaluation {
  return new WantedEvaluation({
    waveletId: wid(wavelet),
    adderAddress: "bob@example.com",
    wanted,
    certainty,
    timestamp,
    agentIdentity: agent,
    ignored,
    comment: 'a "comment"',
  });
}

function describeEvaluation(e: WantedEvaluation): string {
  return [
    e.waveletId?.id,
    e.adderAddress,
    e.wanted,
    javaDoubleToString(e.certainty),
    e.timestamp,
    e.agentIdentity,
    e.ignored,
    e.comment,
    e.javaHashCode(),
  ].join("/");
}

function wanted(sw: Sw, a: string[], wavelet: string): void {
  const set = sw.wantedEvaluationSet(wid(wavelet));
  const all = set.evaluations.map(describeEvaluation).sort((x, y) => (x < y ? -1 : x > y ? 1 : 0));
  a.push(
    `wanted ${wavelet} ${set.isWanted()} ${set.isIgnored()} ` +
      `${describeEvaluation(set.mostCertain())} ${describeEvaluation(set.mostRecent())} ` +
      list(all),
  );
}

/** Java's `TreeMap.toString`. */
function gadget(state: Map<string, string>): string {
  return `{${[...state].map(([k, v]) => `${k}=${v}`).join(", ")}}`;
}

describe("supplement golden", () => {
  it("sup-read", () => {
    const s = new Sup("always");
    const root = wid(ROOT);
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      w.wavelet(ROOT).addParticipant(alice());
      w.wavelet(ROOT).addParticipant(bob());
    });
    const show = (label: string): void => s.step(label, (sw, a) => unread(sw, a, ROOT));
    const readBlip = (label: string, blip: string): void =>
      s.step(label, (sw) => sw.markBlipAsRead(root, blip));
    const readParticipants = (label: string): void =>
      s.step(label, (sw) => sw.markParticipantsAsRead(root));
    const readTags = (label: string): void => s.step(label, (sw) => sw.markTagsAsRead(root));
    const readWave = (label: string): void => s.step(label, (sw) => sw.markAsRead());
    show("initial");
    readBlip("read b+1", "b+1");
    show("after read b+1");
    readBlip("read b+1 again", "b+1");
    s.edit("edit b+1", ROOT, "b+1");
    show("after edit b+1");
    readBlip("read b+1 after edit", "b+1");
    readBlip("read b+2", "b+2");
    show("after read b+2");
    readParticipants("read participants");
    readTags("read tags");
    show("after read participants and tags");
    readParticipants("read participants again");
    s.waveStep("remove bob", (w) => w.wavelet(ROOT).removeParticipant(bob()));
    show("after remove bob");
    readParticipants("read participants after change");
    readTags("read tags after change");
    readWave("read wave");
    show("after read wave");
    readWave("read wave again");
    readBlip("read b+3 when wave read", "b+3");
    s.edit("edit b+3", ROOT, "b+3");
    show("after edit b+3");
    readBlip("read b+3", "b+3");
    readWave("read wave after edit");
    show("after read wave after edit");
    s.step("mark blip read at old version", (sw) => sw.supplement.markBlipAsRead(root, "b+2", 3));
    s.step("mark blip read at future version", (sw) =>
      sw.supplement.markBlipAsRead(root, "b+2", 1000),
    );
    s.step("clear blip read state", (sw) => sw.primitive.clearBlipReadState(root, "b+1"));
    show("after clear blip read state");
    s.step("unread", (sw) => sw.markAsUnread());
    show("after unread");
    s.step("unread again", (sw) => sw.markAsUnread());
    readBlip("read b+2 after unread", "b+2");
    show("at the end");
    s.step("copy", (sw, a) => {
      const copy = PrimitiveSupplementImpl.copyOf(sw.primitive);
      a.push(
        `${copy.lastReadBlipVersion(root, "b+1")} ${copy.lastReadBlipVersion(root, "b+2")} ` +
          `${copy.lastReadParticipantsVersion(root)} ${copy.lastReadTagsVersion(root)} ` +
          `${copy.lastReadWaveletVersion(root)} ${ids(copy.readWavelets())}`,
      );
    });
    s.wave.check("sup-read/live");
    s.wave.check("sup-read/fresh");
  });

  it("sup-inbox", () => {
    const s = new Sup("always");
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      w.wavelet(ROOT).addParticipant(alice());
    });
    const show = (label: string): void => s.step(label, (sw, a) => inbox(sw, a));
    const archive = (label: string): void => s.step(label, (sw) => sw.archive());
    const folder = (label: string, id: number): void =>
      s.step(label, (sw) => sw.moveToFolder(id));
    show("initial");
    archive("archive");
    show("after archive");
    archive("archive again");
    s.edit("edit", ROOT, "b+1");
    show("after edit");
    archive("archive after edit");
    show("after archive after edit");
    s.step("mute", (sw) => sw.mute());
    show("after mute");
    s.step("mute again", (sw) => sw.mute());
    archive("archive when muted");
    s.step("follow", (sw) => sw.follow());
    show("after follow");
    archive("archive at same version after clear");
    show("after archive at same version");
    s.step("inbox", (sw) => sw.inbox());
    show("after inbox");
    s.step("inbox again", (sw) => sw.inbox());
    folder("trash", TRASH_FOLDER);
    show("after trash");
    folder("move to folder 5", 5);
    show("after move to folder 5");
    const addFolder = (label: string, id: number): void =>
      s.step(label, (sw) => sw.primitive.addFolder(id));
    addFolder("add folder 12", 12);
    addFolder("add folder 12 again", 12);
    addFolder("add folder 2", 2);
    s.step("after add folders", (sw, a) => {
      inbox(sw, a);
      a.push(`${sw.primitive.isInFolder(12)} ${sw.primitive.isInFolder(13)}`);
    });
    for (const label of ["remove folder 5", "remove folder 5 again"]) {
      s.step(label, (sw) => sw.primitive.removeFolder(5));
    }
    folder("move to all folder", 3);
    show("after move to all folder");
    folder("move to folder 6", 6);
    folder("move to inbox folder", 1);
    show("after move to inbox folder");
    s.step("unfollow", (sw) => sw.unfollow());
    s.step("clear follow", (sw) => sw.primitive.clearFollow());
    show("after clear follow");
    s.step("clear follow again", (sw) => sw.primitive.clearFollow());
    s.step("clear archive", (sw) => sw.supplement.clearArchive());
    s.step("archive at lower version", (sw) => sw.supplement.archive(wid(ROOT), 2));
    show("after archive at lower version");
    s.wave.check("sup-inbox/live");
    s.wave.check("sup-inbox/fresh");
  });

  it("sup-follow", () => {
    const s = new Sup("whenParticipant");
    s.wave.step("setup", (w) => conversation(w, ROOT));
    const show = (label: string): void => s.step(label, (sw, a) => inbox(sw, a));
    show("not a participant");
    s.step("archive when not followed", (sw) => sw.archive());
    s.step("follow", (sw) => sw.follow());
    show("after follow");
    s.step("clear follow", (sw) => sw.primitive.clearFollow());
    s.waveStep("bob joins", (w) => w.wavelet(ROOT).addParticipant(bob()));
    show("after bob joins");
    s.waveStep("alice joins", (w) => w.wavelet(ROOT).addParticipant(alice()));
    show("after alice joins");
    s.step("unfollow", (sw) => sw.unfollow());
    show("after unfollow");
    s.wave.check("sup-follow/live");
    s.wave.check("sup-follow/fresh");
  });

  it("sup-multi", () => {
    const s = new Sup("always");
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      conversation(w, OTHER);
      // Conversational by id, but without a manifest.
      w.append("conv+empty", "b+1", "<body><line></line></body>");
      w.append("other+data", "conversation", "<conversation></conversation>");
      w.wavelet(OTHER).addParticipant(alice());
    });
    const showUnread = (label: string): void =>
      s.step(label, (sw, a) => {
        unread(sw, a, ROOT);
        unread(sw, a, OTHER);
      });
    const showInbox = (label: string): void => s.step(label, (sw, a) => inbox(sw, a));
    const showSeen = (label: string): void => s.step(label, (sw, a) => seen(sw, a));
    s.step("initial", (sw, a) => {
      unread(sw, a, ROOT);
      unread(sw, a, OTHER);
      inbox(sw, a);
      seen(sw, a);
    });
    s.step("read wave", (sw) => sw.markAsRead());
    showUnread("after read wave");
    s.edit("edit other", OTHER, "b+2");
    showUnread("after edit other");
    s.step("read wave after edit", (sw) => sw.markAsRead());
    s.step("archive", (sw) => sw.archive());
    showInbox("after archive");
    s.edit("edit root", ROOT, "b+1");
    showInbox("after edit root");
    s.step("archive after edit", (sw) => sw.archive());
    showInbox("after archive after edit");
    s.step("see at version zero", (sw) => sw.see());
    showSeen("after see at version zero");
    s.waveStep("sign", (w) => {
      w.wavelet(ROOT)
        .data()
        .setHashedVersion(HashedVersion.of(40, Uint8Array.of(1, 2, 3)));
      w.wavelet(OTHER)
        .data()
        .setHashedVersion(HashedVersion.of(41, Uint8Array.of(0xff, 0, 7, 9)));
    });
    s.step("see root", (sw) => sw.seeWavelet(wid(ROOT)));
    showSeen("after see root");
    s.step("see", (sw) => sw.see());
    showSeen("after see");
    s.step("see again", (sw) => sw.see());
    s.step("notify", (sw) => sw.markAsNotified());
    showSeen("after notify");
    s.waveStep("sign later", (w) => {
      w.wavelet(OTHER)
        .data()
        .setHashedVersion(HashedVersion.of(90, Uint8Array.of(5)));
    });
    s.step("see other", (sw) => sw.seeWavelet(wid(OTHER)));
    showSeen("after see other");
    for (const [label, version] of [
      ["notified at 200", 200],
      ["notified at 100", 100],
    ] as const) {
      s.step(label, (sw) => sw.primitive.setNotifiedVersion(wid(OTHER), version));
    }
    showSeen("after notified");
    const seenLow = (label: string): void =>
      s.step(label, (sw) =>
        sw.primitive.setSeenVersion(wid(OTHER), HashedVersion.of(3, Uint8Array.of(9))),
      );
    seenLow("seen at lower version");
    s.step("clear seen other", (sw) => sw.primitive.clearSeenVersion(wid(OTHER)));
    showSeen("after clear seen other");
    seenLow("seen at lower version after clear");
    showSeen("at the end");
    s.step("unread", (sw) => sw.markAsUnread());
    s.wave.check("sup-multi/live");
    s.wave.check("sup-multi/fresh");
  });

  it("sup-misc", () => {
    const s = new Sup("always");
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      // An empty wavelet, to ask for the evaluations of.
      w.wavelet(OTHER);
    });
    const root = wid(ROOT);
    const other = wid(OTHER);

    s.step("no thread state", (sw, a) => {
      const prim = sw.primitive;
      a.push(
        `${nullable(prim.threadState(root, "t+1"))} ${list(prim.statefulThreads(root))} ` +
          ids(prim.waveletsWithThreadState()),
      );
    });
    const thread = (label: string, wavelet: string, id: string, state: ThreadState | null): void =>
      s.step(label, (sw) => sw.primitive.setThreadState(wid(wavelet), id, state));
    const collapsed = ThreadState.Collapsed;
    const expanded = ThreadState.Expanded;
    thread("remove thread state of unknown wavelet", ROOT, "t+1", null);
    s.step("collapse t+1", (sw, a) => {
      sw.setThreadState(root, "t+1", collapsed);
      a.push(nullable(sw.threadState(root, "t+1")));
    });
    thread("collapse t+1 again", ROOT, "t+1", collapsed);
    thread("expand t+1", ROOT, "t+1", expanded);
    thread("collapse t+2", ROOT, "t+2", collapsed);
    thread("collapse in other", OTHER, "t+9", collapsed);
    s.step("thread states", (sw, a) => {
      const prim = sw.primitive;
      a.push(
        [
          nullable(prim.threadState(root, "t+1")),
          nullable(prim.threadState(root, "t+2")),
          nullable(prim.threadState(root, "t+3")),
          nullable(prim.threadState(other, "t+9")),
          list(prim.statefulThreads(root)),
          list(prim.statefulThreads(other)),
          ids(prim.waveletsWithThreadState()),
        ].join(" "),
      );
    });
    thread("remove t+1", ROOT, "t+1", null);
    thread("remove t+1 again", ROOT, "t+1", null);
    s.step("thread states after remove", (sw, a) => {
      const prim = sw.primitive;
      a.push(`${nullable(prim.threadState(root, "t+1"))} ${list(prim.statefulThreads(root))}`);
    });

    s.step("no gadget state", (sw, a) => {
      a.push(`${gadget(sw.gadgetState("g1"))} ${nullable(sw.gadgetStateValue("g1", "k"))}`);
    });
    const setGadget = (label: string, id: string, key: string, value: string | null): void =>
      s.step(label, (sw) => sw.setGadgetState(id, key, value));
    setGadget("gadget remove on unknown gadget", "g0", "k", null);
    setGadget("gadget set", "g1", "k", "v");
    setGadget("gadget set same", "g1", "k", "v");
    setGadget("gadget set other key", "g1", "k2", 'v <2> & "more"');
    setGadget("gadget replace", "g1", "k", "w");
    setGadget("gadget in other", "g 2", "k", "x");
    s.step("gadget states", (sw, a) => {
      a.push(
        [
          gadget(sw.gadgetState("g0")),
          gadget(sw.gadgetState("g1")),
          gadget(sw.gadgetState("g 2")),
          nullable(sw.gadgetStateValue("g1", "k2")),
          nullable(sw.gadgetStateValue("g1", "zz")),
        ].join(" "),
      );
    });
    setGadget("gadget remove", "g1", "k", null);
    s.step("gadget states after remove", (sw, a) => void a.push(gadget(sw.gadgetState("g1"))));

    s.step("no evaluations", (sw, a) => wanted(sw, a, ROOT));
    const add = (label: string, e: WantedEvaluation): void =>
      s.step(label, (sw) => sw.addWantedEvaluation(e));
    add("evaluation 1", evaluation(ROOT, true, 0.5, 1000, "agent1", false));
    add("evaluation 2", evaluation(ROOT, false, 0.75, 900, "agent2", true));
    s.step("evaluations", (sw, a) => wanted(sw, a, ROOT));
    for (const label of ["evaluation 3", "evaluation 3 again"]) {
      add(label, evaluation(ROOT, true, 1.0e-4, 2000, "agent3", false));
    }
    add("evaluation in other", evaluation(OTHER, false, 1.0e7, 1234567890123, "agent1", false));
    add("evaluation 4", evaluation(ROOT, true, 0.75, 900, "agent4", false));
    s.step("more evaluations", (sw, a) => {
      wanted(sw, a, ROOT);
      wanted(sw, a, OTHER);
      a.push(String(sw.primitive.wantedEvaluations().length));
    });
    s.step("odd certainties", (sw) => {
      const certainties = [
        0.0,
        1.0,
        100.0,
        1234567.125,
        12345678.5,
        0.001,
        0.00099,
        1e21,
        1e-7,
        0.1 + 0.2,
        123456789012345680.0,
        4.9e-324,
        1.7976931348623157e308,
        0.5e-3,
        9999999.999999998,
        1.0e-5,
        3.0e10,
      ];
      for (const certainty of certainties) {
        sw.addWantedEvaluation(evaluation(OTHER, true, certainty, 1, "odd", false));
      }
      sw.addWantedEvaluation(evaluation(OTHER, true, -2.5, -7, "odd", false));
    });
    s.step("all evaluations", (sw, a) => wanted(sw, a, OTHER));
    s.wave.check("sup-misc/live");
    s.wave.check("sup-misc/fresh");
  });

  it("sup-corrupt", () => {
    const s = new Sup("always");
    const root = wid(ROOT);
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      w.wavelet(ROOT).addParticipant(alice());
    });
    const showUnread = (label: string): void => s.step(label, (sw, a) => unread(sw, a, ROOT));
    const showInbox = (label: string): void => s.step(label, (sw, a) => inbox(sw, a));
    const showSeen = (label: string): void => s.step(label, (sw, a) => seen(sw, a));
    s.step("empty", (sw, a) => {
      unread(sw, a, ROOT);
      inbox(sw, a);
      seen(sw, a);
    });
    s.append(
      "corrupt read state",
      "m/read",
      '<wavelet i="example.com/conv+root"><all v="5"></all><participants v="3">' +
        '</participants><blip i="b+1" v="2"></blip><all v="9"></all>' +
        '<blip i="b+1" v="7"></blip><blip i="b+1" v="4"></blip>' +
        '<blip i="b+2" v="1"></blip><all v="7"></all><tags v="1"></tags>' +
        '<tags v="1"></tags><junk></junk><blip i="b+9" v="7"></blip></wavelet>' +
        '<wavelet i="example.com/conv+root"><all v="100"></all>' +
        '<blip i="b+3" v="50"></blip></wavelet><wavelet></wavelet><other></other>' +
        '<wavelet i="example.com/conv+other"><participants v="8"></participants>' +
        '<participants v="2"></participants></wavelet>',
    );
    s.step("corrupt read state read", (sw, a) => {
      unread(sw, a, ROOT);
      const prim = sw.primitive;
      a.push(
        `${prim.lastReadParticipantsVersion(wid(OTHER))} ${prim.lastReadBlipVersion(root, "b+9")}`,
      );
    });
    const readBlip = (label: string, blip: string, version: number): void =>
      s.step(label, (sw) => sw.primitive.setLastReadBlipVersion(root, blip, version));
    readBlip("read blip at lower version", "b+1", 6);
    readBlip("~read blip cleans up", "b+2", 5);
    s.step("~read wavelet cleans up", (sw) => sw.primitive.setLastReadWaveletVersion(root, 12));
    s.step("~read tags cleans up", (sw) => sw.primitive.setLastReadTagsVersion(root, 2));
    s.step("~read participants of other cleans up", (sw) =>
      sw.primitive.setLastReadParticipantsVersion(wid(OTHER), 9),
    );
    showUnread("after cleaning up");

    s.append(
      "corrupt muted",
      "m/muted",
      '<muted muted="false"></muted><x></x><muted muted="true"></muted><muted></muted>',
    );
    showInbox("corrupt muted read");
    s.step("unfollow when muted", (sw) => sw.unfollow());
    s.step("follow cleans up", (sw) => sw.follow());
    s.append(
      "corrupt muted again",
      "m/muted",
      '<muted muted="TRUE"></muted><muted muted="false"></muted>',
    );
    s.step("clear follow cleans up", (sw) => sw.primitive.clearFollow());
    showInbox("after clear follow");

    s.waveStep("corrupt archive", (w) => {
      w.append(
        UDW,
        "m/archiving",
        '<archive i="example.com/conv+root" v="3"></archive>' +
          '<archive i="example.com/conv+root" v="8"></archive>' +
          '<archive i="example.com/conv+root" v="8"></archive>',
      );
      w.append(
        UDW,
        "m/cleared",
        '<cleared cleared="true"></cleared>' +
          '<cleared cleared="false"></cleared><cleared cleared="true"></cleared>',
      );
    });
    showInbox("corrupt archive read");
    const archive = (label: string, version: number): void =>
      s.step(label, (sw) => sw.supplement.archive(root, version));
    archive("archive when cleared", 5);
    showInbox("after archive when cleared");
    archive("~archive cleans up", 30);
    showInbox("after archive");

    s.append(
      "corrupt seen",
      "m/seen",
      '<seen i="example.com/conv+root" signature="5:AQI="></seen>' +
        '<seen i="example.com/conv+root" signature="5:AQM="></seen>' +
        '<seen i="example.com/conv+other" signature="7"></seen>' +
        '<notified i="example.com/conv+root" v="4"></notified>' +
        '<notified i="example.com/conv+root" v="2"></notified>' +
        '<notification pending="true"></notification>' +
        '<notification pending="true"></notification>',
    );
    showSeen("corrupt seen read");
    s.step("see cleans up", (sw) => sw.see());
    showSeen("after see");

    s.append(
      "corrupt presentation",
      "m/presentation",
      '<conversation i="example.com/conv+root"><thread i="t+1" state="COLLAPSED">' +
        '</thread><thread i="t+2" state="EXPANDED"></thread>' +
        '<thread i="t+1" state="EXPANDED"></thread></conversation>' +
        '<conversation i="example.com/conv+root"><thread i="t+3" state="COLLAPSED">' +
        "</thread></conversation>",
    );
    s.step("corrupt presentation read", (sw, a) => {
      const prim = sw.primitive;
      a.push(
        [
          nullable(prim.threadState(root, "t+1")),
          nullable(prim.threadState(root, "t+2")),
          nullable(prim.threadState(root, "t+3")),
          list(prim.statefulThreads(root)),
        ].join(" "),
      );
    });
    s.step("~thread state cleans up", (sw) =>
      sw.primitive.setThreadState(root, "t+2", ThreadState.Collapsed),
    );
    s.step("remove thread state", (sw) => sw.primitive.setThreadState(root, "t+1", null));

    s.append(
      "corrupt gadgets",
      "m/gadgets",
      '<gadget i="g1"><state name="a" value="1"></state><state name="b" value="2">' +
        '</state><state name="a" value="3"></state></gadget><gadget></gadget>' +
        '<gadget i="g1"><state name="c" value="4"></state></gadget>',
    );
    const showGadget = (label: string): void =>
      s.step(label, (sw, a) => void a.push(gadget(sw.primitive.gadgetState("g1"))));
    showGadget("corrupt gadgets read");
    s.step("gadget remove cleans up", (sw) => sw.primitive.setGadgetState("g1", "zz", null));
    showGadget("after gadget remove");

    s.append(
      "corrupt abuse",
      "m/abuse",
      '<wanted wavelet_id="example.com/conv+root"></wanted><x></x>' +
        '<wanted wavelet_id="example.com/conv+root" wanted="TRUE" certainty=" 1e1 " ' +
        'timestamp="+12" ignored="yes" adder="a" agent="b" comment="c"></wanted>' +
        '<wanted wavelet_id="example.com/conv+root"></wanted>',
    );
    s.step("corrupt abuse read", (sw, a) => wanted(sw, a, ROOT));
    s.wave.check("sup-corrupt/live");
    s.wave.check("sup-corrupt/fresh");
  });

  it("sup-legacy-folders", () => {
    const s = new Sup("always");
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      w.append(
        UDW,
        "m/folder",
        '<folder i="5"></folder><folder i="1"></folder>' +
          '<x></x><folder i="5"></folder><folder i="3"></folder><folder i="9"></folder>',
      );
    });
    const show = (label: string): void => s.step(label, (sw, a) => inbox(sw, a));
    show("create removes legacy folders");
    s.step("add legacy folder", (sw, a) => {
      sw.primitive.addFolder(3);
      a.push(list(sw.primitive.folders()));
    });
    show("next create removes it");
    s.step("add folders", (sw) => {
      sw.primitive.addFolder(2);
      sw.primitive.addFolder(15);
      sw.primitive.addFolder(0);
    });
    for (const label of ["remove all folders", "remove all folders again"]) {
      s.step(label, (sw) => sw.primitive.removeAllFolders());
    }
    s.wave.check("sup-legacy-folders");
  });

  it("sup-clear-duplicates", () => {
    const s = new Sup("always");
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      w.append(
        UDW,
        "m/read",
        '<wavelet i="example.com/conv+root"><all v="1"></all></wavelet>' +
          '<wavelet i="example.com/conv+other"><all v="2"></all></wavelet>' +
          '<wavelet i="example.com/conv+root"><all v="100"></all>' +
          '<blip i="b+3" v="50"></blip></wavelet>',
      );
    });
    const show = (label: string): void => s.step(label, (sw, a) => unread(sw, a, ROOT));
    show("before");
    s.step("~unread", (sw) => sw.markAsUnread());
    show("after");
    s.step("unread again", (sw) => sw.markAsUnread());
    show("at the end");
    s.wave.check("sup-clear-duplicates");
  });

  it("sup-udw-participants", () => {
    const s = new Sup("always");
    s.wave.step("setup", (w) => {
      conversation(w, ROOT);
      w.wavelet(UDW).addParticipant(bob());
    });
    s.step("follow", (sw) => sw.follow());
    s.step("unfollow", (sw) => sw.unfollow());
    s.wave.check("sup-udw-participants");
  });
});
