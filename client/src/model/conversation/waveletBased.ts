// Mutation of a conversation through wavelet operations
// (`WaveletBasedConversation`, `WaveletBasedConversationThread`,
// `WaveletBasedConversationBlip`).
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::wavelet_based`), itself derived from
// `org.waveprotocol.wave.model.conversation` of Apache Wave, licensed under
// the Apache License, Version 2.0.

import type { WaveletData } from "../data";
import { DocError, type MutableDocument, Nindo, type NodeId } from "../doc";
import type { DocInitialization } from "../docop";
import { MANIFEST_DOCUMENT_ID, type ParticipantId, TAGS_DOC_ID, util as idUtil } from "../id";
import type { OpBasedWavelet } from "../wavelet";
import * as blips from "./blips";
import { ConversationError } from "./error";
import * as manifest from "./manifest";
import { Conversation, type ConversationBlip, type ConversationThread } from "./model";
import * as tags from "./tags";
import * as title from "./title";

/**
 * The source of ids for new blips and threads.
 *
 * A new reply thread takes the id that the next new blip will get, without
 * consuming it (the original's `createThreadId` is `IdGenerator.peekBlipId`),
 * so `peekBlipId` must return what the next `newBlipId` will return.
 * `IdGeneratorImpl` is one.
 */
export interface BlipIdGenerator {
  newBlipId(): string;
  peekBlipId(): string;
}

/**
 * Whether a wavelet has a conversation manifest as the first element of its
 * manifest document (`WaveletBasedConversation.waveletHasConversation`).
 */
export function waveletHasConversation(wavelet: WaveletData): boolean {
  const doc = wavelet.document(MANIFEST_DOCUMENT_ID);
  return doc !== null && manifest.documentHasManifest(doc.content);
}

/**
 * Gives a wavelet an empty conversation manifest
 * (`WaveletBasedConversation.makeWaveletConversational`). Throws
 * `DocError` if its manifest document has any content.
 */
export function makeWaveletConversational(wavelet: OpBasedWavelet): void {
  wavelet.editDocument(MANIFEST_DOCUMENT_ID, manifest.initialiseDocumentManifest);
}

/** The manifest of a wavelet, or `null` if it has none. */
export function readManifest(wavelet: WaveletData): manifest.Manifest | null {
  const doc = wavelet.document(MANIFEST_DOCUMENT_ID);
  return doc === null ? null : manifest.Manifest.read(doc.content);
}

function elementAt(doc: MutableDocument, location: number): NodeId {
  const element = doc.view().elementStartingAt(location);
  if (element === null) {
    throw DocError.invalidNode();
  }
  return element;
}

/**
 * A conversation in a wavelet, for mutation. Every mutation is made through
 * the wavelet as the operations the original model emits for it.
 *
 * Threads and blips are named by id; the root thread's id is the empty
 * string. The structure is read afresh from the manifest for every call
 * ({@link read} gives the current snapshot), so there are no stale handles:
 * a deleted blip or thread is simply not found, also when it was deleted by
 * a remote operation.
 *
 * Where the original iterates the reply threads of a blip or the blips of a
 * thread in hash-map order while deleting them, this goes in manifest
 * order.
 *
 * Methods throw {@link ConversationError}, and `DocError`, `WaveletError`
 * and `WaveletOperationError` from the layers below.
 */
export class WaveletBasedConversation {
  /** The wavelet the conversation lives in. */
  readonly wavelet: OpBasedWavelet;
  private readonly idGenerator: BlipIdGenerator;

  /**
   * The conversation of a wavelet. Throws {@link ConversationError}
   * `"noManifest"` if the wavelet has no manifest.
   */
  constructor(wavelet: OpBasedWavelet, idGenerator: BlipIdGenerator) {
    if (Conversation.read(wavelet.data()) === null) {
      throw ConversationError.noManifest();
    }
    this.wavelet = wavelet;
    this.idGenerator = idGenerator;
  }

  /** The current structure of the conversation. */
  read(): Conversation {
    const conversation = Conversation.read(this.wavelet.data());
    if (conversation === null) {
      throw ConversationError.noManifest();
    }
    return conversation;
  }

  private thread(threadId: string): ConversationThread {
    const thread = this.read().thread(threadId);
    if (thread === null) {
      throw ConversationError.noSuchThread(threadId);
    }
    return thread;
  }

  private blip(blipId: string): ConversationBlip {
    const blip = this.read().blip(blipId);
    if (blip === null) {
      throw ConversationError.noSuchBlip(blipId);
    }
    return blip;
  }

  private editManifest<R>(edit: (doc: MutableDocument) => R): R {
    return this.wavelet.editDocument(MANIFEST_DOCUMENT_ID, edit);
  }

  /** Edits the content of a blip. */
  editBlip<R>(blipId: string, edit: (doc: MutableDocument) => R): R {
    this.blip(blipId);
    return this.wavelet.editDocument(blipId, edit);
  }

  /**
   * Edits a data document of the conversation's wavelet (`getDataDocument`):
   * any document but blips and the manifest.
   */
  editDataDocument<R>(name: string, edit: (doc: MutableDocument) => R): R {
    if (idUtil.isBlipId(name)) {
      throw ConversationError.invalidArgument(
        `Cannot fetch blip document ${name} as a data document`,
      );
    }
    if (name === MANIFEST_DOCUMENT_ID) {
      throw ConversationError.invalidArgument(
        "Cannot fetch conversation manifest as a data document",
      );
    }
    return this.wavelet.editDocument(name, edit);
  }

  /**
   * Creates the document of a new blip (`ComponentHelper.createBlip`): with
   * `content`, or else with an empty body.
   */
  private createBlip(content: DocInitialization | null): string {
    const blipId = this.idGenerator.newBlipId();
    if (this.wavelet.hasDocument(blipId)) {
      throw new ConversationError({ kind: "blipExists", blipId });
    }
    this.wavelet.editDocument(blipId, (doc) => {
      if (content !== null) {
        doc.consume(Nindo.fromDocOp(content.asOp(), false));
      } else {
        doc.insertXml(doc.size(), blips.initialContent());
      }
    });
    return blipId;
  }

  /**
   * Appends a new blip to a thread and returns its id
   * (`ConversationThread.appendBlip`), with an empty body
   * (`<body><line/></body>`) or the given content
   * (`appendBlip(DocInitialization)`). The blip's document is created
   * first, then its manifest entry.
   */
  appendBlip(threadId: string, content: DocInitialization | null = null): string {
    const thread = this.thread(threadId);
    const blipId = this.createBlip(content);
    this.editManifest((doc) => {
      doc.createChildElement(elementAt(doc, thread.manifestLocation), manifest.BLIP_TAG, [
        [manifest.BLIP_ID_ATTR, blipId],
      ]);
    });
    return blipId;
  }

  /** The same as {@link appendBlip} with content. */
  appendBlipWithContent(threadId: string, content: DocInitialization | null): string {
    return this.appendBlip(threadId, content);
  }

  /**
   * Inserts a new blip into a thread before the blip `successorId` of that
   * thread and returns its id (`ConversationThread.insertBlip`).
   */
  insertBlip(threadId: string, successorId: string): string {
    const thread = this.thread(threadId);
    let location: number | null = null;
    for (const blip of thread.blips) {
      if (blip.id === successorId) {
        location = blip.manifestLocation;
      }
    }
    if (location === null) {
      throw ConversationError.invalidArgument(
        `Can't insert blip before blip ${successorId} not from this thread`,
      );
    }
    const at = location;
    const blipId = this.createBlip(null);
    this.editManifest((doc) => {
      doc.createElement(at, manifest.BLIP_TAG, [[manifest.BLIP_ID_ATTR, blipId]]);
    });
    return blipId;
  }

  private appendReply(blip: ConversationBlip, threadId: string, inline: boolean): void {
    this.editManifest((doc) => {
      const attributes: [string, string][] = [[manifest.THREAD_ID_ATTR, threadId]];
      if (inline) {
        attributes.push([manifest.INLINE_ATTR, "true"]);
      }
      doc.createChildElement(
        elementAt(doc, blip.manifestLocation),
        manifest.THREAD_TAG,
        attributes,
      );
    });
  }

  /**
   * Adds an empty, non-inline reply thread to a blip and returns its id
   * (`ConversationBlip.addReplyThread()`).
   *
   * The thread's id is the id the generator will give out next, without
   * consuming it (`peekBlipId`), so the first blip appended to the thread
   * normally gets the same id as the thread.
   */
  addReplyThread(blipId: string): string {
    const blip = this.blip(blipId);
    const threadId = this.idGenerator.peekBlipId();
    this.appendReply(blip, threadId, false);
    return threadId;
  }

  /**
   * Adds an empty inline reply thread anchored at a location of the blip's
   * document and returns its id (`ConversationBlip.addReplyThread(int)`):
   * first a `reply` anchor element is inserted into the blip, then the
   * thread is added to the manifest.
   */
  addInlineReplyThread(blipId: string, location: number): string {
    const blip = this.blip(blipId);
    const threadId = this.idGenerator.peekBlipId();
    this.wavelet.editDocument(blipId, (doc) => {
      doc.createElement(location, blips.THREAD_INLINE_ANCHOR_TAGNAME, [
        [blips.THREAD_INLINE_ANCHOR_ID_ATTR, threadId],
      ]);
    });
    this.appendReply(blip, threadId, true);
    return threadId;
  }

  /**
   * Deletes anchors from a blip's document, last first: those of one
   * thread, or all (`clearInlineReplyAnchors`,
   * `clearAllInlineReplyAnchors`).
   */
  private clearInlineReplyAnchors(blipId: string, threadId: string | null): void {
    const view = this.wavelet.documentView(blipId);
    const locations = view
      .descendantElements(view.root())
      .filter(
        (el) =>
          view.hasTag(el, blips.THREAD_INLINE_ANCHOR_TAGNAME) &&
          (threadId === null || view.attribute(el, blips.THREAD_INLINE_ANCHOR_ID_ATTR) === threadId),
      )
      .map((el) => view.location(el));
    if (locations.length === 0) {
      return;
    }
    locations.reverse();
    this.wavelet.editDocument(blipId, (doc) => {
      for (const location of locations) {
        // Deleting from the back leaves earlier locations valid.
        doc.deleteNode(elementAt(doc, location));
      }
    });
  }

  private deleteManifestElement(location: number): void {
    this.editManifest((doc) => doc.deleteNode(elementAt(doc, location)));
  }

  /**
   * The current state of the thread whose manifest element is at a
   * location. Deleting things inside an element does not move it.
   */
  private threadAt(location: number): ConversationThread {
    const find = (thread: ConversationThread): ConversationThread | null => {
      if (thread.manifestLocation === location) {
        return thread;
      }
      for (const blip of thread.blips) {
        for (const reply of blip.replyThreads) {
          const found = find(reply);
          if (found !== null) {
            return found;
          }
        }
      }
      return null;
    };
    const found = find(this.read().rootThread);
    if (found === null) {
      throw ConversationError.noSuchThread(`at ${location}`);
    }
    return found;
  }

  private blipAt(location: number): ConversationBlip {
    const find = (thread: ConversationThread): ConversationBlip | null => {
      for (const blip of thread.blips) {
        if (blip.manifestLocation === location) {
          return blip;
        }
        for (const reply of blip.replyThreads) {
          const found = find(reply);
          if (found !== null) {
            return found;
          }
        }
      }
      return null;
    };
    const found = find(this.read().rootThread);
    if (found === null) {
      throw ConversationError.noSuchBlip(`at ${location}`);
    }
    return found;
  }

  /**
   * `WaveletBasedConversationThread.deleteBlips`: deletes every blip of the
   * thread, keeping the thread.
   */
  private deleteBlipsOfThread(threadLocation: number): void {
    // Each deletion removes the first remaining blip.
    for (;;) {
      const blip = this.threadAt(threadLocation).firstBlip();
      if (blip === null) {
        return;
      }
      this.deleteBlipFromThread(blip);
    }
  }

  /**
   * `WaveletBasedConversationThread.deleteBlip` without deleting the
   * thread: deletes the blip's reply threads, all anchors left in it, its
   * manifest entry and its content.
   */
  private deleteBlipFromThread(blip: ConversationBlip): void {
    const location = blip.manifestLocation;
    // deleteThreads: unlike deleteThread, anchors are cleared once at the
    // end, all of them.
    for (;;) {
      const reply = this.blipAt(location).replyThreads[0];
      if (reply === undefined) {
        break;
      }
      this.deleteBlipsOfThread(reply.manifestLocation);
      this.deleteManifestElement(reply.manifestLocation);
    }
    this.clearInlineReplyAnchors(blip.id, null);
    this.deleteManifestElement(location);
    // clearContent
    if (this.wavelet.documentView(blip.id).size() !== 0) {
      this.wavelet.editDocument(blip.id, (doc) => doc.emptyElement(doc.view().root()));
    }
  }

  /**
   * `WaveletBasedConversationBlip.deleteThread`: deletes the thread's
   * blips, its manifest entry and its anchors in the parent blip.
   */
  private deleteReplyThread(parentBlipId: string, thread: ConversationThread): void {
    this.deleteBlipsOfThread(thread.manifestLocation);
    this.deleteManifestElement(thread.manifestLocation);
    this.clearInlineReplyAnchors(parentBlipId, thread.id);
  }

  /**
   * Deletes a blip with all its replies (`ConversationBlip.delete`): its
   * reply threads one by one, then its manifest entry, then its content
   * (the document stays, empty). If that leaves a non-root thread without
   * blips, the thread is deleted too.
   */
  deleteBlip(blipId: string): void {
    const conversation = this.read();
    const blip = conversation.blip(blipId);
    const thread = conversation.threadOf(blipId);
    if (blip === null || thread === null) {
      throw ConversationError.noSuchBlip(blipId);
    }
    const parentBlipId = conversation.parentBlipOf(thread.id)?.id ?? null;
    const location = blip.manifestLocation;
    for (;;) {
      const reply = this.blipAt(location).replyThreads[0];
      if (reply === undefined) {
        break;
      }
      this.deleteReplyThread(blipId, reply);
    }
    this.deleteBlipFromThread(blip);
    // deleteSelfIfEmpty
    if (parentBlipId !== null) {
      const now = this.threadAt(thread.manifestLocation);
      if (now.blips.length === 0) {
        this.deleteReplyThread(parentBlipId, now);
      }
    }
  }

  /**
   * Deletes a thread with all its blips (`ConversationThread.delete`). The
   * root thread itself stays; only its blips go.
   */
  deleteThread(threadId: string): void {
    const conversation = this.read();
    const thread = conversation.thread(threadId);
    if (thread === null) {
      throw ConversationError.noSuchThread(threadId);
    }
    const parentBlipId = conversation.parentBlipOf(threadId)?.id ?? null;
    if (parentBlipId === null) {
      this.deleteBlipsOfThread(thread.manifestLocation);
    } else {
      this.deleteReplyThread(parentBlipId, thread);
    }
  }

  /**
   * Deletes the conversation: all its blips and then the manifest
   * (`Conversation.delete`). This object is of no use afterwards.
   */
  delete(): void {
    this.deleteBlipsOfThread(this.read().rootThread.manifestLocation);
    this.editManifest(manifest.deleteManifest);
  }

  /**
   * Sets or clears (`null`) the anchor: the id of another conversation and
   * of a blip in it (`setAnchor`). Both manifest attributes are written in
   * one operation, whatever they were.
   */
  setAnchor(anchor: { conversationId: string; blipId: string } | null): void {
    this.editManifest((doc) => {
      const top = manifest.manifestElement(doc.view());
      if (top === null) {
        throw DocError.invalidNode();
      }
      doc.updateElementAttributes(top, [
        [manifest.ANCHOR_WAVELET_ATTR, anchor?.conversationId ?? null],
        [manifest.ANCHOR_BLIP_ATTR, anchor?.blipId ?? null],
      ]);
    });
  }

  addParticipant(participant: ParticipantId): void {
    this.wavelet.addParticipant(participant);
  }

  removeParticipant(participant: ParticipantId): void {
    this.wavelet.removeParticipant(participant);
  }

  /** The first blip of the root thread, which carries the title. */
  private rootBlipId(): string {
    const blip = this.read().rootThread.firstBlip();
    if (blip === null) {
      throw ConversationError.noSuchBlip("root blip");
    }
    return blip.id;
  }

  /** Sets an explicit title on the root blip (`TitleHelper.setExplicitTitle`). */
  setExplicitTitle(newTitle: string): void {
    this.wavelet.editDocument(this.rootBlipId(), (doc) => title.setExplicitTitle(doc, newTitle));
  }

  /**
   * Marks the first line or sentence of the root blip as the title, unless
   * it has an explicit one (`TitleHelper.maybeFindAndSetImplicitTitle`).
   */
  updateImplicitTitle(): void {
    this.wavelet.editDocument(this.rootBlipId(), title.maybeFindAndSetImplicitTitle);
  }

  /** Appends a tag to the conversation's tags document. */
  addTag(tag: string): void {
    this.wavelet.editDocument(TAGS_DOC_ID, (doc) => tags.addTag(doc, tag));
  }

  /** Removes the first tag with a name. Returns whether there was one. */
  removeTag(tag: string): boolean {
    return this.wavelet.editDocument(TAGS_DOC_ID, (doc) => tags.deleteTag(doc, tag));
  }
}
