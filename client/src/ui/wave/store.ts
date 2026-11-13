// The view model of one conversation of an open wave: its threads and
// blips as signals keyed by id, the focus frame, the edit session and the
// actions of the wave panel.
//
// The structure is reconciled against a snapshot of the conversation
// whenever a structural event arrives (once per tick, or at once when an
// action needs it), so that only the threads and blips that changed
// re-render. Read state follows the supplement's events.
//
// The focus frame, the edit session and the actions follow the Apache Wave
// web client (`org.waveprotocol.wave.client.wavepanel.impl`:
// `focus/FocusFramePresenter`, `edit/EditSession`, `edit/ActionsImpl`,
// `toolbar/ViewToolbar`, `collapse/CollapsePresenter`, Apache License 2.0).

import { type ReadonlySignal, type Signal, batch, computed, signal } from "@preact/signals";

import type { ConversationTarget, OpenWave } from "../../client";
import type {
  Conversation,
  ConversationThread,
  ObservableConversation,
} from "../../model/conversation";
import type { DocOp } from "../../model/docop";
import type { WaveletId } from "../../model/id";
import { ThreadState } from "../../model/supplement";
import type { BlipView, DocumentOpEvent, EditorDocument } from "../editor/api";
import { Reader, type ReadingSupplement, SimpleReadingSupplement } from "./reader";
import { type BlipTree, ROOT_THREAD, Traverser } from "./traversal";

/** A reply thread of a blip and whether it has an anchor in the blip's text. */
export interface ReplyRef {
  readonly threadId: string;
  readonly anchored: boolean;
}

/** A request to bring a blip to the user's attention, for its component. */
export interface FocusRequest {
  /** Scroll the blip into view. */
  readonly scroll: boolean;
  /** Give the blip the keyboard focus. */
  readonly dom: boolean;
}

const NO_ANCHORS: ReadonlyMap<string, HTMLElement> = new Map();

export class ThreadVM {
  readonly blipIds: Signal<readonly string[]> = signal([]);
  readonly collapsed = signal(false);
  /** The number of blips in the thread, replies included (`getTotalCount`). */
  readonly total: ReadonlySignal<number>;
  /** The number of unread blips in the thread, replies included. */
  readonly unread: ReadonlySignal<number>;

  constructor(
    readonly id: string,
    public parentBlipId: string | null,
    store: ConversationStore,
  ) {
    const count = (unreadOnly: boolean): number => {
      let n = 0;
      for (const blipId of this.blipIds.value) {
        const blip = store.blips.get(blipId);
        if (blip === undefined) {
          continue;
        }
        if (!unreadOnly || blip.unread.value) {
          n++;
        }
        for (const reply of blip.replies.value) {
          const thread = store.threads.get(reply.threadId);
          if (thread !== undefined) {
            n += unreadOnly ? thread.unread.value : thread.total.value;
          }
        }
      }
      return n;
    };
    this.total = computed(() => count(false));
    this.unread = computed(() => count(true));
  }
}

export class BlipVM {
  readonly replies: Signal<readonly ReplyRef[]> = signal([]);
  readonly unread = signal(false);
  readonly focused = signal(false);
  readonly editing = signal(false);
  /** Bumped when author, contributors or time changed. */
  readonly meta = signal(0);
  /** The containers the editor made for the anchors of inline reply threads. */
  readonly anchors: Signal<ReadonlyMap<string, HTMLElement>> = signal(NO_ANCHORS);
  /** The blip's document must be rendered now (it is focused, edited, or has the focus inside). */
  readonly needView = signal(false);
  /** The blip's document is rendered. */
  readonly viewReady = signal(false);
  readonly focusRequest: Signal<FocusRequest | null> = signal(null);

  constructor(
    readonly id: string,
    public threadId: string,
  ) {}
}

export interface BlipMeta {
  readonly author: string;
  readonly contributors: readonly string[];
  readonly lastModifiedTime: number;
}

export interface StoreOptions {
  /** Something a wave digest shows may have changed (not debounced). */
  readonly onChanged?: () => void;
  /** Receives what an action throws (e.g. an edit the wavelet refuses). */
  readonly onError?: (error: unknown) => void;
}

type OpListener = (event: DocumentOpEvent) => void;

/** `LocalSupplementedWave`, if the open wave has it. */
interface LocalReading {
  startReading(blipId: string, target?: ConversationTarget): void;
  stopReading(blipId: string, target?: ConversationTarget): void;
}

function localReadingOf(wave: unknown): LocalReading | null {
  const candidate = wave as Partial<LocalReading>;
  return typeof candidate.startReading === "function" && typeof candidate.stopReading === "function"
    ? (candidate as LocalReading)
    : null;
}

export class ConversationStore implements BlipTree {
  readonly threads = new Map<string, ThreadVM>();
  readonly blips = new Map<string, BlipVM>();
  /** The views of the blips whose document is rendered, set by their components. */
  readonly views = new Map<string, BlipView>();

  readonly title = signal("");
  readonly participants: Signal<readonly string[]> = signal([]);
  readonly tags: Signal<readonly string[]> = signal([]);
  readonly focusedId: Signal<string | null> = signal(null);
  readonly editingId: Signal<string | null> = signal(null);
  /** The view of the blip being edited, once it is editable. */
  readonly editingView: Signal<BlipView | null> = signal(null);
  /** The blip being edited, if a reply action of this user just created it. */
  private draftId: string | null = null;
  /** Whether this user changed the blip being edited during the session. */
  private sessionChanged = false;
  readonly archived = signal(false);
  readonly followed = signal(true);
  /** Whether the wavelet has a conversation (a manifest). */
  readonly hasConversation = signal(false);
  /** The last failure of an action, for display; null clears. */
  readonly error: Signal<string | null> = signal(null);

  readonly waveletId: WaveletId;
  readonly target: ConversationTarget;
  private readonly observable: ObservableConversation;
  private readonly reading: ReadingSupplement;
  private readonly reader: Reader;
  /** The order of the focus frame: collapsed threads are skipped. */
  private readonly traverser: Traverser;
  /** The order of everything: for "next unread" and "recent". */
  private readonly fullTraverser: Traverser;
  private readonly unsubscribe: (() => void)[] = [];
  private readonly opListeners = new Map<string, Set<OpListener>>();
  private readonly dirtyBlips = new Set<string>();
  private structureDirty = false;
  private flushScheduled = false;
  private disposed = false;
  private rootBlip: string | null = null;

  constructor(
    readonly wave: OpenWave,
    observable: ObservableConversation,
    private readonly options: StoreOptions = {},
  ) {
    this.observable = observable;
    this.waveletId = observable.waveletId;
    this.target = { waveletId: this.waveletId };
    this.traverser = new Traverser(this, true);
    this.fullTraverser = new Traverser(this, false);

    const target = this.target;
    const local = localReadingOf(wave);
    if (local !== null) {
      this.reading = {
        startReading: (blipId) => local.startReading(blipId, target),
        stopReading: (blipId) => local.stopReading(blipId, target),
        isUnread: (blipId) => wave.isUnread(blipId, target),
      };
    } else {
      this.reading = new SimpleReadingSupplement({
        isUnread: (blipId) => wave.isUnread(blipId, target),
        markBlipRead: (blipId) => wave.markBlipRead(blipId, target),
      });
    }
    this.reader = new Reader(this.reading, this.fullTraverser, (blipId) => this.blips.has(blipId));

    this.threads.set(ROOT_THREAD, new ThreadVM(ROOT_THREAD, null, this));
    this.structureDirty = true;
    this.flush();
    this.title.value = observable.title();
    this.tags.value = [...observable.tags()];
    this.refreshParticipants();
    this.refreshInbox();

    const e = observable.events;
    const structural = (): void => {
      this.structureDirty = true;
      this.scheduleFlush();
    };
    const parentDirty = ({ parentBlipId }: { parentBlipId: string }): void => {
      this.dirtyBlips.add(parentBlipId);
      structural();
    };
    this.unsubscribe.push(
      e.on("blipAdded", structural),
      e.on("blipDeleted", structural),
      e.on("threadAdded", parentDirty),
      e.on("threadDeleted", parentDirty),
      e.on("inlineThreadAdded", parentDirty),
      e.on("inlineThreadRemoved", parentDirty),
      e.on("structureChanged", structural),
      e.on("blipContentChanged", ({ blipId, op, local }) => this.onContent(blipId, op, local)),
      e.on("blipContributorAdded", ({ blipId }) => this.bumpMeta(blipId)),
      e.on("blipContributorRemoved", ({ blipId }) => this.bumpMeta(blipId)),
      e.on("blipTimestampChanged", ({ blipId }) => this.bumpMeta(blipId)),
      e.on("participantAdded", () => this.refreshParticipants(true)),
      e.on("participantRemoved", () => this.refreshParticipants(true)),
      e.on("titleChanged", ({ title }) => {
        this.title.value = title;
        this.changed();
      }),
      e.on("tagsChanged", ({ tags }) => {
        this.tags.value = [...tags];
        this.changed();
      }),
    );
    const s = wave.supplement.events;
    const mine = (waveletId: WaveletId): boolean => waveletId.equals(this.waveletId);
    this.unsubscribe.push(
      s.on("maybeBlipReadChanged", ({ waveletId, blipId }) => {
        if (mine(waveletId)) {
          this.refreshUnread(blipId);
        }
      }),
      s.on("maybeWaveletReadChanged", ({ waveletId }) => {
        if (mine(waveletId)) {
          this.refreshAllUnread();
        }
      }),
      s.on("threadStateChanged", ({ waveletId, threadId }) => {
        if (mine(waveletId)) {
          const thread = this.threads.get(threadId);
          if (thread !== undefined) {
            thread.collapsed.value = this.storedCollapsed(threadId);
          }
        }
      }),
      s.on("maybeInboxStateChanged", () => this.refreshInbox(true)),
      s.on("maybeFollowStateChanged", () => this.refreshInbox(true)),
    );
  }

  dispose(): void {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    const focused = this.focusedId.peek();
    if (focused !== null && this.blips.has(focused)) {
      // The focus frame goes away with the panel: reading stops.
      this.guard(() => this.reader.onFocusMoved(focused, null));
    }
    for (const unsubscribe of this.unsubscribe) {
      unsubscribe();
    }
    this.opListeners.clear();
  }

  // ---- structure ----

  threadBlips(threadId: string): readonly string[] {
    return this.threads.get(threadId)?.blipIds.peek() ?? [];
  }

  threadOf(blipId: string): string | null {
    return this.blips.get(blipId)?.threadId ?? null;
  }

  replyThreads(blipId: string): readonly string[] {
    return (this.blips.get(blipId)?.replies.peek() ?? []).map((reply) => reply.threadId);
  }

  parentBlipOf(threadId: string): string | null {
    return this.threads.get(threadId)?.parentBlipId ?? null;
  }

  isCollapsed(threadId: string): boolean {
    return this.threads.get(threadId)?.collapsed.peek() ?? false;
  }

  /** The first blip of the root thread, or null. */
  rootBlipId(): string | null {
    return this.rootBlip;
  }

  isRootBlip(blipId: string): boolean {
    return this.rootBlip === blipId;
  }

  /** The blips in display order, collapsed threads included. */
  blipOrder(): string[] {
    const order: string[] = [];
    for (let id = this.fullTraverser.first(); id !== null; id = this.fullTraverser.next(id)) {
      order.push(id);
    }
    return order;
  }

  /** How deep a thread is nested: 0 for the root thread. */
  depthOf(threadId: string): number {
    let depth = 0;
    let current: string | null = threadId;
    while (current !== null && current !== ROOT_THREAD && depth < 1000) {
      const parent: string | null = this.parentBlipOf(current);
      current = parent === null ? null : this.threadOf(parent);
      depth++;
    }
    return depth;
  }

  blipMeta(blipId: string): BlipMeta | null {
    const data = this.observable.wavelet.blip(blipId);
    if (data === null) {
      return null;
    }
    return {
      author: data.author.address,
      contributors: data.contributors.map((c) => c.address),
      lastModifiedTime: data.lastModifiedTime,
    };
  }

  private storedCollapsed(threadId: string): boolean {
    return this.wave.supplement.threadState(this.waveletId, threadId) === ThreadState.Collapsed;
  }

  private scheduleFlush(): void {
    if (this.flushScheduled) {
      return;
    }
    this.flushScheduled = true;
    queueMicrotask(() => {
      this.flushScheduled = false;
      if (!this.disposed) {
        this.flush();
      }
    });
  }

  /** Brings the threads and blips up to date with the conversation. */
  flush(): void {
    if (!this.structureDirty && this.dirtyBlips.size === 0) {
      return;
    }
    this.structureDirty = false;
    const dirty = new Set(this.dirtyBlips);
    this.dirtyBlips.clear();
    const conversation = this.observable.read();
    batch(() => this.reconcile(conversation, dirty));
  }

  private reconcile(conversation: Conversation | null, dirty: Set<string>): void {
    this.hasConversation.value = conversation !== null;
    const seenThreads = new Set<string>([ROOT_THREAD]);
    const seenBlips = new Set<string>();
    const visit = (thread: ConversationThread, parentBlipId: string | null): void => {
      let vm = this.threads.get(thread.id);
      if (vm === undefined) {
        vm = new ThreadVM(thread.id, parentBlipId, this);
        vm.collapsed.value = this.storedCollapsed(thread.id);
        this.threads.set(thread.id, vm);
      } else {
        vm.parentBlipId = parentBlipId;
      }
      const ids: string[] = [];
      for (const blip of thread.blips) {
        if (seenBlips.has(blip.id)) {
          // Listed twice in a malformed manifest: shown once.
          continue;
        }
        seenBlips.add(blip.id);
        ids.push(blip.id);
        let blipVm = this.blips.get(blip.id);
        const isNew = blipVm === undefined;
        if (blipVm === undefined) {
          blipVm = new BlipVM(blip.id, thread.id);
          blipVm.unread.value = this.reading.isUnread(blip.id);
          this.blips.set(blip.id, blipVm);
        } else {
          blipVm.threadId = thread.id;
        }
        if (isNew || dirty.has(blip.id)) {
          const replies: ReplyRef[] = [];
          if (blip.replyThreads.length > 0) {
            for (const located of conversation!.locateReplyThreads(blip)) {
              if (!replies.some((r) => r.threadId === located.thread.id)) {
                replies.push({ threadId: located.thread.id, anchored: located.location !== null });
              }
            }
          }
          if (!sameReplies(blipVm.replies.peek(), replies)) {
            blipVm.replies.value = replies;
          }
        }
        for (const reply of blip.replyThreads) {
          if (!seenThreads.has(reply.id)) {
            seenThreads.add(reply.id);
            visit(reply, blip.id);
          }
        }
      }
      if (!sameIds(vm.blipIds.peek(), ids)) {
        vm.blipIds.value = ids;
      }
    };

    // Where the focus goes if its blip is gone: decided on the old structure.
    const focused = this.focusedId.peek();
    let refocus: string[] = [];
    if (focused !== null) {
      refocus = this.focusFallbacks(focused);
    }

    if (conversation !== null) {
      visit(conversation.rootThread, null);
    } else {
      this.threads.get(ROOT_THREAD)!.blipIds.value = [];
    }
    this.rootBlip = this.threads.get(ROOT_THREAD)!.blipIds.peek()[0] ?? null;

    for (const id of [...this.blips.keys()]) {
      if (!seenBlips.has(id)) {
        this.blips.delete(id);
        this.views.delete(id);
        this.opListeners.delete(id);
      }
    }
    for (const id of [...this.threads.keys()]) {
      if (!seenThreads.has(id)) {
        this.threads.delete(id);
      }
    }

    const editing = this.editingId.peek();
    if (editing !== null && !this.blips.has(editing)) {
      this.editingId.value = null;
      this.editingView.value = null;
    }
    if (focused !== null && !this.blips.has(focused)) {
      this.focusedId.value = null;
      const next = refocus.find((id) => this.blips.has(id));
      if (next !== undefined) {
        this.focus(next, { scroll: false, dom: true });
      }
    }
  }

  /**
   * Where the focus frame goes when its blip is deleted (`ActionsImpl.delete`):
   * the next blip of the thread, else the previous one, else the previous
   * blip in the order. Candidates in order of preference.
   */
  private focusFallbacks(blipId: string): string[] {
    const candidates: string[] = [];
    const threadId = this.threadOf(blipId);
    if (threadId !== null) {
      const blips = this.threadBlips(threadId);
      const index = blips.indexOf(blipId);
      candidates.push(...blips.slice(index + 1), ...blips.slice(0, index).reverse());
    }
    for (let id = this.traverser.previous(blipId); id !== null; id = this.traverser.previous(id)) {
      candidates.push(id);
    }
    return candidates;
  }

  // ---- events ----

  private onContent(blipId: string, op: DocOp, local: boolean): void {
    if (local && blipId === this.editingId.peek()) {
      this.sessionChanged = true;
    }
    const listeners = this.opListeners.get(blipId);
    if (listeners !== undefined) {
      for (const listener of [...listeners]) {
        listener({ op, local });
      }
    }
  }

  private bumpMeta(blipId: string): void {
    const blip = this.blips.get(blipId);
    if (blip !== undefined) {
      blip.meta.value++;
    }
  }

  private refreshParticipants(notify = false): void {
    this.participants.value = this.wave.participants(this.target).map((p) => p.address);
    if (notify) {
      this.changed();
    }
  }

  private refreshInbox(notify = false): void {
    const supplement = this.wave.supplement;
    const archived = supplement.isArchived();
    const followed = supplement.isFollowed();
    if (archived !== this.archived.peek() || followed !== this.followed.peek()) {
      this.archived.value = archived;
      this.followed.value = followed;
      if (notify) {
        this.changed();
      }
    }
  }

  private refreshUnread(blipId: string): void {
    const blip = this.blips.get(blipId);
    if (blip === undefined) {
      return;
    }
    const unread = this.reading.isUnread(blipId);
    if (unread !== blip.unread.peek()) {
      blip.unread.value = unread;
      this.changed();
    }
  }

  private refreshAllUnread(): void {
    let changed = false;
    batch(() => {
      for (const blip of this.blips.values()) {
        const unread = this.reading.isUnread(blip.id);
        if (unread !== blip.unread.peek()) {
          blip.unread.value = unread;
          changed = true;
        }
      }
    });
    if (changed) {
      this.changed();
    }
  }

  private changed(): void {
    this.options.onChanged?.();
  }

  /** Runs an action; what it throws is reported, not propagated. */
  private guard<R>(action: () => R): R | undefined {
    try {
      const result = action();
      return result;
    } catch (error) {
      this.error.value = error instanceof Error ? error.message : String(error);
      this.options.onError?.(error);
      return undefined;
    }
  }

  // ---- documents ----

  /** The document of a blip, as the editor wants it. */
  documentFor(blipId: string): EditorDocument {
    const wavelet = this.observable.wavelet;
    return {
      document: () => wavelet.document(blipId),
      edit: (change) => this.wave.editBlip(blipId, change, this.target),
      applyOp: (op) => wavelet.applyDocOp(blipId, op),
      onOp: (listener) => {
        let listeners = this.opListeners.get(blipId);
        if (listeners === undefined) {
          listeners = new Set();
          this.opListeners.set(blipId, listeners);
        }
        listeners.add(listener);
        return () => {
          this.opListeners.get(blipId)?.delete(listener);
        };
      },
    };
  }

  /** The user changed a blip through its view (`WaveTitleHandler`). */
  onLocalEdit(blipId: string): void {
    if (this.isRootBlip(blipId)) {
      this.guard(() => this.wave.updateImplicitTitle(this.target));
    }
    this.changed();
  }

  // ---- focus ----

  /**
   * Puts the focus frame on a blip (`FocusFramePresenter.focus`). Threads
   * collapsed around it are expanded.
   */
  focus(blipId: string, request: FocusRequest = { scroll: true, dom: true }): void {
    this.flush();
    const blip = this.blips.get(blipId);
    if (blip === undefined) {
      return;
    }
    this.reveal(blipId);
    const old = this.focusedId.peek();
    if (request.scroll || request.dom) {
      blip.focusRequest.value = request;
    }
    if (old === blipId) {
      return;
    }
    batch(() => {
      if (old !== null) {
        const oldBlip = this.blips.get(old);
        if (oldBlip !== undefined) {
          oldBlip.focused.value = false;
        }
      }
      blip.focused.value = true;
      this.focusedId.value = blipId;
    });
    // `EditSession.onFocusMoved`: the session ends when the frame moves.
    if (this.editingId.peek() !== null) {
      this.endSession();
    }
    this.guard(() => this.reader.onFocusMoved(old, blipId));
  }

  /** Makes sure a blip is on screen: its ancestors are expanded and rendered. */
  private reveal(blipId: string): void {
    const blip = this.blips.get(blipId);
    if (blip === undefined) {
      return;
    }
    blip.needView.value = true;
    let threadId: string | null = blip.threadId;
    for (let guard = 0; threadId !== null && threadId !== ROOT_THREAD && guard < 1000; guard++) {
      const thread = this.threads.get(threadId);
      if (thread === undefined) {
        break;
      }
      if (thread.collapsed.peek()) {
        this.setCollapsed(threadId, false);
      }
      const parent = thread.parentBlipId === null ? undefined : this.blips.get(thread.parentBlipId);
      if (parent === undefined) {
        break;
      }
      parent.needView.value = true;
      threadId = parent.threadId;
    }
  }

  /** `moveUp`: to the previous blip in the order. */
  moveUp(): void {
    this.flush();
    const focused = this.focusedId.peek();
    const previous = focused !== null ? this.traverser.previous(focused) : null;
    if (previous !== null) {
      this.focus(previous);
    }
  }

  /**
   * `moveDown`: to the next blip in the order. Unlike the original, which
   * does nothing without a focus frame, this then starts at the first blip.
   */
  moveDown(): void {
    this.flush();
    const focused = this.focusedId.peek();
    const next = focused !== null ? this.traverser.next(focused) : this.traverser.first();
    if (next !== null) {
      this.focus(next);
    }
  }

  /** `focusNext`: to the next unread blip, else to the previous unread one. */
  focusNext(): void {
    this.flush();
    const focused = this.focusedId.peek();
    if (focused === null) {
      return;
    }
    const next = this.reader.next(focused);
    if (next !== null) {
      this.focus(next);
    } else {
      this.focusPrevious();
    }
  }

  /** `focusPrevious`: to the nearest unread blip before the focus. */
  focusPrevious(): void {
    this.flush();
    const focused = this.focusedId.peek();
    if (focused === null) {
      return;
    }
    const previous = this.reader.previous(focused);
    if (previous !== null) {
      this.focus(previous);
    }
  }

  /**
   * The "Next unread" button of `ViewToolbar`: without a focus frame, the
   * root blip if it is unread, else the first unread blip after it.
   */
  nextUnread(): void {
    this.flush();
    if (this.focusedId.peek() !== null) {
      this.focusNext();
      return;
    }
    const root = this.rootBlip;
    if (root === null) {
      return;
    }
    const wasRead = this.reader.isRead(root);
    this.focus(root);
    if (wasRead) {
      this.focusNext();
    }
  }

  /** The "Recent" button: the most recently modified blip (`selectMostRecentlyModified`). */
  focusMostRecent(): void {
    this.flush();
    let best: string | null = null;
    let bestTime = -Infinity;
    for (const id of this.blipOrder()) {
      const time = this.observable.wavelet.blip(id)?.lastModifiedTime ?? 0;
      // The original keeps the last blip seen for a time.
      if (time >= bestTime) {
        best = id;
        bestTime = time;
      }
    }
    if (best !== null) {
      this.focus(best);
    }
  }

  // ---- editing ----

  /** `ActionsImpl.startEditing`: moves the focus to a blip and edits it. */
  startEditing(blipId: string): void {
    this.flush();
    const blip = this.blips.get(blipId);
    if (blip === undefined) {
      return;
    }
    this.endSession();
    this.focus(blipId, { scroll: true, dom: false });
    this.sessionChanged = false;
    batch(() => {
      blip.needView.value = true;
      blip.editing.value = true;
      this.editingId.value = blipId;
    });
  }

  /** `ActionsImpl.stopEditing`; the keyboard focus returns to the blip. */
  stopEditing(): void {
    const editing = this.editingId.peek();
    this.endSession();
    if (editing !== null) {
      const blip = this.blips.get(editing);
      if (blip !== undefined && blip.focused.peek()) {
        blip.focusRequest.value = { scroll: false, dom: true };
      }
    }
  }

  /**
   * Ends the edit session. If the user changed the blip during it and the
   * blip is kept, the others are told that they have finished editing
   * (`OpenWave.signalDoneEditing`), so that an AI agent waiting for them
   * can answer at once. `signal` is false when the blip is being deleted.
   */
  private endSession(signal = true): void {
    const editing = this.editingId.peek();
    if (editing === null) {
      return;
    }
    const changed = this.sessionChanged;
    this.sessionChanged = false;
    batch(() => {
      const blip = this.blips.get(editing);
      if (blip !== undefined) {
        blip.editing.value = false;
      }
      this.editingId.value = null;
      this.editingView.value = null;
    });
    // A reply the user opened and left without writing anything is
    // discarded, so that clicking a reply box and then elsewhere does not
    // leave an empty blip behind (the original kept it).
    if (this.draftId === editing) {
      this.draftId = null;
      if (this.isBlank(editing)) {
        this.deleteBlip(editing);
        return;
      }
    }
    if (signal && changed && this.blips.has(editing)) {
      this.guard(() => this.wave.signalDoneEditing(this.target));
    }
  }

  /** Edits a blip this user just created by a reply action. */
  private startDraft(blipId: string): void {
    this.startEditing(blipId);
    this.draftId = this.editingId.peek() === blipId ? blipId : null;
  }

  /** No text, no elements other than its lines, and no replies. */
  private isBlank(blipId: string): boolean {
    const blip = this.blips.get(blipId);
    if (blip === undefined || blip.replies.peek().length > 0) {
      return false;
    }
    const xml = this.guard(() => this.wave.blipXml(blipId, this.target));
    if (typeof xml !== "string") {
      return false;
    }
    const withoutLines = xml.replace(/<\/?body>|<line(\s[^>]*)?\/>|<line(\s[^>]*)?><\/line>/g, "");
    return withoutLines.trim() === "";
  }

  // ---- actions ----

  /** Replies to a blip in a new thread below it and edits the reply. */
  reply(blipId: string): void {
    const reply = this.guard(() => this.wave.reply(blipId, this.target));
    if (reply !== undefined) {
      this.startDraft(reply.blipId);
    }
  }

  /**
   * Starts an inline reply at the caret of the blip being edited
   * (`ActionsImpl.reply` with a selection) and edits it.
   */
  inlineReplyAtCaret(): void {
    const blipId = this.editingId.peek();
    const view = blipId === null ? undefined : this.views.get(blipId);
    if (blipId === null || view === undefined) {
      return;
    }
    const selection = view.selection();
    if (selection === null) {
      this.reply(blipId);
      return;
    }
    const reply = this.guard(() => this.wave.inlineReply(blipId, selection.end, this.target));
    if (reply !== undefined) {
      this.startDraft(reply.blipId);
    }
  }

  /** `ActionsImpl.addContinuation`: appends a blip to a thread and edits it. */
  continueThread(threadId: string): void {
    const blipId = this.guard(() => this.wave.appendBlip({ threadId, ...this.target }));
    if (blipId !== undefined) {
      this.startDraft(blipId);
    }
  }

  /** `ActionsImpl.delete`: moves the focus off the blip, then deletes it. */
  deleteBlip(blipId: string): void {
    this.flush();
    if (!this.blips.has(blipId)) {
      return;
    }
    if (this.editingId.peek() === blipId) {
      this.endSession(false);
    }
    if (this.focusedId.peek() === blipId) {
      const next = this.focusFallbacks(blipId)[0];
      if (next !== undefined) {
        this.focus(next);
      }
    }
    this.guard(() => this.wave.deleteBlip(blipId, this.target));
    this.flush();
    this.changed();
  }

  /** `CollapsePresenter.toggle`, remembered in the supplement. */
  toggleCollapsed(threadId: string): void {
    this.setCollapsed(threadId, !this.isCollapsed(threadId));
  }

  setCollapsed(threadId: string, collapsed: boolean): void {
    const thread = this.threads.get(threadId);
    if (thread === undefined || thread.collapsed.peek() === collapsed) {
      return;
    }
    if (collapsed) {
      // The focus frame must not disappear into a collapsed thread.
      const focused = this.focusedId.peek();
      if (focused !== null && this.isInThread(focused, threadId) && thread.parentBlipId !== null) {
        this.focus(thread.parentBlipId, { scroll: false, dom: true });
      }
    }
    thread.collapsed.value = collapsed;
    this.guard(() =>
      this.wave.writeSupplement((s) =>
        s.setThreadState(
          this.waveletId,
          threadId,
          collapsed ? ThreadState.Collapsed : ThreadState.Expanded,
        ),
      ),
    );
  }

  /** Whether a blip is in a thread or in a reply (of a reply...) to one of its blips. */
  isInThread(blipId: string, threadId: string): boolean {
    let current: string | null = this.threadOf(blipId);
    for (let guard = 0; current !== null && guard < 1000; guard++) {
      if (current === threadId) {
        return true;
      }
      const parent = this.parentBlipOf(current);
      current = parent === null ? null : this.threadOf(parent);
    }
    return false;
  }

  addParticipants(addresses: readonly string[]): void {
    for (const address of addresses) {
      this.guard(() => this.wave.addParticipant(address, this.target));
    }
  }

  removeParticipant(address: string): void {
    this.guard(() => this.wave.removeParticipant(address, this.target));
  }

  addTag(tag: string): void {
    const trimmed = tag.trim();
    if (trimmed !== "" && !this.tags.peek().includes(trimmed)) {
      this.guard(() => this.wave.addTag(trimmed, this.target));
    }
  }

  removeTag(tag: string): void {
    this.guard(() => this.wave.removeTag(tag, this.target));
  }

  /** Marks everything in the wave as read. */
  markAllRead(): void {
    this.guard(() => this.wave.markRead());
    this.refreshAllUnread();
    this.changed();
  }

  /** "To archive" / "To inbox". */
  setArchived(archived: boolean): void {
    this.guard(() =>
      this.wave.writeSupplement((s) => {
        if (archived) {
          s.archive();
        } else {
          s.inbox();
        }
      }),
    );
    this.refreshInbox();
    this.changed();
  }

  /** "Mute" / "Unmute": unfollow, or follow and back to the inbox. */
  setFollowed(followed: boolean): void {
    this.guard(() =>
      this.wave.writeSupplement((s) => {
        if (followed) {
          s.follow();
          s.inbox();
        } else {
          s.unfollow();
        }
      }),
    );
    this.refreshInbox();
    this.changed();
  }
}

function sameIds(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i]);
}

function sameReplies(a: readonly ReplyRef[], b: readonly ReplyRef[]): boolean {
  return (
    a.length === b.length &&
    a.every((reply, i) => reply.threadId === b[i]!.threadId && reply.anchored === b[i]!.anchored)
  );
}
