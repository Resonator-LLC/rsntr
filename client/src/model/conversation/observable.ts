// Change notifications for a conversation, for local and remote changes
// alike.
//
// Not in the Rust port, which has no listeners. The events follow
// `org.waveprotocol.wave.model.conversation.ObservableConversation.Listener`
// and `AnchorListener`, `ObservableManifest*`, `TitleHelper` and
// `TagsDocument.Listener` of Apache Wave, licensed under the Apache License,
// Version 2.0, with ids in place of objects.

import type { DocOp } from "../docop";
import { MANIFEST_DOCUMENT_ID, type ParticipantId, TAGS_DOC_ID, type WaveletId } from "../id";
import type { OpBasedWavelet } from "../wavelet";
import { Emitter, type Observable, type Unsubscribe } from "../../util/events";
import { THREAD_INLINE_ANCHOR_TAGNAME } from "./blips";
import { type AnchorData, anchorDataEquals } from "./manifest";
import { Conversation, type ConversationThread, conversationIdFor, findAnchors } from "./model";
import { tags as readTags } from "./tags";
import { extractTitle } from "./title";

/**
 * The events of an {@link ObservableConversation}. Blips and threads are
 * named by id (the root thread is `""`); read the current state with
 * {@link ObservableConversation.read}. `local` says whether the change was
 * made through this client's wavelet object or came from the server.
 */
export type ConversationEvents = {
  participantAdded: { participant: ParticipantId; local: boolean; author: ParticipantId };
  participantRemoved: { participant: ParticipantId; local: boolean; author: ParticipantId };
  /** A blip appeared in thread `threadId` at position `index` of its blips. */
  blipAdded: { blipId: string; threadId: string; index: number; local: boolean };
  /** A blip left the conversation; its reply threads were reported before. */
  blipDeleted: { blipId: string; threadId: string; local: boolean };
  /** A reply thread appeared under a blip. `inline` is the manifest's flag. */
  threadAdded: { threadId: string; parentBlipId: string; inline: boolean; local: boolean };
  /**
   * A reply thread has an anchor (`<reply id=threadId/>`) at `location` of
   * its parent blip's document: emitted after `threadAdded` if the anchor
   * is there already, or when the anchor appears later.
   */
  inlineThreadAdded: { threadId: string; parentBlipId: string; location: number; local: boolean };
  /**
   * The anchor of a reply thread disappeared from the parent blip while the
   * thread remains (not in the original). Not emitted when the thread is
   * deleted.
   */
  inlineThreadRemoved: { threadId: string; parentBlipId: string; local: boolean };
  /** A reply thread left the conversation; its blips were reported before. */
  threadDeleted: { threadId: string; parentBlipId: string; local: boolean };
  /**
   * The document of a blip of the conversation was changed by `op`. Not
   * emitted for the operations that precede `blipAdded` or follow
   * `blipDeleted`.
   */
  blipContentChanged: { blipId: string; op: DocOp; local: boolean; author: ParticipantId };
  blipContributorAdded: { blipId: string; contributor: ParticipantId };
  blipContributorRemoved: { blipId: string; contributor: ParticipantId };
  blipTimestampChanged: { blipId: string; oldTime: number; newTime: number };
  /** The title (explicit or implicit) of the conversation changed. */
  titleChanged: { title: string; oldTitle: string };
  /** The tags document changed: tags added and removed, and all tags now. */
  tagsChanged: { added: string[]; removed: string[]; tags: string[] };
  /** The raw anchor of the manifest changed. */
  anchorChanged: { oldAnchor: AnchorData; newAnchor: AnchorData };
  /**
   * The thread structure changed in any way (also in ways without an event
   * of their own, such as a reordering). Emitted last for a change.
   */
  structureChanged: { local: boolean };
};

export interface ObservableConversationOptions {
  /**
   * Receives what a listener throws. Default: the error is rethrown from a
   * microtask, so that it is reported as uncaught.
   */
  onListenerError?: (error: unknown) => void;
}

const NO_ANCHOR: AnchorData = { conversationId: null, blipId: null };

/** The parents of the blips and threads of a structure, by id. */
interface Index {
  /** Blip id to the id of its thread. */
  readonly blips: Map<string, string>;
  /** Reply thread id to the id of its parent blip. */
  readonly threads: Map<string, string>;
  readonly signature: string;
}

function indexOf(root: ConversationThread | null): Index {
  const blips = new Map<string, string>();
  const threads = new Map<string, string>();
  const visit = (thread: ConversationThread): string => {
    const parts: string[] = [];
    for (const blip of thread.blips) {
      // Later entries win, as in `Conversation`.
      blips.set(blip.id, thread.id);
      const replies: string[] = [];
      for (const reply of blip.replyThreads) {
        threads.set(reply.id, blip.id);
        replies.push(`${JSON.stringify(reply.id)}${reply.inline ? "i" : ""}${visit(reply)}`);
      }
      parts.push(`${JSON.stringify(blip.id)}{${replies.join(",")}}`);
    }
    return `[${parts.join(",")}]`;
  };
  const signature = root === null ? "" : visit(root);
  return { blips, threads, signature };
}

/** Whether an operation can add, remove or re-target an anchor. */
function touchesAnchors(op: DocOp): boolean {
  for (const c of op.components) {
    switch (c.type) {
      case "elementStart":
      case "deleteElementStart":
        if (c.tag === THREAD_INLINE_ANCHOR_TAGNAME) {
          return true;
        }
        break;
      case "replaceAttributes":
      case "updateAttributes":
        return true;
      default:
        break;
    }
  }
  return false;
}

type Emit = () => void;

/**
 * Watches the conversation of one wavelet and reports its changes as typed,
 * id-based events ({@link ConversationEvents}), whether they were made
 * locally or arrived through `OpBasedWavelet.consumeRemote`.
 *
 * Structure events are found by comparing the conversation's structure
 * before and after each change of the manifest (or creation of a
 * document), not by interpreting operations: for one change, first the
 * deletions bottom-up (the blips of a thread before the thread, the reply
 * threads of a blip before the blip), then the additions top-down in
 * manifest order, then `anchorChanged`, `titleChanged` and
 * `structureChanged`. A blip or thread that moved to another parent is
 * reported as deleted and added.
 *
 * The wavelet need not have a manifest: without one the conversation is
 * empty, and a manifest appearing or disappearing is reported as additions
 * or deletions.
 *
 * Call {@link dispose} when done; it unsubscribes from the wavelet.
 */
export class ObservableConversation {
  readonly events: Observable<ConversationEvents>;
  /** The wavelet being watched. */
  readonly wavelet: OpBasedWavelet;

  private readonly emitter: Emitter<ConversationEvents>;
  private subscriptions: Unsubscribe[] = [];
  private root: ConversationThread | null = null;
  private index: Index = indexOf(null);
  private anchor: AnchorData = NO_ANCHOR;
  private rootBlipId: string | null = null;
  private currentTitle = "";
  private currentTags: string[] = [];
  /** The reply threads that have an anchor in their parent blip. */
  private readonly anchored = new Set<string>();
  /**
   * The blips added by the wavelet operation being reported: its remaining
   * wavelet events are about a blip the listeners have only just learnt of.
   */
  private readonly fresh = new Set<string>();
  private disposed = false;

  constructor(wavelet: OpBasedWavelet, options: ObservableConversationOptions = {}) {
    this.wavelet = wavelet;
    this.emitter = new Emitter<ConversationEvents>(
      options.onListenerError ??
        ((error) =>
          queueMicrotask(() => {
            throw error;
          })),
    );
    this.events = this.emitter;
    // The initial state, without events.
    this.refreshStructure(false);
    this.refreshTags();
    const e = wavelet.events;
    const emitter = this.emitter;
    this.subscriptions = [
      e.on("participantAdded", (event) => emitter.emit("participantAdded", event)),
      e.on("participantRemoved", (event) => emitter.emit("participantRemoved", event)),
      e.on("documentChanged", (event) => this.onDocumentChanged(event)),
      e.on("blipContributorAdded", ({ docId, contributor }) => {
        if (this.isKnownBlip(docId)) {
          emitter.emit("blipContributorAdded", { blipId: docId, contributor });
        }
      }),
      e.on("blipContributorRemoved", ({ docId, contributor }) => {
        if (this.isKnownBlip(docId)) {
          emitter.emit("blipContributorRemoved", { blipId: docId, contributor });
        }
      }),
      e.on("blipTimestampChanged", ({ docId, oldTime, newTime }) => {
        if (this.isKnownBlip(docId)) {
          emitter.emit("blipTimestampChanged", { blipId: docId, oldTime, newTime });
        }
      }),
      // The last event of every wavelet operation.
      e.on("operation", () => this.fresh.clear()),
    ];
  }

  private isKnownBlip(docId: string): boolean {
    return this.index.blips.has(docId) && !this.fresh.has(docId);
  }

  /** The conversation's id: the serialised id of its wavelet. */
  get conversationId(): string {
    return conversationIdFor(this.wavelet.waveletId());
  }

  get waveletId(): WaveletId {
    return this.wavelet.waveletId();
  }

  /** A snapshot of the conversation now, or `null` if there is no manifest. */
  read(): Conversation | null {
    return Conversation.read(this.wavelet.data());
  }

  /** Whether the wavelet has a conversation manifest. */
  hasConversation(): boolean {
    return this.root !== null;
  }

  /** The title as of the last change. */
  title(): string {
    return this.currentTitle;
  }

  /** The tags as of the last change. */
  tags(): readonly string[] {
    return this.currentTags;
  }

  /** Stops watching the wavelet and drops all listeners. */
  dispose(): void {
    this.disposed = true;
    for (const unsubscribe of this.subscriptions) {
      unsubscribe();
    }
    this.subscriptions = [];
    this.emitter.clear();
  }

  isDisposed(): boolean {
    return this.disposed;
  }

  private onDocumentChanged(event: {
    docId: string;
    op: DocOp;
    local: boolean;
    author: ParticipantId;
    created: boolean;
  }): void {
    const { docId, op, local, author } = event;
    const wasBlip = this.index.blips.has(docId);
    const emits: Emit[] = [];
    if (docId === MANIFEST_DOCUMENT_ID || event.created) {
      // A manifest entry counts as a blip once its document exists.
      emits.push(...this.refreshStructure(local));
    }
    if (docId === TAGS_DOC_ID) {
      emits.push(...this.refreshTags());
    }
    if (!wasBlip && this.index.blips.has(docId)) {
      this.fresh.add(docId);
    }
    if (wasBlip && this.index.blips.has(docId)) {
      const emitter = this.emitter;
      emits.push(() =>
        emitter.emit("blipContentChanged", { blipId: docId, op, local, author }),
      );
      if (touchesAnchors(op)) {
        emits.push(...this.refreshAnchors(docId, local));
      }
      if (docId === this.rootBlipId) {
        emits.push(...this.refreshTitle());
      }
    }
    // All state is up to date before the first listener runs.
    for (const emit of emits) {
      emit();
    }
  }

  private refreshTitle(): Emit[] {
    const content =
      this.rootBlipId === null ? null : this.wavelet.data().document(this.rootBlipId)?.content;
    const title = content == null ? "" : extractTitle(content);
    const oldTitle = this.currentTitle;
    if (title === oldTitle) {
      return [];
    }
    this.currentTitle = title;
    const emitter = this.emitter;
    return [() => emitter.emit("titleChanged", { title, oldTitle })];
  }

  private refreshTags(): Emit[] {
    const doc = this.wavelet.data().document(TAGS_DOC_ID);
    const tags = doc === null ? [] : readTags(doc.content);
    const old = this.currentTags;
    // Multiset difference: a tag may be listed more than once.
    const remaining = [...old];
    const added: string[] = [];
    for (const tag of tags) {
      const at = remaining.indexOf(tag);
      if (at >= 0) {
        remaining.splice(at, 1);
      } else {
        added.push(tag);
      }
    }
    const removed = remaining;
    const same = tags.length === old.length && tags.every((tag, i) => tag === old[i]);
    this.currentTags = tags;
    if (same) {
      return [];
    }
    const emitter = this.emitter;
    return [() => emitter.emit("tagsChanged", { added, removed, tags: [...tags] })];
  }

  /** Re-reads the anchors of one blip's reply threads. */
  private refreshAnchors(blipId: string, local: boolean): Emit[] {
    const blip = this.read()?.blip(blipId) ?? null;
    const content = this.wavelet.data().document(blipId)?.content;
    if (blip === null || content == null) {
      return [];
    }
    const emitter = this.emitter;
    const anchors = findAnchors(content);
    const emits: Emit[] = [];
    const seen = new Set<string>();
    for (const thread of blip.replyThreads) {
      const threadId = thread.id;
      if (seen.has(threadId) || this.index.threads.get(threadId) !== blipId) {
        continue;
      }
      seen.add(threadId);
      const location = anchors.get(threadId);
      if (location !== undefined && !this.anchored.has(threadId)) {
        this.anchored.add(threadId);
        emits.push(() =>
          emitter.emit("inlineThreadAdded", { threadId, parentBlipId: blipId, location, local }),
        );
      } else if (location === undefined && this.anchored.has(threadId)) {
        this.anchored.delete(threadId);
        emits.push(() =>
          emitter.emit("inlineThreadRemoved", { threadId, parentBlipId: blipId, local }),
        );
      }
    }
    return emits;
  }

  /**
   * Re-reads the structure, updates the state and returns the events of
   * the difference.
   */
  private refreshStructure(local: boolean): Emit[] {
    const emitter = this.emitter;
    const data = this.wavelet.data();
    const conversation = Conversation.read(data);
    const oldRoot = this.root;
    const newRoot = conversation?.rootThread ?? null;
    const old = this.index;
    const now = indexOf(newRoot);
    const emits: Emit[] = [];

    // A blip or thread is retained if it has the same parent before and
    // after, and that parent is retained.
    const blipMemo = new Map<string, boolean>();
    const threadMemo = new Map<string, boolean>();
    const blipRetained = (id: string): boolean => {
      let result = blipMemo.get(id);
      if (result === undefined) {
        blipMemo.set(id, false);
        const thread = old.blips.get(id);
        result = thread !== undefined && thread === now.blips.get(id) && threadRetained(thread);
        blipMemo.set(id, result);
      }
      return result;
    };
    const threadRetained = (id: string): boolean => {
      if (id === "") {
        return oldRoot !== null && newRoot !== null;
      }
      let result = threadMemo.get(id);
      if (result === undefined) {
        threadMemo.set(id, false);
        const blip = old.threads.get(id);
        result = blip !== undefined && blip === now.threads.get(id) && blipRetained(blip);
        threadMemo.set(id, result);
      }
      return result;
    };

    // Deletions, bottom-up.
    const deletedBlips = new Set<string>();
    const deletedThreads = new Set<string>();
    const visitOld = (thread: ConversationThread): void => {
      for (const blip of thread.blips) {
        const blipId = blip.id;
        if (old.blips.get(blipId) !== thread.id) {
          continue;
        }
        for (const reply of blip.replyThreads) {
          const threadId = reply.id;
          if (old.threads.get(threadId) !== blipId) {
            continue;
          }
          visitOld(reply);
          if (!threadRetained(threadId) && !deletedThreads.has(threadId)) {
            deletedThreads.add(threadId);
            emits.push(() =>
              emitter.emit("threadDeleted", { threadId, parentBlipId: blipId, local }),
            );
          }
        }
        if (!blipRetained(blipId) && !deletedBlips.has(blipId)) {
          deletedBlips.add(blipId);
          const threadId = thread.id;
          emits.push(() => emitter.emit("blipDeleted", { blipId, threadId, local }));
        }
      }
    };
    if (oldRoot !== null) {
      visitOld(oldRoot);
    }
    for (const threadId of deletedThreads) {
      this.anchored.delete(threadId);
    }

    // Additions, top-down in manifest order.
    const addedBlips = new Set<string>();
    const addedThreads = new Set<string>();
    const visitNew = (thread: ConversationThread): void => {
      thread.blips.forEach((blip, index) => {
        const blipId = blip.id;
        if (now.blips.get(blipId) !== thread.id) {
          return;
        }
        if (!blipRetained(blipId) && !addedBlips.has(blipId)) {
          addedBlips.add(blipId);
          const threadId = thread.id;
          emits.push(() => emitter.emit("blipAdded", { blipId, threadId, index, local }));
        }
        let anchors: Map<string, number> | null = null;
        for (const reply of blip.replyThreads) {
          const threadId = reply.id;
          if (now.threads.get(threadId) !== blipId) {
            continue;
          }
          if (!threadRetained(threadId) && !addedThreads.has(threadId)) {
            addedThreads.add(threadId);
            const inline = reply.inline;
            emits.push(() =>
              emitter.emit("threadAdded", { threadId, parentBlipId: blipId, inline, local }),
            );
            if (anchors === null) {
              const content = data.document(blipId)?.content;
              anchors = content == null ? new Map() : findAnchors(content);
            }
            const location = anchors.get(threadId);
            if (location !== undefined) {
              this.anchored.add(threadId);
              emits.push(() =>
                emitter.emit("inlineThreadAdded", {
                  threadId,
                  parentBlipId: blipId,
                  location,
                  local,
                }),
              );
            }
          }
          visitNew(reply);
        }
      });
    };
    if (newRoot !== null) {
      visitNew(newRoot);
    }

    this.root = newRoot;
    this.index = now;
    this.rootBlipId = newRoot?.firstBlip()?.id ?? null;

    const oldAnchor = this.anchor;
    const newAnchor = conversation?.anchor ?? NO_ANCHOR;
    this.anchor = newAnchor;
    if (!anchorDataEquals(oldAnchor, newAnchor)) {
      emits.push(() => emitter.emit("anchorChanged", { oldAnchor, newAnchor }));
    }
    emits.push(...this.refreshTitle());
    if (old.signature !== now.signature || (oldRoot === null) !== (newRoot === null)) {
      emits.push(() => emitter.emit("structureChanged", { local }));
    }
    return emits;
  }
}
