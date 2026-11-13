// Wavelet operation types.
//
// Derived from `org.waveprotocol.wave.model.operation.wave` (`WaveletOperation`,
// `NoOp`, `AddParticipant`, `RemoveParticipant`, `WaveletBlipOperation`,
// `BlipContentOperation`, `SubmitBlip`, `VersionUpdateOp`) of Apache Wave,
// licensed under the Apache License, Version 2.0.

import type { WaveletData } from "../data";
import type { DocOp } from "../docop";
import type { ParticipantId } from "../id";
import type { HashedVersion } from "../version";
// `apply.ts` imports this module too; neither uses the other at load time.
import { applyOperation, applyOperationAndReturnReverse } from "./apply";
import { NO_TIMESTAMP, WaveletOperationContext } from "./context";
import { WaveletOperationError } from "./error";
import { isBlipIdWorthy, isWorthy } from "./worthy";

/**
 * How applying a blip content operation changes the blip's contributors:
 * `"add"` adds the operation's creator if not already present, `"remove"`
 * removes it if present, `"none"` leaves the contributors alone.
 */
export type UpdateContributorMethod = "add" | "remove" | "none";

/** What a blip operation does. */
export type BlipOperationKind =
  /** Mutates the document content (`BlipContentOperation`). */
  | { readonly type: "content"; readonly op: DocOp; readonly method: UpdateContributorMethod }
  /** Marks the blip as submitted (`SubmitBlip`); changes no state. */
  | { readonly type: "submit" };

/**
 * An operation on one document of a wavelet. Immutable.
 *
 * Equality compares content operations syntactically and ignores the
 * contributor method, as in the original.
 */
export class BlipOperation {
  /** The submit operation. */
  static readonly SUBMIT = new BlipOperation({ type: "submit" });

  readonly kind: BlipOperationKind;

  private constructor(kind: BlipOperationKind) {
    this.kind = kind;
  }

  /** A content operation; by default it adds its creator to the contributors. */
  static content(op: DocOp, method: UpdateContributorMethod = "add"): BlipOperation {
    return new BlipOperation({ type: "content", op, method });
  }

  static submit(): BlipOperation {
    return BlipOperation.SUBMIT;
  }

  isContent(): boolean {
    return this.kind.type === "content";
  }

  /** The document operation of a content operation, else null. */
  get contentOp(): DocOp | null {
    return this.kind.type === "content" ? this.kind.op : null;
  }

  /**
   * Whether the operation is attributed to its creator when applied to the
   * document `blipId`.
   */
  isWorthyOfAttribution(blipId: string): boolean {
    const worthy = this.kind.type === "content" ? isWorthy(this.kind.op) : true;
    return worthy && isBlipIdWorthy(blipId);
  }

  /**
   * Whether applying the operation to the document `blipId` updates the
   * blip's contributors and last-modified metadata.
   */
  updatesBlipMetadata(blipId: string): boolean {
    return this.kind.type === "content" && this.isWorthyOfAttribution(blipId);
  }

  equals(other: BlipOperation): boolean {
    if (this.kind.type === "content") {
      return other.kind.type === "content" && this.kind.op.equals(other.kind.op);
    }
    return other.kind.type === "submit";
  }

  toString(): string {
    return this.kind.type === "content" ? `document op: ${this.kind.op}` : "submit";
  }
}

/** What a wavelet operation does. */
export type WaveletOperationKind =
  /** Changes nothing but the version. */
  | { readonly type: "noOp" }
  /**
   * Adds a participant, at the end unless a position is given. Positions
   * only occur in reverse operations.
   */
  | {
      readonly type: "addParticipant";
      readonly participant: ParticipantId;
      readonly position: number | null;
    }
  | { readonly type: "removeParticipant"; readonly participant: ParticipantId }
  /**
   * Applies a blip operation to a document, creating the document if it
   * does not exist (`WaveletBlipOperation`).
   */
  | { readonly type: "blip"; readonly blipId: string; readonly op: BlipOperation }
  /**
   * Updates only version information, optionally including a document's
   * last-modified version (`VersionUpdateOp`). `fixedDocVersion` is set on
   * reverse operations; otherwise the document gets the new wavelet version.
   */
  | {
      readonly type: "versionUpdate";
      readonly docId: string | null;
      readonly fixedDocVersion: number | null;
    };

function kindEquals(a: WaveletOperationKind, b: WaveletOperationKind): boolean {
  switch (a.type) {
    case "noOp":
      return b.type === "noOp";
    case "addParticipant":
      return (
        b.type === "addParticipant" &&
        a.participant.equals(b.participant) &&
        a.position === b.position
      );
    case "removeParticipant":
      return b.type === "removeParticipant" && a.participant.equals(b.participant);
    case "blip":
      return b.type === "blip" && a.blipId === b.blipId && a.op.equals(b.op);
    case "versionUpdate":
      return (
        b.type === "versionUpdate" &&
        a.docId === b.docId &&
        a.fixedDocVersion === b.fixedDocVersion
      );
  }
}

/**
 * An operation on a wavelet, with the context it is applied in. Immutable.
 *
 * Equality ignores the context, as `equals` does in the original; compare
 * {@link WaveletOperation.context} separately where it matters.
 */
export class WaveletOperation {
  readonly context: WaveletOperationContext;
  readonly kind: WaveletOperationKind;

  constructor(context: WaveletOperationContext, kind: WaveletOperationKind) {
    this.context = context;
    this.kind = kind;
  }

  static noOp(context: WaveletOperationContext): WaveletOperation {
    return new WaveletOperation(context, { type: "noOp" });
  }

  /**
   * Adds a participant at the end, or at `position` (only used by reverse
   * operations).
   */
  static addParticipant(
    context: WaveletOperationContext,
    participant: ParticipantId,
    position: number | null = null,
  ): WaveletOperation {
    return new WaveletOperation(context, { type: "addParticipant", participant, position });
  }

  static removeParticipant(
    context: WaveletOperationContext,
    participant: ParticipantId,
  ): WaveletOperation {
    return new WaveletOperation(context, { type: "removeParticipant", participant });
  }

  static blip(
    context: WaveletOperationContext,
    blipId: string,
    op: BlipOperation,
  ): WaveletOperation {
    return new WaveletOperation(context, { type: "blip", blipId, op });
  }

  /**
   * A content operation on a document that adds its creator to the
   * document's contributors.
   */
  static blipContent(
    context: WaveletOperationContext,
    blipId: string,
    contentOp: DocOp,
  ): WaveletOperation {
    return WaveletOperation.blip(context, blipId, BlipOperation.content(contentOp));
  }

  /** A version update without a timestamp. */
  static versionUpdate(
    creator: ParticipantId,
    versionIncrement: number,
    hashedVersion: HashedVersion | null = null,
    docId: string | null = null,
    fixedDocVersion: number | null = null,
  ): WaveletOperation {
    return new WaveletOperation(
      new WaveletOperationContext(creator, NO_TIMESTAMP, versionIncrement, hashedVersion),
      { type: "versionUpdate", docId, fixedDocVersion },
    );
  }

  /** The document id of a blip operation, else null. */
  get blipId(): string | null {
    return this.kind.type === "blip" ? this.kind.blipId : null;
  }

  /** The blip operation of a blip operation, else null. */
  get blipOp(): BlipOperation | null {
    return this.kind.type === "blip" ? this.kind.op : null;
  }

  /** The document operation of a blip content operation, else null. */
  get contentOp(): DocOp | null {
    return this.kind.type === "blip" ? this.kind.op.contentOp : null;
  }

  isBlipContent(): boolean {
    return this.kind.type === "blip" && this.kind.op.isContent();
  }

  isVersionUpdate(): boolean {
    return this.kind.type === "versionUpdate";
  }

  /**
   * The same operation in a new context (`WaveletOperation.cloneOp`).
   *
   * As in the original, an add-participant position and a content
   * operation's contributor method are reset, and version updates and blip
   * submits cannot be cloned ({@link WaveletOperationError} `"uncloneable"`).
   */
  cloneOp(newContext: WaveletOperationContext): WaveletOperation {
    const kind = this.kind;
    switch (kind.type) {
      case "noOp":
        return WaveletOperation.noOp(newContext);
      case "addParticipant":
        return WaveletOperation.addParticipant(newContext, kind.participant);
      case "removeParticipant":
        return WaveletOperation.removeParticipant(newContext, kind.participant);
      case "blip":
        if (kind.op.kind.type === "content") {
          return WaveletOperation.blipContent(newContext, kind.blipId, kind.op.kind.op);
        }
        break;
      case "versionUpdate":
        break;
    }
    throw new WaveletOperationError({ kind: "uncloneable", op: this.toString() });
  }

  /**
   * The version update that stands for this operation when its effect is
   * already present (`createVersionUpdateOp`).
   */
  createVersionUpdateOp(
    versionIncrement: number,
    hashedVersion: HashedVersion | null = null,
  ): WaveletOperation {
    const kind = this.kind;
    const docId =
      kind.type === "blip" && kind.op.updatesBlipMetadata(kind.blipId) ? kind.blipId : null;
    return WaveletOperation.versionUpdate(
      this.context.creator,
      versionIncrement,
      hashedVersion,
      docId,
      null,
    );
  }

  /**
   * Whether the operation is attributed to its creator. Only unworthy blip
   * operations are not.
   */
  isWorthyOfAttribution(): boolean {
    return this.kind.type === "blip" ? this.kind.op.isWorthyOfAttribution(this.kind.blipId) : true;
  }

  /**
   * Applies the operation to a wavelet and then updates the wavelet's
   * last-modified time, version and hashed version from the context.
   * Throws {@link WaveletOperationError}.
   *
   * On error the wavelet is unchanged, with one exception kept from the
   * original: a blip operation on a missing document creates the document
   * before its content operation is validated, so a failed content
   * operation leaves an empty document behind.
   */
  apply(target: WaveletData): void {
    applyOperation(this, target);
  }

  /**
   * Applies the operation like {@link WaveletOperation.apply} and returns
   * the operations that reverse it.
   *
   * Quirks kept from the original: the reverse of a blip operation restores
   * neither the wavelet's hashed version nor the blip's previous
   * last-modified version, and sets the wavelet's last-modified time to the
   * blip's previous one; a content operation that fails validation has
   * already updated the blip's metadata.
   */
  applyAndReturnReverse(target: WaveletData): WaveletOperation[] {
    return applyOperationAndReturnReverse(this, target);
  }

  /** Equality of what the operations do; the contexts are not compared. */
  equals(other: WaveletOperation): boolean {
    return kindEquals(this.kind, other.kind);
  }

  private suffix(): string {
    const version = this.context.hashedVersion;
    return (
      `by ${this.context.creator} at ${this.context.timestamp} ` +
      `version ${version === null ? "null" : version.toString()}`
    );
  }

  /**
   * Like the original `toString`, except that the timestamp is printed as
   * a number of milliseconds rather than a date.
   */
  toString(): string {
    const kind = this.kind;
    switch (kind.type) {
      case "noOp":
        return `no-op ${this.suffix()}`;
      case "addParticipant":
        return (
          `add participant ${kind.participant}` +
          (kind.position === null ? "" : ` at position ${kind.position}`) +
          ` ${this.suffix()}`
        );
      case "removeParticipant":
        return `remove participant ${kind.participant} ${this.suffix()}`;
      case "blip":
        return (
          `${kind.op} on blip ${kind.blipId} ${this.suffix()} ` +
          `isWorthy ${this.isWorthyOfAttribution()}`
        );
      case "versionUpdate":
        return (
          `version update op, blip id ${kind.docId ?? "null"} ` +
          `blipVersion ${kind.fixedDocVersion ?? -1}`
        );
    }
  }
}
