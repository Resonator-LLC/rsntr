// Supplement state kept in the documents of a user-data wavelet
// (`WaveletBasedSupplement`).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`WaveletBasedSupplement`, `WaveletReadStateCollection`,
// `DocumentBasedWaveletReadState`, `WaveletThreadStateCollection`,
// `DocumentBasedWaveletThreadState`, `GadgetStateCollection`,
// `DocumentBasedGadgetState`, `DocumentBasedAbuseStore`) of Apache Wave,
// licensed under the Apache License, Version 2.0.

import {
  BOOLEAN,
  DOUBLE,
  DocumentBasedBasicMap,
  DocumentBasedBasicSet,
  DocumentBasedBoolean,
  DocumentBasedElementList,
  DocumentBasedMonotonicMap,
  DocumentBasedMonotonicValue,
  HASHED_VERSION,
  INTEGER,
  LONG,
  ROOT,
  STRING,
  WAVELET_ID,
  deleteNodes,
  type InitialAttributes,
  type Locator,
  type Serializer,
} from "../adt";
import type { WaveletData } from "../data";
import { DocView, MutableDocument, type DocumentSink, type NodeId } from "../doc";
import { BootstrapDocument } from "../docop";
import type { WaveletId } from "../id";
import { HashedVersion } from "../version";
import * as c from "./constants";
import { PrimitiveSupplementImpl } from "./primitive";
import {
  NO_VERSION,
  SupplementError,
  THREAD_STATE,
  sortedMap,
  type PrimitiveSupplement,
  type ReadablePrimitiveSupplement,
  type ThreadState,
} from "./types";
import { WantedEvaluation } from "./wanted";

export * from "./constants";

const FOLDERS = new DocumentBasedBasicSet(c.FOLDER_TAG, c.ID_ATTR, INTEGER);
const MUTED = new DocumentBasedBoolean(c.MUTED_TAG, c.MUTED_ATTR);
const CLEARED = new DocumentBasedBoolean(c.CLEARED_TAG, c.CLEARED_ATTR);
const ARCHIVE = new DocumentBasedMonotonicMap(
  c.ARCHIVE_TAG,
  c.ID_ATTR,
  c.VERSION_ATTR,
  WAVELET_ID,
  INTEGER,
);
const SEEN = new DocumentBasedMonotonicMap(
  c.SEEN_VERSION_TAG,
  c.ID_ATTR,
  c.SIGNATURE_ATTR,
  WAVELET_ID,
  HASHED_VERSION,
);
const NOTIFIED = new DocumentBasedMonotonicMap(
  c.NOTIFIED_VERSION_TAG,
  c.ID_ATTR,
  c.VERSION_ATTR,
  WAVELET_ID,
  INTEGER,
);
const PENDING_NOTIFICATION = new DocumentBasedBoolean(
  c.NOTIFICATION_TAG,
  c.PENDING_NOTIFICATION_ATTR,
);
const BLIPS_READ = new DocumentBasedMonotonicMap(
  c.BLIP_READ_TAG,
  c.ID_ATTR,
  c.VERSION_ATTR,
  STRING,
  INTEGER,
);
const PARTICIPANTS_READ = new DocumentBasedMonotonicValue(
  c.PARTICIPANTS_READ_TAG,
  c.VERSION_ATTR,
  INTEGER,
);
const TAGS_READ = new DocumentBasedMonotonicValue(c.TAGS_READ_TAG, c.VERSION_ATTR, INTEGER);
const WAVELET_READ = new DocumentBasedMonotonicValue(c.WAVELET_READ_TAG, c.VERSION_ATTR, INTEGER);
const THREADS = new DocumentBasedBasicMap(
  c.THREAD_TAG,
  c.ID_ATTR,
  c.STATE_ATTR,
  STRING,
  THREAD_STATE,
);
const GADGET_STATE = new DocumentBasedBasicMap(
  c.STATE_TAG,
  c.NAME_ATTR,
  c.VALUE_ATTR,
  STRING,
  STRING,
);
const WANTED = new DocumentBasedElementList(c.WANTED_EVAL_TAG);

/**
 * The inbox and "all" folders, which the original removes from the folder
 * document whenever it creates a supplement (`hackCleanup`).
 */
const LEGACY_FOLDERS: readonly number[] = [1, 3];

/**
 * The top-level elements with a tag name that stand for something: the
 * first element for each id (`WaveletReadStateCollection` and its siblings
 * ignore later elements with an id they already know, and elements without
 * a usable id).
 */
function keyedElements<K>(
  view: DocView,
  tag: string,
  keys: Serializer<K>,
): { key: K; element: NodeId }[] {
  const found: { key: K; element: NodeId }[] = [];
  for (const element of view.childElements(view.root())) {
    if (!view.hasTag(element, tag)) {
      continue;
    }
    const attribute = view.attribute(element, c.ID_ATTR);
    const key = attribute === null ? null : keys.fromAttribute(attribute);
    if (key !== null && !found.some((known) => keys.equals(known.key, key))) {
      found.push({ key, element });
    }
  }
  return found;
}

function keyedElement<K>(view: DocView, tag: string, keys: Serializer<K>, key: K): NodeId | null {
  return keyedElements(view, tag, keys).find((known) => keys.equals(known.key, key))?.element ?? null;
}

/** Names the element standing for a key in the current tree. */
function keyed<K>(tag: string, keys: Serializer<K>, key: K): Locator {
  return (view) => keyedElement(view, tag, keys, key);
}

/**
 * Appends the element standing for a key unless there is one
 * (`getSupplement` and `createEntry` of the collections).
 */
function ensureKeyed<K>(doc: MutableDocument, tag: string, keys: Serializer<K>, key: K): void {
  if (keyedElement(doc.view(), tag, keys, key) === null) {
    const id = keys.toAttribute(key);
    doc.createChildElement(doc.view().root(), tag, [[c.ID_ATTR, id]]);
  }
}

function evaluationOf(view: DocView, element: NodeId): WantedEvaluation | null {
  const parsed = <T>(name: string, serializer: Serializer<T>, fallback: T): T | null => {
    const attribute = view.attribute(element, name);
    return attribute === null ? fallback : serializer.fromAttribute(attribute);
  };
  const text = (name: string): string => view.attribute(element, name) ?? "";
  const id = view.attribute(element, c.WAVELET_ID_ATTR);
  const waveletId = id === null ? null : WAVELET_ID.fromAttribute(id);
  const wanted = parsed(c.WANTED_ATTR, BOOLEAN, false);
  const certainty = parsed(c.CERTAINTY_ATTR, DOUBLE, 0);
  const timestamp = parsed(c.TIMESTAMP_ATTR, LONG, 0);
  const ignored = parsed(c.IGNORED_ATTR, BOOLEAN, false);
  if (
    (id !== null && waveletId === null) ||
    wanted === null ||
    certainty === null ||
    timestamp === null ||
    ignored === null
  ) {
    return null;
  }
  return new WantedEvaluation({
    waveletId,
    adderAddress: text(c.ADDER_ATTR),
    wanted,
    certainty,
    timestamp,
    agentIdentity: text(c.AGENT_ATTR),
    ignored,
    comment: text(c.COMMENT_ATTR),
  });
}

const byWaveletId = (a: WaveletId, b: WaveletId): number => a.compareTo(b);

/**
 * A read-only view of the supplement state in a user-data wavelet: what a
 * `WaveletBasedSupplement` created on the wavelet would report, without
 * creating or changing anything.
 *
 * The original has no read-only supplement; it creates a
 * `WaveletBasedSupplement`, and creating one removes the legacy inbox (1)
 * and "all" (3) folders from the folder document, so this view does not
 * report those folders.
 *
 * Every query interprets a document afresh. To ask many questions, take a
 * snapshot with `PrimitiveSupplementImpl.copyOf`. The data is not copied:
 * the view follows the wavelet.
 */
export class UserDataSupplement implements ReadablePrimitiveSupplement {
  /** Leave out the folders the original removes when it is created. */
  protected readonly hideLegacyFolders: boolean = true;

  constructor(
    /** The data of the user-data wavelet. */
    readonly data: WaveletData,
  ) {}

  /** What was read from the documents, while a snapshot is being taken. */
  private cache: Map<string, unknown> | null = null;

  private memo<T>(key: string, compute: () => T): T {
    if (this.cache === null) {
      return compute();
    }
    if (!this.cache.has(key)) {
      this.cache.set(key, compute());
    }
    return this.cache.get(key) as T;
  }

  private view(docId: string): DocView {
    return this.memo(
      `view ${docId}`,
      () => new DocView(this.data.document(docId)?.content ?? new BootstrapDocument()),
    );
  }

  private readState<T>(
    waveletId: WaveletId,
    read: (view: DocView, element: NodeId) => T | null,
  ): T | null {
    const view = this.view(c.READSTATE_DOCUMENT);
    const element = keyedElement(view, c.WAVELET_TAG, WAVELET_ID, waveletId);
    return element === null ? null : read(view, element);
  }

  /** The last-read versions of the blips of a wavelet. */
  private blipsRead(waveletId: WaveletId): Map<string, number> {
    return this.memo(
      `blips ${waveletId.domain}/${waveletId.id}`,
      () => new Map(this.readState(waveletId, (v, e) => BLIPS_READ.entries(v, e)) ?? []),
    );
  }

  /** The states of the threads of a wavelet. */
  private threads(waveletId: WaveletId): Map<string, ThreadState> {
    return this.memo(`threads ${waveletId.domain}/${waveletId.id}`, () => {
      const view = this.view(c.PRESENTATION_DOCUMENT);
      const element = keyedElement(view, c.CONVERSATION_TAG, WAVELET_ID, waveletId);
      return new Map(element === null ? [] : THREADS.entries(view, element));
    });
  }

  /**
   * A copy of the state in memory, which answers without reading the
   * documents again (`PrimitiveSupplementImpl.copyOf`, reading each
   * document once). The wavelet must not change during the call.
   */
  snapshot(): PrimitiveSupplementImpl {
    this.cache = new Map();
    try {
      return PrimitiveSupplementImpl.copyOf(this);
    } finally {
      this.cache = null;
    }
  }

  threadState(waveletId: WaveletId, threadId: string): ThreadState | null {
    return this.threads(waveletId).get(threadId) ?? null;
  }

  waveletsWithThreadState(): WaveletId[] {
    const view = this.view(c.PRESENTATION_DOCUMENT);
    return keyedElements(view, c.CONVERSATION_TAG, WAVELET_ID)
      .map(({ key }) => key)
      .sort(byWaveletId);
  }

  statefulThreads(waveletId: WaveletId): string[] {
    return [...this.threads(waveletId).keys()];
  }

  lastReadWaveletVersion(waveletId: WaveletId): number {
    return this.readState(waveletId, (v, e) => WAVELET_READ.get(v, e)) ?? NO_VERSION;
  }

  lastReadBlipVersion(waveletId: WaveletId, blipId: string): number {
    return this.blipsRead(waveletId).get(blipId) ?? NO_VERSION;
  }

  lastReadParticipantsVersion(waveletId: WaveletId): number {
    return this.readState(waveletId, (v, e) => PARTICIPANTS_READ.get(v, e)) ?? NO_VERSION;
  }

  lastReadTagsVersion(waveletId: WaveletId): number {
    return this.readState(waveletId, (v, e) => TAGS_READ.get(v, e)) ?? NO_VERSION;
  }

  readWavelets(): WaveletId[] {
    const view = this.view(c.READSTATE_DOCUMENT);
    return keyedElements(view, c.WAVELET_TAG, WAVELET_ID)
      .map(({ key }) => key)
      .sort(byWaveletId);
  }

  readBlips(waveletId: WaveletId): string[] {
    return [...this.blipsRead(waveletId).keys()];
  }

  folders(): number[] {
    const view = this.view(c.FOLDERS_DOCUMENT);
    const folders = FOLDERS.values(view, view.root());
    return this.hideLegacyFolders
      ? folders.filter((folder) => !LEGACY_FOLDERS.includes(folder))
      : folders;
  }

  isInFolder(id: number): boolean {
    return this.folders().includes(id);
  }

  /** Whether the flag that overrides the archive versions is set. */
  protected isCleared(): boolean {
    const view = this.view(c.CLEARED_DOCUMENT);
    return CLEARED.get(view, view.root()) === true;
  }

  archiveWaveletVersion(waveletId: WaveletId): number {
    if (this.isCleared()) {
      return NO_VERSION;
    }
    const view = this.view(c.ARCHIVING_DOCUMENT);
    return ARCHIVE.get(view, view.root(), waveletId) ?? NO_VERSION;
  }

  archiveWavelets(): WaveletId[] {
    const view = this.view(c.ARCHIVING_DOCUMENT);
    return ARCHIVE.keySet(view, view.root());
  }

  followed(): boolean | null {
    const view = this.view(c.MUTED_DOCUMENT);
    const muted = MUTED.get(view, view.root());
    return muted === null ? null : !muted;
  }

  seenVersion(waveletId: WaveletId): HashedVersion {
    const view = this.view(c.SEEN_DOCUMENT);
    return SEEN.get(view, view.root(), waveletId) ?? HashedVersion.unsigned(0);
  }

  seenWavelets(): WaveletId[] {
    const view = this.view(c.SEEN_DOCUMENT);
    return SEEN.keySet(view, view.root());
  }

  wantedEvaluations(): WantedEvaluation[] {
    const view = this.view(c.ABUSE_DOCUMENT);
    const evaluations: WantedEvaluation[] = [];
    for (const element of WANTED.elements(view, view.root())) {
      const evaluation = evaluationOf(view, element);
      if (evaluation !== null && !evaluations.some((known) => known.equals(evaluation))) {
        evaluations.push(evaluation);
      }
    }
    return evaluations;
  }

  pendingNotification(): boolean {
    const view = this.view(c.SEEN_DOCUMENT);
    return PENDING_NOTIFICATION.get(view, view.root()) === true;
  }

  notifiedWavelets(): WaveletId[] {
    const view = this.view(c.SEEN_DOCUMENT);
    return NOTIFIED.keySet(view, view.root());
  }

  notifiedVersion(waveletId: WaveletId): number {
    const view = this.view(c.SEEN_DOCUMENT);
    return NOTIFIED.get(view, view.root(), waveletId) ?? NO_VERSION;
  }

  gadgetState(gadgetId: string): Map<string, string> {
    const view = this.view(c.GADGETS_DOCUMENT);
    const element = keyedElement(view, c.GADGET_TAG, STRING, gadgetId);
    return element === null ? new Map() : sortedMap(GADGET_STATE.entries(view, element));
  }

  /**
   * The private states of all gadgets that have a `gadget` element, by
   * gadget id, in order. Not in the original, whose gadget states can only
   * be asked for by id.
   */
  gadgetStates(): Map<string, Map<string, string>> {
    const view = this.view(c.GADGETS_DOCUMENT);
    return sortedMap(
      keyedElements(view, c.GADGET_TAG, STRING).map(
        ({ key, element }) => [key, sortedMap(GADGET_STATE.entries(view, element))] as const,
      ),
    );
  }
}

/**
 * What {@link WaveletBasedSupplement} needs of the user-data wavelet; an
 * `OpBasedWavelet` of `model/wavelet` is one.
 */
export interface UserDataWavelet {
  /** The wavelet data, which follows the edits. */
  data(): WaveletData;
  /** A document of the wavelet for editing, each edit an operation. */
  documentSink(docId: string): DocumentSink;
}

/**
 * The supplement state in a user-data wavelet, for reading and writing
 * (`WaveletBasedSupplement`). Every change is made through document edits
 * on the wavelet, which turns them into operations to submit.
 *
 * # Documents
 *
 * Every element below is a direct child of its document, and written
 * without content. `i` attributes holding a wavelet id use the form
 * `domain/id`.
 *
 * * `m/read`, read versions:
 *   `<wavelet i="example.com/conv+root"><blip i="b+1" v="7"/><participants v="12"/><tags v="3"/><all v="25"/></wavelet>`.
 *   The greatest `v` counts where a `blip` id or one of the other elements
 *   occurs more than once. Of several `wavelet` elements with one id the
 *   first counts.
 * * `m/presentation`, thread states:
 *   `<conversation i="example.com/conv+root"><thread i="t+1" state="COLLAPSED"/></conversation>`,
 *   state `COLLAPSED` or `EXPANDED`; the last `thread` element of an id
 *   counts, and the first `conversation` element of an id.
 * * `m/folder`: `<folder i="8"/>` per folder.
 * * `m/archiving`: `<archive i="example.com/conv+root" v="48"/>` per
 *   wavelet; the greatest `v` counts.
 * * `m/cleared`: `<cleared cleared="true"/>`; while true, the archive
 *   versions are disregarded. The last element counts.
 * * `m/muted`: `<muted muted="true"/>` when unfollowed, `"false"` when
 *   followed, absent when neither; the last element counts.
 * * `m/seen`: `<seen i="example.com/conv+root" signature="12:aGFzaA=="/>`
 *   (version, colon, base64 history hash; the greatest counts),
 *   `<notified i="example.com/conv+root" v="12"/>` (the greatest counts)
 *   and `<notification pending="true"/>`.
 * * `m/gadgets`: `<gadget i="gadget id"><state name="key" value="value"/></gadget>`;
 *   the last `state` of a name counts, and the first `gadget` of an id.
 * * `m/abuse`: one `<wanted wavelet_id=".." wanted="true" certainty="0.5" timestamp="1234" adder=".." agent=".." comment=".." ignored="false"/>`
 *   per evaluation.
 *
 * New `blip`, `thread`, `state`, `archive`, `seen` and `notified` elements
 * are inserted first in their parent, after the element they replace has
 * been deleted; all others are appended, the elements they replace being
 * deleted afterwards. Any write to one of these structures also deletes
 * the superseded elements it finds in it.
 *
 * # Differences from the original
 *
 * * The original object caches the documents and follows their changes;
 *   this one reads the documents on every call. The two agree except after
 *   `clearReadState` on a read-state document with several `wavelet`
 *   elements for one wavelet: only the first is deleted, the original
 *   object then reports no read state for the wavelet until it is created
 *   anew, whereas here (as for a newly created original) the next element
 *   takes over at once.
 * * Elements whose id or value does not parse are ignored (see
 *   `model/adt`); the original fails to create the supplement.
 * * `clearBlipReadState` for a wavelet without read state does nothing;
 *   the original throws a `NullPointerException`.
 * * Where the original deletes several elements in hash order (clearing
 *   the read state, superseded elements) they are deleted in document
 *   order, and folders are removed in ascending order.
 *
 * Quirks kept: archive versions never decrease, so archiving again at a
 * lower version after the archive state was cleared restores the old,
 * higher version; setting a gadget state to `null` creates the `gadget`
 * element if there is none; the pending notification flag can be cleared
 * but not set.
 */
export class WaveletBasedSupplement extends UserDataSupplement implements PrimitiveSupplement {
  protected override readonly hideLegacyFolders: boolean = false;

  private constructor(
    /** The wavelet holding the state. */
    readonly userData: UserDataWavelet,
  ) {
    super(userData.data());
  }

  /**
   * Wraps a user-data wavelet (`WaveletBasedSupplement.create`). As in the
   * original this removes the legacy inbox (1) and "all" (3) folders from
   * the folder document if they are there, which emits operations.
   */
  static create(userData: UserDataWavelet): WaveletBasedSupplement {
    const supplement = new WaveletBasedSupplement(userData);
    for (const folder of LEGACY_FOLDERS) {
      supplement.removeFolder(folder);
    }
    return supplement;
  }

  private edit<R>(docId: string, edit: (doc: MutableDocument) => R): R {
    return edit(new MutableDocument(this.userData.documentSink(docId)));
  }

  private editReadState(
    waveletId: WaveletId,
    edit: (doc: MutableDocument, wavelet: Locator) => void,
  ): void {
    this.edit(c.READSTATE_DOCUMENT, (doc) => {
      ensureKeyed(doc, c.WAVELET_TAG, WAVELET_ID, waveletId);
      edit(doc, keyed(c.WAVELET_TAG, WAVELET_ID, waveletId));
    });
  }

  setThreadState(waveletId: WaveletId, threadId: string, state: ThreadState | null): void {
    const conversation = keyed(c.CONVERSATION_TAG, WAVELET_ID, waveletId);
    this.edit(c.PRESENTATION_DOCUMENT, (doc) => {
      if (state === null) {
        if (conversation(doc.view()) !== null) {
          THREADS.remove(doc, conversation, threadId);
        }
      } else {
        ensureKeyed(doc, c.CONVERSATION_TAG, WAVELET_ID, waveletId);
        THREADS.put(doc, conversation, threadId, state);
      }
    });
  }

  setLastReadWaveletVersion(waveletId: WaveletId, version: number): void {
    this.editReadState(waveletId, (doc, wavelet) => WAVELET_READ.set(doc, wavelet, version));
  }

  setLastReadBlipVersion(waveletId: WaveletId, blipId: string, version: number): void {
    this.editReadState(waveletId, (doc, wavelet) => BLIPS_READ.put(doc, wavelet, blipId, version));
  }

  setLastReadParticipantsVersion(waveletId: WaveletId, version: number): void {
    this.editReadState(waveletId, (doc, wavelet) => PARTICIPANTS_READ.set(doc, wavelet, version));
  }

  setLastReadTagsVersion(waveletId: WaveletId, version: number): void {
    this.editReadState(waveletId, (doc, wavelet) => TAGS_READ.set(doc, wavelet, version));
  }

  clearReadState(): void {
    this.edit(c.READSTATE_DOCUMENT, (doc) => {
      const elements = keyedElements(doc.view(), c.WAVELET_TAG, WAVELET_ID);
      deleteNodes(
        doc,
        elements.map(({ element }) => element),
      );
    });
  }

  clearBlipReadState(waveletId: WaveletId, blipId: string): void {
    const wavelet = keyed(c.WAVELET_TAG, WAVELET_ID, waveletId);
    this.edit(c.READSTATE_DOCUMENT, (doc) => {
      if (wavelet(doc.view()) !== null) {
        BLIPS_READ.remove(doc, wavelet, blipId);
      }
    });
  }

  addFolder(id: number): void {
    this.edit(c.FOLDERS_DOCUMENT, (doc) => FOLDERS.add(doc, ROOT, id));
  }

  removeFolder(id: number): void {
    this.edit(c.FOLDERS_DOCUMENT, (doc) => FOLDERS.remove(doc, ROOT, id));
  }

  removeAllFolders(): void {
    this.edit(c.FOLDERS_DOCUMENT, (doc) => FOLDERS.clear(doc, ROOT));
  }

  follow(): void {
    this.edit(c.MUTED_DOCUMENT, (doc) => MUTED.set(doc, ROOT, false));
  }

  unfollow(): void {
    this.edit(c.MUTED_DOCUMENT, (doc) => MUTED.set(doc, ROOT, true));
  }

  clearFollow(): void {
    this.edit(c.MUTED_DOCUMENT, (doc) => MUTED.set(doc, ROOT, null));
  }

  archiveAtVersion(waveletId: WaveletId, version: number): void {
    this.edit(c.ARCHIVING_DOCUMENT, (doc) => ARCHIVE.put(doc, ROOT, waveletId, version));
    if (this.isCleared()) {
      this.edit(c.CLEARED_DOCUMENT, (doc) => CLEARED.set(doc, ROOT, false));
    }
  }

  clearArchiveState(): void {
    // The archive versions cannot be removed or lowered; a flag overrides
    // them instead.
    if (!this.isCleared()) {
      this.edit(c.CLEARED_DOCUMENT, (doc) => CLEARED.set(doc, ROOT, true));
    }
  }

  setSeenVersion(waveletId: WaveletId, version: HashedVersion): void {
    this.edit(c.SEEN_DOCUMENT, (doc) => SEEN.put(doc, ROOT, waveletId, version));
  }

  clearSeenVersion(waveletId: WaveletId): void {
    this.edit(c.SEEN_DOCUMENT, (doc) => SEEN.remove(doc, ROOT, waveletId));
  }

  addWantedEvaluation(evaluation: WantedEvaluation): void {
    // A missing wavelet id makes the original fail when it builds the
    // attributes of the element.
    if (evaluation.waveletId === null) {
      throw SupplementError.illegalState("a wanted evaluation to store needs a wavelet id");
    }
    const attributes: InitialAttributes = new Map([
      [c.WAVELET_ID_ATTR, WAVELET_ID.toAttribute(evaluation.waveletId)],
      [c.WANTED_ATTR, BOOLEAN.toAttribute(evaluation.wanted)],
      [c.CERTAINTY_ATTR, DOUBLE.toAttribute(evaluation.certainty)],
      [c.TIMESTAMP_ATTR, LONG.toAttribute(evaluation.timestamp)],
      [c.ADDER_ATTR, evaluation.adderAddress],
      [c.AGENT_ATTR, evaluation.agentIdentity],
      [c.COMMENT_ATTR, evaluation.comment],
      [c.IGNORED_ATTR, BOOLEAN.toAttribute(evaluation.ignored)],
    ]);
    this.edit(c.ABUSE_DOCUMENT, (doc) => WANTED.add(doc, ROOT, attributes));
  }

  setNotifiedVersion(waveletId: WaveletId, version: number): void {
    this.edit(c.SEEN_DOCUMENT, (doc) => NOTIFIED.put(doc, ROOT, waveletId, version));
  }

  clearPendingNotification(): void {
    this.edit(c.SEEN_DOCUMENT, (doc) => PENDING_NOTIFICATION.set(doc, ROOT, null));
  }

  setGadgetState(gadgetId: string, key: string, value: string | null): void {
    const gadget = keyed(c.GADGET_TAG, STRING, gadgetId);
    this.edit(c.GADGETS_DOCUMENT, (doc) => {
      ensureKeyed(doc, c.GADGET_TAG, STRING, gadgetId);
      if (value === null) {
        GADGET_STATE.remove(doc, gadget, key);
      } else {
        GADGET_STATE.put(doc, gadget, key, value);
      }
    });
  }
}
