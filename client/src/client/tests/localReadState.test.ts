// The viewer's own edits do not make a blip unread for them: `OpenWave`'s
// supplement is the optimistic one of the original client
// (`LocalSupplementedWaveImpl`). Two users on the simulated server.

import { describe, expect, test } from "vitest";

import { IdGeneratorImpl, ParticipantId, WaveId } from "../../model/id";
import { EVICT_TIME_MS, REPEAT_MS } from "../../model/supplement";
import { BackoffScheduler } from "../backoffScheduler";
import { LiveWaveView } from "../liveWaveView";
import { OpenWave } from "../openWave";
import { RemoteWaveViewService } from "../remoteWaveViewService";
import { SimLoop, SimServer } from "../testing/simulation";

const DOMAIN = "example.com";
const WAVE = WaveId.of(DOMAIN, "w+read");

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

describe("optimistic read state of an open wave", () => {
  test("a blip its reader edits stays read for them and becomes unread for the other", () => {
    const loop = new SimLoop(7);
    const server = new SimServer(loop);
    const a = open(server, loop, "alice");
    loop.runFor(1000);
    expect(a.wave.state).toBe("open");
    const blip = a.wave.createRootConversation();
    a.wave.addParticipant(`bob@${DOMAIN}`);
    a.wave.appendText(blip, "hello");
    loop.runFor(1000);

    const b = open(server, loop, "bob");
    loop.runFor(1000);
    expect(b.wave.state).toBe("open");
    expect(b.wave.blipText(blip)).toBe("hello");

    // Both read the blip; bob moves on.
    a.wave.startReading(blip);
    b.wave.startReading(blip);
    b.wave.stopReading(blip);
    loop.runFor(1000);
    expect(a.wave.isUnread(blip)).toBe(false);
    expect(b.wave.isUnread(blip)).toBe(false);

    // Alice edits; the server acknowledges; bob receives the edit.
    const changedForBob: string[] = [];
    b.wave.supplement.events.on("maybeBlipReadChanged", ({ blipId }) => changedForBob.push(blipId));
    const version = a.wave.view.root()!.data().version;
    a.wave.appendText(blip, " world");
    loop.runFor(1000);
    expect(a.wave.unsavedState.unacknowledged).toBe(0);
    expect(a.wave.view.root()!.data().version).toBeGreaterThan(version);
    expect(b.wave.blipText(blip)).toBe("hello world");

    expect(a.wave.isUnread(blip)).toBe(false);
    expect(a.wave.unreadBlips()).toEqual([]);
    expect(b.wave.isUnread(blip)).toBe(true);
    expect(b.wave.unreadBlips()).toEqual([blip]);
    expect(changedForBob).toContain(blip);

    // By the versions alone the blip is unread for alice (the gap): the
    // repetition then records the acknowledged version, for later sessions.
    expect(a.wave.supplement.state().isUnread(a.wave.view.rootId(), blip)).toBe(true);
    loop.runFor(REPEAT_MS);
    expect(a.wave.supplement.state().isUnread(a.wave.view.rootId(), blip)).toBe(false);

    // Alice leaves the blip and keeps it read; a second session of hers,
    // opened later, agrees.
    a.wave.stopReading(blip);
    loop.runFor(EVICT_TIME_MS + REPEAT_MS);
    expect(a.wave.isUnread(blip)).toBe(false);
    const a2 = open(server, loop, "alice");
    loop.runFor(1000);
    expect(a2.wave.isUnread(blip)).toBe(false);

    // Bob's change makes it unread for alice once she is not reading it.
    b.wave.appendText(blip, "!");
    loop.runFor(1000);
    expect(a.wave.isUnread(blip)).toBe(true);
    expect(a2.wave.isUnread(blip)).toBe(true);

    expect([...a.failures, ...a2.failures, ...b.failures]).toEqual([]);
    for (const { wave } of [a, a2, b]) {
      wave.close();
    }
    loop.runFor(1000);
  });

  test("a blip marked as read and then edited without being read stays read", () => {
    const loop = new SimLoop(11);
    const server = new SimServer(loop);
    const a = open(server, loop, "alice");
    loop.runFor(1000);
    const blip = a.wave.createRootConversation();
    loop.runFor(1000);
    // Created by alice, never read: unread, as in the original.
    expect(a.wave.isUnread(blip)).toBe(true);
    a.wave.markBlipRead(blip);
    a.wave.appendText(blip, "typing");
    loop.runFor(1000);
    expect(a.wave.isUnread(blip)).toBe(false);
    loop.runFor(EVICT_TIME_MS + REPEAT_MS);
    expect(a.wave.isUnread(blip)).toBe(false);

    // startReading replaces the blip being read; stopReading of another is ignored.
    const second = a.wave.appendBlip({ text: "two" });
    a.wave.startReading(blip);
    a.wave.startReading(second);
    expect(a.wave.readingBlip()?.blipId).toBe(second);
    a.wave.stopReading(blip);
    expect(a.wave.readingBlip()?.blipId).toBe(second);
    a.wave.stopReading(second);
    expect(a.wave.readingBlip()).toBeNull();
    loop.runFor(1000);
    expect(a.wave.unreadBlips()).toEqual([]);
    expect(a.failures).toEqual([]);
    a.wave.close();
    loop.runFor(1000);
  });
});
