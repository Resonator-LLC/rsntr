// A read-only snapshot of a conversation.
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::model`), itself derived from
// `org.waveprotocol.wave.model.conversation` (`WaveletBasedConversation`,
// `WaveletBasedConversationThread`, `WaveletBasedConversationBlip`,
// `BlipIterators`) of Apache Wave, licensed under the Apache License,
// Version 2.0.

import type { BlipData, WaveletData } from "../data";
import { DocView } from "../doc";
import type { BootstrapDocument } from "../docop";
import {
  InvalidIdError,
  MANIFEST_DOCUMENT_ID,
  ModernIdSerialiser,
  type ParticipantId,
  TAGS_DOC_ID,
  type WaveletId,
} from "../id";
import { THREAD_INLINE_ANCHOR_ID_ATTR, THREAD_INLINE_ANCHOR_TAGNAME } from "./blips";
import { type AnchorData, Manifest, type ManifestBlip, type ManifestThread } from "./manifest";
import * as tags from "./tags";
import * as title from "./title";

/** A thread of a conversation: a sequence of blips. */
export class ConversationThread {
  constructor(
    /** The thread's id; the empty string for the root thread. */
    readonly id: string,
    /**
     * Whether the manifest marks the thread as inline. Where a thread is
     * shown is decided by its anchor in the parent blip, see
     * {@link Conversation.locateReplyThreads}; this flag only records how
     * the thread was created.
     */
    readonly inline: boolean,
    /** The blips of the thread that have a document, in manifest order. */
    readonly blips: readonly ConversationBlip[],
    /** Location of the thread's element in the manifest document. */
    readonly manifestLocation: number,
  ) {}

  isRoot(): boolean {
    return this.id === "";
  }

  firstBlip(): ConversationBlip | null {
    return this.blips[0] ?? null;
  }
}

/** A blip of a conversation: a manifest entry whose document exists. */
export class ConversationBlip {
  constructor(
    /** The id of the blip, which is also the id of its document. */
    readonly id: string,
    /** The reply threads, inline or not, in manifest order. */
    readonly replyThreads: readonly ConversationThread[],
    /** Location of the blip's element in the manifest document. */
    readonly manifestLocation: number,
  ) {}

  /** The reply thread with an id; the last one if there are several. */
  replyThread(id: string): ConversationThread | null {
    for (let i = this.replyThreads.length - 1; i >= 0; i--) {
      const thread = this.replyThreads[i]!;
      if (thread.id === id) {
        return thread;
      }
    }
    return null;
  }
}

function adaptThread(wavelet: WaveletData, thread: ManifestThread, id: string): ConversationThread {
  const blips: ConversationBlip[] = [];
  for (const blip of thread.blips) {
    const adapted = adaptBlip(wavelet, blip);
    if (adapted !== null) {
      blips.push(adapted);
    }
  }
  return new ConversationThread(id, thread.inline, blips, thread.location);
}

function adaptBlip(wavelet: WaveletData, blip: ManifestBlip): ConversationBlip | null {
  if (blip.id === null || !wavelet.hasDocument(blip.id)) {
    return null;
  }
  const replies: ConversationThread[] = [];
  for (const thread of blip.threads) {
    if (thread.id !== null) {
      replies.push(adaptThread(wavelet, thread, thread.id));
    }
  }
  return new ConversationBlip(blip.id, replies, blip.location);
}

/** A reply thread with the location of its anchor in the parent blip. */
export interface LocatedReplyThread {
  readonly thread: ConversationThread;
  /**
   * `null` if the blip has no anchor for the thread, as for non-inline
   * replies (`Blips.INVALID_INLINE_LOCATION`).
   */
  readonly location: number | null;
}

/**
 * The structure of a conversation as of the moment it was read, with access
 * to the wavelet it lives in (`WaveletBasedConversation` and its threads
 * and blips, read-only).
 *
 * The structure (threads, blips, anchor) is a snapshot; what is read
 * through the wavelet (participants, blip data and content, title, tags,
 * anchor locations) is current. Read a new snapshot after a change.
 *
 * Degraded but safe on malformed manifests, like the original: manifest
 * blips without an id or without a document in the wavelet are left out
 * (with their replies); reply threads without an id are left out too (the
 * original would index them under a null key).
 */
export class Conversation {
  private walked: [ConversationThread, ConversationBlip][] | null = null;

  private constructor(
    /** The wavelet data the conversation lives in. */
    readonly wavelet: WaveletData,
    /**
     * The raw anchor of the manifest. Whether it refers to an existing blip
     * of another conversation is for the conversation view to say.
     */
    readonly anchor: AnchorData,
    readonly rootThread: ConversationThread,
  ) {}

  /**
   * Reads the conversation of a wavelet, or `null` if the wavelet has no
   * conversation manifest.
   */
  static read(wavelet: WaveletData): Conversation | null {
    const doc = wavelet.document(MANIFEST_DOCUMENT_ID);
    if (doc === null) {
      return null;
    }
    const manifest = Manifest.read(doc.content);
    return manifest === null ? null : Conversation.fromManifest(wavelet, manifest);
  }

  static fromManifest(wavelet: WaveletData, manifest: Manifest): Conversation {
    return new Conversation(wavelet, manifest.anchor, adaptThread(wavelet, manifest.rootThread, ""));
  }

  /**
   * The conversation's id: the serialised id of its wavelet
   * (`WaveletBasedConversation.idFor`).
   */
  get id(): string {
    return conversationIdFor(this.wavelet.waveletId);
  }

  get waveletId(): WaveletId {
    return this.wavelet.waveletId;
  }

  get participants(): readonly ParticipantId[] {
    return this.wavelet.participants;
  }

  /**
   * The blips with their threads in the order of the original's object
   * construction: depth first, a blip before its replies.
   */
  private walk(): [ConversationThread, ConversationBlip][] {
    if (this.walked === null) {
      const out: [ConversationThread, ConversationBlip][] = [];
      const visit = (thread: ConversationThread): void => {
        for (const blip of thread.blips) {
          out.push([thread, blip]);
          blip.replyThreads.forEach(visit);
        }
      };
      visit(this.rootThread);
      this.walked = out;
    }
    return this.walked;
  }

  private findLast(blipId: string): [ConversationThread, ConversationBlip] | null {
    const walk = this.walk();
    for (let i = walk.length - 1; i >= 0; i--) {
      if (walk[i]![1].id === blipId) {
        return walk[i]!;
      }
    }
    return null;
  }

  /**
   * The blip with an id. If the manifest lists the id several times, the
   * last entry wins, as in the original.
   */
  blip(id: string): ConversationBlip | null {
    return this.findLast(id)?.[1] ?? null;
  }

  /** The thread a blip is in. */
  threadOf(blipId: string): ConversationThread | null {
    return this.findLast(blipId)?.[0] ?? null;
  }

  /** The thread with an id; the root thread for the empty string. */
  thread(id: string): ConversationThread | null {
    if (id === "") {
      return this.rootThread;
    }
    const walk = this.walk();
    for (let i = walk.length - 1; i >= 0; i--) {
      const thread = walk[i]![1].replyThread(id);
      if (thread !== null) {
        return thread;
      }
    }
    return null;
  }

  /** The blip a reply thread hangs off; `null` for the root thread. */
  parentBlipOf(threadId: string): ConversationBlip | null {
    if (threadId === "") {
      return null;
    }
    const walk = this.walk();
    for (let i = walk.length - 1; i >= 0; i--) {
      if (walk[i]![1].replyThread(threadId) !== null) {
        return walk[i]![1];
      }
    }
    return null;
  }

  /**
   * Whether a blip is the first blip of the root thread
   * (`ConversationBlip.isRoot`).
   */
  isRootBlip(blipId: string): boolean {
    return this.rootThread.firstBlip()?.id === blipId;
  }

  /**
   * All blips, breadth first: the blips of a thread in order, then the
   * blips of their reply threads, thread by thread
   * (`BlipIterators.breadthFirst`).
   */
  blipsBreadthFirst(): ConversationBlip[] {
    const result: ConversationBlip[] = [];
    const threads: ConversationThread[] = [this.rootThread];
    for (let i = 0; i < threads.length; i++) {
      for (const blip of threads[i]!.blips) {
        threads.push(...blip.replyThreads);
        result.push(blip);
      }
    }
    return result;
  }

  /** All blips, depth first: a blip before its replies, in manifest order. */
  blipsDepthFirst(): ConversationBlip[] {
    return this.walk().map(([, blip]) => blip);
  }

  /**
   * The document and metadata of a blip: author, contributors,
   * last-modified time and version, content.
   */
  blipData(blipId: string): BlipData | null {
    return this.wavelet.document(blipId);
  }

  /** The content of a blip. */
  blipContent(blipId: string): BootstrapDocument | null {
    return this.blipData(blipId)?.content ?? null;
  }

  /**
   * The reply threads of a blip with the locations of their anchors in the
   * blip's document, ordered by location, those without anchor last
   * (`locateReplyThreads`). The anchor of a thread is the first `reply`
   * element with the thread's id.
   */
  locateReplyThreads(blip: ConversationBlip): LocatedReplyThread[] {
    const content = this.blipContent(blip.id);
    const anchors = content === null ? new Map<string, number>() : findAnchors(content);
    const located = blip.replyThreads.map((thread) => ({
      thread,
      location: anchors.get(thread.id) ?? null,
    }));
    // Stable, like Collections.sort.
    const key = (reply: LocatedReplyThread): number => reply.location ?? Number.MAX_SAFE_INTEGER;
    return located.sort((a, b) => key(a) - key(b));
  }

  /**
   * The title of the conversation: that of the first blip of the root
   * thread, or the empty string (`TitleHelper.getTitle`).
   */
  title(): string {
    const first = this.rootThread.firstBlip();
    const content = first === null ? null : this.blipContent(first.id);
    return content === null ? "" : title.extractTitle(content);
  }

  /** The tags of the conversation. */
  tags(): string[] {
    const doc = this.wavelet.document(TAGS_DOC_ID);
    return doc === null ? [] : tags.tags(doc.content);
  }
}

/**
 * The id of the conversation in a wavelet: the wavelet id in the modern
 * serialisation, `domain/id` (`WaveletBasedConversation.idFor`).
 */
export function conversationIdFor(waveletId: WaveletId): string {
  try {
    return ModernIdSerialiser.INSTANCE.serialiseWaveletId(waveletId);
  } catch (e) {
    if (e instanceof InvalidIdError) {
      return `${waveletId.domain}/${waveletId.id}`;
    }
    throw e;
  }
}

/**
 * The wavelet of a conversation id (`WaveletBasedConversation.widFor`), or
 * `null` if it is not one.
 */
export function waveletIdFor(conversationId: string): WaveletId | null {
  try {
    return ModernIdSerialiser.INSTANCE.deserialiseWaveletId(conversationId);
  } catch (e) {
    if (e instanceof InvalidIdError) {
      return null;
    }
    throw e;
  }
}

/** The location of the first anchor of each thread id in a blip document. */
export function findAnchors(doc: BootstrapDocument): Map<string, number> {
  const view = new DocView(doc);
  const anchors = new Map<string, number>();
  for (const element of view.descendantElements(view.root())) {
    if (view.hasTag(element, THREAD_INLINE_ANCHOR_TAGNAME)) {
      const threadId = view.attribute(element, THREAD_INLINE_ANCHOR_ID_ATTR);
      if (threadId !== null && !anchors.has(threadId)) {
        anchors.set(threadId, view.location(element));
      }
    }
  }
  return anchors;
}
