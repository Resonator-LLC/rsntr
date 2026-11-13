// Tests of the optimistic read state: a port of
// `org.waveprotocol.wave.client.wave.LocalSupplementedWaveImplTest` of
// Apache Wave (Apache License 2.0) for `LocalReadState`, and tests of its
// binding to a wave, `LocalSupplementedWave`.

import { beforeEach, describe, expect, it } from "vitest";
import { WaveletData } from "../../data";
import { XmlFragment, type MutableDocument } from "../../doc";
import { ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import { WaveletOperation, WaveletOperationContext } from "../../operation";
import { HashedVersion } from "../../version";
import { OpBasedWavelet, WaveView } from "../../wavelet";
import {
  EVICT_TIME_MS,
  LocalReadState,
  LocalSupplementedWave,
  REPEAT_MS,
  type LocalTimers,
  type ReadStateDelegate,
} from "..";

const wid = (id: string): WaveletId => WaveletId.of("example.com", id);
const ROOT = wid("conv+root");

/** `FakeTimerService`: a clock that runs the timers due when it is advanced. */
class FakeTimers implements LocalTimers {
  time = 0;
  private next = 1;
  private readonly tasks = new Map<number, { at: number; run: () => void }>();

  setTimeout(callback: () => void, delayMs: number): unknown {
    const handle = this.next++;
    this.tasks.set(handle, { at: this.time + delayMs, run: callback });
    return handle;
  }

  clearTimeout(handle: unknown): void {
    this.tasks.delete(handle as number);
  }

  pending(): number {
    return this.tasks.size;
  }

  tick(ms: number): void {
    const end = this.time + ms;
    for (;;) {
      let due: [number, { at: number; run: () => void }] | null = null;
      for (const entry of this.tasks) {
        if (entry[1].at <= end && (due === null || entry[1].at < due[1].at)) {
          due = entry;
        }
      }
      if (due === null) {
        break;
      }
      this.tasks.delete(due[0]);
      this.time = due[1].at;
      due[1].run();
    }
    this.time = end;
  }
}

/** A mock supplement that counts the calls. */
class MockSupplement implements ReadStateDelegate {
  unread = false;
  marked: string[] = [];
  markedUnread = 0;

  isUnread(): boolean {
    return this.unread;
  }

  markBlipAsRead(_waveletId: WaveletId, blipId: string): void {
    this.marked.push(blipId);
  }

  markAsUnread(): void {
    this.markedUnread++;
  }

  reset(): void {
    this.marked = [];
    this.markedUnread = 0;
  }

  times(blipId: string): number {
    return this.marked.filter((id) => id === blipId).length;
  }
}

describe("LocalReadState (LocalSupplementedWaveImplTest)", () => {
  const blip = "b+1";
  let supplement: MockSupplement;
  let timer: FakeTimers;
  let target: LocalReadState;

  beforeEach(() => {
    supplement = new MockSupplement();
    timer = new FakeTimers();
    target = new LocalReadState(supplement, { timers: timer, clock: () => timer.time });
  });

  it("reading actions are scoped", () => {
    target.startReading(ROOT, blip);
    expect(() => target.startReading(ROOT, blip)).toThrow();
    target.stopReading(ROOT, blip);
    expect(() => target.stopReading(ROOT, blip)).toThrow();
  });

  it("stopReading takes the blip being read", () => {
    target.startReading(ROOT, blip);
    expect(() => target.stopReading(ROOT, "b+2")).toThrow();
    expect(target.readingBlip()).toEqual({ waveletId: ROOT, blipId: blip });
  });

  it("the reading blip is always read, otherwise it is what the supplement says", () => {
    supplement.unread = true;

    target.startReading(ROOT, blip);
    expect(target.isUnread(ROOT, blip)).toBe(false);

    // Stop reading, fast-forwarding to when auto-reading has ceased.
    target.stopReading(ROOT, blip);
    timer.tick(EVICT_TIME_MS);
    expect(target.isUnread(ROOT, blip)).toBe(true);
  });

  it("a non-reading blip is what the supplement says", () => {
    supplement.unread = false;
    expect(target.isUnread(ROOT, blip)).toBe(false);
    supplement.unread = true;
    expect(target.isUnread(ROOT, blip)).toBe(true);
  });

  it("stopReading causes auto-reading until evicted", () => {
    target.startReading(ROOT, blip);
    target.stopReading(ROOT, blip);

    supplement.reset();
    const invocations = EVICT_TIME_MS / REPEAT_MS;
    for (let i = 0; i < invocations; i++) {
      timer.tick(REPEAT_MS);
    }
    expect(supplement.times(blip)).toBe(invocations);

    supplement.reset();
    timer.tick(REPEAT_MS);
    expect(supplement.times(blip)).toBe(0);
  });

  it("markAsRead causes auto-reading until evicted", () => {
    target.markBlipAsRead(ROOT, blip);

    supplement.reset();
    const invocations = EVICT_TIME_MS / REPEAT_MS;
    for (let i = 0; i < invocations; i++) {
      timer.tick(REPEAT_MS);
    }
    expect(supplement.times(blip)).toBe(invocations);

    supplement.reset();
    timer.tick(REPEAT_MS);
    expect(supplement.times(blip)).toBe(0);
    // Nothing is left to do: the timer rests.
    expect(timer.pending()).toBe(0);
  });

  it("a remote op stops auto-reading", () => {
    target.markBlipAsRead(ROOT, blip);

    timer.tick(REPEAT_MS);
    supplement.reset();
    target.onRemoteChange(ROOT, blip);
    // Expect that target removed the blip from the auto-read collection.
    timer.tick(REPEAT_MS);
    expect(supplement.times(blip)).toBe(0);
  });

  it("a remote op does not stop reading the blip being read", () => {
    supplement.unread = true;
    target.startReading(ROOT, blip);
    target.onRemoteChange(ROOT, blip);
    expect(target.isUnread(ROOT, blip)).toBe(false);
    supplement.reset();
    timer.tick(REPEAT_MS);
    // Once as the reading blip and once as a member, as in the original.
    expect(supplement.times(blip)).toBe(2);
  });

  it("auto-read supports many blips", () => {
    const blip2 = "b+2";

    target.markBlipAsRead(ROOT, blip);
    target.markBlipAsRead(ROOT, blip2);

    timer.tick(REPEAT_MS);
    expect(supplement.times(blip)).toBe(2);
    expect(supplement.times(blip2)).toBe(2);
  });

  it("blips of the same id in different wavelets are distinct", () => {
    supplement.unread = true;
    target.markBlipAsRead(ROOT, blip);
    expect(target.isUnread(ROOT, blip)).toBe(false);
    expect(target.isUnread(wid("conv+other"), blip)).toBe(true);
  });

  it("markAsRead refreshes the eviction time", () => {
    target.markBlipAsRead(ROOT, blip);
    // Fast-forward to near eviction time.
    timer.tick(EVICT_TIME_MS - 1);

    // Refresh, and expect a full set of future markings.
    target.markBlipAsRead(ROOT, blip);
    supplement.reset();
    const invocations = EVICT_TIME_MS / REPEAT_MS;
    for (let i = 0; i < invocations; i++) {
      timer.tick(REPEAT_MS);
    }
    expect(supplement.times(blip)).toBe(invocations);
  });

  it("markAsUnread clears auto-reading but continues reading the active blip", () => {
    const other = "b+2";
    target.startReading(ROOT, blip);
    target.markBlipAsRead(ROOT, other);

    supplement.reset();
    target.markAsUnread();
    expect(supplement.markedUnread).toBe(1);
    expect(supplement.times(other)).toBe(0);
    expect(supplement.times(blip)).toBe(1);
  });

  it("destroy stops the timer", () => {
    target.markBlipAsRead(ROOT, blip);
    target.destroy();
    supplement.reset();
    timer.tick(EVICT_TIME_MS);
    expect(supplement.marked).toEqual([]);
    expect(timer.pending()).toBe(0);
  });
});

// ---- bound to a wave ----

const TIME = 1234567890123;
const WAVE = WaveId.of("example.com", "w+wave");
const alice = ParticipantId.of("alice@example.com");
const bob = ParticipantId.of("bob@example.com");

function remoteEdit(wavelet: OpBasedWavelet, docId: string, edit: (doc: MutableDocument) => void): void {
  const scratch = new OpBasedWavelet(wavelet.data().clone(), bob, {
    timestamp: TIME,
    participation: "ignorant",
  });
  scratch.editDocument(docId, edit);
  for (const op of scratch.takeOps()) {
    const context = new WaveletOperationContext(bob, TIME, 1);
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

function setUp(writable: () => boolean = () => true) {
  const timers = new FakeTimers();
  const wave = new WaveView(WAVE, alice, { timestamp: TIME });
  const live = new LocalSupplementedWave(wave, {
    timers,
    clock: () => timers.time,
    writable,
    onListenerError: (error) => {
      throw error;
    },
  });
  const name = WaveletName.of(WAVE, ROOT);
  const root = wave.addWavelet(WaveletData.empty(name, bob, HashedVersion.unsigned(0), TIME));
  root.consumeRemote(WaveletOperation.addParticipant(new WaveletOperationContext(bob, TIME, 1), bob));
  root.consumeRemote(WaveletOperation.addParticipant(new WaveletOperationContext(bob, TIME, 1), alice));
  remoteAppend(root, "conversation", '<conversation><blip id="b+1"></blip><blip id="b+2"></blip></conversation>');
  remoteAppend(root, "b+1", "<body><line></line>one</body>");
  remoteAppend(root, "b+2", "<body><line></line>two</body>");
  const events: string[] = [];
  live.events.on("maybeBlipReadChanged", ({ blipId }) => events.push(blipId));
  return { timers, wave, live, root, events };
}

describe("LocalSupplementedWave", () => {
  it("a blip marked as read stays read when the viewer's own edit is acknowledged", () => {
    const { live, root, timers } = setUp();
    live.markBlipAsRead(ROOT, "b+1");
    root.takeOps();
    expect(live.unreadBlips(ROOT)).toEqual(["b+2"]);

    root.editDocument("b+1", (doc) => doc.insertText(3, "y"));
    acknowledge(root);
    // The versions say unread; the optimistic state says read.
    expect(live.state().isUnread(ROOT, "b+1")).toBe(true);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    expect(live.unreadBlips(ROOT)).toEqual(["b+2"]);
    expect(live.unreadCount(ROOT)).toBe(1);

    // The repetition records the acknowledged version as read.
    timers.tick(REPEAT_MS);
    expect(live.state().isUnread(ROOT, "b+1")).toBe(false);
    // And it stays read after eviction.
    timers.tick(EVICT_TIME_MS);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    expect(timers.pending()).toBe(0);
  });

  it("the blip being read stays read whatever happens, and for a while after", () => {
    const { live, root, timers, events } = setUp();
    live.startReading(ROOT, "b+1");
    expect(events).toContain("b+1");
    expect(live.readingBlip()).toEqual({ waveletId: ROOT, blipId: "b+1" });
    root.editDocument("b+1", (doc) => doc.insertText(3, "y"));
    acknowledge(root);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    // Somebody else's change arrives while the viewer looks at the blip.
    remoteEdit(root, "b+1", (doc) => doc.insertText(3, "z"));
    expect(live.isUnread(ROOT, "b+1")).toBe(false);

    // Leaving records the current version; own edits acknowledged late
    // are still covered.
    root.editDocument("b+1", (doc) => doc.insertText(3, "w"));
    live.stopReading(ROOT, "b+1");
    expect(live.readingBlip()).toBeNull();
    acknowledge(root);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    timers.tick(REPEAT_MS);
    expect(live.state().isUnread(ROOT, "b+1")).toBe(false);
  });

  it("a remote change makes an auto-read blip unread, with an event", () => {
    const { live, root, events } = setUp();
    live.markBlipAsRead(ROOT, "b+1");
    events.length = 0;
    let unreadAtEvent: boolean | null = null;
    live.events.on("maybeBlipReadChanged", ({ blipId }) => {
      if (blipId === "b+1") {
        unreadAtEvent = live.isUnread(ROOT, "b+1");
      }
    });
    remoteEdit(root, "b+1", (doc) => doc.insertText(3, "z"));
    expect(live.isUnread(ROOT, "b+1")).toBe(true);
    expect(events).toEqual(["b+1"]);
    expect(unreadAtEvent).toBe(true);
  });

  it("an unworthy remote change (a reply anchor) does not stop auto-reading", () => {
    const { live, root } = setUp();
    live.markBlipAsRead(ROOT, "b+1");
    remoteEdit(root, "b+1", (doc) => void doc.insertXml(3, XmlFragment.parse('<reply id="t+1"></reply>')));
    root.takeOps();
    root.editDocument("b+1", (doc) => doc.insertText(3, "y"));
    acknowledge(root);
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
  });

  it("marking a read blip writes nothing and creates no user-data wavelet", () => {
    const { live, wave, timers } = setUp();
    live.markBlipAsRead(ROOT, "b+7");
    expect(wave.userData()).toBeNull();
    live.markBlipAsRead(ROOT, "b+1");
    const udw = wave.userData()!;
    udw.takeOps();
    timers.tick(EVICT_TIME_MS);
    expect(udw.takeOps()).toEqual([]);
  });

  it("while not writable, nothing is written but the blip reads as read", () => {
    let writable = false;
    const { live, wave, timers } = setUp(() => writable);
    live.startReading(ROOT, "b+1");
    expect(wave.userData()).toBeNull();
    expect(live.isUnread(ROOT, "b+1")).toBe(false);
    writable = true;
    timers.tick(REPEAT_MS);
    expect(wave.userData()).not.toBeNull();
    expect(live.state().isUnread(ROOT, "b+1")).toBe(false);
  });

  it("markAsUnread makes everything unread but the blip being read", () => {
    const { live } = setUp();
    live.startReading(ROOT, "b+1");
    live.markBlipAsRead(ROOT, "b+2");
    live.markAsUnread();
    expect(live.unreadBlips(ROOT)).toEqual(["b+2"]);
  });

  it("follows a wavelet that is replaced, and stops when disposed", () => {
    const { live, wave, root, timers } = setUp();
    live.markBlipAsRead(ROOT, "b+1");
    const replaced = wave.replaceWavelet(root.data().clone());
    remoteEdit(replaced, "b+1", (doc) => doc.insertText(3, "z"));
    expect(live.isUnread(ROOT, "b+1")).toBe(true);
    live.dispose();
    expect(timers.pending()).toBe(0);
  });
});
