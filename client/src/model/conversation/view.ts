// The conversations of a wave (`WaveBasedConversationView`).
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::view`), itself derived from
// `org.waveprotocol.wave.model.conversation.WaveBasedConversationView` and
// `ObservableConversationView` of Apache Wave, licensed under the Apache
// License, Version 2.0. The events are not in the Rust port.

import {
  EscapeError,
  type IdGenerator,
  InvalidIdError,
  MANIFEST_DOCUMENT_ID,
  type WaveletId,
  util as idUtil,
} from "../id";
import type { OpBasedWavelet, OpBasedWaveletOptions, WaveView } from "../wavelet";
import { Emitter, type Observable, type Unsubscribe } from "../../util/events";
import { ConversationError } from "./error";
import { Conversation, conversationIdFor, waveletIdFor } from "./model";
import { ObservableConversation } from "./observable";
import {
  type BlipIdGenerator,
  WaveletBasedConversation,
  makeWaveletConversational,
  waveletHasConversation,
} from "./waveletBased";

export type ConversationViewEvents = {
  /**
   * A conversation appeared: a conversational wavelet with a manifest was
   * added to the wave view, or a conversational wavelet got its manifest.
   */
  conversationAdded: { conversationId: string; waveletId: WaveletId };
  /**
   * A conversation went away: its wavelet left the wave view (also when it
   * is replaced by a new snapshot; then `conversationAdded` follows), or
   * its manifest was deleted.
   */
  conversationRemoved: { conversationId: string; waveletId: WaveletId };
};

export interface ConversationViewOptions {
  /**
   * Receives what a listener of the view or of its observable
   * conversations throws. Default: the error is rethrown from a microtask.
   */
  onListenerError?: (error: unknown) => void;
}

interface Entry {
  readonly wavelet: OpBasedWavelet;
  readonly unsubscribe: Unsubscribe;
  present: boolean;
  observable: ObservableConversation | null;
}

function isConversational(id: WaveletId): boolean {
  try {
    return idUtil.isConversationalId(id);
  } catch (e) {
    if (e instanceof EscapeError) {
      return false;
    }
    throw e;
  }
}

function notConversational(waveletId: WaveletId): ConversationError {
  return ConversationError.invalidArgument(`Wavelet id ${waveletId} is not conversational`);
}

/**
 * The conversations of a wave: its conversational wavelets (`conv+...`)
 * that have a manifest.
 *
 * Works on a {@link WaveView} (not owned: the client keeps adding wavelets
 * and feeding them remote operations) with the id generator used for new
 * wavelets, blips and threads. Reads give snapshots ({@link Conversation});
 * mutations go through {@link WaveletBasedConversation};
 * {@link observe} gives the change notifications of one conversation, and
 * {@link events} tells when conversations come and go.
 *
 * The view listens to the wave view and its conversational wavelets from
 * construction on; call {@link dispose} when done.
 */
export class WaveBasedConversationView<G extends IdGenerator & BlipIdGenerator = IdGenerator & BlipIdGenerator> {
  readonly events: Observable<ConversationViewEvents>;
  /** The wave view the conversations live in. */
  readonly wave: WaveView;
  /** The generator of ids for new wavelets, blips and threads. */
  readonly idGenerator: G;

  private readonly options: ConversationViewOptions;
  private readonly emitter: Emitter<ConversationViewEvents>;
  /** The conversational wavelets of the wave view, by conversation id. */
  private readonly entries = new Map<string, Entry>();
  private subscriptions: Unsubscribe[] = [];

  constructor(wave: WaveView, idGenerator: G, options: ConversationViewOptions = {}) {
    this.wave = wave;
    this.idGenerator = idGenerator;
    this.options = options;
    this.emitter = new Emitter<ConversationViewEvents>(
      options.onListenerError ??
        ((error) =>
          queueMicrotask(() => {
            throw error;
          })),
    );
    this.events = this.emitter;
    for (const wavelet of wave.wavelets()) {
      this.track(wavelet, false);
    }
    this.subscriptions = [
      wave.events.on("waveletAdded", ({ wavelet }) => this.track(wavelet, true)),
      wave.events.on("waveletRemoved", ({ wavelet }) => this.untrack(wavelet)),
    ];
  }

  private track(wavelet: OpBasedWavelet, notify: boolean): void {
    const waveletId = wavelet.waveletId();
    if (!isConversational(waveletId)) {
      return;
    }
    const conversationId = conversationIdFor(waveletId);
    const previous = this.entries.get(conversationId);
    if (previous !== undefined) {
      if (previous.wavelet === wavelet) {
        return;
      }
      this.untrack(previous.wavelet);
    }
    const entry: Entry = {
      wavelet,
      present: waveletHasConversation(wavelet.data()),
      observable: null,
      unsubscribe: wavelet.events.on("documentChanged", ({ docId }) => {
        if (docId !== MANIFEST_DOCUMENT_ID) {
          return;
        }
        const present = waveletHasConversation(wavelet.data());
        if (present !== entry.present) {
          entry.present = present;
          this.emitter.emit(present ? "conversationAdded" : "conversationRemoved", {
            conversationId,
            waveletId,
          });
        }
      }),
    };
    this.entries.set(conversationId, entry);
    if (notify && entry.present) {
      this.emitter.emit("conversationAdded", { conversationId, waveletId });
    }
  }

  private untrack(wavelet: OpBasedWavelet): void {
    const waveletId = wavelet.waveletId();
    const conversationId = conversationIdFor(waveletId);
    const entry = this.entries.get(conversationId);
    if (entry === undefined || entry.wavelet !== wavelet) {
      return;
    }
    this.entries.delete(conversationId);
    entry.unsubscribe();
    if (entry.present) {
      this.emitter.emit("conversationRemoved", { conversationId, waveletId });
    }
    // After the event, so that listeners can still detach in an orderly way.
    entry.observable?.dispose();
  }

  /**
   * Stops listening to the wave view and its wavelets, and disposes the
   * observable conversations handed out. Reads and mutations keep working.
   */
  dispose(): void {
    for (const unsubscribe of this.subscriptions) {
      unsubscribe();
    }
    this.subscriptions = [];
    for (const entry of this.entries.values()) {
      entry.unsubscribe();
      entry.observable?.dispose();
    }
    this.entries.clear();
    this.emitter.clear();
  }

  /**
   * The view's id: the serialised wave id
   * (`WaveBasedConversationView.idFor`).
   */
  id(): string {
    const waveId = this.wave.waveId();
    try {
      return waveId.serialise();
    } catch (e) {
      if (e instanceof InvalidIdError) {
        return `${waveId.domain}/${waveId.id}`;
      }
      throw e;
    }
  }

  /**
   * The conversations, ordered by wavelet id (the original's order is
   * unspecified).
   */
  conversations(): Conversation[] {
    const result: Conversation[] = [];
    for (const wavelet of this.wave.wavelets()) {
      if (isConversational(wavelet.waveletId())) {
        const conversation = Conversation.read(wavelet.data());
        if (conversation !== null) {
          result.push(conversation);
        }
      }
    }
    return result;
  }

  /**
   * The conversation in a wavelet. Throws {@link ConversationError} if the
   * id is not that of a conversational wavelet; `null` if the wavelet is
   * missing or has no manifest.
   */
  conversation(waveletId: WaveletId): Conversation | null {
    if (!isConversational(waveletId)) {
      throw notConversational(waveletId);
    }
    const wavelet = this.wave.wavelet(waveletId);
    return wavelet === null ? null : Conversation.read(wavelet.data());
  }

  /** The conversation with a conversation id (see {@link conversationIdFor}). */
  conversationById(conversationId: string): Conversation | null {
    const waveletId = waveletIdFor(conversationId);
    if (waveletId === null) {
      throw ConversationError.invalidArgument(`Invalid conversation id ${conversationId}`);
    }
    return this.conversation(waveletId);
  }

  /** The conversation of the root wavelet, if any (`getRoot`). */
  root(): Conversation | null {
    const wavelet = this.wave.root();
    return wavelet === null ? null : Conversation.read(wavelet.data());
  }

  /**
   * The conversation in a wavelet, for mutation. Throws
   * {@link ConversationError}: `"invalidArgument"` if the id is not
   * conversational, `"noManifest"` if the wavelet is missing or has no
   * manifest.
   */
  conversationMut(waveletId: WaveletId): WaveletBasedConversation {
    if (!isConversational(waveletId)) {
      throw notConversational(waveletId);
    }
    const wavelet = this.wave.wavelet(waveletId);
    if (wavelet === null) {
      throw ConversationError.noManifest();
    }
    return new WaveletBasedConversation(wavelet, this.idGenerator);
  }

  /** The root conversation, for mutation. */
  rootMut(): WaveletBasedConversation {
    return this.conversationMut(this.wave.rootId());
  }

  /**
   * The change notifications of the conversation in a wavelet; `null` if
   * the wave view has no such wavelet. The wavelet need not have a manifest
   * yet. The same object is returned for as long as the wavelet object is
   * in the wave view; it is disposed when the wavelet leaves the view (also
   * when replaced by a new snapshot: observe again on `conversationAdded`)
   * or the view is disposed. Throws {@link ConversationError} if the id is
   * not conversational.
   */
  observe(waveletId: WaveletId): ObservableConversation | null {
    if (!isConversational(waveletId)) {
      throw notConversational(waveletId);
    }
    const wavelet = this.wave.wavelet(waveletId);
    if (wavelet === null) {
      return null;
    }
    // Also covers wavelets added after `dispose()`.
    this.track(wavelet, false);
    const entry = this.entries.get(conversationIdFor(waveletId));
    if (entry === undefined) {
      return null;
    }
    if (entry.observable === null || entry.observable.isDisposed()) {
      entry.observable = new ObservableConversation(wavelet, this.options);
    }
    return entry.observable;
  }

  /** {@link observe} for the root wavelet. */
  observeRoot(): ObservableConversation | null {
    return this.observe(this.wave.rootId());
  }

  /**
   * Creates the root wavelet with the viewer as participant and an empty
   * conversation in it (`createRoot`). `waveletOptions` are for the new
   * wavelet, see `WaveView.createWavelet`.
   */
  createRoot(waveletOptions?: OpBasedWaveletOptions): WaveletBasedConversation {
    const wavelet = this.wave.createRoot(waveletOptions);
    makeWaveletConversational(wavelet);
    return new WaveletBasedConversation(wavelet, this.idGenerator);
  }

  /** Creates a conversation in a new wavelet (`createConversation`). */
  createConversation(waveletOptions?: OpBasedWaveletOptions): WaveletBasedConversation {
    const wavelet = this.wave.createConversationWavelet(this.idGenerator, waveletOptions);
    makeWaveletConversational(wavelet);
    return new WaveletBasedConversation(wavelet, this.idGenerator);
  }

  /**
   * The conversation and blip a conversation is anchored at, if its
   * manifest names an existing blip of a conversation of this wave
   * (`Conversation.getAnchor`); else `null`.
   */
  anchorOf(waveletId: WaveletId): { waveletId: WaveletId; blipId: string } | null {
    if (!isConversational(waveletId)) {
      return null;
    }
    const anchor = this.conversation(waveletId)?.anchor;
    if (anchor == null || anchor.conversationId === null || anchor.blipId === null) {
      return null;
    }
    const anchorWavelet = waveletIdFor(anchor.conversationId);
    if (anchorWavelet === null || !isConversational(anchorWavelet)) {
      return null;
    }
    const anchorConversation = this.conversation(anchorWavelet);
    if (anchorConversation === null || anchorConversation.blip(anchor.blipId) === null) {
      return null;
    }
    return { waveletId: anchorWavelet, blipId: anchor.blipId };
  }

  /**
   * Anchors a conversation at a blip of another conversation of this wave,
   * or removes its anchor (`Conversation.setAnchor`).
   */
  setAnchor(waveletId: WaveletId, anchor: { waveletId: WaveletId; blipId: string } | null): void {
    if (anchor !== null && anchor.waveletId.equals(waveletId)) {
      throw ConversationError.invalidArgument("A conversation cannot be anchored in itself");
    }
    this.conversationMut(waveletId).setAnchor(
      anchor === null
        ? null
        : { conversationId: conversationIdFor(anchor.waveletId), blipId: anchor.blipId },
    );
  }
}
