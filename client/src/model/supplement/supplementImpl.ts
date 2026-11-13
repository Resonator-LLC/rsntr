// The meaning of the raw supplement state (`SupplementImpl`).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`Supplement`, `SupplementImpl`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { WaveletId } from "../id";
import type { HashedVersion } from "../version";
import {
  NO_VERSION,
  type PrimitiveSupplement,
  type ReadablePrimitiveSupplement,
  type ThreadState,
} from "./types";
import { WantedEvaluationSet, type WantedEvaluation } from "./wanted";

function isUnread(read: number, modified: number): boolean {
  return read === NO_VERSION || read < modified;
}

/**
 * Interprets raw supplement state (`Supplement`, `SupplementImpl`). The
 * queries need a readable state; the changes are only callable when `P` is
 * a writable one.
 */
export class SupplementImpl<P extends ReadablePrimitiveSupplement = PrimitiveSupplement> {
  constructor(
    /** The raw state. */
    readonly primitive: P,
  ) {}

  /** `getThreadState`. */
  threadState(waveletId: WaveletId, threadId: string): ThreadState | null {
    return this.primitive.threadState(waveletId, threadId);
  }

  /**
   * Whether a blip last modified at `version` is unread: neither the blip
   * nor its whole wavelet was marked as read at that version or later
   * (`isBlipUnread`).
   */
  isBlipUnread(waveletId: WaveletId, blipId: string, version: number): boolean {
    return (
      isUnread(this.primitive.lastReadBlipVersion(waveletId, blipId), version) &&
      isUnread(this.primitive.lastReadWaveletVersion(waveletId), version)
    );
  }

  /**
   * Whether the participants of a wavelet now at `version` are unread
   * (`isParticipantsUnread`).
   */
  isParticipantsUnread(waveletId: WaveletId, version: number): boolean {
    return (
      isUnread(this.primitive.lastReadParticipantsVersion(waveletId), version) &&
      isUnread(this.primitive.lastReadWaveletVersion(waveletId), version)
    );
  }

  /** `haveParticipantsEverBeenRead`. */
  haveParticipantsEverBeenRead(waveletId: WaveletId): boolean {
    return (
      this.primitive.lastReadParticipantsVersion(waveletId) !== NO_VERSION ||
      this.primitive.lastReadWaveletVersion(waveletId) !== NO_VERSION
    );
  }

  /**
   * Whether the tags of a wavelet now at `version` are unread
   * (`isTagsUnread`).
   */
  isTagsUnread(waveletId: WaveletId, version: number): boolean {
    return (
      isUnread(this.primitive.lastReadTagsVersion(waveletId), version) &&
      isUnread(this.primitive.lastReadWaveletVersion(waveletId), version)
    );
  }

  /** `getFolders`. */
  folders(): number[] {
    return this.primitive.folders();
  }

  /**
   * Whether a wavelet now at `version` is archived: it was archived at
   * that version or later (`isArchived`).
   */
  isArchived(waveletId: WaveletId, version: number): boolean {
    return this.primitive.archiveWaveletVersion(waveletId) >= version;
  }

  /**
   * The explicit follow state, or the default if there is none
   * (`isFollowed`).
   */
  isFollowed(defaultFollowed: boolean): boolean {
    return this.primitive.followed() ?? defaultFollowed;
  }

  /** `getSeenVersion`. */
  seenVersion(waveletId: WaveletId): HashedVersion {
    return this.primitive.seenVersion(waveletId);
  }

  /** `getNotifiedVersion`. */
  notifiedVersion(waveletId: WaveletId): number {
    return this.primitive.notifiedVersion(waveletId);
  }

  /** `getSeenWavelets`. */
  seenWavelets(): WaveletId[] {
    return this.primitive.seenWavelets();
  }

  /** `hasSeenVersion`. */
  hasSeenVersion(): boolean {
    return this.primitive.seenWavelets().length > 0;
  }

  /** The evaluations of one wavelet (`getWantedEvaluationSet`). */
  wantedEvaluationSet(waveletId: WaveletId): WantedEvaluationSet {
    const relevant = this.primitive
      .wantedEvaluations()
      .filter((evaluation) => evaluation.waveletId?.equals(waveletId) ?? false);
    return new WantedEvaluationSet(waveletId, relevant);
  }

  /** `hasPendingNotification()`. */
  hasPendingNotification(): boolean {
    return this.primitive.pendingNotification();
  }

  /**
   * Whether a wavelet was notified at a later version than it was seen at;
   * if no wavelet has a notified version, the pending flag
   * (`hasPendingNotification(WaveletId)`).
   */
  hasPendingNotificationFor(waveletId: WaveletId): boolean {
    if (!this.hasNotifiedVersion()) {
      return this.hasPendingNotification();
    }
    return this.seenVersion(waveletId).version < this.notifiedVersion(waveletId);
  }

  /** `hasNotifiedVersion`. */
  hasNotifiedVersion(): boolean {
    return this.primitive.notifiedWavelets().length > 0;
  }

  /** `getGadgetState`. */
  gadgetState(gadgetId: string): Map<string, string> {
    return this.primitive.gadgetState(gadgetId);
  }

  /** `setThreadState`. */
  setThreadState(
    this: SupplementImpl<PrimitiveSupplement>,
    waveletId: WaveletId,
    threadId: string,
    state: ThreadState | null,
  ): void {
    this.primitive.setThreadState(waveletId, threadId, state);
  }

  /**
   * Records that a blip was read at `version`, if it is unread at that
   * version (`markBlipAsRead`).
   */
  markBlipAsRead(
    this: SupplementImpl<PrimitiveSupplement>,
    waveletId: WaveletId,
    blipId: string,
    version: number,
  ): void {
    if (this.isBlipUnread(waveletId, blipId, version)) {
      this.primitive.setLastReadBlipVersion(waveletId, blipId, version);
    }
  }

  /** `markParticipantsAsRead`. */
  markParticipantsAsRead(
    this: SupplementImpl<PrimitiveSupplement>,
    waveletId: WaveletId,
    version: number,
  ): void {
    if (this.isParticipantsUnread(waveletId, version)) {
      this.primitive.setLastReadParticipantsVersion(waveletId, version);
    }
  }

  /** `markTagsAsRead`. */
  markTagsAsRead(
    this: SupplementImpl<PrimitiveSupplement>,
    waveletId: WaveletId,
    version: number,
  ): void {
    if (this.isTagsUnread(waveletId, version)) {
      this.primitive.setLastReadTagsVersion(waveletId, version);
    }
  }

  /**
   * Records that everything in a wavelet was read at `version`
   * (`markWaveletAsRead`).
   */
  markWaveletAsRead(
    this: SupplementImpl<PrimitiveSupplement>,
    waveletId: WaveletId,
    version: number,
  ): void {
    this.primitive.setLastReadWaveletVersion(waveletId, version);
  }

  /** Forgets all read state, making the whole wave unread (`markAsUnread`). */
  markAsUnread(this: SupplementImpl<PrimitiveSupplement>): void {
    this.primitive.clearReadState();
  }

  /** Makes a folder the only folder of the wave (`moveToFolder`). */
  moveToFolder(this: SupplementImpl<PrimitiveSupplement>, id: number): void {
    this.primitive.removeAllFolders();
    this.primitive.addFolder(id);
  }

  /** `removeAllFolders`. */
  removeAllFolders(this: SupplementImpl<PrimitiveSupplement>): void {
    this.primitive.removeAllFolders();
  }

  /** `follow`. */
  follow(this: SupplementImpl<PrimitiveSupplement>): void {
    this.primitive.follow();
  }

  /** Unfollows the wave, clearing the archive state first (`unfollow`). */
  unfollow(this: SupplementImpl<PrimitiveSupplement>): void {
    this.primitive.clearArchiveState();
    this.primitive.unfollow();
  }

  /** `clearArchive`. */
  clearArchive(this: SupplementImpl<PrimitiveSupplement>): void {
    this.primitive.clearArchiveState();
  }

  /** `archive`. */
  archive(this: SupplementImpl<PrimitiveSupplement>, waveletId: WaveletId, version: number): void {
    this.primitive.archiveAtVersion(waveletId, version);
  }

  /** `setSeenVersion`. */
  setSeenVersion(
    this: SupplementImpl<PrimitiveSupplement>,
    waveletId: WaveletId,
    signature: HashedVersion,
  ): void {
    this.primitive.setSeenVersion(waveletId, signature);
  }

  /** `addWantedEvaluation`. */
  addWantedEvaluation(
    this: SupplementImpl<PrimitiveSupplement>,
    evaluation: WantedEvaluation,
  ): void {
    this.primitive.addWantedEvaluation(evaluation);
  }

  /** `markWaveletAsNotified`. */
  markWaveletAsNotified(
    this: SupplementImpl<PrimitiveSupplement>,
    waveletId: WaveletId,
    version: number,
  ): void {
    this.primitive.setNotifiedVersion(waveletId, version);
  }

  /** `clearPendingNotification`. */
  clearPendingNotification(this: SupplementImpl<PrimitiveSupplement>): void {
    this.primitive.clearPendingNotification();
  }

  /** `setGadgetState`. */
  setGadgetState(
    this: SupplementImpl<PrimitiveSupplement>,
    gadgetId: string,
    key: string,
    value: string | null,
  ): void {
    this.primitive.setGadgetState(gadgetId, key, value);
  }
}
