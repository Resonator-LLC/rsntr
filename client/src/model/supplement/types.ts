// The raw supplement state as interfaces, and the types they use
// (`ReadablePrimitiveSupplement`, `PrimitiveSupplement`, `ThreadState`,
// `SupplementError` of the Rust `supplement/mod.rs`; a separate module here
// only to keep the module graph free of cycles).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`PrimitiveSupplement`, `ThreadState`) of Apache Wave, licensed under the
// Apache License, Version 2.0.

import { enumSerializer, type Serializer } from "../adt";
import type { WaveletId } from "../id";
import type { HashedVersion } from "../version";
import type { WantedEvaluation } from "./wanted";

/** The version reported where there is none (`PrimitiveSupplement.NO_VERSION`). */
export const NO_VERSION = -1;

/** The reason for a {@link SupplementError}. */
export type SupplementErrorDetail =
  /** A state in which the original fails with a `NullPointerException`. */
  { readonly kind: "illegalState"; readonly message: string };

/**
 * Failure to change supplement state. Failures of the layers below surface
 * as their own errors: `AdtError`, `DocError`, `WaveletError`,
 * `WaveletOperationError` (Rust wraps them in `SupplementError`).
 */
export class SupplementError extends Error {
  readonly detail: SupplementErrorDetail;

  constructor(detail: SupplementErrorDetail) {
    super(detail.message);
    this.name = "SupplementError";
    this.detail = detail;
  }

  get kind(): SupplementErrorDetail["kind"] {
    return this.detail.kind;
  }

  static illegalState(message: string): SupplementError {
    return new SupplementError({ kind: "illegalState", message });
  }
}

/**
 * Whether the user has collapsed or expanded a thread (`ThreadState`). The
 * values are the names of the Java enum constants, which are the stored
 * form.
 */
export const ThreadState = {
  /** The thread has been explicitly collapsed by the user. */
  Collapsed: "COLLAPSED",
  /** The thread has been explicitly expanded by the user. */
  Expanded: "EXPANDED",
} as const;
export type ThreadState = (typeof ThreadState)[keyof typeof ThreadState];

/** `Serializer.EnumSerializer<ThreadState>`. */
export const THREAD_STATE: Serializer<ThreadState> = enumSerializer<ThreadState>([
  ThreadState.Collapsed,
  ThreadState.Expanded,
]);

/**
 * The raw supplement state, for reading (the query half of
 * `PrimitiveSupplement`). Listings are sorted. Versions are 32-bit
 * integers, {@link NO_VERSION} for "none".
 */
export interface ReadablePrimitiveSupplement {
  /** The state of a thread, if the user gave it one (`getThreadState`). */
  threadState(waveletId: WaveletId, threadId: string): ThreadState | null;

  /** `getWaveletsWithThreadState`. */
  waveletsWithThreadState(): WaveletId[];

  /** The threads of a wavelet that have a state (`getStatefulThreads`). */
  statefulThreads(waveletId: WaveletId): string[];

  /**
   * The version at which the whole wavelet was last marked as read
   * (`getLastReadWaveletVersion`).
   */
  lastReadWaveletVersion(waveletId: WaveletId): number;

  /** `getLastReadBlipVersion`. */
  lastReadBlipVersion(waveletId: WaveletId, blipId: string): number;

  /** `getLastReadParticipantsVersion`. */
  lastReadParticipantsVersion(waveletId: WaveletId): number;

  /** `getLastReadTagsVersion`. */
  lastReadTagsVersion(waveletId: WaveletId): number;

  /** The wavelets that have any read state (`getReadWavelets`). */
  readWavelets(): WaveletId[];

  /** The blips of a wavelet that have a read version (`getReadBlips`). */
  readBlips(waveletId: WaveletId): string[];

  /** `getFolders`. */
  folders(): number[];

  /** `isInFolder`. */
  isInFolder(id: number): boolean;

  /**
   * The version up to which a wavelet is archived
   * (`getArchiveWaveletVersion`).
   */
  archiveWaveletVersion(waveletId: WaveletId): number;

  /** `getArchiveWavelets`. */
  archiveWavelets(): WaveletId[];

  /**
   * Whether the user explicitly follows (`true`) or unfollows (`false`)
   * the wave; `null` if neither (`getFollowed`).
   */
  followed(): boolean | null;

  /**
   * The version of a wavelet the user last saw; version zero without hash
   * if none (`getSeenVersion`).
   */
  seenVersion(waveletId: WaveletId): HashedVersion;

  /** `getSeenWavelets`. */
  seenWavelets(): WaveletId[];

  /**
   * All evaluations of whether the wave is wanted, without duplicates
   * (`getWantedEvaluations`).
   */
  wantedEvaluations(): WantedEvaluation[];

  /** `getPendingNotification`. */
  pendingNotification(): boolean;

  /** `getNotifiedWavelets`. */
  notifiedWavelets(): WaveletId[];

  /** `getNotifiedVersion`. */
  notifiedVersion(waveletId: WaveletId): number;

  /**
   * The private state of a gadget, by key, in key order
   * (`getGadgetState`).
   */
  gadgetState(gadgetId: string): Map<string, string>;
}

/**
 * The raw supplement state, for writing (the mutating half of
 * `PrimitiveSupplement`).
 */
export interface PrimitiveSupplement extends ReadablePrimitiveSupplement {
  /** Sets the state of a thread, or removes it for `null` (`setThreadState`). */
  setThreadState(waveletId: WaveletId, threadId: string, state: ThreadState | null): void;

  /** `setLastReadWaveletVersion`. */
  setLastReadWaveletVersion(waveletId: WaveletId, version: number): void;

  /** `setLastReadBlipVersion`. */
  setLastReadBlipVersion(waveletId: WaveletId, blipId: string, version: number): void;

  /** `setLastReadParticipantsVersion`. */
  setLastReadParticipantsVersion(waveletId: WaveletId, version: number): void;

  /** `setLastReadTagsVersion`. */
  setLastReadTagsVersion(waveletId: WaveletId, version: number): void;

  /** Forgets all read state (`clearReadState`). */
  clearReadState(): void;

  /** `clearBlipReadState`. */
  clearBlipReadState(waveletId: WaveletId, blipId: string): void;

  /** `addFolder`. */
  addFolder(id: number): void;

  /** `removeFolder`. */
  removeFolder(id: number): void;

  /** `removeAllFolders`. */
  removeAllFolders(): void;

  /** `follow`. */
  follow(): void;

  /** `unfollow`. */
  unfollow(): void;

  /** Returns to neither following nor unfollowing (`clearFollow`). */
  clearFollow(): void;

  /** `archiveAtVersion`. */
  archiveAtVersion(waveletId: WaveletId, version: number): void;

  /** `clearArchiveState`. */
  clearArchiveState(): void;

  /** `setSeenVersion`. */
  setSeenVersion(waveletId: WaveletId, version: HashedVersion): void;

  /** `clearSeenVersion`. */
  clearSeenVersion(waveletId: WaveletId): void;

  /** `addWantedEvaluation`. */
  addWantedEvaluation(evaluation: WantedEvaluation): void;

  /** `setNotifiedVersion`. */
  setNotifiedVersion(waveletId: WaveletId, version: number): void;

  /** `clearPendingNotification`. */
  clearPendingNotification(): void;

  /**
   * Sets one key of the private state of a gadget, or removes it for
   * `null` (`setGadgetState`).
   */
  setGadgetState(gadgetId: string, key: string, value: string | null): void;
}

/** The key of a wavelet id in a map: its `domain/id` form. */
export function waveletKey(id: WaveletId): string {
  return `${id.domain}/${id.id}`;
}

/** A map keyed by wavelet id that lists its keys in id order. */
export class WaveletMap<V> {
  private readonly map = new Map<string, { id: WaveletId; value: V }>();

  get(id: WaveletId): V | undefined {
    return this.map.get(waveletKey(id))?.value;
  }

  has(id: WaveletId): boolean {
    return this.map.has(waveletKey(id));
  }

  set(id: WaveletId, value: V): void {
    this.map.set(waveletKey(id), { id, value });
  }

  /** The value of a key, created with `make` if absent. */
  getOrInsert(id: WaveletId, make: () => V): V {
    const key = waveletKey(id);
    let entry = this.map.get(key);
    if (entry === undefined) {
      entry = { id, value: make() };
      this.map.set(key, entry);
    }
    return entry.value;
  }

  delete(id: WaveletId): boolean {
    return this.map.delete(waveletKey(id));
  }

  clear(): void {
    this.map.clear();
  }

  get size(): number {
    return this.map.size;
  }

  /** The keys in order of id. */
  keys(): WaveletId[] {
    return [...this.map.values()].map((entry) => entry.id).sort((a, b) => a.compareTo(b));
  }

  /** The entries in order of id. */
  entries(): [WaveletId, V][] {
    return [...this.map.values()]
      .sort((a, b) => a.id.compareTo(b.id))
      .map((entry): [WaveletId, V] => [entry.id, entry.value]);
  }
}

/** A copy of a string-keyed map with its keys in native (UTF-16) order. */
export function sortedMap<V>(entries: Iterable<readonly [string, V]>): Map<string, V> {
  return new Map([...entries].sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)));
}

/** Strings in native (UTF-16) order. */
export function sortedStrings(values: Iterable<string>): string[] {
  return [...values].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}
