// Integration tests of the client against the real resonator server.
// Skipped when the server binary has not been built; see `harness.ts`.

import { afterAll, beforeAll, describe, expect, test } from "vitest";

import { type OpenWave, type WaveClient } from "../../client";
import { WaveId } from "../../model/id";
import { AuthError } from "../../net";
import { deserializeWavelet } from "../../wire";
import { BINARY_PRESENT, DOMAIN, TestServer, sleep, waitFor } from "./harness";

let server: TestServer;

beforeAll(async () => {
  if (BINARY_PRESENT) {
    server = await TestServer.start();
  }
}, 30_000);

afterAll(async () => {
  await server?.stop();
});

/** The XML of a document of the root wavelet as the server's `/fetch` has it. */
async function fetched(client: WaveClient, waveId: WaveId, docId: string): Promise<string | null> {
  const snapshot = await client.api.fetchWaveView(waveId);
  for (const wavelet of snapshot.wavelet) {
    const data = deserializeWavelet(wavelet, waveId);
    if (data.waveletId.id === "conv+root") {
      return data.document(docId)?.content.toXmlString() ?? null;
    }
  }
  return null;
}

function rootXml(wave: OpenWave, docId: string): string | null {
  return wave.view.root()?.document(docId).toXmlString() ?? null;
}

/** Waits until both views and the server agree on a document; returns it. */
async function converged(
  client: WaveClient,
  waves: OpenWave[],
  docId: string,
): Promise<string> {
  await Promise.all(waves.map((wave) => wave.whenSaved()));
  let last: (string | null)[] = [];
  await waitFor(`convergence of ${docId}`, async () => {
    const server = await fetched(client, waves[0]!.waveId, docId);
    last = [server, ...waves.map((wave) => rootXml(wave, docId))];
    return last.every((xml) => xml !== null && xml === server) && waves.every((w) => w.unsavedState.saved);
  }).catch((error: unknown) => {
    throw new Error(`${String(error)}:\n${last.join("\n")}`);
  });
  return last[0]!;
}

/** Characters of the private use area mark insertions; none may appear twice. */
function expectNoDuplicates(xml: string): void {
  const marks = xml.match(/[-]/g) ?? [];
  expect(new Set(marks).size).toBe(marks.length);
}

let mark = 0xe000;
function nextMark(): string {
  return String.fromCharCode(mark++);
}

describe.skipIf(!BINARY_PRESENT)("against the resonator server", () => {
  test("register, sign in with a wrong and the right password, session", async () => {
    const client = server.client();
    const before = await client.start();
    expect(before.address).toBeNull();
    expect(before.domain).toBe(DOMAIN);
    expect(client.signedIn).toBe(false);

    await client.register(`carol@${DOMAIN}`, "password-carol");
    await expect(client.signIn(`carol@${DOMAIN}`, "wrong")).rejects.toSatisfy(
      (error) => error instanceof AuthError && error.kind === "invalidCredentials",
    );
    await expect(client.register(`carol@${DOMAIN}`, "password-carol")).rejects.toBeInstanceOf(
      AuthError,
    );
    const session = await client.signIn(`carol@${DOMAIN}`, "password-carol");
    expect(session.address).toBe(`carol@${DOMAIN}`);
    expect(session.idSeed.length).toBeGreaterThan(0);
    expect(client.address?.address).toBe(`carol@${DOMAIN}`);
    await client.whenConnected();
    expect(client.connectionState).toBe("connected");
  });

  test("create a wave; the server has what the client has", async () => {
    const alice = await server.user("alice1");
    const { wave, rootBlipId } = await alice.createWave({ text: "Hello wave. More text." });
    expect(wave.unsavedState.saved).toBe(false);
    await wave.whenSaved();
    expect(wave.unsavedState.saved).toBe(true);

    const conversation = wave.conversation()!;
    expect(conversation.rootThread.blips.map((blip) => blip.id)).toEqual([rootBlipId]);
    expect(wave.blipText(rootBlipId)).toBe("Hello wave. More text.");
    expect(wave.participants().map(String)).toEqual([`alice1@${DOMAIN}`]);

    // A reply and an inline reply.
    const reply = wave.reply(rootBlipId, { text: "A reply" });
    const range = wave.blipTextRange(rootBlipId)!;
    const inline = wave.inlineReply(rootBlipId, range.start + 5, { text: "inline" });
    wave.updateImplicitTitle();
    expect(wave.title()).toBe("Hello wave.");
    await wave.whenSaved();

    for (const docId of ["conversation", rootBlipId, reply.blipId, inline.blipId]) {
      expect(await fetched(alice, wave.waveId, docId), docId).toBe(rootXml(wave, docId));
    }
    expect(rootXml(wave, "conversation")).toBe(
      `<conversation><blip id="${rootBlipId}">` +
        `<thread id="${reply.threadId}"><blip id="${reply.blipId}"/></thread>` +
        `<thread id="${inline.threadId}" inline="true"><blip id="${inline.blipId}"/></thread>` +
        `</blip></conversation>`,
    );
    expect(rootXml(wave, reply.blipId)).toBe("<body><line/>A reply</body>");
    expect(rootXml(wave, rootBlipId)).toContain(`<reply id="${inline.threadId}"/>`);

    // A fresh view of the same wave gets the same state.
    wave.close();
    const again = await alice.openWave(wave.waveId);
    expect(rootXml(again, rootBlipId)).toBe(await fetched(alice, wave.waveId, rootBlipId));
    expect(again.title()).toBe("Hello wave.");
  });

  test("two clients editing the same blip concurrently converge", async () => {
    const alice = await server.user("alice2");
    const bob = await server.user("bob2");
    const { wave: a, rootBlipId } = await alice.createWave({
      participants: [bob.address!],
      text: "start",
    });
    await a.whenSaved();
    const b = await bob.openWave(a.waveId);
    expect(b.blipText(rootBlipId)).toBe("start");

    const remoteOps: string[] = [];
    b.observe()!.events.on("blipContentChanged", ({ local }) => remoteOps.push(local ? "L" : "R"));

    for (let round = 0; round < 30; round++) {
      // Both type at once, at the start and at the end, without waiting.
      const ra = a.blipTextRange(rootBlipId)!;
      a.insertText(rootBlipId, ra.start, `${nextMark()}a${round} `);
      const rb = b.blipTextRange(rootBlipId)!;
      b.insertText(rootBlipId, rb.end, ` b${round}${nextMark()}`);
      if (round % 5 === 4) {
        const range = b.blipTextRange(rootBlipId)!;
        if (range.end - range.start > 12) {
          b.deleteRange(rootBlipId, range.start + 6, range.start + 9);
        }
      }
      if (round % 3 === 0) {
        await sleep(5);
      }
    }
    const xml = await converged(alice, [a, b], rootBlipId);
    expectNoDuplicates(xml);
    expect(xml).toContain("a29 ");
    expect(xml).toContain(" b29");
    expect(remoteOps).toContain("R");
    expect(remoteOps).toContain("L");
    expect(a.view.root()!.data().hashedVersion.toString()).toBe(
      b.view.root()!.data().hashedVersion.toString(),
    );
  });

  test("adding a participant delivers the wave to a user who has it open", async () => {
    const alice = await server.user("alice3");
    const bob = await server.user("bob3");
    const { wave: a, rootBlipId } = await alice.createWave({ text: "for bob" });
    await a.whenSaved();

    // Bob cannot see the wave yet: it opens empty.
    const b = await bob.openWave(a.waveId);
    expect(b.view.wavelets()).toEqual([]);
    expect(b.hasConversation()).toBe(false);
    const added: string[] = [];
    b.conversations.events.on("conversationAdded", ({ conversationId }) => added.push(conversationId));

    a.addParticipant(bob.address!);
    await waitFor("the wave to reach bob", () => b.hasConversation());
    expect(added.length).toBe(1);
    expect(b.blipText(rootBlipId)).toBe("for bob");

    // And it is live in both directions.
    b.appendText(rootBlipId, " - thanks");
    a.appendBlip({ text: "second blip" });
    await converged(alice, [a, b], rootBlipId);
    await converged(alice, [a, b], "conversation");
    expect(a.blipText(rootBlipId)).toBe("for bob - thanks");
    expect(b.conversation()!.rootThread.blips.length).toBe(2);

    // A user who opens the wave afterwards gets it as a snapshot.
    const carol = await server.user("carol3");
    a.addParticipant(carol.address!);
    await a.whenSaved();
    const c = await carol.openWave(a.waveId);
    expect(c.blipText(rootBlipId)).toBe("for bob - thanks");
  });

  test("a dropped socket resumes without loss and without duplicates", async () => {
    const alice = await server.user("alice4");
    const bob = await server.user("bob4");
    const { wave: a, rootBlipId } = await alice.createWave({
      participants: [bob.address!],
      text: "x",
    });
    await a.whenSaved();
    const b = await bob.openWave(a.waveId);
    let resets = 0;
    a.events.on("waveletReset", () => resets++);
    b.events.on("waveletReset", () => resets++);
    const states: string[] = [];
    alice.events.on("connection", ({ state }) => states.push(state));

    for (let round = 0; round < 12; round++) {
      a.appendText(rootBlipId, `${nextMark()}A${round}`);
      b.insertText(rootBlipId, b.blipTextRange(rootBlipId)!.start, `${nextMark()}B${round}`);
      if (round % 3 === 1) {
        // With a delta of alice's possibly in flight: the server may or
        // may not have it.
        alice.dropConnection();
        a.appendText(rootBlipId, `${nextMark()}off${round}`);
      }
      if (round % 4 === 2) {
        bob.dropConnection();
      }
      await sleep(round % 2 === 0 ? 0 : 15);
    }
    const xml = await converged(alice, [a, b], rootBlipId);
    expectNoDuplicates(xml);
    for (let round = 0; round < 12; round++) {
      expect(xml).toContain(`A${round}`);
      expect(xml).toContain(`B${round}`);
    }
    for (const round of [1, 4, 7, 10]) {
      expect(xml).toContain(`off${round}`);
    }
    expect(resets).toBe(0);
    expect(states).toContain("waiting");
    expect(alice.connectionState).toBe("connected");
    expect(a.state).toBe("open");
    expect(b.state).toBe("open");
  });

  test("edits made offline are sent after reconnecting", async () => {
    const alice = await server.user("alice5");
    const bob = await server.user("bob5");
    const { wave: a, rootBlipId } = await alice.createWave({
      participants: [bob.address!],
      text: "online",
    });
    await a.whenSaved();
    const b = await bob.openWave(a.waveId);

    // Take alice offline for a while: stop the server answering her by
    // dropping the socket whenever it comes back.
    const off = alice.events.on("connection", ({ state }) => {
      if (state === "connected") {
        alice.dropConnection();
      }
    });
    alice.dropConnection();
    await waitFor("alice to be offline", () => alice.connectionState !== "connected");

    a.appendText(rootBlipId, " offline-1");
    const reply = a.reply(rootBlipId, { text: "written offline" });
    a.appendText(rootBlipId, " offline-2");
    expect(a.unsavedState.saved).toBe(false);
    expect(a.unsavedState.unacknowledged).toBeGreaterThan(0);
    // Meanwhile bob edits online.
    b.insertText(rootBlipId, b.blipTextRange(rootBlipId)!.start, "bob ");
    await b.whenSaved();
    await sleep(150);
    expect(b.blipText(rootBlipId)).toBe("bob online");
    expect(a.blipText(rootBlipId)).toBe("online offline-1 offline-2");

    off();
    alice.reconnectNow();
    await converged(alice, [a, b], rootBlipId);
    await converged(alice, [a, b], "conversation");
    expect(b.blipText(rootBlipId)).toBe("bob online offline-1 offline-2");
    expect(b.blipText(reply.blipId)).toBe("written offline");
    expect(a.unsavedState).toEqual({ inFlight: 0, unacknowledged: 0, uncommitted: 0, saved: true });
  });

  test("signing out elsewhere ends the session and closes the waves", async () => {
    const alice = await server.user("alice6");
    const { wave } = await alice.createWave({ text: "bye" });
    await wave.whenSaved();
    const signedOut = new Promise<{ error: unknown }>((resolve) =>
      alice.events.once("signedOut", resolve),
    );
    // The same session signs out behind the client's back (another tab).
    await alice.api.signOut();
    const { error } = await signedOut;
    expect(error).not.toBeNull();
    expect((error as { detail: { kind: string } }).detail.kind).toBe("signedOut");
    expect(alice.signedIn).toBe(false);
    expect(alice.connectionState).toBe("idle");
    expect(wave.state).toBe("closed");
    expect(() => alice.attachWave(wave.waveId)).toThrow("not signed in");

    // Signing in again works, and the wave is still there.
    await alice.signIn(`alice6@${DOMAIN}`, "password-alice6");
    const again = await alice.openWave(wave.waveId);
    expect(again.hasConversation()).toBe(true);

    // An orderly sign-out.
    const events: unknown[] = [];
    alice.events.on("signedOut", (event) => events.push(event.error));
    await alice.signOut();
    expect(events).toEqual([null]);
    expect(again.state).toBe("closed");
    expect((await alice.start()).address).toBeNull();
  });

  test("marking blips read updates the supplement, here and in a later session", async () => {
    const alice = await server.user("alice7");
    const bob = await server.user("bob7");
    const { wave: a, rootBlipId } = await alice.createWave({
      participants: [bob.address!],
      text: "read me",
    });
    const second = a.appendBlip({ text: "and me" });
    await a.whenSaved();

    const b = await bob.openWave(a.waveId);
    expect(b.view.userData()).toBeNull();
    expect(b.isUnread(rootBlipId)).toBe(true);
    expect(b.unreadBlips().sort()).toEqual([rootBlipId, second].sort());
    const changed: string[] = [];
    b.supplement.events.on("maybeBlipReadChanged", ({ blipId }) => changed.push(blipId));

    b.markBlipRead(rootBlipId);
    expect(b.isUnread(rootBlipId)).toBe(false);
    expect(b.isUnread(second)).toBe(true);
    expect(changed).toContain(rootBlipId);
    expect(b.view.userData()).not.toBeNull();
    await b.whenSaved();

    // The user-data wavelet is bob's alone.
    expect(a.view.wavelets().map((w) => w.waveletId().id)).toEqual(["conv+root"]);

    // A later session of bob's sees the read state.
    const bobAgain = await server.user("bob7", false);
    const b2 = await bobAgain.openWave(a.waveId);
    expect(b2.isUnread(rootBlipId)).toBe(false);
    expect(b2.unreadBlips()).toEqual([second]);

    // A change by alice makes the blip unread again, in both sessions.
    changed.length = 0;
    a.appendText(rootBlipId, " again");
    await waitFor("the blip to become unread", () => b.isUnread(rootBlipId) && b2.isUnread(rootBlipId));
    expect(changed).toContain(rootBlipId);

    // Marking everything read in one session reaches the other.
    b2.markRead();
    await waitFor("the read state to reach the first session", () => b.unreadBlips().length === 0);
    expect(b2.unreadBlips()).toEqual([]);
    await b2.whenSaved();
  });
});
