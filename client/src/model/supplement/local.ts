// The optimistic supplement of the client: blips the viewer is reading, or
// has marked as read, stay read while the acknowledgements of the viewer's
// own operations come in.
//
// Port of `org.waveprotocol.wave.client.wave.LocalSupplementedWaveImpl` and
// `LocalSupplementedWave` of Apache Wave, licensed under the Apache License,
// Version 2.0, with ids in place of objects.
//
// Read state is based on versions, and versions only move when the server
// acknowledges local operations. Marking a blip as read while the viewer
// types in it therefore does not keep it read: the acknowledgement raises
// the blip's last-modified version above the version it was read at. The
// original solves that with two rules, kept here:
//
// * the blip being read (the focused one: `startReading`/`stopReading`) and
//   every blip marked as read are put into an "auto-read" collection, whose
//   members are reported as read whatever the versions say;
// * every `REPEAT_MS` each member is marked as read again in the underlying
//   supplement, which records the versions the acknowledgements produced;
// * a member leaves the collection when a *remote* operation changes its
//   content (`WaveletListener.onRemoteBlipContentModified`: from then on the
//   versions tell the truth, and the blip becomes unread), or `EVICT_TIME_MS`
//   after it was last marked or read, by when the acknowledgements are
//   assumed to have arrived. The blip being read never leaves.

import type { Unsubscribe } from "../../util/events";
import { util as idUtil, type WaveletId } from "../id";
import { isBlipIdWorthy, isWorthy } from "../operation";
import type { OpBasedWavelet, WaveView } from "../wavelet";
import { LiveSupplementedWave, type LiveSupplementedWaveOptions } from "./live";
import { waveletKey } from "./types";

/** How often auto-read blips are marked as read (`REPEAT_MS`). */
export const REPEAT_MS = 10 * 1000;

/**
 * Auto-reading stops for blips that were not marked or read for this long
 * (`EVICT_TIME_MS`).
 */
export const EVICT_TIME_MS = 60 * 1000;

/** `setTimeout`/`clearTimeout`, injectable for tests. */
export interface LocalTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

const DEFAULT_TIMERS: LocalTimers = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/** The part of a supplement {@link LocalReadState} decorates. */
export interface ReadStateDelegate {
  isUnread(waveletId: WaveletId, blipId: string): boolean;
  markBlipAsRead(waveletId: WaveletId, blipId: string): void;
  markAsUnread(): void;
}

export interface LocalReadStateOptions {
  /** Default: the global timers. */
  timers?: LocalTimers;
  /** Milliseconds; default `Date.now`. */
  clock?: () => number;
}

interface AutoRead {
  readonly waveletId: WaveletId;
  readonly blipId: string;
  /** When the blip was last marked as read here. */
  touched: number;
}

function blipKey(waveletId: WaveletId, blipId: string): string {
  return `${waveletKey(waveletId)} ${blipId}`;
}

/**
 * The logic of `LocalSupplementedWaveImpl` over the three calls of the
 * supplement it overrides. {@link LocalSupplementedWave} binds it to a wave.
 *
 * Unlike the original, whose task repeats from creation on, the timer runs
 * only while there are auto-read blips; the first repetition comes
 * `REPEAT_MS` after the collection became non-empty.
 */
export class LocalReadState {
  private readonly timers: LocalTimers;
  private readonly clock: () => number;
  private readonly autoRead = new Map<string, AutoRead>();
  /** The key of the blip being read, if there is one. */
  private reading: string | null = null;
  private timer: unknown = null;
  private destroyed = false;

  constructor(
    private readonly delegate: ReadStateDelegate,
    options: LocalReadStateOptions = {},
  ) {
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.clock = options.clock ?? Date.now;
  }

  /** Stops the timer (`destroy`). */
  destroy(): void {
    this.destroyed = true;
    this.cancelTimer();
  }

  /** The blip being read, or null. */
  readingBlip(): { waveletId: WaveletId; blipId: string } | null {
    const entry = this.reading === null ? undefined : this.autoRead.get(this.reading);
    return entry === undefined ? null : { waveletId: entry.waveletId, blipId: entry.blipId };
  }

  /**
   * The viewer starts reading a blip: it is marked as read and stays read
   * until some time after {@link stopReading}. Throws if a blip is being
   * read already.
   */
  startReading(waveletId: WaveletId, blipId: string): void {
    if (this.reading !== null) {
      throw new Error("already reading a blip");
    }
    this.reading = blipKey(waveletId, blipId);
    this.startAutoReading(waveletId, blipId);
  }

  /** Throws unless this is the blip being read. */
  stopReading(waveletId: WaveletId, blipId: string): void {
    if (this.reading === null) {
      throw new Error("not reading a blip");
    }
    if (this.reading !== blipKey(waveletId, blipId)) {
      throw new Error("not the blip being read");
    }
    // Continue marking this blip as read while the acks come in. Delay
    // eviction by refreshing its timestamp.
    this.delegate.markBlipAsRead(waveletId, blipId);
    const entry = this.autoRead.get(this.reading);
    if (entry !== undefined) {
      entry.touched = this.clock();
    }
    this.reading = null;
  }

  private startAutoReading(waveletId: WaveletId, blipId: string): void {
    // Mark it as read first, before putting in the auto-read override, in
    // order to generate the correct read events.
    this.delegate.markBlipAsRead(waveletId, blipId);
    this.autoRead.set(blipKey(waveletId, blipId), { waveletId, blipId, touched: this.clock() });
    this.ensureTimer();
  }

  /** All auto-read blips are read; the others are what the supplement says. */
  isUnread(waveletId: WaveletId, blipId: string): boolean {
    return !this.autoRead.has(blipKey(waveletId, blipId)) && this.delegate.isUnread(waveletId, blipId);
  }

  /** Whether a blip is kept read whatever the versions say. */
  isAutoRead(waveletId: WaveletId, blipId: string): boolean {
    return this.autoRead.has(blipKey(waveletId, blipId));
  }

  markBlipAsRead(waveletId: WaveletId, blipId: string): void {
    this.startAutoReading(waveletId, blipId);
  }

  markAsUnread(): void {
    const reading = this.readingBlip();
    this.autoRead.clear();
    this.delegate.markAsUnread();
    if (reading !== null) {
      this.startAutoReading(reading.waveletId, reading.blipId);
    } else {
      this.cancelTimer();
    }
  }

  /**
   * An operation by someone else changed the content of a blip
   * (`onRemoteBlipContentModified`): stop auto-reading it, unless it is the
   * blip being read.
   */
  onRemoteChange(waveletId: WaveletId, blipId: string): void {
    const key = blipKey(waveletId, blipId);
    if (key !== this.reading && this.autoRead.delete(key) && this.autoRead.size === 0) {
      this.cancelTimer();
    }
  }

  /** The repeating task (`execute`). */
  private execute(): void {
    // Mark as read all blips in the auto-read list. Evict any blips that
    // have not been touched for more than some threshold, but never the
    // currently-reading blip.
    const reading = this.readingBlip();
    if (reading !== null) {
      this.delegate.markBlipAsRead(reading.waveletId, reading.blipId);
    }
    const now = this.clock();
    const evict: string[] = [];
    for (const [key, entry] of [...this.autoRead]) {
      this.delegate.markBlipAsRead(entry.waveletId, entry.blipId);
      if (key !== this.reading && now - entry.touched >= EVICT_TIME_MS) {
        evict.push(key);
      }
    }
    for (const key of evict) {
      this.autoRead.delete(key);
    }
  }

  private ensureTimer(): void {
    if (this.timer !== null || this.destroyed) {
      return;
    }
    const handle = this.timers.setTimeout(() => {
      this.timer = null;
      try {
        this.execute();
      } finally {
        if (this.autoRead.size > 0) {
          this.ensureTimer();
        }
      }
    }, REPEAT_MS);
    // Do not keep a Node process alive for the sake of read state.
    (handle as { unref?: () => void } | null)?.unref?.();
    this.timer = handle;
  }

  private cancelTimer(): void {
    if (this.timer !== null) {
      this.timers.clearTimeout(this.timer);
      this.timer = null;
    }
  }
}

export interface LocalSupplementedWaveOptions extends LiveSupplementedWaveOptions, LocalReadStateOptions {
  /**
   * Whether the supplement can be written now. While false, the periodic
   * and implicit markings are skipped (the optimistic read state is kept all
   * the same). Default: always. A client passes "the wave is open": before
   * that the user-data wavelet has not arrived, and after it is closed
   * nothing can be sent.
   */
  writable?: () => boolean;
}

/**
 * The supplement of an open wave with the optimistic read state of the
 * original client (`LocalSupplementedWave`): a {@link LiveSupplementedWave}
 * whose `isUnread`, `unreadBlips`, `unreadCount`, `markBlipAsRead` and
 * `markAsUnread` go through a {@link LocalReadState}, and that tells remote
 * changes of blips from acknowledgements by listening to the wavelets.
 *
 * `state()` remains the supplement of the versions alone.
 *
 * Marking a blip that is read already writes nothing (and does not create
 * the user-data wavelet), so that the repetitions cost nothing.
 *
 * As in the original, the events are those of the underlying supplement:
 * none is emitted when a blip is evicted from the auto-read collection by
 * time.
 */
export class LocalSupplementedWave extends LiveSupplementedWave {
  private readonly local: LocalReadState;
  private readonly writable: () => boolean;
  private readonly detectors = new Map<string, { wavelet: OpBasedWavelet; off: Unsubscribe }>();
  private readonly offView: Unsubscribe[];

  constructor(wave: WaveView, options: LocalSupplementedWaveOptions = {}) {
    super(wave, options);
    this.writable = options.writable ?? (() => true);
    this.local = new LocalReadState(
      {
        isUnread: (waveletId, blipId) => super.isUnread(waveletId, blipId),
        markBlipAsRead: (waveletId, blipId) => {
          if (this.writable() && super.isUnread(waveletId, blipId)) {
            super.markBlipAsRead(waveletId, blipId);
          }
        },
        markAsUnread: () => super.markAsUnread(),
      },
      options,
    );
    for (const wavelet of wave.wavelets()) {
      this.watch(wavelet);
    }
    this.offView = [
      wave.events.on("waveletAdded", ({ wavelet }) => this.watch(wavelet)),
      wave.events.on("waveletRemoved", ({ wavelet }) => this.unwatch(wavelet)),
    ];
  }

  override dispose(): void {
    this.local.destroy();
    this.offView.forEach((off) => off());
    for (const { off } of this.detectors.values()) {
      off();
    }
    this.detectors.clear();
    super.dispose();
  }

  /** Listens for remote changes of the blips of a conversation wavelet. */
  private watch(wavelet: OpBasedWavelet): void {
    const waveletId = wavelet.waveletId();
    let conversational = false;
    try {
      conversational = idUtil.isConversationalId(waveletId);
    } catch {
      // Not an id of a known kind.
    }
    if (!conversational) {
      return;
    }
    this.unwatchKey(waveletKey(waveletId));
    const off = wavelet.events.on("documentChanged", ({ docId, op, local }) => {
      // `BlipContentOperation.doApply`, which only remote operations go
      // through: a change worthy of attribution.
      if (!local && idUtil.isBlipId(docId) && isBlipIdWorthy(docId) && isWorthy(op)) {
        this.local.onRemoteChange(waveletId, docId);
      }
    });
    this.detectors.set(waveletKey(waveletId), { wavelet, off });
  }

  private unwatch(wavelet: OpBasedWavelet): void {
    const key = waveletKey(wavelet.waveletId());
    if (this.detectors.get(key)?.wavelet === wavelet) {
      this.unwatchKey(key);
    }
  }

  private unwatchKey(key: string): void {
    this.detectors.get(key)?.off();
    this.detectors.delete(key);
  }

  // ---- LocalSupplementedWave ----

  /**
   * The viewer starts reading a blip (it got the focus): it is marked as
   * read and stays read, whatever happens to it, until `stopReading` and
   * for a while after. Throws if a blip is being read already.
   */
  startReading(waveletId: WaveletId, blipId: string): void {
    this.local.startReading(waveletId, blipId);
  }

  /** The viewer stops reading the blip being read. Throws if it is not that one. */
  stopReading(waveletId: WaveletId, blipId: string): void {
    this.local.stopReading(waveletId, blipId);
  }

  /** The blip being read, or null. */
  readingBlip(): { waveletId: WaveletId; blipId: string } | null {
    return this.local.readingBlip();
  }

  // ---- overridden queries and actions ----

  override isUnread(waveletId: WaveletId, blipId: string): boolean {
    return this.local.isUnread(waveletId, blipId);
  }

  override unreadBlips(waveletId: WaveletId, blipIds?: Iterable<string>): string[] {
    return super
      .unreadBlips(waveletId, blipIds)
      .filter((blipId) => !this.local.isAutoRead(waveletId, blipId));
  }

  override markBlipAsRead(waveletId: WaveletId, blipId: string): void {
    this.local.markBlipAsRead(waveletId, blipId);
  }

  override markAsUnread(): void {
    this.local.markAsUnread();
  }
}
