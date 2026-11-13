// Keeping presentation state out of the user-data wavelet
// (`PartitioningPrimitiveSupplement`).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`PartitioningPrimitiveSupplement`) of Apache Wave, licensed under the
// Apache License, Version 2.0.

import type { WaveletId } from "../id";
import type { HashedVersion } from "../version";
import { PrimitiveSupplementImpl } from "./primitive";
import type { PrimitiveSupplement, ThreadState } from "./types";
import type { WantedEvaluation } from "./wanted";

/**
 * A supplement that keeps thread states in memory rather than in the real
 * supplement, unless told to use the presentation model, and that reads
 * and writes private gadget states only if told to
 * (`PartitioningPrimitiveSupplement`). Everything else goes to the real
 * supplement.
 */
export class PartitioningPrimitiveSupplement<P extends PrimitiveSupplement>
  implements PrimitiveSupplement
{
  /** Thread states when they are not kept in the real supplement. */
  private presentation: PrimitiveSupplementImpl | null;

  constructor(
    /** The supplement everything else goes to. */
    readonly real: P,
    usePresentationModel: boolean,
    private readonly usePrivateGadgetStates: boolean,
  ) {
    this.presentation = usePresentationModel ? null : new PrimitiveSupplementImpl();
  }

  /** Forgets the thread states kept in memory (`stopPartitioning`). */
  stopPartitioning(): void {
    if (this.presentation !== null) {
      this.presentation = new PrimitiveSupplementImpl();
    }
  }

  private threads(): PrimitiveSupplement {
    return this.presentation ?? this.real;
  }

  threadState(waveletId: WaveletId, threadId: string): ThreadState | null {
    return this.threads().threadState(waveletId, threadId);
  }

  waveletsWithThreadState(): WaveletId[] {
    return this.threads().waveletsWithThreadState();
  }

  statefulThreads(waveletId: WaveletId): string[] {
    return this.threads().statefulThreads(waveletId);
  }

  lastReadWaveletVersion(waveletId: WaveletId): number {
    return this.real.lastReadWaveletVersion(waveletId);
  }

  lastReadBlipVersion(waveletId: WaveletId, blipId: string): number {
    return this.real.lastReadBlipVersion(waveletId, blipId);
  }

  lastReadParticipantsVersion(waveletId: WaveletId): number {
    return this.real.lastReadParticipantsVersion(waveletId);
  }

  lastReadTagsVersion(waveletId: WaveletId): number {
    return this.real.lastReadTagsVersion(waveletId);
  }

  readWavelets(): WaveletId[] {
    return this.real.readWavelets();
  }

  readBlips(waveletId: WaveletId): string[] {
    return this.real.readBlips(waveletId);
  }

  folders(): number[] {
    return this.real.folders();
  }

  isInFolder(id: number): boolean {
    return this.real.isInFolder(id);
  }

  archiveWaveletVersion(waveletId: WaveletId): number {
    return this.real.archiveWaveletVersion(waveletId);
  }

  archiveWavelets(): WaveletId[] {
    return this.real.archiveWavelets();
  }

  followed(): boolean | null {
    return this.real.followed();
  }

  seenVersion(waveletId: WaveletId): HashedVersion {
    return this.real.seenVersion(waveletId);
  }

  seenWavelets(): WaveletId[] {
    return this.real.seenWavelets();
  }

  wantedEvaluations(): WantedEvaluation[] {
    return this.real.wantedEvaluations();
  }

  pendingNotification(): boolean {
    return this.real.pendingNotification();
  }

  notifiedWavelets(): WaveletId[] {
    return this.real.notifiedWavelets();
  }

  notifiedVersion(waveletId: WaveletId): number {
    return this.real.notifiedVersion(waveletId);
  }

  gadgetState(gadgetId: string): Map<string, string> {
    return this.usePrivateGadgetStates ? this.real.gadgetState(gadgetId) : new Map();
  }

  setThreadState(waveletId: WaveletId, threadId: string, state: ThreadState | null): void {
    this.threads().setThreadState(waveletId, threadId, state);
  }

  setLastReadWaveletVersion(waveletId: WaveletId, version: number): void {
    this.real.setLastReadWaveletVersion(waveletId, version);
  }

  setLastReadBlipVersion(waveletId: WaveletId, blipId: string, version: number): void {
    this.real.setLastReadBlipVersion(waveletId, blipId, version);
  }

  setLastReadParticipantsVersion(waveletId: WaveletId, version: number): void {
    this.real.setLastReadParticipantsVersion(waveletId, version);
  }

  setLastReadTagsVersion(waveletId: WaveletId, version: number): void {
    this.real.setLastReadTagsVersion(waveletId, version);
  }

  clearReadState(): void {
    this.real.clearReadState();
  }

  clearBlipReadState(waveletId: WaveletId, blipId: string): void {
    this.real.clearBlipReadState(waveletId, blipId);
  }

  addFolder(id: number): void {
    this.real.addFolder(id);
  }

  removeFolder(id: number): void {
    this.real.removeFolder(id);
  }

  removeAllFolders(): void {
    this.real.removeAllFolders();
  }

  follow(): void {
    this.real.follow();
  }

  unfollow(): void {
    this.real.unfollow();
  }

  clearFollow(): void {
    this.real.clearFollow();
  }

  archiveAtVersion(waveletId: WaveletId, version: number): void {
    this.real.archiveAtVersion(waveletId, version);
  }

  clearArchiveState(): void {
    this.real.clearArchiveState();
  }

  setSeenVersion(waveletId: WaveletId, version: HashedVersion): void {
    this.real.setSeenVersion(waveletId, version);
  }

  clearSeenVersion(waveletId: WaveletId): void {
    this.real.clearSeenVersion(waveletId);
  }

  addWantedEvaluation(evaluation: WantedEvaluation): void {
    this.real.addWantedEvaluation(evaluation);
  }

  setNotifiedVersion(waveletId: WaveletId, version: number): void {
    this.real.setNotifiedVersion(waveletId, version);
  }

  clearPendingNotification(): void {
    this.real.clearPendingNotification();
  }

  setGadgetState(gadgetId: string, key: string, value: string | null): void {
    if (this.usePrivateGadgetStates) {
      this.real.setGadgetState(gadgetId, key, value);
    }
  }
}
