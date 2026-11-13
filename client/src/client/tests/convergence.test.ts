// Randomized convergence tests of the whole client stack below the API:
// live wave views (model + channel stack + wire translation) of several
// simulated users against the simulated server, with random latency,
// random edits, offline edits and random disconnections. At the end every
// replica must equal the server's state, version and history hash, nothing
// may be unsaved, and no operation may have been applied twice.

import { afterEach, beforeEach, describe, expect, test } from "vitest";

import { ViewChannelImpl } from "../../concurrencycontrol/channel";
import { DocError, XmlFragment } from "../../model/doc";
import { OperationError } from "../../model/docop";
import { ParticipantId, WaveId, WaveletId, WaveletName } from "../../model/id";
import { BackoffScheduler } from "../backoffScheduler";
import { LiveWaveView } from "../liveWaveView";
import { RemoteWaveViewService } from "../remoteWaveViewService";
import { type SimConnection, SimLoop, SimServer } from "../testing/simulation";

const DOMAIN = "example.com";
const WAVE = WaveId.of(DOMAIN, "w+conv");
const ROOT = WaveletId.of(DOMAIN, "conv+root");
const ROOT_NAME = WaveletName.of(WAVE, ROOT);
const DOCS = ["b+1", "b+2"];
const EXTRAS = ["x1", "x2", "x3"].map((name) => ParticipantId.of(`${name}@${DOMAIN}`));

interface Client {
  readonly user: ParticipantId;
  readonly connection: SimConnection;
  readonly service: RemoteWaveViewService;
  readonly scheduler: BackoffScheduler;
  readonly live: LiveWaveView;
  readonly failures: Error[];
  resets: number;
  edits: number;
}

interface Options {
  clients: number;
  steps: number;
  disconnectProbability: number;
  separateSubmitResponses?: boolean;
  historyLimit?: number;
}

function newClient(server: SimServer, loop: SimLoop, index: number, options: Options): Client {
  const user = ParticipantId.of(`user${index}@${DOMAIN}`);
  const connectionOptions =
    options.separateSubmitResponses === true ? { separateSubmitResponses: true } : {};
  const connection = server.connect(user, connectionOptions);
  const scheduler = new BackoffScheduler({ timers: loop, random: loop.random, maxMs: 2000 });
  const service = new RemoteWaveViewService(WAVE, connection, {
    onMarker: () => scheduler.reset(),
    clock: () => loop.now(),
  });
  const failures: Error[] = [];
  const live = new LiveWaveView({
    waveId: WAVE,
    viewer: user,
    service,
    scheduler,
    clock: () => loop.now(),
    onListenerError: (error) => failures.push(error as Error),
  });
  const client: Client = { user, connection, service, scheduler, live, failures, resets: 0, edits: 0 };
  live.events.on("failed", ({ error }) => failures.push(error));
  live.events.on("waveletReset", () => {
    client.resets++;
  });
  return client;
}

const ALPHABET = ["a", "b", "c", " ", "é", "\u{1F600}", "z", "-"];

/** One random edit of the root wavelet by a client. Returns whether something was done. */
function randomEdit(client: Client, loop: SimLoop, counter: { next: number }): boolean {
  const wavelet = client.live.view.wavelet(ROOT);
  if (wavelet === null || !wavelet.data().hasParticipant(client.user)) {
    return false;
  }
  const docId = DOCS[loop.int(0, DOCS.length - 1)]!;
  const kind = loop.int(0, 9);
  try {
    if (kind === 9) {
      const extra = EXTRAS[loop.int(0, EXTRAS.length - 1)]!;
      if (wavelet.data().hasParticipant(extra)) {
        wavelet.removeParticipant(extra);
      } else {
        wavelet.addParticipant(extra);
      }
      return true;
    }
    return wavelet.editDocument(docId, (doc) => {
      const view = doc.view();
      const body = view.firstChildElement(view.root());
      if (body === null) {
        return false;
      }
      const start = view.contentStart(body);
      const end = view.contentEnd(body);
      const at = loop.int(start, end);
      if (kind <= 4) {
        // A unique token, so that a duplicated operation shows.
        // (A private-use character: deletions cannot fabricate one.)
        let text = String.fromCharCode(0xe000 + counter.next++);
        for (let i = loop.int(0, 3); i > 0; i--) {
          text += ALPHABET[loop.int(0, ALPHABET.length - 1)];
        }
        doc.insertText(at, text);
      } else if (kind <= 6) {
        const to = Math.min(end, at + loop.int(1, 6));
        if (to <= at) {
          return false;
        }
        doc.deleteRange(at, to);
      } else if (kind === 7) {
        const to = Math.min(end, at + loop.int(1, 8));
        if (to <= at) {
          return false;
        }
        doc.setAnnotation(at, to, loop.chance(0.5) ? "style/a" : "style/b", loop.chance(0.3) ? null : `v${loop.int(0, 2)}`);
      } else {
        doc.insertXml(at, XmlFragment.parse("<line/>"));
      }
      return true;
    });
  } catch (error) {
    // An edit that is not valid where it was aimed (inside a surrogate
    // pair, across an element boundary): the model refuses it and nothing
    // was sent.
    if (error instanceof DocError || error instanceof OperationError) {
      return false;
    }
    throw error;
  }
}

function runScenario(seed: number, options: Options): { edits: number; drops: number; resets: number; submits: number } {
  const loop = new SimLoop(seed);
  const serverOptions = options.historyLimit !== undefined ? { historyLimit: options.historyLimit } : {};
  const server = new SimServer(loop, serverOptions);
  const clients: Client[] = [];
  for (let i = 0; i < options.clients; i++) {
    clients.push(newClient(server, loop, i, options));
  }
  const context = (): string => `seed ${seed}`;

  // The first client creates the wave.
  const creator = clients[0]!;
  creator.connection.connect();
  creator.live.open();
  loop.runUntilIdle();
  expect(creator.live.state, context()).toBe("open");
  const root = creator.live.createWavelet(ROOT);
  for (const docId of DOCS) {
    root.editDocument(docId, (doc) => {
      doc.appendXml(XmlFragment.parse("<body><line/>seed</body>"));
    });
  }
  for (const client of clients.slice(1)) {
    root.addParticipant(client.user);
  }
  loop.runUntilIdle();
  for (const client of clients.slice(1)) {
    client.connection.connect();
    client.live.open();
  }
  loop.runUntilIdle();
  for (const client of clients) {
    expect(client.live.state, context()).toBe("open");
    expect(client.live.view.wavelet(ROOT), context()).not.toBeNull();
  }

  const counter = { next: 0 };
  for (let step = 0; step < options.steps; step++) {
    const client = clients[loop.int(0, clients.length - 1)]!;
    const action = loop.random();
    if (action < options.disconnectProbability) {
      if (client.connection.isConnected()) {
        client.connection.disconnect();
      } else {
        client.connection.connect();
      }
    } else if (action < options.disconnectProbability + 0.1 && !client.connection.isConnected()) {
      client.connection.connect();
    } else if (randomEdit(client, loop, counter)) {
      client.edits++;
    }
    // Let some time pass: sometimes none, so that edits pile up.
    if (loop.chance(0.6)) {
      loop.runFor(loop.int(0, 60));
    }
    for (const c of clients) {
      if (c.failures.length > 0) {
        throw new Error(`${context()}, step ${step}: ${c.user} failed: ${describe_(c.failures[0])}`);
      }
    }
  }

  // Everybody back online; let everything settle and commit.
  for (const client of clients) {
    client.connection.connect();
  }
  loop.runUntilIdle();
  server.commitAll();
  loop.runUntilIdle();

  expect(server.submitErrors, context()).toEqual([]);
  const serverWavelet = server.wavelet(ROOT_NAME);
  expect(serverWavelet, context()).toBeDefined();
  const expected = serverWavelet!.data;
  for (const client of clients) {
    expect(client.failures, `${context()} ${client.user}`).toEqual([]);
    expect(client.live.state, `${context()} ${client.user}`).toBe("open");
    const wavelet = client.live.view.wavelet(ROOT);
    expect(wavelet, `${context()} ${client.user}`).not.toBeNull();
    const data = wavelet!.data();
    for (const docId of DOCS) {
      expect(data.document(docId)?.content.toXmlString(), `${context()} ${client.user} ${docId}`).toBe(
        expected.document(docId)?.content.toXmlString(),
      );
    }
    // As a set: concurrent additions land in arrival order on the server
    // and in local-first order on a client; the protocol does not make the
    // order of the participant list converge (nor did the original).
    expect(
      data.participants.map(String).sort(),
      `${context()} ${client.user} participants`,
    ).toEqual(expected.participants.map(String).sort());
    expect(data.version, `${context()} ${client.user} version`).toBe(expected.version);
    expect(data.hashedVersion.toString(), `${context()} ${client.user} hash`).toBe(
      expected.hashedVersion.toString(),
    );
    expect(client.live.unsavedState, `${context()} ${client.user} unsaved`).toEqual({
      inFlight: 0,
      unacknowledged: 0,
      uncommitted: 0,
      saved: true,
    });
  }
  // No inserted token appears twice: no operation was applied twice.
  for (const docId of DOCS) {
    const xml = expected.document(docId)!.content.toXmlString();
    const tokens = xml.match(/[\ue000-\uf8ff]/g) ?? [];
    expect(new Set(tokens).size, `${context()} duplicate tokens in ${docId}`).toBe(tokens.length);
  }

  for (const client of clients) {
    client.live.close();
    client.service.dispose();
    client.scheduler.cancel();
  }
  loop.runUntilIdle();
  return {
    edits: clients.reduce((sum, c) => sum + c.edits, 0),
    drops: clients.reduce((sum, c) => sum + c.connection.drops, 0),
    resets: clients.reduce((sum, c) => sum + c.resets, 0),
    submits: server.submitsApplied,
  };
}

function describe_(error: unknown): string {
  let text = "";
  for (let e: unknown = error, depth = 0; e instanceof Error && depth < 8; e = e.cause, depth++) {
    text += `\n  <- ${e.name}: ${e.message}`;
  }
  return text;
}

const SEEDS = Number(process.env.WAVE_CONVERGENCE_SEEDS ?? 150);
const FIRST_SEED = Number(process.env.WAVE_CONVERGENCE_FIRST_SEED ?? 1);

describe("convergence", () => {
  beforeEach(() => {
    ViewChannelImpl.setMaxViewChannelsPerWave(1000);
  });
  afterEach(() => {
    ViewChannelImpl.resetViewChannelsPerWave();
  });

  test("clients that stay connected converge", () => {
    let edits = 0;
    for (let seed = FIRST_SEED; seed < FIRST_SEED + SEEDS; seed++) {
      edits += runScenario(seed, { clients: 3, steps: 120, disconnectProbability: 0 }).edits;
    }
    expect(edits).toBeGreaterThan(SEEDS * 50);
  });

  test("clients converge across random disconnections and offline edits", () => {
    let drops = 0;
    let resets = 0;
    for (let seed = FIRST_SEED; seed < FIRST_SEED + SEEDS; seed++) {
      const result = runScenario(1000 + seed, { clients: 4, steps: 200, disconnectProbability: 0.08 });
      drops += result.drops;
      resets += result.resets;
    }
    expect(drops).toBeGreaterThan(SEEDS);
    // With full history on the server every reconnection resumes.
    expect(resets).toBe(0);
  });

  test("clients whose known version the server has forgotten are reset and converge", () => {
    let resets = 0;
    for (let seed = FIRST_SEED; seed < FIRST_SEED + SEEDS; seed++) {
      resets += runScenario(3000 + seed, {
        clients: 3,
        steps: 150,
        disconnectProbability: 0.06,
        historyLimit: 3,
      }).resets;
    }
    expect(resets).toBeGreaterThan(0);
  });

  test("clients converge when submit responses and updates are reordered", () => {
    for (let seed = FIRST_SEED; seed < FIRST_SEED + SEEDS; seed++) {
      runScenario(2000 + seed, {
        clients: 3,
        steps: 150,
        disconnectProbability: 0.04,
        separateSubmitResponses: true,
      });
    }
  });
});
