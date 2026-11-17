// An open wave: the live wavelets of one wave with its conversations and
// the viewer's read state, and the edits a user interface or an agent
// makes, each of which is applied locally at once and sent to the server.
//
// Stands where `StageTwo`/`StageThree` of the Apache Wave web client
// (`org.waveprotocol.wave.client`, Apache License 2.0) assemble the
// conversation view, supplement and channel binding of a wave.
//
// Reads are snapshots (`conversation()`), mutations name blips and threads
// by id, and changes, local or remote, are announced by events
// (`observe()`, `supplement.events`, `events`). Locations in a blip are
// item offsets into its document: an element start or end is one item, a
// character one UTF-16 code unit.

import {
  type Conversation,
  type ObservableConversation,
  WaveBasedConversationView,
  type WaveletBasedConversation,
  blips,
  makeWaveletConversational,
  title as titles,
} from "../model/conversation";
import { type DocView, LINE_TAGNAME, type MutableDocument } from "../model/doc";
import type { DocInitialization } from "../model/docop";
import { type IdGeneratorImpl, ParticipantId, type WaveId, type WaveletId } from "../model/id";
import {
  type LiveSupplementedWave,
  type LocalReadStateOptions,
  LocalSupplementedWave,
} from "../model/supplement";
import type { OpBasedWavelet, WaveView } from "../model/wavelet";
import type { Observable } from "../util/events";
import type {
  LiveWaveView,
  LiveWaveViewEvents,
  LiveWaveViewState,
  UnsavedState,
} from "./liveWaveView";

/** A blip and the thread created for it. */
export interface NewReply {
  readonly threadId: string;
  readonly blipId: string;
}

/** A conversation is addressed by its wavelet; the default is the root conversation. */
export interface ConversationTarget {
  waveletId?: WaveletId;
}

export class OpenWave {
  /** The wavelets of the wave. */
  readonly view: WaveView;
  /** The conversations of the wave: snapshot reads, mutation by id, events. */
  readonly conversations: WaveBasedConversationView<IdGeneratorImpl>;
  /**
   * The viewer's read, follow and inbox state, with events. Optimistic as
   * the original client's (`LocalSupplementedWaveImpl`): the blip being read
   * (`startReading`) and blips marked as read stay read while the
   * acknowledgements of the viewer's own edits come in.
   */
  readonly supplement: LocalSupplementedWave;
  /** Life cycle and unsaved-state events. */
  readonly events: Observable<LiveWaveViewEvents>;

  private disposed = false;

  constructor(
    readonly waveId: WaveId,
    /** The channel binding underneath. */
    readonly live: LiveWaveView,
    private readonly idGenerator: IdGeneratorImpl,
    private readonly onClose: (wave: OpenWave) => void,
    onListenerError?: (error: unknown) => void,
    /** The timers and clock of the optimistic read state (tests). */
    readState: LocalReadStateOptions = {},
  ) {
    this.view = live.view;
    this.events = live.events;
    const options = onListenerError !== undefined ? { onListenerError } : {};
    this.conversations = new WaveBasedConversationView(this.view, idGenerator, options);
    this.supplement = new LocalSupplementedWave(this.view, {
      ...options,
      ...readState,
      createUserData: () => live.createWavelet(this.view.userDataId()),
      // Before the wave is open its user-data wavelet has not arrived.
      writable: () => live.state === "open",
    });
  }

  get state(): LiveWaveViewState {
    return this.live.state;
  }

  get unsavedState(): UnsavedState {
    return this.live.unsavedState;
  }

  /** Resolves when the initial wavelets have arrived. */
  whenOpened(): Promise<void> {
    return this.live.whenOpened();
  }

  /** Resolves when the server has committed every local change. */
  whenSaved(): Promise<void> {
    return this.live.whenSaved();
  }

  /** Stops the wave: no more events, nothing more is sent. */
  close(): void {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    this.supplement.dispose();
    this.conversations.dispose();
    this.live.close();
    this.onClose(this);
  }

  // ---- reading ----

  /** The id of the conversation wavelet a target names. */
  conversationWaveletId(target: ConversationTarget = {}): WaveletId {
    return target.waveletId ?? this.view.rootId();
  }

  /** Whether the conversation exists (the wave may be new or not visible). */
  hasConversation(target: ConversationTarget = {}): boolean {
    return this.conversation(target) !== null;
  }

  /** A snapshot of a conversation's structure, or null if there is none. */
  conversation(target: ConversationTarget = {}): Conversation | null {
    return this.conversations.conversation(this.conversationWaveletId(target));
  }

  /**
   * The event source of a conversation, or null if its wavelet is not in
   * view. It is tied to the wavelet object: after `waveletReset` or
   * `conversationAdded`, ask again.
   */
  observe(target: ConversationTarget = {}): ObservableConversation | null {
    return this.conversations.observe(this.conversationWaveletId(target));
  }

  title(target: ConversationTarget = {}): string {
    return this.conversation(target)?.title() ?? "";
  }

  tags(target: ConversationTarget = {}): string[] {
    return this.conversation(target)?.tags() ?? [];
  }

  participants(target: ConversationTarget = {}): readonly ParticipantId[] {
    return this.view.wavelet(this.conversationWaveletId(target))?.participants() ?? [];
  }

  /** The XML of a blip's document, or null if there is no such blip. */
  blipXml(blipId: string, target: ConversationTarget = {}): string | null {
    return this.conversation(target)?.blipContent(blipId)?.toXmlString() ?? null;
  }

  /**
   * The text of a blip, lines separated by `\n`; the text of inline
   * elements other than lines is left out. Null if there is no such blip.
   */
  blipText(blipId: string, target: ConversationTarget = {}): string | null {
    const wavelet = this.view.wavelet(this.conversationWaveletId(target));
    if (wavelet === null || wavelet.blip(blipId) === null) {
      return null;
    }
    const view = wavelet.documentView(blipId);
    const body = blips.body(view);
    if (body === null) {
      return "";
    }
    let text = "";
    let lines = 0;
    for (const node of view.children(body)) {
      if (view.isText(node)) {
        text += view.text(node) ?? "";
      } else if (view.hasTag(node, LINE_TAGNAME)) {
        if (lines++ > 0) {
          text += "\n";
        }
      }
    }
    return text;
  }

  /**
   * The locations between which a blip's text lies: `start` is just after
   * the first line element of the body, `end` the end of the body's
   * content. Null if the blip has no body.
   */
  blipTextRange(
    blipId: string,
    target: ConversationTarget = {},
  ): { start: number; end: number } | null {
    const wavelet = this.view.wavelet(this.conversationWaveletId(target));
    if (wavelet === null || wavelet.blip(blipId) === null) {
      return null;
    }
    const view = wavelet.documentView(blipId);
    const body = blips.body(view);
    if (body === null) {
      return null;
    }
    const first = view.firstChildElement(body);
    const start =
      first !== null && view.hasTag(first, LINE_TAGNAME)
        ? view.locationAfter(first)
        : view.contentStart(body);
    return { start, end: view.contentEnd(body) };
  }

  // ---- editing ----

  private wavelet(target: ConversationTarget): OpBasedWavelet {
    const waveletId = this.conversationWaveletId(target);
    const wavelet = this.view.wavelet(waveletId);
    if (wavelet === null) {
      throw new Error(`wave ${this.waveId} has no wavelet ${waveletId}`);
    }
    return wavelet;
  }

  /** The mutating interface of a conversation. Throws if there is none. */
  conversationMut(target: ConversationTarget = {}): WaveletBasedConversation {
    return this.conversations.conversationMut(this.conversationWaveletId(target));
  }

  /**
   * Creates the root conversation of a new wave: the root wavelet with
   * the viewer as participant, the manifest, and a first blip whose first
   * line is the title. Returns the id of that blip.
   */
  createRootConversation(): string {
    const root = this.live.createWavelet(this.view.rootId());
    makeWaveletConversational(root);
    return this.conversationMut().appendBlip("", titles.emptyDocumentWithTitle());
  }

  /** Creates another (private reply) conversation in the wave. Returns its wavelet id. */
  createConversation(): WaveletId {
    const waveletId = this.idGenerator.newConversationWaveletId();
    makeWaveletConversational(this.live.createWavelet(waveletId));
    return waveletId;
  }

  /**
   * Appends a blip to a thread (default: the root thread), optionally with
   * text. Returns the blip id.
   */
  appendBlip(
    options: { threadId?: string; text?: string; content?: DocInitialization } & ConversationTarget = {},
  ): string {
    const blipId = this.conversationMut(options).appendBlip(
      options.threadId ?? "",
      options.content ?? null,
    );
    if (options.text !== undefined && options.text !== "") {
      this.appendText(blipId, options.text, options);
    }
    return blipId;
  }

  /** Starts a reply thread under a blip with one blip in it. */
  reply(blipId: string, options: { text?: string } & ConversationTarget = {}): NewReply {
    const conversation = this.conversationMut(options);
    const threadId = conversation.addReplyThread(blipId);
    return { threadId, blipId: this.appendBlip({ ...options, threadId }) };
  }

  /** Starts an inline reply thread anchored at a location of a blip, with one blip in it. */
  inlineReply(
    blipId: string,
    location: number,
    options: { text?: string } & ConversationTarget = {},
  ): NewReply {
    const conversation = this.conversationMut(options);
    const threadId = conversation.addInlineReplyThread(blipId, location);
    return { threadId, blipId: this.appendBlip({ ...options, threadId }) };
  }

  /**
   * Tells the other participants (the AI agents, in practice) that the
   * viewer has finished editing in a conversation: one no-op wavelet
   * operation, sent like any other operation (queued while offline). The
   * server's agents then answer at once instead of waiting for the wave
   * to be quiet. It changes no blip and makes nothing unread; like any
   * delta it advances the wavelet's version and last-modified time.
   */
  signalDoneEditing(target: ConversationTarget = {}): void {
    this.wavelet(target).touch();
  }

  /** Deletes a blip with its replies. */
  deleteBlip(blipId: string, target: ConversationTarget = {}): void {
    this.conversationMut(target).deleteBlip(blipId);
  }

  deleteThread(threadId: string, target: ConversationTarget = {}): void {
    this.conversationMut(target).deleteThread(threadId);
  }

  /**
   * Edits a blip's document. Every call on the document handed to `edit`
   * becomes one operation, applied at once. The document object must not
   * be kept.
   */
  editBlip<R>(blipId: string, edit: (doc: MutableDocument) => R, target: ConversationTarget = {}): R {
    return this.conversationMut(target).editBlip(blipId, edit);
  }

  insertText(blipId: string, location: number, text: string, target: ConversationTarget = {}): void {
    this.editBlip(blipId, (doc) => doc.insertText(location, text), target);
  }

  /** Appends text to the end of a blip's body. */
  appendText(blipId: string, text: string, target: ConversationTarget = {}): void {
    const range = this.blipTextRange(blipId, target);
    if (range === null) {
      throw new Error(`blip ${blipId} has no body`);
    }
    this.insertText(blipId, range.end, text, target);
  }

  /** Deletes the items between two locations of a blip. */
  deleteRange(blipId: string, start: number, end: number, target: ConversationTarget = {}): void {
    this.editBlip(blipId, (doc) => doc.deleteRange(start, end), target);
  }

  /** Sets (or with null clears) an annotation over a range of a blip. */
  setAnnotation(
    blipId: string,
    start: number,
    end: number,
    key: string,
    value: string | null,
    target: ConversationTarget = {},
  ): void {
    this.editBlip(blipId, (doc) => doc.setAnnotation(start, end, key, value), target);
  }

  /** Sets an explicit title, which no longer follows the first line of the root blip. */
  setTitle(title: string, target: ConversationTarget = {}): void {
    this.conversationMut(target).setExplicitTitle(title);
  }

  /**
   * Marks the first line or sentence of the root blip as the title, unless
   * the title is explicit. An editor calls this after changing the root
   * blip, as the original's does.
   */
  updateImplicitTitle(target: ConversationTarget = {}): void {
    this.conversationMut(target).updateImplicitTitle();
  }

  addParticipant(participant: ParticipantId | string, target: ConversationTarget = {}): void {
    this.wavelet(target).addParticipant(toParticipant(participant));
  }

  removeParticipant(participant: ParticipantId | string, target: ConversationTarget = {}): void {
    this.wavelet(target).removeParticipant(toParticipant(participant));
  }

  addTag(tag: string, target: ConversationTarget = {}): void {
    this.conversationMut(target).addTag(tag);
  }

  removeTag(tag: string, target: ConversationTarget = {}): boolean {
    return this.conversationMut(target).removeTag(tag);
  }

  // ---- read state (the user-data wavelet) ----

  /** Whether a blip has changes the viewer has not read. */
  isUnread(blipId: string, target: ConversationTarget = {}): boolean {
    return this.supplement.isUnread(this.conversationWaveletId(target), blipId);
  }

  /** The ids of the unread blips of a conversation. */
  unreadBlips(target: ConversationTarget = {}): string[] {
    const conversation = this.conversation(target);
    if (conversation === null) {
      return [];
    }
    return this.supplement.unreadBlips(
      this.conversationWaveletId(target),
      conversation.blipsDepthFirst().map((blip) => blip.id),
    );
  }

  /**
   * The viewer's user-data wavelet, created (and connected) if the wave
   * has none yet.
   */
  private ensureUserData(): void {
    if (this.view.userData() === null) {
      this.live.createWavelet(this.view.userDataId());
    }
  }

  /**
   * Marks a blip as read. It then stays read while the server acknowledges
   * the viewer's own edits of it, until someone else changes it.
   */
  markBlipRead(blipId: string, target: ConversationTarget = {}): void {
    this.supplement.markBlipAsRead(this.conversationWaveletId(target), blipId);
  }

  /**
   * The viewer starts reading a blip: the user interface calls this when a
   * blip gets the focus (the original's `Reader.onFocusMoved`). The blip is
   * marked as read and stays read, also while the viewer edits it, until
   * {@link stopReading} and about a minute beyond, unless someone else
   * changes it after the viewer left. One blip is read at a time: reading
   * another one stops reading the previous.
   */
  startReading(blipId: string, target: ConversationTarget = {}): void {
    const waveletId = this.conversationWaveletId(target);
    const reading = this.supplement.readingBlip();
    if (reading !== null) {
      if (reading.blipId === blipId && reading.waveletId.equals(waveletId)) {
        return;
      }
      this.supplement.stopReading(reading.waveletId, reading.blipId);
    }
    this.supplement.startReading(waveletId, blipId);
  }

  /**
   * The viewer stops reading a blip (the focus left it). Does nothing if
   * that blip is not the one being read.
   */
  stopReading(blipId: string, target: ConversationTarget = {}): void {
    const waveletId = this.conversationWaveletId(target);
    const reading = this.supplement.readingBlip();
    if (reading !== null && reading.blipId === blipId && reading.waveletId.equals(waveletId)) {
      this.supplement.stopReading(waveletId, blipId);
    }
  }

  /** The blip being read, or null. */
  readingBlip(): { waveletId: WaveletId; blipId: string } | null {
    return this.supplement.readingBlip();
  }

  /** Marks everything in the wave as read. */
  markRead(): void {
    this.ensureUserData();
    this.supplement.markAsRead();
  }

  /**
   * A document of the viewer's own user-data wavelet, such as the
   * settings documents kept there; null if the wave has no such wavelet.
   */
  userDataDocument(docId: string): DocView | null {
    return this.view.userData()?.documentView(docId) ?? null;
  }

  /**
   * Edits a document of the viewer's own user-data wavelet, which is
   * created if the wave has none yet. For documents other than the
   * supplement's.
   */
  editUserDataDocument<R>(docId: string, edit: (doc: MutableDocument) => R): R {
    this.ensureUserData();
    const userData = this.view.userData();
    if (userData === null) {
      throw new Error(`wave ${this.waveId} has no user-data wavelet`);
    }
    return userData.editDocument(docId, edit);
  }

  /** Any other change of the supplement (follow, archive, folders, thread state...). */
  writeSupplement<R>(change: Parameters<LiveSupplementedWave["write"]>[0]): R {
    this.ensureUserData();
    return this.supplement.write(change) as R;
  }
}

function toParticipant(participant: ParticipantId | string): ParticipantId {
  return typeof participant === "string" ? ParticipantId.of(participant) : participant;
}
