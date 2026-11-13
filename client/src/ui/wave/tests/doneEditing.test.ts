// The "finished editing" signal: when an edit session that changed its
// blip ends, the store sends one no-op on the conversation's wavelet
// (`OpenWave.signalDoneEditing`), which the server's AI agents take as
// "answer now". Two users on the simulated server.

import { afterEach, describe, expect, test, vi } from "vitest";

import { BackoffScheduler } from "../../../client/backoffScheduler";
import { LiveWaveView } from "../../../client/liveWaveView";
import { OpenWave } from "../../../client/openWave";
import { RemoteWaveViewService } from "../../../client/remoteWaveViewService";
import { SimLoop, SimServer } from "../../../client/testing/simulation";
import { IdGeneratorImpl, ParticipantId, WaveId } from "../../../model/id";
import { digestOfWave } from "../../search/liveDigest";
import { ConversationStore } from "../store";

const DOMAIN = "example.com";
const WAVE = WaveId.of(DOMAIN, "w+done");

function open(server: SimServer, loop: SimLoop, name: string): { wave: OpenWave; failures: unknown[] } {
  const user = ParticipantId.of(`${name}@${DOMAIN}`);
  const connection = server.connect(user);
  const scheduler = new BackoffScheduler({ timers: loop, random: loop.random, maxMs: 2000 });
  const service = new RemoteWaveViewService(WAVE, connection, {
    onMarker: () => scheduler.reset(),
    clock: () => loop.now(),
  });
  const failures: unknown[] = [];
  const live = new LiveWaveView({
    waveId: WAVE,
    viewer: user,
    service,
    scheduler,
    clock: () => loop.now(),
    onListenerError: (error) => failures.push(error),
  });
  live.events.on("failed", ({ error }) => failures.push(error));
  const wave = new OpenWave(
    WAVE,
    live,
    new IdGeneratorImpl(DOMAIN, () => name),
    () => {
      scheduler.cancel();
      service.dispose();
    },
    (error) => failures.push(error),
    { timers: loop, clock: () => loop.now() },
  );
  connection.connect();
  live.open();
  return { wave, failures };
}

interface Setup {
  loop: SimLoop;
  alice: OpenWave;
  bob: OpenWave;
  store: ConversationStore;
  root: string;
  second: string;
  signals: () => number;
  version: () => number;
  failures: () => unknown[];
}

const opened: OpenWave[] = [];

afterEach(() => {
  for (const wave of opened.splice(0)) {
    wave.close();
  }
});

function setup(seed: number): Setup {
  const loop = new SimLoop(seed);
  const server = new SimServer(loop);
  const a = open(server, loop, "alice");
  loop.runFor(1000);
  const root = a.wave.createRootConversation();
  a.wave.addParticipant(`bob@${DOMAIN}`);
  a.wave.appendText(root, "Question");
  const second = a.wave.appendBlip({ text: "Another" });
  loop.runFor(1000);
  const b = open(server, loop, "bob");
  loop.runFor(1000);
  opened.push(a.wave, b.wave);
  const store = new ConversationStore(a.wave, a.wave.observe()!);
  const spy = vi.spyOn(a.wave, "signalDoneEditing");
  return {
    loop,
    alice: a.wave,
    bob: b.wave,
    store,
    root,
    second,
    signals: () => spy.mock.calls.length,
    version: () => a.wave.view.root()!.data().version,
    failures: () => [...a.failures, ...b.failures],
  };
}

describe("the finished-editing signal", () => {
  test("ending a session that changed the blip sends one no-op; an untouched one sends nothing", () => {
    const s = setup(3);
    const start = s.version();
    // Edit mode entered and left without typing: no delta at all.
    s.store.startEditing(s.root);
    s.store.stopEditing();
    s.loop.runFor(1000);
    expect(s.signals()).toBe(0);
    expect(s.version()).toBe(start);

    // Typing, then Done: exactly one more operation than the typing.
    s.store.startEditing(s.root);
    s.alice.appendText(s.root, " here");
    s.loop.runFor(1000);
    const typed = s.version();
    expect(typed).toBeGreaterThan(start);
    s.store.stopEditing();
    s.loop.runFor(1000);
    expect(s.signals()).toBe(1);
    expect(s.version()).toBe(typed + 1);
    expect(s.alice.unsavedState.unacknowledged).toBe(0);
    // Already ended: nothing more.
    s.store.stopEditing();
    expect(s.signals()).toBe(1);
    expect(s.failures()).toEqual([]);
  });

  test("focus moving to another blip, or editing another blip, ends the session with the signal", () => {
    const s = setup(5);
    s.store.startEditing(s.root);
    s.alice.appendText(s.root, "!");
    s.store.focus(s.second);
    expect(s.store.editingId.peek()).toBeNull();
    expect(s.signals()).toBe(1);

    s.store.startEditing(s.second);
    s.alice.appendText(s.second, "!");
    s.store.startEditing(s.root);
    expect(s.signals()).toBe(2);
    // The new session has changed nothing yet.
    s.store.stopEditing();
    expect(s.signals()).toBe(2);
    // Someone else's change to the blip being edited is not this user's.
    s.store.startEditing(s.second);
    s.bob.appendText(s.second, " (bob)");
    s.loop.runFor(1000);
    expect(s.alice.blipText(s.second)).toContain("(bob)");
    s.store.stopEditing();
    expect(s.signals()).toBe(2);
  });

  test("a reply left empty is discarded without a signal; one with text sends it", () => {
    const s = setup(7);
    const blips = () => s.alice.conversation()!.blipsBreadthFirst().length;
    const before = blips();
    s.store.reply(s.root);
    expect(blips()).toBe(before + 1);
    s.store.stopEditing();
    expect(blips()).toBe(before);
    expect(s.signals()).toBe(0);

    s.store.reply(s.root);
    const draft = s.store.editingId.peek()!;
    s.alice.appendText(draft, "An answer");
    s.store.stopEditing();
    expect(blips()).toBe(before + 1);
    expect(s.signals()).toBe(1);
  });

  test("deleting the blip being edited sends no signal", () => {
    const s = setup(9);
    s.store.startEditing(s.second);
    s.alice.appendText(s.second, "!");
    s.store.deleteBlip(s.second);
    expect(s.store.editingId.peek()).toBeNull();
    expect(s.signals()).toBe(0);
  });

  test("the no-op makes nothing unread for others and leaves their digest as it was", () => {
    const s = setup(11);
    for (const blip of [s.root, s.second]) {
      s.bob.startReading(blip);
      s.bob.stopReading(blip);
    }
    s.loop.runFor(1000);
    expect(s.bob.unreadBlips()).toEqual([]);
    const bobsVersion = () => s.bob.view.root()!.data().version;
    const versionBefore = bobsVersion();
    const blipTimes = () =>
      [s.root, s.second].map((id) => {
        const data = s.bob.conversation()!.blipData(id)!;
        return [data.lastModifiedTime, data.lastModifiedVersion];
      });
    const timesBefore = blipTimes();
    const digestBefore = digestOfWave(s.bob);
    const waveletTimeBefore = s.bob.view.root()!.data().lastModifiedTime;
    const changed: string[] = [];
    s.bob.supplement.events.on("maybeBlipReadChanged", ({ blipId }) => changed.push(blipId));

    s.loop.runFor(1000);
    s.alice.signalDoneEditing();
    s.loop.runFor(1000);

    expect(bobsVersion()).toBe(versionBefore + 1);
    expect(s.bob.unreadBlips()).toEqual([]);
    expect(blipTimes()).toEqual(timesBefore);
    expect(changed).toEqual([]);
    expect(digestOfWave(s.bob)).toEqual(digestBefore);
    // The wavelet's own last-modified time moves, as for any delta (the
    // server's inbox sorts by it; the digest's time comes from the blips).
    expect(s.bob.view.root()!.data().lastModifiedTime).toBeGreaterThan(waveletTimeBefore);
  });
});
