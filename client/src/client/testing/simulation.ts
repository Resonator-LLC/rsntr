// A simulated wave server and network for tests: an in-memory server that
// speaks the wire protocol of the resonator front end (snapshots, known
// versions, deltas, commit notices, own-delta suppression, submit
// responses) over connections with random latency and disconnections, on a
// virtual clock with a seeded random number generator.
//
// The server side follows `crates/resonator-server/src/frontend/mod.rs`; the
// transform of submitted deltas is the model's `transformSubmittedDelta`.
// Test support only.

import { WaveletData } from "../../model/data";
import {
  ModernIdSerialiser,
  type ParticipantId,
  type WaveId,
  type WaveletId,
  WaveletName,
} from "../../model/id";
import {
  SubmitTransformError,
  TransformedWaveletDelta,
  type WaveletDelta,
  applyWaveletDelta,
  transformSubmittedDelta,
} from "../../model/operation";
import { HashedVersion, HashedVersionFactory } from "../../model/version";
import type { Unsubscribe } from "../../util/events";
import {
  ProtocolSubmitRequest,
  ProtocolSubmitResponse,
  ProtocolWaveletUpdate,
  deserializeDelta,
  deserializeHashedVersion,
  serializeHashedVersion,
  serializeTransformedDelta,
  serializeWavelet,
} from "../../wire";
import type { ProtocolOpenRequest } from "../../wire";
import type { Timers } from "../backoffScheduler";
import type { ViewStream, ViewStreamHandler, WaveRpcTransport } from "../waveRpc";

/** mulberry32. */
export function seededRandom(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface Scheduled {
  readonly at: number;
  readonly order: number;
  readonly run: () => void;
  cancelled: boolean;
}

/** A virtual clock with an event queue. */
export class SimLoop implements Timers {
  readonly random: () => number;
  private time = 0;
  private order = 0;
  private queue: Scheduled[] = [];

  constructor(seed: number) {
    this.random = seededRandom(seed);
  }

  now(): number {
    return this.time;
  }

  /** A random integer in `[min, max]`. */
  int(min: number, max: number): number {
    return min + Math.floor(this.random() * (max - min + 1));
  }

  chance(probability: number): boolean {
    return this.random() < probability;
  }

  at(delayMs: number, run: () => void): Scheduled {
    const event: Scheduled = {
      at: this.time + Math.max(0, delayMs),
      order: this.order++,
      run,
      cancelled: false,
    };
    this.queue.push(event);
    return event;
  }

  setTimeout(callback: () => void, delayMs: number): unknown {
    return this.at(delayMs, callback);
  }

  clearTimeout(handle: unknown): void {
    if (handle !== undefined && handle !== null) {
      (handle as Scheduled).cancelled = true;
    }
  }

  pending(): number {
    return this.queue.filter((event) => !event.cancelled).length;
  }

  /** Runs the next event. Returns false if there is none. */
  step(): boolean {
    let best = -1;
    for (let i = 0; i < this.queue.length; i++) {
      const event = this.queue[i]!;
      if (event.cancelled) {
        continue;
      }
      const current = best < 0 ? undefined : this.queue[best];
      if (
        current === undefined ||
        event.at < current.at ||
        (event.at === current.at && event.order < current.order)
      ) {
        best = i;
      }
    }
    if (best < 0) {
      this.queue = [];
      return false;
    }
    const event = this.queue[best]!;
    this.queue.splice(best, 1);
    this.time = Math.max(this.time, event.at);
    event.run();
    return true;
  }

  /** Runs events until none is left. Throws after `maxSteps`. */
  runUntilIdle(maxSteps = 1_000_000): void {
    for (let steps = 0; this.step(); steps++) {
      if (steps > maxSteps) {
        throw new Error("simulation does not settle");
      }
    }
  }

  /** Runs the events of the next `durationMs`. */
  runFor(durationMs: number): void {
    const end = this.time + durationMs;
    for (;;) {
      const next = this.queue
        .filter((event) => !event.cancelled)
        .reduce<Scheduled | undefined>(
          (a, b) => (a === undefined || b.at < a.at || (b.at === a.at && b.order < a.order) ? b : a),
          undefined,
        );
      if (next === undefined || next.at > end) {
        break;
      }
      this.step();
    }
    this.time = end;
  }
}

interface ServerWavelet {
  readonly name: WaveletName;
  readonly data: WaveletData;
  /** History, oldest first. */
  readonly deltas: TransformedWaveletDelta[];
  /** `versions[i]` is the hashed version before `deltas[i]`; one more at the end. */
  readonly versions: HashedVersion[];
  committed: HashedVersion;
}

interface ChannelState {
  lastVersion: number | null;
  readonly ownEndVersions: Set<number>;
}

interface Subscription {
  readonly connection: SimConnection;
  readonly epoch: number;
  readonly channelId: string;
  readonly waveId: WaveId;
  readonly prefixes: readonly string[];
  readonly handler: ViewStreamHandler;
  readonly channels: Map<string, ChannelState>;
  closed: boolean;
}

export interface SimServerOptions {
  /** Probability that a commit notice follows a delta at once; otherwise it comes with a later one. Default 0.7. */
  commitProbability?: number;
  /** Largest number of deltas kept as history per wavelet (0: unlimited). Older known versions get a snapshot. */
  historyLimit?: number;
}

/** The simulated server: wavelets, subscriptions and delta submission. */
export class SimServer {
  readonly wavelets = new Map<string, ServerWavelet>();
  private readonly subscriptions: Subscription[] = [];
  private nextChannel = 1;
  /** Deltas applied, by wavelet key: for assertions. */
  submitsApplied = 0;
  duplicateSubmits = 0;
  submitErrors: string[] = [];

  constructor(
    readonly loop: SimLoop,
    private readonly options: SimServerOptions = {},
  ) {}

  connect(user: ParticipantId, options: SimConnectionOptions = {}): SimConnection {
    return new SimConnection(this, user, options);
  }

  wavelet(name: WaveletName): ServerWavelet | undefined {
    return this.wavelets.get(keyOf(name));
  }

  private hash(name: WaveletName, version: number, salt: number): HashedVersion {
    // Any value unique per wavelet and version will do.
    let h = 2166136261 ^ salt;
    for (const c of `${keyOf(name)}#${version}`) {
      h = Math.imul(h ^ c.charCodeAt(0), 16777619);
    }
    return HashedVersion.of(
      version,
      Uint8Array.of((h >>> 24) & 255, (h >>> 16) & 255, (h >>> 8) & 255, h & 255),
    );
  }

  private sees(user: ParticipantId, wavelet: ServerWavelet): boolean {
    return wavelet.data.hasParticipant(user);
  }

  private includes(subscription: Subscription, waveletId: WaveletId): boolean {
    return subscription.prefixes.some((prefix) => waveletId.id.startsWith(prefix));
  }

  private send(subscription: Subscription, update: ProtocolWaveletUpdate): void {
    if (subscription.closed) {
      return;
    }
    // Through the JSON codec, as on the wire.
    const text = ProtocolWaveletUpdate.encode(update);
    subscription.connection.toClient(subscription.epoch, () => {
      if (!subscription.closed) {
        subscription.handler.onUpdate(ProtocolWaveletUpdate.parse(text));
      }
    });
  }

  /** Handles an open request (`ClientFrontend::open`). */
  open(
    connection: SimConnection,
    epoch: number,
    request: ProtocolOpenRequest,
    handler: ViewStreamHandler,
  ): Subscription {
    const waveId = ModernIdSerialiser.INSTANCE.deserialiseWaveId(request.waveId);
    const subscription: Subscription = {
      connection,
      epoch,
      channelId: `ch${this.nextChannel++}`,
      waveId,
      prefixes: request.waveletIdPrefix,
      handler,
      channels: new Map(),
      closed: false,
    };
    this.subscriptions.push(subscription);
    const serialiser = ModernIdSerialiser.INSTANCE;
    let inView = 0;
    const wavelets = [...this.wavelets.values()]
      .filter((wavelet) => wavelet.name.waveId.equals(waveId))
      .sort((a, b) => (a.name.waveletId.id < b.name.waveletId.id ? -1 : 1));
    for (const wavelet of wavelets) {
      const waveletId = wavelet.name.waveletId;
      if (!this.includes(subscription, waveletId) || !this.sees(connection.user, wavelet)) {
        continue;
      }
      const waveletName = serialiser.serialiseWaveletName(wavelet.name);
      const current = wavelet.data.hashedVersion;
      const known = request.knownWavelet.find(
        (entry) => entry.waveletId === serialiser.serialiseWaveletId(waveletId),
      );
      let resync: TransformedWaveletDelta[] | null = null;
      let knownVersion: HashedVersion | null = null;
      if (known !== undefined) {
        knownVersion = deserializeHashedVersion(known.hashedVersion);
        const index = wavelet.versions.findIndex((version) => version.equals(knownVersion!));
        const limit = this.options.historyLimit ?? 0;
        const forgotten = limit > 0 && index >= 0 && wavelet.deltas.length - index > limit;
        if (index >= 0 && !forgotten) {
          resync = wavelet.deltas.slice(index);
        }
      }
      const base = {
        waveletName,
        appliedDelta: [],
        commitNotice: serializeHashedVersion(wavelet.committed),
        channelId: subscription.channelId,
      };
      if (resync !== null && knownVersion !== null) {
        this.send(subscription, { ...base, resultingVersion: serializeHashedVersion(knownVersion) });
        if (resync.length > 0) {
          this.send(subscription, {
            waveletName,
            appliedDelta: resync.map(serializeTransformedDelta),
            resultingVersion: serializeHashedVersion(current),
            channelId: subscription.channelId,
          });
        }
      } else {
        this.send(subscription, {
          ...base,
          resultingVersion: serializeHashedVersion(current),
          // The snapshot's own version is the committed version.
          snapshot: serializeWavelet(wavelet.data, wavelet.committed),
        });
      }
      subscription.channels.set(waveletId.serialise(), {
        lastVersion: current.version,
        ownEndVersions: new Set(),
      });
      inView++;
    }
    const dummy = `${request.waveId}/~/dummy+root`;
    if (inView === 0) {
      this.send(subscription, {
        waveletName: dummy,
        appliedDelta: [],
        channelId: subscription.channelId,
      });
    }
    this.send(subscription, { waveletName: dummy, appliedDelta: [], marker: true });
    return subscription;
  }

  close(subscription: Subscription): void {
    subscription.closed = true;
    const index = this.subscriptions.indexOf(subscription);
    if (index >= 0) {
      this.subscriptions.splice(index, 1);
    }
  }

  /** Drops the subscriptions of a connection that went away. */
  dropConnection(connection: SimConnection): void {
    for (const subscription of [...this.subscriptions]) {
      if (subscription.connection === connection) {
        this.close(subscription);
      }
    }
  }

  /** Handles a submit request; returns the response. */
  submit(connection: SimConnection, request: ProtocolSubmitRequest): ProtocolSubmitResponse {
    try {
      return this.doSubmit(connection, request);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.submitErrors.push(message);
      return { operationsApplied: 0, errorMessage: message };
    }
  }

  private doSubmit(
    connection: SimConnection,
    request: ProtocolSubmitRequest,
  ): ProtocolSubmitResponse {
    const name = ModernIdSerialiser.INSTANCE.deserialiseWaveletName(request.waveletName);
    const key = keyOf(name);
    const submitted: WaveletDelta = deserializeDelta(request.delta);
    if (!submitted.author.equals(connection.user)) {
      throw new Error("delta author is not the connected user");
    }
    let wavelet = this.wavelets.get(key);
    if (wavelet === undefined) {
      const v0 = HashedVersionFactory.createVersionZero(name);
      wavelet = {
        name,
        data: WaveletData.empty(name, submitted.author, v0, this.loop.now()),
        deltas: [],
        versions: [v0],
        committed: v0,
      };
    } else if (!this.sees(connection.user, wavelet)) {
      throw new Error(`${connection.user} is not a participant of ${name}`);
    }
    const current = wavelet.data.hashedVersion;
    const index = wavelet.versions.findIndex((version) => version.equals(submitted.targetVersion));
    if (index < 0) {
      throw new Error(
        `delta targets ${submitted.targetVersion}, which is not in the history of ${name}`,
      );
    }
    let transformed: WaveletDelta;
    try {
      transformed = transformSubmittedDelta(submitted, current, wavelet.deltas.slice(index));
    } catch (error) {
      if (error instanceof SubmitTransformError) {
        throw new Error(`transform failed: ${error.message}`);
      }
      throw error;
    }
    if (!transformed.targetVersion.equals(current)) {
      // A duplicate of a delta in the history: answered like the first time.
      this.duplicateSubmits++;
      const at = wavelet.versions.findIndex((version) => version.equals(transformed.targetVersion));
      const end = wavelet.versions[at + 1] ?? transformed.targetVersion;
      return {
        operationsApplied: transformed.ops.length,
        hashedVersionAfterApplication: serializeHashedVersion(end),
      };
    }
    const resulting = this.hash(name, current.version + transformed.ops.length, wavelet.deltas.length);
    const applied = TransformedWaveletDelta.fromDelta(resulting, this.loop.now(), transformed);
    applyWaveletDelta(applied, wavelet.data);
    wavelet.deltas.push(applied);
    wavelet.versions.push(resulting);
    this.wavelets.set(key, wavelet);
    this.submitsApplied++;

    // The submitter's own delta is not echoed on the channel it names.
    if (request.channelId !== undefined) {
      const own = this.subscriptions.find(
        (subscription) =>
          subscription.channelId === request.channelId && subscription.connection === connection,
      );
      if (own !== undefined) {
        let channel = own.channels.get(name.waveletId.serialise());
        if (channel === undefined) {
          channel = { lastVersion: null, ownEndVersions: new Set() };
          own.channels.set(name.waveletId.serialise(), channel);
        }
        channel.ownEndVersions.add(resulting.version);
      }
    }
    this.distribute(wavelet, applied);
    if (this.loop.chance(this.options.commitProbability ?? 0.7)) {
      this.commit(wavelet);
    }
    return {
      operationsApplied: applied.ops.length,
      hashedVersionAfterApplication: serializeHashedVersion(resulting),
    };
  }

  /** `Subscription::on_update` for every subscription of the wave. */
  private distribute(wavelet: ServerWavelet, delta: TransformedWaveletDelta): void {
    const waveletName = ModernIdSerialiser.INSTANCE.serialiseWaveletName(wavelet.name);
    const key = wavelet.name.waveletId.serialise();
    for (const subscription of [...this.subscriptions]) {
      if (
        subscription.closed ||
        !subscription.waveId.equals(wavelet.name.waveId) ||
        !this.includes(subscription, wavelet.name.waveletId)
      ) {
        continue;
      }
      const user = subscription.connection.user;
      const visibleAfter = this.sees(user, wavelet);
      const removed = delta.ops.some(
        (op) => op.kind.type === "removeParticipant" && op.kind.participant.equals(user),
      );
      if (!visibleAfter && !removed) {
        continue;
      }
      let channel = subscription.channels.get(key);
      if (channel === undefined) {
        channel = { lastVersion: null, ownEndVersions: new Set() };
        subscription.channels.set(key, channel);
      }
      if (channel.lastVersion === null && delta.appliedAtVersion !== 0) {
        // The wavelet comes into view in the middle of its history.
        if (!visibleAfter) {
          subscription.channels.delete(key);
          continue;
        }
        channel.lastVersion = wavelet.data.version;
        this.send(subscription, {
          waveletName,
          appliedDelta: [],
          resultingVersion: serializeHashedVersion(wavelet.data.hashedVersion),
          snapshot: serializeWavelet(wavelet.data, wavelet.data.hashedVersion),
          channelId: subscription.channelId,
        });
        continue;
      }
      channel.lastVersion = delta.resultingVersion.version;
      const own = channel.ownEndVersions.delete(delta.resultingVersion.version);
      if (!own) {
        this.send(subscription, {
          waveletName,
          appliedDelta: [serializeTransformedDelta(delta)],
          resultingVersion: serializeHashedVersion(delta.resultingVersion),
          channelId: subscription.channelId,
        });
      }
      if (!visibleAfter) {
        subscription.channels.delete(key);
      }
    }
  }

  /** Commits a wavelet's current version and notifies its viewers. */
  commit(wavelet: ServerWavelet): void {
    if (wavelet.committed.equals(wavelet.data.hashedVersion)) {
      return;
    }
    wavelet.committed = wavelet.data.hashedVersion;
    const waveletName = ModernIdSerialiser.INSTANCE.serialiseWaveletName(wavelet.name);
    const key = wavelet.name.waveletId.serialise();
    for (const subscription of this.subscriptions) {
      const channel = subscription.channels.get(key);
      if (
        subscription.waveId.equals(wavelet.name.waveId) &&
        channel !== undefined &&
        channel.lastVersion !== null
      ) {
        this.send(subscription, {
          waveletName,
          appliedDelta: [],
          commitNotice: serializeHashedVersion(wavelet.committed),
          channelId: subscription.channelId,
        });
      }
    }
  }

  commitAll(): void {
    for (const wavelet of this.wavelets.values()) {
      this.commit(wavelet);
    }
  }
}

function keyOf(name: WaveletName): string {
  return ModernIdSerialiser.INSTANCE.serialiseWaveletName(name);
}

export interface SimConnectionOptions {
  /** Latency range of each message in either direction, in ms. Default 1 to 40. */
  minLatencyMs?: number;
  maxLatencyMs?: number;
  /**
   * Submit responses travel apart from view updates and may overtake them
   * or be overtaken (the original protocol's separate channels). Default
   * false: one ordered pipe, as on a WebSocket.
   */
  separateSubmitResponses?: boolean;
  /** Probability that a request in flight at a disconnection still reaches the server. Default 0.5. */
  deliverOnDropProbability?: number;
}

interface InFlight {
  readonly deliver: () => void;
  readonly isSubmit: boolean;
}

/** One client's connection to the simulated server. */
export class SimConnection implements WaveRpcTransport {
  private connected = false;
  private epoch = 0;
  private readonly listeners = new Set<(connected: boolean) => void>();
  private readonly streams = new Set<{ handler: ViewStreamHandler; finished: boolean }>();
  private readonly submits = new Set<{ onFailure: (reason: string) => void }>();
  /** Requests on their way to the server, in order. */
  private toServerQueue: InFlight[] = [];
  private lastArrival = { toServer: 0, toClient: 0, responses: 0 };
  /** Counters for assertions. */
  drops = 0;
  submitsSent = 0;

  constructor(
    private readonly server: SimServer,
    readonly user: ParticipantId,
    private readonly options: SimConnectionOptions,
  ) {}

  private get loop(): SimLoop {
    return this.server.loop;
  }

  private latency(): number {
    return this.loop.int(this.options.minLatencyMs ?? 1, this.options.maxLatencyMs ?? 40);
  }

  /** The delay that keeps a pipe in order. */
  private delayOn(pipe: "toServer" | "toClient" | "responses"): number {
    const arrival = Math.max(this.lastArrival[pipe], this.loop.now() + this.latency());
    this.lastArrival[pipe] = arrival;
    return arrival - this.loop.now();
  }

  isConnected(): boolean {
    return this.connected;
  }

  onConnectionChange(listener: (connected: boolean) => void): Unsubscribe {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Brings the connection up. */
  connect(): void {
    if (this.connected) {
      return;
    }
    this.connected = true;
    for (const listener of [...this.listeners]) {
      listener(true);
    }
  }

  /** Drops the connection: what is in flight is lost, or arrives without an answer. */
  disconnect(reason = "connection lost"): void {
    if (!this.connected) {
      return;
    }
    this.drops++;
    this.connected = false;
    this.epoch++;
    // Requests in flight: a prefix of them still reaches the server.
    const inFlight = this.toServerQueue;
    this.toServerQueue = [];
    const probability = this.options.deliverOnDropProbability ?? 0.5;
    for (const request of inFlight) {
      if (!request.isSubmit || !this.loop.chance(probability)) {
        break;
      }
      request.deliver();
    }
    this.server.dropConnection(this);
    const streams = [...this.streams];
    this.streams.clear();
    const submits = [...this.submits];
    this.submits.clear();
    for (const submit of submits) {
      submit.onFailure(reason);
    }
    for (const stream of streams) {
      if (!stream.finished) {
        stream.finished = true;
        stream.handler.onDisconnected(reason);
      }
    }
    for (const listener of [...this.listeners]) {
      listener(false);
    }
  }

  /** Schedules a delivery to the client on the ordered update pipe. */
  toClient(epoch: number, deliver: () => void, pipe: "toClient" | "responses" = "toClient"): void {
    this.loop.at(this.delayOn(pipe), () => {
      if (this.connected && this.epoch === epoch) {
        deliver();
      }
    });
  }

  private toServer(isSubmit: boolean, deliver: () => void): void {
    const epoch = this.epoch;
    const request: InFlight = { deliver, isSubmit };
    this.toServerQueue.push(request);
    this.loop.at(this.delayOn("toServer"), () => {
      const index = this.toServerQueue.indexOf(request);
      if (index < 0 || this.epoch !== epoch) {
        return;
      }
      this.toServerQueue.splice(index, 1);
      deliver();
    });
  }

  openView(request: ProtocolOpenRequest, handler: ViewStreamHandler): ViewStream {
    if (!this.connected) {
      throw new Error("not connected");
    }
    const epoch = this.epoch;
    const stream = { handler, finished: false };
    this.streams.add(stream);
    let subscription: Subscription | null = null;
    let cancelled = false;
    const guarded: ViewStreamHandler = {
      onUpdate: (update) => {
        if (!stream.finished) {
          handler.onUpdate(update);
        }
      },
      onFinished: (error) => handler.onFinished(error),
      onDisconnected: (reason) => handler.onDisconnected(reason),
    };
    this.toServer(false, () => {
      if (!cancelled) {
        subscription = this.server.open(this, epoch, request, guarded);
      }
    });
    return {
      cancel: () => {
        if (cancelled || stream.finished) {
          return;
        }
        cancelled = true;
        if (!this.connected || this.epoch !== epoch) {
          return;
        }
        this.toServer(false, () => {
          if (subscription !== null) {
            this.server.close(subscription);
          }
          this.toClient(epoch, () => {
            if (!stream.finished) {
              stream.finished = true;
              this.streams.delete(stream);
              handler.onFinished(undefined);
            }
          });
        });
      },
    };
  }

  submit(
    request: ProtocolSubmitRequest,
    onResponse: (response: ProtocolSubmitResponse) => void,
    onFailure: (reason: string) => void,
  ): void {
    if (!this.connected) {
      onFailure("not connected");
      return;
    }
    this.submitsSent++;
    const epoch = this.epoch;
    const pending = { onFailure };
    this.submits.add(pending);
    // Through the JSON codec, as on the wire.
    const text = ProtocolSubmitRequest.encode(request);
    this.toServer(true, () => {
      const response = ProtocolSubmitResponse.encode(
        this.server.submit(this, ProtocolSubmitRequest.parse(text)),
      );
      this.toClient(
        epoch,
        () => {
          if (this.submits.delete(pending)) {
            onResponse(ProtocolSubmitResponse.parse(response));
          }
        },
        this.options.separateSubmitResponses === true ? "responses" : "toClient",
      );
    });
  }
}
