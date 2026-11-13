// Tests of the wire translation of the view service, and of the two
// places where the channel stack deliberately departs from the original
// for this server (see `operationChannelImpl.ts` and the multiplexer's
// `dropSnapshotOfCreatedWavelets`).

import { afterEach, describe, expect, test } from "vitest";

import {
  ImmediateExecutionScheduler,
  ViewChannelImpl,
  WaveletIdMap,
  type OpenCallback,
  type WaveViewServiceUpdate,
} from "../../concurrencycontrol/channel";
import { WaveletData } from "../../model/data";
import { XmlFragment } from "../../model/doc";
import { Attributes, DocOpBuilder } from "../../model/docop";
import {
  IdFilter,
  ParticipantId,
  WaveId,
  WaveletId,
  WaveletName,
} from "../../model/id";
import {
  TransformedWaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
} from "../../model/operation";
import { HashedVersion, HashedVersionFactory } from "../../model/version";
import type { Unsubscribe } from "../../util/events";
import {
  type ProtocolOpenRequest,
  type ProtocolSubmitRequest,
  type ProtocolSubmitResponse,
  type ProtocolWaveletUpdate,
  deserializeDelta,
  serializeHashedVersion,
  serializeTransformedDelta,
  serializeWavelet,
} from "../../wire";
import { LiveWaveView } from "../liveWaveView";
import { RemoteWaveViewService } from "../remoteWaveViewService";
import type {
  ViewStream,
  ViewStreamHandler,
  WaveRpcTransport,
} from "../waveRpc";

const WAVE = WaveId.of("example.com", "w+1");
const ROOT = WaveletId.of("example.com", "conv+root");
const NAME = WaveletName.of(WAVE, ROOT);
const WIRE_NAME = "example.com/w+1/~/conv+root";
const DUMMY = "example.com/w+1/~/dummy+root";
const ALICE = ParticipantId.of("alice@example.com");
const BOB = ParticipantId.of("bob@example.com");

function version(v: number): HashedVersion {
  return v === 0
    ? HashedVersionFactory.createVersionZero(NAME)
    : HashedVersion.of(v, Uint8Array.of(v, 7));
}

class FakeTransport implements WaveRpcTransport {
  connected = true;
  listeners = new Set<(connected: boolean) => void>();
  opens: {
    request: ProtocolOpenRequest;
    handler: ViewStreamHandler;
    cancelled: boolean;
  }[] = [];
  submits: {
    request: ProtocolSubmitRequest;
    respond: (response: ProtocolSubmitResponse) => void;
    fail: (reason: string) => void;
    pending: () => void;
  }[] = [];

  isConnected(): boolean {
    return this.connected;
  }
  onConnectionChange(listener: (connected: boolean) => void): Unsubscribe {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  setConnected(connected: boolean): void {
    this.connected = connected;
    for (const listener of [...this.listeners]) {
      listener(connected);
    }
  }
  openView(
    request: ProtocolOpenRequest,
    handler: ViewStreamHandler,
  ): ViewStream {
    const open = { request, handler, cancelled: false };
    this.opens.push(open);
    return {
      cancel: () => {
        open.cancelled = true;
      },
    };
  }
  submit(
    request: ProtocolSubmitRequest,
    respond: (response: ProtocolSubmitResponse) => void,
    fail: (reason: string) => void,
    pending?: () => void,
  ): void {
    this.submits.push({ request, respond, fail, pending: pending ?? (() => {}) });
  }
  get stream(): ViewStreamHandler {
    return this.opens[this.opens.length - 1]!.handler;
  }
}

class Recorder implements OpenCallback {
  log: string[] = [];
  updates: WaveViewServiceUpdate[] = [];
  onException(e: Error): void {
    this.log.push(`exception ${e.message}`);
  }
  onUpdate(update: WaveViewServiceUpdate): void {
    this.updates.push(update);
    const parts: string[] = [];
    if (update.hasChannelId()) parts.push(`channel=${update.getChannelId()}`);
    if (update.hasWaveletId())
      parts.push(`wavelet=${update.getWaveletId()!.id}`);
    if (update.hasWaveletSnapshot())
      parts.push(`snapshot@${update.getWaveletSnapshot()!.version}`);
    if (update.hasDeltas()) {
      parts.push(
        `deltas=${update
          .getDeltaList()
          .map((d) => `${d.appliedAtVersion}+${d.length}`)
          .join(",")}`,
      );
    }
    if (update.hasLastCommittedVersion())
      parts.push(`committed=${update.getLastCommittedVersion()!.version}`);
    if (update.hasCurrentVersion())
      parts.push(`current=${update.getCurrentVersion()!.version}`);
    if (update.hasMarker()) parts.push("marker");
    this.log.push(parts.join(" "));
  }
  onSuccess(response: string | null): void {
    this.log.push(`success ${response}`);
  }
  onFailure(reason: string): void {
    this.log.push(`failure ${reason}`);
  }
}

function insertDelta(
  author: ParticipantId,
  at: number,
  text: string,
  size: number,
): TransformedWaveletDelta {
  const end = version(at + 1);
  const op = new DocOpBuilder()
    .retain(3)
    .characters(text)
    .retain(size - 3)
    .build();
  return new TransformedWaveletDelta(author, end, 0, [
    WaveletOperation.blipContent(
      WaveletOperationContext.withHashedVersion(author, 0, 1, end),
      "b+1",
      op,
    ),
  ]);
}

function deltasUpdate(
  deltas: TransformedWaveletDelta[],
): ProtocolWaveletUpdate {
  return {
    waveletName: WIRE_NAME,
    appliedDelta: deltas.map(serializeTransformedDelta),
    resultingVersion: serializeHashedVersion(
      deltas[deltas.length - 1]!.resultingVersion,
    ),
    channelId: "ch1",
  };
}

function snapshotData(v: number): WaveletData {
  const data = WaveletData.empty(NAME, ALICE, version(v), 1000);
  data.addParticipant(ALICE);
  data.addParticipant(BOB);
  data.createDocument(
    "b+1",
    ALICE,
    [ALICE],
    new DocOpBuilder()
      .elementStart("body", Attributes.EMPTY)
      .elementStart("line", Attributes.EMPTY)
      .elementEnd()
      .characters("hello")
      .elementEnd()
      .buildInitialization(),
    1000,
    v,
  );
  return data;
}

function snapshotUpdate(v: number, committed: number): ProtocolWaveletUpdate {
  return {
    waveletName: WIRE_NAME,
    appliedDelta: [],
    commitNotice: serializeHashedVersion(version(committed)),
    resultingVersion: serializeHashedVersion(version(v)),
    snapshot: serializeWavelet(snapshotData(v), version(committed)),
    channelId: "ch1",
  };
}

function chain(error: unknown): string | undefined {
  let text: string | undefined;
  for (let e = error, i = 0; e instanceof Error && i < 8; e = e.cause, i++) {
    text = `${text ?? ""} <- ${e.message.slice(0, 300)}`;
  }
  return text;
}

const MARKER: ProtocolWaveletUpdate = {
  waveletName: DUMMY,
  appliedDelta: [],
  marker: true,
};

describe("RemoteWaveViewService", () => {
  test("open request: filter prefixes and the latest known version", () => {
    const transport = new FakeTransport();
    const service = new RemoteWaveViewService(WAVE, transport);
    const known = new WaveletIdMap<readonly HashedVersion[]>([
      [ROOT, [version(3), version(5)]],
    ]);
    service.viewOpen(
      IdFilter.of([WaveletId.of("example.com", "user+x")], ["conv"]),
      known,
      new Recorder(),
    );
    expect(transport.opens[0]!.request).toEqual({
      participantId: "",
      waveId: "example.com/w+1",
      waveletIdPrefix: ["conv", "user+x"],
      knownWavelet: [
        {
          waveletId: "example.com/conv+root",
          hashedVersion: serializeHashedVersion(version(5)),
        },
      ],
    });
  });

  test("the open waits for the connection", () => {
    const transport = new FakeTransport();
    transport.connected = false;
    const service = new RemoteWaveViewService(WAVE, transport);
    service.viewOpen(IdFilter.allIds(), new WaveletIdMap(), new Recorder());
    expect(transport.opens.length).toBe(0);
    transport.setConnected(true);
    expect(transport.opens.length).toBe(1);
    transport.setConnected(false);
    transport.setConnected(true);
    expect(transport.opens.length).toBe(1);
  });

  test("snapshot: channel id alone first, the update's version, the commit notice", () => {
    const transport = new FakeTransport();
    let markers = 0;
    const service = new RemoteWaveViewService(WAVE, transport, {
      onMarker: () => markers++,
    });
    const recorder = new Recorder();
    service.viewOpen(IdFilter.allIds(), new WaveletIdMap(), recorder);
    transport.stream.onUpdate(snapshotUpdate(7, 5));
    transport.stream.onUpdate(MARKER);
    transport.stream.onUpdate(
      deltasUpdate([insertDelta(BOB, 7, "x", 9), insertDelta(BOB, 8, "y", 10)]),
    );
    transport.stream.onUpdate({
      waveletName: WIRE_NAME,
      appliedDelta: [],
      commitNotice: serializeHashedVersion(version(9)),
      channelId: "ch1",
    });
    expect(recorder.log).toEqual([
      "channel=ch1",
      "wavelet=conv+root snapshot@7 committed=5 current=7",
      "marker",
      "wavelet=conv+root deltas=7+1,8+1 current=9",
      "wavelet=conv+root committed=9",
    ]);
    expect(markers).toBe(1);
    const snapshot = recorder.updates[1]!.getWaveletSnapshot()!;
    expect(snapshot.hashedVersion.equals(version(7))).toBe(true);
    // Only the last delta of an update has a history hash.
    const deltas = recorder.updates[3]!.getDeltaList();
    expect(deltas[0]!.resultingVersion.historyHash.length).toBe(0);
    expect(deltas[1]!.resultingVersion.equals(version(9))).toBe(true);
  });

  test("a rewind: the deltas taken back, ending at the last version (not in the original)", () => {
    const transport = new FakeTransport();
    const recorder = new Recorder();
    new RemoteWaveViewService(WAVE, transport).viewOpen(
      IdFilter.allIds(),
      new WaveletIdMap(),
      recorder,
    );
    transport.stream.onUpdate(snapshotUpdate(7, 5));
    transport.stream.onUpdate(MARKER);
    const rewound = [insertDelta(BOB, 7, "x", 9), insertDelta(BOB, 8, "y", 10)];
    transport.stream.onUpdate(deltasUpdate(rewound));
    transport.stream.onUpdate({
      waveletName: WIRE_NAME,
      appliedDelta: rewound.map(serializeTransformedDelta),
      resultingVersion: serializeHashedVersion(version(7)),
      resetVersion: serializeHashedVersion(version(7)),
    });
    expect(recorder.log.slice(4)).toEqual([
      "wavelet=conv+root deltas=7+1,8+1 current=7",
    ]);
    const reset = recorder.updates[4]!;
    expect(reset.getResetVersion!()!.equals(version(7))).toBe(true);
    expect(reset.getDeltaList()[1]!.resultingVersion.equals(version(9))).toBe(
      true,
    );
    // The history goes on from the fork.
    transport.stream.onUpdate(deltasUpdate([insertDelta(ALICE, 7, "z", 9)]));
    expect(recorder.log.slice(5)).toEqual([
      "wavelet=conv+root deltas=7+1 current=8",
    ]);
  });

  test("an empty view: channel id on the dummy wavelet, then the marker", () => {
    const transport = new FakeTransport();
    const service = new RemoteWaveViewService(WAVE, transport);
    const recorder = new Recorder();
    service.viewOpen(IdFilter.allIds(), new WaveletIdMap(), recorder);
    transport.stream.onUpdate({
      waveletName: DUMMY,
      appliedDelta: [],
      channelId: "ch1",
    });
    transport.stream.onUpdate(MARKER);
    expect(recorder.log).toEqual(["channel=ch1", "marker"]);
  });

  test("an acknowledged known version becomes an empty delta with the current version", () => {
    const resync: ProtocolWaveletUpdate = {
      waveletName: WIRE_NAME,
      appliedDelta: [],
      commitNotice: serializeHashedVersion(version(4)),
      resultingVersion: serializeHashedVersion(version(5)),
      channelId: "ch1",
    };
    const known = new WaveletIdMap<readonly HashedVersion[]>([
      [ROOT, [version(5)]],
    ]);

    // Nothing missed: the known version is the current one.
    let transport = new FakeTransport();
    let recorder = new Recorder();
    new RemoteWaveViewService(WAVE, transport).viewOpen(
      IdFilter.allIds(),
      known,
      recorder,
    );
    transport.stream.onUpdate(resync);
    expect(recorder.log).toEqual(["channel=ch1"]);
    transport.stream.onUpdate(MARKER);
    expect(recorder.log).toEqual([
      "channel=ch1",
      "wavelet=conv+root deltas=5+0 committed=4 current=5",
      "marker",
    ]);
    expect(
      recorder.updates[1]!.getDeltaList()[0]!.resultingVersion.equals(
        version(5),
      ),
    ).toBe(true);

    // Deltas missed: the current version is where they end.
    transport = new FakeTransport();
    recorder = new Recorder();
    new RemoteWaveViewService(WAVE, transport).viewOpen(
      IdFilter.allIds(),
      known,
      recorder,
    );
    transport.stream.onUpdate(resync);
    transport.stream.onUpdate(
      deltasUpdate([insertDelta(BOB, 5, "x", 9), insertDelta(BOB, 6, "y", 10)]),
    );
    transport.stream.onUpdate(MARKER);
    expect(recorder.log).toEqual([
      "channel=ch1",
      "wavelet=conv+root deltas=5+0 committed=4 current=7",
      "wavelet=conv+root deltas=5+1,6+1 current=7",
      "marker",
    ]);
  });

  test("a wavelet coming into view by its deltas from version zero becomes a snapshot", () => {
    const transport = new FakeTransport();
    const recorder = new Recorder();
    new RemoteWaveViewService(WAVE, transport).viewOpen(
      IdFilter.allIds(),
      new WaveletIdMap(),
      recorder,
    );
    transport.stream.onUpdate({
      waveletName: DUMMY,
      appliedDelta: [],
      channelId: "ch1",
    });
    transport.stream.onUpdate(MARKER);
    const end = version(1);
    const first = new TransformedWaveletDelta(ALICE, end, 0, [
      WaveletOperation.addParticipant(
        WaveletOperationContext.withHashedVersion(ALICE, 0, 1, end),
        ALICE,
      ),
    ]);
    transport.stream.onUpdate(deltasUpdate([first]));
    expect(recorder.log).toEqual([
      "channel=ch1",
      "marker",
      "wavelet=conv+root snapshot@1 committed=0 current=1",
    ]);
    const snapshot = recorder.updates[2]!.getWaveletSnapshot()!;
    expect(snapshot.participants.map(String)).toEqual(["alice@example.com"]);
    expect(snapshot.hashedVersion.equals(end)).toBe(true);
  });

  test("a snapshot in mid-stream gets its own version as commit notice", () => {
    const transport = new FakeTransport();
    const recorder = new Recorder();
    new RemoteWaveViewService(WAVE, transport).viewOpen(
      IdFilter.allIds(),
      new WaveletIdMap(),
      recorder,
    );
    transport.stream.onUpdate({
      waveletName: DUMMY,
      appliedDelta: [],
      channelId: "ch1",
    });
    transport.stream.onUpdate(MARKER);
    transport.stream.onUpdate({
      waveletName: WIRE_NAME,
      appliedDelta: [],
      resultingVersion: serializeHashedVersion(version(7)),
      snapshot: serializeWavelet(snapshotData(7), version(7)),
      channelId: "ch1",
    });
    expect(recorder.log[2]).toBe(
      "wavelet=conv+root snapshot@7 committed=7 current=7",
    );
  });

  test("how a stream ends decides whether the view is opened again", () => {
    const open = (): { transport: FakeTransport; recorder: Recorder } => {
      const transport = new FakeTransport();
      const recorder = new Recorder();
      new RemoteWaveViewService(WAVE, transport).viewOpen(
        IdFilter.allIds(),
        new WaveletIdMap(),
        recorder,
      );
      return { transport, recorder };
    };
    let { transport, recorder } = open();
    transport.stream.onFinished(
      "Wave view changed while it was opened; reopen the wave",
    );
    expect(recorder.log).toEqual([
      "failure Wave view changed while it was opened; reopen the wave",
    ]);
    ({ transport, recorder } = open());
    transport.stream.onFinished("Not logged in");
    expect(recorder.log).toEqual(["success Not logged in"]);
    ({ transport, recorder } = open());
    transport.stream.onDisconnected("gone");
    transport.stream.onUpdate(MARKER);
    expect(recorder.log).toEqual(["failure gone"]);
    ({ transport, recorder } = open());
    transport.stream.onUpdate({ waveletName: "nonsense", appliedDelta: [] });
    expect(recorder.log[0]).toMatch(/^exception /);
  });

  test("viewClose cancels the stream of that channel; submit responses", () => {
    const transport = new FakeTransport();
    const service = new RemoteWaveViewService(WAVE, transport);
    const recorder = new Recorder();
    service.viewOpen(IdFilter.allIds(), new WaveletIdMap(), recorder);
    transport.stream.onUpdate(snapshotUpdate(7, 7));
    const results: string[] = [];
    const callback = {
      onSuccess: (
        v: HashedVersion,
        ops: number,
        error: string | null,
        code: { name: string },
      ) => results.push(`${v.version} ${ops} ${error} ${code.name}`),
      onFailure: (reason: string) => results.push(`failure ${reason}`),
    };
    const delta = deserializeDelta({
      hashedVersion: serializeHashedVersion(version(7)),
      author: ALICE.address,
      operation: [{ noOp: true }],
      addressPath: [],
    });
    for (let i = 0; i < 3; i++) {
      service.viewSubmit(NAME, delta, "ch1", callback);
    }
    expect(transport.submits[0]!.request.channelId).toBe("ch1");
    expect(transport.submits[0]!.request.waveletName).toBe(WIRE_NAME);
    transport.submits[0]!.respond({
      operationsApplied: 1,
      hashedVersionAfterApplication: serializeHashedVersion(version(8)),
    });
    transport.submits[1]!.respond({ operationsApplied: 0, errorMessage: "no" });
    transport.submits[2]!.fail("dropped");
    expect(results).toEqual([
      "8 1 null OK",
      "0 0 no INTERNAL_ERROR",
      "failure dropped",
    ]);

    let closed = 0;
    service.viewClose(WAVE, "ch1", {
      onSuccess: () => closed++,
      onFailure: () => {},
    });
    expect(closed).toBe(1);
    expect(transport.opens[0]!.cancelled).toBe(true);
  });

  test("a delta waiting for its host stays unanswered and is counted", () => {
    const transport = new FakeTransport();
    const changes: number[] = [];
    const service = new RemoteWaveViewService(WAVE, transport, {
      onAwaitingHost: (change) => changes.push(change),
    });
    service.viewOpen(IdFilter.allIds(), new WaveletIdMap(), new Recorder());
    transport.stream.onUpdate(snapshotUpdate(7, 7));
    const results: string[] = [];
    const callback = {
      onSuccess: (v: HashedVersion, ops: number, error: string | null) =>
        results.push(`${v.version} ${ops} ${error}`),
      onFailure: (reason: string) => results.push(`failure ${reason}`),
    };
    const delta = deserializeDelta({
      hashedVersion: serializeHashedVersion(version(7)),
      author: ALICE.address,
      operation: [{ noOp: true }],
      addressPath: [],
    });
    service.viewSubmit(NAME, delta, "ch1", callback);
    service.viewSubmit(NAME, delta, "ch1", callback);
    transport.submits[0]!.pending();
    transport.submits[0]!.pending();
    transport.submits[1]!.pending();
    // No answer yet, and no error: the deltas stay unacknowledged.
    expect(results).toEqual([]);
    expect(changes).toEqual([1, 1]);
    transport.submits[0]!.respond({
      operationsApplied: 1,
      hashedVersionAfterApplication: serializeHashedVersion(version(8)),
    });
    expect(results).toEqual(["8 1 null"]);
    expect(changes).toEqual([1, 1, -1]);
    service.dispose();
    expect(changes).toEqual([1, 1, -1, -1]);
  });
});

describe("LiveWaveView over the wire translation", () => {
  afterEach(() => ViewChannelImpl.resetViewChannelsPerWave());

  function openLive(): { transport: FakeTransport; live: LiveWaveView } {
    const transport = new FakeTransport();
    const service = new RemoteWaveViewService(WAVE, transport, {
      clock: () => 5,
    });
    const live = new LiveWaveView({
      waveId: WAVE,
      viewer: ALICE,
      service,
      scheduler: new ImmediateExecutionScheduler(),
      clock: () => 5,
    });
    live.open();
    return { transport, live };
  }

  test("a delta held back behind several server deltas is sent in its transformed form", () => {
    const { transport, live } = openLive();
    transport.stream.onUpdate(snapshotUpdate(7, 7));
    transport.stream.onUpdate(MARKER);
    expect(live.state).toBe("open");
    const root = live.view.wavelet(ROOT)!;

    // A delta goes out; the connection drops before the answer, and
    // another edit is made offline.
    root.editDocument("b+1", (doc) => doc.insertText(7, "!"));
    expect(transport.submits.length).toBe(1);
    const first = deserializeDelta(transport.submits[0]!.request.delta);
    transport.setConnected(false);
    transport.submits[0]!.fail("gone");
    transport.stream.onDisconnected("gone");
    root.editDocument("b+1", (doc) => doc.insertText(8, "?"));
    expect(live.unsavedState.saved).toBe(false);
    transport.setConnected(true);
    expect(transport.opens.length).toBe(2);
    expect(transport.opens[1]!.request.knownWavelet).toEqual([
      {
        waveletId: "example.com/conv+root",
        hashedVersion: serializeHashedVersion(version(7)),
      },
    ]);

    // The server had applied the first delta, and one of bob's after it.
    // It acknowledges version 7 and sends both in one update, in which only
    // the last delta has a history hash.
    transport.stream.onUpdate({
      waveletName: WIRE_NAME,
      appliedDelta: [],
      commitNotice: serializeHashedVersion(version(7)),
      resultingVersion: serializeHashedVersion(version(7)),
      channelId: "ch1",
    });
    const echoEnd = version(8);
    const echo = new TransformedWaveletDelta(ALICE, echoEnd, 0, [
      first.ops[0]!.cloneOp(
        WaveletOperationContext.withHashedVersion(ALICE, 0, 1, echoEnd),
      ),
    ]);
    transport.stream.onUpdate(
      deltasUpdate([echo, insertDelta(BOB, 8, "y", 10)]),
    );
    transport.stream.onUpdate(MARKER);
    expect(chain(live.error)).toBeUndefined();

    // The first delta was recognised and is not sent again. The offline
    // edit is submitted once, against version 9 with its hash and moved
    // behind bob's character. (The original would send it as it was when
    // the echo was recognised: against version 8, which has no hash here.)
    expect(transport.submits.length).toBe(2);
    const sent = deserializeDelta(transport.submits[1]!.request.delta);
    expect(sent.targetVersion.toString()).toBe(version(9).toString());
    expect(sent.ops.map(String).join()).toContain('__9; ++"?"; __2;');
    expect(root.document("b+1").toXmlString()).toBe(
      "<body><line/>yhell!?o</body>",
    );

    transport.submits[1]!.respond({
      operationsApplied: 1,
      hashedVersionAfterApplication: serializeHashedVersion(version(10)),
    });
    expect(root.data().hashedVersion.equals(version(10))).toBe(true);
    transport.stream.onUpdate({
      waveletName: WIRE_NAME,
      appliedDelta: [],
      commitNotice: serializeHashedVersion(version(10)),
      channelId: "ch1",
    });
    expect(live.unsavedState.saved).toBe(true);
    expect(live.state).toBe("open");
    live.close();
  });

  test("a wavelet created here is replaced by the server's snapshot when it cannot be resumed", () => {
    const { transport, live } = openLive();
    transport.stream.onUpdate({
      waveletName: DUMMY,
      appliedDelta: [],
      channelId: "ch1",
    });
    transport.stream.onUpdate(MARKER);
    const root = live.createWavelet(ROOT);
    root.editDocument("b+1", (doc) => {
      doc.appendXml(XmlFragment.parse("<body><line/></body>"));
    });
    expect(transport.submits.length).toBe(1);
    expect(
      deserializeDelta(
        transport.submits[0]!.request.delta,
      ).targetVersion.equals(version(0)),
    ).toBe(true);
    transport.submits[0]!.respond({
      operationsApplied: 1,
      hashedVersionAfterApplication: serializeHashedVersion(version(1)),
    });
    expect(transport.submits.length).toBe(2);
    transport.submits[1]!.respond({
      operationsApplied: 1,
      hashedVersionAfterApplication: serializeHashedVersion(version(2)),
    });
    expect(chain(live.error)).toBeUndefined();

    // Reconnect; the server does not know version 2 and sends a snapshot.
    let resets = 0;
    live.events.on("waveletReset", () => resets++);
    transport.setConnected(false);
    transport.stream.onDisconnected("gone");
    transport.setConnected(true);
    transport.stream.onUpdate(snapshotUpdate(7, 7));
    transport.stream.onUpdate(MARKER);
    expect(`${live.state} ${chain(live.error)}`).toBe("open undefined");
    expect(resets).toBe(1);
    const replaced = live.view.wavelet(ROOT)!;
    expect(replaced).not.toBe(root);
    expect(replaced.document("b+1").toXmlString()).toBe(
      "<body><line/>hello</body>",
    );

    // And the new wavelet is live.
    transport.stream.onUpdate(deltasUpdate([insertDelta(BOB, 7, "x", 9)]));
    expect(replaced.document("b+1").toXmlString()).toBe(
      "<body><line/>xhello</body>",
    );
    replaced.editDocument("b+1", (doc) => doc.insertText(3, "a"));
    expect(transport.submits.length).toBe(3);
    expect(
      deserializeDelta(
        transport.submits[2]!.request.delta,
      ).targetVersion.equals(version(8)),
    ).toBe(true);
    live.close();
  });
});
