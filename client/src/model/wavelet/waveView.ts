// The wavelets of one wave as seen by one user (`WaveViewImpl`).
//
// TypeScript port of the Rust crate `resonator-model` (module `wavelet`), itself
// derived from `org.waveprotocol.wave.model.wave.opbased.WaveViewImpl` and
// `wave.ObservableWaveView` of Apache Wave, licensed under the Apache
// License, Version 2.0.

import { WaveletData } from "../data";
import {
  CONVERSATION_ROOT_WAVELET,
  type IdGenerator,
  InvalidIdError,
  type ParticipantId,
  type WaveId,
  WaveletId,
  WaveletName,
  util as idUtil,
} from "../id";
import type { WaveletDelta } from "../operation";
import { HashedVersionFactory } from "../version";
import { Emitter, type Observable } from "../../util/events";
import { WaveletError } from "./error";
import { OpBasedWavelet, type OpBasedWaveletOptions } from "./opBasedWavelet";

export type WaveViewEvents = {
  /** A wavelet was added to the view (created here or added from data). */
  waveletAdded: { waveletId: WaveletId; wavelet: OpBasedWavelet };
  /**
   * A wavelet left the view: removed, or replaced by another object for the
   * same id (then `waveletAdded` follows).
   */
  waveletRemoved: { waveletId: WaveletId; wavelet: OpBasedWavelet };
};

export interface WaveViewOptions {
  /** Timestamp source for the wavelets' operations. Default `Date.now`. */
  clock?: () => number;
  /** A fixed timestamp that overrides the clock (the Rust `timestamp`). */
  timestamp?: number | null;
  /**
   * Options for each wavelet the view wraps or creates, e.g. its `output`
   * sink. They override the view's `clock`, `timestamp`, `onListenerError`
   * and `bumpVersions`, and are overridden by options given to
   * `addWavelet`/`createWavelet` directly.
   */
  waveletOptions?: (data: WaveletData) => OpBasedWaveletOptions;
  /** As for {@link OpBasedWaveletOptions.onListenerError}; also for the view's own events. */
  onListenerError?: (error: unknown) => void;
  /** Test only, see {@link OpBasedWaveletOptions.bumpVersions}. */
  bumpVersions?: boolean;
}

/** The modern serialisation, also for legacy ids it would reject. */
function keyOf(id: WaveletId): string {
  return `${id.domain}/${id.id}`;
}

function invalidId<T>(f: () => T): T {
  try {
    return f();
  } catch (e) {
    if (e instanceof InvalidIdError) {
      throw new WaveletError({ kind: "invalidId", message: e.message });
    }
    throw e;
  }
}

/**
 * The wavelets of one wave as seen by one user, each wrapped for mutation
 * by that user.
 *
 * A client adds wavelets as their snapshots arrive ({@link addWavelet}),
 * feeds each the server's operations (`OpBasedWavelet.consumeRemote`) and
 * collects local operations from each wavelet's `output` sink, given per
 * wavelet through `waveletOptions` or the `options` argument.
 *
 * New wavelets get their creator as first participant, as with the
 * original's default `WaveletConfigurator.ADD_CREATOR`.
 */
export class WaveView {
  readonly events: Observable<WaveViewEvents>;

  private readonly id: WaveId;
  private readonly viewerId: ParticipantId;
  private readonly options: WaveViewOptions;
  private fixedTimestamp: number | null;
  private readonly emitter: Emitter<WaveViewEvents>;
  /** Keyed by the serialised wavelet id (`domain/id`). */
  private readonly map = new Map<string, OpBasedWavelet>();

  constructor(waveId: WaveId, viewer: ParticipantId, options: WaveViewOptions = {}) {
    this.id = waveId;
    this.viewerId = viewer;
    this.options = options;
    this.fixedTimestamp = options.timestamp ?? null;
    this.emitter = new Emitter<WaveViewEvents>(
      options.onListenerError ??
        ((error) =>
          queueMicrotask(() => {
            throw error;
          })),
    );
    this.events = this.emitter;
  }

  waveId(): WaveId {
    return this.id;
  }

  viewer(): ParticipantId {
    return this.viewerId;
  }

  /** The time given to operations made now: the fixed one, else the clock's. */
  timestamp(): number {
    return this.fixedTimestamp ?? (this.options.clock ?? Date.now)();
  }

  /**
   * Fixes the time of the operations to come, for all wavelets present and
   * future; `null` returns them to their clocks.
   */
  setTimestamp(timestamp: number | null): void {
    this.fixedTimestamp = timestamp;
    for (const wavelet of this.map.values()) {
      wavelet.setTimestamp(timestamp);
    }
  }

  private wrap(data: WaveletData, options: OpBasedWaveletOptions | undefined): OpBasedWavelet {
    const base: OpBasedWaveletOptions = { timestamp: this.fixedTimestamp };
    if (this.options.clock !== undefined) {
      base.clock = this.options.clock;
    }
    if (this.options.onListenerError !== undefined) {
      base.onListenerError = this.options.onListenerError;
    }
    if (this.options.bumpVersions !== undefined) {
      base.bumpVersions = this.options.bumpVersions;
    }
    return new OpBasedWavelet(data, this.viewerId, {
      ...base,
      ...this.options.waveletOptions?.(data),
      ...options,
    });
  }

  /**
   * Adds an existing wavelet of the wave, replacing any with the same id
   * (emitting `waveletRemoved` for that one first). Given wavelet data, it
   * is wrapped (not copied) for the viewer with the view's options, the
   * `waveletOptions` of the view and `options`, later ones winning; a
   * ready-made {@link OpBasedWavelet} is taken as it is.
   */
  addWavelet(
    wavelet: WaveletData | OpBasedWavelet,
    options?: OpBasedWaveletOptions,
  ): OpBasedWavelet {
    const added = wavelet instanceof OpBasedWavelet ? wavelet : this.wrap(wavelet, options);
    const waveletId = added.waveletId();
    const key = keyOf(waveletId);
    const previous = this.map.get(key);
    if (previous === added) {
      return added;
    }
    this.map.set(key, added);
    if (previous !== undefined) {
      this.emitter.emit("waveletRemoved", { waveletId, wavelet: previous });
    }
    this.emitter.emit("waveletAdded", { waveletId, wavelet: added });
    return added;
  }

  /**
   * Replaces a wavelet by a new snapshot of it, as after a failed resync.
   * The same as {@link addWavelet}: operations recorded in the old object
   * are dropped with it, and listeners of the old object's events must
   * move to the new one (`waveletRemoved`, then `waveletAdded`).
   */
  replaceWavelet(
    wavelet: WaveletData | OpBasedWavelet,
    options?: OpBasedWaveletOptions,
  ): OpBasedWavelet {
    return this.addWavelet(wavelet, options);
  }

  /** Removes a wavelet from the view. Returns it, or `null` if not there. */
  removeWavelet(waveletId: WaveletId): OpBasedWavelet | null {
    const key = keyOf(waveletId);
    const wavelet = this.map.get(key);
    if (wavelet === undefined) {
      return null;
    }
    this.map.delete(key);
    this.emitter.emit("waveletRemoved", { waveletId: wavelet.waveletId(), wavelet });
    return wavelet;
  }

  wavelet(waveletId: WaveletId): OpBasedWavelet | null {
    return this.map.get(keyOf(waveletId)) ?? null;
  }

  hasWavelet(waveletId: WaveletId): boolean {
    return this.map.has(keyOf(waveletId));
  }

  /** The wavelets ordered by id. */
  wavelets(): OpBasedWavelet[] {
    return [...this.map.values()].sort((a, b) => a.waveletId().compareTo(b.waveletId()));
  }

  /** The wavelet ids in order. */
  waveletIds(): WaveletId[] {
    return this.wavelets().map((wavelet) => wavelet.waveletId());
  }

  /** The id of the conversation root wavelet: `conv+root` in the wave's domain. */
  rootId(): WaveletId {
    return invalidId(() => WaveletId.of(this.id.domain, CONVERSATION_ROOT_WAVELET));
  }

  root(): OpBasedWavelet | null {
    return this.wavelet(this.rootId());
  }

  /**
   * The id under which the viewer's user-data wavelet is created:
   * `user+<address>` in the viewer's domain
   * (`IdUtil.buildUserDataWaveletId`).
   */
  userDataId(): WaveletId {
    return invalidId(() => idUtil.buildUserDataWaveletId(this.viewerId));
  }

  /**
   * The viewer's user-data wavelet: the first wavelet whose id is
   * `user+<address>`, whatever its domain.
   */
  userData(): OpBasedWavelet | null {
    const address = this.viewerId.address;
    return (
      this.wavelets().find((wavelet) => idUtil.isUserDataWaveletOf(address, wavelet.waveletId())) ??
      null
    );
  }

  /**
   * Creates an empty wavelet at version zero, created by the viewer, with
   * the viewer as participant (`createWavelet(WaveletId)`). `waveletAdded`
   * is emitted for the still empty wavelet; the operation adding the viewer
   * follows and reaches the wavelet's `output` like any other. Throws
   * {@link WaveletError} `"duplicateWavelet"`.
   */
  createWavelet(waveletId: WaveletId, options?: OpBasedWaveletOptions): OpBasedWavelet {
    if (this.hasWavelet(waveletId)) {
      throw new WaveletError({ kind: "duplicateWavelet", waveletId });
    }
    const name = WaveletName.of(this.id, waveletId);
    const data = WaveletData.empty(
      name,
      this.viewerId,
      HashedVersionFactory.createVersionZero(name),
      this.timestamp(),
    );
    const wavelet = this.addWavelet(data, options);
    wavelet.addParticipant(this.viewerId);
    return wavelet;
  }

  /** Creates the conversation root wavelet (`createRoot`). */
  createRoot(options?: OpBasedWaveletOptions): OpBasedWavelet {
    return this.createWavelet(this.rootId(), options);
  }

  /** Creates a conversation wavelet with a new id (`createWavelet()`). */
  createConversationWavelet(
    idGenerator: IdGenerator,
    options?: OpBasedWaveletOptions,
  ): OpBasedWavelet {
    return this.createWavelet(
      invalidId(() => idGenerator.newConversationWaveletId()),
      options,
    );
  }

  /** Creates the viewer's user-data wavelet (`createUserData`). */
  createUserData(options?: OpBasedWaveletOptions): OpBasedWavelet {
    return this.createWavelet(this.userDataId(), options);
  }

  /**
   * Takes the recorded operations of every wavelet that has any, as one
   * delta per wavelet, ordered by wavelet id.
   */
  takeDeltas(): [name: WaveletName, delta: WaveletDelta][] {
    const deltas: [WaveletName, WaveletDelta][] = [];
    for (const wavelet of this.wavelets()) {
      const delta = wavelet.takeDelta();
      if (delta !== null) {
        deltas.push([wavelet.data().waveletName, delta]);
      }
    }
    return deltas;
  }
}
