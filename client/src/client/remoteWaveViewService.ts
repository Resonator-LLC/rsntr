// The wave view service of one wave over the wire protocol: translates
// between the concurrency-control channel stack (`WaveViewService`) and
// `ProtocolOpenRequest` / `ProtocolWaveletUpdate` / `ProtocolSubmitRequest`.
//
// Derived from `RemoteWaveViewService` and `RemoteViewServiceMultiplexer` of
// `org.waveprotocol.box.webclient.client` in Apache Wave, licensed under the
// Apache License, Version 2.0.
//
// Differences from the original, all owed to the resonator server:
//
// - Known wavelet versions are sent with the open request (the original
//   server refused them), so a view can resume after a dropped connection.
//   Of the reconnect versions the channel stack offers per wavelet, the
//   last, i.e. the most recent, is sent: the server honours one.
// - The server acknowledges a known version with an update that has no
//   deltas and the known version as resulting version, optionally followed
//   by one update with the deltas since. The channel stack expects the
//   original protocol's form instead: an empty delta at the known version
//   together with the *current* version, which tells the concurrency
//   control whether deltas follow and so whether an unacknowledged delta
//   may have been applied. The acknowledgement is therefore held back until
//   the next message of the stream shows which it is.
// - A snapshot's own version is the committed version (a quirk kept from
//   the Java server); the wavelet is given the update's resulting version.
// - Every update carries the channel id; it is passed on once, alone, as
//   the view channel requires.
// - A wavelet that comes into view while the view is open arrives as its
//   deltas from version zero (the channel stack has no channel for it and
//   would fail), or as a snapshot without a committed version (which the
//   delta channel requires). The first is turned into a snapshot built
//   from those deltas, the second gets the snapshot's own version, which
//   is the committed version, as its commit notice.
// - Received deltas get the local clock's time as application time.
// - The open waits for the connection; a dropped connection is reported as
//   a failure of the stream, which makes the multiplexer reconnect.

import {
  ChannelException,
  Recoverable,
  ResponseCode,
} from "../concurrencycontrol/common";
import type {
  CloseCallback,
  KnownWaveletVersions,
  OpenCallback,
  WaveViewService,
  WaveViewServiceSubmitCallback,
  WaveViewServiceUpdate,
} from "../concurrencycontrol/channel";
import type { WaveletData } from "../model/data";
import {
  type IdFilter,
  ModernIdSerialiser,
  ParticipantId,
  type WaveId,
  type WaveletId,
  type WaveletName,
} from "../model/id";
import {
  TransformedWaveletDelta,
  type WaveletDelta,
  buildWaveletFromDeltas,
} from "../model/operation";
import { HashedVersion, HashedVersionFactory } from "../model/version";
import type { Unsubscribe } from "../util/events";
import {
  type ProtocolWaveletDelta,
  type ProtocolWaveletUpdate,
  deserializeHashedVersion,
  deserializeTransformedDelta,
  deserializeWavelet,
  serializeDelta,
  serializeHashedVersion,
} from "../wire";
import type { ViewStream, WaveRpcTransport } from "./waveRpc";

/** Stream failures after which opening again is worth it. */
const RETRYABLE_OPEN_ERRORS: readonly string[] = [
  "Wave view changed while it was opened; reopen the wave",
  "Wave view receiver cannot keep up",
  "Wave server failed to look up wave",
  "Wave server failure retrieving wavelet",
];

/** Whether a failed open stream should be opened again rather than given up. */
export function isRetryableOpenError(error: string): boolean {
  return RETRYABLE_OPEN_ERRORS.some((text) => error.includes(text));
}

/** One message to the view channel. */
class Update implements WaveViewServiceUpdate {
  channelId: string | null = null;
  waveletId: WaveletId | null = null;
  lastCommittedVersion: HashedVersion | null = null;
  currentVersion: HashedVersion | null = null;
  snapshot: WaveletData | null = null;
  deltas: readonly TransformedWaveletDelta[] = [];
  marker = false;
  resetVersion: HashedVersion | null = null;

  getResetVersion(): HashedVersion | null {
    return this.resetVersion;
  }

  hasChannelId(): boolean {
    return this.channelId !== null;
  }
  getChannelId(): string | null {
    return this.channelId;
  }
  hasWaveletId(): boolean {
    return this.waveletId !== null;
  }
  getWaveletId(): WaveletId | null {
    return this.waveletId;
  }
  hasLastCommittedVersion(): boolean {
    return this.lastCommittedVersion !== null;
  }
  getLastCommittedVersion(): HashedVersion | null {
    return this.lastCommittedVersion;
  }
  hasCurrentVersion(): boolean {
    return this.currentVersion !== null;
  }
  getCurrentVersion(): HashedVersion | null {
    return this.currentVersion;
  }
  hasWaveletSnapshot(): boolean {
    return this.snapshot !== null;
  }
  getWaveletSnapshot(): WaveletData | null {
    return this.snapshot;
  }
  hasDeltas(): boolean {
    return this.deltas.length > 0;
  }
  getDeltaList(): readonly TransformedWaveletDelta[] {
    return this.deltas;
  }
  hasMarker(): boolean {
    return this.marker;
  }
  toString(): string {
    return (
      `[update channel=${this.channelId} wavelet=${this.waveletId} ` +
      `committed=${this.lastCommittedVersion} current=${this.currentVersion} ` +
      `snapshot=${this.snapshot !== null} deltas=${this.deltas.length} marker=${this.marker}]`
    );
  }
}

/** A known version acknowledged by the server, waiting to learn the current version. */
interface PendingResync {
  readonly waveletId: WaveletId;
  readonly version: HashedVersion;
  readonly committed: HashedVersion | null;
}

/** Author of the empty delta that carries a reconnect version; never read. */
const NO_AUTHOR = ParticipantId.newUnchecked("");

/** State of one open (or opening) view stream. */
class OpenView {
  stream: ViewStream | null = null;
  channelId: string | null = null;
  markerSeen = false;
  finished = false;
  cancelRequested = false;
  /** Waiting for the connection before the stream can be opened. */
  waiting: Unsubscribe | null = null;
  pendingResync: PendingResync[] = [];
  /** Wavelets the channel stack has a channel for, as far as this stream shows. */
  readonly announced = new Set<string>();

  constructor(readonly callback: OpenCallback) {}
}

/**
 * Remembers the last hashed version the server reported per wavelet, to
 * sign deltas whose target version came without a hash
 * (`VersionSignatureManager`).
 */
class VersionSignatures {
  private readonly versions = new Map<string, HashedVersion>();

  update(wavelet: WaveletName, version: HashedVersion): void {
    if (version.historyHash.length === 0) {
      return;
    }
    const key = wavelet.toString();
    const current = this.versions.get(key);
    if (current !== undefined && current.version > version.version) {
      return;
    }
    this.versions.set(key, version);
  }

  /** The last version reported, if any. */
  current(wavelet: WaveletName): HashedVersion | null {
    return this.versions.get(wavelet.toString()) ?? null;
  }

  /** The wavelet's history was rewound to `version`. */
  rewind(wavelet: WaveletName, version: HashedVersion): void {
    this.versions.set(wavelet.toString(), version);
  }

  /** The hashed version to submit a delta against, or null if unknown. */
  signed(wavelet: WaveletName, target: HashedVersion): HashedVersion | null {
    if (target.version === 0) {
      return HashedVersionFactory.createVersionZero(wavelet);
    }
    if (target.historyHash.length > 0) {
      return target;
    }
    const current = this.versions.get(wavelet.toString());
    return current !== undefined && current.version === target.version
      ? current
      : null;
  }
}

export interface RemoteWaveViewServiceOptions {
  /**
   * Called when a stream delivered its marker: the view is up to date. Used
   * to reset the reconnection backoff.
   */
  onMarker?: () => void;
  /**
   * Application time given to deltas received from the server, whose wire
   * form carries none (the original uses 0). Default `Date.now`, an
   * approximation that keeps blip modification times meaningful.
   */
  clock?: () => number;
  /**
   * Called with +1 when a submitted delta waits for its host (the server
   * keeps it until the host can be reached) and with -1 when it no longer
   * does (answered, failed, or this service disposed).
   */
  onAwaitingHost?: (change: 1 | -1) => void;
}

/** The view service of one wave. Serves the successive view channels of one multiplexer. */
export class RemoteWaveViewService implements WaveViewService {
  private readonly views = new Set<OpenView>();
  private readonly versions = new VersionSignatures();
  /** Wavelets deltas were submitted to: they have channels, created locally. */
  private readonly submitted = new Set<string>();
  private disposed = false;
  /** Submitted deltas waiting for their host. */
  private awaitingHost = 0;

  constructor(
    private readonly waveId: WaveId,
    private readonly transport: WaveRpcTransport,
    private readonly options: RemoteWaveViewServiceOptions = {},
  ) {}

  /** Drops every stream and pending open without notifying anyone. */
  dispose(): void {
    this.disposed = true;
    for (; this.awaitingHost > 0; this.awaitingHost--) {
      this.options.onAwaitingHost?.(-1);
    }
    for (const view of this.views) {
      this.endView(view, true);
    }
  }

  viewOpen(
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
    callback: OpenCallback,
  ): void {
    if (this.disposed) {
      return;
    }
    const view = new OpenView(callback);
    for (const waveletId of knownWavelets.keys()) {
      view.announced.add(waveletId.serialise());
    }
    this.views.add(view);
    if (this.transport.isConnected()) {
      this.startStream(view, waveletFilter, knownWavelets);
      return;
    }
    view.waiting = this.transport.onConnectionChange((connected) => {
      if (connected && view.waiting !== null) {
        view.waiting();
        view.waiting = null;
        this.startStream(view, waveletFilter, knownWavelets);
      }
    });
  }

  private startStream(
    view: OpenView,
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
  ): void {
    // The box protocol has no explicit wavelet ids in the filter: they go
    // into the prefix list (issue 161 of the original).
    const prefixes = [
      ...waveletFilter.prefixes,
      ...waveletFilter.ids.map((id) => id.id),
    ];
    const known = knownWavelets
      .entries()
      .filter(([, versions]) => versions.length > 0)
      .map(([waveletId, versions]) => ({
        waveletId: ModernIdSerialiser.INSTANCE.serialiseWaveletId(waveletId),
        hashedVersion: serializeHashedVersion(versions[versions.length - 1]!),
      }));
    try {
      view.stream = this.transport.openView(
        {
          participantId: "",
          waveId: ModernIdSerialiser.INSTANCE.serialiseWaveId(this.waveId),
          waveletIdPrefix: prefixes,
          knownWavelet: known,
        },
        {
          onUpdate: (update) => this.onUpdate(view, update),
          onFinished: (error) => this.onFinished(view, error),
          onDisconnected: (reason) => this.onDisconnected(view, reason),
        },
      );
    } catch (error) {
      // Lost the connection between the check and the call.
      this.onDisconnected(
        view,
        error instanceof Error ? error.message : String(error),
      );
      return;
    }
    if (view.cancelRequested) {
      view.stream.cancel();
    }
  }

  private endView(view: OpenView, cancel: boolean): void {
    view.finished = true;
    if (view.waiting !== null) {
      view.waiting();
      view.waiting = null;
    }
    if (cancel && view.stream !== null) {
      view.stream.cancel();
    }
    this.views.delete(view);
  }

  private onFinished(view: OpenView, error: string | undefined): void {
    if (view.finished) {
      return;
    }
    this.endView(view, false);
    if (error === undefined) {
      view.callback.onSuccess(null);
    } else if (isRetryableOpenError(error)) {
      view.callback.onFailure(error);
    } else {
      view.callback.onSuccess(error);
    }
  }

  private onDisconnected(view: OpenView, reason: string): void {
    if (view.finished) {
      return;
    }
    this.endView(view, false);
    view.callback.onFailure(reason);
  }

  private onUpdate(view: OpenView, message: ProtocolWaveletUpdate): void {
    if (view.finished) {
      return;
    }
    let updates: Update[];
    try {
      updates = this.translate(view, message);
    } catch (error) {
      view.callback.onException(
        new ChannelException(
          ResponseCode.INTERNAL_ERROR,
          `Invalid wavelet update: ${error instanceof Error ? error.message : String(error)}`,
          error,
          Recoverable.NOT_RECOVERABLE,
          this.waveId,
          null,
        ),
      );
      return;
    }
    for (const update of updates) {
      if (view.finished) {
        return;
      }
      view.callback.onUpdate(update);
    }
    if (message.marker === true && !view.finished) {
      this.options.onMarker?.();
    }
  }

  /** Turns one wire update into the messages the view channel expects. */
  private translate(view: OpenView, message: ProtocolWaveletUpdate): Update[] {
    const out: Update[] = [];
    const name = ModernIdSerialiser.INSTANCE.deserialiseWaveletName(
      message.waveletName,
    );
    if (!name.waveId.equals(this.waveId)) {
      throw new Error(
        `update for wave ${name.waveId} on the stream of ${this.waveId}`,
      );
    }
    const waveletId = name.waveletId;

    if (message.channelId !== undefined && view.channelId === null) {
      view.channelId = message.channelId;
      const first = new Update();
      first.channelId = message.channelId;
      out.push(first);
    }

    const resulting =
      message.resultingVersion !== undefined
        ? deserializeHashedVersion(message.resultingVersion)
        : null;
    const committed =
      message.commitNotice !== undefined
        ? deserializeHashedVersion(message.commitNotice)
        : null;
    if (message.resetVersion !== undefined) {
      // The history was rewound: the deltas are those taken back, ending
      // at the version last reported.
      this.flushResyncs(view, null, null, out);
      const fork = deserializeHashedVersion(message.resetVersion);
      const deltas = message.appliedDelta;
      const last = deltas[deltas.length - 1];
      let end = this.versions.current(name);
      if (last !== undefined) {
        const lastAt = deserializeHashedVersion(last.hashedVersion).version;
        const expected = lastAt + last.operation.length;
        if (end === null || end.version !== expected) {
          end = HashedVersion.unsigned(expected);
        }
      }
      const update = new Update();
      update.waveletId = waveletId;
      update.resetVersion = fork;
      update.currentVersion = fork;
      update.lastCommittedVersion = committed;
      update.deltas =
        last !== undefined && end !== null
          ? deserializeDeltas(deltas, end, (this.options.clock ?? Date.now)())
          : [];
      this.versions.rewind(name, fork);
      out.push(update);
      return out;
    }
    if (resulting !== null) {
      this.versions.update(name, resulting);
    }
    const deltas = message.appliedDelta;

    if (message.snapshot !== undefined) {
      this.flushResyncs(view, null, null, out);
      const snapshot = deserializeWavelet(message.snapshot, this.waveId);
      const ownVersion = snapshot.hashedVersion;
      view.announced.add(waveletId.serialise());
      if (resulting !== null) {
        snapshot.setVersion(resulting.version);
        snapshot.setHashedVersion(resulting);
      }
      const update = new Update();
      update.waveletId = waveletId;
      update.snapshot = snapshot;
      update.lastCommittedVersion = committed ?? ownVersion;
      update.currentVersion = resulting;
      out.push(update);
    } else if (deltas.length > 0) {
      if (resulting === null) {
        throw new Error("deltas without a resulting version");
      }
      // The deltas after an acknowledged known version: now the current
      // version is known.
      this.flushResyncs(view, waveletId, resulting, out);
      const parsed = deserializeDeltas(
        deltas,
        resulting,
        (this.options.clock ?? Date.now)(),
      );
      const key = waveletId.serialise();
      const update = new Update();
      update.waveletId = waveletId;
      update.currentVersion = resulting;
      if (
        !view.announced.has(key) &&
        !this.submitted.has(key) &&
        parsed[0]!.appliedAtVersion === 0
      ) {
        // A new wavelet comes into view with its whole history.
        update.snapshot = buildWaveletFromDeltas(name, parsed);
        update.lastCommittedVersion =
          committed ?? HashedVersionFactory.createVersionZero(name);
      } else {
        update.deltas = parsed;
        update.lastCommittedVersion = committed;
      }
      view.announced.add(key);
      out.push(update);
    } else if (resulting !== null && !view.markerSeen) {
      // A known version acknowledged.
      this.flushResyncs(view, null, null, out);
      view.announced.add(waveletId.serialise());
      view.pendingResync.push({ waveletId, version: resulting, committed });
    } else if (committed !== null) {
      this.flushResyncs(view, null, null, out);
      const update = new Update();
      update.waveletId = waveletId;
      update.lastCommittedVersion = committed;
      out.push(update);
    }

    if (message.marker === true) {
      this.flushResyncs(view, null, null, out);
      view.markerSeen = true;
      const update = new Update();
      update.marker = true;
      out.push(update);
    }
    return out;
  }

  /**
   * Releases the held-back acknowledgements of known versions. The one for
   * `waveletId`, if any, is followed by deltas up to `current`; the others
   * are followed by nothing, so their known version is the current one.
   */
  private flushResyncs(
    view: OpenView,
    waveletId: WaveletId | null,
    current: HashedVersion | null,
    out: Update[],
  ): void {
    for (const pending of view.pendingResync) {
      const update = new Update();
      update.waveletId = pending.waveletId;
      update.deltas = [
        new TransformedWaveletDelta(NO_AUTHOR, pending.version, 0, []),
      ];
      update.lastCommittedVersion = pending.committed ?? pending.version;
      update.currentVersion =
        waveletId !== null &&
        current !== null &&
        pending.waveletId.equals(waveletId)
          ? current
          : pending.version;
      out.push(update);
    }
    view.pendingResync = [];
  }

  viewSubmit(
    wavelet: WaveletName,
    delta: WaveletDelta,
    channelId: string,
    callback: WaveViewServiceSubmitCallback,
  ): string | null {
    this.submitted.add(wavelet.waveletId.serialise());
    let request;
    try {
      const target = this.versions.signed(wavelet, delta.targetVersion);
      if (target === null) {
        throw new Error(
          `no server signature for version ${delta.targetVersion.version} of ${wavelet}`,
        );
      }
      request = {
        waveletName: ModernIdSerialiser.INSTANCE.serialiseWaveletName(wavelet),
        delta: {
          ...serializeDelta(delta),
          hashedVersion: serializeHashedVersion(target),
        },
        channelId,
      };
    } catch (error) {
      // Not submittable: a defect, not a network problem.
      callback.onSuccess(
        HashedVersion.unsigned(0),
        0,
        `Delta cannot be submitted: ${error instanceof Error ? error.message : String(error)}`,
        ResponseCode.INVALID_OPERATION,
      );
      return null;
    }
    let awaiting = false;
    const settled = (): void => {
      if (awaiting) {
        awaiting = false;
        if (this.awaitingHost > 0) {
          this.awaitingHost--;
          this.options.onAwaitingHost?.(-1);
        }
      }
    };
    this.transport.submit(
      request,
      (response) => {
        settled();
        let version = HashedVersion.unsigned(0);
        if (response.hashedVersionAfterApplication !== undefined) {
          version = deserializeHashedVersion(
            response.hashedVersionAfterApplication,
          );
          this.versions.update(wavelet, version);
        }
        if (response.errorMessage !== undefined) {
          callback.onSuccess(
            version,
            response.operationsApplied,
            response.errorMessage,
            ResponseCode.INTERNAL_ERROR,
          );
        } else {
          callback.onSuccess(
            version,
            response.operationsApplied,
            null,
            ResponseCode.OK,
          );
        }
      },
      (reason) => {
        settled();
        callback.onFailure(reason);
      },
      () => {
        // The delta stays unacknowledged, without an error, until the
        // host has it.
        if (!awaiting && !this.disposed) {
          awaiting = true;
          this.awaitingHost++;
          this.options.onAwaitingHost?.(1);
        }
      },
    );
    return null;
  }

  viewClose(waveId: WaveId, channelId: string, callback: CloseCallback): void {
    if (!waveId.equals(this.waveId)) {
      callback.onFailure(
        `view service of ${this.waveId} asked to close ${waveId}`,
      );
      return;
    }
    for (const view of [...this.views]) {
      if (view.channelId === channelId) {
        view.cancelRequested = true;
        this.endView(view, true);
      }
    }
    callback.onSuccess();
  }

  debugGetProfilingInfo(): string {
    return `[RemoteWaveViewService ${this.waveId}, ${this.views.size} open streams]`;
  }
}

/**
 * The deltas of an update. Each delta message carries only the version it
 * applied at; a delta ends where the next begins and the last ends at the
 * update's resulting version, the only one with a history hash.
 */
function deserializeDeltas(
  deltas: readonly ProtocolWaveletDelta[],
  end: HashedVersion,
  timestamp: number,
): TransformedWaveletDelta[] {
  return deltas.map((delta, i) => {
    const next = deltas[i + 1];
    const resulting =
      next !== undefined ? deserializeHashedVersion(next.hashedVersion) : end;
    return deserializeTransformedDelta(delta, resulting, timestamp);
  });
}
