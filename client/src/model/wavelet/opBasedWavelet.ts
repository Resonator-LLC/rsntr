// A wavelet whose mutations are applied locally and turned into wavelet
// operations for submission, and that reports every change, local or
// remote, to listeners (`OpBasedWavelet`).
//
// TypeScript port of the Rust crate `resonator-model` (module `wavelet`), itself
// derived from `org.waveprotocol.wave.model.wave.opbased.OpBasedWavelet`,
// `OpBasedBlip`, `ParticipationHelper`, `wave.WaveletListener` and
// `operation.wave.AbstractWaveletOperationContextFactory` of Apache Wave,
// licensed under the Apache License, Version 2.0.
//
// The Rust port has no listeners; the events here follow the original's
// `WaveletListener`, with ids in place of objects.

import type { BlipData, WaveletData } from "../data";
import { DocError, type DocumentSink, DocView, MutableDocument } from "../doc";
import { BootstrapDocument, type DocOp } from "../docop";
import type { ParticipantId, WaveletId } from "../id";
import {
  WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
  WaveletOperationError,
} from "../operation";
import type { HashedVersion } from "../version";
import { Emitter, type Observable } from "../../util/events";
import { WaveletError } from "./error";

/**
 * What happens when the author of an operation is not a participant of a
 * wavelet that has participants (`ParticipationHelper`):
 *
 * * `"auto"`: the author first adds themself as a participant (`AUTO`, the
 *   original's default);
 * * `"ignorant"`: nothing, the operation is emitted as it is (`IGNORANT`);
 * * `"readOnly"`: the mutation fails (`READONLY`).
 */
export type ParticipationHelper = "auto" | "ignorant" | "readOnly";

/**
 * The events of an {@link OpBasedWavelet}. All are emitted synchronously
 * after the wavelet data changed. `local` tells operations made through this
 * object from those given to {@link OpBasedWavelet.consumeRemote}.
 *
 * For one operation the order is: `participantAdded`/`participantRemoved`,
 * `documentChanged`, `blipContributorAdded`/`Removed`,
 * `blipTimestampChanged`, `blipVersionChanged`, `lastModifiedTimeChanged`,
 * `versionChanged`, `hashedVersionChanged`, and last `operation`.
 */
export type WaveletEvents = {
  /** Any wavelet operation was applied (local or remote). */
  operation: { op: WaveletOperation; local: boolean };
  participantAdded: { participant: ParticipantId; local: boolean; author: ParticipantId };
  participantRemoved: { participant: ParticipantId; local: boolean; author: ParticipantId };
  /**
   * The content of document `docId` was changed by `op`. `created` is true
   * if the document did not exist before the operation.
   */
  documentChanged: {
    docId: string;
    op: DocOp;
    local: boolean;
    author: ParticipantId;
    created: boolean;
  };
  /** Also emitted for the first contributor of a document being created. */
  blipContributorAdded: { docId: string; contributor: ParticipantId };
  blipContributorRemoved: { docId: string; contributor: ParticipantId };
  /** Not emitted for a document being created. */
  blipTimestampChanged: { docId: string; oldTime: number; newTime: number };
  /** Not emitted for a document being created. */
  blipVersionChanged: { docId: string; oldVersion: number; newVersion: number };
  lastModifiedTimeChanged: { oldTime: number; newTime: number };
  versionChanged: { oldVersion: number; newVersion: number };
  hashedVersionChanged: { oldVersion: HashedVersion; newVersion: HashedVersion };
};

export interface OpBasedWaveletOptions {
  /**
   * Timestamp source for operation contexts, milliseconds since the epoch.
   * Default: `Date.now`.
   */
  clock?: () => number;
  /**
   * A fixed timestamp that overrides the clock (the Rust `timestamp`); see
   * {@link OpBasedWavelet.setTimestamp}.
   */
  timestamp?: number | null;
  /** Default `"auto"`. */
  participation?: ParticipationHelper;
  /**
   * Receives every locally generated operation after it was applied and
   * its events were delivered (the concurrency-control channel).
   */
  output?: (op: WaveletOperation) => void;
  /**
   * Whether local operations are recorded for {@link OpBasedWavelet.ops},
   * `takeOps` and `takeDelta`. Default true, also with an `output` sink:
   * a client that only uses the sink should pass false, or the recorded
   * operations accumulate.
   */
  record?: boolean;
  /**
   * Receives what an event listener throws. A listener's failure never
   * reaches the caller of a mutation or of `consumeRemote`. Default: the
   * error is rethrown from a microtask, so that it is reported as uncaught.
   */
  onListenerError?: (error: unknown) => void;
  /**
   * Test only: follows every local operation with a version update of one
   * (as if the server had acknowledged it at once) and advances a fixed
   * timestamp by a millisecond, as the reference tests'
   * `OpBasedWaveletFactory` and the golden generator do.
   */
  bumpVersions?: boolean;
}

function reportLater(error: unknown): void {
  queueMicrotask(() => {
    throw error;
  });
}

/** The state an operation can change, as it was before the operation. */
interface Before {
  readonly version: number;
  readonly hashedVersion: HashedVersion;
  readonly lastModifiedTime: number;
  readonly docId: string | null;
  readonly blip: {
    readonly contributors: readonly ParticipantId[];
    readonly time: number;
    readonly version: number;
  } | null;
}

type Task = () => void;

/**
 * A wavelet together with an author on whose behalf it is mutated.
 *
 * Every mutation is turned into a {@link WaveletOperation} by the author,
 * applied to the wavelet data at once, recorded (unless `record` is false),
 * reported to the listeners, and then given to the `output` sink.
 * Operations from the server are applied with {@link consumeRemote}.
 *
 * As in the original, local operations are created with a version increment
 * of zero: applying them changes neither the wavelet's version nor its
 * hashed version, only its content and last-modified time. A blip touched
 * by a local operation gets the unchanged wavelet version as its
 * last-modified version. Versions move when the server's operations and
 * acknowledgements (version updates) are consumed.
 *
 * Mutations throw `WaveletOperationError` if the operation does not apply,
 * `DocError` for an invalid document edit, and {@link WaveletError}
 * (`"readOnly"`).
 *
 * Operations may be made from inside an event listener. They are applied
 * at once, but their events (and their delivery to `output`) are queued
 * behind those of the operation being reported, so every listener sees the
 * events, and the sink the operations, in the order of application.
 */
export class OpBasedWavelet {
  /** Change notifications; see {@link WaveletEvents}. */
  readonly events: Observable<WaveletEvents>;

  private readonly waveletData: WaveletData;
  private currentAuthor: ParticipantId;
  private readonly clock: () => number;
  private fixedTimestamp: number | null;
  private participation: ParticipationHelper;
  private readonly output: ((op: WaveletOperation) => void) | null;
  private readonly record: boolean;
  private readonly bumpVersions: boolean;
  private readonly emitter: Emitter<WaveletEvents>;
  private recorded: WaveletOperation[] = [];
  /** The hashed version at which the oldest recorded operation was made. */
  private targetVersion: HashedVersion | null = null;
  private readonly queue: Task[] = [];
  private draining = false;

  /** Wraps wavelet data for mutation by `author`. The data is not copied. */
  constructor(data: WaveletData, author: ParticipantId, options: OpBasedWaveletOptions = {}) {
    this.waveletData = data;
    this.currentAuthor = author;
    this.clock = options.clock ?? Date.now;
    this.fixedTimestamp = options.timestamp ?? null;
    this.participation = options.participation ?? "auto";
    this.output = options.output ?? null;
    this.record = options.record ?? true;
    this.bumpVersions = options.bumpVersions ?? false;
    this.emitter = new Emitter<WaveletEvents>(options.onListenerError ?? reportLater);
    this.events = this.emitter;
  }

  /**
   * The wavelet data. Changing it directly is neither recorded nor
   * reported; use {@link consumeRemote} for operations from elsewhere.
   */
  data(): WaveletData {
    return this.waveletData;
  }

  waveletId(): WaveletId {
    return this.waveletData.waveletId;
  }

  author(): ParticipantId {
    return this.currentAuthor;
  }

  /**
   * Changes the author of subsequent operations. Take the delta of the
   * previous author first: a delta has a single author.
   */
  setAuthor(author: ParticipantId): void {
    this.currentAuthor = author;
  }

  participationHelper(): ParticipationHelper {
    return this.participation;
  }

  setParticipationHelper(helper: ParticipationHelper): void {
    this.participation = helper;
  }

  /**
   * The time given to the next operation: the fixed timestamp if one is
   * set, else the clock's.
   */
  timestamp(): number {
    return this.fixedTimestamp ?? this.clock();
  }

  /** Fixes the time of the operations to come; `null` returns to the clock. */
  setTimestamp(timestamp: number | null): void {
    this.fixedTimestamp = timestamp;
  }

  /** The operations recorded since the last take, oldest first. */
  ops(): readonly WaveletOperation[] {
    return this.recorded;
  }

  /** Removes and returns the recorded operations. */
  takeOps(): WaveletOperation[] {
    const ops = this.recorded;
    this.recorded = [];
    this.targetVersion = null;
    return ops;
  }

  /**
   * Removes the recorded operations and returns them as a delta by the
   * current author, or `null` if nothing was recorded. The delta's target
   * is the hashed version the wavelet had when the oldest of these
   * operations was made (in Rust: when the data was wrapped; the same
   * unless remote operations arrived in between).
   */
  takeDelta(): WaveletDelta | null {
    if (this.recorded.length === 0) {
      return null;
    }
    const target = this.targetVersion ?? this.waveletData.hashedVersion;
    return new WaveletDelta(this.currentAuthor, target, this.takeOps());
  }

  private createContext(): WaveletOperationContext {
    return new WaveletOperationContext(this.currentAuthor, this.timestamp(), 0);
  }

  private snapshot(op: WaveletOperation): Before {
    const data = this.waveletData;
    const kind = op.kind;
    const docId =
      kind.type === "blip" ? kind.blipId : kind.type === "versionUpdate" ? kind.docId : null;
    const blip = docId === null ? null : data.document(docId);
    return {
      version: data.version,
      hashedVersion: data.hashedVersion,
      lastModifiedTime: data.lastModifiedTime,
      docId,
      blip:
        blip === null
          ? null
          : {
              contributors: [...blip.contributors],
              time: blip.lastModifiedTime,
              version: blip.lastModifiedVersion,
            },
    };
  }

  /** The events of an operation just applied, as deliveries to make. */
  private eventsOf(op: WaveletOperation, local: boolean, before: Before): Task[] {
    const emitter = this.emitter;
    const data = this.waveletData;
    const author = op.context.creator;
    const kind = op.kind;
    const tasks: Task[] = [];
    if (kind.type === "addParticipant") {
      const participant = kind.participant;
      tasks.push(() => emitter.emit("participantAdded", { participant, local, author }));
    } else if (kind.type === "removeParticipant") {
      const participant = kind.participant;
      tasks.push(() => emitter.emit("participantRemoved", { participant, local, author }));
    }
    const docId = before.docId;
    const blip = docId === null ? null : data.document(docId);
    if (docId !== null && blip !== null) {
      const created = before.blip === null;
      const contentOp = op.contentOp;
      if (contentOp !== null) {
        tasks.push(() =>
          emitter.emit("documentChanged", { docId, op: contentOp, local, author, created }),
        );
      }
      const old = before.blip?.contributors ?? [];
      const now = [...blip.contributors];
      for (const contributor of old) {
        if (!now.some((p) => p.equals(contributor))) {
          tasks.push(() => emitter.emit("blipContributorRemoved", { docId, contributor }));
        }
      }
      for (const contributor of now) {
        if (!old.some((p) => p.equals(contributor))) {
          tasks.push(() => emitter.emit("blipContributorAdded", { docId, contributor }));
        }
      }
      if (before.blip !== null) {
        const oldTime = before.blip.time;
        const newTime = blip.lastModifiedTime;
        if (oldTime !== newTime) {
          tasks.push(() => emitter.emit("blipTimestampChanged", { docId, oldTime, newTime }));
        }
        const oldVersion = before.blip.version;
        const newVersion = blip.lastModifiedVersion;
        if (oldVersion !== newVersion) {
          tasks.push(() =>
            emitter.emit("blipVersionChanged", { docId, oldVersion, newVersion }),
          );
        }
      }
    }
    if (before.lastModifiedTime !== data.lastModifiedTime) {
      const oldTime = before.lastModifiedTime;
      const newTime = data.lastModifiedTime;
      tasks.push(() => emitter.emit("lastModifiedTimeChanged", { oldTime, newTime }));
    }
    if (before.version !== data.version) {
      const oldVersion = before.version;
      const newVersion = data.version;
      tasks.push(() => emitter.emit("versionChanged", { oldVersion, newVersion }));
    }
    if (!before.hashedVersion.equals(data.hashedVersion)) {
      const oldVersion = before.hashedVersion;
      const newVersion = data.hashedVersion;
      tasks.push(() => emitter.emit("hashedVersionChanged", { oldVersion, newVersion }));
    }
    tasks.push(() => emitter.emit("operation", { op, local }));
    return tasks;
  }

  /**
   * Runs deliveries in order. Deliveries added while one runs (operations
   * made by a listener) wait their turn. Listener errors go to the
   * emitter's handler; the first error of the output sink is rethrown once
   * the queue is empty.
   */
  private dispatch(tasks: readonly Task[]): void {
    this.queue.push(...tasks);
    if (this.draining) {
      return;
    }
    this.draining = true;
    let failed = false;
    let firstError: unknown;
    try {
      for (let task = this.queue.shift(); task !== undefined; task = this.queue.shift()) {
        try {
          task();
        } catch (error) {
          if (!failed) {
            failed = true;
            firstError = error;
          }
        }
      }
    } finally {
      this.draining = false;
    }
    if (failed) {
      throw firstError;
    }
  }

  private applyOp(op: WaveletOperation, local: boolean): Task[] {
    const before = this.snapshot(op);
    op.apply(this.waveletData);
    return this.eventsOf(op, local, before);
  }

  private applyAndSend(op: WaveletOperation): void {
    const target = this.waveletData.hashedVersion;
    const tasks = this.applyOp(op, true);
    if (this.record) {
      if (this.recorded.length === 0) {
        this.targetVersion = target;
      }
      this.recorded.push(op);
    }
    if (this.bumpVersions) {
      try {
        tasks.push(...this.applyOp(op.createVersionUpdateOp(1, null), false));
      } finally {
        // The clock of the golden generator: a millisecond per operation.
        if (this.fixedTimestamp !== null) {
          this.fixedTimestamp += 1;
        }
      }
    }
    const output = this.output;
    if (output !== null) {
      tasks.push(() => output(op));
    }
    this.dispatch(tasks);
  }

  /**
   * Applies an operation that came from the server, already transformed
   * against the local operations: another client's operation, or a version
   * update that acknowledges local ones. It is neither recorded nor sent to
   * `output`; its events have `local: false`. Throws
   * `WaveletOperationError` if it does not apply (see
   * `WaveletOperation.apply` for the state then), in which case nothing is
   * emitted.
   */
  consumeRemote(op: WaveletOperation): void {
    this.dispatch(this.applyOp(op, false));
  }

  /**
   * Makes sure the author may act on the wavelet (`authorise`). Returns
   * whether an add-participant operation for the author was injected.
   */
  private authorise(): boolean {
    const data = this.waveletData;
    if (data.participants.length === 0 || data.hasParticipant(this.currentAuthor)) {
      return false;
    }
    switch (this.participation) {
      case "ignorant":
        return false;
      case "readOnly":
        throw new WaveletError({ kind: "readOnly", author: this.currentAuthor });
      case "auto":
        this.applyAndSend(
          WaveletOperation.addParticipant(this.createContext(), this.currentAuthor),
        );
        return true;
    }
  }

  /** Participants in the order they were added. */
  participants(): readonly ParticipantId[] {
    return this.waveletData.participants;
  }

  /** Adds a participant unless already present (`addParticipant`). */
  addParticipant(participant: ParticipantId): void {
    if (this.waveletData.hasParticipant(participant)) {
      return;
    }
    const injected = this.authorise();
    if (injected && participant.equals(this.currentAuthor)) {
      return;
    }
    this.applyAndSend(WaveletOperation.addParticipant(this.createContext(), participant));
  }

  /** Removes a participant if present (`removeParticipant`). */
  removeParticipant(participant: ParticipantId): void {
    if (!this.waveletData.hasParticipant(participant)) {
      return;
    }
    this.authorise();
    this.applyAndSend(WaveletOperation.removeParticipant(this.createContext(), participant));
  }

  /** Emits an operation that changes nothing (`touch`). */
  touch(): void {
    this.authorise();
    this.applyAndSend(WaveletOperation.noOp(this.createContext()));
  }

  /** The document or blip with an id, if an operation ever touched it. */
  blip(docId: string): BlipData | null {
    return this.waveletData.document(docId);
  }

  hasDocument(docId: string): boolean {
    return this.waveletData.hasDocument(docId);
  }

  /** Document ids in UTF-16 order. */
  documentIds(): readonly string[] {
    return this.waveletData.documentIds();
  }

  /**
   * The content of a document; an empty document if it does not exist (the
   * original creates documents on first access). Read only: change it
   * through {@link applyDocOp}, {@link editDocument} or
   * {@link documentSink}.
   */
  document(docId: string): BootstrapDocument {
    return this.waveletData.document(docId)?.content ?? new BootstrapDocument();
  }

  /** The element tree of a document as it is now. */
  documentView(docId: string): DocView {
    return new DocView(this.document(docId));
  }

  /**
   * Applies a document operation by the author to a document, creating the
   * document if needed, and records it.
   */
  applyDocOp(docId: string, op: DocOp): void {
    this.authorise();
    this.applyAndSend(WaveletOperation.blipContent(this.createContext(), docId, op));
  }

  /**
   * A document of the wavelet for editing. Each edit becomes one wavelet
   * operation.
   */
  documentSink(docId: string): WaveletDocument {
    return new WaveletDocument(this, docId);
  }

  /**
   * Edits a document through `edit`, which gets a fresh
   * {@link MutableDocument} that must not be kept.
   */
  editDocument<R>(docId: string, edit: (doc: MutableDocument) => R): R {
    return edit(new MutableDocument(this.documentSink(docId)));
  }
}

/**
 * One document of an {@link OpBasedWavelet}, as the target of a
 * {@link MutableDocument}.
 */
export class WaveletDocument implements DocumentSink {
  constructor(
    private readonly wavelet: OpBasedWavelet,
    readonly docId: string,
  ) {}

  /**
   * A new editing facade. After the document was changed by a remote
   * operation, call its `refresh()` or get a new one.
   */
  mutable(): MutableDocument {
    return new MutableDocument(this);
  }

  document(): BootstrapDocument {
    return this.wavelet.document(this.docId);
  }

  /**
   * Throws {@link DocError}: of kind `"wavelet"` wrapping the
   * `WaveletOperationError` if the wavelet rejects the operation,
   * `"invalidArgument"` for a read-only wavelet.
   */
  consume(op: DocOp): void {
    try {
      this.wavelet.applyDocOp(this.docId, op);
    } catch (e) {
      if (e instanceof WaveletOperationError) {
        throw DocError.wavelet(e);
      }
      if (e instanceof WaveletError) {
        throw DocError.invalidArgument(e.message);
      }
      throw e;
    }
  }
}
