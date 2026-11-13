// Supplement state combined with the wave it is about
// (`SupplementedWaveImpl`).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`SupplementedWave`, `SupplementedWaveImpl`, `SupplementWaveView`) and
// `conversation.InboxState` of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { WaveletData } from "../data";
import { DocView } from "../doc";
import { MANIFEST_DOCUMENT_ID, util as idUtil, type ParticipantId, type WaveletId } from "../id";
import { HashedVersion } from "../version";
import { SupplementImpl } from "./supplementImpl";
import {
  NO_VERSION,
  WaveletMap,
  sortedMap,
  type PrimitiveSupplement,
  type ReadablePrimitiveSupplement,
  type ThreadState,
} from "./types";
import type { WantedEvaluation, WantedEvaluationSet } from "./wanted";

/**
 * The legacy inbox folder. Moving a wave there puts it in the inbox; the
 * folder itself is never stored.
 */
export const INBOX_FOLDER = 1;
/**
 * The legacy "all" folder. Moving a wave there archives it and removes it
 * from all folders; the folder itself is never stored.
 */
export const ALL_FOLDER = 3;
/** The folder of trashed waves. */
export const TRASH_FOLDER = 8;

/** Where a wave shows up for a user (`conversation.InboxState`). */
export type InboxState = "inbox" | "mute" | "archive";

/**
 * What the supplement needs to know about the conversational wavelets of
 * the wave (`SupplementWaveView`).
 */
export interface SupplementWaveView {
  /**
   * The ids of the conversational wavelets in view (`getWavelets`). Ids
   * that are not conversational are disregarded by
   * {@link SupplementedWaveImpl} (the original throws when it meets one).
   */
  wavelets(): WaveletId[];

  /**
   * The version of a wavelet, or -1 ({@link NO_VERSION}) if it is not in
   * view (`getVersion`).
   */
  version(waveletId: WaveletId): number;

  /**
   * The hashed version of a wavelet; version zero without hash if it is
   * not in view (`getSignature`).
   */
  signature(waveletId: WaveletId): HashedVersion;

  /**
   * The last-modified version of every blip of a wavelet, in order of blip
   * id (`getBlipVersions`).
   */
  blipVersions(waveletId: WaveletId): Map<string, number>;

  /**
   * The last-modified version of one blip, or `null` if the wavelet does
   * not have it. Stands in for the blip objects the original is handed.
   */
  blipVersion(waveletId: WaveletId, blipId: string): number | null;

  /**
   * Whether the viewer is a participant of any of the wavelets
   * (`isExplicitParticipant`).
   */
  isExplicitParticipant(): boolean;
}

/** Whether a wavelet id is conversational; false where the check throws. */
function isConversational(waveletId: WaveletId): boolean {
  try {
    return idUtil.isConversationalId(waveletId);
  } catch {
    return false;
  }
}

/**
 * Whether a wavelet is a conversation as the original's conversation view
 * sees it: a conversational id and a manifest document that starts with a
 * `conversation` element.
 */
export function hasConversation(wavelet: WaveletData): boolean {
  if (!isConversational(wavelet.waveletId)) {
    return false;
  }
  const manifest = wavelet.document(MANIFEST_DOCUMENT_ID);
  if (manifest === null) {
    return false;
  }
  const view = new DocView(manifest.content);
  const top = view.firstChildElement(view.root());
  return top !== null && view.hasTag(top, "conversation");
}

/** The last-modified versions of the documents with a blip id. */
function blipVersionsOf(wavelet: WaveletData): Map<string, number> {
  return sortedMap(
    wavelet
      .documents()
      .filter((document) => idUtil.isBlipId(document.id))
      .map((document) => [document.id, document.lastModifiedVersion] as const),
  );
}

/**
 * A {@link SupplementWaveView} over wavelet data
 * (`SupplementedWaveImpl.WaveViewAdapter`, which sits on the conversation
 * model).
 *
 * In view are the wavelets with a conversational id whose manifest
 * document starts with a `conversation` element, which is what the
 * original's conversation view takes for conversations. They are listed in
 * order of id (the original: hash order). The blips of a wavelet are taken
 * to be all its documents with a blip id, whereas the original walks the
 * manifest; the difference only shows in `blipVersions`, which the
 * supplement does not use.
 *
 * Which wavelets are in view is decided at construction. The data is not
 * copied: versions are read from the wavelets as they are when asked.
 */
export class WaveletDataView implements SupplementWaveView {
  private readonly map = new WaveletMap<WaveletData>();

  /**
   * A view for `viewer` of the conversations among `wavelets`; other
   * wavelets are left out.
   */
  constructor(
    private readonly viewer: ParticipantId,
    wavelets: Iterable<WaveletData>,
  ) {
    for (const wavelet of wavelets) {
      if (hasConversation(wavelet)) {
        this.map.set(wavelet.waveletId, wavelet);
      }
    }
  }

  wavelets(): WaveletId[] {
    return this.map.keys();
  }

  version(waveletId: WaveletId): number {
    return this.map.get(waveletId)?.version ?? NO_VERSION;
  }

  signature(waveletId: WaveletId): HashedVersion {
    return this.map.get(waveletId)?.hashedVersion ?? HashedVersion.unsigned(0);
  }

  blipVersions(waveletId: WaveletId): Map<string, number> {
    const wavelet = this.map.get(waveletId);
    return wavelet === undefined ? new Map() : blipVersionsOf(wavelet);
  }

  blipVersion(waveletId: WaveletId, blipId: string): number | null {
    return this.map.get(waveletId)?.document(blipId)?.lastModifiedVersion ?? null;
  }

  isExplicitParticipant(): boolean {
    return this.map.entries().some(([, wavelet]) => wavelet.hasParticipant(this.viewer));
  }
}

/**
 * Whether a user follows a wave they have neither followed nor unfollowed
 * (`SupplementedWaveImpl.DefaultFollow`):
 *
 * * `"whenParticipant"`: only if the user is a participant of one of its
 *   conversations (`WHEN_PARTICIPANT`);
 * * `"always"` (`ALWAYS`): what the server uses for digests and the client
 *   for an open wave.
 */
export type DefaultFollow = "whenParticipant" | "always";

/** The answer of a {@link DefaultFollow} policy for a wave. */
export function isFollowedByDefault(policy: DefaultFollow, wave: SupplementWaveView): boolean {
  return policy === "always" || wave.isExplicitParticipant();
}

/** Java's narrowing of a `long` to an `int`. */
const toInt = (version: number): number => version | 0;

type Writable<V extends SupplementWaveView> = SupplementedWaveImpl<PrimitiveSupplement, V>;

/**
 * The supplement of a wave, interpreted against the wave
 * (`SupplementedWave`, `SupplementedWaveImpl`).
 *
 * `P` is the raw state: a {@link ReadablePrimitiveSupplement} suffices for
 * the queries; the changes are only callable when it is a
 * {@link PrimitiveSupplement}. Where the original takes wavelet, blip and
 * thread objects, the methods take ids and ask the view `V` for versions.
 *
 * Java narrows wavelet versions to `int` before comparing them with stored
 * versions; so does this.
 */
export class SupplementedWaveImpl<
  P extends ReadablePrimitiveSupplement = PrimitiveSupplement,
  V extends SupplementWaveView = SupplementWaveView,
> {
  /** The interpretation of the raw state that does not involve the wave. */
  readonly supplement: SupplementImpl<P>;

  /** `SupplementedWaveImpl.create`. */
  constructor(
    primitive: P,
    /** What is known about the wave. */
    readonly wave: V,
    readonly followPolicy: DefaultFollow,
  ) {
    this.supplement = new SupplementImpl(primitive);
  }

  /** The raw state. */
  get primitive(): P {
    return this.supplement.primitive;
  }

  /** The conversational wavelets in view. */
  private wavelets(): WaveletId[] {
    return this.wave.wavelets().filter(isConversational);
  }

  private version(waveletId: WaveletId): number {
    return toInt(this.wave.version(waveletId));
  }

  /** `getThreadState`. */
  threadState(waveletId: WaveletId, threadId: string): ThreadState | null {
    return this.supplement.threadState(waveletId, threadId);
  }

  /**
   * Whether a blip changed since it was last read (`isUnread`). False for
   * a blip the view does not know.
   */
  isUnread(waveletId: WaveletId, blipId: string): boolean {
    const version = this.wave.blipVersion(waveletId, blipId);
    return version !== null && this.supplement.isBlipUnread(waveletId, blipId, toInt(version));
  }

  /**
   * Whether the participants of a wavelet may have changed since they
   * were last read: whether the wavelet has (`isParticipantsUnread`).
   */
  isParticipantsUnread(waveletId: WaveletId): boolean {
    return this.supplement.isParticipantsUnread(waveletId, this.version(waveletId));
  }

  /** `haveParticipantsEverBeenRead`. */
  haveParticipantsEverBeenRead(waveletId: WaveletId): boolean {
    return this.supplement.haveParticipantsEverBeenRead(waveletId);
  }

  /**
   * Whether the tags of a wavelet may have changed since they were last
   * read: whether the wavelet has (`isTagsUnread`).
   */
  isTagsUnread(waveletId: WaveletId): boolean {
    return this.supplement.isTagsUnread(waveletId, this.version(waveletId));
  }

  /** `getFolders`. */
  folders(): number[] {
    return this.supplement.folders();
  }

  /** Whether the wave is in the inbox: followed and not archived (`isInbox`). */
  isInbox(): boolean {
    return this.isFollowed() && !this.isArchived();
  }

  /**
   * Whether every wavelet in view was archived at its current version or
   * later (`isArchived`). True for a wave without conversations; any
   * change to a wavelet after archiving makes it false again.
   */
  isArchived(): boolean {
    return this.wavelets().every((id) => this.supplement.isArchived(id, this.version(id)));
  }

  /** `isMute`: not followed. */
  isMute(): boolean {
    return !this.isFollowed();
  }

  /**
   * Whether the user follows the wave, explicitly or by the default
   * policy (`isFollowed`).
   */
  isFollowed(): boolean {
    return this.supplement.isFollowed(isFollowedByDefault(this.followPolicy, this.wave));
  }

  /** Whether the wave is in the trash folder (`isTrashed`). */
  isTrashed(): boolean {
    return this.supplement.folders().includes(TRASH_FOLDER);
  }

  /**
   * Muted if not followed, else archived if {@link isArchived}, else in
   * the inbox. The original has no such method; this is the reading its
   * documentation of `PrimitiveSupplement` gives.
   */
  inboxState(): InboxState {
    return !this.isFollowed() ? "mute" : this.isArchived() ? "archive" : "inbox";
  }

  /** `getWantedEvaluationSet`. */
  wantedEvaluationSet(waveletId: WaveletId): WantedEvaluationSet {
    return this.supplement.wantedEvaluationSet(waveletId);
  }

  /** `getSeenVersion`. */
  seenVersion(waveletId: WaveletId): HashedVersion {
    return this.supplement.seenVersion(waveletId);
  }

  /**
   * Whether any wavelet in view was seen at a version above zero
   * (`hasBeenSeen`).
   */
  hasBeenSeen(): boolean {
    return this.wavelets().some((id) => this.supplement.seenVersion(id).version > 0);
  }

  /**
   * Whether any wavelet in view was notified at a later version than it
   * was seen at; if no wavelet has a notified version, the pending flag
   * (`hasPendingNotification`).
   */
  hasPendingNotification(): boolean {
    if (!this.supplement.hasNotifiedVersion()) {
      return this.supplement.hasPendingNotification();
    }
    return this.wavelets().some((id) => this.supplement.hasPendingNotificationFor(id));
  }

  /** `getGadgetState`. */
  gadgetState(gadgetId: string): Map<string, string> {
    return this.supplement.gadgetState(gadgetId);
  }

  /** `getGadgetStateValue`. */
  gadgetStateValue(gadgetId: string, key: string): string | null {
    return this.supplement.gadgetState(gadgetId).get(key) ?? null;
  }

  /** `setThreadState`. */
  setThreadState(
    this: Writable<V>,
    waveletId: WaveletId,
    threadId: string,
    state: ThreadState | null,
  ): void {
    this.supplement.setThreadState(waveletId, threadId, state);
  }

  /**
   * Marks every wavelet in view as read at its current version
   * (`markAsRead()`).
   */
  markAsRead(this: Writable<V>): void {
    for (const id of this.wavelets()) {
      this.supplement.markWaveletAsRead(id, this.version(id));
    }
  }

  /**
   * Marks a blip as read, if it is unread, at the current version of its
   * wavelet (`markAsRead(ConversationBlip)`). Does nothing for a blip the
   * view does not know.
   */
  markBlipAsRead(this: Writable<V>, waveletId: WaveletId, blipId: string): void {
    const modified = this.wave.blipVersion(waveletId, blipId);
    if (modified === null) {
      return;
    }
    if (this.supplement.isBlipUnread(waveletId, blipId, toInt(modified))) {
      const version = toInt(Math.max(modified, this.wave.version(waveletId)));
      this.supplement.markBlipAsRead(waveletId, blipId, version);
    }
  }

  /** `markParticipantAsRead`. */
  markParticipantsAsRead(this: Writable<V>, waveletId: WaveletId): void {
    this.supplement.markParticipantsAsRead(waveletId, this.version(waveletId));
  }

  /** `markTagsAsRead`. */
  markTagsAsRead(this: Writable<V>, waveletId: WaveletId): void {
    this.supplement.markTagsAsRead(waveletId, this.version(waveletId));
  }

  /** Makes the whole wave unread (`markAsUnread`). */
  markAsUnread(this: Writable<V>): void {
    this.supplement.markAsUnread();
  }

  /** `mute`: the same as {@link unfollow}. */
  mute(this: Writable<V>): void {
    this.unfollow();
  }

  /** `follow`. */
  follow(this: Writable<V>): void {
    this.supplement.follow();
  }

  /**
   * Stops following the wave, which also clears its archive state
   * (`unfollow`).
   */
  unfollow(this: Writable<V>): void {
    this.supplement.unfollow();
  }

  /**
   * Moves the wave to a folder (`moveToFolder`): the inbox folder means
   * {@link inbox}; the "all" folder archives the wave and removes it from
   * all folders; any other folder archives the wave and becomes its only
   * folder.
   */
  moveToFolder(this: Writable<V>, folderId: number): void {
    if (folderId === INBOX_FOLDER) {
      this.inbox();
    } else if (folderId === ALL_FOLDER) {
      this.archive();
      this.supplement.removeAllFolders();
    } else {
      this.archive();
      this.supplement.moveToFolder(folderId);
    }
  }

  /**
   * Puts the wave in the inbox: follows it, removes it from all folders
   * and clears its archive state (`inbox`).
   */
  inbox(this: Writable<V>): void {
    this.follow();
    this.supplement.removeAllFolders();
    this.supplement.clearArchive();
  }

  /**
   * Records the current hashed version of every wavelet in view as seen
   * and clears the pending notification (`see()`).
   */
  see(this: Writable<V>): void {
    for (const id of this.wavelets()) {
      this.supplement.setSeenVersion(id, this.wave.signature(id));
    }
    this.supplement.clearPendingNotification();
  }

  /** The same for one wavelet (`see(Wavelet)`). */
  seeWavelet(this: Writable<V>, waveletId: WaveletId): void {
    this.supplement.setSeenVersion(waveletId, this.wave.signature(waveletId));
    this.supplement.clearPendingNotification();
  }

  /**
   * Archives every wavelet in view at its current version, if the wave is
   * followed; does nothing otherwise (`archive`).
   */
  archive(this: Writable<V>): void {
    if (this.isFollowed()) {
      for (const id of this.wavelets()) {
        this.supplement.archive(id, this.version(id));
      }
    }
  }

  /** `addWantedEvaluation`. */
  addWantedEvaluation(this: Writable<V>, evaluation: WantedEvaluation): void {
    this.supplement.addWantedEvaluation(evaluation);
  }

  /**
   * Records the current version of every wavelet in view as notified
   * (`markAsNotified`).
   */
  markAsNotified(this: Writable<V>): void {
    for (const id of this.wavelets()) {
      this.supplement.markWaveletAsNotified(id, this.version(id));
    }
  }

  /** `setGadgetState`. */
  setGadgetState(this: Writable<V>, gadgetId: string, key: string, value: string | null): void {
    this.supplement.setGadgetState(gadgetId, key, value);
  }
}
