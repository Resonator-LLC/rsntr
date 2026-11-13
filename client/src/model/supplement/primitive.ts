// Supplement state held in memory (`PrimitiveSupplementImpl`).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`PrimitiveSupplementImpl`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { WaveletId } from "../id";
import { HashedVersion } from "../version";
import {
  NO_VERSION,
  SupplementError,
  WaveletMap,
  sortedMap,
  sortedStrings,
  type PrimitiveSupplement,
  type ReadablePrimitiveSupplement,
  type ThreadState,
} from "./types";
import type { WantedEvaluation } from "./wanted";

interface WaveletReadState {
  blipVersions: Map<string, number>;
  waveletVersion: number | null;
  participantsVersion: number | null;
  tagsVersion: number | null;
}

const emptyReadState = (): WaveletReadState => ({
  blipVersions: new Map(),
  waveletVersion: null,
  participantsVersion: null,
  tagsVersion: null,
});

/**
 * Supplement state held in memory: a stand-in where a user has no
 * user-data wavelet, and a cheap snapshot of one that does
 * ({@link PrimitiveSupplementImpl.copyOf}).
 *
 * Quirks of the original that are kept:
 *
 * * The last-read version of a whole wavelet, once set, is only replaced
 *   when it is less than the last-read *participants* version of the
 *   wavelet (the original compares the wrong field). If there is no
 *   participants version at that point the original throws a
 *   `NullPointerException`; here {@link SupplementError} `illegalState` is
 *   thrown.
 * * Archive versions are simply overwritten, not kept monotonic, and
 *   clearing the archive state forgets them, unlike the wavelet-based
 *   supplement.
 * * Clearing the pending notification is all that can be done to it: it
 *   can only be set by copying.
 *
 * Deviations: reading a thread state or gadget state does not create an
 * empty entry for the wavelet or gadget, and evaluations are kept in the
 * order added rather than in a hash set.
 */
export class PrimitiveSupplementImpl implements PrimitiveSupplement {
  private readonly readStates = new WaveletMap<WaveletReadState>();
  private readonly threadStates = new WaveletMap<Map<string, ThreadState>>();
  private readonly folderSet = new Set<number>();
  private readonly archiveVersions = new WaveletMap<number>();
  private readonly seenVersions = new WaveletMap<HashedVersion>();
  private readonly evaluations: WantedEvaluation[] = [];
  private followState: boolean | null = null;
  private pending = false;
  private readonly notifiedVersions = new WaveletMap<number>();
  private readonly gadgetStates = new Map<string, Map<string, string>>();

  /**
   * A copy of the state of another supplement
   * (`PrimitiveSupplementImpl(PrimitiveSupplement)`). As in the original,
   * gadget states are not copied.
   */
  static copyOf(other: ReadablePrimitiveSupplement): PrimitiveSupplementImpl {
    const copy = new PrimitiveSupplementImpl();
    // Set directly: on a fresh state the original's setters do just that.
    const version = (v: number): number | null => (v === NO_VERSION ? null : v);
    for (const wavelet of other.readWavelets()) {
      const state = copy.readStates.getOrInsert(wavelet, emptyReadState);
      state.waveletVersion = version(other.lastReadWaveletVersion(wavelet));
      state.participantsVersion = version(other.lastReadParticipantsVersion(wavelet));
      state.tagsVersion = version(other.lastReadTagsVersion(wavelet));
      for (const blip of other.readBlips(wavelet)) {
        state.blipVersions.set(blip, other.lastReadBlipVersion(wavelet, blip));
      }
    }
    for (const wavelet of other.waveletsWithThreadState()) {
      const threads = copy.threadStates.getOrInsert(wavelet, () => new Map());
      for (const thread of other.statefulThreads(wavelet)) {
        const state = other.threadState(wavelet, thread);
        if (state !== null) {
          threads.set(thread, state);
        }
      }
    }
    for (const wavelet of other.archiveWavelets()) {
      copy.archiveVersions.set(wavelet, other.archiveWaveletVersion(wavelet));
    }
    for (const folder of other.folders()) {
      copy.folderSet.add(folder);
    }
    for (const evaluation of other.wantedEvaluations()) {
      copy.addWantedEvaluation(evaluation);
    }
    for (const wavelet of other.seenWavelets()) {
      copy.seenVersions.set(wavelet, other.seenVersion(wavelet));
    }
    for (const wavelet of other.notifiedWavelets()) {
      copy.notifiedVersions.set(wavelet, other.notifiedVersion(wavelet));
    }
    copy.followState = other.followed();
    copy.pending = other.pendingNotification();
    return copy;
  }

  threadState(waveletId: WaveletId, threadId: string): ThreadState | null {
    return this.threadStates.get(waveletId)?.get(threadId) ?? null;
  }

  waveletsWithThreadState(): WaveletId[] {
    return this.threadStates.keys();
  }

  statefulThreads(waveletId: WaveletId): string[] {
    return sortedStrings(this.threadStates.get(waveletId)?.keys() ?? []);
  }

  lastReadWaveletVersion(waveletId: WaveletId): number {
    return this.readStates.get(waveletId)?.waveletVersion ?? NO_VERSION;
  }

  lastReadBlipVersion(waveletId: WaveletId, blipId: string): number {
    return this.readStates.get(waveletId)?.blipVersions.get(blipId) ?? NO_VERSION;
  }

  lastReadParticipantsVersion(waveletId: WaveletId): number {
    return this.readStates.get(waveletId)?.participantsVersion ?? NO_VERSION;
  }

  lastReadTagsVersion(waveletId: WaveletId): number {
    return this.readStates.get(waveletId)?.tagsVersion ?? NO_VERSION;
  }

  readWavelets(): WaveletId[] {
    return this.readStates.keys();
  }

  readBlips(waveletId: WaveletId): string[] {
    return sortedStrings(this.readStates.get(waveletId)?.blipVersions.keys() ?? []);
  }

  folders(): number[] {
    return [...this.folderSet].sort((a, b) => a - b);
  }

  isInFolder(id: number): boolean {
    return this.folderSet.has(id);
  }

  archiveWaveletVersion(waveletId: WaveletId): number {
    return this.archiveVersions.get(waveletId) ?? NO_VERSION;
  }

  archiveWavelets(): WaveletId[] {
    return this.archiveVersions.keys();
  }

  followed(): boolean | null {
    return this.followState;
  }

  seenVersion(waveletId: WaveletId): HashedVersion {
    return this.seenVersions.get(waveletId) ?? HashedVersion.unsigned(0);
  }

  seenWavelets(): WaveletId[] {
    return this.seenVersions.keys();
  }

  wantedEvaluations(): WantedEvaluation[] {
    return [...this.evaluations];
  }

  pendingNotification(): boolean {
    return this.pending;
  }

  notifiedWavelets(): WaveletId[] {
    return this.notifiedVersions.keys();
  }

  notifiedVersion(waveletId: WaveletId): number {
    return this.notifiedVersions.get(waveletId) ?? NO_VERSION;
  }

  gadgetState(gadgetId: string): Map<string, string> {
    return sortedMap(this.gadgetStates.get(gadgetId) ?? []);
  }

  setThreadState(waveletId: WaveletId, threadId: string, state: ThreadState | null): void {
    const threads = this.threadStates.getOrInsert(waveletId, () => new Map());
    if (state === null) {
      threads.delete(threadId);
    } else {
      threads.set(threadId, state);
    }
  }

  setLastReadWaveletVersion(waveletId: WaveletId, version: number): void {
    const state = this.readStates.getOrInsert(waveletId, emptyReadState);
    if (state.waveletVersion === null) {
      state.waveletVersion = version;
    } else if (state.participantsVersion === null) {
      throw SupplementError.illegalState(
        "the wavelet has a last-read version but no last-read participants version " +
          "to compare it with",
      );
    } else if (state.waveletVersion < state.participantsVersion) {
      state.waveletVersion = version;
    }
  }

  setLastReadBlipVersion(waveletId: WaveletId, blipId: string, version: number): void {
    const state = this.readStates.getOrInsert(waveletId, emptyReadState);
    const current = state.blipVersions.get(blipId);
    if (current === undefined || current < version) {
      state.blipVersions.set(blipId, version);
    }
  }

  setLastReadParticipantsVersion(waveletId: WaveletId, version: number): void {
    const state = this.readStates.getOrInsert(waveletId, emptyReadState);
    if (state.participantsVersion === null || state.participantsVersion < version) {
      state.participantsVersion = version;
    }
  }

  setLastReadTagsVersion(waveletId: WaveletId, version: number): void {
    const state = this.readStates.getOrInsert(waveletId, emptyReadState);
    if (state.tagsVersion === null || state.tagsVersion < version) {
      state.tagsVersion = version;
    }
  }

  clearReadState(): void {
    this.readStates.clear();
  }

  clearBlipReadState(waveletId: WaveletId, blipId: string): void {
    this.readStates.getOrInsert(waveletId, emptyReadState).blipVersions.delete(blipId);
  }

  addFolder(id: number): void {
    this.folderSet.add(id);
  }

  removeFolder(id: number): void {
    this.folderSet.delete(id);
  }

  removeAllFolders(): void {
    this.folderSet.clear();
  }

  follow(): void {
    this.followState = true;
  }

  unfollow(): void {
    this.followState = false;
  }

  clearFollow(): void {
    this.followState = null;
  }

  archiveAtVersion(waveletId: WaveletId, version: number): void {
    this.archiveVersions.set(waveletId, version);
  }

  clearArchiveState(): void {
    this.archiveVersions.clear();
  }

  setSeenVersion(waveletId: WaveletId, version: HashedVersion): void {
    this.seenVersions.set(waveletId, version);
  }

  clearSeenVersion(waveletId: WaveletId): void {
    this.seenVersions.delete(waveletId);
  }

  addWantedEvaluation(evaluation: WantedEvaluation): void {
    if (!this.evaluations.some((known) => known.equals(evaluation))) {
      this.evaluations.push(evaluation);
    }
  }

  setNotifiedVersion(waveletId: WaveletId, version: number): void {
    this.notifiedVersions.set(waveletId, version);
  }

  clearPendingNotification(): void {
    this.pending = false;
  }

  setGadgetState(gadgetId: string, key: string, value: string | null): void {
    let state = this.gadgetStates.get(gadgetId);
    if (state === undefined) {
      state = new Map();
      this.gadgetStates.set(gadgetId, state);
    }
    if (value === null) {
      state.delete(key);
    } else {
      state.set(key, value);
    }
  }
}
