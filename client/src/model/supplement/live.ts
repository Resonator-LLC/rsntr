// The supplement of an open wave in a client: reads from a cached
// snapshot, writes through the user-data wavelet, and reports changes,
// local or remote, as events (`LiveSupplementedWave`).
//
// Not in the Rust port, which has no listeners. The events follow
// `org.waveprotocol.wave.model.supplement.ObservableSupplementedWave`,
// `ObservablePrimitiveSupplement` and `LiveSupplementedWaveImpl` of Apache
// Wave, licensed under the Apache License, Version 2.0, with ids in place
// of objects.

import { Emitter, type Observable, type Unsubscribe } from "../../util/events";
import { MANIFEST_DOCUMENT_ID, util as idUtil, type WaveletId } from "../id";
import { HashedVersion } from "../version";
import type { OpBasedWavelet, WaveView } from "../wavelet";
import { PrimitiveSupplementImpl } from "./primitive";
import {
  SupplementedWaveImpl,
  hasConversation,
  type DefaultFollow,
  type InboxState,
  type SupplementWaveView,
} from "./supplementedWave";
import {
  NO_VERSION,
  WaveletMap,
  sortedMap,
  waveletKey,
  type PrimitiveSupplement,
  type ReadablePrimitiveSupplement,
  type ThreadState,
} from "./types";
import type { WantedEvaluation, WantedEvaluationSet } from "./wanted";
import { UserDataSupplement, WaveletBasedSupplement } from "./waveletBased";

/**
 * The events of a {@link LiveSupplementedWave}. They are emitted
 * synchronously, after the state changed, for changes made here and for
 * operations that came from the server alike. The "maybe" events may be
 * emitted although the answer of the query they are about did not change,
 * as in the original; ask again.
 */
export type SupplementEvents = {
  /**
   * The read state of a blip may have changed: its last-read version did,
   * or the blip itself changed or appeared.
   */
  maybeBlipReadChanged: { waveletId: WaveletId; blipId: string };
  /**
   * The read state of anything in a wavelet may have changed: the
   * wavelet's last-read version did, or its read state was cleared.
   */
  maybeWaveletReadChanged: { waveletId: WaveletId };
  maybeParticipantsReadChanged: { waveletId: WaveletId };
  maybeTagsReadChanged: { waveletId: WaveletId };
  /** Whether the wave is in the inbox, archived or muted may have changed. */
  maybeInboxStateChanged: Record<string, never>;
  maybeFollowStateChanged: Record<string, never>;
  /**
   * The state of a thread changed. Unlike the original, also emitted for a
   * thread the conversation does not have.
   */
  threadStateChanged: { waveletId: WaveletId; threadId: string };
  folderAdded: { folder: number };
  folderRemoved: { folder: number };
  wantedEvaluationsChanged: { waveletId: WaveletId };
  maybeGadgetStateChanged: { gadgetId: string };
  /**
   * The seen or notified versions or the pending notification flag
   * changed (not in the original).
   */
  maybeSeenChanged: Record<string, never>;
};

export interface LiveSupplementedWaveOptions {
  /**
   * Whether the wave is followed while the user has neither followed nor
   * unfollowed it. Default `"always"`, as in the original client.
   */
  followPolicy?: DefaultFollow;
  /**
   * Receives what an event listener throws. Default: the error is rethrown
   * from a microtask, so that it is reported as uncaught.
   */
  onListenerError?: (error: unknown) => void;
  /**
   * Creates the viewer's user-data wavelet when a change needs it and the
   * view has none. Default `wave.createUserData()`. A view bound to a
   * server passes what also connects the new wavelet.
   */
  createUserData?: () => OpBasedWavelet;
}

/** The raw state at one moment, with the gadget states `copyOf` leaves out. */
interface Snapshot {
  readonly primitive: PrimitiveSupplementImpl;
  readonly gadgets: ReadonlyMap<string, Map<string, string>>;
}

const EMPTY: Snapshot = { primitive: new PrimitiveSupplementImpl(), gadgets: new Map() };

function isConversational(waveletId: WaveletId): boolean {
  try {
    return idUtil.isConversationalId(waveletId);
  } catch {
    return false;
  }
}

function sameMap(a: ReadonlyMap<string, string>, b: ReadonlyMap<string, string>): boolean {
  if (a.size !== b.size) {
    return false;
  }
  for (const [key, value] of a) {
    if (b.get(key) !== value) {
      return false;
    }
  }
  return true;
}

/** The wavelets either listing has, each once. */
function union(a: readonly WaveletId[], b: readonly WaveletId[]): WaveletId[] {
  const all = new WaveletMap<null>();
  for (const id of [...a, ...b]) {
    all.set(id, null);
  }
  return all.keys();
}

/**
 * The conversations of a {@link WaveView} as the supplement sees them
 * (`SupplementedWaveImpl.WaveViewAdapter`): like `WaveletDataView`, but
 * following the view. Whether a wavelet has a conversation manifest is
 * remembered until its manifest changes.
 */
class LiveWaveView implements SupplementWaveView {
  private readonly conversations = new WeakMap<OpBasedWavelet, boolean>();

  constructor(private readonly wave: WaveView) {}

  /** Forgets what is known about the manifest of a wavelet. */
  invalidate(wavelet: OpBasedWavelet): void {
    this.conversations.delete(wavelet);
  }

  private isConversation(wavelet: OpBasedWavelet): boolean {
    let known = this.conversations.get(wavelet);
    if (known === undefined) {
      known = hasConversation(wavelet.data());
      this.conversations.set(wavelet, known);
    }
    return known;
  }

  private conversation(waveletId: WaveletId): OpBasedWavelet | null {
    const wavelet = this.wave.wavelet(waveletId);
    return wavelet !== null && this.isConversation(wavelet) ? wavelet : null;
  }

  wavelets(): WaveletId[] {
    return this.wave
      .wavelets()
      .filter((wavelet) => this.isConversation(wavelet))
      .map((wavelet) => wavelet.waveletId());
  }

  version(waveletId: WaveletId): number {
    return this.conversation(waveletId)?.data().version ?? NO_VERSION;
  }

  signature(waveletId: WaveletId): HashedVersion {
    return this.conversation(waveletId)?.data().hashedVersion ?? HashedVersion.unsigned(0);
  }

  blipVersions(waveletId: WaveletId): Map<string, number> {
    const documents = this.conversation(waveletId)?.data().documents() ?? [];
    return sortedMap(
      documents
        .filter((document) => idUtil.isBlipId(document.id))
        .map((document) => [document.id, document.lastModifiedVersion] as const),
    );
  }

  blipVersion(waveletId: WaveletId, blipId: string): number | null {
    return this.conversation(waveletId)?.data().document(blipId)?.lastModifiedVersion ?? null;
  }

  isExplicitParticipant(): boolean {
    const viewer = this.wave.viewer();
    return this.wave
      .wavelets()
      .some((wavelet) => this.isConversation(wavelet) && wavelet.data().hasParticipant(viewer));
  }
}

/**
 * The supplement of the viewer for the wave of a {@link WaveView}
 * (`ObservableSupplementedWave`, `LiveSupplementedWaveImpl`).
 *
 * **Reading.** Queries are answered from a snapshot of the user-data
 * wavelet that is renewed whenever an operation is applied to that wavelet,
 * and from the current versions of the conversation wavelets, so that asking
 * for the read state of every blip of a conversation costs a couple of map
 * lookups per blip. Without a user-data wavelet everything is unread and
 * the wave is in the inbox (by the follow policy). Wavelets, blips and
 * threads are named by id; the conversations are the wavelets with a
 * conversational id and a conversation manifest.
 *
 * **Writing.** Every change is made through `WaveletBasedSupplement` on
 * the viewer's user-data wavelet (`wave.userData()`), whose operations
 * reach its `output` sink like any other. If the view has no user-data
 * wavelet when a change is made, it is created first with
 * `wave.createUserData()`, exactly as the original client does when it
 * builds its supplement (`StageTwo.createSupplement`): that adds an empty
 * wavelet `user+<address>` to the view (event `waveletAdded`) and emits the
 * operation adding the viewer as participant, followed by the operations of
 * the change. A change that turns out to change nothing (marking a read
 * blip as read) creates the wavelet all the same. As in the original,
 * creating the supplement on a user-data wavelet removes the legacy folders
 * 1 and 3 from its folder document; here that happens with the first
 * change, and queries never report those folders.
 *
 * **Events.** See {@link SupplementEvents}. The events about the raw state
 * are found by comparing the snapshots before and after each operation on
 * the user-data wavelet, whoever made it; those about the conversations
 * come from the wavelets' own events: a blip whose last-modified version
 * changes (a remote edit, or the server's acknowledgement of a local one)
 * or that is created gives `maybeBlipReadChanged`, a wavelet whose version
 * changes gives `maybeInboxStateChanged`, `maybeTagsReadChanged` and
 * `maybeParticipantsReadChanged`. Wavelets may come and go
 * (`WaveView.addWavelet`, `replaceWavelet`, `removeWavelet`): the
 * supplement follows, and reports what changed for it.
 *
 * Call {@link dispose} when done, to stop listening to the view.
 *
 * The client's `LocalSupplementedWaveImpl`, which keeps the blips the user
 * is reading or has marked as read read while the acknowledgements of their
 * own edits come in, is the subclass `LocalSupplementedWave` (`local.ts`).
 * Here, a blip the viewer edits becomes unread for them when the server
 * acknowledges the edit, until it is marked as read again.
 */
export class LiveSupplementedWave {
  /** Change notifications; see {@link SupplementEvents}. */
  readonly events: Observable<SupplementEvents>;

  readonly followPolicy: DefaultFollow;

  private readonly emitter: Emitter<SupplementEvents>;
  private readonly view: LiveWaveView;
  private snapshot: Snapshot = EMPTY;
  private current: SupplementedWaveImpl<ReadablePrimitiveSupplement, SupplementWaveView>;
  /** Subscriptions by wavelet key. */
  private readonly subscriptions = new Map<
    string,
    { wavelet: OpBasedWavelet; unsubscribe: Unsubscribe[] }
  >();
  private readonly viewSubscriptions: Unsubscribe[];
  private disposed = false;
  private readonly createUserData: () => OpBasedWavelet;

  constructor(
    /** The wave the supplement is about. */
    readonly wave: WaveView,
    options: LiveSupplementedWaveOptions = {},
  ) {
    this.followPolicy = options.followPolicy ?? "always";
    this.createUserData = options.createUserData ?? (() => wave.createUserData());
    this.emitter = new Emitter<SupplementEvents>(
      options.onListenerError ??
        ((error) =>
          queueMicrotask(() => {
            throw error;
          })),
    );
    this.events = this.emitter;
    this.view = new LiveWaveView(wave);
    this.current = new SupplementedWaveImpl(EMPTY.primitive, this.view, this.followPolicy);
    for (const wavelet of wave.wavelets()) {
      this.attach(wavelet);
    }
    this.refresh(false);
    this.viewSubscriptions = [
      wave.events.on("waveletRemoved", ({ wavelet }) => this.onWaveletRemoved(wavelet)),
      wave.events.on("waveletAdded", ({ wavelet }) => this.onWaveletAdded(wavelet)),
    ];
  }

  /** Stops listening to the wave; no events are emitted afterwards. */
  dispose(): void {
    this.disposed = true;
    for (const unsubscribe of this.viewSubscriptions) {
      unsubscribe();
    }
    for (const { unsubscribe } of this.subscriptions.values()) {
      unsubscribe.forEach((f) => f());
    }
    this.subscriptions.clear();
    this.emitter.clear();
  }

  // ---- following the wave ----

  private attach(wavelet: OpBasedWavelet): void {
    const waveletId = wavelet.waveletId();
    const key = waveletKey(waveletId);
    this.detach(wavelet);
    const unsubscribe: Unsubscribe[] = [];
    if (idUtil.isUserDataWaveletOf(this.wave.viewer().address, waveletId)) {
      unsubscribe.push(wavelet.events.on("operation", () => this.refresh(true)));
    } else if (isConversational(waveletId)) {
      const emitter = this.emitter;
      const blipChanged = (docId: string): void => {
        if (idUtil.isBlipId(docId)) {
          emitter.emit("maybeBlipReadChanged", { waveletId, blipId: docId });
        }
      };
      unsubscribe.push(
        wavelet.events.on("documentChanged", ({ docId, created }) => {
          if (docId === MANIFEST_DOCUMENT_ID) {
            // The wavelet may have become a conversation, or ceased to be.
            this.view.invalidate(wavelet);
            emitter.emit("maybeInboxStateChanged", {});
          } else if (created) {
            blipChanged(docId);
          }
        }),
        wavelet.events.on("blipVersionChanged", ({ docId }) => blipChanged(docId)),
        wavelet.events.on("versionChanged", () => {
          emitter.emit("maybeInboxStateChanged", {});
          emitter.emit("maybeTagsReadChanged", { waveletId });
          emitter.emit("maybeParticipantsReadChanged", { waveletId });
        }),
        wavelet.events.on("participantAdded", ({ participant }) =>
          this.onParticipantChanged(participant.address),
        ),
        wavelet.events.on("participantRemoved", ({ participant }) =>
          this.onParticipantChanged(participant.address),
        ),
      );
    }
    if (unsubscribe.length > 0) {
      this.subscriptions.set(key, { wavelet, unsubscribe });
    }
  }

  private detach(wavelet: OpBasedWavelet): void {
    const key = waveletKey(wavelet.waveletId());
    const subscription = this.subscriptions.get(key);
    if (subscription !== undefined) {
      subscription.unsubscribe.forEach((f) => f());
      this.subscriptions.delete(key);
    }
  }

  /** With the default policy, following depends on the viewer's participation. */
  private onParticipantChanged(address: string): void {
    if (this.followPolicy === "whenParticipant" && address === this.wave.viewer().address) {
      this.emitter.emit("maybeFollowStateChanged", {});
      this.emitter.emit("maybeInboxStateChanged", {});
    }
  }

  private onWaveletAdded(wavelet: OpBasedWavelet): void {
    if (this.disposed) {
      return;
    }
    this.attach(wavelet);
    this.onWaveletSetChanged(wavelet);
  }

  private onWaveletRemoved(wavelet: OpBasedWavelet): void {
    if (this.disposed) {
      return;
    }
    const subscription = this.subscriptions.get(waveletKey(wavelet.waveletId()));
    if (subscription?.wavelet === wavelet) {
      this.detach(wavelet);
    }
    this.onWaveletSetChanged(wavelet);
  }

  /** A wavelet came or went: everything about it may have changed. */
  private onWaveletSetChanged(wavelet: OpBasedWavelet): void {
    const waveletId = wavelet.waveletId();
    if (isConversational(waveletId)) {
      this.emitter.emit("maybeWaveletReadChanged", { waveletId });
      this.emitter.emit("maybeParticipantsReadChanged", { waveletId });
      this.emitter.emit("maybeTagsReadChanged", { waveletId });
      this.emitter.emit("maybeInboxStateChanged", {});
      if (this.followPolicy === "whenParticipant") {
        this.emitter.emit("maybeFollowStateChanged", {});
      }
    } else {
      this.refresh(true);
    }
  }

  // ---- the snapshot and its events ----

  /** Renews the snapshot from the user-data wavelet now in view. */
  private refresh(notify: boolean): void {
    const userData = this.wave.userData();
    let snapshot = EMPTY;
    if (userData !== null) {
      const reader = new UserDataSupplement(userData.data());
      snapshot = { primitive: reader.snapshot(), gadgets: reader.gadgetStates() };
    }
    const old = this.snapshot;
    this.snapshot = snapshot;
    this.current = new SupplementedWaveImpl(snapshot.primitive, this.view, this.followPolicy);
    if (notify && !this.disposed) {
      this.notify(old, snapshot);
    }
  }

  /** Emits the events for the differences between two snapshots. */
  private notify(old: Snapshot, now: Snapshot): void {
    const emitter = this.emitter;
    const a = old.primitive;
    const b = now.primitive;

    for (const waveletId of union(a.readWavelets(), b.readWavelets())) {
      if (a.lastReadWaveletVersion(waveletId) !== b.lastReadWaveletVersion(waveletId)) {
        emitter.emit("maybeWaveletReadChanged", { waveletId });
      }
      for (const blipId of new Set([...a.readBlips(waveletId), ...b.readBlips(waveletId)])) {
        if (a.lastReadBlipVersion(waveletId, blipId) !== b.lastReadBlipVersion(waveletId, blipId)) {
          emitter.emit("maybeBlipReadChanged", { waveletId, blipId });
        }
      }
      if (a.lastReadParticipantsVersion(waveletId) !== b.lastReadParticipantsVersion(waveletId)) {
        emitter.emit("maybeParticipantsReadChanged", { waveletId });
      }
      if (a.lastReadTagsVersion(waveletId) !== b.lastReadTagsVersion(waveletId)) {
        emitter.emit("maybeTagsReadChanged", { waveletId });
      }
    }

    const followChanged = a.followed() !== b.followed();
    if (followChanged) {
      emitter.emit("maybeFollowStateChanged", {});
    }
    const archiveChanged = union(a.archiveWavelets(), b.archiveWavelets()).some(
      (id) => a.archiveWaveletVersion(id) !== b.archiveWaveletVersion(id),
    );
    if (followChanged || archiveChanged) {
      emitter.emit("maybeInboxStateChanged", {});
    }

    const oldFolders = a.folders();
    const newFolders = b.folders();
    for (const folder of oldFolders) {
      if (!newFolders.includes(folder)) {
        emitter.emit("folderRemoved", { folder });
      }
    }
    for (const folder of newFolders) {
      if (!oldFolders.includes(folder)) {
        emitter.emit("folderAdded", { folder });
      }
    }

    for (const waveletId of union(a.waveletsWithThreadState(), b.waveletsWithThreadState())) {
      const threads = new Set([...a.statefulThreads(waveletId), ...b.statefulThreads(waveletId)]);
      for (const threadId of threads) {
        if (a.threadState(waveletId, threadId) !== b.threadState(waveletId, threadId)) {
          emitter.emit("threadStateChanged", { waveletId, threadId });
        }
      }
    }

    const oldEvaluations = a.wantedEvaluations();
    const newEvaluations = b.wantedEvaluations();
    const changed = new WaveletMap<null>();
    const mark = (from: WantedEvaluation[], to: WantedEvaluation[]): void => {
      for (const evaluation of from) {
        if (evaluation.waveletId !== null && !to.some((other) => other.equals(evaluation))) {
          changed.set(evaluation.waveletId, null);
        }
      }
    };
    mark(oldEvaluations, newEvaluations);
    mark(newEvaluations, oldEvaluations);
    for (const waveletId of changed.keys()) {
      emitter.emit("wantedEvaluationsChanged", { waveletId });
    }

    const none: ReadonlyMap<string, string> = new Map();
    for (const gadgetId of new Set([...old.gadgets.keys(), ...now.gadgets.keys()])) {
      if (!sameMap(old.gadgets.get(gadgetId) ?? none, now.gadgets.get(gadgetId) ?? none)) {
        emitter.emit("maybeGadgetStateChanged", { gadgetId });
      }
    }

    const seenChanged =
      a.pendingNotification() !== b.pendingNotification() ||
      union(a.seenWavelets(), b.seenWavelets()).some(
        (id) => !a.seenVersion(id).equals(b.seenVersion(id)),
      ) ||
      union(a.notifiedWavelets(), b.notifiedWavelets()).some(
        (id) => a.notifiedVersion(id) !== b.notifiedVersion(id),
      );
    if (seenChanged) {
      emitter.emit("maybeSeenChanged", {});
    }
  }

  // ---- reading ----

  /**
   * The supplement as it is now, for the queries not repeated here. Valid
   * until the next change to the user-data wavelet: do not keep it.
   */
  state(): SupplementedWaveImpl<ReadablePrimitiveSupplement, SupplementWaveView> {
    return this.current;
  }

  /** Whether the viewer has a user-data wavelet for the wave. */
  hasUserData(): boolean {
    return this.wave.userData() !== null;
  }

  /** The ids of the conversation wavelets the supplement is about. */
  conversationWavelets(): WaveletId[] {
    return this.view.wavelets();
  }

  /**
   * Whether a blip changed since the viewer last read it (`isUnread`).
   * False for a blip the wavelet does not have, and for a wavelet that is
   * not a conversation in view.
   */
  isUnread(waveletId: WaveletId, blipId: string): boolean {
    return this.state().isUnread(waveletId, blipId);
  }

  /**
   * The unread blips among `blipIds`, in the order given; by default among
   * all documents of the wavelet with a blip id, in id order. Pass the
   * blip ids of the conversation manifest to leave out deleted blips,
   * whose documents remain.
   */
  unreadBlips(waveletId: WaveletId, blipIds?: Iterable<string>): string[] {
    const state = this.state();
    const candidates = blipIds ?? this.view.blipVersions(waveletId).keys();
    return [...candidates].filter((blipId) => state.isUnread(waveletId, blipId));
  }

  /** The number of {@link unreadBlips}. */
  unreadCount(waveletId: WaveletId, blipIds?: Iterable<string>): number {
    return this.unreadBlips(waveletId, blipIds).length;
  }

  /** `isParticipantsUnread`. */
  isParticipantsUnread(waveletId: WaveletId): boolean {
    return this.state().isParticipantsUnread(waveletId);
  }

  /** `haveParticipantsEverBeenRead`. */
  haveParticipantsEverBeenRead(waveletId: WaveletId): boolean {
    return this.state().haveParticipantsEverBeenRead(waveletId);
  }

  /** `isTagsUnread`. */
  isTagsUnread(waveletId: WaveletId): boolean {
    return this.state().isTagsUnread(waveletId);
  }

  /** `getThreadState`. */
  threadState(waveletId: WaveletId, threadId: string): ThreadState | null {
    return this.state().threadState(waveletId, threadId);
  }

  /** `getFolders`. */
  folders(): number[] {
    return this.state().folders();
  }

  /** `isInbox`: followed and not archived. */
  isInbox(): boolean {
    return this.state().isInbox();
  }

  /** `isArchived`. */
  isArchived(): boolean {
    return this.state().isArchived();
  }

  /** `isMute`: not followed. */
  isMute(): boolean {
    return this.state().isMute();
  }

  /** `isFollowed`. */
  isFollowed(): boolean {
    return this.state().isFollowed();
  }

  /** `isTrashed`. */
  isTrashed(): boolean {
    return this.state().isTrashed();
  }

  /** Muted, archived or in the inbox. */
  inboxState(): InboxState {
    return this.state().inboxState();
  }

  /** `getSeenVersion`. */
  seenVersion(waveletId: WaveletId): HashedVersion {
    return this.state().seenVersion(waveletId);
  }

  /** `hasBeenSeen`. */
  hasBeenSeen(): boolean {
    return this.state().hasBeenSeen();
  }

  /** `hasPendingNotification`. */
  hasPendingNotification(): boolean {
    return this.state().hasPendingNotification();
  }

  /** `getWantedEvaluationSet`. */
  wantedEvaluationSet(waveletId: WaveletId): WantedEvaluationSet {
    return this.state().wantedEvaluationSet(waveletId);
  }

  /** `getGadgetState`: the private state of a gadget, in key order. */
  gadgetState(gadgetId: string): Map<string, string> {
    return new Map(this.snapshot.gadgets.get(gadgetId) ?? []);
  }

  /** `getGadgetStateValue`. */
  gadgetStateValue(gadgetId: string, key: string): string | null {
    return this.snapshot.gadgets.get(gadgetId)?.get(key) ?? null;
  }

  // ---- writing ----

  /**
   * The viewer's user-data wavelet, created with `wave.createUserData()`
   * if the view has none.
   */
  ensureUserData(): OpBasedWavelet {
    return this.wave.userData() ?? this.createUserData();
  }

  /**
   * Makes changes through a supplement on the user-data wavelet, which is
   * created if missing. For the changes not repeated here (the raw state
   * is `supplemented.primitive`). The supplement handed to `change` must
   * not be kept.
   */
  write<R>(
    change: (supplemented: SupplementedWaveImpl<PrimitiveSupplement, SupplementWaveView>) => R,
  ): R {
    try {
      const primitive = WaveletBasedSupplement.create(this.ensureUserData());
      return change(new SupplementedWaveImpl(primitive, this.view, this.followPolicy));
    } finally {
      // The operations' events have renewed the snapshot already, unless
      // they are held back because this was called from a listener of the
      // user-data wavelet.
      this.refresh(true);
    }
  }

  /**
   * Marks a blip as read, if it is unread, at the current version of its
   * wavelet (`markAsRead(ConversationBlip)`). Does nothing for a blip the
   * wavelet does not have.
   */
  markBlipAsRead(waveletId: WaveletId, blipId: string): void {
    this.write((s) => s.markBlipAsRead(waveletId, blipId));
  }

  /**
   * Marks every conversation wavelet as read at its current version
   * (`markAsRead()`): all blips, participants and tags.
   */
  markAsRead(): void {
    this.write((s) => s.markAsRead());
  }

  /** `markParticipantAsRead`. */
  markParticipantsAsRead(waveletId: WaveletId): void {
    this.write((s) => s.markParticipantsAsRead(waveletId));
  }

  /** `markTagsAsRead`. */
  markTagsAsRead(waveletId: WaveletId): void {
    this.write((s) => s.markTagsAsRead(waveletId));
  }

  /** Makes the whole wave unread (`markAsUnread`). */
  markAsUnread(): void {
    this.write((s) => s.markAsUnread());
  }

  /** `setThreadState`; `null` removes the state. */
  setThreadState(waveletId: WaveletId, threadId: string, state: ThreadState | null): void {
    this.write((s) => s.setThreadState(waveletId, threadId, state));
  }

  /** `follow`. */
  follow(): void {
    this.write((s) => s.follow());
  }

  /** `unfollow`: also clears the archive state. */
  unfollow(): void {
    this.write((s) => s.unfollow());
  }

  /** `mute`: the same as {@link unfollow}. */
  mute(): void {
    this.unfollow();
  }

  /** `archive`: at the current versions, if the wave is followed. */
  archive(): void {
    this.write((s) => s.archive());
  }

  /** `inbox`: follows, removes from all folders, clears the archive state. */
  inbox(): void {
    this.write((s) => s.inbox());
  }

  /** `moveToFolder`. */
  moveToFolder(folderId: number): void {
    this.write((s) => s.moveToFolder(folderId));
  }

  /** `see()`: records the current hashed versions as seen. */
  see(): void {
    this.write((s) => s.see());
  }

  /** `see(Wavelet)`. */
  seeWavelet(waveletId: WaveletId): void {
    this.write((s) => s.seeWavelet(waveletId));
  }

  /** `markAsNotified`. */
  markAsNotified(): void {
    this.write((s) => s.markAsNotified());
  }

  /** `addWantedEvaluation`. */
  addWantedEvaluation(evaluation: WantedEvaluation): void {
    this.write((s) => s.addWantedEvaluation(evaluation));
  }

  /** `setGadgetState`; `null` removes the key. */
  setGadgetState(gadgetId: string, key: string, value: string | null): void {
    this.write((s) => s.setGadgetState(gadgetId, key, value));
  }
}
